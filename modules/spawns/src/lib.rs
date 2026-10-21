//! The spawns of the world database (milestone 15, step 15.11b): the creatures and the game objects
//! of the map the terrain shows, read by a job through the service `world-db` once it is connected,
//! with their templates' names and looks; given to the scene by tile, drawn through `models` by
//! their looks; their places changed, created and deleted in memory, each one undo entry, kept in the
//! project. The quests their templates start and end read with them and changed in memory (step
//! 15.11c). Nothing is written to the database.

mod data;
mod paths;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use uniwow_api::formats::TileId;
use uniwow_api::glam::{Mat4, Quat, Vec3};
use uniwow_api::models::{self, Instance, LookId, Motion};
use uniwow_api::scene::{self, EntityId, Kind, PieceKey};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, State};
use uniwow_api::{Command, Context, DockArea, JobId, JobOutcome, Module, Registrar, egui, log};

use data::{Changes, Link, Quests, Spawn};

/// The id of the module, the provider of its pieces and the owner of its instances.
const MODULE: &str = "spawns";

/// What a reading job gives back: the spawns of a map, the highest guid of each table, the quests.
type Read = Result<(Vec<Spawn>, [u64; 3], Quests, paths::Paths, &'static str), String>;

/// The looks of the displays, by kind and display: a look and its scale, none for none.
type Looks = HashMap<(Kind, u32), Option<(LookId, f32)>>;

/// The changes made, from and to: one undo entry.
struct SetChanges {
    label: String,
    before: Changes,
    after: Changes,
}

impl Command for SetChanges {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut SpawnsModule = module.downcast_mut().expect("the spawns");
        module.set_changes(self.after.clone());
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut SpawnsModule = module.downcast_mut().expect("the spawns");
        module.set_changes(self.before.clone());
    }
}

#[derive(Default)]
struct SpawnsModule {
    /// The map the terrain shows, by its id; the map and the server the spawns were read for.
    map: Option<u32>,
    read_for: Option<(u32, String)>,
    reading: Option<JobId>,
    /// The spawns as the database has them, and the highest guid of each table.
    spawns: Arc<Vec<Spawn>>,
    /// The place of each spawn in `spawns`, by its id, and those of each tile, made once read.
    index: HashMap<EntityId, usize>,
    by_tile: HashMap<TileId, Vec<usize>>,
    /// Whether the looks of the spawns are to be looked at again, once they or the changes did.
    looks_wanted: bool,
    highest: [u64; 3],
    quests: Arc<Quests>,
    changes: Changes,
    looks: Looks,
    resolving: Option<JobId>,
    /// The tiles given to the scene and placed, for those to take away.
    tiles: BTreeSet<TileId>,
    refused: Option<String>,
    /// The folder of the map shown, its spawns' areas read from its tiles by `placing`, and the
    /// areas of each spawn, by its place in `spawns`, once read.
    directory: Option<String>,
    placing: Option<JobId>,
    areas: Arc<Vec<u32>>,
    /// The tiles around the camera whose spawns are given to the scene and drawn; none while no
    /// view tells where the camera is, no spawn then.
    around: Option<BTreeSet<TileId>>,
    /// Whether the spawns of game events are shown whether the events run or not.
    show_events: bool,
    /// The patrol paths of the spawns, those drawn, whether all are, and what they were drawn for.
    paths: Arc<paths::Paths>,
    drawn: Arc<std::sync::Mutex<paths::Drawn>>,
    all_paths: bool,
    paths_for: Option<(u64, bool, usize, u64)>,
    /// Counts the frames, the marks of the spawns that move drawn at each.
    frames: u64,
    /// The client's tables, for the displays of game objects.
    formats: Option<Arc<dyn uniwow_api::formats::Formats>>,
    /// The spawns the world as it runs shows live, not drawn again; the generation of the scene they
    /// were seen in.
    live: BTreeSet<EntityId>,
    seen: u64,
    models: Option<models::Handle>,
    scene: Option<scene::Handle>,
    /// The column of a creature's template in this database, once read; how many times the spawns
    /// or the changes changed, and when the changes for the server were given last.
    entry_column: Option<&'static str>,
    generation: u64,
    given: Option<u64>,
}

/// The creatures and game objects the world as it runs lists in `snapshot`, by the pieces of the
/// live world alone: a live creature or game object of a spawn is listed by its spawn's id.
fn live_of(snapshot: &scene::Snapshot) -> BTreeSet<EntityId> {
    let mut out = BTreeSet::new();
    for placed in snapshot.slots.iter().flatten() {
        let piece = &placed.piece;
        if piece.flags.first().is_none_or(|flags| flags & scene::flags::LIVE == 0) {
            continue;
        }
        for (row, id) in piece.ids.iter().enumerate() {
            if piece.flags[row] & scene::flags::LIVE != 0 && matches!(id.kind, Kind::Creature | Kind::GameObject) {
                out.insert(*id);
            }
        }
    }
    out
}

/// The setting of how far around the camera the spawns are given and drawn, in tiles.
const DISTANCE: &str = "distance";
const DEFAULT_DISTANCE: i64 = 3;

