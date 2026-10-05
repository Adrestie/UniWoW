//! The 3D view. Draws a ground grid and the layers that other modules add through the
//! "viewport" service, into an offscreen target shown in its panel. Its camera is offered to every
//! language: animatable properties and commands (step 8.3). A layer's bundle is kept while its
//! version stays the same, and a frame signal tells the threads of modules that a frame was
//! submitted (step 9.2a). The camera flies with its hotkeys, looks with the right or middle drag,
//! and turns around its target with the drag while its orbit hotkey is held (step 9.2d). Its
//! statistics, shown over the view from the menu View, time the interface thread and the GPU, and
//! say what each layer drew (step 9.2e).

mod camera;
mod grid;
mod stats;

use std::any::Any;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::hotkey::{Hotkey, HotkeyKind, Keys};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::viewport::{
    self, Allowance, Demand, Drawing, Fog, Frame, Label, Layer, MAX_FRAME_WAIT, Sun, Target, View,
};
use uniwow_api::{
    Context, DockArea, Event, MODULE_FAILED_TOPIC, Module, PropertyKind, PropertyValue, Registrar, egui, egui_wgpu,
    wgpu,
};

use camera::{FOV, OrbitCamera, REACH};
use grid::Grid;
use stats::{GpuTimer, LayerTiming, Sample, Stats};

/// The setting that shows the statistics over the view.
const STATISTICS: &str = "statistics";

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

/// The camera and frame drawn, the camera locked once, with the fog set last.
fn view(shared: &Camera, size: [u32; 2], time: f32, fog: Fog) -> View {
    let mut camera = camera(shared);
    let aspect = size[0] as f32 / size[1] as f32;
    // Kept for viewport.frame, which fits a box in the width as in the height.
    camera.set_aspect(aspect);
    View {
        view_proj: camera.view_proj(aspect),
        view: camera.view(),
        eye: camera.eye(),
        size,
        time,
        fog,
        sun: Sun::default(),
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

/// The GPU budget of the view: its size, what each module told it, what that allows, and a size
/// set and not yet saved in the settings.
#[derive(Default)]
struct BudgetState {
    bytes: u64,
    demands: HashMap<String, Demand>,
    allowance: Allowance,
    unsaved: Option<u64>,
}

impl BudgetState {
    /// Decides again what the budget allows, after a change.
    fn allow(&mut self) -> Allowance {
        self.allowance = viewport::allow(self.bytes, &self.demands.values().collect::<Vec<_>>());
        self.allowance
    }
}

type Budget = Arc<Mutex<BudgetState>>;

fn budget(budget: &Budget) -> MutexGuard<'_, BudgetState> {
    budget.lock().unwrap_or_else(|e| e.into_inner())
}

/// The setting of the budget, in MB; half the memory of the GPU's own by default, when told.
const BUDGET: &str = "gpu_budget_mb";
const FALLBACK_BUDGET: u64 = 1024;
const BUDGETS: [u64; 2] = [64, 65_536];
const MB: u64 = 1024 * 1024;

/// The budget by default, in MB: half of `gpu_memory` bytes, the memory of the GPU's own, when the
/// system tells it.
fn default_budget(gpu_memory: Option<u64>) -> u64 {
    gpu_memory
        .map_or(FALLBACK_BUDGET, |bytes| bytes / 2 / MB)
        .clamp(BUDGETS[0], BUDGETS[1])
}

/// Implementation of the service, sharing the layer list, the frame signal and the budget with the
/// module.
struct Service {
    layers: Layers,
    frames: Arc<FrameSignal>,
    budget: Budget,
    fog: Arc<Mutex<Fog>>,
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
        let mut state = budget(&self.budget);
        if state.demands.remove(owner).is_some() {
            state.allow();
        }
    }

    fn target(&self) -> Target {
        TARGET
    }

    fn wait_frame(&self, after: u64, timeout: Duration) -> Option<Frame> {
        self.frames.wait(after, timeout)
    }

    fn tell_budget(&self, owner: &str, demand: Demand) -> Allowance {
        let mut state = budget(&self.budget);
        state.demands.insert(owner.to_owned(), demand);
        state.allow()
    }

    fn allowance(&self) -> Allowance {
        budget(&self.budget).allowance
    }

    fn set_budget(&self, bytes: u64) {
        let bytes = bytes.clamp(BUDGETS[0] * MB, BUDGETS[1] * MB);
        let mut state = budget(&self.budget);
        state.bytes = bytes;
        state.unsaved = Some(bytes);
        state.allow();
    }

    fn set_fog(&self, fog: Fog) {
        *self.fog.lock().unwrap_or_else(|e| e.into_inner()) = fog;
    }
}

/// The speed of the flight at start, the slowest and the fastest the wheel sets, in yards a
/// second, and how many times faster it goes while its hotkey is held.
const SPEED: f32 = 30.0;
const SPEEDS: [f32; 2] = [1.0, 2_000.0];
const FASTER: f32 = 4.0;

/// The hotkeys of the camera.
struct CameraKeys {
    forward: Hotkey,
    back: Hotkey,
    left: Hotkey,
    right: Hotkey,
    up: Hotkey,
    down: Hotkey,
    faster: Hotkey,
    orbit: Hotkey,
}

impl CameraKeys {
    fn declare(reg: &mut Registrar) -> Self {
        let mut held = |name, label, keys| reg.hotkey(name, label, HotkeyKind::Hold, keys);
        Self {
            forward: held("fly_forward", "Fly forward", Keys::key(egui::Key::Z)),
            back: held("fly_back", "Fly back", Keys::key(egui::Key::S)),
            left: held("fly_left", "Fly left", Keys::key(egui::Key::Q)),
            right: held("fly_right", "Fly right", Keys::key(egui::Key::D)),
            up: held("fly_up", "Fly up", Keys::key(egui::Key::E)),
            down: held("fly_down", "Fly down", Keys::key(egui::Key::A)),
            faster: held("fly_faster", "Fly faster", Keys::SHIFT),
            orbit: held("orbit", "Turn around the target with the drag", Keys::ALT),
        }
    }

    /// The flight asked now, forward, right and up: each -1, 0 or 1.
    fn flight(&self, ctx: &egui::Context) -> Vec3 {
        let axis =
            |plus: &Hotkey, minus: &Hotkey| f32::from(u8::from(plus.held(ctx))) - f32::from(u8::from(minus.held(ctx)));
        Vec3::new(
            axis(&self.forward, &self.back),
            axis(&self.right, &self.left),
            axis(&self.up, &self.down),
        )
    }

