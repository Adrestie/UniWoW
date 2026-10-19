//! The scene of the world (milestone 15): the registry of every entity the providers hold, the
//! terrain's chunks, the water, the doodads, the buildings with their rooms and doodads, the live
//! world, the lights and the weathers; each with its place, the chain of the areas of the chunk
//! under its origin, and its default name. The providers publish their pieces through the service
//! `scene` from any thread; one job at a time indexes them, and `scene.changed` says so once a
//! frame at most. Nothing is changed: it lists.

mod picking;
mod registry;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use uniwow_api::formats::{self, Formats, TileId};
use uniwow_api::scene::{
    self, Answer, Area, EntityId, EntityRef, Hit, Kind, Locks, Picker, PieceKey, Question, Select, Selection, Snapshot,
    chunk_at,
};
use uniwow_api::serde_json::{self, Value, json};
use uniwow_api::viewport::{Ray, Seen};
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
    selection: Mutex<Arc<Selection>>,
    locks: Mutex<Arc<Locks>>,
    names: Mutex<Arc<HashMap<EntityId, String>>>,
    pickers: Mutex<HashMap<String, Arc<dyn Picker>>>,
    /// The questions to the thread of `scene`, each with where its answer goes; none until it runs.
    questions: Mutex<Option<Sender<Asked>>>,
}

/// A question to the thread of `scene`, with where its answer goes.
type Asked = (Question, Sender<Answer>);

/// The kinds locked at first: their entities lie under or around the others.
const LOCKED: [Kind; 6] = [
    Kind::Chunk,
    Kind::Water,
    Kind::Light,
    Kind::LightZone,
    Kind::Weather,
    Kind::Area,
];

impl Shared {
    fn answer(&self, question: Question) -> Answer {
        let snapshot = lock(&self.published).clone();
        let locked = lock(&self.locks).clone();
        match question {
            Question::Pick(ray) => {
                let pickers = lock(&self.pickers).clone();
                Answer::Picked(picking::pick(&snapshot, &ray, &locked, &pickers))
            }
            Question::Within { seen, rect, kinds } => {
                Answer::Within(picking::within(&snapshot, &seen, rect, &kinds, &locked))
            }
        }
    }
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

    fn selection(&self) -> Arc<Selection> {
        journal::lock(&self.0.selection, "scene selection").clone()
    }

    fn select(&self, change: Select) {
        let mut selection = journal::lock(&self.0.selection, "scene selection");
        let ids = selection.changed(&change);
        if ids != selection.ids {
            *selection = Arc::new(Selection {
                generation: selection.generation + 1,
                ids,
            });
        }
    }

    fn add_picker(&self, provider: &str, picker: Arc<dyn Picker>) {
        lock(&self.0.pickers).insert(provider.to_owned(), picker);
    }

    fn locked_kinds(&self) -> Vec<Kind> {
        lock(&self.0.locks).kinds.clone()
    }

    fn set_kind_locked(&self, kind: Kind, locked: bool) {
        let mut locks = lock(&self.0.locks);
        let mut changed = (**locks).clone();
        changed.kinds.retain(|kept| *kept != kind);
        if locked {
            changed.kinds.push(kind);
        }
        changed.generation += 1;
        *locks = Arc::new(changed);
    }

    fn locks(&self) -> Arc<Locks> {
        lock(&self.0.locks).clone()
    }

    fn set_locked(&self, ids: &[EntityId], areas: &[u32], locked: bool) {
        let mut locks = lock(&self.0.locks);
        let mut changed = (**locks).clone();
        for id in ids {
            if locked {
                changed.entities.insert(*id);
            } else {
                changed.entities.remove(id);
            }
        }
        for area in areas {
            if locked {
                changed.areas.insert(*area);
            } else {
                changed.areas.remove(area);
            }
        }
        changed.generation += 1;
        *locks = Arc::new(changed);
    }

    fn names(&self) -> Arc<HashMap<EntityId, String>> {
        lock(&self.0.names).clone()
    }

    fn rename(&self, id: EntityId, name: Option<String>) {
        let mut names = lock(&self.0.names);
        let mut changed = (**names).clone();
        match name.filter(|name| !name.trim().is_empty()) {
            Some(name) => changed.insert(id, name),
            None => changed.remove(&id),
        };
        *names = Arc::new(changed);
    }

