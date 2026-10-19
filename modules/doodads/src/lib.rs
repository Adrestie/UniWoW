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
    /// The doodads whose textures changed, for `refresh` to make their instances again.
    retextured: HashSet<EntityId>,
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
    /// The job reading the highest unique id of the map shown.
    highest_job: Option<JobId>,
    /// The services the changes are shown through, once there.
    models: Option<models::Handle>,
    scene: Option<scene::Handle>,
    vfs: Option<Arc<dyn vfs::Vfs>>,
}

/// The ids of `arguments`, as text.
fn read_ids(arguments: &Value) -> Result<Vec<EntityId>, String> {
    arguments
        .get("ids")
        .and_then(Value::as_array)
        .ok_or("'ids' is a list of ids")?
        .iter()
        .map(|id| id.as_str().ok_or("'ids' is a list of ids as text")?.parse())
        .collect()
}

/// Doodads created: one undo entry.
struct Create {
    label: String,
    items: Vec<(EntityId, editing::Added)>,
}

impl Command for Create {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut DoodadsModule = module.downcast_mut().expect("the doodads");
        let mut listed = lock(&module.listed);
        for (id, added) in &self.items {
            listed.added.insert(*id, added.clone());
            listed.deleted.remove(id);
        }
        drop(listed);
        module.refresh(self.items.iter().map(|(id, _)| *id).collect(), Vec::new());
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut DoodadsModule = module.downcast_mut().expect("the doodads");
        let mut listed = lock(&module.listed);
        for (id, _) in &self.items {
            listed.added.remove(id);
            listed.moved.remove(id);
        }
        drop(listed);
        let tiles = self.items.iter().filter_map(|(_, added)| added.tile()).collect();
        module.refresh(self.items.iter().map(|(id, _)| *id).collect(), tiles);
    }
}

/// Doodads deleted, those the user had created with what they were: one undo entry.
struct Delete {
    label: String,
    items: Vec<(EntityId, Option<editing::Added>)>,
}

impl Command for Delete {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut DoodadsModule = module.downcast_mut().expect("the doodads");
        let mut listed = lock(&module.listed);
        let mut tiles = Vec::new();
        for (id, added) in &self.items {
            match added {
                Some(added) => {
                    tiles.extend(added.tile());
                    listed.added.remove(id);
                }
                None => {
                    listed.deleted.insert(*id);
                }
            }
        }
        drop(listed);
        module.refresh(self.items.iter().map(|(id, _)| *id).collect(), tiles);
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut DoodadsModule = module.downcast_mut().expect("the doodads");
        let mut listed = lock(&module.listed);
        for (id, added) in &self.items {
            match added {
                Some(added) => {
                    listed.added.insert(*id, added.clone());
                }
                None => {
                    listed.deleted.remove(id);
                }
            }
        }
        drop(listed);
        module.refresh(self.items.iter().map(|(id, _)| *id).collect(), Vec::new());
    }
}

/// The textures replaced of doodads, each from and to: one undo entry.
/// The textures replaced of a doodad, by their index; none for none.
type Textures = Option<std::collections::BTreeMap<u32, formats::FileRef>>;

struct Retexture {
    label: String,
    changes: Vec<(EntityId, Textures, Textures)>,
}

impl Retexture {
    fn set(&self, module: &mut DoodadsModule, after: bool) {
        let mut listed = lock(&module.listed);
        for (id, before, done) in &self.changes {
            match if after { done } else { before } {
                Some(textures) => listed.textures.insert(*id, textures.clone()),
                None => listed.textures.remove(id),
            };
        }
        drop(listed);
        let ids: Vec<EntityId> = self.changes.iter().map(|(id, _, _)| *id).collect();
        module.retextured.extend(ids.iter().copied());
        module.refresh(ids, Vec::new());
    }
}