/// The places in `spawns` of the spawns of each tile.
fn by_tile(spawns: &[Spawn]) -> HashMap<TileId, Vec<usize>> {
    let mut out: HashMap<TileId, Vec<usize>> = HashMap::new();
    for (at, spawn) in spawns.iter().enumerate() {
        if let Some(tile) = data::tile_of(spawn.position) {
            out.entry(tile).or_default().push(at);
        }
    }
    out
}

/// The tiles within `distance` of the tile `centre`, on each side.
fn tiles_around(centre: TileId, distance: u32) -> BTreeSet<TileId> {
    let span = |at: u32| at.saturating_sub(distance)..=(at + distance).min(63);
    span(centre.x)
        .flat_map(|x| span(centre.y).map(move |y| TileId { x, y }))
        .collect()
}

fn owner(map: u32, tile: TileId) -> String {
    format!("{MODULE}/{map}/{}_{}", tile.x, tile.y)
}

impl SpawnsModule {
    /// The spawns as the user changed them given to the scene again and placed, by tile; the looks
    /// not yet resolved asked of a job.
    fn refresh(&mut self) {
        self.refresh_tiles(None);
    }

    /// The areas of the spawns of the map, read once from the areas of the chunks of their tiles
    /// by a job, for the Hierarchy to list them all.
    fn read_areas(&mut self, ctx: &mut Context) {
        let (Some(formats), Some(directory)) = (self.formats.clone(), self.directory.clone()) else {
            return;
        };
        let (spawns, by_tile) = (self.spawns.clone(), self.by_tile.clone());
        self.placing = Some(
            ctx.spawn("Read the areas of the spawns of the map", move |job| -> Vec<u32> {
                let mut areas = vec![0; spawns.len()];
                for (tile, ats) in &by_tile {
                    if job.is_cancelled() {
                        break;
                    }
                    let Ok(Some(chunks)) = formats.tile_areas(&directory, tile.x, tile.y) else {
                        continue;
                    };
                    for at in ats {
                        let [x, y, _] = spawns[*at].position;
                        if let Some((_, [cx, cy])) = scene::chunk_at(x, y) {
                            areas[*at] = chunks.get((cy * 16 + cx) as usize).copied().unwrap_or(0);
                        }
                    }
                }
                areas
            }),
        );
    }

    /// The spawns of the whole map given to the scene as known, for the Hierarchy: those the user
    /// deleted and those of game events not shown left out.
    fn give_known(&self) {
        let Some(scene) = &self.scene else {
            return;
        };
        if self.areas.len() != self.spawns.len() {
            return;
        }
        let known: Vec<scene::Known> = self
            .spawns
            .iter()
            .zip(self.areas.iter())
            .filter(|(spawn, _)| !self.changes.deleted.contains(&spawn.id()))
            .filter(|(spawn, _)| self.show_events || spawn.event <= 0)
            .map(|(spawn, area)| scene::Known {
                id: spawn.id(),
                label: spawn.name.clone(),
                extra: spawn.entry,
                area: *area,
                origin: spawn.position,
            })
            .collect();
        scene.set_known(MODULE, Arc::new(known));
    }

    /// The changes made `changes`: the tiles of the spawns they touch given and placed again alone.
    fn set_changes(&mut self, changes: Changes) {
        self.generation += 1;
        let before = std::mem::replace(&mut self.changes, changes);
        let mut tiles = self.tiles_of_changes(&before);
        let after = self.changes.clone();
        tiles.extend(self.tiles_of_changes(&after));
        self.looks_wanted = true;
        self.refresh_tiles(Some(&tiles));
        if before.deleted != self.changes.deleted {
            self.give_known();
        }
    }

    /// The tiles of the spawns `changes` names, where the database and `changes` have them.
    fn tiles_of_changes(&self, changes: &Changes) -> BTreeSet<TileId> {
        let mut out = BTreeSet::new();
        let ids = changes
            .moved
            .keys()
            .chain(changes.added.keys())
            .chain(changes.deleted.iter());
        for id in ids {
            let read = self.index.get(id).and_then(|at| self.spawns.get(*at));
            let moved = changes.moved.get(id).map(|(position, _)| *position);
            let added = changes.added.get(id).map(|spawn| spawn.position);
            for position in read.map(|spawn| spawn.position).into_iter().chain(moved).chain(added) {
                out.extend(data::tile_of(position));
            }
        }
        out
    }

