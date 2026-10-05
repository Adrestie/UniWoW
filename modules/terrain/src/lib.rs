//! The terrain of a map: its tiles loaded around the camera of the 3D view by jobs of the pool, in
//! the order of `loading`; kept in a model chunk by chunk, as they will be edited; uploaded to the
//! GPU by those jobs; handed to the drawing within a time each frame; released beyond the GPU
//! budget of its settings; drawn a draw each by its layer, at a level of detail by their distance,
//! with the horizon of the map beyond them and a fog. Its panel chooses the map. Nothing is
//! changed: no undo entry, no file written.

pub mod gpu;
mod horizon;
mod layer;
mod loading;
pub mod mesh;
pub mod model;
#[cfg(test)]
mod tests;
mod textures;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::formats::{self, Formats, Wdt};
use uniwow_api::vfs::{self, VfsState};
use uniwow_api::{
    Context, DockArea, JobContext, JobId, JobOutcome, Module, PropertyValue, Registrar, egui, log, serde_json, viewport,
};

use gpu::{Shared, TileGpu};
use horizon::HorizonGpu;
use layer::{Scene, TerrainLayer, lock};
use loading::Kept;
use model::{TILE, TileId, TileModel};

/// The settings: the map shown, how far around the camera tiles load, and the GPU budget.
const MAP: &str = "map";
const DISTANCE: &str = "view_distance";
const BUDGET: &str = "gpu_budget_mb";
const DEFAULT_DISTANCE: u32 = 3;
/// The GPU budget in MB when the memory of the GPU is not told, and the least and most it can be.
const FALLBACK_BUDGET: u64 = 1024;
const BUDGETS: [u64; 2] = [64, 65_536];

/// The GPU budget by default, in MB: half the memory of its own of the GPU, `gpu_memory` bytes,
/// when the system tells it.
fn default_budget(gpu_memory: Option<u64>) -> u64 {
    gpu_memory
        .map_or(FALLBACK_BUDGET, |bytes| bytes / 2 / (1024 * 1024))
        .clamp(BUDGETS[0], BUDGETS[1])
}

/// The time a frame gives to handing ready tiles to the drawing.
const HAND_OVER: Duration = Duration::from_millis(2);
/// The tile loads refused the panel keeps.
const REFUSALS: usize = 8;

/// A map that has terrain, as the panel offers it.
struct MapChoice {
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
                directory: map.directory.clone(),
                name: map.name.clone(),
                wdt,
            })
        })
        .collect())
}

/// A tile loaded by a job: its model and its resources on the GPU; none when cancelled.
type Loaded = Result<Option<(TileModel, TileGpu)>, String>;

/// Reads the tile `tile` of the map `directory` into its model, then builds its resources.
fn load(formats: &dyn Formats, shared: &Shared, directory: &str, tile: TileId, job: &JobContext) -> Loaded {
    let read = formats
        .tile(directory, tile.x, tile.y)?
        .ok_or_else(|| "named by its WDT, but not read".to_owned())?;
    if job.is_cancelled() {
        return Ok(None);
    }
    let model = TileModel::new(tile, read);
    Ok(gpu::build_tile(shared, formats, &model, &|| job.is_cancelled())?.map(|gpu| (model, gpu)))
}

#[derive(Default)]
struct TerrainModule {
    view: Option<viewport::Handle>,
    /// Where the job building the pipeline leaves what the tiles share, for the layer.
    incoming: Arc<Mutex<Option<Arc<Shared>>>>,
    shared: Option<Arc<Shared>>,
    setup: Option<JobId>,
    scene: Arc<Mutex<Scene>>,
    maps: Option<Result<Vec<MapChoice>, String>>,
    maps_job: Option<JobId>,
    /// The map shown, by its index in `maps`; the one the settings name, until the maps are listed.
    shown: Option<usize>,
    remembered: Option<String>,
    /// Counts the maps shown, so that a load for a map left behind is dropped.
    showing: u64,
    models: HashMap<TileId, TileModel>,
    loading: HashMap<TileId, JobId>,
    jobs: HashMap<JobId, (u64, TileId)>,
    ready: VecDeque<(TileModel, TileGpu)>,
    refused: HashSet<TileId>,
    refusals: VecDeque<String>,
    /// The job building the horizon, and the map shown it is for, by `showing`.
    horizon: Option<(JobId, u64)>,
    horizon_for: Option<u64>,
    /// The last frame signal seen.
    frame: u64,
    distance: u32,
    budget_mb: u64,
}

