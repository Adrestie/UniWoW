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
mod editing;
mod entities;
mod eye;
#[cfg(test)]
mod eye_tests;
mod gpu;
mod keeping;
mod layer;
mod liquid;
mod occlusion;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use uniwow_api::arena::{self, Refusal};
use uniwow_api::buildings::{self as service, Eye};
use uniwow_api::formats::{self, Building, DoodadSet, FileRef, TileId, Wdt, WmoDoodad};
use uniwow_api::glam::Vec3;
use uniwow_api::scene::{self, PieceKey};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::viewport::Demand;
use uniwow_api::{
    Context, DockArea, JobId, JobOutcome, Module, Priority, PropertyValue, Registrar, SettingSpec, egui, liquids, log,
    models, vfs, viewport,
};

use doodads::Owners;
use gpu::{Shared, WmoGpu};
use keeping::Kept;
use layer::{BuildingsLayer, Placed, Poured, Scene};

/// The setting of how far around the camera tiles are read, in tiles (`formats::distance_setting`).
const DISTANCE: &str = "distance";
/// The setting of the highlight of the specular and metal materials (`highlights_setting`).
const HIGHLIGHTS: &str = "highlights";

/// The highlight of the materials of the shaders specular (1) and metal (2), by the formula of later
/// clients, or none, as the client 3.3.5a may draw them: on by default.
fn highlights_setting() -> SettingSpec {
    SettingSpec::integer(
        HIGHLIGHTS,
        "Highlights of the specular and metal materials, by the formula of later clients (1) or none (0)",
        [0, 1],
        1,
    )
}
/// The refusals the panel keeps.
const REFUSALS: usize = 8;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A file of a building read: on the GPU, what its doodads need, and the liquids of its groups.
pub struct WmoFile {
    pub gpu: Arc<WmoGpu>,
    /// Its id for `WMOAreaTable.dbc`, and its groups as the scene lists them.
    pub id: u32,
    pub rooms: Vec<entities::Room>,
    pub sets: Vec<DoodadSet>,
    pub doodads: Vec<WmoDoodad>,
    /// The groups holding each doodad, whether each is lit as the rooms are, and the lights of the
    /// building on each (`doodads::lamps`).
    pub holders: Vec<Vec<u16>>,
    pub inside: Vec<bool>,
    pub lamps: Vec<Vec<doodads::Lamp>>,
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
                    .sum::<usize>()
                + self.inside.len()
                + self
                    .lamps
                    .iter()
                    .map(|lit| size_of::<Vec<doodads::Lamp>>() + lit.len() * size_of::<doodads::Lamp>())
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

type Picks = Arc<Mutex<HashMap<u32, Arc<entities::Picked>>>>;

/// The placements of buildings changed, each from and to: one undo entry.
struct MoveBuildings {
    label: String,
    changes: Vec<(u32, Building, Building)>,
}

impl uniwow_api::Command for MoveBuildings {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut BuildingsModule = module.downcast_mut().expect("the buildings");
        module.set_buildings(self.changes.iter().map(|(id, _, to)| (*id, to.clone())).collect());
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut BuildingsModule = module.downcast_mut().expect("the buildings");
        module.set_buildings(self.changes.iter().map(|(id, from, _)| (*id, from.clone())).collect());
    }
}

/// The bytes of the triangles of the buildings and their doodads the picking keeps.
const MESHES: usize = 96 << 20;

/// The picking of the buildings: the ray against the triangles of their groups, or of a group for a
/// room, and of the models of their doodads.
struct Walls {
    picked: Picks,
    meshes: scene::meshes::MeshCache,
}