    /// What the caption says of them.
    fn caption(&self) -> String {
        let flight = [&self.forward, &self.left, &self.back, &self.right, &self.up, &self.down];
        let names: Vec<String> = flight.iter().map(|hotkey| hotkey.keys().to_string()).collect();
        format!(
            "right drag: look · {} + right drag: orbit · {}: fly · {}: faster",
            self.orbit.keys(),
            names.join(" "),
            self.faster.keys()
        )
    }
}

impl Default for CameraKeys {
    fn default() -> Self {
        Self::declare(&mut Registrar::default())
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
    keys: CameraKeys,
    /// The speed of the flight, in yards a second.
    speed: f32,
    stats: Stats,
    /// The timer of the GPU, when the device has timestamps; when the last frame was drawn; and
    /// whether the statistics are shown.
    timer: Option<GpuTimer>,
    last_frame: Option<Instant>,
    show_stats: bool,
    /// What the layers wrote over the view at the last frame, with the transform it was drawn with.
    labels: (Mat4, Vec<Label>),
    budget: Budget,
    /// The fog the layers draw with, as the terrain sets it.
    fog: Arc<Mutex<Fog>>,
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
            keys: CameraKeys::default(),
            speed: SPEED,
            stats: Stats::default(),
            timer: None,
            last_frame: None,
            show_stats: false,
            labels: (Mat4::IDENTITY, Vec::new()),
            budget: Arc::default(),
            fog: Arc::default(),
        }
    }
}

impl Module for ViewportModule {
    fn register(&mut self, reg: &mut Registrar) {
        let service: viewport::Handle = Arc::new(Service {
            layers: self.layers.clone(),
            frames: self.frames.clone(),
            budget: self.budget.clone(),
            fog: self.fog.clone(),
        });
        reg.panel("view", "3D View", DockArea::Center)
            .provide(viewport::SERVICE, service)
            .subscribe(MODULE_FAILED_TOPIC)
            .menu_item("View", "Reset camera", "reset_camera")
            .menu_item("View", "Statistics", STATISTICS);
        self.keys = CameraKeys::declare(reg);

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

    fn init(&mut self, ctx: &mut Context) {
        self.show_stats = ctx
            .setting(STATISTICS)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let mut state = budget(&self.budget);
        state.bytes = ctx
            .setting(BUDGET)
            .and_then(|value| value.as_u64())
            .unwrap_or_else(|| default_budget(ctx.gpu_memory()))
            .clamp(BUDGETS[0], BUDGETS[1])
            * MB;
        state.allow();
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(gpu) = ctx.gpu().cloned() else {
            ui.label("No GPU device is available.");
            return;
        };
        let size = ui.available_size().max(egui::vec2(1.0, 1.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        self.steer(ui, &response);

        let pixels = size * ui.ctx().pixels_per_point();
        let pixels = [pixels.x.round().max(1.0) as u32, pixels.y.round().max(1.0) as u32];
        self.ensure_targets(&gpu, pixels);
        self.render(&gpu, pixels, ctx);

        let targets = self.targets.as_ref().expect("created above");
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(targets.texture_id, rect, uv, egui::Color32::WHITE);
        let caption = format!(
            "{} layers · {} · wheel: speed {:.0} yd/s",
            lock(&self.layers).layers.len(),
            self.keys.caption(),
            self.speed
        );
        ui.painter().text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            caption,
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(150),
        );
        let painter = ui.painter_at(rect);
        for label in &self.labels.1 {
            let Some(at) = project(&self.labels.0, rect, label.position) else {
                continue;
            };
            let [r, g, b, a] = label.colour;
            let colour = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
            let galley = painter.layout_no_wrap(label.text.clone(), egui::FontId::proportional(11.0), colour);
            let at = at - egui::vec2(galley.size().x / 2.0, galley.size().y + 4.0);
            painter.rect_filled(
                egui::Rect::from_min_size(at, galley.size()).expand(2.0),
                2.0,
                egui::Color32::from_black_alpha(140),
            );
            painter.galley(at, galley, colour);
        }
        if self.show_stats {
            let colour = egui::Color32::from_gray(225);
            let allowance = budget(&self.budget).allowance;
            let text = self
                .stats
                .text(self.timer.is_some(), stats::process_memory(), &allowance);
            let galley = painter.layout_no_wrap(text, egui::FontId::monospace(11.0), colour);
            let at = rect.left_top() + egui::vec2(8.0, 8.0);
            let back = egui::Rect::from_min_size(at, galley.size()).expand(4.0);
            painter.rect_filled(back, 3.0, egui::Color32::from_black_alpha(170));
            painter.galley(at, galley, colour);
        }
        if let Some(bytes) = budget(&self.budget).unsaved.take() {
            ctx.set_setting(BUDGET, json!(bytes / MB));
        }
        ui.ctx().request_repaint();
    }

    fn on_event(&mut self, event: &Event, _ctx: &mut Context) {
        if event.topic == MODULE_FAILED_TOPIC
            && let Some(id) = event.payload.get("id").and_then(|v| v.as_str())
        {
            remove(&self.layers, id);
            let mut state = budget(&self.budget);
            if state.demands.remove(id).is_some() {
                state.allow();
            }
        }
    }

    fn on_menu(&mut self, action: &str, ctx: &mut Context) {
        match action {
            "reset_camera" => *camera(&self.camera) = OrbitCamera::default(),
            STATISTICS => {
                self.show_stats = !self.show_stats;
                ctx.set_setting(STATISTICS, json!(self.show_stats));
            }
            _ => {}
        }
    }
}

impl ViewportModule {
    /// Moves the camera as the user asks: the right or middle drag looks, or turns around the
    /// target while the orbit hotkey is held; with the pointer over the view, the hotkeys fly and
    /// the wheel sets the speed. A left click does nothing: it is kept for the tools to come.
    fn steer(&mut self, ui: &egui::Ui, response: &egui::Response) {
        let ctx = ui.ctx();
        let mut camera = camera(&self.camera);
        if response.dragged_by(egui::PointerButton::Secondary) || response.dragged_by(egui::PointerButton::Middle) {
            if self.keys.orbit.held(ctx) {
                camera.orbit(response.drag_delta());
            } else {
                camera.look(response.drag_delta());
            }
        }
        if !(response.hovered() || response.dragged()) {
            return;
        }
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            self.speed = (self.speed * (scroll * 0.002).exp()).clamp(SPEEDS[0], SPEEDS[1]);
        }
        let flight = self.keys.flight(ctx);
        if flight != Vec3::ZERO {
            let speed = self.speed * if self.keys.faster.held(ctx) { FASTER } else { 1.0 };
            let seconds = ui.input(|i| i.stable_dt).min(0.1);
            camera.fly(flight.normalize() * speed * seconds);
        }
    }

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
        let view = view(
            &self.camera,
            size,
            self.start.elapsed().as_secs_f32(),
            *self.fog.lock().unwrap_or_else(|e| e.into_inner()),
        );
        let new_device = self.device.as_ref() != Some(&gpu.device);
        if new_device {
            self.device = Some(gpu.device.clone());
            self.timer = GpuTimer::new(&gpu.device, &gpu.queue);
        }
        let targets = self.targets.as_ref().expect("created before rendering");
        let grid = self.grid.get_or_insert_with(|| Grid::new(&gpu.device, &TARGET));
        grid.update(&gpu.queue, &view);
        let pass = PassTargets {
            colour: &targets.msaa,
            resolve: Some(&targets.resolved),
            depth: &targets.depth,
        };
        let drawn = draw_frame(
            &self.layers,
            gpu,
            &view,
            new_device,
            &pass,
            Some(grid),
            self.timer.as_mut(),
        );
        self.labels = (view.view_proj, drawn.labels);
        for (owner, message) in drawn.failures {
            ctx.report_failure(&owner, &message);
        }
        self.sample(gpu, drawn.timings, drawn.submit);
        self.signal_frame(view.time);
    }

