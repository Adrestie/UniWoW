//! The terrain of a map: its tiles loaded around the camera of the 3D view by jobs of the pool, in
//! the order of `loading`, as many as the GPU budget of its settings holds: full near the camera,
//! kept in a model chunk by chunk as they will be edited, light beyond; uploaded to the GPU by those
//! jobs; handed to the drawing within a time each frame; drawn a draw each by its layer, at a level
//! of detail by their distance, with the horizon of the map beyond them and a fog. Its panel
//! chooses the map. Nothing is changed: no undo entry, no file written.

pub mod gpu;
mod horizon;
mod layer;
mod loading;
pub mod mesh;
pub mod model;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::formats::{self, Formats, Wdt};
use uniwow_api::glam::Vec3;
use uniwow_api::vfs::{self, VfsState};
use uniwow_api::viewport::{Allowance, Demand, Fog};
use uniwow_api::{
    Context, DockArea, JobContext, JobId, JobOutcome, Module, PropertyValue, Registrar, egui, log, serde_json, viewport,
};

use gpu::{Shared, TileGpu};
use horizon::HorizonGpu;
use layer::{Scene, TerrainLayer, lock};
use loading::{Held, Inputs, Kind};
use model::{TILE, TileId, TileModel};

/// The settings: the map shown and how far around the camera tiles load (`formats::distance_setting`,
/// in the window *Settings*). The GPU budget is the view's, shared with its other layers; the
/// terrain's own, before, is given to the view once.
const MAP: &str = "map";
const DISTANCE: &str = "view_distance";
const OLD_BUDGET: &str = "gpu_budget_mb";
const MB: u64 = 1024 * 1024;

/// The time a frame gives to handing ready tiles to the drawing.
const HAND_OVER: Duration = Duration::from_millis(2);
/// The tile loads refused the panel keeps.
const REFUSALS: usize = 8;

/// A map that has terrain, as the panel offers it.
struct MapChoice {
    id: u32,
    directory: String,
    name: String,
    wdt: Arc<Wdt>,
}

/// The maps with terrain, from `formats`, by increasing id.
fn list_maps(formats: &dyn Formats) -> Result<Vec<MapChoice>, String> {
    Ok(formats
        .maps()?
        .iter()
        .filter_map(|map| {
            let wdt = formats.wdt(&map.directory).ok()?;
            wdt.tiles.iter().any(|tile| *tile).then(|| MapChoice {
                id: map.id,
                directory: map.directory.clone(),
                name: map.name.clone(),
                wdt,
            })
        })
        .collect())
}

/// A tile loaded by a job: its model for a full one, and its resources on the GPU; none when
/// cancelled.
type Loaded = Result<Option<(Option<TileModel>, TileGpu)>, String>;

/// Reads the tile `tile` of the map `directory`, then builds its resources as `kind` says, a full
/// tile keeping its model.
fn load(formats: &dyn Formats, shared: &Shared, directory: &str, tile: TileId, kind: Kind, job: &JobContext) -> Loaded {
    let read = formats
        .tile(directory, tile.x, tile.y)?
        .ok_or_else(|| "named by its WDT, but not read".to_owned())?;
    if job.is_cancelled() {
        return Ok(None);
    }
    let cancelled = || job.is_cancelled();
    match kind {
        Kind::Full => {
            let model = TileModel::new(tile, read);
            Ok(gpu::build_tile(shared, formats, &model, &cancelled)?.map(|gpu| (Some(model), gpu)))
        }
        Kind::Light => Ok(gpu::build_light(shared, formats, tile, &read, &cancelled)?.map(|gpu| (None, gpu))),
    }
}

