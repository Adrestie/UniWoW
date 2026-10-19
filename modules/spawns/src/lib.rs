//! The spawns of the world database (milestone 15, step 15.11b): the creatures and the game objects
//! of the map the terrain shows, read by a job through the service `world-db` once it is connected,
//! with their templates' names and looks; given to the scene by tile, drawn through `models` by
//! their looks; their places changed, created and deleted in memory, each one undo entry, kept in the
//! project. Nothing is written to the database.

mod data;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use uniwow_api::formats::TileId;
use uniwow_api::glam::{Mat4, Quat, Vec3};
use uniwow_api::models::{self, Instance, LookId, Motion};
use uniwow_api::scene::{self, EntityId, Kind, PieceKey};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, State};
use uniwow_api::{Command, Context, DockArea, JobId, JobOutcome, Module, Registrar, egui, log};

use data::{Changes, Spawn};

/// The id of the module, the provider of its pieces and the owner of its instances.
const MODULE: &str = "spawns";

/// What a reading job gives back: the spawns of a map, and the highest guid of each table.
type Read = Result<(Vec<Spawn>, u64, u64), String>;

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
        module.changes = self.after.clone();
        module.refresh();
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut SpawnsModule = module.downcast_mut().expect("the spawns");
        module.changes = self.before.clone();
        module.refresh();
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
    highest: [u64; 2],
    changes: Changes,
    looks: Looks,
    resolving: Option<JobId>,
    /// The tiles given to the scene and placed, for those to take away.
    tiles: BTreeSet<TileId>,
    refused: Option<String>,
    models: Option<models::Handle>,
    scene: Option<scene::Handle>,
}

fn owner(map: u32, tile: TileId) -> String {
    format!("{MODULE}/{map}/{}_{}", tile.x, tile.y)
}

impl SpawnsModule {
    /// The spawns as the user changed them given to the scene again and placed, by tile; the looks
    /// not yet resolved asked of a job.
    fn refresh(&mut self) {
        let Some(map) = self.map else {
            return;
        };
        let spawns = self.changes.laid_over(&self.spawns);
        let pieces = data::pieces(&spawns);
        let now: BTreeSet<TileId> = pieces.keys().copied().collect();
        if let Some(scene) = &self.scene {
            for tile in self.tiles.difference(&now) {
                scene.withdraw(PieceKey::tile(MODULE, map, *tile));
            }
            for (tile, piece) in pieces {
                scene.publish(PieceKey::tile(MODULE, map, tile), Arc::new(piece));
            }
        }
        if let Some(models) = &self.models {
            for tile in self.tiles.difference(&now) {
                models.clear(&owner(map, *tile));
            }
            let mut by_tile: BTreeMap<TileId, Vec<Instance>> = BTreeMap::new();
            for spawn in &spawns {
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
                        Quat::from_rotation_z(spawn.orientation),
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
        self.tiles = now;
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
        self.read_for = None;
    }

    /// The spawn `id` as the user changed it.
    fn find(&self, id: &EntityId) -> Option<Spawn> {
        self.changes
            .laid_over(&self.spawns)
            .into_iter()
            .find(|spawn| spawn.id() == *id)
    }

    fn ids(arguments: &Value) -> Result<Vec<EntityId>, String> {
        arguments["ids"]
            .as_array()
            .ok_or("'ids' is a list of ids")?
            .iter()
            .map(|id| id.as_str().ok_or("'ids' is a list of ids as text")?.parse())
            .collect()
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
            self.scene = ctx.service(scene::SERVICE);
        }
        let map = ctx
            .editor()
            .call("terrain.map", json!({}))
            .ok()
            .and_then(|map| map.get("id")?.as_u64())
            .map(|id| id as u32);
        if map != self.map {
            self.clear();
            self.map = map;
        }
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
                let creatures = database.query(data::CREATURES, &[json!(map)])?;
                let objects = database.query(data::OBJECTS, &[json!(map)])?;
                let mut spawns = data::spawns(&creatures, Kind::Creature);
                spawns.extend(data::spawns(&objects, Kind::GameObject));
                let highest_creature = data::highest(&database.query(data::HIGHEST[0], &[])?);
                let highest_object = data::highest(&database.query(data::HIGHEST[1], &[])?);
                Ok((spawns, highest_creature, highest_object))
            }),
        );
    }

    /// The looks of the displays not yet resolved, asked of a job.
    fn resolve(&mut self, ctx: &mut Context) {
        let Some(models) = self.models.clone() else {
            return;
        };
        if self.resolving.is_some() {
            return;
        }
        let wanted: Vec<(Kind, u32)> = self
            .changes
            .laid_over(&self.spawns)
            .iter()
            .map(|spawn| (spawn.kind, spawn.display))
            .filter(|display| display.1 != 0 && !self.looks.contains_key(display))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if wanted.is_empty() {
            return;
        }
        self.resolving = Some(ctx.spawn("Read the looks of the spawns", move |_| -> Looks {
            wanted
                .into_iter()
                .map(|(kind, display)| {
                    let look = match kind {
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
            );
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        let ids = Self::ids(&arguments)?;
        match name {
            "spawns.read" => {
                let entities: Vec<Value> = ids
                    .iter()
                    .filter_map(|id| {
                        let spawn = self.find(id)?;
                        Some(data::read(
                            &spawn,
                            self.changes.moved.contains_key(id) || self.changes.added.contains_key(id),
                        ))
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
                    ))
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
        self.refresh();
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if self.reading == Some(job) {
            self.reading = None;
            match outcome.take::<Read>() {
                Some(Ok((spawns, creatures, objects))) => {
                    log::info!("{} spawns of the map read from the world database", spawns.len());
                    self.spawns = Arc::new(spawns);
                    self.highest = [creatures, objects];
                    self.refused = None;
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
        }
    }

    fn shutdown(&mut self) {
        self.clear();
    }
}

uniwow_api::export_module!(SpawnsModule::default());