    /// Notes the statistics of the frame just submitted, and the times of the GPU come back.
    fn sample(&mut self, gpu: &egui_wgpu::RenderState, layers: Vec<LayerTiming>, submit: Duration) {
        let now = Instant::now();
        // A pause of the view is not a frame's time.
        let interval = self
            .last_frame
            .map(|last| now - last)
            .filter(|interval| *interval < Duration::from_secs(1));
        self.last_frame = Some(now);
        let sample = Sample {
            interval,
            prepare: layers.iter().map(|layer| layer.prepare).sum(),
            record: layers.iter().filter_map(|layer| layer.record).sum(),
            submit,
            layers,
        };
        self.stats.push(now, sample);
        if let Some(timer) = &mut self.timer {
            for frame in timer.collect(&gpu.device) {
                self.stats.push_gpu(now, frame);
            }
        }
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

/// What a frame drew: the layers that panicked or failed, with why; what each layer cost and drew;
/// what they write over the view; and the time of recording the pass and submitting, the drawing of
/// the layers in it left out.
struct Drawn {
    failures: Vec<(String, String)>,
    timings: Vec<LayerTiming>,
    labels: Vec<Label>,
    submit: Duration,
}

/// The textures the pass of a frame draws into.
struct PassTargets<'a> {
    colour: &'a wgpu::TextureView,
    resolve: Option<&'a wgpu::TextureView>,
    depth: &'a wgpu::TextureView,
}

/// What a layer gives its frame: its bundle, or none for its drawing in the pass; what it computes;
/// its number among the layers timed on the GPU; and what it cost.
struct Prepared {
    bundle: Option<wgpu::RenderBundle>,
    computed: Option<wgpu::CommandBuffer>,
    timed: Option<u32>,
    prepare: Duration,
    record: Option<Duration>,
}

/// Where `position` of the world falls in `rect` through `view_proj`; none behind the eye or out
/// of the view.
fn project(view_proj: &Mat4, rect: egui::Rect, position: Vec3) -> Option<egui::Pos2> {
    let clip = *view_proj * position.extend(1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let [x, y] = [clip.x / clip.w, clip.y / clip.w];
    if !(-1.0..=1.0).contains(&x) || !(-1.0..=1.0).contains(&y) {
        return None;
    }
    Some(egui::pos2(
        rect.left() + (x + 1.0) / 2.0 * rect.width(),
        rect.top() + (1.0 - y) / 2.0 * rect.height(),
    ))
}

/// Draws a frame: each layer prepared, its computing recorded into an encoder of its own and its
/// bundle recorded, or kept for its version unless the device is `new_device`; then the pass into
/// `targets`: `grid`, then the layers in their order, their bundles run or their drawing recorded
/// in it; everything submitted, the computing first. The layers that panicked or failed are removed.
fn draw_frame(
    layers: &Layers,
    gpu: &egui_wgpu::RenderState,
    view: &View,
    new_device: bool,
    targets: &PassTargets,
    grid: Option<&Grid>,
    mut timer: Option<&mut GpuTimer>,
) -> Drawn {
    // Layers may be added or removed meanwhile, by a layer or by another thread: take the list
    // out, then put it back in front, without the layers removed in between.
    let mut entries = {
        let mut list = lock(layers);
        list.drawing = true;
        std::mem::take(&mut list.layers)
    };
    if let Some(timer) = timer.as_deref_mut() {
        timer.begin();
    }
    let mut failures = Vec::new();
    let mut prepared = Vec::with_capacity(entries.len());
    entries.retain_mut(|entry| {
        if new_device {
            entry.kept = None;
        }
        match prepare_layer(entry, gpu, view, timer.as_deref_mut()) {
            Ok(layer) => {
                prepared.push(layer);
                true
            }
            Err(message) => {
                failures.push((entry.owner.clone(), message));
                false
            }
        }
    });

    let submitting = Instant::now();
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("viewport"),
    });
    let mut panicked: Vec<Option<String>> = vec![None; entries.len()];
    let mut drawing_in_pass = Duration::ZERO;
    {
        let timestamp_writes = timer.as_deref().and_then(GpuTimer::writes);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("viewport"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: targets.colour,
                depth_slice: None,
                resolve_target: targets.resolve,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(BACKGROUND),
                    store: if targets.resolve.is_some() {
                        wgpu::StoreOp::Discard
                    } else {
                        wgpu::StoreOp::Store
                    },
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: targets.depth,
                depth_ops: Some(wgpu::Operations {
                    // Reverse Z: infinity is 0.
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(grid) = grid {
            grid.draw(&mut pass);
        }
        for ((entry, layer), panic) in entries.iter_mut().zip(&mut prepared).zip(&mut panicked) {
            if let (Some(timer), Some(timed)) = (timer.as_deref(), layer.timed) {
                timer.drawing(&mut pass, timed, false);
            }
            match &layer.bundle {
                Some(bundle) => pass.execute_bundles([bundle]),
                None => {
                    let started = Instant::now();
                    let drawn = catch_unwind(AssertUnwindSafe(|| {
                        entry.layer.draw_pass(gpu, &TARGET, view, &mut pass);
                    }));
                    let took = started.elapsed();
                    drawing_in_pass += took;
                    layer.record = Some(took);
                    if let Err(payload) = drawn {
                        *panic = Some(format!(
                            "its viewport layer panicked drawing in the pass: {}",
                            panic_text(payload)
                        ));
                    }
                }
            }
            if let (Some(timer), Some(timed)) = (timer.as_deref(), layer.timed) {
                timer.drawing(&mut pass, timed, true);
            }
        }
    }
    if let Some(timer) = timer.as_deref() {
        timer.resolve(&mut encoder);
    }
    // A GPU error in the pass is learnt here only: it is put on the layers drawn in it.
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let finished = catch_unwind(AssertUnwindSafe(move || encoder.finish()));
    let error = resolved(scope.pop()).flatten();
    let failed_pass = match (&finished, error) {
        (Err(payload), _) => Some(format!(
            "the pass it drew in panicked once finished: {}",
            panic_text_ref(payload)
        )),
        (Ok(_), Some(error)) => Some(format!("a GPU error in the pass it drew in: {error}")),
        (Ok(_), None) => None,
    };
    let computed: Vec<wgpu::CommandBuffer> = prepared.iter_mut().filter_map(|layer| layer.computed.take()).collect();
    match finished.ok().filter(|_| failed_pass.is_none()) {
        Some(frame) => {
            gpu.queue.submit(computed.into_iter().chain([frame]));
            if let Some(timer) = timer {
                timer.submitted();
            }
        }
        None => {
            gpu.queue.submit(computed);
            // Not timed: what its timestamps would read was not submitted.
            if let Some(timer) = timer {
                timer.begin();
            }
        }
    }
    let submit = submitting.elapsed().saturating_sub(drawing_in_pass);

    let mut timings = Vec::with_capacity(entries.len());
    let mut labels = Vec::new();
    let mut index = 0;
    entries.retain_mut(|entry| {
        let (layer, panic) = (&prepared[index], panicked[index].take());
        index += 1;
        let failure = panic.or_else(|| failed_pass.clone().filter(|_| layer.bundle.is_none()));
        if let Some(message) = failure {
            failures.push((entry.owner.clone(), message));
            return false;
        }
        match observe(entry, layer) {
            Ok((timing, mut written)) => {
                timings.push(timing);
                labels.append(&mut written);
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
    Drawn {
        failures,
        timings,
        labels,
        submit,
    }
}

/// Prepares one layer and records its computing, then gives the bundle kept for its version, or
/// records it, each inside a validation error scope; or, for a layer drawn in the pass, no bundle.
fn prepare_layer(
    entry: &mut Entry,
    gpu: &egui_wgpu::RenderState,
    view: &View,
    timer: Option<&mut GpuTimer>,
) -> Result<Prepared, String> {
    let started = Instant::now();
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let layer = entry.layer.as_mut();
    let prepared = catch_unwind(AssertUnwindSafe(|| {
        layer.prepare(gpu, view);
        (layer.version(), layer.drawing())
    }));
    let error = resolved(scope.pop()).flatten();
    let (version, drawing) = match (prepared, error) {
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
        (Ok(prepared), None) => prepared,
    };
    let (computed, timed) = compute(entry, gpu, view, timer)?;
    let prepare = started.elapsed();
    if drawing == Drawing::Pass {
        entry.kept = None;
        return Ok(Prepared {
            bundle: None,
            computed: Some(computed),
            timed,
            prepare,
            record: None,
        });
    }
    let (bundle, record) = match (version, &entry.kept) {
        (Some(version), Some((kept, bundle))) if *kept == version => (bundle.clone(), None),
        _ => {
            let recording = Instant::now();
            let bundle = record(&entry.owner, entry.layer.as_mut(), gpu, view)?;
            entry.kept = version.map(|version| (version, bundle.clone()));
            (bundle, Some(recording.elapsed()))
        }
    };
    Ok(Prepared {
        bundle: Some(bundle),
        computed: Some(computed),
        timed,
        prepare,
        record,
    })
}

/// The computing of a layer, recorded into an encoder of its own between its timestamps when the
/// frame times it apart, and finished inside a validation error scope; with its number among the
/// layers timed.
fn compute(
    entry: &mut Entry,
    gpu: &egui_wgpu::RenderState,
    view: &View,
    timer: Option<&mut GpuTimer>,
) -> Result<(wgpu::CommandBuffer, Option<u32>), String> {
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some(&entry.owner),
    });
    let timed = timer.and_then(|timer| timer.layer(&entry.owner).map(|number| (timer, number)));
    if let Some((timer, number)) = &timed {
        timer.computing(&mut encoder, *number, false);
    }
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let layer = entry.layer.as_mut();
    let recorded = catch_unwind(AssertUnwindSafe(|| layer.compute(gpu, view, &mut encoder)));
    let number = timed.map(|(timer, number)| {
        timer.computing(&mut encoder, number, true);
        number
    });
    let finished = catch_unwind(AssertUnwindSafe(move || encoder.finish()));
    let error = resolved(scope.pop()).flatten();
    match (recorded, finished, error) {
        (Err(payload), _, _) => Err(format!(
            "its viewport layer panicked while computing: {}",
            panic_text(payload)
        )),
        (Ok(()), Err(payload), _) => Err(format!(
            "its viewport layer recorded invalid GPU commands while computing: {}",
            panic_text(payload)
        )),
        (Ok(()), Ok(_), Some(error)) => Err(format!(
            "its viewport layer caused a GPU error while computing: {error}"
        )),
        (Ok(()), Ok(buffer), None) => Ok((buffer, number)),
    }
}

/// What a layer drew and writes over the view, once drawn, with what it cost.
fn observe(entry: &Entry, layer: &Prepared) -> Result<(LayerTiming, Vec<Label>), String> {
    let drawn = entry.layer.as_ref();
    let stats = catch_unwind(AssertUnwindSafe(|| drawn.stats())).map_err(|payload| {
        format!(
            "its viewport layer panicked giving its statistics: {}",
            panic_text(payload)
        )
    })?;
    let labels = catch_unwind(AssertUnwindSafe(|| drawn.labels()))
        .map_err(|payload| format!("its viewport layer panicked giving its labels: {}", panic_text(payload)))?;
    Ok((
        LayerTiming {
            owner: entry.owner.clone(),
            prepare: layer.prepare,
            record: layer.record,
            stats,
        },
        labels,
    ))
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
    panic_text_ref(&payload)
}

fn panic_text_ref(payload: &Box<dyn Any + Send>) -> String {
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
    use uniwow_api::viewport::{self, Allowance, Drawing, Frame, Label, Layer, LayerStats, Target, View};
    use uniwow_api::{egui, egui_wgpu, wgpu};

    use uniwow_api::egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
    use uniwow_api::glam::Vec3;
    use uniwow_api::hotkey::Keys;

    use super::stats::{GpuFrame, GpuTimer, LayerTiming, Sample, Stats};
    use super::{
        Camera, CameraKeys, Drawn, Entry, FrameSignal, Layers, PassTargets, TARGET, ViewportModule, camera, draw_frame,
        frame, lock, look_at, project, resolved, view,
    };

    /// A device of the software adapter of the system, with timestamps, inside encoders and passes
    /// too, when it offers them, as the editor asks for them; or none where there is no such
    /// adapter.
    fn gpu() -> Option<egui_wgpu::RenderState> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
            force_fallback_adapter: true,
            ..Default::default()
        }))?
        .ok()?;
        let (device, queue) = resolved(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: adapter.features()
                & (wgpu::Features::TIMESTAMP_QUERY
                    | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
                    | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES),
            ..Default::default()
        }))?
        .ok()?;
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

