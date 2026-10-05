//! The 3D view. Draws a ground grid and the layers that other modules add through the
//! "viewport" service, into an offscreen target shown in its panel. Its camera is offered to every
//! language: animatable properties and commands (step 8.3). A layer's bundle is kept while its
//! version stays the same, and a frame signal tells the threads of modules that a frame was
//! submitted (step 9.2a).

mod camera;
mod grid;

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use uniwow_api::glam::Vec3;
use uniwow_api::serde_json::{Value, json};
use uniwow_api::viewport::{self, Frame, Layer, MAX_FRAME_WAIT, Target, View};
use uniwow_api::{
    Context, DockArea, Event, MODULE_FAILED_TOPIC, Module, PropertyKind, PropertyValue, Registrar, egui, egui_wgpu,
    wgpu,
};

use camera::{FOV, OrbitCamera, REACH};
use grid::Grid;

const TARGET: Target = Target {
    color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
    depth_format: wgpu::TextureFormat::Depth32Float,
    sample_count: 4,
    depth_compare: wgpu::CompareFunction::Greater,
};

const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.012,
    g: 0.014,
    b: 0.02,
    a: 1.0,
};

/// A layer, with the bundle kept for it and the version it was recorded at.
struct Entry {
    owner: String,
    layer: Box<dyn Layer>,
    kept: Option<(u64, wgpu::RenderBundle)>,
}

/// The layers, and the owners whose layers were removed while the list was out being drawn.
#[derive(Default)]
struct LayerList {
    layers: Vec<Entry>,
    /// Set while `record_layers` has the layers out.
    drawing: bool,
    /// Removed again from the drawn layers when they come back.
    removed: Vec<String>,
}

type Layers = Arc<Mutex<LayerList>>;

/// The layer list, even if a panic left its lock poisoned: layers are taken out while drawn.
fn lock(layers: &Layers) -> MutexGuard<'_, LayerList> {
    layers.lock().unwrap_or_else(|e| e.into_inner())
}

/// The camera, moved by the panel, by its properties and by its commands, from any thread.
type Camera = Arc<Mutex<OrbitCamera>>;

fn camera(camera: &Camera) -> MutexGuard<'_, OrbitCamera> {
    camera.lock().unwrap_or_else(|e| e.into_inner())
}

/// A number of the camera as a property shows it: the shortest decimal of the f32.
fn widen(value: f32) -> f64 {
    value.to_string().parse().unwrap_or(f64::from(value))
}

fn vector(point: Vec3) -> PropertyValue {
    PropertyValue::Vector([widen(point.x), widen(point.y), widen(point.z)])
}

fn point(value: PropertyValue) -> Vec3 {
    let numbers = value.components();
    Vec3::new(numbers[0] as f32, numbers[1] as f32, numbers[2] as f32)
}

/// The camera as the commands give it.
fn camera_json(camera: &OrbitCamera) -> Value {
    let three = |p: Vec3| json!([widen(p.x), widen(p.y), widen(p.z)]);
    json!({ "position": three(camera.eye()), "target": three(camera.target()), "fov": widen(camera.fov()) })
}

/// The argument `name`: three finite numbers within `reach` of the origin on each axis.
fn point_argument(arguments: &Value, name: &str, reach: f64) -> Result<Vec3, String> {
    let numbers: Option<Vec<f64>> = arguments[name]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_f64).collect());
    match numbers.as_deref() {
        Some([x, y, z]) if [x, y, z].iter().all(|n| n.is_finite() && n.abs() <= reach) => {
            Ok(Vec3::new(*x as f32, *y as f32, *z as f32))
        }
        _ => Err(format!("'{name}' must be three numbers within {reach:e}")),
    }
}

