//! The buildings (WMO) of the tiles of the map the terrain shows, around the camera of the 3D view
//! within a distance of their own: the placements of each tile read by a job, the nearest first, a
//! building kept by its unique id while a tile listing it is held; each file read once by a job and
//! put on the GPU for all its placements, within what the budget of the view allows; drawn by the
//! layer of the module, their doodads placed through the service `models`. Nothing is changed: no
//! undo entry, no file written.

mod budget;
mod cells;
#[cfg(test)]
mod cells_tests;
mod colours;
mod doodads;
mod gpu;
mod keeping;
mod layer;
mod liquid;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use uniwow_api::arena::{self, Refusal};
use uniwow_api::formats::{self, Building, DoodadSet, FileRef, TileId, Wdt, WmoDoodad};
use uniwow_api::glam::Vec3;
use uniwow_api::serde_json::json;
use uniwow_api::viewport::Demand;
use uniwow_api::{
    Context, DockArea, JobId, JobOutcome, Module, PropertyValue, Registrar, egui, liquids, log, models, viewport,
};

use doodads::Owners;
use gpu::{Shared, WmoGpu};
use keeping::Kept;
use layer::{BuildingsLayer, Placed, Poured, Scene};

/// The setting of how far around the camera tiles are read, in tiles.
const DISTANCE: &str = "distance";
const DEFAULT_DISTANCE: u32 = 3;
const DISTANCES: [u32; 2] = [1, 8];
/// The refusals the panel keeps.
const REFUSALS: usize = 8;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A file of a building read: on the GPU, what its doodads need, and the liquids of its groups.
pub struct WmoFile {
    pub gpu: Arc<WmoGpu>,
    pub sets: Vec<DoodadSet>,
    pub doodads: Vec<WmoDoodad>,
    /// The groups holding each doodad.
    pub holders: Vec<Vec<u16>>,
    pub liquids: Vec<liquid::GroupLiquid>,
}

impl WmoFile {
    /// What it keeps on the CPU: its groups, its sets and its doodads with their names and the
    /// groups holding them.
    fn cpu(&self) -> u64 {
        let named = |file: &FileRef| match file {
            FileRef::Path(path) => path.len(),
            FileRef::Id(_) => 0,
        };
        self.gpu.cpu()
            + (self.sets.len() * size_of::<DoodadSet>()
                + self
                    .doodads
                    .iter()
                    .map(|doodad| size_of::<WmoDoodad>() + named(&doodad.file))
                    .sum::<usize>()
                + self
                    .holders
                    .iter()
                    .map(|held| size_of::<Vec<u16>>() + held.len() * 2)
                    .sum::<usize>()) as u64
    }
}

enum FileState {
    /// Wanted, not yet read: beyond what the budget allows, or waiting for its turn.
    Waiting,
    Loading(JobId),
    Ready(Arc<WmoFile>),
    /// Refused for want of room, when the arenas had given back so many ranges and the camera
    /// stood there: read again once room may have been made (`arena::room_made`).
    NoRoom(u64, [f32; 2]),
    Refused,
}

/// A file and the buildings of it kept, by their unique id.
struct FileEntry {
    state: FileState,
    users: HashSet<u32>,
}

/// Where a file in `state` stands for the budget, the arenas having given back `given` ranges and
/// the camera at `eye`: one refused for want of room waiting again once room may have been made.
fn held(state: &FileState, given: u64, eye: [f32; 2]) -> budget::Held {
    match state {
        FileState::Waiting => budget::Held::Waiting,
        FileState::Loading(_) => budget::Held::Loading,
        FileState::Ready(read) => budget::Held::Ready(read.gpu.bytes),
        FileState::NoRoom(then, at) if arena::room_made(*then, *at, given, eye) => budget::Held::Waiting,
        FileState::NoRoom(..) => budget::Held::NoRoom,
        FileState::Refused => budget::Held::Refused,
    }
}

/// The owner of the liquids of the building `id` of the map `directory`.
fn owner(directory: &str, id: u32) -> String {
    format!("buildings/{directory}/{id}")
}

/// The buildings a tile lists, or why it could not be read.
type Read = Result<Vec<Building>, String>;
/// A file read and put on the GPU, or why not.
type Loaded = Result<WmoFile, Refusal>;