        fn stats(&self) -> LayerStats {
            LayerStats {
                draws: self.counts.drawn.load(Ordering::Relaxed) as u64,
                ..LayerStats::default()
            }
        }

        fn labels(&self) -> Vec<Label> {
            vec![Label {
                position: Vec3::ZERO,
                text: format!("prepared {}", self.counts.prepared.load(Ordering::Relaxed)),
                colour: [255; 4],
            }]
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
        let view = view(&Camera::default(), [64, 64], 0.0, viewport::Fog::default());
        let targets = Targets::new(&gpu);
        let frames = |new_device| {
            let Drawn {
                failures,
                timings,
                labels,
                ..
            } = targets.draw(&layers, &gpu, &view, new_device, None);
            assert!(failures.is_empty(), "{failures:?}");
            assert_eq!(timings.len(), 2, "what each layer cost and drew");
            assert_eq!(labels.len(), 2, "what each layer writes over the view");
            assert!(labels.iter().all(|label| label.text.starts_with("prepared ")));
            timings
        };
        for frame in 0..3u64 {
            let timings = frames(false);
            assert_eq!(timings[0].owner, "terrain");
            assert_eq!(timings[0].record.is_some(), frame == 0, "a bundle kept is not recorded");
            assert!(timings[1].record.is_some());
            assert_eq!(timings[1].stats.draws, frame + 1, "its statistics after its drawing");
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
        let Drawn { failures, timings, .. } = targets.draw(&layers, &gpu, &view, false, None);
        assert_eq!(timings.len(), 2);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "faulty");
        assert!(failures[0].1.contains("panicked while preparing"), "{}", failures[0].1);
        assert_eq!(lock(&layers).layers.len(), 2, "the faulty layer is removed");
    }