impl Command for Retexture {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        self.set(module.downcast_mut().expect("the doodads"), true);
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        self.set(module.downcast_mut().expect("the doodads"), false);
    }
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
        {
            let mut listed = lock(&self.listed);
            for (id, spot) in &spots {
                listed.moved.insert(*id, *spot);
            }
        }
        self.refresh(spots.iter().map(|(id, _)| *id).collect(), Vec::new());
    }

    /// Shows the doodads `ids` as the changes have them: placed, moved, or taken away; the pieces of
    /// the tiles listing them, and of `tiles` besides, given to the scene again.
    fn refresh(&mut self, ids: Vec<EntityId>, tiles: Vec<TileId>) {
        let retextured = std::mem::take(&mut self.retextured);
        let listed = lock(&self.listed);
        let unique: HashSet<u32> = ids.iter().map(|id| id.key as u32).collect();
        let mut touched = listed.tiles_listing(&unique);
        touched.extend(tiles.into_iter().filter(|tile| listed.tiles.contains_key(tile)));
        touched.sort();
        touched.dedup();
        let tiles: Vec<(TileId, Arc<formats::Placements>)> = touched
            .into_iter()
            .filter_map(|tile| Some((tile, listed.tiles.get(&tile)?.clone())))
            .collect();
        let found: Vec<_> = ids
            .iter()
            .map(|id| (*id, listed.find(id), listed.added.contains_key(id)))
            .collect();
        let changes = listed.changes();
        drop(listed);
        let Some(directory) = self.map.clone() else {
            return;
        };
        let mut placing = lock(&self.placing);
        if let Some(models) = &self.models {
            for (id, found, added) in found
                .iter()
                .filter(|(id, _, _)| id.again().is_none() && id.map == self.map_id)
            {
                match found {
                    // Created, or its textures replaced: its instance made again.
                    Some((tile, doodad, spot))
                        if *added || changes.textures.contains_key(id) || retextured.contains(id) =>
                    {
                        let mut placed = doodad.clone();
                        placed.position = spot.position;
                        placed.rotation = spot.rotation;
                        placed.scale = spot.scale;
                        let instance = placing::instance(&placed, &**models, changes.replaced(id));
                        placing.put(&**models, &directory, *tile, instance);
                    }
                    Some((_, _, spot)) => placing.moved(&**models, &directory, id.key, spot.transform()),
                    None => placing.remove(&**models, &directory, id.key),
                }
            }
        }
        for (tile, placements) in tiles {
            let again = placing.differing(tile);
            let effective = editing::edited(&placements, self.map_id, tile, &again, &changes);
            lock(&self.picked).insert(tile, Arc::new(placing::placed(&effective)));
            if let Some(scene) = &self.scene {
                let vfs = self.vfs.clone();
                let path_of = |file| vfs.as_ref().and_then(|vfs| vfs.path_of(file));
                let piece = placing::piece(&effective, self.map_id, tile, &again, &path_of);
                scene.publish(PieceKey::tile(MODULE, self.map_id, tile), Arc::new(piece));
            }
        }
    }

    /// A unique id for a doodad created on the map shown.
    fn give(&self) -> Result<EntityId, String> {
        lock(&self.listed)
            .give(self.map_id)
            .map(|unique| EntityId::doodad(self.map_id, unique))
            .ok_or_else(|| "the unique ids of the map are still being read: a moment".to_owned())
    }

    /// `doodads.create`: a doodad of the model `file` at `position` in the world, one undoable
    /// command; its id.
    fn create(&mut self, arguments: &Value, ctx: &mut Context) -> Result<Value, String> {
        let file = match &arguments["file"] {
            Value::String(path) if !path.is_empty() => formats::FileRef::Path(path.clone()),
            Value::Number(id) => formats::FileRef::Id(id.as_u64().ok_or("'file' is a path or a FileDataID")? as u32),
            _ => return Err("'file' is a path or a FileDataID".to_owned()),
        };
        let vector = |key: &str, default: [f32; 3]| -> Result<[f32; 3], String> {
            match arguments.get(key) {
                None | Some(Value::Null) => Ok(default),
                Some(value) => {
                    let values = value.as_array().ok_or(format!("'{key}' is [x, y, z]"))?;
                    let number = |at: usize| values.get(at).and_then(Value::as_f64).map(|v| v as f32);
                    match (number(0), number(1), number(2)) {
                        (Some(x), Some(y), Some(z)) => Ok([x, y, z]),
                        _ => Err(format!("'{key}' is [x, y, z]")),
                    }
                }
            }
        };
        let world = vector("position", [f32::NAN; 3])?;
        if world.iter().any(|value| !value.is_finite()) {
            return Err("'position' is [x, y, z] in the world".to_owned());
        }
        let spot = Spot {
            position: scene::file_of_world(world),
            rotation: vector("rotation", [0.0; 3])?,
            scale: arguments["scale"].as_f64().unwrap_or(1.0) as f32,
        };
        let id = self.give()?;
        ctx.execute(Create {
            label: "Create a doodad".to_owned(),
            items: vec![(id, editing::Added { file, spot })],
        });
        Ok(json!({ "ids": [id.to_string()] }))
    }

    /// `doodads.duplicate`: the doodads `ids` again, the same at the same place, one undoable
    /// command; the new ids, in the order of `ids`.
    fn duplicate(&mut self, arguments: &Value, ctx: &mut Context) -> Result<Value, String> {
        let ids = read_ids(arguments)?;
        let mut items = Vec::new();
        for id in &ids {
            let Some((_, doodad, spot)) = lock(&self.listed).find(id) else {
                continue;
            };
            let new = self.give()?;
            items.push((
                new,
                editing::Added {
                    file: doodad.file,
                    spot,
                },
            ));
        }
        let new: Vec<String> = items.iter().map(|(id, _)| id.to_string()).collect();
        if !items.is_empty() {
            let label = match items.len() {
                1 => "Duplicate a doodad".to_owned(),
                n => format!("Duplicate {n} doodads"),
            };
            ctx.execute(Create { label, items });
        }
        Ok(json!({ "ids": new }))
    }

    /// `doodads.delete`: the doodads `ids` taken away, one undoable command.
    fn delete(&mut self, arguments: &Value, ctx: &mut Context) -> Result<Value, String> {
        let ids = read_ids(arguments)?;
        let listed = lock(&self.listed);
        let items: Vec<(EntityId, Option<editing::Added>)> = ids
            .iter()
            .filter(|id| listed.find(id).is_some())
            .map(|id| (*id, listed.added.get(id).cloned()))
            .collect();
        drop(listed);
        let deleted = items.len();
        if deleted > 0 {
            let label = match deleted {
                1 => "Delete a doodad".to_owned(),
                n => format!("Delete {n} doodads"),
            };
            ctx.execute(Delete { label, items });
        }
        Ok(json!({ "deleted": deleted }))
    }

    /// `doodads.write`: the field of the doodads named changed as asked, one undoable command.
    /// Replaces the texture 'texture' of the models of the doodads 'ids' by the file 'file', or gives
    /// it back without one: one undo entry.
    fn retexture(&mut self, arguments: &Value, ctx: &mut Context) -> Result<Value, String> {
        let ids: Vec<EntityId> = arguments["ids"]
            .as_array()
            .ok_or("'ids' is a list of ids")?
            .iter()
            .map(|id| id.as_str().ok_or("'ids' is a list of ids as text")?.parse())
            .collect::<Result<_, String>>()?;
        let index = arguments["texture"]
            .as_u64()
            .ok_or("'texture' is the index of a texture of the model")? as u32;
        let file = arguments["file"].as_str().map(str::trim).unwrap_or_default();
        let listed = lock(&self.listed);
        let changes: Vec<_> = ids
            .iter()
            .filter(|id| listed.find(id).is_some())
            .filter_map(|id| {
                let before = listed.textures.get(id).cloned();
                let mut after = before.clone().unwrap_or_default();
                if file.is_empty() {
                    after.remove(&index);
                } else {
                    after.insert(index, formats::FileRef::Path(file.to_owned()));
                }
                let after = (!after.is_empty()).then_some(after);
                (after != before).then_some((*id, before, after))
            })
            .collect();
        drop(listed);
        let changed = changes.len();
        if changed > 0 {
            let label = if file.is_empty() {
                "Give the texture back"
            } else {
                "Replace a texture"
            };
            ctx.execute(Retexture {
                label: label.to_owned(),
                changes,
            });
        }
        Ok(json!({ "changed": changed }))
    }

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
        // The highest unique id of the map, read once over all its tiles, before any is given.
        if self.highest_job.is_none() && !lock(&self.listed).highest.contains_key(&self.map_id) {
            let (formats, read, listed, map_id, tiles) = (
                formats.clone(),
                directory.clone(),
                self.listed.clone(),
                self.map_id,
                (0..4096u32)
                    .filter(|index| wdt.tiles[*index as usize])
                    .map(|index| TileId {
                        x: index % 64,
                        y: index / 64,
                    })
                    .collect::<Vec<_>>(),
            );
            self.highest_job = Some(
                ctx.spawn(&format!("Read the highest unique id of {directory}"), move |_| {
                    let highest = std::sync::atomic::AtomicU32::new(0);
                    uniwow_api::parallel_for(tiles.len(), 8, |range| {
                        for tile in &tiles[range] {
                            let Ok(Some(placements)) = formats.placements(&read, tile.x, tile.y) else {
                                continue;
                            };
                            let most = placements
                                .doodads
                                .iter()
                                .map(|doodad| doodad.unique_id)
                                .chain(placements.buildings.iter().map(|building| building.unique_id))
                                .max()
                                .unwrap_or(0);
                            highest.fetch_max(most, std::sync::atomic::Ordering::Relaxed);
                        }
                    });
                    lock(&listed)
                        .highest
                        .insert(map_id, highest.load(std::sync::atomic::Ordering::Relaxed));
                }),
            );
        }
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
                // The doodads as the user changed them.
                let changes = lock(&listed).changes();
                let effective = editing::edited(&read, map_id, tile, &HashSet::new(), &changes);
                let instances = placing::instances(&effective, &*models, |unique| {
                    changes.replaced(&EntityId::doodad(map_id, unique))
                });
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
                    let effective = editing::edited(&read, map_id, tile, &again, &changes);
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
                "doodads.create",
                "Creates a doodad of the model 'file' (a path in the archives, or a FileDataID) at 'position' in the \
                 world, turned by 'rotation' (degrees about the axes of the files), scaled by 'scale'; a unique id of \
                 the map above those its tiles hold; kept in memory, one undo entry. Its id.",
                json!({
                    "type": "object",
                    "properties": { "file": {}, "position": { "type": "array" }, "rotation": { "type": "array" }, "scale": { "type": "number" } },
                    "required": ["file", "position"]
                }),
                json!({ "type": "object", "properties": { "ids": { "type": "array" } } }),
            )
            .command(
                "doodads.duplicate",
                "Duplicates the doodads 'ids': new doodads of the same model at the same places, new unique ids; one \
                 undo entry. The new ids, in the order of 'ids'.",
                json!({ "type": "object", "properties": { "ids": { "type": "array" } }, "required": ["ids"] }),
                json!({ "type": "object", "properties": { "ids": { "type": "array" } } }),
            )
            .command(
                "doodads.delete",
                "Deletes the doodads 'ids', in memory; one undo entry.",
                json!({ "type": "object", "properties": { "ids": { "type": "array" } }, "required": ["ids"] }),
                json!({ "type": "object", "properties": { "deleted": { "type": "integer" } } }),
            )
            .command(
                "doodads.texture",
                "Replaces the texture 'texture' (its index in the model) of the doodads 'ids' by the file 'file' of the                  game, drawn so; without a file, gives the model's back. In memory, kept in the project; one undo entry.",
                json!({ "type": "object", "properties": { "ids": { "type": "array" }, "texture": { "type": "integer" }, "file": { "type": "string" } }, "required": ["ids", "texture"] }),
                json!({ "type": "object", "properties": { "changed": { "type": "integer" } } }),
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
        Some(lock(&self.listed).changes().section())
    }

    fn open_project(&mut self, section: Option<Value>, _ctx: &mut Context) {
        let opened = editing::Changes::of_section(&section.unwrap_or_default());
        // The changes before and those of the project shown as the project has them.
        let (mut ids, tiles) = {
            let mut listed = lock(&self.listed);
            let before = listed.changes();
            let tiles: Vec<TileId> = before.added.values().filter_map(editing::Added::tile).collect();
            listed.moved = opened.moved.clone();
            listed.added = opened.added.clone();
            listed.deleted = opened.deleted.clone();
            listed.given = opened.given.clone();
            listed.textures = opened.textures.clone();
            self.retextured
                .extend(before.textures.keys().chain(opened.textures.keys()).copied());
            (before.ids(), tiles)
        };
        ids.extend(opened.ids());
        ids.sort();
        ids.dedup();
        self.refresh(ids, tiles);
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        match name {
            "doodads.write" => self.write(&arguments, ctx),
            "doodads.create" => self.create(&arguments, ctx),
            "doodads.duplicate" => self.duplicate(&arguments, ctx),
            "doodads.delete" => self.delete(&arguments, ctx),
            "doodads.texture" => self.retexture(&arguments, ctx),
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
        if self.highest_job == Some(job) {
            self.highest_job = None;
            if let JobOutcome::Panicked(message) = outcome {
                log::error!("the highest unique id of the map was not read: {message}");
            }
            return;
        }
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
