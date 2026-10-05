//! The M2 models the modules place in the 3D view, through the service `models`: each instance a
//! look of a model at a transform, kept until its owner gives others. The looks wanted are loaded
//! by jobs of the pool, the nearest first, as many as the shared budget of the view holds, their
//! models and textures read once whoever asks; drawn by the layer of the module instanced, a draw
//! per group of instances and batch. Its panel sets how far an instance is drawn and previews a
//! display before the camera. Nothing is changed: no undo entry, no file written.

mod cache;
mod display;
mod gpu;
mod groups;
mod layer;
mod loading;
mod service;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use uniwow_api::formats::{self, Formats};
use uniwow_api::glam::{Mat4, Quat, Vec3};
use uniwow_api::models::{self, Instance, LookId, LookState, Models};
use uniwow_api::viewport::Demand;
use uniwow_api::{
    Context, DockArea, Event, JobId, JobOutcome, MODULE_FAILED_TOPIC, Module, PropertyValue, Registrar, egui, log,
    serde_json, viewport,
};

use gpu::Shared;
use layer::{ModelsLayer, Scene};
use loading::{Caches, LookGpu};
use service::Service;

pub fn lock<T>(shared: &Mutex<T>) -> MutexGuard<'_, T> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// The setting of how far an instance is drawn, in radii of it, and its least and most.
const REACH: &str = "reach";
const DEFAULT_REACH: f32 = 100.0;
const REACHES: [f32; 2] = [10.0, 1_000.0];
/// The owner of the instances previewed.
const PREVIEW: &str = "models";
/// What a look not yet loaded is expected to take on the GPU, before any is held.
const EXPECTED: u64 = 1 << 20;
const MB: f64 = 1024.0 * 1024.0;

/// What a load ends with: the look on the GPU or why not, and the textures drawn white.
type Loaded = Option<(Result<LookGpu, String>, Vec<String>)>;

/// The instances of a preview of the display `display`: `count` of them in a grid before the
/// camera at `eye` looking at `target`, the first twice its height away, facing it.
fn preview(
    service: &Service,
    formats: &dyn Formats,
    display: u32,
    count: u32,
    eye: Vec3,
    target: Vec3,
) -> Result<String, String> {
    let (look, scale) = display::display(formats, display)?;
    let model = formats.model(&look.model)?;
    // Its size at rest, from its vertices: its bounds hold its animations too.
    let (low, high) = model.vertices.iter().fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(low, high), vertex| {
            let at = Vec3::from(vertex.position);
            (low.min(at), high.max(at))
        },
    );
    if !low.is_finite() {
        return Err(format!("display {display}: its model has no vertex"));
    }
    let size = (high - low).length() * scale;
    let displays = formats.creature_displays()?;
    let alpha = displays
        .iter()
        .find(|row| row.id == display)
        .map_or(1.0, |row| row.alpha as f32 / 255.0);
    let id = service.look(&look);
    let looking = (target - eye).try_normalize().unwrap_or(Vec3::X);
    // Its middle at the height of the eye.
    let middle = (low.z + high.z) / 2.0 * scale;
    let height = (high.z - low.z) * scale;
    let at = eye + looking * (2.0 * height).max(3.0) - Vec3::Z * middle;
    let towards = (eye - at).truncate();
    let facing = towards.y.atan2(towards.x);
    let forward = Vec3::new(facing.cos(), facing.sin(), 0.0);
    let right = Vec3::new(-forward.y, forward.x, 0.0);
    let spacing = size.max(1.0);
    let side = (count as f32).sqrt().ceil() as u32;
    let instances: Vec<Instance> = (0..count)
        .map(|index| {
            let (row, column) = ((index / side) as f32, (index % side) as f32);
            let offset = right * (column - (side - 1) as f32 / 2.0) * spacing - forward * row * spacing;
            Instance {
                id: u64::from(index),
                look: id,
                transform: Mat4::from_scale_rotation_translation(
                    Vec3::splat(scale),
                    Quat::from_rotation_z(facing),
                    at + offset,
                ),
                alpha,
            }
        })
        .collect();
    service.place(PREVIEW, &instances);
    Ok(format!(
        "display {display}: {count} of {:?}, scale {scale:.2}, radius {:.2}, at {:.0} {:.0} {:.0}",
        look.model, model.radius, at.x, at.y, at.z
    ))
}

/// Clears the instances of a module that failed, as `event` says.
fn forget_failed(service: &Service, event: &Event) {
    if event.topic == MODULE_FAILED_TOPIC
        && let Some(id) = event.payload["id"].as_str()
    {
        service.clear(id);
    }
}