    #[test]
    fn the_fog_set_is_given_to_every_layer_with_the_view_of_the_next_frame() {
        let fog = Arc::new(std::sync::Mutex::new(viewport::Fog::default()));
        let service = super::Service {
            layers: Layers::default(),
            frames: Arc::default(),
            budget: Default::default(),
            fog: fog.clone(),
        };
        use uniwow_api::viewport::Viewport as _;
        let set = viewport::Fog {
            colour: [1.0, 0.0, 0.0],
            start: 10.0,
            middle: 20.0,
            end: 30.0,
        };
        service.set_fog(set);
        let view = super::view(&Camera::default(), [64, 64], 0.0, *fog.lock().unwrap());
        assert_eq!(view.fog, set);
        assert_eq!(
            view.sun,
            viewport::Sun::default(),
            "the sun of before, until the lights of the maps"
        );
    }

    #[test]
    fn the_budget_of_the_view_is_half_the_memory_of_the_gpu_by_default_and_follows_what_is_told() {
        assert_eq!(super::default_budget(Some(12 << 30)), 6144, "a GPU of 12 GB");
        assert_eq!(super::default_budget(None), 1024, "not told");
        assert_eq!(super::default_budget(Some(64 << 20)), 64, "never under the least");
        assert_eq!(super::default_budget(Some(1 << 40)), 65_536, "nor over the most");

        let shared = super::Budget::default();
        let service = super::Service {
            layers: Layers::default(),
            frames: Arc::default(),
            budget: shared.clone(),
            fog: Arc::default(),
        };
        use uniwow_api::viewport::Viewport as _;
        service.set_budget(100 << 20);
        let mut terrain = viewport::Demand {
            fixed: 20 << 20,
            ..viewport::Demand::default()
        };
        terrain.wanted[..10].fill(10 << 20);
        let allowance = service.tell_budget("terrain", terrain.clone());
        assert_eq!(allowance.load, 7.0 * viewport::BAND, "at once, with what was told");
        assert_eq!(service.allowance(), allowance);
        let allowance = service.tell_budget("models", terrain);
        assert_eq!(
            allowance.load,
            2.0 * viewport::BAND,
            "40 fixed, then 20 a band within 90"
        );
        service.remove_layers("models");
        assert_eq!(
            service.allowance().load,
            7.0 * viewport::BAND,
            "given back once it is gone"
        );
        assert_eq!(super::budget(&shared).unsaved, Some(100 << 20), "kept in the settings");
    }

