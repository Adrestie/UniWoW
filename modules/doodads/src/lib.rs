//! The doodads of the tiles of the map the terrain shows, around the camera of the 3D view, within
//! a distance of their own: the placements of each tile read by a job, the nearest first, and its
//! doodads placed through the service `models`, an owner a tile. Their places changed by the
//! Inspector are kept in memory and undoable (`editing`); no file is written.

mod editing;
mod placing;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use uniwow_api::formats::{self, TileId, Wdt};
use uniwow_api::scene::EntityId;
use uniwow_api::scene::{self, PieceKey};
use uniwow_api::serde_json::Value;
use uniwow_api::serde_json::json;
use uniwow_api::{
    Command, Context, DockArea, JobId, JobOutcome, Module, Priority, PropertyValue, Registrar, egui, log, models, vfs,
    viewport,
};

use editing::{Listed, Spot};
use placing::Placing;

/// The id of the module, the provider of its pieces of the scene.
const MODULE: &str = "doodads";

/// The setting of how far around the camera tiles are placed, in tiles (`formats::distance_setting`).
const DISTANCE: &str = "distance";
/// The refusals of tiles the panel keeps.
const REFUSALS: usize = 8;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A read of the placements of a tile: whether it was placed, or why it could not be read.
type Read = Result<bool, String>;

#[derive(Default)]
struct DoodadsModule {
    distance: u32,
    placing: Arc<Mutex<Placing>>,
    /// The map the terrain shows, by its folder; its WDT once read, or why it could not be, and
    /// the job reading it.
    map: Option<String>,
    /// Its id in `Map.dbc`, for the scene.
    map_id: u32,
    wdt: Option<Result<Arc<Wdt>, String>>,
    reading_wdt: Option<JobId>,
    /// The tiles told wanted to `placing`, the reads running, by tile and by job, the tiles placed
    /// and those refused.
    told: HashSet<TileId>,
    reading: HashMap<TileId, JobId>,
    jobs: HashMap<JobId, TileId>,
    placed: HashSet<TileId>,
    refused: HashSet<TileId>,
    refusals: VecDeque<String>,
    /// The jobs taking away the tiles left.
    settling: HashSet<JobId>,
    /// The last frame signal seen.
    frame: u64,
    /// When the map or the distance changed, until the tiles wanted are all placed; then what that
    /// took and how many they are.
    since: Option<Instant>,
    took: Option<(Duration, usize)>,
    /// What `placing` counted last: the tiles placed, the doodads placed and those listed.
    counts: [usize; 3],
    /// The model and the transform of each doodad of the tiles placed, for the picking.
    picked: Picked,
    /// What the tiles placed list and the doodads changed, shared with the jobs and the reads.
    listed: Arc<Mutex<Listed>>,
    /// The services the changes are shown through, once there.
    models: Option<models::Handle>,
    scene: Option<scene::Handle>,
    vfs: Option<Arc<dyn vfs::Vfs>>,
}

/// The places of doodads changed, each from and to: one undo entry.
struct MoveDoodads {
    label: String,
    changes: Vec<(EntityId, Spot, Spot)>,
}

impl Command for MoveDoodads {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut DoodadsModule = module.downcast_mut().expect("the doodads");
        module.set_spots(self.changes.iter().map(|(id, _, to)| (*id, *to)).collect());
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut DoodadsModule = module.downcast_mut().expect("the doodads");
        module.set_spots(self.changes.iter().map(|(id, from, _)| (*id, *from)).collect());
    }
}

type Picked = Arc<Mutex<HashMap<TileId, Arc<placing::Placed>>>>;

/// The picking of the doodads: the ray against the triangles at rest of their model.
struct Models {
    placed: Picked,
    meshes: scene::meshes::MeshCache,
}

/// The bytes of the triangles of the models the picking keeps.
const MESHES: usize = 48 << 20;