    /// The spawns of the tiles `only` (every tile around the camera when none) given to the scene
    /// again and placed, those of the tiles left behind taken away: each spawn found by the index
    /// of its tile, the map never copied whole.
    fn refresh_tiles(&mut self, only: Option<&BTreeSet<TileId>>) {
        let Some(map) = self.map else {
            return;
        };
        if only.is_none() {
            self.looks_wanted = true;
        }
        // The tiles looked at: those asked for, around the camera; none until it is known, which
        // gives them as it comes.
        let every: BTreeSet<TileId> = self.around.clone().unwrap_or_default();
        let targets: BTreeSet<TileId> = match only {
            Some(only) => only.intersection(&every).copied().collect(),
            None => every,
        };
        // Those held and no longer looked at, once every tile is: taken away.
        let left: Vec<TileId> = match only {
            None => self.tiles.difference(&targets).copied().collect(),
            Some(_) => Vec::new(),
        };
        for tile in &left {
            self.take_away(map, *tile);
        }
        // The spawns of the targets as the user changed them: those read there, and those he moved
        // or created, wherever they come from; once each.
        let mut found: BTreeMap<EntityId, Spawn> = BTreeMap::new();
        let candidates = targets
            .iter()
            .filter_map(|tile| self.by_tile.get(tile))
            .flatten()
            .filter_map(|at| self.spawns.get(*at))
            .map(Spawn::id)
            .chain(self.changes.moved.keys().copied())
            .chain(self.changes.added.keys().copied());
        for id in candidates {
            if found.contains_key(&id) {
                continue;
            }
            if let Some(spawn) = self.find(&id)
                && data::tile_of(spawn.position).is_some_and(|tile| targets.contains(&tile))
                // The spawns of game events left out unless the user shows them or the world as it
                // runs has them: the server spawns those of the events running alone.
                && (self.show_events || spawn.event <= 0 || self.live.contains(&id))
            {
                found.insert(id, spawn);
            }
        }
        let spawns: Vec<Spawn> = found.into_values().collect();
        let pieces = data::pieces(&spawns);
        let now: BTreeSet<TileId> = pieces.keys().copied().collect();
        // The targets held before that hold nothing now: taken away.
        let emptied: Vec<TileId> = self
            .tiles
            .intersection(&targets)
            .filter(|tile| !now.contains(tile))
            .copied()
            .collect();
        for tile in &emptied {
            self.take_away(map, *tile);
        }
        if let Some(scene) = &self.scene {
            for (tile, piece) in pieces {
                scene.publish(PieceKey::tile(MODULE, map, tile), Arc::new(piece));
            }
        }
        if let Some(models) = &self.models {
            let mut by_tile: BTreeMap<TileId, Vec<Instance>> = BTreeMap::new();
            for spawn in &spawns {
                if self.live.contains(&spawn.id()) {
                    continue;
                }
                let (Some(tile), Some(Some((look, scale)))) = (
                    data::tile_of(spawn.position),
                    self.looks.get(&(spawn.kind, spawn.display)),
                ) else {
                    continue;
                };
                let id = match spawn.kind {
                    Kind::GameObject => spawn.guid | (1 << 63),
                    _ => spawn.guid,
                };
                by_tile.entry(tile).or_default().push(Instance {
                    id,
                    look: *look,
                    transform: Mat4::from_scale_rotation_translation(
                        Vec3::splat(scale * spawn.scale),
                        spawn
                            .rotation
                            .map_or_else(|| Quat::from_rotation_z(spawn.orientation), Quat::from_array),
                        Vec3::from(spawn.position),
                    ),
                    alpha: 1.0,
                    motion: Motion::Standing,
                    interior: None,
                });
            }
            for tile in &now {
                models.place(
                    &owner(map, *tile),
                    by_tile.get(tile).map(Vec::as_slice).unwrap_or_default(),
                );
            }
        }
        self.tiles.extend(now);
        // The looks of the tiles that came, which may be new.
        if only.is_some() {
            self.looks_wanted = true;
        }
    }

    /// The spawns of the tile `tile` taken out of the scene and of the view.
    fn take_away(&mut self, map: u32, tile: TileId) {
        if let Some(scene) = &self.scene {
            scene.withdraw(PieceKey::tile(MODULE, map, tile));
        }
        if let Some(models) = &self.models {
            models.clear(&owner(map, tile));
        }
        self.tiles.remove(&tile);
    }

    /// Takes the spawns of the map left away.
    fn clear(&mut self) {
        if let Some(map) = self.map {
            for tile in std::mem::take(&mut self.tiles) {
                if let Some(models) = &self.models {
                    models.clear(&owner(map, tile));
                }
            }
        }
        if let Some(scene) = &self.scene {
            scene.withdraw_provider(MODULE, None);
        }
        self.spawns = Arc::default();
        self.index.clear();
        self.by_tile.clear();
        self.areas = Arc::default();
        if let Some(scene) = &self.scene {
            scene.set_known(MODULE, Arc::default());
        }
        self.read_for = None;
    }

    /// The spawn `id` as the user changed it.
    fn find(&self, id: &EntityId) -> Option<Spawn> {
        self.changes.find(&self.spawns, &self.index, id)
    }

    fn ids(arguments: &Value) -> Result<Vec<EntityId>, String> {
        arguments["ids"]
            .as_array()
            .ok_or("'ids' is a list of ids")?
            .iter()
            .map(|id| id.as_str().ok_or("'ids' is a list of ids as text")?.parse())
            .collect()
    }

    /// Gives the changes for the server to the world database, once the spawns or the changes
    /// changed since.
    fn give_pending(&mut self, ctx: &mut Context) {
        let Some(entry_column) = self.entry_column else {
            return;
        };
        if self.given == Some(self.generation) {
            return;
        }
        let Some(database) = ctx.service(world_db::SERVICE) else {
            return;
        };
        self.given = Some(self.generation);
        database.set_pending(self.changes.pending(&self.spawns, &self.index, entry_column));
    }

    /// Makes `after` the changes, one undo entry.
    fn change(&mut self, label: String, after: Changes, ctx: &mut Context) {
        let before = self.changes.clone();
        if before != after {
            ctx.execute(SetChanges { label, before, after });
        }
    }

