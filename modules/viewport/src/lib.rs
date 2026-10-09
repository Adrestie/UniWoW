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
mod pyramid;
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
use uniwow_api::journal;
use uniwow_api::serde_json::{Value, json};
use uniwow_api::viewport::{
    self, Allowance, Demand, Drawing, Fog, Frame, Label, Layer, MAX_FRAME_WAIT, MapLight, Phase, Pyramid, Sun, Target,
    View,
};
use uniwow_api::{
    Context, DockArea, Event, MODULE_FAILED_TOPIC, Module, PropertyKind, PropertyValue, Registrar, SettingSpec, egui,
    egui_wgpu, log, wgpu,
};

use camera::{FOV, OrbitCamera, REACH};
use grid::Grid;
use pyramid::Builder;
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

/// The bundles of a layer, one for each phase.
type Bundles = [wgpu::RenderBundle; Phase::ALL.len()];

/// A layer, with the bundle kept for it and the version it was recorded at.
struct Entry {
    owner: String,
    layer: Box<dyn Layer>,
    /// Its bundles of the phases, kept with the version they were recorded at.
    kept: Option<(u64, Bundles)>,
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

/// The fog the terrain sets and the light the module of the light sets, with that module, which the
/// view draws with.
#[derive(Clone, Debug, Default)]
struct Lighting {
    fog: Fog,
    light: Option<(String, MapLight)>,
}

impl Lighting {
    /// The light `owner` gives; none takes back its own.
    fn give(&mut self, owner: &str, light: Option<MapLight>) {
        match light {
            Some(light) => self.light = Some((owner.to_owned(), light)),
            None if self.light.as_ref().is_some_and(|(by, _)| by == owner) => self.light = None,
            None => {}
        }
    }