    fn scope(&self) -> Option<(u32, TileId, u32)> {
        let (map, [x, y, _]) = (*lock(&self.0.camera))?;
        let (tile, _) = chunk_at(x, y)?;
        Some((map, tile, self.0.scope.load(Ordering::Relaxed).max(1)))
    }

    fn pick(&self, ray: &Ray) -> Option<Hit> {
        match self.0.answer(Question::Pick(*ray)) {
            Answer::Picked(hit) => hit,
            Answer::Within(_) => None,
        }
    }

    fn within(&self, seen: &Seen, rect: [[f32; 2]; 2], kinds: &[Kind]) -> Vec<EntityId> {
        let question = Question::Within {
            seen: *seen,
            rect,
            kinds: kinds.to_vec(),
        };
        match self.0.answer(question) {
            Answer::Within(ids) => ids,
            Answer::Picked(_) => Vec::new(),
        }
    }

    fn ask(&self, question: Question) -> Receiver<Answer> {
        let (answer, answered) = mpsc::channel();
        let sent = lock(&self.0.questions)
            .as_ref()
            .is_some_and(|questions| questions.send((question.clone(), answer.clone())).is_ok());
        if !sent {
            // No thread of its own: answered here.
            let _ = answer.send(self.0.answer(question));
        }
        answered
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
    /// The thread answering the questions, and the generation of the selection last said.
    thread: Option<JobId>,
    said: u64,
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
        let selection = journal::lock(&self.shared.selection, "scene selection").clone();
        if selection.generation != self.said {
            self.said = selection.generation;
            ctx.publish(scene::SELECTION_CHANGED_TOPIC, selection_json(&selection));
        }
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

/// The selection as the commands and the event give it.
fn selection_json(selection: &Selection) -> Value {
    json!({
        "generation": selection.generation,
        "ids": texts(&selection.ids),
        "active": selection.active().map(|id| id.to_string()),
    })
}

/// The ids of a list of ids written as text.
fn read_ids(arguments: &Value) -> Result<Vec<EntityId>, String> {
    match arguments.get("ids") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|id| id.as_str().ok_or("'ids' is a list of ids as text")?.parse())
            .collect(),
        Some(_) => Err("'ids' is a list of ids as text".to_owned()),
    }
}

/// `scene.select`: the selection changed as `mode` says.
fn select(arguments: Value, shared: &Arc<Shared>) -> Result<Value, String> {
    use scene::Scene as _;
    let ids = read_ids(&arguments)?;
    let change = match arguments.get("mode").and_then(Value::as_str).unwrap_or("set") {
        "set" => Select::Set(ids),
        "add" => Select::Add(ids),
        "toggle" => Select::Toggle(ids),
        "remove" => Select::Remove(ids),
        "clear" => Select::Clear,
        other => return Err(format!("no mode '{other}': set, add, toggle, remove or clear")),
    };
    let service = Service(shared.clone());
    service.select(change);
    Ok(selection_json(&service.selection()))
}

/// `scene.pick`: the nearest entity a ray hits.
fn pick(arguments: Value, shared: &Shared) -> Result<Value, String> {
    let vector = |key: &str| -> Result<[f32; 3], String> {
        let values = arguments
            .get(key)
            .and_then(Value::as_array)
            .ok_or(format!("'{key}' is [x, y, z]"))?;
        let number = |at: usize| values.get(at).and_then(Value::as_f64).map(|v| v as f32);
        match (number(0), number(1), number(2)) {
            (Some(x), Some(y), Some(z)) => Ok([x, y, z]),
            _ => Err(format!("'{key}' is [x, y, z]")),
        }
    };
    let direction = uniwow_api::glam::Vec3::from(vector("direction")?)
        .try_normalize()
        .ok_or("'direction' has no length")?;
    let ray = Ray {
        origin: vector("origin")?.into(),
        direction,
    };
    let snapshot = lock(&shared.published).clone();
    Ok(match shared.answer(Question::Pick(ray)) {
        Answer::Picked(Some(hit)) => json!({
            "id": hit.id.to_string(),
            "name": snapshot.get(&hit.id).map(|entity| entity.name()),
            "distance": hit.distance,
            "chain": texts(&hit.chain),
        }),
        _ => Value::Null,
    })
}