#[derive(Default)]
struct BuildingsModule {
    distance: u32,
    shared: Option<Arc<Shared>>,
    /// Why the device does not draw the buildings.
    refused_device: Option<String>,
    scene: Arc<Mutex<Scene>>,
    owners: Arc<Mutex<Owners>>,
    /// The map the terrain shows, by its folder; its WDT once read, or why it could not be, and
    /// the job reading it.
    map: Option<String>,
    wdt: Option<Result<Arc<Wdt>, String>>,
    reading_wdt: Option<JobId>,
    /// The buildings of the tiles held, the reads of tiles running, by tile and by job, the tiles
    /// refused.
    kept: Kept,
    reading: HashMap<TileId, JobId>,
    tile_jobs: HashMap<JobId, TileId>,
    refused: HashSet<TileId>,
    /// The files of the buildings kept, each building's file and the ground it covers, and the
    /// reads of files running, with the ranges the arenas had given back when they started.
    files: HashMap<FileRef, FileEntry>,
    building_files: HashMap<u32, FileRef>,
    grounds: HashMap<u32, [[f32; 2]; 2]>,
    file_jobs: HashMap<JobId, (FileRef, u64)>,
    /// The buildings whose doodads were told wanted, the jobs placing or taking them away, and the
    /// flags of the doodads placed, by building.
    told: HashSet<u32>,
    doodad_jobs: HashSet<JobId>,
    parts: HashMap<u32, Arc<[doodads::Part]>>,
    /// The liquids of the buildings drawn, placed through the service `liquids`: by building, the
    /// flag of each with its group.
    poured: HashMap<u32, Poured>,
    refusals: VecDeque<String>,
    /// Whether the buildings drawn changed since the scene was last given them.
    changed: bool,
    told_budget: Option<Demand>,
    /// The last frame signal seen, and the camera then.
    frame: u64,
    eye: [f32; 2],
    /// When the map or the distance changed, until the tiles wanted are all read and their files
    /// loaded; then what that took and how many tiles they are.
    since: Option<Instant>,
    took: Option<(Duration, usize)>,
}

impl BuildingsModule {
    fn refuse(&mut self, what: String) {
        log::warn!("{what}");
        self.refusals.push_back(what);
        if self.refusals.len() > REFUSALS {
            self.refusals.pop_front();
        }
    }

    /// Shows the buildings of the map `map`, by its folder, or of none: those of the map before
    /// taken away.
    fn show(&mut self, map: Option<String>, models: Option<&dyn models::Models>, ctx: &mut Context) {
        for job in self.reading.values().chain(self.file_jobs.keys()) {
            ctx.cancel(*job);
        }
        self.reading.clear();
        self.tile_jobs.clear();
        self.file_jobs.clear();
        self.files.clear();
        self.building_files.clear();
        self.grounds.clear();
        self.kept.clear();
        self.refused.clear();
        self.refusals.clear();
        self.told.clear();
        self.parts.clear();
        self.wdt = None;
        self.reading_wdt = None;
        self.took = None;
        self.since = map.is_some().then(Instant::now);
        self.changed = true;
        if let Some(models) = models {
            lock(&self.owners).want(models, map.as_deref().unwrap_or_default(), HashSet::new());
        }
        self.map = map;
    }

    /// Keeps the building `id` the tiles brought: its file wanted, read when the budget allows it.
    fn bring(&mut self, id: u32) {
        let Some(building) = self.kept.get(id) else {
            return;
        };
        let file = building.file.clone();
        self.grounds.insert(id, budget::ground(building));
        self.building_files.insert(id, file.clone());
        self.files
            .entry(file)
            .or_insert_with(|| FileEntry {
                state: FileState::Waiting,
                users: HashSet::new(),
            })
            .users
            .insert(id);
        self.changed = true;
    }

    /// Reads the file `file` by a job and puts it on the GPU.
    fn load(&mut self, file: FileRef, formats: &Arc<dyn formats::Formats>, ctx: &mut Context) {
        let given = self.shared.as_ref().map_or(0, |shared| shared.given());
        let (shared, formats, read) = (self.shared.clone(), formats.clone(), file.clone());
        let job = ctx.spawn(&format!("Read the building {read:?}"), move |_| -> Loaded {
            let shared = shared.ok_or("no device to draw on")?;
            let wmo = formats.wmo(&read)?;
            let (sets, doodads) = (wmo.doodad_sets.clone(), wmo.doodads.clone());
            let holders = doodads::holders(&wmo.groups, doodads.len());
            let liquids = liquid::liquids(&wmo);
            if !wmo.faults.is_empty() {
                log::warn!("{read:?}: {} faults, the first {}", wmo.faults.len(), wmo.faults[0]);
            }
            let gpu = gpu::upload(&shared, &*formats, wmo)?;
            Ok(WmoFile {
                gpu: Arc::new(gpu),
                sets,
                doodads,
                holders,
                liquids,
            })
        });
        self.file_jobs.insert(job, (file.clone(), given));
        if let Some(entry) = self.files.get_mut(&file) {
            entry.state = FileState::Loading(job);
        }
    }