    /// The fog and the sun of the view: the light's sun and colour of the fog over the terrain's
    /// fog, and the light's distances with the curve of the game when it gives them; the fixed light
    /// without it.
    fn resolved(&self) -> (Fog, Sun) {
        let Some((_, light)) = self.light else {
            return (self.fog, Sun::default());
        };
        let mut fog = Fog {
            colour: light.fog_colour,
            ..self.fog
        };
        if let Some([start, end]) = light.fog {
            fog = Fog {
                start,
                middle: (start + end) / 2.0,
                end,
                rate: viewport::fog_rate(start, end),
                ..fog
            };
        }
        (fog, light.sun)
    }
}

/// The camera and frame drawn, the camera locked once, with the fog and the sun set last.
fn view(shared: &Camera, size: [u32; 2], time: f32, (fog, sun): (Fog, Sun)) -> View {
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
        sun,
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

/// The setting of the budget in the window *Settings*, in the category *View*, its default
/// following `gpu_memory`.
fn budget_setting(gpu_memory: Option<u64>) -> SettingSpec {
    SettingSpec::integer(
        BUDGET,
        "GPU budget (MB)",
        [BUDGETS[0] as i64, BUDGETS[1] as i64],
        default_budget(gpu_memory) as i64,
    )
}

/// Implementation of the service, sharing the layer list, the frame signal and the budget with the
/// module.
struct Service {
    layers: Layers,
    frames: Arc<FrameSignal>,
    budget: Budget,
    lighting: Arc<Mutex<Lighting>>,
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
        self.lighting.lock().unwrap_or_else(|e| e.into_inner()).fog = fog;
    }

    fn set_light(&self, owner: &str, light: Option<MapLight>) {
        self.lighting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .give(owner, light);
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

/// The texture the frame is resolved into, of `extent` and `usage`: its view for the pass, which
/// encodes to sRGB, and the view egui shows, which reads the bytes as stored. egui takes the values
/// of a texture as already encoded: through a view of sRGB they would be decoded twice, and the
/// whole view darkened.
fn resolve_target(
    device: &wgpu::Device,
    extent: wgpu::Extent3d,
    usage: wgpu::TextureUsages,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
    let stored = TARGET.color_format.remove_srgb_suffix();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewport colour"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET.color_format,
        usage,
        view_formats: &[stored],
    });
    let resolve = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let shown = texture.create_view(&wgpu::TextureViewDescriptor {
        format: Some(stored),
        ..wgpu::TextureViewDescriptor::default()
    });
    (texture, resolve, shown)
}

/// Offscreen textures, recreated when the panel changes size.
struct Targets {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    resolved: wgpu::TextureView,
    depth: wgpu::TextureView,
    pyramid: pyramid::Pyramid,
    texture_id: egui::TextureId,
}

struct ViewportModule {
    layers: Layers,
    camera: Camera,
    targets: Option<Targets>,
    grid: Option<Grid>,
    /// What builds the pyramids of the depth, made once.
    pyramids: Option<Arc<Builder>>,
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
    pass: PassWatch,
    last_frame: Option<Instant>,
    show_stats: bool,
    /// The report of the allocator of the device last made, and when: its bytes allocated, those
    /// reserved and its blocks.
    allocator: Option<(Instant, Option<[u64; 3]>)>,
    /// What the layers wrote over the view at the last frame, with the transform it was drawn with.
    labels: (Mat4, Vec<Label>),
    budget: Budget,
    /// The fog the layers draw with, as the terrain sets it, and the light of the map.
    lighting: Arc<Mutex<Lighting>>,
}

impl Default for ViewportModule {
    fn default() -> Self {
        Self {
            layers: Arc::default(),
            camera: Arc::default(),
            targets: None,
            grid: None,
            pyramids: None,
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
            pass: PassWatch::default(),
            last_frame: None,
            show_stats: false,
            allocator: None,
            labels: (Mat4::IDENTITY, Vec::new()),
            budget: Arc::default(),
            lighting: Arc::default(),
        }
    }
}

impl Module for ViewportModule {
    fn register(&mut self, reg: &mut Registrar) {
        let service: viewport::Handle = Arc::new(Service {
            layers: self.layers.clone(),
            frames: self.frames.clone(),
            budget: self.budget.clone(),
            lighting: self.lighting.clone(),
        });
        let gpu_memory = reg.gpu_memory;
        reg.panel("view", "3D View", DockArea::Center)
            .settings("View", vec![budget_setting(gpu_memory)])
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
        state.bytes = budget_setting(ctx.gpu_memory()).value(ctx.setting(BUDGET).as_ref()) as u64 * MB;
        state.allow();
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown: a budget set through the service
        // kept, then the one the window *Settings* holds taken.
        let mut state = budget(&self.budget);
        if let Some(bytes) = state.unsaved.take() {
            ctx.set_setting(BUDGET, json!(bytes / MB));
        }
        let bytes = budget_setting(ctx.gpu_memory()).value(ctx.setting(BUDGET).as_ref()) as u64 * MB;
        if bytes != state.bytes {
            state.bytes = bytes;
            state.allow();
        }
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
        // Over black: egui lets what lies under an image show by what its alpha leaves of 1, which
        // the layers leave below 1 (a texture's alpha, a batch modulating it) without meaning it.
        ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
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
            // The report of the allocator lists every allocation: made again once a second.
            if self
                .allocator
                .as_ref()
                .is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(1))
            {
                let start = Instant::now();
                let report = ctx.gpu().and_then(|gpu| gpu.device.generate_allocator_report());
                journal::spent("view: the report of the allocator", start.elapsed());
                self.allocator = Some((Instant::now(), report.as_ref().map(stats::allocator)));
            }
            let allocator = self.allocator.as_ref().and_then(|(_, report)| *report);
            let text = self
                .stats
                .text(self.timer.is_some(), stats::process_memory(), allocator, &allowance);
            let galley = painter.layout_no_wrap(text, egui::FontId::monospace(11.0), colour);
            let at = rect.left_top() + egui::vec2(8.0, 8.0);
            let back = egui::Rect::from_min_size(at, galley.size()).expand(4.0);
            painter.rect_filled(back, 3.0, egui::Color32::from_black_alpha(170));
            painter.galley(at, galley, colour);
        }
        ui.ctx().request_repaint();
    }

    fn on_event(&mut self, event: &Event, _ctx: &mut Context) {
        if event.topic == MODULE_FAILED_TOPIC
            && let Some(id) = event.payload.get("id").and_then(|v| v.as_str())
        {
            self.failed(id);
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
    /// What the module `id`, failed, gave the view taken back: its layers, what it told the budget,
    /// its light.
    fn failed(&self, id: &str) {
        remove(&self.layers, id);
        let mut state = budget(&self.budget);
        if state.demands.remove(id).is_some() {
            state.allow();
        }
        self.lighting.lock().unwrap_or_else(|e| e.into_inner()).give(id, None);
    }

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
        let (_, resolved, shown) = resolve_target(
            device,
            extent,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        // Read by the pyramid between the two passes.
        let depth = texture(
            "viewport depth",
            TARGET.depth_format,
            TARGET.sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let builder = self
            .pyramids
            .get_or_insert_with(|| Arc::new(Builder::new(device, TARGET.sample_count)))
            .clone();
        let pyramid = pyramid::Pyramid::new(device, builder, &depth, size);

        let mut renderer = gpu.renderer.write();
        let texture_id = match &self.targets {
            Some(old) => {
                renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    &shown,
                    wgpu::FilterMode::Linear,
                    old.texture_id,
                );
                old.texture_id
            }
            None => renderer.register_native_texture(device, &shown, wgpu::FilterMode::Linear),
        };
        self.targets = Some(Targets {
            size,
            msaa,
            resolved,
            depth,
            pyramid,
            texture_id,
        });
    }

    fn render(&mut self, gpu: &egui_wgpu::RenderState, size: [u32; 2], ctx: &mut Context) {
        let view = view(
            &self.camera,
            size,
            self.start.elapsed().as_secs_f32(),
            self.lighting.lock().unwrap_or_else(|e| e.into_inner()).resolved(),
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
            pyramid: &targets.pyramid,
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
        match self.pass.note(drawn.pass_error) {
            Watched::Drawn => {}
            Watched::First(error) => log::error!("the pass of the view failed, its frames are not drawn: {error}"),
            Watched::Again => {}
            Watched::Given(error) => ctx.report_failure(
                "viewport",
                &format!("its pass failed {MISSED_FRAMES} frames in a row: {error}"),
            ),
        }
        self.stats.set_pass(self.pass.status());
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
        for layer in &layers {
            journal::spent(&format!("view: {} prepare", layer.owner), layer.prepare);
            if let Some(record) = layer.record {
                journal::spent(&format!("view: {} record", layer.owner), record);
            }
        }
        journal::spent("view: submit", submit);
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
                let layers: Vec<String> = frame
                    .layers
                    .iter()
                    .map(|(owner, computing, drawing)| format!("{owner} {:.2}", computing + drawing))
                    .collect();
                journal::set_gpu(format!("{:.2} ms: {}", frame.total, layers.join(", ")));
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
    /// Why the pass failed, the frame not drawn; the layers drawn in it are among the failures.
    pass_error: Option<String>,
}

/// The frames whose pass failed in a row after which the view gives up: the viewport reports itself
/// failed, no layer drawn in the pass being left to put the error on.
const MISSED_FRAMES: u32 = 30;

/// What a frame drawn tells the watch of the pass.
#[derive(Debug, PartialEq)]
enum Watched {
    Drawn,
    /// The first frame missed in a row, and why: said in the log once.
    First(String),
    Again,
    /// `MISSED_FRAMES` missed in a row: given up.
    Given(String),
}

/// The frames missed in a row because their pass failed, and the last error.
#[derive(Default)]
struct PassWatch {
    missed: u32,
    error: Option<String>,
}

impl PassWatch {
    fn note(&mut self, error: Option<String>) -> Watched {
        let Some(error) = error else {
            self.missed = 0;
            self.error = None;
            return Watched::Drawn;
        };
        self.missed += 1;
        self.error = Some(error.clone());
        match self.missed {
            1 => Watched::First(error),
            MISSED_FRAMES => Watched::Given(error),
            _ => Watched::Again,
        }
    }

    /// The frames missed in a row and why, for the statistics; none while the frames are drawn.
    fn status(&self) -> Option<(u32, String)> {
        self.error.clone().map(|error| (self.missed, error))
    }
}

/// The textures the pass of a frame draws into.
struct PassTargets<'a> {
    colour: &'a wgpu::TextureView,
    resolve: Option<&'a wgpu::TextureView>,
    depth: &'a wgpu::TextureView,
    /// Of `depth`, built between the two passes.
    pyramid: &'a pyramid::Pyramid,
}

/// What a layer gives its frame: its bundle, or none for its drawing in the pass; what it computes;
/// its number among the layers timed on the GPU; and what it cost.
struct Prepared {
    /// Its bundles of the phases, for a layer drawing in bundles.
    bundles: Option<Bundles>,
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
/// bundles recorded, or kept for its version unless the device is `new_device`; then the pass into
/// `targets`: `grid`, then in each phase, the opaque then the blended, the layers by their stage,
/// in the order they were added within one, their bundles of the phase run or their drawing of the
/// phase recorded in it; everything submitted, the computing first. The layers that panicked or
/// failed are removed.
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
    // The ground and its sky before the scene, whatever the order the modules started in.
    entries.sort_by_key(|entry| entry.layer.stage());
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
    let mut panicked: Vec<Option<String>> = vec![None; entries.len()];
    let mut drawing_in_pass = Duration::ZERO;
    // The first pass: the opaque phase, its colour and depth kept for the second; then the pyramid
    // of the depth it leaves.
    let mut first = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("viewport first pass"),
    });
    {
        let mut pass = first.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("viewport first pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: targets.colour,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(BACKGROUND),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: targets.depth,
                depth_ops: Some(wgpu::Operations {
                    // Reverse Z: infinity is 0.
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: timer.as_deref().and_then(|timer| timer.writes(true)),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(grid) = grid {
            grid.draw(&mut pass);
        }
        drawing_in_pass += draw_phases(
            &mut pass,
            true,
            &mut entries,
            &mut prepared,
            &mut panicked,
            gpu,
            view,
            timer.as_deref(),
        );
    }
    if let Some(timer) = timer.as_deref() {
        timer.pyramid(&mut first, false);
    }
    targets.pyramid.build(&mut first);
    if let Some(timer) = timer.as_deref() {
        timer.pyramid(&mut first, true);
    }
    // What the layers compute against it, each in an encoder of its own.
    let pyramid = targets.pyramid.given();
    let mut occluded = Vec::new();
    for ((entry, layer), panic) in entries.iter_mut().zip(&prepared).zip(&mut panicked) {
        if panic.is_some() {
            continue;
        }
        match occlude(entry, gpu, view, &pyramid, timer.as_deref().zip(layer.timed)) {
            Ok(Some(buffer)) => occluded.push(buffer),
            Ok(None) => {}
            Err(message) => *panic = Some(message),
        }
    }
    // The second pass: what they found in sight, then the blended phases; resolved.
    let mut second = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("viewport second pass"),
    });
    {
        let mut pass = second.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("viewport second pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: targets.colour,
                depth_slice: None,
                resolve_target: targets.resolve,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
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
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: timer.as_deref().and_then(|timer| timer.writes(false)),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        drawing_in_pass += draw_phases(
            &mut pass,
            false,
            &mut entries,
            &mut prepared,
            &mut panicked,
            gpu,
            view,
            timer.as_deref(),
        );
    }
    if let Some(timer) = timer.as_deref() {
        timer.resolve(&mut second);
    }
    // A GPU error in a pass is learnt here only: it is put on the layers drawn in the passes.
    let (first, failed_first) = finish(gpu, first);
    let (second, failed_second) = finish(gpu, second);
    let failed_pass = failed_first.or(failed_second);
    let computed: Vec<wgpu::CommandBuffer> = prepared.iter_mut().filter_map(|layer| layer.computed.take()).collect();
    match (first, second) {
        (Some(first), Some(second)) if failed_pass.is_none() => {
            gpu.queue
                .submit(computed.into_iter().chain([first]).chain(occluded).chain([second]));
            if let Some(timer) = timer {
                timer.submitted();
            }
        }
        _ => {
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
        let failure = panic.or_else(|| failed_pass.clone().filter(|_| layer.bundles.is_none()));
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
        pass_error: failed_pass,
    }
}

/// Draws the phases of the first pass, or of the second, of every layer in its order, each phase
/// of a layer between its timestamps when the frame times it; a layer that panicked is not drawn
/// again. Returns the time the layers drawing in the pass took.
#[allow(clippy::too_many_arguments)]
fn draw_phases(
    pass: &mut wgpu::RenderPass<'_>,
    first: bool,
    entries: &mut [Entry],
    prepared: &mut [Prepared],
    panicked: &mut [Option<String>],
    gpu: &egui_wgpu::RenderState,
    view: &View,
    timer: Option<&GpuTimer>,
) -> Duration {
    let mut drawing_in_pass = Duration::ZERO;
    for (index, phase) in Phase::ALL.into_iter().enumerate() {
        if phase.first_pass() != first {
            continue;
        }
        for ((entry, layer), panic) in entries.iter_mut().zip(prepared.iter_mut()).zip(panicked.iter_mut()) {
            // A layer that panicked in a phase is not drawn in the next.
            if panic.is_some() {
                continue;
            }
            if let (Some(timer), Some(timed)) = (timer, layer.timed) {
                timer.drawing(pass, timed, phase, false);
            }
            match &layer.bundles {
                Some(bundles) => pass.execute_bundles([&bundles[index]]),
                None => {
                    let started = Instant::now();
                    let drawn = catch_unwind(AssertUnwindSafe(|| {
                        entry.layer.draw_pass(gpu, &TARGET, view, phase, pass);
                    }));
                    let took = started.elapsed();
                    drawing_in_pass += took;
                    layer.record = Some(layer.record.unwrap_or_default() + took);
                    if let Err(payload) = drawn {
                        *panic = Some(format!(
                            "its viewport layer panicked drawing in the pass: {}",
                            panic_text(payload)
                        ));
                    }
                }
            }
            if let (Some(timer), Some(timed)) = (timer, layer.timed) {
                timer.drawing(pass, timed, phase, true);
            }
        }
    }
    drawing_in_pass
}

/// The encoder of a pass finished inside a validation error scope; or why the pass failed.
fn finish(
    gpu: &egui_wgpu::RenderState,
    encoder: wgpu::CommandEncoder,
) -> (Option<wgpu::CommandBuffer>, Option<String>) {
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let finished = catch_unwind(AssertUnwindSafe(move || encoder.finish()));
    let error = resolved(scope.pop()).flatten();
    match (finished, error) {
        (Err(payload), _) => (
            None,
            Some(format!(
                "the pass it drew in panicked once finished: {}",
                panic_text(payload)
            )),
        ),
        (Ok(_), Some(error)) => (None, Some(format!("a GPU error in the pass it drew in: {error}"))),
        (Ok(buffer), None) => (Some(buffer), None),
    }
}

/// What a layer computes against the pyramid of the first pass, recorded into an encoder of its
/// own between its timestamps when the frame times it apart, its timer and number among the layers
/// timed in `timed`, and finished inside a validation error scope; none for a layer that does not
/// occlude at this frame.
fn occlude(
    entry: &mut Entry,
    gpu: &egui_wgpu::RenderState,
    view: &View,
    pyramid: &Pyramid<'_>,
    timed: Option<(&GpuTimer, u32)>,
) -> Result<Option<wgpu::CommandBuffer>, String> {
    let layer = entry.layer.as_ref();
    let occludes = catch_unwind(AssertUnwindSafe(|| layer.occludes())).map_err(|payload| {
        format!(
            "its viewport layer panicked telling whether it occludes: {}",
            panic_text(payload)
        )
    })?;
    if !occludes {
        return Ok(None);
    }
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some(&entry.owner),
    });
    if let Some((timer, number)) = timed {
        timer.occluding(&mut encoder, number, false);
    }
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let layer = entry.layer.as_mut();
    let recorded = catch_unwind(AssertUnwindSafe(|| layer.occlude(gpu, view, pyramid, &mut encoder)));
    if let Some((timer, number)) = timed {
        timer.occluding(&mut encoder, number, true);
    }
    let finished = catch_unwind(AssertUnwindSafe(move || encoder.finish()));
    let error = resolved(scope.pop()).flatten();
    match (recorded, finished, error) {
        (Err(payload), _, _) => Err(format!(
            "its viewport layer panicked while testing against the depth: {}",
            panic_text(payload)
        )),
        (Ok(()), Err(payload), _) => Err(format!(
            "its viewport layer recorded invalid GPU commands while testing against the depth: {}",
            panic_text(payload)
        )),
        (Ok(()), Ok(_), Some(error)) => Err(format!(
            "its viewport layer caused a GPU error while testing against the depth: {error}"
        )),
        (Ok(()), Ok(buffer), None) => Ok(Some(buffer)),
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
            bundles: None,
            computed,
            timed,
            prepare,
            record: None,
        });
    }
    let (bundles, record) = match (version, &entry.kept) {
        (Some(version), Some((kept, bundles))) if *kept == version => (bundles.clone(), None),
        _ => {
            let recording = Instant::now();
            let mut recorded = Vec::with_capacity(Phase::ALL.len());
            for phase in Phase::ALL {
                recorded.push(record(&entry.owner, entry.layer.as_mut(), gpu, view, phase)?);
            }
            let bundles: Bundles = recorded.try_into().expect("a bundle a phase");
            entry.kept = version.map(|version| (version, bundles.clone()));
            (bundles, Some(recording.elapsed()))
        }
    };
    Ok(Prepared {
        bundles: Some(bundles),
        computed,
        timed,
        prepare,
        record,
    })
}