/// The camera and frame drawn, the camera locked once.
fn view(shared: &Camera, size: [u32; 2], time: f32) -> View {
    let mut camera = camera(shared);
    let aspect = size[0] as f32 / size[1] as f32;
    // Kept for viewport.frame, which fits a box in the width as in the height.
    camera.set_aspect(aspect);
    View {
        view_proj: camera.view_proj(aspect),
        eye: camera.eye(),
        size,
        time,
    }
}

/// `viewport.look_at`: the eye at `position`, looking at `target`, with the angle `fov` if given.
fn look_at(shared: &Camera, arguments: &Value) -> Result<Value, String> {
    let (position, target) = (
        point_argument(arguments, "position", 2.0 * REACH)?,
        point_argument(arguments, "target", REACH)?,
    );
    let fov = match &arguments["fov"] {
        Value::Null => None,
        value => Some(
            value
                .as_f64()
                .filter(|fov| fov.is_finite())
                .ok_or("'fov' must be a number of degrees")?,
        ),
    };
    let mut camera = camera(shared);
    camera.look_at(position, target);
    if let Some(fov) = fov {
        camera.set_fov(fov as f32);
    }
    Ok(camera_json(&camera))
}

/// `viewport.frame`: the box from `min` to `max` in view, seen from the same direction.
fn frame(shared: &Camera, arguments: &Value) -> Result<Value, String> {
    let (min, max) = (
        point_argument(arguments, "min", REACH)?,
        point_argument(arguments, "max", REACH)?,
    );
    if min.cmpgt(max).any() {
        return Err("'min' must be below 'max' on every axis".to_owned());
    }
    let mut camera = camera(shared);
    camera.frame(min, max);
    Ok(camera_json(&camera))
}

/// Removes the layers of `owner`, including those out being drawn at this moment.
fn remove(layers: &Layers, owner: &str) {
    let mut list = lock(layers);
    list.layers.retain(|entry| entry.owner != owner);
    if list.drawing {
        list.removed.push(owner.to_owned());
    }
}

/// The frame signal: the frame to come, given once a frame is submitted, and the threads waiting.
#[derive(Default)]
struct FrameSignal {
    next: Mutex<Option<Frame>>,
    given: Condvar,
}

impl FrameSignal {
    fn give(&self, frame: Frame) {
        *self.next.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
        self.given.notify_all();
    }