impl scene::Picker for Walls {
    fn intersect(&self, entity: &scene::EntityRef<'_>, ray: &uniwow_api::viewport::Ray) -> scene::Picked {
        let id = entity.id();
        let Some(building) = lock(&self.picked).get(&(id.key as u32)).cloned() else {
            return scene::Picked::ByBox;
        };
        let nearest = |meshes: &[scene::meshes::Mesh], ray: &uniwow_api::viewport::Ray| {
            meshes
                .iter()
                .filter_map(|mesh| mesh.hits(ray))
                .min_by(f32::total_cmp)
                .map_or(scene::Picked::Missed, scene::Picked::Hit)
        };
        match id.kind {
            scene::Kind::Building | scene::Kind::Room => {
                let Some(groups) = self.meshes.wmo(&building.file) else {
                    return scene::Picked::ByBox;
                };
                let local = ray.into_model(&building.transform);
                match id.kind {
                    scene::Kind::Room => groups.get(id.sub as usize).map_or(scene::Picked::ByBox, |group| {
                        nearest(std::slice::from_ref(group), &local)
                    }),
                    _ => nearest(&groups, &local),
                }
            }
            scene::Kind::BuildingDoodad => {
                let Some((file, transform)) = building.doodads.get(&id.sub) else {
                    return scene::Picked::ByBox;
                };
                match self.meshes.model(file) {
                    Some(meshes) if meshes.iter().any(|mesh| !mesh.triangles.is_empty()) => {
                        nearest(&meshes, &ray.into_model(transform))
                    }
                    _ => scene::Picked::ByBox,
                }
            }
            _ => scene::Picked::ByBox,
        }
    }
}

/// The service `buildings`: where the eye stood among the buildings, as the layer left it in the
/// scene.
struct Seen(Arc<Mutex<Scene>>);