    /// The files as the budget sees them from `eye`: each by the distance of its nearest building;
    /// one refused for want of room waiting again once room may have been made, the arenas having
    /// given back `given` ranges.
    fn budgeted(&self, eye: [f32; 2], given: u64) -> Vec<budget::File<FileRef>> {
        self.files
            .iter()
            .map(|(key, entry)| budget::File {
                key: key.clone(),
                distance: entry
                    .users
                    .iter()
                    .filter_map(|id| self.grounds.get(id))
                    .map(|ground| budget::distance(eye, *ground))
                    .fold(f32::INFINITY, f32::min),
                held: held(&entry.state, given, eye),
                arenas: match &entry.state {
                    FileState::Ready(read) => Some(read.gpu.arenas()),
                    _ => None,
                },
            })
            .collect()
    }

    /// Lets the building `id` go: its file, when no other building kept has it, dropped.
    fn take_away(&mut self, id: u32, ctx: &mut Context) {
        self.grounds.remove(&id);
        let Some(file) = self.building_files.remove(&id) else {
            return;
        };
        if let Some(entry) = self.files.get_mut(&file) {
            entry.users.remove(&id);
            if entry.users.is_empty() {
                if let FileState::Loading(job) = entry.state {
                    ctx.cancel(job);
                }
                self.files.remove(&file);
            }
        }
        self.changed = true;
    }

    /// The buildings kept whose file is ready, with it.
    fn ready(&self) -> Vec<(u32, Building, Arc<WmoFile>)> {
        self.building_files
            .iter()
            .filter_map(|(id, file)| match &self.files.get(file)?.state {
                FileState::Ready(read) => Some((*id, self.kept.get(*id)?.clone(), read.clone())),
                _ => None,
            })
            .collect()
    }

    /// At each frame: follows the map the terrain shows; then, at each frame signal, reads the
    /// tiles the camera wants, the nearest first, lets go those it left, gives the layer the
    /// buildings whose files are ready, and has their doodads placed.
    fn steer(&mut self, ctx: &mut Context) {
        let start = Instant::now();
        self.steer_tiles(ctx);
        let mut scene = lock(&self.scene);
        if scene.liquids.is_none() {
            scene.liquids = ctx.service(liquids::SERVICE);
        }
        scene.steering = start.elapsed();
    }