#[derive(Default)]
struct TerrainModule {
    view: Option<viewport::Handle>,
    /// Where the job building the pipeline leaves what the tiles share, for the layer.
    incoming: Arc<Mutex<Option<Arc<Shared>>>>,
    shared: Option<Arc<Shared>>,
    setup: Option<JobId>,
    scene: Arc<Mutex<Scene>>,
    /// The map shown, as the command `terrain.map` gives it from any thread, with the distance of
    /// the terrain.
    shown_map: Arc<Mutex<serde_json::Value>>,
    shared_distance: Arc<AtomicU32>,
    maps: Option<Result<Vec<MapChoice>, String>>,
    maps_job: Option<JobId>,
    /// The map shown, by its index in `maps`; the one the settings name, until the maps are listed.
    shown: Option<usize>,
    remembered: Option<String>,
    /// Counts the maps shown, so that a load for a map left behind is dropped.
    showing: u64,
    /// The models of the full tiles.
    models: HashMap<TileId, TileModel>,
    /// The loads running, by tile, with their job and the kind each loads.
    loading: HashMap<TileId, (JobId, Kind)>,
    jobs: HashMap<JobId, (u64, TileId, Kind)>,
    ready: VecDeque<(Option<TileModel>, TileGpu)>,
    refused: HashSet<TileId>,
    refusals: VecDeque<String>,
    /// The job building the horizon, and the map shown it is for, by `showing`.
    horizon: Option<(JobId, u64)>,
    horizon_for: Option<u64>,
    /// The last frame signal seen.
    frame: u64,
    distance: u32,
    /// What the terrain told the budget of the view last.
    told: Option<Demand>,
    /// When the map shown was chosen, until all the tiles within reach are loaded; then what that
    /// took and how many they are.
    shown_at: Option<Instant>,
    loaded_in: Option<(Duration, usize)>,
    /// What the last plan was made for: none is made again until it changes.
    planned: Option<PlanKey>,
    /// What the models of the full tiles take in memory.
    models_bytes: u64,
    /// The models given up this frame: thousands of allocations each, freed by a job.
    dropped: Vec<TileModel>,
}

/// What a plan of the loads depends on: the camera, by eighths of a tile; the distance; what the
/// budget of the view allows, the reaches by eighths of a tile and what the other layers hold by
/// 16 MB; and the counts of the changes of the tiles, of their loads and of the textures.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PlanKey {
    eye: [i32; 2],
    distance: u32,
    allowed: [u64; 4],
    tiles: u64,
    loading: usize,
    ready: usize,
    refused: usize,
    textures: u64,
}

impl TerrainModule {
    fn wdt(&self) -> Option<&Arc<Wdt>> {
        let maps = self.maps.as_ref()?.as_ref().ok()?;
        Some(&maps.get(self.shown?)?.wdt)
    }

    /// Forgets the tiles of the map shown, its loads cancelled.
    fn reset(&mut self, ctx: &mut Context) {
        for (job, _) in self.loading.values() {
            ctx.cancel(*job);
        }
        self.loading.clear();
        self.ready.clear();
        self.dropped.extend(self.models.drain().map(|(_, model)| model));
        self.models_bytes = 0;
        self.planned = None;
        self.refused.clear();
        self.refusals.clear();
        self.loaded_in = None;
        let mut scene = lock(&self.scene);
        scene.tiles.clear();
        scene.limited = None;
        scene.horizon = None;
        scene.map = None;
        scene.generation += 1;
        drop(scene);
        self.purge_textures(ctx);
        self.showing += 1;
    }

    /// Forgets the textures no tile holds any more, in a job: it waits for the jobs placing
    /// textures, which the interface thread never does.
    fn purge_textures(&self, ctx: &mut Context) {
        if let Some(shared) = self.shared.clone() {
            ctx.spawn("Forget the textures of the terrain no tile holds", move |_| {
                shared.textures.purge()
            });
        }
    }