    #[test]
    fn a_label_is_written_where_its_point_falls_in_the_view_and_not_behind_the_eye() {
        let shared = Camera::default();
        let view = view(&shared, [200, 100], 0.0, viewport::Fog::default());
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(200.0, 100.0));
        let target = camera(&shared).target();
        let centre = project(&view.view_proj, rect, target).expect("the point looked at");
        assert!((centre - rect.center()).length() < 0.5, "{centre:?}");
        let eye = view.eye;
        let behind = eye + (eye - target);
        assert_eq!(project(&view.view_proj, rect, behind), None);
        let aside = target + (target - eye).cross(Vec3::Z).normalize() * 100_000.0;
        assert_eq!(project(&view.view_proj, rect, aside), None, "out of the view");
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
        let drawn = view(&shared, [640, 480], 0.0, viewport::Fog::default());
        assert_eq!(drawn.eye, camera(&shared).eye());
    }

    /// A frame of `ctx` with `events`, a view 400 × 300 at its top left steered by `module`.
    fn steered(module: &mut ViewportModule, ctx: &egui::Context, events: Vec<Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let (_, response) = ui.allocate_exact_size(vec2(400.0, 300.0), egui::Sense::click_and_drag());
            module.steer(ui, &response);
        });
        output.textures_delta.clear();
    }

    fn key(key: Key, pressed: bool) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    /// A drag of `button` in the view, 120 points to the right, `held` down meanwhile.
    fn drag(module: &mut ViewportModule, ctx: &egui::Context, button: PointerButton, held: Modifiers) {
        let at = Pos2::new(100.0, 100.0);
        let press = |pos, pressed| Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: held,
        };
        steered(
            module,
            ctx,
            vec![Event::ModifiersChanged(held), Event::PointerMoved(at)],
        );
        steered(module, ctx, vec![press(at, true)]);
        steered(module, ctx, vec![Event::PointerMoved(at + vec2(60.0, 0.0))]);
        steered(module, ctx, vec![Event::PointerMoved(at + vec2(120.0, 0.0))]);
        steered(
            module,
            ctx,
            vec![
                press(at + vec2(120.0, 0.0), false),
                Event::ModifiersChanged(Modifiers::NONE),
            ],
        );
    }

    fn eye_and_target(module: &ViewportModule) -> (Vec3, Vec3) {
        let camera = camera(&module.camera);
        (camera.eye(), camera.target())
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn the_right_drag_looks_or_orbits_while_held_and_the_left_does_nothing() {
        let ctx = egui::Context::default();
        let mut module = ViewportModule::default();
        let (eye, target) = eye_and_target(&module);
        drag(&mut module, &ctx, PointerButton::Primary, Modifiers::NONE);
        assert_eq!(eye_and_target(&module), (eye, target), "a left drag does nothing");
        drag(&mut module, &ctx, PointerButton::Secondary, Modifiers::NONE);
        let (looked_eye, looked) = eye_and_target(&module);
        assert!(close(looked_eye, eye) && !close(looked, target), "looks: the eye stays");
        drag(&mut module, &ctx, PointerButton::Middle, Modifiers::ALT);
        let (turned, kept) = eye_and_target(&module);
        assert!(
            close(kept, looked) && !close(turned, eye),
            "Alt: turns around the target"
        );
    }

    #[test]
    fn its_hotkeys_fly_the_camera_while_the_pointer_is_over_the_view() {
        let ctx = egui::Context::default();
        let keys = CameraKeys::default();
        let flight = |events: Vec<Event>| {
            let mut asked = Vec3::ZERO;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| asked = keys.flight(ui.ctx()),
            );
            output.textures_delta.clear();
            asked
        };
        assert_eq!(flight(vec![key(Key::Z, true)]), Vec3::X, "Z forward");
        assert_eq!(flight(vec![key(Key::D, true)]), Vec3::new(1.0, 1.0, 0.0), "D right");
        assert_eq!(
            flight(vec![
                key(Key::Z, false),
                key(Key::D, false),
                key(Key::Q, true),
                key(Key::S, true)
            ]),
            Vec3::new(-1.0, -1.0, 0.0),
            "Q left, S back"
        );
        assert_eq!(
            flight(vec![key(Key::Q, false), key(Key::S, false), key(Key::E, true)]),
            Vec3::Z,
            "E up"
        );
        assert_eq!(flight(vec![key(Key::A, true)]), Vec3::ZERO, "A down, against E");
        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            flight(vec![
                key(Key::E, false),
                key(Key::A, false),
                key(Key::Z, true),
                Event::ModifiersChanged(ctrl)
            ]),
            Vec3::ZERO,
            "Ctrl+Z is Undo"
        );
        keys.forward.set_keys(Keys::key(Key::W));
        assert_eq!(
            flight(vec![Event::ModifiersChanged(Modifiers::NONE)]),
            Vec3::ZERO,
            "bound to W"
        );
        assert_eq!(flight(vec![key(Key::W, true)]), Vec3::X);

        let mut module = ViewportModule::default();
        let (eye, target) = eye_and_target(&module);
        steered(&mut module, &ctx, vec![key(Key::Z, true)]);
        assert_eq!(
            eye_and_target(&module),
            (eye, target),
            "the pointer is not over the view"
        );
        steered(&mut module, &ctx, vec![Event::PointerMoved(Pos2::new(100.0, 100.0))]);
        let (flown, ahead) = eye_and_target(&module);
        let forward = (target - eye).normalize();
        assert!((flown - eye).normalize().dot(forward) > 0.999, "forward along the view");
        assert!(close(ahead - flown, target - eye), "the eye and the target together");
    }

    #[test]
    fn the_wheel_sets_the_speed_and_shift_flies_four_times_faster() {
        let ctx = egui::Context::default();
        let mut module = ViewportModule::default();
        let at = Pos2::new(100.0, 100.0);
        let wheel = |y: f32| Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, y),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        };
        steered(&mut module, &ctx, vec![Event::PointerMoved(at), wheel(200.0)]);
        for _ in 0..30 {
            steered(&mut module, &ctx, Vec::new());
        }
        let faster = module.speed;
        assert!(faster > super::SPEED, "{faster}");
        steered(&mut module, &ctx, vec![wheel(-100_000.0)]);
        for _ in 0..30 {
            steered(&mut module, &ctx, Vec::new());
        }
        assert_eq!(module.speed, super::SPEEDS[0], "never slower than the slowest");

        let step = |module: &mut ViewportModule, events| {
            let (eye, _) = eye_and_target(module);
            steered(module, &ctx, events);
            (eye_and_target(module).0 - eye).length()
        };
        let slow = step(&mut module, vec![key(Key::Z, true)]);
        let fast = step(&mut module, vec![Event::ModifiersChanged(Modifiers::SHIFT)]);
        assert!(
            slow > 0.0 && (fast / slow - super::FASTER).abs() < 0.01,
            "{slow} then {fast}"
        );
    }

    #[test]
    fn the_gpu_is_timed_by_its_timestamps_without_waiting_each_layer_apart_when_the_device_can() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let Some(mut timer) = GpuTimer::new(&gpu.device, &gpu.queue) else {
            assert!(!gpu.device.features().contains(wgpu::Features::TIMESTAMP_QUERY));
            eprintln!("skipped: the device has no timestamps");
            return;
        };
        let layers = Layers::default();
        put(
            &layers,
            "computing",
            Painter::new(Drawing::Pass, [0.0, 1.0, 0.0, 1.0]).computing(),
        );
        add(&layers, "bundled", &Counts::default(), false);
        let targets = Targets::new(&gpu);
        let view = view(&Camera::default(), [8, 8], 0.0, viewport::Fog::default());
        let mut frames: Vec<GpuFrame> = Vec::new();
        for _ in 0..6 {
            let drawn = targets.draw(&layers, &gpu, &view, false, Some(&mut timer));
            assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
            frames.extend(timer.collect(&gpu.device));
        }
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        frames.extend(timer.collect(&gpu.device));
        assert!(
            frames.len() >= 4,
            "{frames:?}: the frames finding a buffer free are timed"
        );
        assert!(
            frames.iter().all(|frame| (0.0..1000.0).contains(&frame.total)),
            "{frames:?}"
        );
        let inside = wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
        if gpu.device.features().contains(inside) {
            for frame in &frames {
                let owners: Vec<&str> = frame.layers.iter().map(|(owner, _, _)| owner.as_str()).collect();
                assert_eq!(owners, ["computing", "bundled"], "each layer timed, in its order");
                assert!(
                    frame
                        .layers
                        .iter()
                        .all(|(_, compute, draw)| (0.0..1000.0).contains(compute) && (0.0..1000.0).contains(draw))
                );
            }
        }
    }

    /// A full screen of `colour`, drawn without depth by a pipeline of the view's target; the
    /// colour read from a storage buffer its computing writes, for a layer that computes.
    const PAINT: &str = r#"