    fn steer_tiles(&mut self, ctx: &mut Context) {
        let (Some(view), Some(formats)) = (ctx.service(viewport::SERVICE), ctx.service(formats::SERVICE)) else {
            return;
        };
        if self.shared.is_none() {
            return;
        }
        let models = ctx.service(models::SERVICE);
        let map = ctx
            .editor()
            .call("terrain.map", json!({}))
            .ok()
            .and_then(|map| Some(map.get("directory")?.as_str()?.to_owned()));
        let liquids = ctx.service(liquids::SERVICE);
        if map != self.map {
            if let (Some(liquids), Some(directory)) = (&liquids, &self.map) {
                for id in self.poured.keys() {
                    liquids.clear(&owner(directory, *id));
                }
            }
            self.poured.clear();
            self.show(map, models.as_deref(), ctx);
        }
        let Some(directory) = self.map.clone() else {
            return;
        };
        let wdt = match &self.wdt {
            Some(Ok(wdt)) => wdt.clone(),
            Some(Err(_)) => return,
            None => {
                if self.reading_wdt.is_none() {
                    let (formats, read) = (formats.clone(), directory.clone());
                    self.reading_wdt = Some(
                        ctx.spawn(&format!("Read the WDT of {directory} for its buildings"), move |_| {
                            formats.wdt(&read)
                        }),
                    );
                }
                return;
            }
        };
        let Some(frame) = view.wait_frame(self.frame, Duration::ZERO) else {
            return;
        };
        self.frame = frame.number;
        let eye = match ctx.read_property("viewport/camera_position") {
            Ok(PropertyValue::Vector([x, y, _])) => [x as f32, y as f32],
            _ => return,
        };
        self.eye = eye;

        // The tiles: those left let go, the nearest wanted read.
        let wanted = formats::tiles_around(&wdt.tiles, eye, self.distance, &self.kept.tiles());
        let wanted_set: HashSet<TileId> = wanted.iter().copied().collect();
        let left: Vec<TileId> = self
            .kept
            .tiles()
            .into_iter()
            .chain(self.reading.keys().copied())
            .filter(|tile| !wanted_set.contains(tile))
            .collect();
        for tile in left {
            if let Some(job) = self.reading.remove(&tile) {
                ctx.cancel(job);
            }
            for id in self.kept.release(tile) {
                self.take_away(id, ctx);
            }
        }
        let slots = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
            .max(1);
        for tile in &wanted {
            if self.reading.len() >= slots {
                break;
            }
            if self.kept.holds(*tile) || self.reading.contains_key(tile) || self.refused.contains(tile) {
                continue;
            }
            let (formats, read, tile) = (formats.clone(), directory.clone(), *tile);
            let label = format!("Read the buildings of the tile {} {} of {directory}", tile.x, tile.y);
            let job = ctx.spawn(&label, move |_| -> Read {
                Ok(formats
                    .placements(&read, tile.x, tile.y)?
                    .ok_or("named by its WDT, but not read")?
                    .buildings)
            });
            self.reading.insert(tile, job);
            self.tile_jobs.insert(job, tile);
        }

        // The files: told to the budget by the distance of their nearest building, read within the
        // reach it allows to load and the room of the arenas, let go beyond the reach it allows to
        // keep or past the room of the arenas.
        let Some(shared) = self.shared.clone() else {
            return;
        };
        let files = self.budgeted(eye, shared.given());
        let room = [
            budget::room(&files, shared.most(), budget::LOAD_SHARE),
            budget::room(&files, shared.most(), 1.0),
        ];
        let demand = budget::demand(&files, shared.arrays.bytes(), budget::expected(&files), room[0]);
        let allowance = if self.told_budget.as_ref() == Some(&demand) {
            view.allowance()
        } else {
            self.told_budget = Some(demand.clone());
            view.tell_budget(ctx.module_id(), demand)
        };
        let plan = budget::plan(&files, &allowance, room, slots);
        for file in plan.start {
            self.load(file, &formats, ctx);
        }
        for file in plan.release {
            if let Some(entry) = self.files.get_mut(&file) {
                entry.state = FileState::Waiting;
                self.changed = true;
            }
        }

        // The buildings drawn, given to the layer when they change.
        if self.changed {
            self.changed = false;
            // The flags of the doodads placed, as the jobs placing them left them.
            if let Ok(owners) = self.owners.try_lock() {
                self.parts = owners.parts();
            }
            if let Some(liquids) = &liquids {
                self.pour(&directory, &**liquids);
            }
            let placed: Vec<Placed> = self
                .ready()
                .into_iter()
                .map(|(id, building, file)| Placed {
                    transform: formats::placement(building.position, building.rotation, building.scale),
                    wmo: file.gpu.clone(),
                    parts: self.parts.get(&id).cloned(),
                    liquids: self.poured.get(&id).cloned(),
                })
                .collect();
            let cpu = self
                .files
                .values()
                .filter_map(|entry| match &entry.state {
                    FileState::Ready(read) => Some(read.cpu()),
                    _ => None,
                })
                .sum();
            let mut scene = lock(&self.scene);
            scene.placed = placed;
            scene.cpu = cpu;
            drop(scene);
            self.steer_doodads(&directory, models.as_ref(), ctx);
        }

        let loading = self
            .files
            .values()
            .any(|entry| matches!(entry.state, FileState::Loading(_)));
        let done = self.reading.is_empty()
            && !loading
            && !plan.waiting
            && wanted
                .iter()
                .all(|tile| self.kept.holds(*tile) || self.refused.contains(tile));
        if done && let Some(since) = self.since.take() {
            let took = since.elapsed();
            self.took = Some((took, wanted.len()));
            log::info!(
                "{directory}: the {} buildings of the {} tiles within {} tiles drawn in {:.1} s",
                self.kept.len(),
                wanted.len(),
                self.distance,
                took.as_secs_f32()
            );
        }
    }

