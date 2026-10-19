//! The scene of the world (milestone 15): the registry of every entity the providers hold, the
//! terrain's chunks, the water, the doodads, the buildings with their rooms and doodads, the live
//! world, the lights and the weathers; each with its place, the chain of the areas of the chunk
//! under its origin, and its default name. The providers publish their pieces through the service
//! `scene` from any thread; one job at a time indexes them, and `scene.changed` says so once a
//! frame at most. Nothing is changed: it lists.

mod registry;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uniwow_api::formats::{self, Formats, TileId};
use uniwow_api::scene::{self, Area, EntityId, EntityRef, Kind, PieceKey, Snapshot, chunk_at};
use uniwow_api::serde_json::{self, Value, json};
use uniwow_api::{
    Context, Event, JobId, JobOutcome, MODULE_FAILED_TOPIC, Module, PropertyValue, Registrar, SettingSpec, journal, log,
};

use registry::{AreaSource, Change, Registry, Summary};

/// The setting of how far around the camera the views list the entities, in tiles.
const SCOPE: &str = "scope";
/// The ids a `scene.changed` lists at most, past which it says `many`.
const LISTED: usize = 1000;
/// The entities `scene.entities` gives at most in one page.
const PAGE: usize = 1000;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn scope_setting() -> SettingSpec {
    SettingSpec::integer(SCOPE, "Scope (tiles)", [1, i64::from(formats::WHOLE_MAP)], 2)
}

/// What the service and the commands share with the module, between threads.
#[derive(Default)]
struct Shared {
    /// The changes published since the last job took them.
    pending: Mutex<Vec<Change>>,
    published: Mutex<Arc<Snapshot>>,
    /// The map the terrain shows, by its id, and the camera, as the interface thread last saw them.
    camera: Mutex<Option<(u32, [f32; 3])>>,
    scope: AtomicU32,
}

struct Service(Arc<Shared>);

impl scene::Scene for Service {
    fn publish(&self, key: PieceKey, piece: Arc<scene::Piece>) {
        journal::lock(&self.0.pending, "scene pending").push(Change::Publish(key, piece));
    }

    fn withdraw(&self, key: PieceKey) {
        journal::lock(&self.0.pending, "scene pending").push(Change::Withdraw(key));
    }

    fn withdraw_provider(&self, provider: &str, map: Option<u32>) {
        journal::lock(&self.0.pending, "scene pending").push(Change::WithdrawProvider(provider.to_owned(), map));
    }

    fn snapshot(&self) -> Arc<Snapshot> {
        journal::lock(&self.0.published, "scene registry").clone()
    }
}

/// The areas as the module `formats` reads them, the folders of the maps read once.
struct FormatsAreas {
    formats: Option<Arc<dyn Formats>>,
    directories: Option<HashMap<u32, String>>,
}

impl FormatsAreas {
    fn directory(&mut self, map: u32) -> Result<Option<String>, ()> {
        let formats = self.formats.as_ref().ok_or(())?;
        if self.directories.is_none() {
            let maps = formats.maps().map_err(|_| ())?;
            self.directories = Some(maps.iter().map(|map| (map.id, map.directory.clone())).collect());
        }
        Ok(self.directories.as_ref().and_then(|maps| maps.get(&map).cloned()))
    }
}

impl AreaSource for std::cell::RefCell<FormatsAreas> {
    fn table(&self) -> Option<HashMap<u32, Area>> {
        let areas = self.borrow().formats.as_ref()?.areas().ok()?;
        Some(
            areas
                .iter()
                .map(|area| {
                    let record = Area {
                        parent: area.parent,
                        map: area.map,
                        name: area.name.clone(),
                    };
                    (area.id, record)
                })
                .collect(),
        )
    }

    fn tile(&self, map: u32, tile: TileId) -> Result<Option<Arc<[u32]>>, ()> {
        let mut source = self.borrow_mut();
        let Some(directory) = source.directory(map)? else {
            return Ok(None);
        };
        let formats = source.formats.clone().ok_or(())?;
        match formats.tile_areas(&directory, tile.x, tile.y) {
            Ok(areas) => Ok(areas.map(Arc::from)),
            Err(reason) => {
                log::debug!("the areas of the tile {} {} of {directory}: {reason}", tile.x, tile.y);
                Ok(None)
            }
        }
    }
}