    /// Shows the map `index` of the list, the camera over its middle.
    fn show(&mut self, index: usize, ctx: &mut Context) {
        self.reset(ctx);
        self.shown = Some(index);
        let Some(Ok(maps)) = &self.maps else {
            return;
        };
        let map = &maps[index];
        ctx.set_setting(MAP, serde_json::json!(map.directory));
        *lock(&self.shown_map) = serde_json::json!({ "id": map.id, "directory": map.directory, "name": map.name });
        self.shown_at = Some(Instant::now());
        let tiles: Vec<TileId> = (0..4096u32)
            .filter(|i| map.wdt.tiles[*i as usize])
            .map(|i| TileId { x: i % 64, y: i / 64 })
            .collect();
        let corners = tiles.iter().map(|tile| tile.corner());
        lock(&self.scene).map = Some(corners.fold([[f32::MAX; 2], [f32::MIN; 2]], |[low, high], [x, y]| {
            [
                [low[0].min(x - TILE), low[1].min(y - TILE)],
                [high[0].max(x), high[1].max(y)],
            ]
        }));
        let count = tiles.len().max(1) as f32;
        let middle = [
            tiles.iter().map(|t| t.centre()[0]).sum::<f32>() / count,
            tiles.iter().map(|t| t.centre()[1]).sum::<f32>() / count,
        ];
        let Some(nearest) = tiles
            .iter()
            .min_by(|a, b| loading::distance(**a, middle).total_cmp(&loading::distance(**b, middle)))
        else {
            return;
        };
        let [x, y] = nearest.centre().map(f64::from);
        // High enough to be over the mountains of the maps of 3.3.5a.
        for (property, value) in [
            ("viewport/camera_target", [x, y, 100.0]),
            ("viewport/camera_position", [x + 800.0, y, 1200.0]),
        ] {
            if let Err(error) = ctx.write_property(property, PropertyValue::Vector(value)) {
                log::warn!("the camera is not moved to the map: {error}");
            }
        }
    }

    /// Hands the tiles ready to the drawing, for `HAND_OVER` at most, one at least: each takes the
    /// place of the one of the other kind it replaces, a full one bringing its model.
    fn hand_over(&mut self) {
        let start = Instant::now();
        let shared_scene = self.scene.clone();
        let mut scene = lock(&shared_scene);
        while let Some((model, gpu)) = self.ready.pop_front() {
            let id = gpu.id;
            self.keep_model(id, model);
            let gpu = Arc::new(gpu);
            match scene.tiles.iter_mut().find(|tile| tile.id == id) {
                Some(place) => *place = gpu,
                None => scene.tiles.push(gpu),
            }
            scene.generation += 1;
            if start.elapsed() >= HAND_OVER {
                break;
            }
        }
    }

    /// The tiles held, drawn or ready, with their kind and what they take on the GPU: one ready to
    /// take the place of another counts both until then.
    fn held(&self) -> HashMap<TileId, Held> {
        let changed = |id: &TileId| self.models.get(id).is_some_and(TileModel::changed);
        let mut held: HashMap<TileId, Held> = lock(&self.scene)
            .tiles
            .iter()
            .map(|tile| {
                let held = Held {
                    kind: tile.kind,
                    bytes: tile.bytes,
                    changed: changed(&tile.id),
                };
                (tile.id, held)
            })
            .collect();
        for (_, gpu) in &self.ready {
            let before = held.get(&gpu.id).map_or(0, |held| held.bytes);
            held.insert(
                gpu.id,
                Held {
                    kind: gpu.kind,
                    bytes: before + gpu.bytes,
                    changed: changed(&gpu.id),
                },
            );
        }
        held
    }

    /// What the terrain takes on the GPU besides its tiles: the arrays of textures and the horizon.
    fn fixed(&self) -> u64 {
        let horizon = lock(&self.scene).horizon.as_ref().map_or(0, |horizon| horizon.bytes);
        horizon + self.shared.as_ref().map_or(0, |shared| shared.textures.bytes())
    }

    /// Takes the outcome of the job `job` if it loaded a tile: only the load the plan still waits
    /// for counts. One cancelled once done, one of a kind no longer wanted or for a map left is
    /// dropped, its model freed by a job.
    fn tile_loaded(&mut self, job: JobId, outcome: JobOutcome) {
        let Some((showing, tile, kind)) = self.jobs.remove(&job) else {
            return;
        };
        if showing != self.showing || self.loading.get(&tile) != Some(&(job, kind)) {
            if let Some(Ok(Some((model, _)))) = outcome.take::<Loaded>() {
                self.dropped.extend(model);
            }
            return;
        }
        self.loading.remove(&tile);
        let refusal = match outcome {
            JobOutcome::Panicked(message) => Some(message),
            JobOutcome::Cancelled => None,
            outcome => match outcome.take::<Loaded>() {
                Some(Ok(Some(loaded))) => {
                    self.ready.push_back(loaded);
                    None
                }
                Some(Err(reason)) => Some(reason),
                _ => None,
            },
        };
        if let Some(reason) = refusal {
            log::warn!("the tile {} {} is not drawn: {reason}", tile.x, tile.y);
            self.refused.insert(tile);
            self.refusals.push_back(format!("Tile {} {}: {reason}", tile.x, tile.y));
            if self.refusals.len() > REFUSALS {
                self.refusals.pop_front();
            }
        }
    }