/// The computing of a layer, recorded into an encoder of its own between its timestamps when the
/// frame times it apart, and finished inside a validation error scope; none for a layer that does
/// not compute at this frame, its timestamps of computing left unwritten, read as nothing. With its
/// number among the layers timed.
fn compute(
    entry: &mut Entry,
    gpu: &egui_wgpu::RenderState,
    view: &View,
    timer: Option<&mut GpuTimer>,
) -> Result<(Option<wgpu::CommandBuffer>, Option<u32>), String> {
    let timed = timer.and_then(|timer| timer.layer(&entry.owner).map(|number| (timer, number)));
    let layer = entry.layer.as_ref();
    let computes = catch_unwind(AssertUnwindSafe(|| layer.computes())).map_err(|payload| {
        format!(
            "its viewport layer panicked telling whether it computes: {}",
            panic_text(payload)
        )
    })?;
    if !computes {
        return Ok((None, timed.map(|(_, number)| number)));
    }
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some(&entry.owner),
    });
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
        (Ok(()), Ok(buffer), None) => Ok((Some(buffer), number)),
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
    phase: Phase,
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
        layer.draw(gpu, &TARGET, view, phase, recording)
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
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use uniwow_api::serde_json::json;
    use uniwow_api::viewport::{self, Allowance, Drawing, Frame, Label, Layer, LayerStats, Phase, Stage, Target, View};
    use uniwow_api::{egui, egui_wgpu, wgpu};

    use uniwow_api::egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
    use uniwow_api::glam::Vec3;
    use uniwow_api::hotkey::Keys;

    use super::pyramid::{Builder, Pyramid, level_size, levels};
    use super::stats::{GpuFrame, GpuTimer, LayerTiming, Sample, Stats};
    use super::{
        Camera, CameraKeys, Drawn, Entry, FrameSignal, Layers, MISSED_FRAMES, PassTargets, PassWatch, TARGET,
        ViewportModule, Watched, camera, draw_frame, frame, lock, look_at, project, resolved, view,
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
            phase: Phase,
            _bundle: &mut wgpu::RenderBundleEncoder<'a>,
        ) {
            // Once a recording, of its bundles.
            if phase == Phase::Opaque {
                self.counts.drawn.fetch_add(1, Ordering::Relaxed);
            }
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
        let view = view(
            &Camera::default(),
            [64, 64],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
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
        let module = super::ViewportModule::default();
        let lighting = module.lighting.clone();
        let service = super::Service {
            layers: Layers::default(),
            frames: Arc::default(),
            budget: Default::default(),
            lighting: lighting.clone(),
        };
        use uniwow_api::viewport::Viewport as _;
        let set = viewport::Fog {
            colour: [1.0, 0.0, 0.0],
            start: 10.0,
            middle: 20.0,
            end: 30.0,
            rate: 0.0,
        };
        service.set_fog(set);
        let view = super::view(&Camera::default(), [64, 64], 0.0, lighting.lock().unwrap().resolved());
        assert_eq!(view.fog, set);
        assert_eq!(
            view.sun,
            viewport::Sun::default(),
            "the fixed light without the light of a map"
        );
        // The light of a map: its sun and the colour of its fog over the terrain's distances.
        let sun = viewport::Sun {
            direction: [0.0, 0.0, 1.0],
            colour: [0.5, 0.25, 0.0],
            ambient: [0.2; 3],
        };
        let light = viewport::MapLight {
            sun,
            fog_colour: [0.0, 0.0, 1.0],
            fog: None,
        };
        service.set_light("lighting", Some(light));
        let view = super::view(&Camera::default(), [64, 64], 0.0, lighting.lock().unwrap().resolved());
        assert_eq!(view.sun, sun);
        assert_eq!(
            view.fog,
            viewport::Fog {
                colour: [0.0, 0.0, 1.0],
                ..set
            }
        );
        // The fog of the game: its distances and its curve, steeper as it is short.
        service.set_light(
            "lighting",
            Some(viewport::MapLight {
                fog: Some([125.0, 500.0]),
                ..light
            }),
        );
        let fog = super::view(&Camera::default(), [64, 64], 0.0, lighting.lock().unwrap().resolved()).fog;
        assert_eq!([fog.start, fog.end], [125.0, 500.0]);
        assert!((fog.rate - 5.697).abs() < 1e-3, "{}", fog.rate);
        assert_eq!(viewport::fog_rate(0.0, 2_000.0), 1.5, "past its span, the gentlest");
        // Taken back only by the module that gave it: the fixed light again.
        service.set_light("terrain", None);
        assert_eq!(
            lighting.lock().unwrap().resolved().1,
            sun,
            "not the terrain's to take back"
        );
        service.set_light("lighting", None);
        let view = super::view(&Camera::default(), [64, 64], 0.0, lighting.lock().unwrap().resolved());
        assert_eq!((view.fog, view.sun), (set, viewport::Sun::default()));
        // Taken back too when that module fails, not when another does.
        service.set_light("lighting", Some(light));
        module.failed("terrain");
        assert_eq!(lighting.lock().unwrap().resolved().1, sun);
        module.failed("lighting");
        assert_eq!(lighting.lock().unwrap().resolved(), (set, viewport::Sun::default()));
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
            lighting: Arc::default(),
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

    /// A host keeping the settings of the module, on a GPU of `gpu_memory` bytes, the device `gpu`.
    #[derive(Default)]
    struct SettingsHost {
        settings: std::collections::HashMap<String, uniwow_api::serde_json::Value>,
        gpu_memory: Option<u64>,
        gpu: Option<egui_wgpu::RenderState>,
    }

    impl uniwow_api::Host for SettingsHost {
        fn publish(&mut self, _source: &str, _topic: &str, _payload: uniwow_api::serde_json::Value) {}

        fn execute(&mut self, _owner: &str, _command: Box<dyn uniwow_api::Command>) {}

        fn forget_document(&mut self, _owner: &str, _document: &str) {}

        fn service(&self, _id: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
            None
        }

        fn service_provider(&self, _id: &str) -> Option<String> {
            None
        }

        fn gpu(&self) -> Option<&egui_wgpu::RenderState> {
            self.gpu.as_ref()
        }

        fn gpu_memory(&self) -> Option<u64> {
            self.gpu_memory
        }

        fn draw_panel(&mut self, _owner: &str, _objects: &uniwow_api::ui::SharedUi, _panel: &str, _ui: &mut egui::Ui) {}

        fn draw_dialogs(&mut self, _owner: &str, _objects: &uniwow_api::ui::SharedUi, _egui: &egui::Context) {}

        fn adopt_objects(&mut self, _owner: &str, _objects: &uniwow_api::ui::SharedUi) {}

        fn setting(&self, _module: &str, key: &str) -> Option<uniwow_api::serde_json::Value> {
            self.settings.get(key).cloned()
        }

        fn set_setting(&mut self, _module: &str, key: &str, value: uniwow_api::serde_json::Value) {
            self.settings.insert(key.to_owned(), value);
        }

        fn report_failure(&mut self, _reporter: &str, _culprit: &str, _message: &str) {}

        fn spawn(&mut self, _owner: &str, _label: &str, _job: uniwow_api::JobFn) -> uniwow_api::JobId {
            unimplemented!("the view starts no job here")
        }

        fn spawn_thread(&mut self, _owner: &str, _label: &str, _job: uniwow_api::JobFn) -> uniwow_api::JobId {
            unimplemented!("the view starts no job here")
        }

        fn cancel(&mut self, _owner: &str, _job: uniwow_api::JobId) {}

        fn call(
            &mut self,
            _caller: &str,
            _name: &str,
            _arguments: uniwow_api::serde_json::Value,
        ) -> uniwow_api::CallId {
            uniwow_api::CallId(1)
        }

        fn editor(&self, _caller: &str) -> uniwow_api::Editor {
            unimplemented!("the tests give the view no editor")
        }
    }

    #[test]
    fn the_budget_is_a_setting_of_the_category_view_taken_at_each_frame() {
        use uniwow_api::{Context, Module, Registrar};
        let mut module = ViewportModule::default();
        let mut reg = Registrar {
            gpu_memory: Some(12 << 30),
            ..Registrar::default()
        };
        module.register(&mut reg);
        let category = reg.settings.expect("declared");
        assert_eq!(category.title, "View");
        let setting = &category.settings[0];
        assert_eq!(
            (setting.key.as_str(), setting.range, setting.default),
            ("gpu_budget_mb", [64, 65_536], 6144),
            "half the memory of a GPU of 12 GB by default"
        );
        let mut host = SettingsHost {
            gpu_memory: Some(12 << 30),
            ..SettingsHost::default()
        };
        module.init(&mut Context::new(&mut host, "viewport"));
        assert_eq!(super::budget(&module.budget).bytes, 6144 << 20);
        let egui = egui::Context::default();
        host.settings.insert("gpu_budget_mb".to_owned(), json!(2048));
        module.windows_ui(&egui, &mut Context::new(&mut host, "viewport"));
        assert_eq!(super::budget(&module.budget).bytes, 2048 << 20, "chosen in the window");
        // Set through the service, as the terrain gives its budget of before once: kept.
        let service = super::Service {
            layers: Layers::default(),
            frames: Arc::default(),
            budget: module.budget.clone(),
            lighting: Arc::default(),
        };
        use uniwow_api::viewport::Viewport as _;
        service.set_budget(512 << 20);
        module.windows_ui(&egui, &mut Context::new(&mut host, "viewport"));
        assert_eq!(host.settings["gpu_budget_mb"], json!(512));
        assert_eq!(super::budget(&module.budget).bytes, 512 << 20);
    }

    #[test]
    fn the_image_of_the_view_is_shown_over_black_whatever_alpha_the_layers_leave_in_it() {
        use uniwow_api::{Context, Module};
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let mut module = ViewportModule::default();
        let mut host = SettingsHost {
            gpu: Some(gpu),
            ..SettingsHost::default()
        };
        module.init(&mut Context::new(&mut host, "viewport"));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(64.0, 64.0))),
            ..egui::RawInput::default()
        };
        let mut output = egui::Context::default().run_ui(input, |ui| {
            module.panel_ui("view", ui, &mut Context::new(&mut host, "viewport"));
        });
        output.textures_delta.clear();
        let texture = module.targets.as_ref().expect("drawn").texture_id;
        let shapes: Vec<&egui::Shape> = output.shapes.iter().map(|clipped| &clipped.shape).collect();
        let image = shapes
            .iter()
            .position(|shape| matches!(shape, egui::Shape::Mesh(mesh) if mesh.texture_id == texture))
            .expect("the image of the view");
        let egui::Shape::Mesh(mesh) = shapes[image] else {
            unreachable!("found as a mesh")
        };
        assert!(
            image > 0
                && matches!(shapes[image - 1], egui::Shape::Rect(under)
                    if under.fill == egui::Color32::BLACK && under.rect == mesh.calc_bounds()),
            "{:?}",
            shapes.get(image.wrapping_sub(1))
        );
    }

    #[test]
    fn a_label_is_written_where_its_point_falls_in_the_view_and_not_behind_the_eye() {
        let shared = Camera::default();
        let view = view(
            &shared,
            [200, 100],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
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
        let drawn = view(
            &shared,
            [640, 480],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
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
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
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
// Its colour, then its depth.
struct Paint {
    colour: vec4<f32>,
    depth: vec4<f32>,
};
@group(0) @binding(0) var<storage, read> paint: Paint;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, paint.depth.x, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return paint.colour;
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
        PanicOccluding,
        ErrorOccluding,
    }

    /// A layer painting the whole view with one colour, in a bundle or in the pass; or with the colour
    /// its computing writes, in the same frame. At the depth 0.5, tested always and not written, its
    /// colour replacing what is under it, in the scene, in the opaque phase; or as `at`, `deep`,
    /// `blended` and `in_phase` say.
    struct Painter {
        drawing: Drawing,
        colour: [f32; 4],
        stage: Stage,
        phase: Phase,
        /// How many times it was drawn in the pass, whatever the phase.
        calls: Arc<AtomicUsize>,
        depth: (f32, bool, wgpu::CompareFunction),
        blended: bool,
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
                stage: Stage::Scene,
                phase: Phase::Opaque,
                calls: Arc::default(),
                depth: (0.5, false, wgpu::CompareFunction::Always),
                blended: false,
                computes: false,
                fault: Fault::None,
                made: None,
            }
        }

        fn at(mut self, stage: Stage) -> Self {
            self.stage = stage;
            self
        }

        fn in_phase(mut self, phase: Phase) -> Self {
            self.phase = phase;
            self
        }

        /// At the depth `depth`, written when `write`, tested by `compare`.
        fn deep(mut self, depth: f32, write: bool, compare: wgpu::CompareFunction) -> Self {
            self.depth = (depth, write, compare);
            self
        }

        /// Its colour mixed over what is under it by its alpha.
        fn blended(mut self) -> Self {
            self.blended = true;
            self
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
            let (colour, (depth, write, compare), blended) = (self.colour, self.depth, self.blended);
            self.made.get_or_insert_with(|| {
                let device = &gpu.device;
                let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: None,
                    source: wgpu::ShaderSource::Wgsl(PAINT.into()),
                });
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 32,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let paint = [colour, [depth, 0.0, 0.0, 0.0]];
                gpu.queue
                    .write_buffer(&buffer, 0, uniwow_api::bytemuck::cast_slice(&paint));
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
                        depth_write_enabled: Some(write),
                        depth_compare: Some(compare),
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
                        targets: &[Some(wgpu::ColorTargetState {
                            format: TARGET.color_format,
                            blend: blended.then_some(wgpu::BlendState::ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
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

        fn computes(&self) -> bool {
            self.computes || matches!(self.fault, Fault::PanicComputing | Fault::ErrorComputing)
        }

        fn occlude(
            &mut self,
            gpu: &egui_wgpu::RenderState,
            _view: &View,
            _pyramid: &viewport::Pyramid<'_>,
            encoder: &mut wgpu::CommandEncoder,
        ) {
            assert!(
                self.fault != Fault::PanicOccluding,
                "its test against the depth could not be made"
            );
            let (_, _, _, _, buffer) = self.made(gpu);
            // Not a multiple of 4: refused.
            encoder.clear_buffer(buffer, 1, None);
        }

        fn occludes(&self) -> bool {
            matches!(self.fault, Fault::PanicOccluding | Fault::ErrorOccluding)
        }

        fn drawing(&self) -> Drawing {
            self.drawing
        }

        fn stage(&self) -> Stage {
            self.stage
        }

        fn draw<'a>(
            &'a mut self,
            gpu: &egui_wgpu::RenderState,
            _target: &Target,
            _view: &View,
            phase: Phase,
            bundle: &mut wgpu::RenderBundleEncoder<'a>,
        ) {
            if phase != self.phase {
                return;
            }
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
            phase: Phase,
            pass: &mut wgpu::RenderPass<'_>,
        ) {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if phase != self.phase {
                return;
            }
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
        pyramid: Pyramid,
        samples: u32,
    }

    impl Targets {
        fn new(gpu: &egui_wgpu::RenderState) -> Self {
            Self::with_samples(gpu, TARGET.sample_count)
        }

        /// Multisampled `samples` times, unlike the bundles of the layers when not the view's; resolved
        /// only when multisampled.
        fn with_samples(gpu: &egui_wgpu::RenderState, samples: u32) -> Self {
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
            let depth = texture(
                TARGET.depth_format,
                samples,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
            .create_view(&Default::default());
            let builder = Arc::new(Builder::new(&gpu.device, samples));
            Self {
                colour: texture(TARGET.color_format, samples, wgpu::TextureUsages::RENDER_ATTACHMENT)
                    .create_view(&Default::default()),
                resolve: resolved.create_view(&Default::default()),
                resolved,
                pyramid: Pyramid::new(&gpu.device, builder, &depth, [8, 8]),
                depth,
                samples,
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
                resolve: (self.samples > 1).then_some(&self.resolve),
                depth: &self.depth,
                pyramid: &self.pyramid,
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
    fn egui_is_shown_the_bytes_the_frame_stores_in_srgb() {
        let Some(gpu) = gpu() else {
            return;
        };
        let extent = wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        };
        let (texture, resolve, shown) = super::resolve_target(
            &gpu.device,
            extent,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        // A grey of 0.5 written through a view, then the byte stored read back.
        let stored = |view: &wgpu::TextureView| {
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.5,
                            g: 0.5,
                            b: 0.5,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(1),
                    },
                },
                extent,
            );
            gpu.queue.submit([encoder.finish()]);
            buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            buffer.slice(..).get_mapped_range().expect("mapped")[0]
        };
        // Through the view of the pass, encoded to sRGB; through the view egui shows, as it is:
        // what egui reads there is what is stored.
        assert_eq!(stored(&resolve), 188);
        assert_eq!(stored(&shown), 128);
    }

    #[test]
    fn layers_drawn_in_the_pass_and_in_bundles_are_drawn_in_their_order_each_with_its_own_state() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
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
    fn what_the_scene_blends_over_the_ground_and_its_sky_stays_whatever_the_order_they_came_in() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
        let targets = Targets::new(&gpu);
        // Half red, blended in front without writing the depth, as a blended batch of the models,
        // added before the terrain as their module starts first. Behind it, the ground, green,
        // writing its depth; or the sky, blue, where the depth is still cleared.
        let half_red = [1.0, 0.0, 0.0, 0.5];
        let greater = wgpu::CompareFunction::Greater;
        let ground = Painter::new(Drawing::Bundle, GREEN).deep(0.25, true, greater);
        let sky = Painter::new(Drawing::Bundle, [0.0, 0.0, 1.0, 1.0])
            .deep(0.0, false, wgpu::CompareFunction::Equal)
            .in_phase(Phase::Beyond);
        for (under, channel) in [(ground, 1), (sky, 2)] {
            let layers = Layers::default();
            put(
                &layers,
                "models",
                Painter::new(Drawing::Pass, half_red)
                    .deep(0.5, false, greater)
                    .blended()
                    .in_phase(Phase::Near),
            );
            put(&layers, "terrain", under.at(Stage::Ground));
            let drawn = targets.draw(&layers, &gpu, &view, false, None);
            assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
            let seen = targets.middle(&gpu);
            assert!(seen[0] > 100 && seen[channel] > 100, "both seen: {seen:?}");
        }
    }

    #[test]
    fn what_is_blended_beyond_the_water_is_drawn_before_it_and_what_is_on_this_side_after() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
        let targets = Targets::new(&gpu);
        // Red beyond the surface, blue the water, green on this side, each half over what is
        // under it; added in the other order, the water in its stage: the phases order them.
        let layers = Layers::default();
        let half = |colour: [f32; 3], phase| {
            Painter::new(Drawing::Pass, [colour[0], colour[1], colour[2], 0.5])
                .blended()
                .in_phase(phase)
        };
        put(&layers, "near", half([0.0, 1.0, 0.0], Phase::Near));
        put(&layers, "water", half([0.0, 0.0, 1.0], Phase::Water).at(Stage::Water));
        put(
            &layers,
            "beyond",
            Painter::new(Drawing::Bundle, RED).blended().in_phase(Phase::Beyond),
        );
        let drawn = targets.draw(&layers, &gpu, &view, false, None);
        assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
        // Linear: red, then (0.5, 0, 0.5), then (0.25, 0.5, 0.25).
        let seen = targets.middle(&gpu);
        assert!(seen[1] > seen[0] && seen[0] == seen[2] && seen[0] > 100, "{seen:?}");
    }

    #[test]
    fn a_blended_batch_of_a_layer_is_drawn_over_the_opaque_ones_of_a_later_layer_and_hidden_behind() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
        let targets = Targets::new(&gpu);
        let greater = wgpu::CompareFunction::Greater;
        // Half red blended, of a layer added first; green opaque, writing its depth at 0.5, of a
        // layer added after it: in front of the green (nearer, 0.75), then behind it (0.25).
        for (depth, both) in [(0.75, true), (0.25, false)] {
            let layers = Layers::default();
            put(
                &layers,
                "first",
                Painter::new(Drawing::Bundle, [1.0, 0.0, 0.0, 0.5])
                    .deep(depth, false, greater)
                    .blended()
                    .in_phase(Phase::Near),
            );
            put(
                &layers,
                "second",
                Painter::new(Drawing::Pass, GREEN).deep(0.5, true, greater),
            );
            let drawn = targets.draw(&layers, &gpu, &view, false, None);
            assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
            let seen = targets.middle(&gpu);
            if both {
                assert!(seen[0] > 100 && seen[1] > 100, "seen over the green: {seen:?}");
            } else {
                assert_eq!(seen, [0, 255, 0, 255], "hidden behind it");
            }
        }
    }

    #[test]
    fn a_pass_failing_without_a_layer_drawn_in_it_is_said_then_given_up() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
        // A bundle recorded for the view's multisampling, run in a pass of one sample: refused when
        // the pass is finished, no layer drawn in it to blame.
        let targets = Targets::with_samples(&gpu, 1);
        let layers = Layers::default();
        put(&layers, "bundled", Painter::new(Drawing::Bundle, GREEN));
        let mut watch = PassWatch::default();
        for frame in 1..=MISSED_FRAMES {
            let drawn = targets.draw(&layers, &gpu, &view, false, None);
            assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
            let error = drawn.pass_error.expect("the pass refused");
            let watched = watch.note(Some(error.clone()));
            match frame {
                1 => assert_eq!(watched, Watched::First(error), "said once"),
                MISSED_FRAMES => assert_eq!(watched, Watched::Given(error), "given up"),
                _ => assert_eq!(watched, Watched::Again),
            }
            assert_eq!(watch.status().map(|(missed, _)| missed), Some(frame));
        }
        assert_eq!(lock(&layers).layers.len(), 1, "no layer to put it on");
        assert_eq!(watch.note(None), Watched::Drawn);
        assert_eq!(watch.status(), None, "a frame drawn begins again");
        assert_eq!(watch.note(Some("again".to_owned())), Watched::First("again".to_owned()));
    }

    #[test]
    fn what_a_layer_computes_is_drawn_in_the_same_frame() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
        let targets = Targets::new(&gpu);
        let layers = Layers::default();
        put(&layers, "chosen", Painter::new(Drawing::Pass, RED).computing());
        let drawn = targets.draw(&layers, &gpu, &view, false, None);
        assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
        assert_eq!(targets.middle(&gpu), [0, 0, 255, 255], "the blue its computing wrote");
    }

    #[test]
    fn a_layer_that_computes_nothing_gets_no_encoder() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
        let entry = |painter: Painter| Entry {
            owner: "painter".to_owned(),
            layer: Box::new(painter),
            kept: None,
        };
        let (idle, _) = super::compute(&mut entry(Painter::new(Drawing::Pass, RED)), &gpu, &view, None).unwrap();
        assert!(idle.is_none(), "no command buffer for nothing");
        let computing = Painter::new(Drawing::Pass, RED).computing();
        let (computed, _) = super::compute(&mut entry(computing), &gpu, &view, None).unwrap();
        assert!(computed.is_some());
    }

    #[test]
    fn a_layer_failing_to_compute_or_to_draw_in_the_pass_is_removed_and_reported() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
        let targets = Targets::new(&gpu);
        for (fault, said) in [
            (Fault::PanicComputing, "panicked while computing"),
            (Fault::ErrorComputing, "caused a GPU error while computing"),
            (Fault::PanicDrawing, "panicked drawing in the pass"),
            (Fault::ErrorDrawing, "a GPU error in the pass it drew in"),
            (Fault::PanicOccluding, "panicked while testing against the depth"),
            (
                Fault::ErrorOccluding,
                "caused a GPU error while testing against the depth",
            ),
        ] {
            let layers = Layers::default();
            put(&layers, "kept", Painter::new(Drawing::Bundle, GREEN));
            let faulty = Painter::new(Drawing::Pass, RED).faulty(fault);
            let calls = faulty.calls.clone();
            put(&layers, "faulty", faulty);
            let drawn = targets.draw(&layers, &gpu, &view, false, None);
            if fault == Fault::PanicDrawing {
                assert_eq!(calls.load(Ordering::Relaxed), 1, "not drawn again in the blended phase");
            }
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

    /// Writes each pixel's depth from a storage buffer, its samples a little farther each, the
    /// second the farthest, so that the least of them is neither the first nor the last.
    const DEPTHS: &str = r#"
@group(0) @binding(0) var<storage, read> depths: array<f32>;
@group(0) @binding(1) var<uniform> width: vec4<u32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) at: vec4<f32>, @builtin(sample_index) sample: u32) -> @builtin(frag_depth) f32 {
    let pixel = vec2<u32>(at.xy);
    let farther = array<f32, 4>(0.0, 0.03, 0.01, 0.02);
    return depths[pixel.y * width.x + pixel.x] - farther[sample];
}
"#;

    /// The levels of the pyramid of a depth of `size` and `samples` samples a pixel, the depth of
    /// each pixel `depth` at its place, its samples farther as `DEPTHS` says, read back.
    fn pyramid_of(gpu: &egui_wgpu::RenderState, size: [u32; 2], samples: u32, depth: &[f32]) -> Vec<Vec<f32>> {
        let device = &gpu.device;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET.depth_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let depths = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (depth.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue
            .write_buffer(&depths, 0, uniwow_api::bytemuck::cast_slice(depth));
        let width = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue
            .write_buffer(&width, 0, uniwow_api::bytemuck::cast_slice(&[size[0], 0, 0, 0]));
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(DEPTHS.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: TARGET.depth_format,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[],
            }),
            multiview_mask: None,
            cache: None,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: depths.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: width.as_entire_binding(),
                },
            ],
        });
        let pyramid = Pyramid::new(device, Arc::new(Builder::new(device, samples)), &view, size);
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        pyramid.build(&mut encoder);
        let read: Vec<(wgpu::Buffer, [u32; 2])> = (0..levels(size))
            .map(|level| {
                let [width, height] = level_size(size, level);
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 256 * u64::from(height),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: pyramid.texture(),
                        mip_level: level,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(256),
                            rows_per_image: Some(height),
                        },
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
                (buffer, [width, height])
            })
            .collect();
        gpu.queue.submit([encoder.finish()]);
        for (buffer, _) in &read {
            buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        }
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        read.iter()
            .map(|(buffer, [width, height])| {
                let data = buffer.slice(..).get_mapped_range().expect("mapped").to_vec();
                (0..*height as usize)
                    .flat_map(|y| {
                        let row = &data[y * 256..];
                        (0..*width as usize)
                            .map(|x| f32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().expect("four bytes")))
                            .collect::<Vec<f32>>()
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_pyramid_holds_the_farthest_depth_under_each_texel_at_every_level() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        // Odd sides, so that the last texel of a level covers three of the level under it.
        let size = [5u32, 3];
        let depth: Vec<f32> = (0..15u32).map(|at| 0.05 + ((at * 7) % 10) as f32 / 11.0).collect();
        for samples in [TARGET.sample_count, 1] {
            let read = pyramid_of(&gpu, size, samples, &depth);
            assert_eq!(read.len(), 3, "5 × 3, 2 × 1, 1 × 1");
            // The first level: the farthest sample of each pixel.
            let farthest = if samples > 1 { 0.03 } else { 0.0 };
            let mut expected: Vec<f32> = depth.iter().map(|at| at - farthest).collect();
            for (level, read) in read.iter().enumerate() {
                let [width, height] = level_size(size, level as u32);
                if level > 0 {
                    let [under_width, under_height] = level_size(size, level as u32 - 1);
                    let under = expected.clone();
                    // The texels under one: its 2 × 2, the last of a side to the end of the level.
                    let span =
                        |at: u32, side: u32, under: u32| 2 * at..=if at == side - 1 { under - 1 } else { 2 * at + 1 };
                    expected = (0..height)
                        .flat_map(|y| (0..width).map(move |x| (x, y)))
                        .map(|(x, y)| {
                            span(y, height, under_height)
                                .flat_map(|row| span(x, width, under_width).map(move |column| (column, row)))
                                .map(|(column, row)| under[(row * under_width + column) as usize])
                                .fold(1.0, f32::min)
                        })
                        .collect();
                }
                assert_eq!(read.len(), (width * height) as usize);
                for (got, wanted) in read.iter().zip(&expected) {
                    assert!(
                        (got - wanted).abs() < 1e-6,
                        "{samples} samples, level {level}: {read:?}, not {expected:?}"
                    );
                }
            }
        }
    }

    /// The size and the levels of a pyramid given.
    type Given = ([u32; 2], u32);

    /// A layer reading, between the two passes, the farthest depth the first pass left over the whole
    /// view, from the last level of the pyramid, into a buffer to read back; with the size and levels
    /// of the pyramid it was given.
    struct Sounding {
        given: Arc<Mutex<Option<Given>>>,
        read: Arc<Mutex<Option<wgpu::Buffer>>>,
        made: Option<(wgpu::ComputePipeline, wgpu::BindGroupLayout, wgpu::Buffer)>,
    }

    const SOUNDING: &str = r#"
@group(0) @binding(0) var pyramid: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> farthest: f32;

@compute @workgroup_size(1)
fn main() {
    farthest = textureLoad(pyramid, vec2<u32>(0u), textureNumLevels(pyramid) - 1u).r;
}
"#;

    impl Layer for Sounding {
        fn occlude(
            &mut self,
            gpu: &egui_wgpu::RenderState,
            _view: &View,
            pyramid: &viewport::Pyramid<'_>,
            encoder: &mut wgpu::CommandEncoder,
        ) {
            *self.given.lock().unwrap() = Some((pyramid.size, pyramid.levels));
            let device = &gpu.device;
            let (pipeline, layout, written) = self.made.get_or_insert_with(|| {
                let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: None,
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: false },
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });
                let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: None,
                    source: wgpu::ShaderSource::Wgsl(SOUNDING.into()),
                });
                let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: None,
                    layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: None,
                        bind_group_layouts: &[Some(&layout)],
                        immediate_size: 0,
                    })),
                    module: &shader,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                });
                let written = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 4,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                });
                (pipeline, layout, written)
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(pyramid.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: written.as_entire_binding(),
                    },
                ],
            });
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            let read = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_buffer_to_buffer(written, 0, &read, 0, 4);
            *self.read.lock().unwrap() = Some(read);
        }

        fn occludes(&self) -> bool {
            true
        }
    }

    #[test]
    fn the_layers_test_against_the_depth_their_first_pass_left_then_draw_what_they_reveal_over_it() {
        let Some(gpu) = gpu() else {
            eprintln!("skipped: no software adapter for a device");
            return;
        };
        let view = view(
            &Camera::default(),
            [8, 8],
            0.0,
            (viewport::Fog::default(), viewport::Sun::default()),
        );
        let targets = Targets::new(&gpu);
        let layers = Layers::default();
        put(
            &layers,
            "ground",
            Painter::new(Drawing::Bundle, RED).deep(0.5, true, wgpu::CompareFunction::Always),
        );
        let (given, read) = (Arc::default(), Arc::default());
        lock(&layers).layers.push(Entry {
            owner: "sounding".to_owned(),
            layer: Box::new(Sounding {
                given: Arc::clone(&given),
                read: Arc::clone(&read),
                made: None,
            }),
            kept: None,
        });
        // Revealed nearer than the ground, its depth written in the second pass only; then behind
        // the ground, hidden by the depth the first pass left.
        put(
            &layers,
            "revealed",
            Painter::new(Drawing::Pass, GREEN).in_phase(Phase::Revealed).deep(
                0.6,
                true,
                wgpu::CompareFunction::Greater,
            ),
        );
        put(
            &layers,
            "hidden",
            Painter::new(Drawing::Bundle, [0.0, 0.0, 1.0, 1.0])
                .in_phase(Phase::Revealed)
                .deep(0.4, false, wgpu::CompareFunction::Greater),
        );
        let drawn = targets.draw(&layers, &gpu, &view, false, None);
        assert!(drawn.failures.is_empty(), "{:?}", drawn.failures);
        assert_eq!(
            targets.middle(&gpu),
            [0, 255, 0, 255],
            "revealed over the ground, what lies behind it hidden"
        );
        assert_eq!(*given.lock().unwrap(), Some(([8, 8], 4)));
        let buffer = read.lock().unwrap().take().expect("read between the passes");
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = buffer.slice(..).get_mapped_range().expect("mapped").to_vec();
        let farthest = f32::from_le_bytes(data[..4].try_into().expect("four bytes"));
        assert_eq!(
            farthest, 0.5,
            "the depth of the first pass of the same frame, without what the second drew"
        );
    }

    #[test]
    fn a_layer_is_timed_computing_and_drawing_in_every_phase_and_the_pyramid_apart() {
        // The passes, the pyramid 4 ticks, then a layer: its computing 2 ticks, against the pyramid
        // 1, its drawing 3, 5, 7, 11 and 13 in the five phases.
        let ticks = [0, 100, 60, 64, 10, 12, 13, 14, 20, 23, 30, 35, 40, 47, 50, 61, 70, 83];
        let (total, pyramid, layers) = crate::stats::spans(&ticks, 1, 1e6);
        assert_eq!((total, pyramid, layers), (100.0, 4.0, vec![(3.0, 39.0)]));
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
                pyramid: 0.5,
                layers: vec![("terrain".to_owned(), 0.25, 1.5)],
            },
        );
        let text = stats.text(true, None, None, &Allowance::default());
        assert!(text.starts_with("48 fps: a frame 20.7 ms, the longest 40.0"), "{text}");
        assert!(text.contains("view, interface thread: 2.00 ms"), "{text}");
        assert!(text.contains("GPU: 3.00 ms"), "{text}");
        assert!(text.contains("the pyramid of the depth 0.50"), "{text}");
        assert!(
            text.contains("  GPU: computing 0.25 ms (0.25), drawing 1.50 (1.50)"),
            "{text}"
        );
        assert!(!text.contains("the pass failed"), "{text}");
        stats.set_pass(Some((3, "refused".to_owned())));
        let failing = stats.text(true, None, None, &Allowance::default());
        assert!(
            failing.contains("the pass failed, 3 frames in a row not drawn: refused"),
            "{failing}"
        );
        stats.set_pass(None);
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
        assert!(
            stats
                .text(false, None, None, &Allowance::default())
                .contains("not timed")
        );
        let report = wgpu::AllocatorReport {
            allocations: Vec::new(),
            blocks: Vec::new(),
            total_allocated_bytes: 100 << 20,
            total_reserved_bytes: 400 << 20,
        };
        assert_eq!(super::stats::allocator(&report), [100 << 20, 400 << 20, 0]);
        let memory = stats.text(
            true,
            Some((300 << 20, 200 << 20)),
            Some([100 << 20, 400 << 20, 7]),
            &Allowance::default(),
        );
        assert!(memory.contains("process: 300 MB in memory, 200 MB private"), "{memory}");
        assert!(
            memory.contains("allocator of the device: 100 MB allocated of 400 MB reserved, 7 blocks"),
            "{memory}"
        );
        assert!(!memory.contains("GPU budget"), "no budget told, none written");
        let budget = Allowance {
            budget: 1000 << 20,
            used: 300 << 20,
            limited: Some(viewport::BAND * 8.0),
            ..Allowance::default()
        };
        let limited = stats.text(true, None, None, &budget);
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
            stats
                .text(true, None, None, &Allowance::default())
                .starts_with("100 fps"),
            "{}",
            stats.text(true, None, None, &Allowance::default())
        );
    }
}