@group(0) @binding(0) var<storage, read> colour: vec4<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.5, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return colour;
}

@group(0) @binding(0) var<storage, read_write> written: vec4<f32>;

@compute @workgroup_size(1)
fn cs_main() {
    written = vec4<f32>(0.0, 0.0, 1.0, 1.0);
}
"#;

    /// What a test layer painting the view does wrong, on purpose.
    #[derive(Clone, Copy, PartialEq)]
    enum Fault {
        None,
        PanicComputing,
        ErrorComputing,
        PanicDrawing,
        ErrorDrawing,
    }

    /// A layer painting the whole view with one colour, in a bundle or in the pass; or with the colour
    /// its computing writes, in the same frame.
    struct Painter {
        drawing: Drawing,
        colour: [f32; 4],
        computes: bool,
        fault: Fault,
        made: Option<(
            wgpu::RenderPipeline,
            wgpu::BindGroup,
            wgpu::ComputePipeline,
            wgpu::BindGroup,
            wgpu::Buffer,
        )>,
    }

    impl Painter {
        fn new(drawing: Drawing, colour: [f32; 4]) -> Self {
            Self {
                drawing,
                colour,
                computes: false,
                fault: Fault::None,
                made: None,
            }
        }

        fn computing(mut self) -> Self {
            self.computes = true;
            self
        }

        fn faulty(mut self, fault: Fault) -> Self {
            self.fault = fault;
            self
        }

        fn made(
            &mut self,
            gpu: &egui_wgpu::RenderState,
        ) -> &(
            wgpu::RenderPipeline,
            wgpu::BindGroup,
            wgpu::ComputePipeline,
            wgpu::BindGroup,
            wgpu::Buffer,
        ) {
            let colour = self.colour;
            self.made.get_or_insert_with(|| {
                let device = &gpu.device;
                let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: None,
                    source: wgpu::ShaderSource::Wgsl(PAINT.into()),
                });
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 16,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                gpu.queue
                    .write_buffer(&buffer, 0, uniwow_api::bytemuck::cast_slice(&colour));
                let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: None,
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: TARGET.depth_format,
                        depth_write_enabled: Some(false),
                        depth_compare: Some(wgpu::CompareFunction::Always),
                        stencil: Default::default(),
                        bias: Default::default(),
                    }),
                    multisample: wgpu::MultisampleState {
                        count: TARGET.sample_count,
                        ..Default::default()
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_main"),
                        compilation_options: Default::default(),
                        targets: &[Some(TARGET.color_format.into())],
                    }),
                    multiview_mask: None,
                    cache: None,
                });
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                let compute = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: None,
                    layout: None,
                    module: &shader,
                    entry_point: Some("cs_main"),
                    compilation_options: Default::default(),
                    cache: None,
                });
                let written = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &compute.get_bind_group_layout(0),
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                (pipeline, group, compute, written, buffer)
            })
        }
    }

    impl Layer for Painter {
        fn compute(&mut self, gpu: &egui_wgpu::RenderState, _view: &View, encoder: &mut wgpu::CommandEncoder) {
            assert!(self.fault != Fault::PanicComputing, "its choice could not be made");
            let (fault, computes) = (self.fault, self.computes);
            let (_, _, compute, written, buffer) = self.made(gpu);
            if fault == Fault::ErrorComputing {
                // Not a multiple of 4: refused.
                encoder.clear_buffer(buffer, 1, None);
            }
            if computes {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(compute);
                pass.set_bind_group(0, written, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
        }

        fn drawing(&self) -> Drawing {
            self.drawing
        }

        fn draw<'a>(
            &'a mut self,
            gpu: &egui_wgpu::RenderState,
            _target: &Target,
            _view: &View,
            bundle: &mut wgpu::RenderBundleEncoder<'a>,
        ) {
            let (pipeline, group, ..) = self.made(gpu);
            bundle.set_pipeline(pipeline);
            bundle.set_bind_group(0, group, &[]);
            bundle.draw(0..3, 0..1);
        }

        fn draw_pass(
            &mut self,
            gpu: &egui_wgpu::RenderState,
            _target: &Target,
            _view: &View,
            pass: &mut wgpu::RenderPass<'_>,
        ) {
            assert!(self.fault != Fault::PanicDrawing, "its draws could not be made");
            let fault = self.fault;
            let (pipeline, group, ..) = self.made(gpu);
            if fault == Fault::ErrorDrawing {
                // No pipeline set: refused.
                pass.draw(0..3, 0..1);
                return;
            }
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    fn put(layers: &Layers, owner: &str, layer: Painter) {
        lock(layers).layers.push(Entry {
            owner: owner.to_owned(),
            layer: Box::new(layer),
            kept: None,
        });
    }

    /// The textures of the view, 8 × 8, multisampled as the view's, resolved to a texture read back.
    struct Targets {
        colour: wgpu::TextureView,
        resolved: wgpu::Texture,
        resolve: wgpu::TextureView,
        depth: wgpu::TextureView,
    }

    impl Targets {
        fn new(gpu: &egui_wgpu::RenderState) -> Self {
            let texture = |format, samples, usage| {
                gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: 8,
                        height: 8,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
            };
            let resolved = texture(
                TARGET.color_format,
                1,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            );
            Self {
                colour: texture(
                    TARGET.color_format,
                    TARGET.sample_count,
                    wgpu::TextureUsages::RENDER_ATTACHMENT,
                )
                .create_view(&Default::default()),
                resolve: resolved.create_view(&Default::default()),
                resolved,
                depth: texture(
                    TARGET.depth_format,
                    TARGET.sample_count,
                    wgpu::TextureUsages::RENDER_ATTACHMENT,
                )
                .create_view(&Default::default()),
            }
        }

        fn draw(
            &self,
            layers: &Layers,
            gpu: &egui_wgpu::RenderState,
            view: &View,
            new_device: bool,
            timer: Option<&mut GpuTimer>,
        ) -> Drawn {
            let targets = PassTargets {
                colour: &self.colour,
                resolve: Some(&self.resolve),
                depth: &self.depth,
            };
            draw_frame(layers, gpu, view, new_device, &targets, None, timer)
        }

        /// The pixel at the middle of the image resolved, RGBA.
        fn middle(&self, gpu: &egui_wgpu::RenderState) -> [u8; 4] {
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256 * 8,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(
                self.resolved.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(8),
                    },
                },
                wgpu::Extent3d {
                    width: 8,
                    height: 8,
                    depth_or_array_layers: 1,
                },
            );
            gpu.queue.submit([encoder.finish()]);
            buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let data = buffer.slice(..).get_mapped_range().expect("mapped").to_vec();
            let at = 4 * 256 + 4 * 4;
            [data[at], data[at + 1], data[at + 2], data[at + 3]]
        }
    }

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

    #[test]
    fn layers_drawn_in_the_pass_and_in_bundles_are_drawn_in_their_order_each_with_its_own_state() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(&Camera::default(), [8, 8], 0.0, viewport::Fog::default());
        let targets = Targets::new(&gpu);
        // The last drawn covers the view: a bundle after the pass, then the pass after a bundle,
        // then a pass after another pass and a bundle.
        for (order, wanted) in [
            (vec![(Drawing::Pass, GREEN), (Drawing::Bundle, RED)], [255, 0, 0, 255]),
            (vec![(Drawing::Bundle, RED), (Drawing::Pass, GREEN)], [0, 255, 0, 255]),
            (
                vec![(Drawing::Pass, RED), (Drawing::Bundle, GREEN), (Drawing::Pass, RED)],
                [255, 0, 0, 255],
            ),
        ] {
            let layers = Layers::default();
            for (number, (drawing, colour)) in order.iter().enumerate() {
                put(&layers, &format!("layer {number}"), Painter::new(*drawing, *colour));
            }
            for _ in 0..2 {
                let drawn = targets.draw(&layers, &gpu, &view, false, None);
                assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
                assert_eq!(targets.middle(&gpu), wanted);
                assert_eq!(drawn.timings.len(), order.len());
                for (timing, (drawing, _)) in drawn.timings.iter().zip(&order) {
                    if *drawing == Drawing::Pass {
                        assert!(timing.record.is_some(), "drawn in the pass at each frame");
                    }
                }
            }
        }
    }

    #[test]
    fn what_a_layer_computes_is_drawn_in_the_same_frame() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(&Camera::default(), [8, 8], 0.0, viewport::Fog::default());
        let targets = Targets::new(&gpu);
        let layers = Layers::default();
        put(&layers, "chosen", Painter::new(Drawing::Pass, RED).computing());
        let drawn = targets.draw(&layers, &gpu, &view, false, None);
        assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
        assert_eq!(targets.middle(&gpu), [0, 0, 255, 255], "the blue its computing wrote");
    }

    #[test]
    fn a_layer_failing_to_compute_or_to_draw_in_the_pass_is_removed_and_reported() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(&Camera::default(), [8, 8], 0.0, viewport::Fog::default());
        let targets = Targets::new(&gpu);
        for (fault, said) in [
            (Fault::PanicComputing, "panicked while computing"),
            (Fault::ErrorComputing, "caused a GPU error while computing"),
            (Fault::PanicDrawing, "panicked drawing in the pass"),
            (Fault::ErrorDrawing, "a GPU error in the pass it drew in"),
        ] {
            let layers = Layers::default();
            put(&layers, "kept", Painter::new(Drawing::Bundle, GREEN));
            put(&layers, "faulty", Painter::new(Drawing::Pass, RED).faulty(fault));
            let drawn = targets.draw(&layers, &gpu, &view, false, None);
            assert_eq!(drawn.failures.len(), 1, "{:?}", drawn.failures);
            assert_eq!(drawn.failures[0].0, "faulty");
            assert!(drawn.failures[0].1.contains(said), "{}", drawn.failures[0].1);
            let owners: Vec<String> = lock(&layers).layers.iter().map(|entry| entry.owner.clone()).collect();
            assert_eq!(owners, ["kept"], "the faulty layer removed, the other kept");
            let drawn = targets.draw(&layers, &gpu, &view, false, None);
            assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
            assert_eq!(targets.middle(&gpu), [0, 255, 0, 255], "the next frame drawn");
        }
    }

    #[test]
    fn the_statistics_give_the_frames_the_interface_the_gpu_and_each_layer_over_a_second() {
        let mut stats = Stats::default();
        let start = Instant::now();
        let layer = |owner: &str, steering_ms| LayerTiming {
            owner: owner.to_owned(),
            prepare: Duration::from_millis(1),
            record: None,
            stats: LayerStats {
                draws: 87,
                triangles: 2_150_000,
                bytes: 300 << 20,
                items: "85 tiles".to_owned(),
                steering: Duration::from_millis(steering_ms),
            },
        };
        for frame in 0..30u64 {
            let sample = Sample {
                interval: Some(Duration::from_millis(if frame == 29 { 40 } else { 20 })),
                prepare: Duration::from_millis(1),
                record: Duration::ZERO,
                submit: Duration::from_millis(1),
                layers: vec![layer("terrain", 2 + frame % 2)],
            };
            stats.push(start + Duration::from_millis(frame * 20), sample);
        }
        stats.push_gpu(
            start,
            GpuFrame {
                total: 3.0,
                layers: vec![("terrain".to_owned(), 0.25, 1.5)],
            },
        );
        let text = stats.text(true, None, &Allowance::default());
        assert!(text.starts_with("48 fps: a frame 20.7 ms, the longest 40.0"), "{text}");
        assert!(text.contains("view, interface thread: 2.00 ms"), "{text}");
        assert!(text.contains("GPU: 3.00 ms"), "{text}");
        assert!(
            text.contains("  GPU: computing 0.25 ms (0.25), drawing 1.50 (1.50)"),
            "{text}"
        );
        assert!(
            text.contains("terrain: 87 draws, 2.15 M triangles, 300 MB, 85 tiles"),
            "{text}"
        );
        assert!(
            text.contains(
                "interface 3.50 ms, the longest 4.00: steering 2.50 (3.00), prepare 1.00 (1.00), record 0.00 (0.00)"
            ),
            "{text}"
        );
        assert!(stats.text(false, None, &Allowance::default()).contains("not timed"));
        let memory = stats.text(true, Some((300 << 20, 200 << 20)), &Allowance::default());
        assert!(memory.contains("process: 300 MB in memory, 200 MB private"), "{memory}");
        assert!(!memory.contains("GPU budget"), "no budget told, none written");
        let budget = Allowance {
            budget: 1000 << 20,
            used: 300 << 20,
            limited: Some(viewport::BAND * 8.0),
            ..Allowance::default()
        };
        let limited = stats.text(true, None, &budget);
        assert!(
            limited.contains("GPU budget of the view: 300 of 1000 MB; reach limited to 2.0 tiles"),
            "{limited}"
        );
        let (working, private) = super::stats::process_memory().expect("Windows tells it");
        assert!(working > 1 << 20 && private > 1 << 20);

        // A second later, the frames before are no longer counted.
        let later = Sample {
            interval: Some(Duration::from_millis(10)),
            ..Sample::default()
        };
        stats.push(start + Duration::from_secs(3), later);
        assert!(
            stats.text(true, None, &Allowance::default()).starts_with("100 fps"),
            "{}",
            stats.text(true, None, &Allowance::default())
        );
    }
}