/// `scene.lock`: a kind, entities or areas locked or not; the locks.
fn lock_kind(arguments: Value, shared: &Arc<Shared>) -> Result<Value, String> {
    use scene::Scene as _;
    let service = Service(shared.clone());
    let locked = arguments.get("locked").and_then(Value::as_bool).unwrap_or(true);
    if let Some(kind) = arguments.get("kind").and_then(Value::as_str) {
        let kind = Kind::parse(kind).ok_or_else(|| format!("no kind '{kind}'"))?;
        service.set_kind_locked(kind, locked);
    }
    let ids = read_ids(&arguments)?;
    let areas: Vec<u32> = arguments
        .get("areas")
        .and_then(Value::as_array)
        .map(|areas| areas.iter().filter_map(Value::as_u64).map(|area| area as u32).collect())
        .unwrap_or_default();
    if !ids.is_empty() || !areas.is_empty() {
        service.set_locked(&ids, &areas, locked);
    }
    let locks = service.locks();
    let kinds: Vec<&str> = locks.kinds.iter().copied().map(Kind::name).collect();
    let mut areas: Vec<u32> = locks.areas.iter().copied().collect();
    areas.sort_unstable();
    Ok(
        json!({ "locked": kinds, "entities": texts(&locks.entities.iter().copied().collect::<Vec<_>>()), "areas": areas }),
    )
}

/// `scene.rename`: the entity `id` named `name`, or given back its default name without one.
fn rename(arguments: Value, shared: &Arc<Shared>) -> Result<Value, String> {
    use scene::Scene as _;
    let id: EntityId = arguments
        .get("id")
        .and_then(Value::as_str)
        .ok_or("'id' is the id of an entity")?
        .parse()?;
    let name = arguments.get("name").and_then(Value::as_str).map(str::to_owned);
    Service(shared.clone()).rename(id, name);
    Ok(json!({ "id": id.to_string(), "name": lock(&shared.names).get(&id) }))
}