    /// The frame to come once its number is past `after`, waiting `timeout` at most, and never more
    /// than `MAX_FRAME_WAIT`.
    fn wait(&self, after: u64, timeout: Duration) -> Option<Frame> {
        let deadline = Instant::now() + timeout.min(MAX_FRAME_WAIT);
        let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(frame) = *next
                && frame.number > after
            {
                return Some(frame);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            next = self.given.wait_timeout(next, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

/// Implementation of the service, sharing the layer list and the frame signal with the module.
struct Service {
    layers: Layers,
    frames: Arc<FrameSignal>,
}

impl viewport::Viewport for Service {
    fn add_layer(&self, owner: &str, layer: Box<dyn Layer>) {
        lock(&self.layers).layers.push(Entry {
            owner: owner.to_owned(),
            layer,
            kept: None,
        });
    }

    fn remove_layers(&self, owner: &str) {
        remove(&self.layers, owner);
    }

    fn target(&self) -> Target {
        TARGET
    }

    fn wait_frame(&self, after: u64, timeout: Duration) -> Option<Frame> {
        self.frames.wait(after, timeout)
    }
}

/// Offscreen textures, recreated when the panel changes size.
struct Targets {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    resolved: wgpu::TextureView,
    depth: wgpu::TextureView,
    texture_id: egui::TextureId,
}

struct ViewportModule {
    layers: Layers,
    camera: Camera,
    targets: Option<Targets>,
    grid: Option<Grid>,
    start: Instant,
    frames: Arc<FrameSignal>,
    /// The frames submitted, the time of the last one, and the time between two, averaged.
    submitted: u64,
    last_time: Option<f32>,
    interval: f32,
    /// The device the kept bundles were recorded on.
    device: Option<wgpu::Device>,
}

impl Default for ViewportModule {
    fn default() -> Self {
        Self {
            layers: Arc::default(),
            camera: Arc::default(),
            targets: None,
            grid: None,
            start: Instant::now(),
            frames: Arc::default(),
            submitted: 0,
            last_time: None,
            interval: 0.0,
            device: None,
        }
    }
}

impl Module for ViewportModule {
    fn register(&mut self, reg: &mut Registrar) {
        let service: viewport::Handle = Arc::new(Service {
            layers: self.layers.clone(),
            frames: self.frames.clone(),
        });
        reg.panel("view", "3D View", DockArea::Center)
            .provide(viewport::SERVICE, service)
            .subscribe(MODULE_FAILED_TOPIC)
            .menu_item("View", "Reset camera", "reset_camera");

        // The camera for every language: animatable, and moved by commands.
        let shared = self.camera.clone();
        let (read, write) = (shared.clone(), shared.clone());
        reg.animatable(
            "camera_position",
            "Camera position",
            PropertyKind::Vector,
            [-2.0 * REACH, 2.0 * REACH],
            move || vector(camera(&read).eye()),
            move |value| camera(&write).set_position(point(value)),
        );
        let (read, write) = (shared.clone(), shared.clone());
        reg.animatable(
            "camera_target",
            "Camera target",
            PropertyKind::Vector,
            [-REACH, REACH],
            move || vector(camera(&read).target()),
            move |value| camera(&write).set_target(point(value)),
        );
        let (read, write) = (shared.clone(), shared.clone());
        reg.animatable(
            "camera_fov",
            "Camera angle of view",
            PropertyKind::Number,
            [f64::from(FOV[0]), f64::from(FOV[1])],
            move || PropertyValue::Number(widen(camera(&read).fov())),
            move |value| camera(&write).set_fov(value.components()[0] as f32),
        );
        let point = json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 });
        let answer = json!({ "type": "object", "properties": { "position": point, "target": point, "fov": { "type": "number" } } });
        let read = shared.clone();
        reg.command_on_caller(
            "viewport.camera",
            "The camera of the 3D view: its position, the point it looks at, and its vertical angle of view in degrees.",
            json!({ "type": "object" }),
            answer.clone(),
            Arc::new(move |_| Ok(camera_json(&camera(&read)))),
        );
        let moved = shared.clone();
        reg.command_on_caller(
            "viewport.look_at",
            "Puts the camera at position, looking at target, with the angle of view fov in degrees if given.",
            json!({ "type": "object", "properties": { "position": point, "target": point, "fov": { "type": "number" } }, "required": ["position", "target"] }),
            answer.clone(),
            Arc::new(move |arguments| look_at(&moved, &arguments)),
        );
        reg.command_on_caller(
            "viewport.frame",
            "Fits the box from min to max in the 3D view, seen from the same direction.",
            json!({ "type": "object", "properties": { "min": point, "max": point }, "required": ["min", "max"] }),
            answer,
            Arc::new(move |arguments| frame(&shared, &arguments)),
        );
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(gpu) = ctx.gpu().cloned() else {
            ui.label("No GPU device is available.");
            return;
        };
        let size = ui.available_size().max(egui::vec2(1.0, 1.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        camera(&self.camera).handle_input(ui, &response);

        let pixels = size * ui.ctx().pixels_per_point();
        let pixels = [pixels.x.round().max(1.0) as u32, pixels.y.round().max(1.0) as u32];
        self.ensure_targets(&gpu, pixels);
        self.render(&gpu, pixels, ctx);

        let targets = self.targets.as_ref().expect("created above");
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(targets.texture_id, rect, uv, egui::Color32::WHITE);
        let caption = format!(
            "{} layers · drag: orbit · right drag: pan · wheel: zoom",
            lock(&self.layers).layers.len()
        );
        ui.painter().text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            caption,
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(150),
        );
        ui.ctx().request_repaint();
    }

    fn on_event(&mut self, event: &Event, _ctx: &mut Context) {
        if event.topic == MODULE_FAILED_TOPIC
            && let Some(id) = event.payload.get("id").and_then(|v| v.as_str())
        {
            remove(&self.layers, id);
        }
    }

    fn on_menu(&mut self, action: &str, _ctx: &mut Context) {
        if action == "reset_camera" {
            *camera(&self.camera) = OrbitCamera::default();
        }
    }
}

impl ViewportModule {
    fn ensure_targets(&mut self, gpu: &egui_wgpu::RenderState, size: [u32; 2]) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let device = &gpu.device;
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let texture = |label, format, samples, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let msaa = texture(
            "viewport msaa",
            TARGET.color_format,
            TARGET.sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let resolved = texture(
            "viewport colour",
            TARGET.color_format,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = texture(
            "viewport depth",
            TARGET.depth_format,
            TARGET.sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );

        let mut renderer = gpu.renderer.write();
        let texture_id = match &self.targets {
            Some(old) => {
                renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    &resolved,
                    wgpu::FilterMode::Linear,
                    old.texture_id,
                );
                old.texture_id
            }
            None => renderer.register_native_texture(device, &resolved, wgpu::FilterMode::Linear),
        };
        self.targets = Some(Targets {
            size,
            msaa,
            resolved,
            depth,
            texture_id,
        });
    }

    fn render(&mut self, gpu: &egui_wgpu::RenderState, size: [u32; 2], ctx: &mut Context) {
        let view = view(&self.camera, size, self.start.elapsed().as_secs_f32());
        let new_device = self.device.as_ref() != Some(&gpu.device);
        if new_device {
            self.device = Some(gpu.device.clone());
        }
        let (bundles, failures) = draw_layers(&self.layers, gpu, &view, new_device);
        for (owner, message) in failures {
            ctx.report_failure(&owner, &message);
        }
        let targets = self.targets.as_ref().expect("created before rendering");
        let grid = self.grid.get_or_insert_with(|| Grid::new(&gpu.device, &TARGET));
        grid.update(&gpu.queue, &view);

        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewport"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa,
                    depth_slice: None,
                    resolve_target: Some(&targets.resolved),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(BACKGROUND),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        // Reverse Z: infinity is 0.
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            grid.draw(&mut pass);
            pass.execute_bundles(bundles.iter());
        }
        gpu.queue.submit([encoder.finish()]);
        self.signal_frame(view.time);
    }

    /// Gives the frame signal of the frame to come, once the frame drawn at `time` is submitted.
    fn signal_frame(&mut self, time: f32) {
        if let Some(last) = self.last_time {
            // A pause of the view, hidden or minimised, is not a frame's time.
            let step = time - last;
            if step > 0.0 && step < 1.0 {
                self.interval = if self.interval > 0.0 {
                    self.interval * 0.9 + step * 0.1
                } else {
                    step
                };
            }
        }
        self.last_time = Some(time);
        self.submitted += 1;
        self.frames.give(Frame {
            number: self.submitted + 1,
            time: time + self.interval,
        });
    }
}

/// Prepares each layer with the view of this frame, then records it into its own render bundle,
/// or keeps the bundle of its version unless the device is `new_device`. The bundles to draw, and
/// the layers that panicked or failed, with why: those are removed.
fn draw_layers(
    layers: &Layers,
    gpu: &egui_wgpu::RenderState,
    view: &View,
    new_device: bool,
) -> (Vec<wgpu::RenderBundle>, Vec<(String, String)>) {
    // Layers may be added or removed meanwhile, by a layer or by another thread: take the list
    // out, then put it back in front, without the layers removed in between.
    let mut entries = {
        let mut list = lock(layers);
        list.drawing = true;
        std::mem::take(&mut list.layers)
    };
    let mut bundles = Vec::new();
    let mut failures = Vec::new();
    entries.retain_mut(|entry| {
        if new_device {
            entry.kept = None;
        }
        match draw_layer(entry, gpu, view) {
            Ok(bundle) => {
                bundles.push(bundle);
                true
            }
            Err(message) => {
                failures.push((entry.owner.clone(), message));
                false
            }
        }
    });
    let mut list = lock(layers);
    list.drawing = false;
    let removed = std::mem::take(&mut list.removed);
    entries.retain(|entry| !removed.contains(&entry.owner));
    entries.append(&mut list.layers);
    list.layers = entries;
    (bundles, failures)
}

/// Prepares one layer, then gives the bundle kept for its version, or records it, each inside a
/// validation error scope.
fn draw_layer(entry: &mut Entry, gpu: &egui_wgpu::RenderState, view: &View) -> Result<wgpu::RenderBundle, String> {
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let layer = entry.layer.as_mut();
    let prepared = catch_unwind(AssertUnwindSafe(|| {
        layer.prepare(gpu, view);
        layer.version()
    }));
    let error = resolved(scope.pop()).flatten();
    let version = match (prepared, error) {
        (Err(payload), _) => {
            return Err(format!(
                "its viewport layer panicked while preparing: {}",
                panic_text(payload)
            ));
        }
        (Ok(_), Some(error)) => {
            return Err(format!(
                "its viewport layer caused a GPU error while preparing: {error}"
            ));
        }
        (Ok(version), None) => version,
    };
    if let (Some(version), Some((kept, bundle))) = (version, &entry.kept)
        && *kept == version
    {
        return Ok(bundle.clone());
    }
    let bundle = record(&entry.owner, entry.layer.as_mut(), gpu, view)?;
    entry.kept = version.map(|version| (version, bundle.clone()));
    Ok(bundle)
}

/// Records `layer` into its own render bundle, inside a validation error scope.
fn record(
    owner: &str,
    layer: &mut dyn Layer,
    gpu: &egui_wgpu::RenderState,
    view: &View,
) -> Result<wgpu::RenderBundle, String> {
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut encoder = gpu
        .device
        .create_render_bundle_encoder(&wgpu::RenderBundleEncoderDescriptor {
            label: Some(owner),
            color_formats: &[Some(TARGET.color_format)],
            depth_stencil: Some(wgpu::RenderBundleDepthStencil {
                format: TARGET.depth_format,
                depth_read_only: false,
                stencil_read_only: true,
            }),
            sample_count: TARGET.sample_count,
            multiview: None,
        });
    let recording = &mut encoder;
    // Moved into the closure: the encoder borrows the layer's resources until it is finished.
    let drawn = catch_unwind(AssertUnwindSafe(move || {
        let layer = layer;
        layer.draw(gpu, &TARGET, view, recording)
    }));
    // wgpu 30 validates the recorded commands here and panics on an invalid one instead of
    // reporting it to the error scope, so the panic is caught too.
    let finished = catch_unwind(AssertUnwindSafe(move || {
        encoder.finish(&wgpu::RenderBundleDescriptor { label: Some(owner) })
    }));
    let error = resolved(scope.pop()).flatten();
    match (drawn, finished, error) {
        (Err(payload), _, _) => Err(format!("its viewport layer panicked: {}", panic_text(payload))),
        (Ok(()), Err(payload), _) => Err(format!(
            "its viewport layer recorded invalid GPU commands: {}",
            panic_text(payload)
        )),
        (Ok(()), Ok(_), Some(error)) => Err(format!("its viewport layer caused a GPU error: {error}")),
        (Ok(()), Ok(bundle), None) => Ok(bundle),
    }
}

/// The value of a future that is already complete, as error scopes are on native backends.
fn resolved<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut std::task::Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

fn panic_text(payload: Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic without message".to_owned())
}

uniwow_api::export_module!(ViewportModule::default());

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use uniwow_api::serde_json::json;
    use uniwow_api::viewport::{Frame, Layer, Target, View};
    use uniwow_api::{egui, egui_wgpu, wgpu};

    use super::{
        Camera, Entry, FrameSignal, Layers, ViewportModule, camera, draw_layers, frame, lock, look_at, resolved, view,
    };

    /// A device of the software adapter of the system, or none where there is none.
    fn gpu() -> Option<egui_wgpu::RenderState> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
            force_fallback_adapter: true,
            ..Default::default()
        }))?
        .ok()?;
        let (device, queue) = resolved(adapter.request_device(&wgpu::DeviceDescriptor::default()))?.ok()?;
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
        Some(egui_wgpu::RenderState {
            adapter,
            available_adapters: Vec::new(),
            instance,
            device,
            queue,
            target_format: format,
            renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
            surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
        })
    }

    /// How often a test layer was prepared and recorded; its version, `u64::MAX` for none.
    #[derive(Clone, Default)]
    struct Counts {
        prepared: Arc<AtomicUsize>,
        drawn: Arc<AtomicUsize>,
        version: Arc<AtomicU64>,
    }

    struct Counted {
        counts: Counts,
        failing: bool,
    }

    impl Layer for Counted {
        fn prepare(&mut self, _gpu: &egui_wgpu::RenderState, _view: &View) {
            self.counts.prepared.fetch_add(1, Ordering::Relaxed);
            assert!(!self.failing, "its buffers could not be written");
        }

        fn version(&self) -> Option<u64> {
            Some(self.counts.version.load(Ordering::Relaxed)).filter(|version| *version != u64::MAX)
        }

        fn draw<'a>(
            &'a mut self,
            _gpu: &egui_wgpu::RenderState,
            _target: &Target,
            _view: &View,
            _bundle: &mut wgpu::RenderBundleEncoder<'a>,
        ) {
            self.counts.drawn.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn add(layers: &Layers, owner: &str, counts: &Counts, failing: bool) {
        lock(layers).layers.push(Entry {
            owner: owner.to_owned(),
            layer: Box::new(Counted {
                counts: counts.clone(),
                failing,
            }),
            kept: None,
        });
    }

    #[test]
    fn a_bundle_is_kept_while_its_version_stays_and_recorded_again_once_it_changes() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let layers = Layers::default();
        let (kept, every) = (Counts::default(), Counts::default());
        kept.version.store(1, Ordering::Relaxed);
        every.version.store(u64::MAX, Ordering::Relaxed);
        add(&layers, "terrain", &kept, false);
        add(&layers, "cube", &every, false);
        let view = view(&Camera::default(), [64, 64], 0.0);
        let frames = |new_device| {
            let (bundles, failures) = draw_layers(&layers, &gpu, &view, new_device);
            assert!(failures.is_empty(), "{failures:?}");
            bundles.len()
        };
        for _ in 0..3 {
            assert_eq!(frames(false), 2);
        }
        let count = |counts: &Counts| {
            (
                counts.prepared.load(Ordering::Relaxed),
                counts.drawn.load(Ordering::Relaxed),
            )
        };
        assert_eq!(count(&kept), (3, 1), "prepared at each frame, recorded once");
        assert_eq!(
            count(&every),
            (3, 3),
            "a layer without a version is recorded at each frame"
        );
        kept.version.store(2, Ordering::Relaxed);
        frames(false);
        assert_eq!(count(&kept), (4, 2), "recorded again once its version changed");
        frames(true);
        assert_eq!(count(&kept), (5, 3), "recorded again on a device created again");
        frames(false);
        assert_eq!(count(&kept), (6, 3));

        add(&layers, "faulty", &Counts::default(), true);
        let (bundles, failures) = draw_layers(&layers, &gpu, &view, false);
        assert_eq!(bundles.len(), 2);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "faulty");
        assert!(failures[0].1.contains("panicked while preparing"), "{}", failures[0].1);
        assert_eq!(lock(&layers).layers.len(), 2, "the faulty layer is removed");
    }

    #[test]
    fn the_frame_signal_wakes_the_threads_waiting_and_never_holds_them_long() {
        let signal = Arc::new(FrameSignal::default());
        let started = Instant::now();
        assert_eq!(signal.wait(0, Duration::from_secs(10)), None);
        assert!(started.elapsed() < Duration::from_secs(1), "100 ms at most at a time");
        let waiting = {
            let signal = signal.clone();
            std::thread::spawn(move || {
                loop {
                    if let Some(frame) = signal.wait(0, Duration::from_millis(100)) {
                        return frame;
                    }
                }
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        let first = Frame { number: 1, time: 0.5 };
        signal.give(first);
        assert_eq!(waiting.join().unwrap(), first);
        assert_eq!(
            signal.wait(1, Duration::from_millis(10)),
            None,
            "a frame seen is not given again"
        );
        assert_eq!(signal.wait(0, Duration::ZERO), Some(first));
    }

    #[test]
    fn the_frame_to_come_is_given_with_its_time_estimated_from_the_frames_before() {
        let mut module = ViewportModule::default();
        let next = |module: &ViewportModule| module.frames.wait(0, Duration::ZERO).unwrap();
        module.signal_frame(1.0);
        assert_eq!(next(&module), Frame { number: 2, time: 1.0 });
        module.signal_frame(1.016);
        let frame = next(&module);
        assert_eq!(frame.number, 3);
        assert!((frame.time - 1.032).abs() < 1e-4, "{frame:?}");
        module.signal_frame(5.0);
        let frame = next(&module);
        assert_eq!(frame.number, 4);
        assert!(
            (frame.time - 5.016).abs() < 1e-4,
            "a pause is not a frame's time: {frame:?}"
        );
    }

    #[test]
    fn the_commands_move_the_camera_and_refuse_what_is_not_a_point() {
        let camera = Camera::default();
        let moved = look_at(
            &camera,
            &json!({ "position": [10, 0, 5], "target": [0, 0, 1], "fov": 60 }),
        )
        .unwrap();
        assert_eq!(moved["position"], json!([10.0, 0.0, 5.0]));
        assert_eq!(moved["target"], json!([0.0, 0.0, 1.0]));
        assert_eq!(moved["fov"], json!(60.0));
        assert!(look_at(&camera, &json!({ "position": [1, 2], "target": [0, 0, 0] })).is_err());
        assert!(look_at(&camera, &json!({ "position": [1e9, 0, 0], "target": [0, 0, 0] })).is_err());
        // The eye goes twice as far as the target: a position read is a position written back.
        let far = look_at(
            &camera,
            &json!({ "position": [200000, 0, 0], "target": [100000, 0, 0] }),
        )
        .unwrap();
        assert_eq!(far["position"], json!([200000.0, 0.0, 0.0]));
        let eye = super::vector(super::camera(&camera).eye()).components();
        assert!(eye.iter().all(|n| n.abs() <= 2.0 * super::REACH));
        assert!(
            look_at(
                &camera,
                &json!({ "position": [1, 0, 0], "target": [0, 0, 0], "fov": "wide" })
            )
            .is_err()
        );
        let framed = frame(&camera, &json!({ "min": [-2, -2, 0], "max": [2, 2, 2] })).unwrap();
        assert_eq!(framed["target"], json!([0.0, 0.0, 1.0]));
        assert!(frame(&camera, &json!({ "min": [2, 0, 0], "max": [1, 1, 1] })).is_err());
    }

    #[test]
    fn a_frame_locks_the_camera_once() {
        let shared = Camera::default();
        // A second lock while the first is held would never return.
        let drawn = view(&shared, [640, 480], 0.0);
        assert_eq!(drawn.eye, camera(&shared).eye());
    }
}