    /// Keeps `model` as the model of the tile `id`, or none, counting what the models take.
    fn keep_model(&mut self, id: TileId, model: Option<TileModel>) {
        if let Some(model) = &model {
            self.models_bytes += model.bytes();
        }
        let old = match model {
            Some(model) => self.models.insert(id, model),
            None => self.models.remove(&id),
        };
        if let Some(old) = old {
            self.models_bytes -= old.bytes();
            self.dropped.push(old);
        }
    }

    /// Releases the tiles `released`, ready or drawn, and their models.
    fn release(&mut self, released: &[TileId], ctx: &mut Context) {
        if released.is_empty() {
            return;
        }
        let mut scene = lock(&self.scene);
        scene.tiles.retain(|tile| !released.contains(&tile.id));
        scene.generation += 1;
        drop(scene);
        self.ready.retain(|(_, gpu)| !released.contains(&gpu.id));
        for tile in released {
            self.keep_model(*tile, None);
        }
        self.purge_textures(ctx);
    }

    /// Starts building the horizon of the map shown, once what the terrain shares is built.
    fn build_horizon(&mut self, ctx: &mut Context, shared: &Arc<Shared>, formats: &Arc<dyn Formats>, directory: &str) {
        if self.horizon_for == Some(self.showing) {
            return;
        }
        self.horizon_for = Some(self.showing);
        let (shared, formats, directory) = (shared.clone(), formats.clone(), directory.to_owned());
        let job = ctx.spawn(&format!("Build the horizon of {directory}"), move |_| {
            Ok::<_, String>(formats.wdl(&directory)?.and_then(|wdl| horizon::build(&shared, &wdl)))
        });
        self.horizon = Some((job, self.showing));
    }

    /// What the terrain may take of the budget allowed: all of it but what the other layers hold.
    fn budget(&self, allowance: &Allowance) -> u64 {
        let own = self.told.as_ref().map_or(0, Demand::used);
        allowance.budget.saturating_sub(allowance.used.saturating_sub(own))
    }

    /// `allowance` as a plan depends on it.
    fn allowed(&self, allowance: &Allowance) -> [u64; 4] {
        let tiles = |reach: f32| {
            if reach.is_finite() {
                (reach / TILE * 8.0) as u64
            } else {
                u64::MAX
            }
        };
        [
            allowance.budget / MB,
            self.budget(allowance) / (16 * MB),
            tiles(allowance.load),
            tiles(allowance.keep),
        ]
    }

    /// At each frame: hands over what is ready, then, while the view is drawn, starts the loads the
    /// camera wants, cancels those it left, and keeps the terrain to its budget; then tells the
    /// layer how long it took, how far the tiles load and what the terrain takes on the GPU.
    fn steer(&mut self, ctx: &mut Context) {
        let start = Instant::now();
        self.distance = formats::distance_setting(DISTANCE).value(ctx.setting(DISTANCE).as_ref()) as u32;
        self.shared_distance.store(self.distance, Ordering::Relaxed);
        self.steer_loads(ctx);
        if !self.dropped.is_empty() {
            let dropped = std::mem::take(&mut self.dropped);
            ctx.spawn("Free the models of the tiles left", move |_| drop(dropped));
        }
        let fixed = self.fixed();
        let ready: u64 = self.ready.iter().map(|(_, gpu)| gpu.bytes).sum();
        let eye = match ctx.read_property("viewport/camera_position") {
            Ok(PropertyValue::Vector([x, y, z])) => Some(Vec3::new(x as f32, y as f32, z as f32)),
            _ => None,
        };
        let mut scene = lock(&self.scene);
        let used = fixed + ready + scene.tiles.iter().map(|tile| tile.bytes).sum::<u64>();
        // The fog follows the reach the budget leaves.
        let reach = scene
            .limited
            .map_or(self.distance as f32, |limited| limited.min(self.distance as f32));
        scene.reach = (reach + 0.5) * TILE;
        scene.bytes = used;
        scene.models = self.models_bytes;
        // The fog of the view: more than half of it at the reach of the tiles loaded, where the
        // horizon starts, all of it at the farthest of the map.
        if let (Some(view), Some(eye)) = (&self.view, eye) {
            let far = scene.map.map_or(scene.reach * 2.0, |map| layer::farthest(eye, map));
            view.set_fog(Fog {
                start: 0.5 * scene.reach,
                middle: scene.reach,
                end: far.max(scene.reach * 1.01),
                ..Fog::default()
            });
        }
        scene.steering = start.elapsed();
    }