/// What a job of the registry gives back: the registry, and what it changed.
type Indexed = (Box<Registry>, Summary);

#[derive(Default)]
struct SceneModule {
    shared: Arc<Shared>,
    /// The registry, here between its jobs; the job running.
    registry: Option<Box<Registry>>,
    job: Option<JobId>,
}

impl SceneModule {
    /// At each frame: the camera and the scope for the commands of other threads; then, while no
    /// job runs, one started for the changes gathered.
    fn steer(&mut self, ctx: &mut Context) {
        let scope = scope_setting().value(ctx.setting(SCOPE).as_ref());
        self.shared.scope.store(scope as u32, Ordering::Relaxed);
        let map = ctx
            .editor()
            .call("terrain.map", json!({}))
            .ok()
            .and_then(|map| map.get("id")?.as_u64());
        let camera = match (map, ctx.read_property("viewport/camera_position")) {
            (Some(map), Ok(PropertyValue::Vector([x, y, z]))) => Some((map as u32, [x as f32, y as f32, z as f32])),
            _ => None,
        };
        *journal::lock(&self.shared.camera, "scene camera") = camera;
        if self.job.is_some() || journal::lock(&self.shared.pending, "scene pending").is_empty() {
            return;
        }
        let Some(mut registry) = self.registry.take() else {
            return;
        };
        let (shared, formats) = (self.shared.clone(), ctx.service(formats::SERVICE));
        self.job = Some(ctx.spawn("Index the entities of the scene", move |_| -> Indexed {
            let changes = std::mem::take(&mut *lock(&shared.pending));
            let source = std::cell::RefCell::new(FormatsAreas {
                formats,
                directories: None,
            });
            let (snapshot, summary) = registry.apply(changes, &source);
            *lock(&shared.published) = Arc::new(snapshot);
            (registry, summary)
        }));
    }
}

/// The ids of `ids` as text.
fn texts(ids: &[EntityId]) -> Vec<String> {
    ids.iter().map(ToString::to_string).collect()
}

/// An entity as the commands give it.
fn describe(entity: &EntityRef, whole: bool) -> Value {
    let snapshot = entity.snapshot;
    let place: Vec<Value> = snapshot
        .place(entity.area())
        .into_iter()
        .map(|(id, name)| json!({ "area": id, "name": name }))
        .collect();
    let [x, y, z] = entity.origin();
    let mut described = json!({
        "id": entity.id().to_string(),
        "kind": entity.id().kind.name(),
        "name": entity.name(),
        "place": place,
        "parent": entity.parent().map(|parent| parent.to_string()),
        "position": [x, y, z],
    });
    if whole {
        let map = entity.placed.key.map;
        let [low, high] = entity.bounds();
        let own = entity.area();
        let others: Vec<Value> = snapshot
            .areas_within(map, entity.bounds())
            .into_iter()
            .filter(|area| *area != own)
            .map(|area| json!({ "area": area, "name": snapshot.areas.get(&area).map_or("", |a| a.name.as_str()) }))
            .collect();
        let tiles: Vec<String> = entity
            .tiles()
            .iter()
            .map(|tile| format!("{}_{}", tile.x, tile.y))
            .collect();
        let listed = snapshot.listings(&entity.id()).len();
        let fields = described.as_object_mut().expect("an object");
        fields.insert("label".to_owned(), json!(entity.label()));
        fields.insert("map".to_owned(), json!(map));
        fields.insert("bounds".to_owned(), json!([low, high]));
        fields.insert("other_zones".to_owned(), json!(others));
        fields.insert("tiles".to_owned(), json!(tiles));
        fields.insert("listed".to_owned(), json!(listed));
        fields.insert("provider".to_owned(), json!(entity.provider()));
        fields.insert("flags".to_owned(), json!(entity.flags()));
        fields.insert("extra".to_owned(), json!(entity.extra()));
    }
    described
}