impl service::Buildings for Seen {
    fn eye(&self) -> Eye {
        lock(&self.0).eye.clone()
    }
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
    /// Its id in `Map.dbc`, for the scene.
    map_id: u32,
    /// The names of the rooms of `WMOAreaTable.dbc`, read by the first job that needs them.
    room_names: Arc<OnceLock<entities::RoomNames>>,
    /// The buildings the user moved or turned, kept in memory, by their unique id: their position
    /// and rotation in the axes of the files.
    moved: HashMap<u32, ([f32; 3], [f32; 3])>,
    /// The buildings the user gave another doodad set, by their unique id.
    sets: HashMap<u32, u16>,
    /// What the picking needs of each building kept, by its unique id.
    picked: Picks,
    /// The buildings moved whose liquids are to be poured again.
    repour: HashSet<u32>,
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
    /// The buildings whose doodads are placed, as the panel last read it.
    doodads_placed: usize,
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
        lock(&self.scene).eye = Eye::default();
        if let Some(models) = models {
            lock(&self.owners).want(models, map.as_deref().unwrap_or_default(), HashSet::new());
        }
        if let Some(scene) = ctx.service(scene::SERVICE) {
            scene.withdraw_provider(ctx.module_id(), None);
        }
        lock(&self.picked).clear();
        self.map = map;
    }

    /// The building `id` kept, as the user changed it: its bounds following its position.
    fn building(&self, id: u32) -> Option<Building> {
        let mut building = self.kept.get(id).cloned()?;
        if let Some((position, rotation)) = self.moved.get(&id) {
            for corner in &mut building.bounds {
                for ((value, to), from) in corner.iter_mut().zip(position).zip(building.position) {
                    *value += to - from;
                }
            }
            building.position = *position;
            building.rotation = *rotation;
        }
        if let Some(set) = self.sets.get(&id) {
            building.doodad_set = *set;
        }
        Some(building)
    }

    /// Places the buildings `changed` as given: kept in memory; drawn there from the next frame, their
    /// doodads and their liquids placed again, their entities given to the scene again.
    fn set_buildings(&mut self, changed: Vec<(u32, Building)>) {
        for (id, building) in changed {
            let original = self.kept.get(id).map(|kept| kept.doodad_set);
            if original == Some(building.doodad_set) {
                self.sets.remove(&id);
            } else {
                self.sets.insert(id, building.doodad_set);
            }
            self.place_as(id, building.position, building.rotation);
        }
        self.changed = true;
    }

    /// Places the building `id` at `position`, turned by `rotation`, in the axes of the files: kept in
    /// memory unless it is where its file has it.
    fn place_as(&mut self, id: u32, position: [f32; 3], rotation: [f32; 3]) {
        let original = self
            .kept
            .get(id)
            .is_some_and(|kept| kept.position == position && kept.rotation == rotation);
        if original {
            self.moved.remove(&id);
        } else {
            self.moved.insert(id, (position, rotation));
        }
        if let Some(building) = self.building(id) {
            self.grounds.insert(id, budget::ground(&building));
            lock(&self.picked).insert(id, Arc::new(entities::picked(&building, None)));
            // Their doodads placed again by their jobs, which give the scene their entities whole.
            self.told.remove(&id);
            self.repour.insert(id);
        }
        self.changed = true;
    }

    /// Keeps the building `id` the tiles brought: its file wanted, read when the budget allows it.
    fn bring(&mut self, id: u32, ctx: &mut Context) {
        let Some(building) = self.building(id) else {
            return;
        };
        let building = &building;
        if let Some(scene) = ctx.service(scene::SERVICE) {
            let vfs = ctx.service(vfs::SERVICE);
            let path_of = |file| vfs.as_ref().and_then(|vfs| vfs.path_of(file));
            lock(&self.picked).insert(id, Arc::new(entities::picked(building, None)));
            let piece = entities::bare(self.map_id, building, self.kept.tiles_of(id), &path_of);
            scene.publish(PieceKey::part(ctx.module_id(), self.map_id, id), Arc::new(piece));
        }
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

    /// Reads the file `file` by a job and puts it on the GPU, its nearest building `distance` yards
    /// from the camera.
    fn load(&mut self, file: FileRef, distance: f32, formats: &Arc<dyn formats::Formats>, ctx: &mut Context) {
        let given = self.shared.as_ref().map_or(0, |shared| shared.given());
        let (shared, formats, read) = (self.shared.clone(), formats.clone(), file.clone());
        let label = format!("Read the building {read:?}");
        let job = ctx.spawn_with_priority(&label, Priority::distance(distance), move |_| -> Loaded {
            let shared = shared.ok_or("no device to draw on")?;
            let wmo = formats.wmo(&read)?;
            let (sets, doodads) = (wmo.doodad_sets.clone(), wmo.doodads.clone());
            let holders = doodads::holders(&wmo.groups, doodads.len());
            let inside = doodads::lit_inside(&wmo.groups, &holders);
            let lamps = doodads::lamps(&wmo.groups, &wmo.lights, &doodads, &holders);
            let liquids = liquid::liquids(&wmo);
            let (id, rooms) = (wmo.id, entities::rooms(&wmo));
            if !wmo.faults.is_empty() {
                log::warn!("{read:?}: {} faults, the first {}", wmo.faults.len(), wmo.faults[0]);
            }
            let gpu = gpu::upload(&shared, &*formats, wmo)?;
            Ok(WmoFile {
                gpu: Arc::new(gpu),
                id,
                rooms,
                sets,
                doodads,
                holders,
                inside,
                lamps,
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
        if let Some(scene) = ctx.service(scene::SERVICE) {
            scene.withdraw(PieceKey::part(ctx.module_id(), self.map_id, id));
        }
        lock(&self.picked).remove(&id);
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
                FileState::Ready(read) => Some((*id, self.building(*id)?, read.clone())),
                _ => None,
            })
            .collect()
    }

    /// At each frame: follows the map the terrain shows; then, at each frame signal, reads the
    /// tiles the camera wants, the nearest first, lets go those it left, gives the layer the
    /// buildings whose files are ready, and has their doodads placed.
    fn steer(&mut self, ctx: &mut Context) {
        let start = Instant::now();
        let distance = formats::distance_setting(DISTANCE).value(ctx.setting(DISTANCE).as_ref()) as u32;
        if distance != self.distance {
            self.distance = distance;
            self.since = Some(Instant::now());
            self.took = None;
        }
        self.steer_tiles(ctx);
        let highlights = highlights_setting().value(ctx.setting(HIGHLIGHTS).as_ref()) == 1;
        let mut scene = lock(&self.scene);
        if scene.liquids.is_none() {
            scene.liquids = ctx.service(liquids::SERVICE);
        }
        scene.highlights = highlights;
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
        let shown = ctx.editor().call("terrain.map", json!({})).ok();
        let map = shown
            .as_ref()
            .and_then(|map| Some(map.get("directory")?.as_str()?.to_owned()));
        let liquids = ctx.service(liquids::SERVICE);
        if map != self.map {
            let id = shown.as_ref().and_then(|map| map.get("id")?.as_u64());
            if let (Some(liquids), Some(directory)) = (&liquids, &self.map) {
                for id in self.poured.keys() {
                    liquids.clear(&owner(directory, *id));
                }
            }
            self.poured.clear();
            self.show(map, models.as_deref(), ctx);
            self.map_id = id.unwrap_or_default() as u32;
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
            let job = ctx.spawn_with_priority(&label, Priority::distance(tile.nearest(eye)), move |_| -> Read {
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
            let distance = files
                .iter()
                .find(|budgeted| budgeted.key == file)
                .map_or(f32::INFINITY, |budgeted| budgeted.distance);
            self.load(file, distance, &formats, ctx);
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
            let placed: Arc<[Placed]> = placed.into();
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
        for id in std::mem::take(&mut self.repour) {
            if self.poured.remove(&id).is_some() {
                service.clear(&owner(directory, id));
            }
        }
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
                    room: liquid.room,
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
        let (scene, vfs, formats) = (
            ctx.service(scene::SERVICE),
            ctx.service(vfs::SERVICE),
            ctx.service(formats::SERVICE),
        );
        for (id, building, file) in ready {
            if self.told.contains(&id) {
                continue;
            }
            let (owners, models, map) = (self.owners.clone(), models.clone(), directory.to_owned());
            let (scene, vfs, formats, names) = (scene.clone(), vfs.clone(), formats.clone(), self.room_names.clone());
            let picks = self.picked.clone();
            let (key, tiles, map_id) = (
                PieceKey::part(ctx.module_id(), self.map_id, id),
                self.kept.tiles_of(id),
                self.map_id,
            );
            let placed = building.clone();
            let transform = formats::placement(building.position, building.rotation, building.scale);
            let named = building.doodad_set;
            let distance = budget::distance(self.eye, budget::ground(&building));
            let label = format!("Place the doodads of the building {id}");
            let job = ctx.spawn_with_priority(&label, Priority::distance(distance), move |_| {
                let parts = doodads::instances(
                    &*models,
                    &transform,
                    &file.sets,
                    &file.doodads,
                    &file.holders,
                    &file.inside,
                    &file.lamps,
                    named,
                );
                let placing = lock(&owners).place(&*models, &map, id, &parts);
                if placing && let Some(scene) = scene {
                    let names = names.get_or_init(|| {
                        formats
                            .and_then(|formats| formats.wmo_areas().ok())
                            .map(|rows| entities::room_names(&rows))
                            .unwrap_or_default()
                    });
                    let path_of = |file| vfs.as_ref().and_then(|vfs| vfs.path_of(file));
                    let piece = entities::whole(map_id, &placed, tiles, &file, names, &path_of);
                    lock(&picks).insert(id, Arc::new(entities::picked(&placed, Some(&file))));
                    scene.publish(key, Arc::new(piece));
                }
            });
            self.doodad_jobs.insert(job);
        }
        self.told = wanted;
    }
}

impl Module for BuildingsModule {
    fn register(&mut self, reg: &mut Registrar) {
        let seen: service::Handle = Arc::new(Seen(self.scene.clone()));
        reg.panel("buildings", "Buildings", DockArea::Right)
            .settings(
                "Buildings",
                vec![formats::distance_setting(DISTANCE), highlights_setting()],
            )
            .provide(service::SERVICE, seen)
            .command(
                "buildings.read",
                "The components of the buildings 'ids' held: Transform (position in the world, rotation in degrees \
                 about the axes of their file; no scale, which the client of 3.3.5a does not read), Model (its file, \
                 its doodad set and name set), and their raw data as their tile stores it.",
                json!({ "type": "object", "properties": { "ids": { "type": "array" } }, "required": ["ids"] }),
                json!({ "type": "object", "properties": { "entities": { "type": "array" } } }),
            )
            .command(
                "buildings.write",
                "Changes the 'field' (position or rotation) of the buildings 'ids', on the 'axis' 0, 1 or 2 or all of \
                 them: 'change' a value, or +n, -=n, *n, /n from each one's own; kept in memory, one undo entry. The \
                 position in the world.",
                json!({
                    "type": "object",
                    "properties": {
                        "ids": { "type": "array" },
                        "field": { "type": "string", "enum": ["position", "rotation"] },
                        "axis": { "type": "integer" },
                        "change": { "type": "string" }
                    },
                    "required": ["ids", "field", "change"]
                }),
                json!({ "type": "object", "properties": { "changed": { "type": "integer" }, "refused": { "type": "array" } } }),
            );
    }

    fn project_section(&self) -> Option<Value> {
        let mut moved: Vec<_> = self.moved.iter().collect();
        moved.sort_by_key(|(id, _)| **id);
        let moved: Vec<Value> = moved
            .into_iter()
            .map(|(id, (position, rotation))| json!({ "unique id": id, "position": position, "rotation": rotation }))
            .collect();
        let mut sets: Vec<_> = self.sets.iter().collect();
        sets.sort();
        let sets: Vec<Value> = sets
            .into_iter()
            .map(|(id, set)| json!({ "unique id": id, "doodad set": set }))
            .collect();
        Some(json!({ "moved": moved, "doodad sets": sets }))
    }

    fn open_project(&mut self, section: Option<Value>, _ctx: &mut Context) {
        let before: Vec<u32> = self.moved.keys().chain(self.sets.keys()).copied().collect();
        self.moved.clear();
        self.sets.clear();
        for entry in section
            .as_ref()
            .and_then(|section| section["doodad sets"].as_array())
            .cloned()
            .unwrap_or_default()
        {
            if let (Some(id), Some(set)) = (entry["unique id"].as_u64(), entry["doodad set"].as_u64()) {
                self.sets.insert(id as u32, set as u16);
            }
        }
        let mut placed: Vec<u32> = before;
        for entry in section
            .as_ref()
            .and_then(|section| section["moved"].as_array())
            .cloned()
            .unwrap_or_default()
        {
            let vector = |key: &str| -> Option<[f32; 3]> {
                let values = entry[key].as_array()?;
                Some([0, 1, 2].map(|at| values.get(at).and_then(Value::as_f64).unwrap_or(0.0) as f32))
            };
            let (Some(id), Some(position), Some(rotation)) =
                (entry["unique id"].as_u64(), vector("position"), vector("rotation"))
            else {
                continue;
            };
            self.moved.insert(id as u32, (position, rotation));
            placed.push(id as u32);
        }
        placed.extend(self.sets.keys().copied());
        // Those held, placed again where they are now.
        for id in placed {
            if let Some(building) = self.building(id) {
                self.place_as(id, building.position, building.rotation);
            }
        }
        self.changed = true;
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        let ids: Vec<scene::EntityId> = arguments
            .get("ids")
            .and_then(Value::as_array)
            .ok_or("'ids' is a list of ids")?
            .iter()
            .filter_map(|id| id.as_str()?.parse().ok())
            .filter(|id: &scene::EntityId| id.kind == scene::Kind::Building)
            .collect();
        match name {
            "buildings.read" => {
                let entities: Vec<Value> = ids
                    .iter()
                    .filter_map(|id| Some((id, self.building(id.key as u32)?)))
                    .map(|(id, building)| {
                        let key = id.key as u32;
                        editing::read(
                            id,
                            &building,
                            self.moved.contains_key(&key) || self.sets.contains_key(&key),
                        )
                    })
                    .collect();
                Ok(json!({ "entities": entities }))
            }
            "buildings.write" => {
                let field = arguments
                    .get("field")
                    .and_then(Value::as_str)
                    .ok_or("'field' names a field")?;
                let axis = arguments.get("axis").and_then(Value::as_u64).map(|axis| axis as usize);
                let change = arguments
                    .get("change")
                    .and_then(Value::as_str)
                    .ok_or("'change' is a value")?;
                let mut changes = Vec::new();
                let mut refused = Vec::new();
                for id in &ids {
                    let Some(building) = self.building(id.key as u32) else {
                        refused.push(format!("{id}: not held"));
                        continue;
                    };
                    match editing::written(&building, field, axis, change) {
                        Ok(to) if to != building => changes.push((id.key as u32, building, to)),
                        Ok(_) => {}
                        Err(reason) => refused.push(format!("{id}: {reason}")),
                    }
                }
                let changed = changes.len();
                if changed > 0 {
                    let label = match changed {
                        1 => format!("Set the {field} of a building"),
                        n => format!("Set the {field} of {n} buildings"),
                    };
                    ctx.execute(MoveBuildings { label, changes });
                }
                Ok(json!({ "changed": changed, "refused": refused }))
            }
            _ => Err(format!("'{name}' is not handled")),
        }
    }

    fn init(&mut self, ctx: &mut Context) {
        if let (Some(scene), Some(formats)) = (ctx.service(scene::SERVICE), ctx.service(formats::SERVICE)) {
            let picker = Walls {
                picked: self.picked.clone(),
                meshes: scene::meshes::MeshCache::new(formats, MESHES),
            };
            scene.add_picker(ctx.module_id(), Arc::new(picker));
        }
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

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, _ctx: &mut Context) {
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
        // A job placing doodads holds the lock: the count of the frame before then.
        if let Ok(owners) = self.owners.try_lock() {
            self.doodads_placed = owners.placed();
        }
        let doodads = self.doodads_placed;
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
                        self.bring(id, ctx);
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