    fn steer_loads(&mut self, ctx: &mut Context) {
        if self.maps.is_none()
            && self.maps_job.is_none()
            && ctx
                .service(vfs::SERVICE)
                .is_some_and(|files| matches!(files.state(), VfsState::Ready { .. }))
            && let Some(formats) = ctx.service(formats::SERVICE)
        {
            self.maps_job = Some(ctx.spawn("List the maps with terrain", move |_| list_maps(&*formats)));
        }
        let (Some(view), Some(shared), Some(wdt), Some(formats)) = (
            self.view.clone(),
            self.shared.clone(),
            self.wdt().cloned(),
            ctx.service(formats::SERVICE),
        ) else {
            return;
        };
        self.hand_over();
        // The loads pause while the view is not drawn: no frame signal then.
        let Some(frame) = view.wait_frame(self.frame, Duration::ZERO) else {
            return;
        };
        self.frame = frame.number;
        let eye = match ctx.read_property("viewport/camera_position") {
            Ok(PropertyValue::Vector([x, y, _])) => [x as f32, y as f32],
            _ => return,
        };
        let directory = self
            .maps
            .as_ref()
            .and_then(|maps| maps.as_ref().ok())
            .and_then(|maps| maps.get(self.shown?).map(|map| map.directory.clone()));
        let Some(directory) = directory else {
            return;
        };
        self.build_horizon(ctx, &shared, &formats, &directory);
        let allowance = view.allowance();
        let mut key = PlanKey {
            eye: eye.map(|at| (at / TILE * 8.0).floor() as i32),
            distance: self.distance,
            allowed: self.allowed(&allowance),
            tiles: lock(&self.scene).generation,
            loading: self.loading.len(),
            ready: self.ready.len(),
            refused: self.refused.len(),
            textures: shared.textures.generation(),
        };
        if self.planned == Some(key) {
            return;
        }
        let wanted = loading::wanted(&wdt.tiles, eye, self.distance);
        let held = self.held();
        let fixed = self.fixed();
        let loading: HashMap<TileId, Kind> = self.loading.iter().map(|(tile, (_, kind))| (*tile, *kind)).collect();
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get());
        let mut inputs = Inputs {
            wanted: &wanted,
            held: &held,
            loading: &loading,
            refused: &self.refused,
            used: fixed + held.values().map(|held| held.bytes).sum::<u64>(),
            budget: self.budget(&allowance),
            allowance,
            costs: loading::costs(&held),
            slots: workers.saturating_sub(1).max(1),
        };
        // What the terrain wants told to the budget of the view, which answers with what it allows
        // now; planned with that, and with the same at the next frame unless something changed.
        let demand = loading::demand(&inputs, eye, fixed);
        if self.told.as_ref() != Some(&demand) {
            inputs.allowance = view.tell_budget(ctx.module_id(), demand.clone());
            self.told = Some(demand);
            inputs.budget = self.budget(&inputs.allowance);
            key.allowed = self.allowed(&inputs.allowance);
        }
        self.planned = Some(key);
        let plan = loading::plan(&inputs);
        lock(&self.scene).limited = plan.limited;
        for tile in &plan.cancel {
            if let Some((job, _)) = self.loading.remove(tile) {
                ctx.cancel(job);
            }
        }
        self.release(&plan.release, ctx);
        for &(tile, kind) in &plan.start {
            let (formats, shared, directory) = (formats.clone(), shared.clone(), directory.clone());
            let label = match kind {
                Kind::Full => format!("Load the tile {} {} of {directory}", tile.x, tile.y),
                Kind::Light => format!("Load the tile {} {} of {directory}, light", tile.x, tile.y),
            };
            let job = ctx.spawn(&label, move |job| load(&*formats, &shared, &directory, tile, kind, job));
            self.loading.insert(tile, (job, kind));
            self.jobs.insert(job, (self.showing, tile, kind));
        }
        let done = plan.start.is_empty()
            && plan.limited.is_none()
            && self.loading.is_empty()
            && self.ready.is_empty()
            && wanted.iter().all(|(tile, distance)| {
                self.refused.contains(tile)
                    || held
                        .get(tile)
                        .is_some_and(|held| held.kind == loading::kind(*distance, Some(held.kind), held.changed))
            });
        if done && let Some(shown_at) = self.shown_at.take() {
            let took = shown_at.elapsed();
            self.loaded_in = Some((took, wanted.len()));
            log::info!(
                "{directory}: the {} tiles within {} tiles loaded in {:.1} s",
                wanted.len(),
                self.distance,
                took.as_secs_f32()
            );
        }
    }
}