/// The preview of the panel.
struct Preview {
    display: u32,
    count: u32,
    job: Option<JobId>,
    said: String,
}

impl Default for Preview {
    fn default() -> Self {
        Self {
            display: 1985,
            count: 1,
            job: None,
            said: String::new(),
        }
    }
}

struct ModelsModule {
    service: Arc<Service>,
    scene: Arc<Mutex<Scene>>,
    /// Where the job building what the models share leaves it, for the layer.
    incoming: Arc<Mutex<Option<Arc<Shared>>>>,
    shared: Option<Arc<Shared>>,
    setup: Option<JobId>,
    view: Option<viewport::Handle>,
    caches: Arc<Caches>,
    /// The looks on the GPU, those loading by their job, and the jobs by look.
    held: HashMap<LookId, Arc<LookGpu>>,
    loading: HashMap<LookId, JobId>,
    jobs: HashMap<JobId, LookId>,
    generation: u64,
    /// What the models told the budget of the view last.
    told: Option<Demand>,
    frame: u64,
    reach: f32,
    preview: Preview,
    /// The last refusals, looks and textures.
    refusals: Vec<String>,
}

impl Default for ModelsModule {
    fn default() -> Self {
        Self {
            service: Arc::default(),
            scene: Arc::default(),
            incoming: Arc::default(),
            shared: None,
            setup: None,
            view: None,
            caches: Arc::default(),
            held: HashMap::new(),
            loading: HashMap::new(),
            jobs: HashMap::new(),
            generation: 0,
            told: None,
            frame: 0,
            reach: DEFAULT_REACH,
            preview: Preview::default(),
            refusals: Vec::new(),
        }
    }
}

/// The refusals the panel keeps.
const REFUSALS: usize = 8;

impl ModelsModule {
    fn refuse(&mut self, why: String) {
        log::warn!("models: {why}");
        self.refusals.push(why);
        if self.refusals.len() > REFUSALS {
            self.refusals.remove(0);
        }
    }

    /// Hands the looks held to the layer.
    fn publish(&mut self) {
        self.generation += 1;
        let mut scene = lock(&self.scene);
        scene.looks = Arc::new(self.held.clone());
        scene.generation = self.generation;
    }

    /// At each frame, while the view is drawn: the nearest group of each look placed, the loads it
    /// wants started the nearest first, those beyond what the budget keeps released, and the
    /// models' demand told to the budget.
    fn steer(&mut self, ctx: &mut Context) {
        let start = Instant::now();
        if lock(&self.service.formats).is_none()
            && let Some(formats) = ctx.service(formats::SERVICE)
        {
            *lock(&self.service.formats) = Some(formats);
        }
        let (Some(view), Some(shared)) = (self.view.clone(), self.shared.clone()) else {
            return;
        };
        let Some(frame) = view.wait_frame(self.frame, Duration::ZERO) else {
            return;
        };
        self.frame = frame.number;
        let eye = match ctx.read_property("viewport/camera_position") {
            Ok(PropertyValue::Vector([x, y, z])) => Vec3::new(x as f32, y as f32, z as f32),
            _ => return,
        };
        let mut nearest: HashMap<LookId, f32> = HashMap::new();
        for slot in self.service.owners() {
            for group in &slot.published().groups {
                let radius = self.held.get(&group.look).map_or(0.0, |look| look.radius()) * group.scale;
                let bounds = [group.low - Vec3::splat(radius), group.high + Vec3::splat(radius)];
                let distance = layer::nearest(eye, bounds);
                nearest
                    .entry(group.look)
                    .and_modify(|at| *at = at.min(distance))
                    .or_insert(distance);
            }
        }
        let allowance = view.allowance();
        // Released beyond what the budget keeps, or placed no more, the farthest first.
        let released: Vec<LookId> = self
            .held
            .keys()
            .filter(|id| nearest.get(id).is_none_or(|distance| *distance > allowance.keep))
            .copied()
            .collect();
        if !released.is_empty() {
            let gone: Vec<Arc<LookGpu>> = released.iter().filter_map(|id| self.held.remove(id)).collect();
            for id in &released {
                self.service.set_state(*id, LookState::Waiting);
            }
            self.publish();
            let caches = self.caches.clone();
            ctx.spawn("Free the models left", move |_| {
                drop(gone);
                caches.models.purge();
                caches.textures.purge();
            });
        }
        // Loads no longer wanted are cancelled.
        let stale: Vec<LookId> = self
            .loading
            .keys()
            .filter(|id| !nearest.contains_key(id))
            .copied()
            .collect();
        for id in stale {
            if let Some(job) = self.loading.remove(&id) {
                ctx.cancel(job);
                self.jobs.remove(&job);
                self.service.set_state(id, LookState::Waiting);
            }
        }
        let mut wanted: Vec<(f32, LookId)> = nearest
            .iter()
            .filter(|(id, distance)| {
                !self.held.contains_key(id)
                    && !self.loading.contains_key(id)
                    && **distance <= allowance.load
                    && !matches!(self.service.state(**id), LookState::Refused(_))
            })
            .map(|(id, distance)| (*distance, *id))
            .collect();
        wanted.sort_by(|a, b| a.0.total_cmp(&b.0));
        let most = std::thread::available_parallelism().map_or(1, |cores| cores.get().saturating_sub(1).max(1));
        if let Some(formats) = lock(&self.service.formats).clone() {
            for (_, id) in wanted.into_iter().take(most.saturating_sub(self.loading.len())) {
                let Some((look, _)) = self.service.look_of(id) else {
                    continue;
                };
                let (shared, caches, formats) = (shared.clone(), self.caches.clone(), formats.clone());
                let job = ctx.spawn(&format!("Load the model {:?}", look.model), move |job| -> Loaded {
                    if job.is_cancelled() {
                        return None;
                    }
                    let mut refused = Vec::new();
                    let result = loading::look(&shared, &*formats, &caches, &look, &mut refused);
                    Some((result, refused))
                });
                self.loading.insert(id, job);
                self.jobs.insert(job, id);
                self.service.set_state(id, LookState::Loading);
            }
        }
        self.tell_budget(&view, &nearest);
        let mut scene = lock(&self.scene);
        let (models, textures) = (self.caches.models.counts(), self.caches.textures.counts());
        let waiting = nearest
            .keys()
            .filter(|id| !self.held.contains_key(id) && !self.loading.contains_key(id))
            .count();
        let bytes = self.told.as_ref().map_or(0, Demand::used);
        scene.reach = self.reach;
        scene.bytes = bytes;
        scene.summary = format!(
            "{} looks on the GPU ({:.0} MB), {} loading, {waiting} waiting; {} models and {} textures held, {} textures unreadable",
            self.held.len(),
            bytes as f64 / MB,
            self.loading.len(),
            models.0,
            textures.0,
            textures.1
        );
        scene.steering = start.elapsed();
    }