/// `scene.select_like`: the entities within the scope like the active one: of the same model, or of
/// its kind in its zone.
fn select_like(arguments: Value, shared: &Arc<Shared>) -> Result<Value, String> {
    use scene::Scene as _;
    let service = Service(shared.clone());
    let snapshot = lock(&shared.published).clone();
    let id = match arguments.get("id").and_then(Value::as_str) {
        Some(id) => id.parse()?,
        None => service.selection().active().ok_or("nothing selected")?,
    };
    let like = snapshot
        .get(&id)
        .ok_or_else(|| format!("no entity {id} in the scene"))?;
    let by = arguments.get("by").and_then(Value::as_str).unwrap_or("model");
    let (label, kind) = (like.label().to_owned(), id.kind);
    let zone = snapshot.place(like.area()).first().map(|(area, _)| *area);
    let filter = Filter::read(&json!({}), shared)?;
    let ids: Vec<EntityId> = snapshot
        .entities()
        .filter(|entity| filter.keeps(entity) && entity.id().kind == kind)
        .filter(|entity| match by {
            "model" => entity.label() == label,
            "kind-in-zone" => snapshot.place(entity.area()).first().map(|(area, _)| *area) == zone,
            _ => false,
        })
        .map(|entity| entity.id())
        .collect();
    if !matches!(by, "model" | "kind-in-zone") {
        return Err(format!("no way '{by}': model or kind-in-zone"));
    }
    service.select(Select::Set(ids));
    Ok(selection_json(&service.selection()))
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
    let names = lock(&shared.names).clone();
    let mut total = 0;
    let mut page = Vec::new();
    for entity in snapshot.entities().filter(|entity| filter.keeps(entity)) {
        if total >= offset && page.len() < limit {
            let mut described = describe(&entity, false);
            if let Some(name) = names.get(&entity.id()) {
                described["name"] = json!(name);
            }
            page.push(described);
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
    if let Some(name) = lock(&shared.names).get(&id) {
        described["name"] = json!(name);
    }
    described["generation"] = json!(snapshot.generation);
    Ok(described)
}

impl Module for SceneModule {
    fn register(&mut self, reg: &mut Registrar) {
        self.registry = Some(Box::default());
        self.shared.scope.store(2, Ordering::Relaxed);
        *lock(&self.shared.locks) = Arc::new(Locks::of_kinds(&LOCKED));
        let for_rename = self.shared.clone();
        reg.command_on_caller(
            "scene.rename",
            "Names the entity 'id' 'name', or gives it back its default name without one; kept in memory until the \
             project keeps it.",
            json!({ "type": "object", "properties": { "id": { "type": "string" }, "name": { "type": ["string", "null"] } }, "required": ["id"] }),
            json!({ "type": "object" }),
            Arc::new(move |arguments| rename(arguments, &for_rename)),
        );
        let (for_selection, for_select, for_pick, for_lock, for_like) = (
            self.shared.clone(),
            self.shared.clone(),
            self.shared.clone(),
            self.shared.clone(),
            self.shared.clone(),
        );
        reg.command_on_caller(
            "scene.selection",
            "The entities selected, in the order they were chosen, the last the active one.",
            json!({ "type": "object" }),
            json!({ "type": "object", "properties": { "generation": { "type": "integer" }, "ids": { "type": "array" }, "active": { "type": ["string", "null"] } } }),
            Arc::new(move |_| Ok(selection_json(&lock(&for_selection.selection)))),
        )
        .command_on_caller(
            "scene.select",
            "Changes the selection: 'mode' set (by default), add, toggle, remove or clear, of the entities 'ids'. \
             Choosing is not a change of the world: no undo entry. The selection after.",
            json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["set", "add", "toggle", "remove", "clear"] },
                    "ids": { "type": "array", "items": { "type": "string" } }
                }
            }),
            json!({ "type": "object" }),
            Arc::new(move |arguments| select(arguments, &for_select)),
        )
        .command_on_caller(
            "scene.pick",
            "The nearest entity of the scene the ray from 'origin' along 'direction' hits, its kind not locked: its \
             id, name, distance and the chain of its parents from the outermost; null for none.",
            json!({
                "type": "object",
                "properties": { "origin": { "type": "array" }, "direction": { "type": "array" } },
                "required": ["origin", "direction"]
            }),
            json!({ "type": ["object", "null"] }),
            Arc::new(move |arguments| pick(arguments, &for_pick)),
        )
        .command_on_caller(
            "scene.lock",
            "Locks a kind of entity, the entities 'ids' or the areas 'areas' (what lies in them), or unlocks them with \
             'locked': false: they are then not picked in the 3D view, but can be chosen in the Hierarchy; without \
             any, only the locks. The chunks, the water, the lights, the zones of light, the weathers and the areas \
             are locked at first.",
            json!({ "type": "object", "properties": { "kind": { "type": "string" }, "ids": { "type": "array" }, "areas": { "type": "array" }, "locked": { "type": "boolean" } } }),
            json!({ "type": "object", "properties": { "locked": { "type": "array" } } }),
            Arc::new(move |arguments| lock_kind(arguments, &for_lock)),
        )
        .command_on_caller(
            "scene.select_like",
            "Selects the entities within the scope like the entity 'id', the active one by default: 'by' model (the \
             same model, of its kind), or kind-in-zone (its kind in its zone).",
            json!({ "type": "object", "properties": { "id": { "type": "string" }, "by": { "type": "string", "enum": ["model", "kind-in-zone"] } } }),
            json!({ "type": "object" }),
            Arc::new(move |arguments| select_like(arguments, &for_like)),
        );
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

    fn init(&mut self, ctx: &mut Context) {
        // The questions answered on a thread of their own, never behind the jobs of the pool.
        let (questions, asked) = mpsc::channel::<Asked>();
        *lock(&self.shared.questions) = Some(questions);
        let shared = self.shared.clone();
        self.thread = Some(ctx.spawn_thread("Answer the picking of the scene", move |job| {
            while !job.is_cancelled() {
                match asked.recv_timeout(Duration::from_millis(100)) {
                    Ok((question, answer)) => {
                        let _ = answer.send(shared.answer(question));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        }));
    }

    fn shutdown(&mut self) {
        *lock(&self.shared.questions) = None;
    }

    fn windows_ui(&mut self, _egui: &uniwow_api::egui::Context, ctx: &mut Context) {
        self.steer(ctx);
    }

    fn project_section(&self) -> Option<Value> {
        Some(section_of(&lock(&self.shared.names), &lock(&self.shared.locks)))
    }

    fn open_project(&mut self, section: Option<Value>, _ctx: &mut Context) {
        let (names, mut locks) = opened(&section.unwrap_or_default());
        locks.generation = lock(&self.shared.locks).generation + 1;
        *lock(&self.shared.names) = Arc::new(names);
        *lock(&self.shared.locks) = Arc::new(locks);
    }

    fn on_event(&mut self, event: &Event, _ctx: &mut Context) {
        if event.topic == MODULE_FAILED_TOPIC
            && let Some(id) = event.payload.get("id").and_then(Value::as_str)
        {
            journal::lock(&self.shared.pending, "scene pending").push(Change::WithdrawProvider(id.to_owned(), None));
        }
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if self.thread == Some(job) {
            self.thread = None;
            *lock(&self.shared.questions) = None;
            if let JobOutcome::Panicked(message) = outcome {
                log::error!("the thread of the picking stopped, the questions answered where asked: {message}");
            }
            return;
        }
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

/// The section of the project of the scene: the names given and the locks, in order.
fn section_of(names: &HashMap<EntityId, String>, locks: &Locks) -> Value {
    let names: std::collections::BTreeMap<String, &String> =
        names.iter().map(|(id, name)| (id.to_string(), name)).collect();
    let mut entities: Vec<String> = locks.entities.iter().map(ToString::to_string).collect();
    entities.sort();
    let mut areas: Vec<u32> = locks.areas.iter().copied().collect();
    areas.sort_unstable();
    let mut kinds: Vec<&str> = locks.kinds.iter().map(|kind| kind.name()).collect();
    kinds.sort_unstable();
    json!({
        "names": names,
        "locks": { "kinds": kinds, "entities": entities, "areas": areas },
    })
}

/// The names and the locks of the section `section` of the project; the kinds locked at first
/// without one.
fn opened(section: &Value) -> (HashMap<EntityId, String>, Locks) {
    let names: HashMap<EntityId, String> = section["names"]
        .as_object()
        .map(|names| {
            names
                .iter()
                .filter_map(|(id, name)| Some((id.parse().ok()?, name.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let strings = |key: &str| -> Vec<String> {
        section["locks"][key]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut locks = match section["locks"]["kinds"].as_array() {
        Some(_) => Locks::of_kinds(
            &strings("kinds")
                .iter()
                .filter_map(|kind| Kind::parse(kind))
                .collect::<Vec<_>>(),
        ),
        None => Locks::of_kinds(&LOCKED),
    };
    locks.entities = strings("entities").iter().filter_map(|id| id.parse().ok()).collect();
    locks.areas = section["locks"]["areas"]
        .as_array()
        .map(|areas| areas.iter().filter_map(Value::as_u64).map(|area| area as u32).collect())
        .unwrap_or_default();
    (names, locks)
}

#[cfg(test)]
mod project_tests {
    use super::*;

    #[test]
    fn the_names_and_the_locks_are_kept_in_the_project_and_read_back() {
        let names = HashMap::from([(EntityId::doodad(0, 7), "The old oak".to_owned())]);
        let mut locks = Locks::of_kinds(&[Kind::Chunk, Kind::Water]);
        locks.entities.insert(EntityId::building(0, 9));
        locks.areas.insert(87);
        let section = section_of(&names, &locks);
        let (read_names, read_locks) = opened(&section);
        assert_eq!(read_names, names);
        assert_eq!(read_locks.entities, locks.entities);
        assert_eq!(read_locks.areas, locks.areas);
        let mut kinds = read_locks.kinds.clone();
        kinds.sort();
        assert_eq!(kinds, [Kind::Chunk, Kind::Water]);
        let (none, fresh) = opened(&Value::Null);
        assert!(none.is_empty());
        assert_eq!(fresh.kinds, LOCKED.to_vec(), "a new project: the kinds locked at first");
    }
}

uniwow_api::export_module!(SceneModule::default());