impl TerrainModule {
    fn wdt(&self) -> Option<&Arc<Wdt>> {
        let maps = self.maps.as_ref()?.as_ref().ok()?;
        Some(&maps.get(self.shown?)?.wdt)
    }

    /// Forgets the tiles of the map shown, its loads cancelled.
    fn reset(&mut self, ctx: &mut Context) {
        for job in self.loading.values() {
            ctx.cancel(*job);
        }
        self.loading.clear();
        self.ready.clear();
        self.models.clear();
        self.refused.clear();
        self.refusals.clear();
        let mut scene = lock(&self.scene);
        scene.tiles.clear();
        scene.seen.clear();
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

    /// Hands the tiles ready to the drawing, for `HAND_OVER` at most, one at least.
    fn hand_over(&mut self) {
        let start = Instant::now();
        let mut scene = lock(&self.scene);
        while let Some((model, gpu)) = self.ready.pop_front() {
            self.models.insert(model.id, model);
            scene.tiles.push(Arc::new(gpu));
            scene.generation += 1;
            if start.elapsed() >= HAND_OVER {
                break;
            }
        }
    }

    /// The bytes the terrain takes on the GPU: its tiles, those ready, their textures and the horizon.
    fn used(&self) -> u64 {
        let scene = lock(&self.scene);
        let tiles: u64 = scene.tiles.iter().map(|tile| tile.bytes).sum();
        let horizon = scene.horizon.as_ref().map_or(0, |horizon| horizon.bytes);
        drop(scene);
        let ready: u64 = self.ready.iter().map(|(_, gpu)| gpu.bytes).sum();
        tiles + ready + horizon + self.shared.as_ref().map_or(0, |shared| shared.textures.bytes())
    }

    /// Releases tiles out of sight while the terrain takes more than its budget.
    fn keep_to_budget(&mut self, eye: [f32; 2], ctx: &mut Context) {
        let used = self.used();
        let budget = self.budget_mb * 1024 * 1024;
        if used <= budget {
            return;
        }
        let mut scene = lock(&self.scene);
        let kept: Vec<Kept> = scene
            .tiles
            .iter()
            .map(|tile| Kept {
                tile: tile.id,
                bytes: tile.bytes,
                seen: scene.seen.get(&tile.id).copied().unwrap_or(0),
            })
            .collect();
        let released = loading::release(&kept, scene.frame, eye, used, budget);
        if released.is_empty() {
            return;
        }
        scene.tiles.retain(|tile| !released.contains(&tile.id));
        scene.generation += 1;
        for tile in &released {
            scene.seen.remove(tile);
            self.models.remove(tile);
        }
        drop(scene);
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

    /// At each frame: hands over what is ready, then, while the view is drawn, starts the loads the
    /// camera wants, cancels those it left, and keeps the terrain to its budget; then tells the
    /// layer how long it took, how far the tiles load and what the terrain takes on the GPU.
    fn steer(&mut self, ctx: &mut Context) {
        let start = Instant::now();
        self.steer_loads(ctx);
        let used = self.used();
        let mut scene = lock(&self.scene);
        scene.reach = (self.distance as f32 + 0.5) * TILE;
        scene.bytes = used;
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
        let wanted = loading::wanted(&wdt.tiles, eye, self.distance);
        let loading: HashSet<TileId> = self.loading.keys().copied().collect();
        let present: HashSet<TileId> = self
            .models
            .keys()
            .copied()
            .chain(self.ready.iter().map(|(model, _)| model.id))
            .chain(self.refused.iter().copied())
            .collect();
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get());
        let plan = loading::plan(&wanted, &loading, &present, workers.saturating_sub(1).max(1));
        for tile in plan.cancel {
            if let Some(job) = self.loading.remove(&tile) {
                ctx.cancel(job);
            }
        }
        let directory = self
            .maps
            .as_ref()
            .and_then(|maps| maps.as_ref().ok())
            .and_then(|maps| maps.get(self.shown?).map(|map| map.directory.clone()));
        let Some(directory) = directory else {
            return;
        };
        self.build_horizon(ctx, &shared, &formats, &directory);
        for tile in plan.start {
            let (formats, shared, directory) = (formats.clone(), shared.clone(), directory.clone());
            let job = ctx.spawn(
                &format!("Load the tile {} {} of {directory}", tile.x, tile.y),
                move |job| load(&*formats, &shared, &directory, tile, job),
            );
            self.loading.insert(tile, job);
            self.jobs.insert(job, (self.showing, tile));
        }
        self.keep_to_budget(eye, ctx);
    }
}

impl Module for TerrainModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("terrain", "Terrain", DockArea::Right);
    }

    fn init(&mut self, ctx: &mut Context) {
        self.distance = ctx
            .setting(DISTANCE)
            .and_then(|value| value.as_u64())
            .map_or(DEFAULT_DISTANCE, |value| value.clamp(1, 8) as u32);
        self.budget_mb = ctx
            .setting(BUDGET)
            .and_then(|value| value.as_u64())
            .unwrap_or_else(|| default_budget(ctx.gpu_memory()))
            .clamp(BUDGETS[0], BUDGETS[1]);
        self.remembered = ctx.setting(MAP).and_then(|value| value.as_str().map(str::to_owned));
        let Some(view) = ctx.service(viewport::SERVICE) else {
            log::info!("no viewport service: the terrain is not drawn");
            return;
        };
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
            Arc::new(Shared::new(&gpu, &target))
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
        let used = self.used() as f64 / (1024.0 * 1024.0);
        let textures = match &self.shared {
            Some(shared) if shared.textures.block_compression() => "textures as stored (BC)",
            Some(_) => "textures decoded (RGBA): the device has no BC",
            None => "pipeline being built",
        };
        ui.label(format!(
            "{} tiles drawn, {} loading; {used:.0} MB of {} MB on the GPU; {textures}",
            lock(&self.scene).tiles.len(),
            self.loading.len(),
            self.budget_mb
        ));
        ui.horizontal(|ui| {
            ui.label("Distance (tiles)");
            if ui.add(egui::DragValue::new(&mut self.distance).range(1..=8)).changed() {
                ctx.set_setting(DISTANCE, serde_json::json!(self.distance));
            }
            ui.label("GPU budget (MB)");
            if ui
                .add(
                    egui::DragValue::new(&mut self.budget_mb)
                        .range(BUDGETS[0]..=BUDGETS[1])
                        .speed(16),
                )
                .changed()
            {
                ctx.set_setting(BUDGET, serde_json::json!(self.budget_mb));
            }
        });
        for refusal in &self.refusals {
            ui.colored_label(ui.visuals().warn_fg_color, refusal);
        }
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if self.setup == Some(job) {
            self.setup = None;
            match outcome {
                JobOutcome::Panicked(message) => log::error!("the terrain's pipeline could not be built: {message}"),
                outcome => {
                    if let Some(shared) = outcome.take::<Arc<Shared>>() {
                        *lock(&self.incoming) = Some(shared.clone());
                        self.shared = Some(shared);
                    }
                }
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
        } else if let Some((showing, tile)) = self.jobs.remove(&job) {
            if self.loading.get(&tile) == Some(&job) {
                self.loading.remove(&tile);
            }
            if showing != self.showing {
                return;
            }
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
    }
}

uniwow_api::export_module!(TerrainModule::default());