    /// Tells the budget of the view what the models hold and want, each model and texture once, in
    /// the band of the nearest of the looks holding it.
    fn tell_budget(&mut self, view: &viewport::Handle, nearest: &HashMap<LookId, f32>) {
        let mut demand = Demand::default();
        let mut counted: HashSet<usize> = HashSet::new();
        let mut held: Vec<(f32, &Arc<LookGpu>)> = self
            .held
            .iter()
            .map(|(id, look)| (nearest.get(id).copied().unwrap_or(f32::INFINITY), look))
            .collect();
        held.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut total = 0;
        for (distance, look) in &held {
            let mut bytes = look.bytes;
            if counted.insert(Arc::as_ptr(&look.model) as usize) {
                bytes += look.model.bytes;
            }
            for texture in &look.textures {
                if counted.insert(Arc::as_ptr(texture) as usize) {
                    bytes += texture.bytes;
                }
            }
            demand.held[Demand::band(*distance)] += bytes;
            demand.wanted[Demand::band(*distance)] += bytes;
            total += bytes;
        }
        let expected = if held.is_empty() {
            EXPECTED
        } else {
            total / held.len() as u64
        };
        for (id, distance) in nearest {
            if !self.held.contains_key(id) {
                demand.wanted[Demand::band(*distance)] += expected;
            }
        }
        if self.told.as_ref() != Some(&demand) {
            view.tell_budget("models", demand.clone());
            self.told = Some(demand);
        }
    }

    /// Starts the preview of the panel, or clears it for a count of 0: from a job, which reads the
    /// tables and the model.
    fn start_preview(&mut self, display: u32, count: u32, ctx: &mut Context) -> Result<(), String> {
        if let Some(job) = self.preview.job.take() {
            ctx.cancel(job);
        }
        if count == 0 {
            self.service.clear(PREVIEW);
            self.preview.said = "cleared".to_owned();
            return Ok(());
        }
        let formats = ctx.service(formats::SERVICE).ok_or("the client's files are not open")?;
        let point = |name: &str| match ctx.read_property(name) {
            Ok(PropertyValue::Vector([x, y, z])) => Ok(Vec3::new(x as f32, y as f32, z as f32)),
            _ => Err(format!("the view has no {name}")),
        };
        let (eye, target) = (point("viewport/camera_position")?, point("viewport/camera_target")?);
        let service = self.service.clone();
        self.preview.job = Some(ctx.spawn(&format!("Preview the display {display}"), move |_| {
            preview(&service, &*formats, display, count, eye, target)
        }));
        self.preview.said = format!("reading the display {display}");
        Ok(())
    }
}