    fn steer(&mut self, ctx: &mut Context) {
        if self.models.is_none() {
            self.models = ctx.service(models::SERVICE);
            self.formats = ctx.service(uniwow_api::formats::SERVICE);
            self.scene = ctx.service(scene::SERVICE);
        }
        let shown = ctx.editor().call("terrain.map", json!({})).ok();
        let map = shown
            .as_ref()
            .and_then(|map| map.get("id")?.as_u64())
            .map(|id| id as u32);
        if map != self.map {
            self.clear();
            self.map = map;
            self.directory = shown
                .as_ref()
                .and_then(|map| Some(map.get("directory")?.as_str()?.to_owned()));
        }
        self.see_live();
        self.follow_camera(ctx);
        let (Some(map), Some(database)) = (self.map, ctx.service(world_db::SERVICE)) else {
            return;
        };
        let State::Connected(server) = database.state() else {
            return;
        };
        if self.reading.is_some() || self.read_for.as_ref() == Some(&(map, server.clone())) {
            return;
        }
        self.read_for = Some((map, server));
        self.reading = Some(
            ctx.spawn_thread("Read the spawns of the map from the world database", move |_| -> Read {
                // The names of this database's version of AzerothCore.
                let schema = data::Schema::of(
                    &database.query(data::ENTRY_COLUMN, &[])?,
                    &database.query(data::MODELS_TABLE, &[])?,
                );
                let creatures = database.query(&data::creatures(schema), &[json!(map)])?;
                let objects = database.query(data::OBJECTS, &[json!(map)])?;
                let mut spawns = data::spawns(&creatures, Kind::Creature);
                spawns.extend(data::spawns(&objects, Kind::GameObject));
                // The game events of the spawns, which a database without those tables leaves out.
                for (kind, query) in [Kind::Creature, Kind::GameObject].into_iter().zip(data::EVENTS) {
                    match database.query(query, &[]) {
                        Ok(rows) => data::set_events(&mut spawns, kind, &rows),
                        Err(reason) => log::warn!("the game events of the spawns are not read: {reason}"),
                    }
                }
                let highest = [
                    data::highest(&database.query(data::HIGHEST[0], &[])?),
                    data::highest(&database.query(data::HIGHEST[1], &[])?),
                    data::highest(&database.query(data::HIGHEST_TEMPLATE, &[])?),
                ];
                let titles = database.query(data::TITLES, &[])?;
                let links = data::LINKS
                    .iter()
                    .map(|(_, _, query)| database.query(query, &[]))
                    .collect::<Result<Vec<_>, _>>()?;
                // The patrol paths, which a database without those tables leaves out.
                let mut paths = paths::Paths::new();
                for query in [paths::addon_paths(schema.entry), paths::smart_paths(schema.entry)] {
                    match database.query(&query, &[json!(map)]) {
                        Ok(rows) => paths::add_paths(&mut paths, &rows),
                        Err(reason) => log::warn!("the patrol paths are not read: {reason}"),
                    }
                }
                Ok((spawns, highest, Quests::of_rows(&titles, &links), paths, schema.entry))
            }),
        );
    }

    /// The tiles around the camera, within the distance of the setting: the spawns of the map given
    /// again when the camera reaches another tile or the distance changes.
    fn follow_camera(&mut self, ctx: &mut Context) {
        let distance = ctx
            .setting(DISTANCE)
            .and_then(|value| value.as_i64())
            .unwrap_or(DEFAULT_DISTANCE)
            .clamp(1, 64);
        let Some(eye) = ctx
            .service(uniwow_api::viewport::SERVICE)
            .and_then(|view| view.last_view())
            .map(|seen| seen.eye)
        else {
            return;
        };
        let Some(centre) = data::tile_of([eye.x, eye.y, eye.z]) else {
            return;
        };
        let around = tiles_around(centre, distance as u32);
        if self.around.as_ref() == Some(&around) {
            return;
        }
        // The tiles that came near given, those left behind taken away; the others as they are.
        let came: BTreeSet<TileId> = match &self.around {
            Some(before) => around.difference(before).copied().collect(),
            None => around.clone(),
        };
        let left: Vec<TileId> = self.tiles.difference(&around).copied().collect();
        self.around = Some(around);
        self.paths_for = None;
        if let Some(map) = self.map {
            for tile in left {
                self.take_away(map, tile);
            }
        }
        self.refresh_tiles(Some(&came));
    }

    /// The spawns the world as it runs shows live, again when the scene changed; placed again when
    /// they are other ones.
    fn see_live(&mut self) {
        let Some(scene) = &self.scene else {
            return;
        };
        let snapshot = scene.snapshot();
        if snapshot.generation == self.seen {
            return;
        }
        self.seen = snapshot.generation;
        let live = live_of(&snapshot);
        if live == self.live {
            return;
        }
        // Placed again: the tiles of the spawns that came or left alone.
        let tiles: BTreeSet<TileId> = live
            .symmetric_difference(&self.live)
            .filter_map(|id| self.find(id))
            .filter_map(|spawn| data::tile_of(spawn.position))
            .collect();
        self.live = live;
        if !tiles.is_empty() {
            self.refresh_tiles(Some(&tiles));
        }
    }