    /// Places the liquids of the buildings drawn of the map `directory` in the world through
    /// `service`, those of the buildings let go taken away.
    fn pour(&mut self, directory: &str, service: &dyn liquids::Liquids) {
        let ready = self.ready();
        let drawn: HashSet<u32> = ready.iter().map(|(id, _, _)| *id).collect();
        self.poured.retain(|id, _| {
            let kept = drawn.contains(id);
            if !kept {
                service.clear(&owner(directory, *id));
            }
            kept
        });
        for (id, building, file) in ready {
            if file.liquids.is_empty() || self.poured.contains_key(&id) {
                continue;
            }
            let transform = formats::placement(building.position, building.rotation, building.scale);
            let placed = file
                .liquids
                .iter()
                .map(|liquid| liquids::Placed {
                    liquid: liquid.liquid,
                    positions: liquid
                        .positions
                        .iter()
                        .map(|position| transform.transform_point3(Vec3::from(*position)).to_array())
                        .collect(),
                    coordinates: liquid.coordinates.clone(),
                    depths: liquid.depths.clone(),
                    triangles: liquid.triangles.clone(),
                })
                .collect();
            let flags = service.place(&owner(directory, id), placed);
            let groups = file.liquids.iter().map(|liquid| liquid.group).zip(flags).collect();
            self.poured.insert(id, groups);
        }
    }

    /// Has the doodads of the buildings drawn placed, those of the buildings let go taken away.
    fn steer_doodads(&mut self, directory: &str, models: Option<&models::Handle>, ctx: &mut Context) {
        let Some(models) = models else {
            return;
        };
        let ready = self.ready();
        let wanted: HashSet<u32> = ready.iter().map(|(id, _, _)| *id).collect();
        if wanted == self.told {
            return;
        }
        // A job placing doodads holds the lock: told at a change to come.
        let Ok(mut owners) = self.owners.try_lock() else {
            self.changed = true;
            return;
        };
        let leaving = owners.want(&**models, directory, wanted.clone());
        drop(owners);
        if leaving {
            let (owners, models) = (self.owners.clone(), models.clone());
            let job = ctx.spawn("Take away the doodads of the buildings left", move |_| {
                lock(&owners).settle(&*models)
            });
            self.doodad_jobs.insert(job);
        }
        for (id, building, file) in ready {
            if self.told.contains(&id) {
                continue;
            }
            let (owners, models, map) = (self.owners.clone(), models.clone(), directory.to_owned());
            let transform = formats::placement(building.position, building.rotation, building.scale);
            let named = building.doodad_set;
            let job = ctx.spawn(&format!("Place the doodads of the building {id}"), move |_| {
                let parts = doodads::instances(&*models, &transform, &file.sets, &file.doodads, &file.holders, named);
                lock(&owners).place(&*models, &map, id, &parts);
            });
            self.doodad_jobs.insert(job);
        }
        self.told = wanted;
    }
}