/// The filter of `scene.entities`.
#[derive(Default)]
struct Filter {
    kinds: Vec<Kind>,
    text: Option<String>,
    provider: Option<String>,
    /// The map and the tile around which, and how many tiles, the entities are kept; none for all.
    around: Option<(u32, TileId, u32)>,
}

impl Filter {
    fn read(arguments: &Value, shared: &Shared) -> Result<Self, String> {
        let kinds = match arguments.get("kind") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(kind)) => vec![Kind::parse(kind).ok_or_else(|| format!("no kind '{kind}'"))?],
            Some(Value::Array(kinds)) => kinds
                .iter()
                .map(|kind| {
                    let name = kind.as_str().unwrap_or_default();
                    Kind::parse(name).ok_or_else(|| format!("no kind '{name}'"))
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("'kind' is a kind or a list of kinds".to_owned()),
        };
        let all = arguments.get("all").and_then(Value::as_bool).unwrap_or(false);
        let scope = match arguments.get("scope").and_then(Value::as_u64) {
            Some(scope) => scope.clamp(1, u64::from(formats::WHOLE_MAP)) as u32,
            None => shared.scope.load(Ordering::Relaxed).max(1),
        };
        let around = if all {
            None
        } else {
            let at = match arguments.get("around").and_then(Value::as_array) {
                Some(point) => {
                    let number = |at: usize| point.get(at).and_then(Value::as_f64).map(|v| v as f32);
                    let (Some(x), Some(y)) = (number(0), number(1)) else {
                        return Err("'around' is [x, y]".to_owned());
                    };
                    let map = lock(&shared.camera).map(|(map, _)| map);
                    map.map(|map| (map, [x, y]))
                }
                None => lock(&shared.camera).map(|(map, [x, y, _])| (map, [x, y])),
            };
            let Some((map, [x, y])) = at else {
                return Err("no map shown and no camera: give 'all': true".to_owned());
            };
            let (tile, _) = chunk_at(x, y).ok_or("the point is off the map")?;
            Some((map, tile, scope))
        };
        Ok(Self {
            kinds,
            text: arguments
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_lowercase),
            provider: arguments.get("provider").and_then(Value::as_str).map(str::to_owned),
            around,
        })
    }

    fn keeps(&self, entity: &EntityRef) -> bool {
        let id = entity.id();
        if !self.kinds.is_empty() && !self.kinds.contains(&id.kind) {
            return false;
        }
        if self
            .provider
            .as_deref()
            .is_some_and(|provider| provider != entity.provider())
        {
            return false;
        }
        if let Some((map, tile, scope)) = self.around {
            let [x, y, _] = entity.origin();
            let near = entity.placed.key.map == map
                && chunk_at(x, y).is_some_and(|(at, _)| at.x.abs_diff(tile.x).max(at.y.abs_diff(tile.y)) <= scope);
            if !near {
                return false;
            }
        }
        if let Some(text) = &self.text {
            let label = entity.label().to_lowercase();
            if !label.contains(text.as_str()) && !entity.name().to_lowercase().contains(text.as_str()) {
                return false;
            }
        }
        true
    }
}

/// `scene.entities`: the entities a filter keeps, a page of them.
fn entities(arguments: Value, shared: &Shared) -> Result<Value, String> {
    let filter = Filter::read(&arguments, shared)?;
    let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = (arguments.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize).min(PAGE);
    let snapshot = lock(&shared.published).clone();
    let mut total = 0;
    let mut page = Vec::new();
    for entity in snapshot.entities().filter(|entity| filter.keeps(entity)) {
        if total >= offset && page.len() < limit {
            page.push(describe(&entity, false));
        }
        total += 1;
    }
    Ok(json!({
        "generation": snapshot.generation,
        "total": total,
        "offset": offset,
        "entities": page,
    }))
}

/// `scene.entity`: the entity `id`, whole.
fn entity(arguments: Value, shared: &Shared) -> Result<Value, String> {
    let id: EntityId = arguments
        .get("id")
        .and_then(Value::as_str)
        .ok_or("'id' is the id of an entity")?
        .parse()?;
    let snapshot = lock(&shared.published).clone();
    let entity = snapshot
        .get(&id)
        .ok_or_else(|| format!("no entity {id} in the scene"))?;
    let mut described = describe(&entity, true);
    described["generation"] = json!(snapshot.generation);
    Ok(described)
}