    /// The patrol paths drawn again when the selection, the choice of all or the paths read change:
    /// of the spawns selected, or of all.
    fn draw_paths(&mut self) {
        let Some(scene) = &self.scene else {
            return;
        };
        let selection = scene.selection();
        let selected = scene.groups().expanded(&selection.ids);
        let snapshot = scene.snapshot();
        // The spawns selected that the live world shows: their places of spawn marked and joined to
        // them, again as they move.
        let live: Vec<(EntityId, [f32; 3])> = selected
            .iter()
            .filter(|id| matches!(id.kind, Kind::Creature | Kind::GameObject))
            .filter_map(|id| {
                snapshot.live(id)?;
                Some((*id, scene.position_now(id)?))
            })
            .collect();
        // Drawn again at each frame while one of them moves.
        let moving = if live.is_empty() { 0 } else { self.frames };
        self.frames = self.frames.wrapping_add(1);
        let now = (
            selection.generation,
            self.all_paths,
            Arc::as_ptr(&self.paths) as usize,
            moving,
        );
        if self.paths_for == Some(now) {
            return;
        }
        self.paths_for = Some(now);
        let mut segments = if self.all_paths {
            // Those of the spawns around the camera alone.
            let around: Vec<u64> = self
                .paths
                .keys()
                .copied()
                .filter(|guid| {
                    let Some(around) = &self.around else {
                        return true;
                    };
                    self.find(&EntityId::creature(*guid))
                        .and_then(|spawn| data::tile_of(spawn.position))
                        .is_some_and(|tile| around.contains(&tile))
                })
                .collect();
            paths::segments(&self.paths, &around)
        } else {
            let guids: Vec<u64> = selected
                .iter()
                .filter(|id| id.kind == Kind::Creature)
                .map(|id| id.key)
                .collect();
            paths::segments(&self.paths, &guids)
        };
        for (id, now) in live {
            if let Some(spawn) = self.find(&id) {
                segments.extend(paths::spawn_marks(spawn.position, now));
            }
        }
        let mut drawn = paths::lock(&self.drawn);
        drawn.segments = segments;
        drawn.version += 1;
    }