impl Module for ModelsModule {
    fn register(&mut self, reg: &mut Registrar) {
        let service: models::Handle = self.service.clone();
        reg.provide(models::SERVICE, service)
            .panel("models", "Models", DockArea::Right)
            .subscribe(MODULE_FAILED_TOPIC)
            .command(
                "models.preview",
                "Shows count instances of the creature display display before the camera, in a grid facing it, as the owner models; a count of 0 clears them.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "display": { "type": "integer", "minimum": 0 },
                        "count": { "type": "integer", "minimum": 0, "maximum": 10000 },
                    },
                    "required": ["display"],
                }),
                serde_json::json!({ "type": "object" }),
            );
    }

    fn init(&mut self, ctx: &mut Context) {
        if let Some(reach) = ctx.setting(REACH).and_then(|value| value.as_f64()) {
            self.reach = (reach as f32).clamp(REACHES[0], REACHES[1]);
        }
        let (Some(view), Some(gpu)) = (ctx.service(viewport::SERVICE), ctx.gpu().cloned()) else {
            return;
        };
        let _ = self.service.gpu.set((gpu.device.clone(), gpu.queue.clone()));
        view.add_layer(
            "models",
            Box::new(ModelsLayer::new(
                self.service.clone(),
                self.scene.clone(),
                self.incoming.clone(),
            )),
        );
        let target = view.target();
        self.view = Some(view);
        self.setup = Some(ctx.spawn("Build the pipelines of the models", move |_| {
            Arc::new(Shared::new(&gpu, &target))
        }));
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        ui.horizontal(|ui| {
            ui.label("Instances drawn up to");
            let changed = ui
                .add(
                    egui::DragValue::new(&mut self.reach)
                        .range(REACHES[0]..=REACHES[1])
                        .speed(1.0),
                )
                .changed();
            ui.label("times their radius");
            if changed {
                ctx.set_setting(REACH, serde_json::json!(self.reach));
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Preview the display");
            ui.add(egui::DragValue::new(&mut self.preview.display));
            ui.label("count");
            ui.add(egui::DragValue::new(&mut self.preview.count).range(1..=10_000));
            if ui.button("Show").clicked() {
                let (display, count) = (self.preview.display, self.preview.count);
                if let Err(why) = self.start_preview(display, count, ctx) {
                    self.preview.said = why;
                }
            }
            if ui.button("Clear").clicked() {
                let display = self.preview.display;
                let _ = self.start_preview(display, 0, ctx);
            }
        });
        if !self.preview.said.is_empty() {
            ui.label(&self.preview.said);
        }
        ui.label(&lock(&self.scene).summary);
        for why in &self.refusals {
            ui.colored_label(ui.visuals().warn_fg_color, why);
        }
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown: the loads are steered here.
        self.steer(ctx);
    }

    fn on_command(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
        ctx: &mut Context,
    ) -> Result<serde_json::Value, String> {
        match name {
            "models.preview" => {
                let display = arguments["display"].as_u64().ok_or("'display' must be a number")? as u32;
                let count = arguments["count"].as_u64().unwrap_or(1).min(10_000) as u32;
                self.start_preview(display, count, ctx)?;
                Ok(serde_json::json!({ "display": display, "count": count }))
            }
            _ => Err(format!("'{name}' is not handled")),
        }
    }

    fn on_event(&mut self, event: &Event, _ctx: &mut Context) {
        forget_failed(&self.service, event);
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if self.setup == Some(job) {
            self.setup = None;
            if let Some(shared) = outcome.take::<Arc<Shared>>() {
                *lock(&self.incoming) = Some(shared.clone());
                self.shared = Some(shared);
            }
        } else if self.preview.job == Some(job) {
            self.preview.job = None;
            if let Some(said) = outcome.take::<Result<String, String>>() {
                self.preview.said = said.unwrap_or_else(|why| why);
            }
        } else if let Some(id) = self.jobs.remove(&job) {
            self.loading.remove(&id);
            match outcome.take::<Loaded>() {
                Some(Some((Ok(look), refused))) => {
                    for why in refused {
                        self.refuse(why);
                    }
                    self.held.insert(id, Arc::new(look));
                    self.service.set_state(id, LookState::Drawn);
                    self.publish();
                }
                Some(Some((Err(why), _))) => {
                    self.service.set_state(id, LookState::Refused(why.clone()));
                    self.refuse(why);
                }
                _ => self.service.set_state(id, LookState::Waiting),
            }
        }
    }
}

uniwow_api::export_module!(ModelsModule::default());
