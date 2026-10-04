//! The 3D view. Draws a ground grid and the layers that other modules add through the
//! "viewport" service, into an offscreen target shown in its panel. Its camera is offered to every
//! language: animatable properties and commands (step 8.3).

mod camera;
mod grid;

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Poll, Waker};
use std::time::Instant;

use uniwow_api::glam::Vec3;
use uniwow_api::serde_json::{Value, json};
use uniwow_api::viewport::{self, Layer, Target, View};
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

/// The layers, and the owners whose layers were removed while the list was out being drawn.
#[derive(Default)]
struct LayerList {
    layers: Vec<(String, Box<dyn Layer>)>,
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
    list.layers.retain(|(o, _)| o != owner);
    if list.drawing {
        list.removed.push(owner.to_owned());
    }
}

/// Implementation of the service, sharing the layer list with the module.
struct Service {
    layers: Layers,
}

impl viewport::Viewport for Service {
    fn add_layer(&self, owner: &str, layer: Box<dyn Layer>) {
        lock(&self.layers).layers.push((owner.to_owned(), layer));
    }

    fn remove_layers(&self, owner: &str) {
        remove(&self.layers, owner);
    }

    fn target(&self) -> Target {
        TARGET
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
}

impl Default for ViewportModule {
    fn default() -> Self {
        Self {
            layers: Arc::default(),
            camera: Arc::default(),
            targets: None,
            grid: None,
            start: Instant::now(),
        }
    }
}

impl Module for ViewportModule {
    fn register(&mut self, reg: &mut Registrar) {
        let service: viewport::Handle = Arc::new(Service {
            layers: self.layers.clone(),
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
        let bundles = self.record_layers(gpu, &view, ctx);
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
    }

    /// Records each layer into its own render bundle, inside a validation error scope. A layer
    /// that panics or records invalid commands is removed and its module reported; the bundles of
    /// the others are returned.
    fn record_layers(
        &mut self,
        gpu: &egui_wgpu::RenderState,
        view: &View,
        ctx: &mut Context,
    ) -> Vec<wgpu::RenderBundle> {
        // Layers may be added or removed meanwhile, by a layer or by another thread: take the
        // list out, then put it back in front, without the layers removed in between.
        let mut layers = {
            let mut list = lock(&self.layers);
            list.drawing = true;
            std::mem::take(&mut list.layers)
        };
        let mut bundles = Vec::new();
        layers.retain_mut(|(owner, layer)| {
            let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut encoder = gpu
                .device
                .create_render_bundle_encoder(&wgpu::RenderBundleEncoderDescriptor {
                    label: Some(owner.as_str()),
                    color_formats: &[Some(TARGET.color_format)],
                    depth_stencil: Some(wgpu::RenderBundleDepthStencil {
                        format: TARGET.depth_format,
                        depth_read_only: false,
                        stencil_read_only: true,
                    }),
                    sample_count: TARGET.sample_count,
                    multiview: None,
                });
            let layer: &mut dyn Layer = layer.as_mut();
            let recording = &mut encoder;
            // Moved into the closure: the bundle borrows the layer's resources for its whole life.
            let drawn = catch_unwind(AssertUnwindSafe(move || {
                let layer = layer;
                layer.draw(gpu, &TARGET, view, recording)
            }));
            // wgpu 30 validates the recorded commands here and panics on an invalid one instead of
            // reporting it to the error scope, so the panic is caught too.
            let label = owner.as_str();
            let finished = catch_unwind(AssertUnwindSafe(move || {
                encoder.finish(&wgpu::RenderBundleDescriptor { label: Some(label) })
            }));
            let error = resolved(scope.pop()).flatten();
            let failure = match (drawn, finished, error) {
                (Err(payload), _, _) => Err(format!("its viewport layer panicked: {}", panic_text(payload))),
                (Ok(()), Err(payload), _) => Err(format!(
                    "its viewport layer recorded invalid GPU commands: {}",
                    panic_text(payload)
                )),
                (Ok(()), Ok(_), Some(error)) => Err(format!("its viewport layer caused a GPU error: {error}")),
                (Ok(()), Ok(bundle), None) => Ok(bundle),
            };
            match failure {
                Ok(bundle) => {
                    bundles.push(bundle);
                    true
                }
                Err(message) => {
                    ctx.report_failure(owner, &message);
                    false
                }
            }
        });
        let mut list = lock(&self.layers);
        list.drawing = false;
        let removed = std::mem::take(&mut list.removed);
        layers.retain(|(owner, _)| !removed.contains(owner));
        layers.append(&mut list.layers);
        list.layers = layers;
        bundles
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
    use uniwow_api::serde_json::json;

    use super::{Camera, camera, frame, look_at, view};

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