    /// The doodad `arguments` describes made a game object (`spawns.from_doodad`).
    fn make_game_object(&mut self, arguments: &Value, ctx: &mut Context) -> Result<Value, String> {
        if self.read_for.is_none() || self.reading.is_some() {
            return Err(
                "the spawns of the world database are not read: its guids and entries are not known".to_owned(),
            );
        }
        let vector = |key: &str| -> Result<[f32; 3], String> {
            let values = arguments[key].as_array().filter(|values| values.len() == 3);
            let values = values.ok_or_else(|| format!("'{key}' is three numbers"))?;
            let mut out = [0.0; 3];
            for (at, value) in out.iter_mut().zip(values) {
                *at = value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| format!("'{key}' is three numbers"))? as f32;
            }
            Ok(out)
        };
        let model = arguments["model"]
            .as_str()
            .filter(|model| !model.is_empty())
            .ok_or("'model' is the path of a model")?;
        let (position, rotation) = (vector("position")?, vector("rotation")?);
        let scale = arguments["scale"].as_f64().filter(|scale| *scale > 0.0).unwrap_or(1.0) as f32;
        let displays: Vec<(u32, String)> = self
            .formats
            .as_ref()
            .and_then(|formats| formats.game_object_displays().ok())
            .map(|displays| {
                displays
                    .iter()
                    .map(|display| (display.id, display.path.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut after = self.changes.clone();
        let made = data::made_of_doodad(&mut after, model, position, rotation, scale, self.highest, &displays);
        self.change("Make a doodad a game object".to_owned(), after, ctx);
        if let Some(doodad) = arguments["doodad"].as_str() {
            ctx.call("doodads.delete", json!({ "ids": [doodad] }));
        }
        Ok(json!({
            "id": made.spawn.id().to_string(),
            "template": made.template.entry,
            "display": made.template.display,
            "prepared": made.display.is_some(),
        }))
    }

    /// The looks of the displays not yet resolved, asked of a job.
    fn resolve(&mut self, ctx: &mut Context) {
        let Some(models) = self.models.clone() else {
            return;
        };
        if self.resolving.is_some() || !self.looks_wanted {
            return;
        }
        self.looks_wanted = false;
        // Those of the spawns of the tiles held alone, and of those the user created.
        let wanted: Vec<(Kind, u32)> = self
            .tiles
            .iter()
            .filter_map(|tile| self.by_tile.get(tile))
            .flatten()
            .filter_map(|at| self.spawns.get(*at))
            .chain(self.changes.added.values())
            .map(|spawn| (spawn.kind, spawn.display))
            .filter(|display| display.1 != 0 && !self.looks.contains_key(display))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if wanted.is_empty() {
            return;
        }
        let prepared = self.changes.displays.clone();
        self.resolving = Some(ctx.spawn("Read the looks of the spawns", move |_| -> Looks {
            wanted
                .into_iter()
                .map(|(kind, display)| {
                    let look = match kind {
                        // A row prepared for a doodad made a game object: its model.
                        Kind::GameObject if prepared.contains_key(&display) => Some((
                            models.look(&models::Look {
                                model: uniwow_api::formats::FileRef::Path(prepared[&display].clone()),
                                textures: Vec::new(),
                                geosets: models::Geosets::Default,
                            }),
                            1.0,
                        )),
                        Kind::GameObject => models
                            .object(display)
                            .ok()
                            .flatten()
                            .map(|look| (models.look(&look), 1.0)),
                        _ => models
                            .display(display)
                            .ok()
                            .map(|(look, scale)| (models.look(&look), scale)),
                    };
                    ((kind, display), look)
                })
                .collect()
        }));
    }
}

impl Module for SpawnsModule {
    fn register(&mut self, reg: &mut Registrar) {
        let ids = json!({ "type": "object", "properties": { "ids": { "type": "array" } }, "required": ["ids"] });
        reg.settings(
            "Spawns",
            vec![uniwow_api::SettingSpec::integer(
                DISTANCE,
                "Distance of the spawns (tiles around the camera)",
                [1, 64],
                DEFAULT_DISTANCE,
            )],
        );
        reg.subscribe(world_db::APPLIED_TOPIC);
        reg.panel("spawns", "Spawns", DockArea::Right)
            .command(
                "spawns.read",
                "The components of the spawns 'ids' (creature:<guid>, gameobject:<guid>): Transform (position, \
                 orientation as the rotation about the vertical), the template, and their columns.",
                ids.clone(),
                json!({ "type": "object", "properties": { "entities": { "type": "array" } } }),
            )
            .command(
                "spawns.write",
                "Changes the 'field' (position, or rotation on the axis 1 for the orientation in degrees) of the spawns \
                 'ids': 'change' a value, or +n, -=n, *n, /n; kept in memory, one undo entry.",
                json!({ "type": "object", "properties": { "ids": { "type": "array" }, "field": { "type": "string" }, "axis": { "type": "integer" }, "change": { "type": "string" } }, "required": ["ids", "field", "change"] }),
                json!({ "type": "object", "properties": { "changed": { "type": "integer" }, "refused": { "type": "array" } } }),
            )
            .command(
                "spawns.duplicate",
                "Duplicates the spawns 'ids', a guid above the highest of their table; one undo entry. The new ids.",
                ids.clone(),
                json!({ "type": "object", "properties": { "ids": { "type": "array" } } }),
            )
            .command(
                "spawns.delete",
                "Deletes the spawns 'ids', in memory; one undo entry.",
                ids,
                json!({ "type": "object", "properties": { "deleted": { "type": "integer" } } }),
            )
            .command(
                "spawns.quests",
                "Gives the quest 'quest' to the templates of the spawns 'ids' ('given' true) or takes it from them, \
                 as the one that 'starts' or 'ends' it ('role'); in memory, one undo entry.",
                json!({ "type": "object", "properties": { "ids": { "type": "array" }, "quest": { "type": "integer" }, "role": { "type": "string" }, "given": { "type": "boolean" } }, "required": ["ids", "quest", "role", "given"] }),
                json!({ "type": "object", "properties": { "changed": { "type": "integer" } } }),
            )
            .command(
                "spawns.quest_titles",
                "The quests whose title holds 'search' (without case) or whose id it is, 50 at most: their ids and titles.",
                json!({ "type": "object", "properties": { "search": { "type": "string" } } }),
                json!({ "type": "object", "properties": { "quests": { "type": "array" } } }),
            )
            .command(
                "spawns.from_doodad",
                "Makes the doodad of the 'model' at 'position' (in the world), turned by 'rotation' (degrees about the                  axes of its file) and scaled by 'scale', a game object, in memory, one undo entry: a template of its                  size (type 5, 2 once it gives a quest) and a spawn at its place, its rotation and yaw; a row of                  GameObjectDisplayInfo prepared when none names its model. The 'doodad' given by its id deleted with it,                  in the gesture of the call. Once the spawns of the world database are read.",
                json!({ "type": "object", "properties": { "model": { "type": "string" }, "position": { "type": "array" }, "rotation": { "type": "array" }, "scale": { "type": "number" } }, "required": ["model", "position", "rotation"] }),
                json!({ "type": "object", "properties": { "id": { "type": "string" }, "template": { "type": "integer" }, "display": { "type": "integer" }, "prepared": { "type": "boolean" } } }),
            );
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        if name == "spawns.from_doodad" {
            return self.make_game_object(&arguments, ctx);
        }
        if name == "spawns.quest_titles" {
            let search = arguments["search"].as_str().unwrap_or_default();
            let quests: Vec<Value> = self
                .quests
                .search(search, 50)
                .into_iter()
                .map(|(id, title)| json!({ "quest": id, "title": title }))
                .collect();
            return Ok(json!({ "quests": quests }));
        }
        let ids = Self::ids(&arguments)?;
        match name {
            "spawns.quests" => {
                let quest = arguments["quest"].as_u64().ok_or("'quest' is the id of a quest")? as u32;
                let ends = match arguments["role"].as_str() {
                    Some("starts") => false,
                    Some("ends") => true,
                    _ => return Err("'role' is starts or ends".to_owned()),
                };
                let given = arguments["given"].as_bool().ok_or("'given' is true or false")?;
                let mut after = self.changes.clone();
                let templates: BTreeSet<(Kind, u32)> = ids
                    .iter()
                    .filter_map(|id| self.find(id))
                    .map(|spawn| (spawn.kind, spawn.entry))
                    .collect();
                let changed = templates
                    .into_iter()
                    .filter(|(kind, entry)| {
                        let link = Link {
                            kind: *kind,
                            entry: *entry,
                            quest,
                            ends,
                        };
                        after.link(&self.quests, link, given)
                    })
                    .count();
                let verb = if given { "Give" } else { "Take" };
                self.change(format!("{verb} the quest {quest}"), after, ctx);
                Ok(json!({ "changed": changed }))
            }
            "spawns.read" => {
                let entities: Vec<Value> = ids
                    .iter()
                    .filter_map(|id| {
                        let spawn = self.find(id)?;
                        let quests = self.changes.quests_of(&self.quests, spawn.kind, spawn.entry);
                        let mut read = data::read(
                            &spawn,
                            self.changes.moved.contains_key(id) || self.changes.added.contains_key(id),
                            &quests,
                            &self.quests.titles,
                        );
                        if let Some(template) = self
                            .changes
                            .templates
                            .get(&spawn.entry)
                            .filter(|_| spawn.kind == Kind::GameObject)
                        {
                            read["components"]["template"] = json!({
                                "entry": template.entry,
                                "name": template.name,
                                "type": template.kind(&quests),
                                "display": template.display,
                                "display prepared": self.changes.displays.get(&template.display),
                                "size": template.size,
                                "created": true,
                            });
                        }
                        if let Some(rotation) = spawn.rotation {
                            read["components"]["raw"]["rotation"] = json!(rotation);
                        }
                        Some(read)
                    })
                    .collect();
                Ok(json!({ "entities": entities }))
            }
            "spawns.write" => {
                let field = arguments["field"].as_str().ok_or("'field' names a field")?;
                let axis = arguments["axis"].as_u64().map(|axis| axis as usize);
                let change = arguments["change"].as_str().ok_or("'change' is a value")?;
                let mut after = self.changes.clone();
                let (mut changed, mut refused) = (0, Vec::new());
                for id in &ids {
                    let Some(spawn) = self.find(id) else {
                        refused.push(format!("{id}: not held"));
                        continue;
                    };
                    match data::written(&spawn, field, axis, change) {
                        Ok(place) if place != (spawn.position, spawn.orientation) => {
                            changed += 1;
                            match after.added.get_mut(id) {
                                Some(added) => {
                                    added.position = place.0;
                                    if added.orientation != place.1 && added.rotation.is_some() {
                                        added.rotation = Some(data::yaw_quaternion(place.1));
                                    }
                                    added.orientation = place.1;
                                }
                                None => {
                                    after.moved.insert(*id, place);
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(reason) => refused.push(format!("{id}: {reason}")),
                    }
                }
                self.change(format!("Set the {field} of {changed} spawns"), after, ctx);
                Ok(json!({ "changed": changed, "refused": refused }))
            }
            "spawns.duplicate" => {
                let mut after = self.changes.clone();
                let mut made = Vec::new();
                for id in &ids {
                    let Some(mut spawn) = self.find(id) else {
                        continue;
                    };
                    let highest = if spawn.kind == Kind::GameObject {
                        self.highest[1]
                    } else {
                        self.highest[0]
                    };
                    spawn.guid = after.give(spawn.kind, highest);
                    made.push(spawn.id().to_string());
                    // Made of a spawn of the database, or of one made of it.
                    let source = after.sources.get(id).copied().unwrap_or(*id);
                    after.sources.insert(spawn.id(), source);
                    after.added.insert(spawn.id(), spawn);
                }
                self.change(format!("Duplicate {} spawns", made.len()), after, ctx);
                Ok(json!({ "ids": made }))
            }
            "spawns.delete" => {
                let mut after = self.changes.clone();
                let mut deleted = 0;
                for id in &ids {
                    if self.find(id).is_none() {
                        continue;
                    }
                    deleted += 1;
                    if after.added.remove(id).is_none() {
                        after.deleted.insert(*id);
                    }
                }
                self.change(format!("Delete {deleted} spawns"), after, ctx);
                Ok(json!({ "deleted": deleted }))
            }
            _ => Err(format!("'{name}' is not handled")),
        }
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        self.steer(ctx);
        self.resolve(ctx);
        self.draw_paths();
        self.give_pending(ctx);
    }

    fn on_event(&mut self, event: &uniwow_api::Event, _ctx: &mut Context) {
        // Applied to the database, or undone there: the changes are its rows now, read again.
        let ours = event.payload["owners"]
            .as_array()
            .is_some_and(|owners| owners.iter().any(|owner| owner.as_str() == Some(MODULE)));
        if event.topic == world_db::APPLIED_TOPIC && ours {
            self.set_changes(Changes::default());
            self.read_for = None;
        }
    }

    fn init(&mut self, ctx: &mut Context) {
        if let Some(view) = ctx.service(uniwow_api::viewport::SERVICE) {
            let layer = paths::PathsLayer::new(self.drawn.clone(), view.target());
            view.add_layer(ctx.module_id(), Box::new(layer));
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        let state = ctx.service(world_db::SERVICE).map(|database| database.state());
        match state {
            None => ui.label("No module world-db: the spawns are not read."),
            Some(State::Connected(_)) => match (&self.refused, self.reading) {
                (_, Some(_)) => ui.label("Reading the spawns of the map…"),
                (Some(reason), _) => ui.colored_label(ui.visuals().warn_fg_color, reason),
                _ => {
                    let creatures = self.spawns.iter().filter(|spawn| spawn.kind == Kind::Creature).count();
                    ui.label(format!(
                        "{creatures} creatures and {} game objects of the map, in {} tiles; {} moved, {} created, {} deleted",
                        self.spawns.len() - creatures,
                        self.tiles.len(),
                        self.changes.moved.len(),
                        self.changes.added.len(),
                        self.changes.deleted.len()
                    ));
                    ui.label(format!(
                        "{} quests, {} givers and enders",
                        self.quests.titles.len(),
                        self.quests.links.len()
                    ));
                    let in_events = self.spawns.iter().filter(|spawn| spawn.event > 0).count();
                    if ui
                        .checkbox(
                            &mut self.show_events,
                            format!("Show the spawns of game events ({in_events})"),
                        )
                        .on_hover_text(
                            "Otherwise those of the events the server runs now alone, as the live world shows them",
                        )
                        .changed()
                    {
                        self.refresh();
                        self.give_known();
                    }
                    ui.checkbox(
                        &mut self.all_paths,
                        format!("Show every patrol path ({} creatures walk one)", self.paths.len()),
                    )
                    .on_hover_text("Otherwise those of the creatures selected alone, in red")
                }
            },
            Some(_) => ui.label("The world database is not connected: see the panel World database."),
        };
    }

    fn project_section(&self) -> Option<Value> {
        Some(self.changes.section())
    }

    fn open_project(&mut self, section: Option<Value>, _ctx: &mut Context) {
        self.changes = Changes::of_section(&section.unwrap_or_default());
        self.generation += 1;
        self.refresh();
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if self.reading == Some(job) {
            self.reading = None;
            match outcome.take::<Read>() {
                Some(Ok((spawns, highest, quests, paths, entry_column))) => {
                    log::info!(
                        "{} spawns of the map read from the world database; {} quests, {} givers and enders",
                        spawns.len(),
                        quests.titles.len(),
                        quests.links.len()
                    );
                    self.index = spawns.iter().enumerate().map(|(at, spawn)| (spawn.id(), at)).collect();
                    self.by_tile = by_tile(&spawns);
                    self.areas = Arc::default();
                    self.spawns = Arc::new(spawns);
                    self.highest = highest;
                    self.quests = Arc::new(quests);
                    self.paths = Arc::new(paths);
                    self.entry_column = Some(entry_column);
                    self.generation += 1;
                    self.refused = None;
                    self.read_areas(ctx);
                    self.refresh();
                }
                Some(Err(reason)) => {
                    log::warn!("the spawns are not read: {reason}");
                    self.refused = Some(format!("The spawns are not read: {reason}"));
                }
                None => {}
            }
            return;
        }
        if self.resolving == Some(job) {
            self.resolving = None;
            if let Some(looks) = outcome.take::<Looks>() {
                self.looks.extend(looks);
                self.refresh();
            }
            return;
        }
        if self.placing == Some(job) {
            self.placing = None;
            if let Some(areas) = outcome.take::<Vec<u32>>() {
                self.areas = Arc::new(areas);
                self.give_known();
            }
        }
    }

    fn shutdown(&mut self) {
        self.clear();
    }
}

uniwow_api::export_module!(SpawnsModule::default());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tiles_around_the_camera_stay_within_the_map() {
        let around = tiles_around(TileId { x: 32, y: 48 }, 3);
        assert_eq!(around.len(), 49);
        assert!(around.contains(&TileId { x: 29, y: 51 }) && !around.contains(&TileId { x: 28, y: 48 }));
        let corner = tiles_around(TileId { x: 0, y: 63 }, 2);
        assert_eq!(corner.len(), 9, "cut by the edges of the map");
    }

    #[test]
    fn a_spawn_is_found_by_its_index_as_the_user_changed_it() {
        let spawn = Spawn {
            kind: Kind::Creature,
            guid: 79666,
            entry: 68,
            name: "Stormwind City Guard".to_owned(),
            display: 3167,
            position: [0.0; 3],
            orientation: 0.0,
            scale: 1.0,
            rotation: None,
            event: 0,
        };
        let spawns = vec![spawn.clone()];
        let index = HashMap::from([(spawn.id(), 0)]);
        let mut changes = Changes::default();
        assert_eq!(changes.find(&spawns, &index, &spawn.id()), Some(spawn.clone()));
        changes.moved.insert(spawn.id(), ([5.0, 0.0, 0.0], 1.0));
        assert_eq!(
            changes.find(&spawns, &index, &spawn.id()).unwrap().position,
            [5.0, 0.0, 0.0]
        );
        changes.deleted.insert(spawn.id());
        assert_eq!(changes.find(&spawns, &index, &spawn.id()), None);
        assert_eq!(changes.find(&spawns, &index, &EntityId::creature(1)), None);
    }
}