impl Module for BuildingsModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("buildings", "Buildings", DockArea::Right);
    }

    fn init(&mut self, ctx: &mut Context) {
        self.distance = ctx
            .setting(DISTANCE)
            .and_then(|value| value.as_u64())
            .map_or(DEFAULT_DISTANCE, |value| {
                value.clamp(u64::from(DISTANCES[0]), u64::from(DISTANCES[1])) as u32
            });
        let (Some(view), Some(gpu)) = (ctx.service(viewport::SERVICE), ctx.gpu().cloned()) else {
            log::info!("no 3D view: the buildings are not drawn");
            return;
        };
        match Shared::new(&gpu, &view.target()) {
            Ok(shared) => {
                let shared = Arc::new(shared);
                view.add_layer(
                    ctx.module_id(),
                    Box::new(BuildingsLayer::new(shared.clone(), self.scene.clone())),
                );
                self.shared = Some(shared);
            }
            Err(reason) => {
                log::warn!("{reason}");
                self.refused_device = Some(reason);
            }
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if let Some(reason) = &self.refused_device {
            ui.colored_label(ui.visuals().warn_fg_color, reason);
            return;
        }
        if self.shared.is_none() {
            ui.colored_label(ui.visuals().warn_fg_color, "No 3D view: the buildings are not drawn.");
            return;
        }
        let Some(map) = &self.map else {
            ui.label("No map shown by the terrain.");
            return;
        };
        if let Some(Err(reason)) = &self.wdt {
            ui.colored_label(ui.visuals().warn_fg_color, format!("{map}: {reason}"));
        }
        let (mut ready, mut loading, mut waiting, mut no_room, mut refused) = (0, 0, 0, 0, 0);
        for entry in self.files.values() {
            match entry.state {
                FileState::Ready(_) => ready += 1,
                FileState::Loading(_) => loading += 1,
                FileState::Waiting => waiting += 1,
                FileState::NoRoom(..) => no_room += 1,
                FileState::Refused => refused += 1,
            }
        }
        let doodads = lock(&self.owners).placed();
        let bytes = self.shared.as_ref().map_or(0, |shared| shared.bytes());
        ui.label(format!(
            "{map}: {} tiles held, {} reading; {} buildings, of {ready} files read, {loading} reading, {waiting} \
             waiting for the budget or their turn, {no_room} for room, {refused} refused; the doodads of {doodads} \
             placed; {:.0} MB on the GPU",
            self.kept.tiles().len(),
            self.reading.len(),
            self.kept.len(),
            bytes as f64 / (1024.0 * 1024.0)
        ));
        if let Some((took, tiles)) = self.took {
            ui.label(format!(
                "The buildings of all {tiles} tiles within reach drawn in {:.1} s",
                took.as_secs_f32()
            ));
        }
        ui.horizontal(|ui| {
            ui.label("Distance (tiles)");
            if ui
                .add(egui::DragValue::new(&mut self.distance).range(DISTANCES[0]..=DISTANCES[1]))
                .changed()
            {
                ctx.set_setting(DISTANCE, json!(self.distance));
                self.since = Some(Instant::now());
                self.took = None;
            }
        });
        for refusal in &self.refusals {
            ui.colored_label(ui.visuals().warn_fg_color, refusal);
        }
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown: the buildings are steered here.
        self.steer(ctx);
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if self.doodad_jobs.remove(&job) {
            if let JobOutcome::Panicked(message) = outcome {
                log::error!("the doodads of a building could not be placed: {message}");
            }
            // The flags of the doodads placed or taken away, for the layer.
            self.changed = true;
            return;
        }
        if self.reading_wdt == Some(job) {
            self.reading_wdt = None;
            self.wdt = Some(match outcome {
                JobOutcome::Panicked(message) => Err(message),
                outcome => match outcome.take::<Result<Arc<Wdt>, String>>() {
                    Some(read) => read,
                    None => return,
                },
            });
            if let Some(Err(reason)) = &self.wdt {
                log::warn!("the buildings of the map shown are not drawn: {reason}");
            }
            return;
        }
        if let Some(tile) = self.tile_jobs.remove(&job) {
            if self.reading.get(&tile) != Some(&job) {
                return;
            }
            self.reading.remove(&tile);
            let read = match outcome {
                JobOutcome::Panicked(message) => Err(message),
                JobOutcome::Cancelled => return,
                outcome => match outcome.take::<Read>() {
                    Some(read) => read,
                    None => return,
                },
            };
            match read {
                Ok(buildings) => {
                    let (brought, gone) = self.kept.hold(tile, buildings);
                    for id in gone {
                        self.take_away(id, ctx);
                    }
                    for id in brought {
                        self.bring(id);
                    }
                }
                Err(reason) => {
                    self.refused.insert(tile);
                    self.refuse(format!("Tile {} {}: {reason}", tile.x, tile.y));
                }
            }
            return;
        }
        let Some((file, given)) = self.file_jobs.remove(&job) else {
            return;
        };
        let Some(entry) = self.files.get_mut(&file) else {
            return;
        };
        if !matches!(entry.state, FileState::Loading(loading) if loading == job) {
            return;
        }
        let loaded = match outcome {
            JobOutcome::Panicked(message) => Err(Refusal::Failed(message)),
            JobOutcome::Cancelled => return,
            outcome => outcome
                .take::<Loaded>()
                .unwrap_or_else(|| Err(Refusal::Failed("no outcome".to_owned()))),
        };
        match loaded {
            Ok(read) => entry.state = FileState::Ready(Arc::new(read)),
            Err(Refusal::NoRoom(_)) => entry.state = FileState::NoRoom(given, self.eye),
            Err(Refusal::Failed(reason)) => {
                entry.state = FileState::Refused;
                self.refuse(format!("{file:?}: {reason}"));
            }
        }
        self.changed = true;
    }
}

uniwow_api::export_module!(BuildingsModule::default());