impl scene::Picker for Models {
    fn intersect(&self, entity: &scene::EntityRef<'_>, ray: &uniwow_api::viewport::Ray) -> scene::Picked {
        let id = entity.id();
        let Some(tile) = entity.placed.piece.tiles.first() else {
            return scene::Picked::ByBox;
        };
        let Some((file, transform)) = lock(&self.placed)
            .get(tile)
            .and_then(|placed| placed.get(&id.key).cloned())
        else {
            return scene::Picked::ByBox;
        };
        match self.meshes.model(&file) {
            Some(meshes) if meshes.iter().any(|mesh| !mesh.triangles.is_empty()) => {
                let local = ray.into_model(&transform);
                meshes
                    .iter()
                    .filter_map(|mesh| mesh.hits(&local))
                    .min_by(f32::total_cmp)
                    .map_or(scene::Picked::Missed, scene::Picked::Hit)
            }
            _ => scene::Picked::ByBox,
        }
    }
}

impl DoodadsModule {
    /// Puts the doodads `spots` at their places: kept in memory, their instances moved, the pieces
    /// of the tiles listing them given to the scene again.
    fn set_spots(&mut self, spots: Vec<(EntityId, Spot)>) {
        let mut listed = lock(&self.listed);
        for (id, spot) in &spots {
            listed.moved.insert(*id, *spot);
        }
        let unique: HashSet<u32> = spots.iter().map(|(id, _)| id.key as u32).collect();
        let tiles: Vec<(TileId, Arc<formats::Placements>)> = listed
            .tiles_listing(&unique)
            .into_iter()
            .filter_map(|tile| Some((tile, listed.tiles.get(&tile)?.clone())))
            .collect();
        let moved = listed.moved.clone();
        drop(listed);
        let Some(directory) = self.map.clone() else {
            return;
        };
        let mut placing = lock(&self.placing);
        if let Some(models) = &self.models {
            for (id, spot) in spots.iter().filter(|(id, _)| id.again().is_none()) {
                placing.moved(&**models, &directory, id.key, spot.transform());
            }
        }
        for (tile, placements) in tiles {
            let again = placing.differing(tile);
            let effective = editing::moved(&placements, self.map_id, tile, &again, &moved);
            lock(&self.picked).insert(tile, Arc::new(placing::placed(&effective)));
            if let Some(scene) = &self.scene {
                let vfs = self.vfs.clone();
                let path_of = |file| vfs.as_ref().and_then(|vfs| vfs.path_of(file));
                let piece = placing::piece(&effective, self.map_id, tile, &again, &path_of);
                scene.publish(PieceKey::tile(MODULE, self.map_id, tile), Arc::new(piece));
            }
        }
    }

    /// `doodads.write`: the field of the doodads named changed as asked, one undoable command.
    fn write(&mut self, arguments: &Value, ctx: &mut Context) -> Result<Value, String> {
        let ids: Vec<EntityId> = arguments
            .get("ids")
            .and_then(Value::as_array)
            .ok_or("'ids' is a list of ids")?
            .iter()
            .map(|id| id.as_str().ok_or("'ids' is a list of ids as text")?.parse())
            .collect::<Result<_, String>>()?;
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
        {
            let listed = lock(&self.listed);
            for id in &ids {
                let Some((_, _, spot)) = listed.find(id) else {
                    refused.push(format!("{id}: not held"));
                    continue;
                };
                match editing::written(spot, field, axis, change) {
                    Ok(to) if to != spot => changes.push((*id, spot, to)),
                    Ok(_) => {}
                    Err(reason) => refused.push(format!("{id}: {reason}")),
                }
            }
        }
        let changed = changes.len();
        if changed > 0 {
            let label = match changed {
                1 => format!("Set the {field} of a doodad"),
                n => format!("Set the {field} of {n} doodads"),
            };
            ctx.execute(MoveDoodads { label, changes });
        }
        Ok(json!({ "changed": changed, "refused": refused }))
    }

    /// Shows the doodads of the map `map`, by its folder, or of none: what the map before held
    /// taken away.
    fn show(&mut self, map: Option<String>, models: &dyn models::Models, ctx: &mut Context) {
        for job in self.reading.values() {
            ctx.cancel(*job);
        }
        self.reading.clear();
        self.jobs.clear();
        self.told.clear();
        self.placed.clear();
        self.refused.clear();
        self.refusals.clear();
        self.wdt = None;
        self.reading_wdt = None;
        self.took = None;
        self.since = map.is_some().then(Instant::now);
        lock(&self.placing).want(models, map.as_deref().unwrap_or_default(), HashSet::new());
        if let Some(scene) = ctx.service(scene::SERVICE) {
            scene.withdraw_provider(ctx.module_id(), None);
        }
        lock(&self.picked).clear();
        lock(&self.listed).tiles.clear();
        self.map = map;
    }