impl Module for SceneModule {
    fn register(&mut self, reg: &mut Registrar) {
        self.registry = Some(Box::default());
        self.shared.scope.store(2, Ordering::Relaxed);
        let service: scene::Handle = Arc::new(Service(self.shared.clone()));
        let (for_entities, for_entity) = (self.shared.clone(), self.shared.clone());
        reg.settings("Scene", vec![scope_setting()])
            .provide(scene::SERVICE, service)
            .subscribe(MODULE_FAILED_TOPIC)
            .command_on_caller(
                "scene.entities",
                "The entities of the scene a filter keeps, each once, a page of them: by default those within the \
                 scope (tiles) around the camera on the map shown; 'all' for every one held. Each with its id, kind, \
                 default name, place (the chain of the areas from its zone down), game parent and position.",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": ["string", "array"], "description": "A kind or a list of kinds: chunk, water, doodad, building, room, building-doodad, live, light, light-zone, weather…" },
                        "text": { "type": "string", "description": "Kept when its name or label holds it, whatever the case" },
                        "provider": { "type": "string", "description": "The module that published it" },
                        "scope": { "type": "integer", "minimum": 1, "description": "Tiles around the camera, the setting by default" },
                        "around": { "type": "array", "items": { "type": "number" }, "description": "[x, y], in place of the camera" },
                        "all": { "type": "boolean" },
                        "offset": { "type": "integer", "minimum": 0 },
                        "limit": { "type": "integer", "minimum": 1, "maximum": PAGE }
                    }
                }),
                json!({
                    "type": "object",
                    "properties": {
                        "generation": { "type": "integer" },
                        "total": { "type": "integer" },
                        "offset": { "type": "integer" },
                        "entities": { "type": "array" }
                    }
                }),
                Arc::new(move |arguments| entities(arguments, &for_entities)),
            )
            .command_on_caller(
                "scene.entity",
                "One entity of the scene, whole: besides what scene.entities gives, its label, map, bounds, the other \
                 zones its bounds reach, the tiles whose files store it, how many listings it has, its provider, \
                 flags and extra number.",
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string", "description": "As doodad:0:183214" } },
                    "required": ["id"]
                }),
                json!({ "type": "object" }),
                Arc::new(move |arguments| entity(arguments, &for_entity)),
            );
    }

    fn windows_ui(&mut self, _egui: &uniwow_api::egui::Context, ctx: &mut Context) {
        self.steer(ctx);
    }

    fn on_event(&mut self, event: &Event, _ctx: &mut Context) {
        if event.topic == MODULE_FAILED_TOPIC
            && let Some(id) = event.payload.get("id").and_then(Value::as_str)
        {
            journal::lock(&self.shared.pending, "scene pending").push(Change::WithdrawProvider(id.to_owned(), None));
        }
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if self.job != Some(job) {
            return;
        }
        self.job = None;
        let Some((registry, summary)) = outcome.take::<Indexed>() else {
            // Made again from nothing: the providers' pieces come back as they publish them again.
            log::error!("the registry of the scene failed: made again, empty");
            self.registry = Some(Box::default());
            return;
        };
        self.registry = Some(registry);
        let count = summary.added.len() + summary.changed.len() + summary.removed.len();
        if count == 0 {
            return;
        }
        log::debug!(
            "scene: {} pieces indexed in {:.2} ms, {} entities",
            summary.pieces,
            summary.took.as_secs_f64() * 1000.0,
            summary.entities
        );
        let many = count > LISTED;
        let list = |ids: &[EntityId]| if many { Vec::new() } else { texts(ids) };
        ctx.publish(
            scene::CHANGED_TOPIC,
            serde_json::json!({
                "generation": summary.generation,
                "entities": summary.entities,
                "added": list(&summary.added),
                "changed": list(&summary.changed),
                "removed": list(&summary.removed),
                "many": many,
            }),
        );
    }
}

uniwow_api::export_module!(SceneModule::default());