impl Module for TerrainModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("terrain", "Terrain", DockArea::Right)
            .settings("Terrain", vec![formats::distance_setting(DISTANCE)]);
        let (shown, distance) = (self.shown_map.clone(), self.shared_distance.clone());
        reg.command_on_caller(
            "terrain.map",
            "The map the terrain shows: its id in Map.dbc, its folder and its name, and how far around the \
             camera its tiles are loaded, in tiles; null while none is shown.",
            serde_json::json!({ "type": "object" }),
            serde_json::json!({
                "type": ["object", "null"],
                "properties": {
                    "id": { "type": "integer" },
                    "directory": { "type": "string" },
                    "name": { "type": "string" },
                    "distance": { "type": "integer" }
                }
            }),
            Arc::new(move |_| {
                let mut shown = lock(&shown).clone();
                if let Some(map) = shown.as_object_mut() {
                    map.insert("distance".to_owned(), distance.load(Ordering::Relaxed).into());
                }
                Ok(shown)
            }),
        );
    }

    fn init(&mut self, ctx: &mut Context) {
        self.remembered = ctx.setting(MAP).and_then(|value| value.as_str().map(str::to_owned));
        let Some(view) = ctx.service(viewport::SERVICE) else {
            log::info!("no viewport service: the terrain is not drawn");
            return;
        };
        if let Some(mb) = ctx.setting(OLD_BUDGET).and_then(|value| value.as_u64()) {
            view.set_budget(mb * MB);
            ctx.set_setting(OLD_BUDGET, serde_json::Value::Null);
        }
        let Some(gpu) = ctx.gpu().cloned() else {
            return;
        };
        view.add_layer(
            ctx.module_id(),
            Box::new(TerrainLayer::new(self.incoming.clone(), self.scene.clone())),
        );
        let target = view.target();
        self.view = Some(view);
        self.setup = Some(ctx.spawn("Build the terrain's pipeline", move |_| {
            Shared::new(&gpu, &target).map(Arc::new)
        }));
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown: the loads are steered here.
        self.steer(ctx);
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        let mut chosen = None;
        match &self.maps {
            None => {
                ui.weak("Waiting for the client's files (panel Assets).");
            }
            Some(Err(reason)) => {
                ui.colored_label(ui.visuals().warn_fg_color, reason);
            }
            Some(Ok(maps)) => {
                let label = |map: &MapChoice| format!("{} ({})", map.name, map.directory);
                let selected = self
                    .shown
                    .and_then(|index| maps.get(index))
                    .map_or_else(|| "Choose a map".to_owned(), label);
                egui::ComboBox::from_label("Map")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (index, map) in maps.iter().enumerate() {
                            let tiles = map.wdt.tiles.iter().filter(|tile| **tile).count();
                            if ui
                                .selectable_label(self.shown == Some(index), format!("{} — {tiles} tiles", label(map)))
                                .clicked()
                            {
                                chosen = Some(index);
                            }
                        }
                    });
            }
        }
        if let Some(index) = chosen {
            self.show(index, ctx);
        }
        if self.view.is_none() {
            ui.colored_label(ui.visuals().warn_fg_color, "No 3D view: the terrain is not drawn.");
            return;
        }
        let scene = lock(&self.scene);
        let full = scene.tiles.iter().filter(|tile| tile.kind == Kind::Full).count();
        let (light, used, limited) = (scene.tiles.len() - full, scene.bytes, scene.limited);
        drop(scene);
        let textures = match &self.shared {
            Some(shared) if shared.textures.block_compression() => "textures as stored (BC)",
            Some(_) => "textures decoded (RGBA): the device has no BC",
            None => "pipeline being built",
        };
        let allowance = self.view.as_ref().map(|view| view.allowance()).unwrap_or_default();
        ui.label(format!(
            "{} tiles drawn ({full} full, {light} light), {} loading; {:.0} MB on the GPU, the view {:.0} of {} MB; {textures}",
            full + light,
            self.loading.len(),
            used as f64 / MB as f64,
            allowance.used as f64 / MB as f64,
            allowance.budget / MB
        ));
        if let Some(shared) = &self.shared {
            let counts = shared.textures.counts();
            ui.label(format!(
                "Textures: {} placed, {} unreadable, {} waiting for room; {} of {} arrays",
                counts.placed,
                counts.unreadable,
                counts.no_room,
                counts.arrays,
                gpu::SLOTS
            ));
        }
        if let Some(limited) = limited {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Reach limited by the budget: {limited:.0} tiles"),
            );
        }
        if let Some((took, tiles)) = self.loaded_in {
            ui.label(format!(
                "All {tiles} tiles within reach loaded in {:.1} s",
                took.as_secs_f32()
            ));
        }
        for refusal in &self.refusals {
            ui.colored_label(ui.visuals().warn_fg_color, refusal);
        }
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if self.setup == Some(job) {
            self.setup = None;
            match outcome {
                JobOutcome::Panicked(message) => log::error!("the terrain's pipeline could not be built: {message}"),
                outcome => match outcome.take::<Result<Arc<Shared>, String>>() {
                    Some(Ok(shared)) => {
                        *lock(&self.incoming) = Some(shared.clone());
                        self.shared = Some(shared);
                    }
                    Some(Err(reason)) => log::error!("the terrain is not drawn: {reason}"),
                    None => {}
                },
            }
        } else if self.maps_job == Some(job) {
            self.maps_job = None;
            let maps = match outcome {
                JobOutcome::Panicked(message) => Err(message),
                outcome => outcome
                    .take::<Result<Vec<MapChoice>, String>>()
                    .unwrap_or_else(|| Err("the maps were not listed".to_owned())),
            };
            let remembered = maps.as_ref().ok().and_then(|maps| {
                maps.iter()
                    .position(|map| Some(&map.directory) == self.remembered.as_ref())
            });
            self.maps = Some(maps);
            if let Some(index) = remembered {
                self.show(index, ctx);
            }
        } else if let Some((_, showing)) = self.horizon.filter(|(horizon, _)| *horizon == job) {
            self.horizon = None;
            let horizon = match outcome {
                JobOutcome::Panicked(message) => Err(message),
                outcome => outcome.take::<Result<Option<HorizonGpu>, String>>().unwrap_or(Ok(None)),
            };
            match horizon {
                Ok(Some(horizon)) if showing == self.showing => {
                    let mut scene = lock(&self.scene);
                    scene.horizon = Some(Arc::new(horizon));
                    scene.generation += 1;
                }
                Ok(_) => {}
                Err(reason) => log::warn!("the horizon of the map is not drawn: {reason}"),
            }
        } else {
            self.tile_loaded(job, outcome);
        }
    }
}

uniwow_api::export_module!(TerrainModule::default());