    /// Records the refusal of the tile `tile`.
    fn refuse(&mut self, tile: TileId, reason: &str) {
        log::warn!("the doodads of the tile {} {} are not placed: {reason}", tile.x, tile.y);
        self.refused.insert(tile);
        self.refusals.push_back(format!("Tile {} {}: {reason}", tile.x, tile.y));
        if self.refusals.len() > REFUSALS {
            self.refusals.pop_front();
        }
    }

    /// At each frame: follows the map the terrain shows; then, at each frame signal, tells
    /// `placing` the tiles the camera wants, cancels the reads of those it left, has those placed
    /// taken away, and starts the reads of the nearest it wants, as many at once as the workers but
    /// one.
    fn steer(&mut self, ctx: &mut Context) {
        let distance = formats::distance_setting(DISTANCE).value(ctx.setting(DISTANCE).as_ref()) as u32;
        if distance != self.distance {
            self.distance = distance;
            self.since = Some(Instant::now());
            self.took = None;
        }
        let (Some(view), Some(models), Some(formats)) = (
            ctx.service(viewport::SERVICE),
            ctx.service(models::SERVICE),
            ctx.service(formats::SERVICE),
        ) else {
            return;
        };
        if self.models.is_none() {
            self.models = Some(models.clone());
            self.scene = ctx.service(scene::SERVICE);
            self.vfs = ctx.service(vfs::SERVICE);
        }
        let shown = ctx.editor().call("terrain.map", json!({})).ok();
        let map = shown
            .as_ref()
            .and_then(|map| Some(map.get("directory")?.as_str()?.to_owned()));
        if map != self.map {
            self.show(map, &*models, ctx);
            self.map_id = shown
                .as_ref()
                .and_then(|map| map.get("id")?.as_u64())
                .unwrap_or_default() as u32;
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
                        ctx.spawn(&format!("Read the WDT of {directory} for its doodads"), move |_| {
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
        let wanted = formats::tiles_around(&wdt.tiles, eye, self.distance, &self.placed);
        let set: HashSet<TileId> = wanted.iter().copied().collect();
        if set != self.told {
            // A job placing a tile holds the lock: told at a frame to come.
            let Ok(mut placing) = self.placing.try_lock() else {
                return;
            };
            let leaving = placing.want(&*models, &directory, set.clone());
            drop(placing);
            let scene = ctx.service(scene::SERVICE);
            for tile in self.told.difference(&set) {
                if let Some(job) = self.reading.remove(tile) {
                    ctx.cancel(job);
                }
                self.placed.remove(tile);
                if let Some(scene) = &scene {
                    scene.withdraw(PieceKey::tile(ctx.module_id(), self.map_id, *tile));
                }
                lock(&self.picked).remove(tile);
                lock(&self.listed).tiles.remove(tile);
            }
            if leaving {
                let (placing, models) = (self.placing.clone(), models.clone());
                let job = ctx.spawn("Take away the doodads of the tiles left", move |_| {
                    lock(&placing).settle(&*models)
                });
                self.settling.insert(job);
            }
            self.told = set;
        }
        let slots = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
            .max(1);
        for tile in &wanted {
            if self.reading.len() >= slots {
                break;
            }
            if self.placed.contains(tile) || self.reading.contains_key(tile) || self.refused.contains(tile) {
                continue;
            }
            let (formats, models, placing, directory) =
                (formats.clone(), models.clone(), self.placing.clone(), directory.clone());
            let (scene, vfs, provider, map_id, picked, listed) = (
                ctx.service(scene::SERVICE),
                ctx.service(vfs::SERVICE),
                ctx.module_id().to_owned(),
                self.map_id,
                self.picked.clone(),
                self.listed.clone(),
            );
            let tile = *tile;
            let label = format!("Place the doodads of the tile {} {} of {directory}", tile.x, tile.y);
            let priority = Priority::distance(tile.nearest(eye));
            let job = ctx.spawn_with_priority(&label, priority, move |job| -> Read {
                let read = formats
                    .placements(&directory, tile.x, tile.y)?
                    .ok_or("named by its WDT, but not read")?;
                if job.is_cancelled() {
                    return Ok(false);
                }
                // The doodads the user changed at their places as changed.
                let moved = lock(&listed).moved.clone();
                let effective = editing::moved(&read, map_id, tile, &HashSet::new(), &moved);
                let instances = placing::instances(&effective, &*models);
                if job.is_cancelled() {
                    return Ok(false);
                }
                let (placed, again) = {
                    let mut placing = lock(&placing);
                    let placed = placing.place(&*models, &directory, tile, instances);
                    let again = if placed {
                        placing.differing(tile)
                    } else {
                        HashSet::new()
                    };
                    (placed, again)
                };
                if placed {
                    lock(&listed).tiles.insert(tile, Arc::new(read.clone()));
                }
                if placed && let Some(scene) = scene {
                    let path_of = |file| vfs.as_ref().and_then(|vfs| vfs.path_of(file));
                    let effective = editing::moved(&read, map_id, tile, &again, &moved);
                    let piece = placing::piece(&effective, map_id, tile, &again, &path_of);
                    lock(&picked).insert(tile, Arc::new(placing::placed(&effective)));
                    scene.publish(PieceKey::tile(&provider, map_id, tile), Arc::new(piece));
                }
                Ok(placed)
            });
            self.reading.insert(tile, job);
            self.jobs.insert(job, tile);
        }
        let done = self.reading.is_empty()
            && wanted
                .iter()
                .all(|tile| self.placed.contains(tile) || self.refused.contains(tile));
        if done && let Some(since) = self.since.take() {
            let took = since.elapsed();
            self.took = Some((took, wanted.len()));
            log::info!(
                "{directory}: the doodads of the {} tiles within {} tiles placed in {:.1} s",
                wanted.len(),
                self.distance,
                took.as_secs_f32()
            );
        }
        if let Ok(placing) = self.placing.try_lock() {
            self.counts = placing.counts();
        }
    }
}

impl Module for DoodadsModule {
    fn register(&mut self, reg: &mut Registrar) {
        let listed = self.listed.clone();
        reg.panel("doodads", "Doodads", DockArea::Right)
            .settings("Doodads", vec![formats::distance_setting(DISTANCE)])
            .command_on_caller(
                "doodads.read",
                "The components of the doodads 'ids' held: Transform (position in the world, rotation in degrees \
                 about the axes of their file, scale), Model, and their raw data as their tile stores it.",
                json!({ "type": "object", "properties": { "ids": { "type": "array" } }, "required": ["ids"] }),
                json!({ "type": "object", "properties": { "entities": { "type": "array" } } }),
                Arc::new(move |arguments| {
                    let ids = arguments.get("ids").and_then(Value::as_array).cloned().unwrap_or_default();
                    let listed = lock(&listed);
                    let entities: Vec<Value> = ids
                        .iter()
                        .filter_map(|id| id.as_str()?.parse::<EntityId>().ok())
                        .filter_map(|id| editing::read(&listed, &id))
                        .collect();
                    Ok(json!({ "entities": entities }))
                }),
            )
            .command(
                "doodads.write",
                "Changes the 'field' (position, rotation or scale) of the doodads 'ids', on the 'axis' 0, 1 or 2 of a \
                 vector or all of them: 'change' a value, or +n, -=n, *n, /n from each one's own; kept in memory, \
                 one undo entry. The position in the world.",
                json!({
                    "type": "object",
                    "properties": {
                        "ids": { "type": "array" },
                        "field": { "type": "string", "enum": ["position", "rotation", "scale"] },
                        "axis": { "type": "integer" },
                        "change": { "type": "string" }
                    },
                    "required": ["ids", "field", "change"]
                }),
                json!({ "type": "object", "properties": { "changed": { "type": "integer" }, "refused": { "type": "array" } } }),
            );
    }

    fn project_section(&self) -> Option<Value> {
        let listed = lock(&self.listed);
        let mut moved: Vec<(&EntityId, &Spot)> = listed.moved.iter().collect();
        moved.sort_by_key(|(id, _)| **id);
        let moved: Vec<Value> = moved
            .into_iter()
            .map(|(id, spot)| {
                json!({ "id": id.to_string(), "position": spot.position, "rotation": spot.rotation, "scale": spot.scale })
            })
            .collect();
        Some(json!({ "moved": moved }))
    }

    fn open_project(&mut self, section: Option<Value>, _ctx: &mut Context) {
        let vector = |value: &Value| -> Option<[f32; 3]> {
            let values = value.as_array()?;
            Some([0, 1, 2].map(|at| values.get(at).and_then(Value::as_f64).unwrap_or(0.0) as f32))
        };
        let opened: HashMap<EntityId, Spot> = section
            .as_ref()
            .and_then(|section| section["moved"].as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| {
                let id: EntityId = entry["id"].as_str()?.parse().ok()?;
                let spot = Spot {
                    position: vector(&entry["position"])?,
                    rotation: vector(&entry["rotation"])?,
                    scale: entry["scale"].as_f64()? as f32,
                };
                Some((id, spot))
            })
            .collect();
        // Those moved before and not in the project put back where their tiles have them.
        let mut spots: Vec<(EntityId, Spot)> = {
            let mut listed = lock(&self.listed);
            let before: Vec<EntityId> = listed.moved.keys().copied().collect();
            listed.moved.clear();
            before
                .into_iter()
                .filter(|id| !opened.contains_key(id))
                .filter_map(|id| Some((id, listed.find(&id)?.2)))
                .collect()
        };
        spots.extend(opened.iter().map(|(id, spot)| (*id, *spot)));
        self.set_spots(spots);
        lock(&self.listed).moved = opened;
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        match name {
            "doodads.write" => self.write(&arguments, ctx),
            _ => Err(format!("'{name}' is not handled")),
        }
    }

    fn init(&mut self, ctx: &mut Context) {
        if let (Some(scene), Some(formats)) = (ctx.service(scene::SERVICE), ctx.service(formats::SERVICE)) {
            let picker = Models {
                placed: self.picked.clone(),
                meshes: scene::meshes::MeshCache::new(formats, MESHES),
            };
            scene.add_picker(ctx.module_id(), Arc::new(picker));
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if ctx.service(models::SERVICE).is_none() || ctx.service(viewport::SERVICE).is_none() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "No 3D view or no service models: the doodads are not drawn.",
            );
            return;
        }
        let Some(map) = &self.map else {
            ui.label("No map shown by the terrain.");
            return;
        };
        if let Some(Err(reason)) = &self.wdt {
            ui.colored_label(ui.visuals().warn_fg_color, format!("{map}: {reason}"));
        }
        let [tiles, placed, listed] = self.counts;
        ui.label(format!(
            "{map}: {tiles} tiles placed, {} reading, {} refused; {placed} doodads placed of {listed} listed, \
             those listed by several tiles placed once",
            self.reading.len(),
            self.refused.len()
        ));
        if let Some((took, tiles)) = self.took {
            ui.label(format!(
                "The doodads of all {tiles} tiles within reach placed in {:.1} s",
                took.as_secs_f32()
            ));
        }
        for refusal in &self.refusals {
            ui.colored_label(ui.visuals().warn_fg_color, refusal);
        }
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown: the tiles are steered here.
        self.steer(ctx);
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if self.settling.remove(&job) {
            if let JobOutcome::Panicked(message) = outcome {
                log::error!("the doodads of the tiles left could not be taken away: {message}");
            }
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
                log::warn!("the doodads of the map shown are not placed: {reason}");
            }
            return;
        }
        let Some(tile) = self.jobs.remove(&job) else {
            return;
        };
        if self.reading.get(&tile) != Some(&job) {
            return;
        }
        self.reading.remove(&tile);
        match outcome {
            JobOutcome::Panicked(message) => self.refuse(tile, &message),
            JobOutcome::Cancelled => {}
            outcome => match outcome.take::<Read>() {
                Some(Ok(true)) => {
                    self.placed.insert(tile);
                }
                Some(Err(reason)) => self.refuse(tile, &reason),
                _ => {}
            },
        }
    }
}

uniwow_api::export_module!(DoodadsModule::default());
