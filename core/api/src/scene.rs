//! Interface of the "scene" service (milestone 15): everything the Hierarchy lists is an entity,
//! with a stable id that survives a restart; the modules that hold them, the providers, publish
//! them by pieces (a tile of doodads, a building with its rooms, the live world), from any thread,
//! and the module `scene` keeps them in its registry, indexed by jobs, read by anyone from a
//! snapshot shared between threads (rule T3). Nothing is changed through it: it lists.

pub mod meshes;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use crate::ServiceKey;
use crate::formats::{FileRef, TileId};
use crate::viewport::{Ray, Seen};

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("scene");

pub type Handle = Arc<dyn Scene>;

/// Topic published by the module `scene` once a frame at most, when its registry changed. Payload:
/// `{ "generation", "entities", "added", "changed", "removed" }`, the last three lists of ids as
/// text, empty with `"many": true` past 1,000 ids in all.
pub const CHANGED_TOPIC: &str = "scene.changed";

/// Shared between threads (rule T3).
pub trait Scene: Send + Sync {
    /// Publishes `piece` as the piece `key`, in place of the one before it: from any thread, the
    /// time of a push; indexed by the next job of the registry.
    fn publish(&self, key: PieceKey, piece: Arc<Piece>);

    /// Takes away the piece `key`.
    fn withdraw(&self, key: PieceKey);

    /// Takes away every piece of `provider`, of the map `map` alone when one is given.
    fn withdraw_provider(&self, provider: &str, map: Option<u32>);

    /// The registry as its last job left it.
    fn snapshot(&self) -> Arc<Snapshot>;

    /// The selection as it is now.
    fn selection(&self) -> Arc<Selection>;

    /// Changes the selection, from any thread: said by `scene.selection_changed` at the end of the
    /// frame. Choosing is not a change of the world: no undo entry.
    fn select(&self, change: Select);

    /// Gives the narrow phase of the picking of `provider`'s entities: without one, a box hit is
    /// the hit.
    fn add_picker(&self, provider: &str, picker: Arc<dyn Picker>);

    /// The kinds locked: their entities are not picked in the 3D view, by a click or a rectangle,
    /// but can be chosen in the Hierarchy. The chunks, the water, the lights, the zones of light and
    /// the weathers by default.
    fn locked_kinds(&self) -> Vec<Kind>;

    fn set_kind_locked(&self, kind: Kind, locked: bool);

    /// The nearest entity the ray `ray` hits, not locked, with the chain of its parents; none for
    /// none. On the calling thread.
    fn pick(&self, ray: &Ray) -> Option<Hit>;

    /// The entities not locked, of `kinds` (every kind not locked when empty), whose origin falls in
    /// front of the eye within `rect` of the view `seen`, in points from its top left; on the calling
    /// thread.
    fn within(&self, seen: &Seen, rect: [[f32; 2]; 2], kinds: &[Kind]) -> Vec<EntityId>;

    /// Asks `question` of the thread of `scene`, never queued behind the jobs of the pool: its
    /// answer comes through the receiver.
    fn ask(&self, question: Question) -> Receiver<Answer>;
}

/// Topic published by the module `scene` at the end of a frame its selection changed in. Payload:
/// `{ "generation", "ids", "active" }`, the ids as text, the active one the last chosen.
pub const SELECTION_CHANGED_TOPIC: &str = "scene.selection_changed";

/// The entities selected, in the order they were chosen, the last the active one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub generation: u64,
    pub ids: Vec<EntityId>,
}

/// A change of the selection.
#[derive(Clone, Debug, PartialEq)]
pub enum Select {
    Set(Vec<EntityId>),
    Add(Vec<EntityId>),
    /// Each removed when selected, added otherwise.
    Toggle(Vec<EntityId>),
    Remove(Vec<EntityId>),
    Clear,
}

impl Selection {
    pub fn active(&self) -> Option<EntityId> {
        self.ids.last().copied()
    }

    pub fn contains(&self, id: &EntityId) -> bool {
        self.ids.contains(id)
    }

    /// The ids selected once `change` is made, each once, in the order they were chosen.
    pub fn changed(&self, change: &Select) -> Vec<EntityId> {
        let mut ids = self.ids.clone();
        let add = |ids: &mut Vec<EntityId>, id: &EntityId| {
            ids.retain(|kept| kept != id);
            ids.push(*id);
        };
        match change {
            Select::Set(chosen) => {
                ids.clear();
                for id in chosen {
                    add(&mut ids, id);
                }
            }
            Select::Add(chosen) => {
                for id in chosen {
                    add(&mut ids, id);
                }
            }
            Select::Toggle(chosen) => {
                for id in chosen {
                    if ids.contains(id) {
                        ids.retain(|kept| kept != id);
                    } else {
                        ids.push(*id);
                    }
                }
            }
            Select::Remove(chosen) => ids.retain(|kept| !chosen.contains(kept)),
            Select::Clear => ids.clear(),
        }
        ids
    }
}

/// What the narrow phase of the picking finds of an entity whose box the ray hits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Picked {
    /// Its surface, at that distance along the ray.
    Hit(f32),
    Missed,
    /// Not known: its box is its surface.
    ByBox,
}

/// The narrow phase of the picking of a provider's entities, from the thread of `scene` or any
/// other: the triangles of a model at rest, the heights of a chunk.
pub trait Picker: Send + Sync {
    fn intersect(&self, entity: &EntityRef<'_>, ray: &Ray) -> Picked;
}

/// An entity a ray hit: the deepest, its distance, and the chain from the outermost of its parents
/// down to it.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: EntityId,
    pub distance: f32,
    pub chain: Vec<EntityId>,
}

/// A question to the thread of `scene`.
#[derive(Clone, Debug)]
pub enum Question {
    Pick(Ray),
    Within {
        seen: Seen,
        rect: [[f32; 2]; 2],
        kinds: Vec<Kind>,
    },
}

#[derive(Clone, Debug)]
pub enum Answer {
    Picked(Option<Hit>),
    Within(Vec<EntityId>),
}

/// The kinds of entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Map,
    Area,
    Chunk,
    Water,
    Doodad,
    Building,
    Room,
    BuildingDoodad,
    Creature,
    GameObject,
    Live,
    Light,
    LightZone,
    Weather,
    Group,
    Prefab,
    Instance,
}

impl Kind {
    pub const ALL: [Kind; 17] = [
        Kind::Map,
        Kind::Area,
        Kind::Chunk,
        Kind::Water,
        Kind::Doodad,
        Kind::Building,
        Kind::Room,
        Kind::BuildingDoodad,
        Kind::Creature,
        Kind::GameObject,
        Kind::Live,
        Kind::Light,
        Kind::LightZone,
        Kind::Weather,
        Kind::Group,
        Kind::Prefab,
        Kind::Instance,
    ];

    /// Its name in an id written as text.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Map => "map",
            Kind::Area => "area",
            Kind::Chunk => "chunk",
            Kind::Water => "water",
            Kind::Doodad => "doodad",
            Kind::Building => "building",
            Kind::Room => "room",
            Kind::BuildingDoodad => "building-doodad",
            Kind::Creature => "creature",
            Kind::GameObject => "gameobject",
            Kind::Live => "live",
            Kind::Light => "light",
            Kind::LightZone => "light-zone",
            Kind::Weather => "weather",
            Kind::Group => "group",
            Kind::Prefab => "prefab",
            Kind::Instance => "instance",
        }
    }

    pub fn parse(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// Whether its ids name a map.
    fn has_map(self) -> bool {
        matches!(
            self,
            Kind::Map
                | Kind::Chunk
                | Kind::Water
                | Kind::Doodad
                | Kind::Building
                | Kind::Room
                | Kind::BuildingDoodad
                | Kind::LightZone
        )
    }
}

/// The mark of a doodad listed again under its unique id with another model or place: the tile
/// listing it, in `EntityId::sub`.
const AGAIN: u32 = 0x8000_0000;

/// The stable id of an entity: its kind, its map where its key is the map's own, its key and a
/// second number for the kinds that need one. Written as text in JSON and in the project:
/// `chunk:0:32_48:3_15`, `doodad:0:183214`, `room:0:4128:17`, `creature:79753`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityId {
    pub kind: Kind,
    pub map: u32,
    pub key: u64,
    pub sub: u32,
}

impl EntityId {
    const fn new(kind: Kind, map: u32, key: u64, sub: u32) -> Self {
        Self { kind, map, key, sub }
    }

    pub const fn map(id: u32) -> Self {
        Self::new(Kind::Map, id, 0, 0)
    }

    pub const fn area(id: u32) -> Self {
        Self::new(Kind::Area, 0, id as u64, 0)
    }

    /// The chunk of index `chunk` (along Y, then X, as its header gives it) of the tile `tile`.
    pub const fn chunk(map: u32, tile: TileId, chunk: [u32; 2]) -> Self {
        Self::new(Kind::Chunk, map, chunk_key(tile, chunk), 0)
    }

    /// The layer `layer` of liquid of a chunk.
    pub const fn water(map: u32, tile: TileId, chunk: [u32; 2], layer: u32) -> Self {
        Self::new(Kind::Water, map, chunk_key(tile, chunk), layer)
    }

    pub const fn doodad(map: u32, unique_id: u32) -> Self {
        Self::new(Kind::Doodad, map, unique_id as u64, 0)
    }

    /// A doodad the tile `tile` lists under `unique_id` with another model or place than its first
    /// listing (Kalimdor 39_23): an entity of its own.
    pub const fn doodad_again(map: u32, unique_id: u32, tile: TileId) -> Self {
        Self::new(Kind::Doodad, map, unique_id as u64, AGAIN | (tile.x << 8) | tile.y)
    }

    pub const fn building(map: u32, unique_id: u32) -> Self {
        Self::new(Kind::Building, map, unique_id as u64, 0)
    }

    /// The group of index `group` of the building `unique_id`.
    pub const fn room(map: u32, unique_id: u32, group: u32) -> Self {
        Self::new(Kind::Room, map, unique_id as u64, group)
    }

    /// The doodad at the place `place` of `MODD` of the building `unique_id`.
    pub const fn building_doodad(map: u32, unique_id: u32, place: u32) -> Self {
        Self::new(Kind::BuildingDoodad, map, unique_id as u64, place)
    }

    pub const fn creature(guid: u64) -> Self {
        Self::new(Kind::Creature, 0, guid, 0)
    }

    pub const fn game_object(guid: u64) -> Self {
        Self::new(Kind::GameObject, 0, guid, 0)
    }

    /// A live entity by its GUID at run time.
    pub const fn live(guid: u64) -> Self {
        Self::new(Kind::Live, 0, guid, 0)
    }

    pub const fn light(id: u32) -> Self {
        Self::new(Kind::Light, 0, id as u64, 0)
    }

    /// The zone of light `index` of a map, in the order of Wow.exe.
    pub const fn light_zone(map: u32, index: u32) -> Self {
        Self::new(Kind::LightZone, map, index as u64, 0)
    }

    /// The weather of the zone `zone`.
    pub const fn weather(zone: u32) -> Self {
        Self::new(Kind::Weather, 0, zone as u64, 0)
    }

    /// The tile and the chunk of a chunk or a water.
    pub fn chunk_place(&self) -> Option<(TileId, [u32; 2])> {
        matches!(self.kind, Kind::Chunk | Kind::Water).then(|| {
            let key = self.key as u32;
            (
                TileId {
                    x: key >> 24,
                    y: (key >> 16) & 0xFF,
                },
                [(key >> 8) & 0xFF, key & 0xFF],
            )
        })
    }

    /// The tile of a doodad listed again with other data.
    pub fn again(&self) -> Option<TileId> {
        (self.kind == Kind::Doodad && self.sub & AGAIN != 0).then_some(TileId {
            x: (self.sub >> 8) & 0xFF,
            y: self.sub & 0xFF,
        })
    }

    /// The number its default name ends with, where it has one.
    pub fn number(&self) -> u64 {
        match self.kind {
            Kind::Room | Kind::BuildingDoodad => u64::from(self.sub),
            _ => self.key,
        }
    }
}

const fn chunk_key(tile: TileId, chunk: [u32; 2]) -> u64 {
    ((tile.x << 24) | (tile.y << 16) | (chunk[0] << 8) | chunk[1]) as u64
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = self.kind.name();
        match self.kind {
            Kind::Map => write!(f, "{kind}:{}", self.map),
            Kind::Chunk | Kind::Water => {
                let (tile, [x, y]) = self.chunk_place().unwrap_or_default();
                write!(f, "{kind}:{}:{}_{}:{x}_{y}", self.map, tile.x, tile.y)?;
                if self.kind == Kind::Water {
                    write!(f, ":{}", self.sub)?;
                }
                Ok(())
            }
            Kind::Doodad => {
                write!(f, "{kind}:{}:{}", self.map, self.key)?;
                if let Some(tile) = self.again() {
                    write!(f, "@{}_{}", tile.x, tile.y)?;
                }
                Ok(())
            }
            Kind::Building => write!(f, "{kind}:{}:{}", self.map, self.key),
            Kind::Room | Kind::BuildingDoodad => write!(f, "{kind}:{}:{}:{}", self.map, self.key, self.sub),
            Kind::Live => write!(f, "{kind}:{:x}", self.key),
            Kind::LightZone => write!(f, "{kind}:{}:{}", self.map, self.key),
            _ => write!(f, "{kind}:{}", self.key),
        }
    }
}

impl std::str::FromStr for EntityId {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let wrong = || format!("'{text}' is not the id of an entity");
        let (kind, rest) = text.split_once(':').ok_or_else(wrong)?;
        let kind = Kind::parse(kind).ok_or_else(wrong)?;
        let parts: Vec<&str> = rest.split(':').collect();
        let number =
            |at: usize| -> Result<u64, String> { parts.get(at).ok_or_else(wrong)?.parse().map_err(|_| wrong()) };
        let pair = |at: usize| -> Result<[u32; 2], String> {
            let (a, b) = parts.get(at).ok_or_else(wrong)?.split_once('_').ok_or_else(wrong)?;
            let (a, b): (u32, u32) = (a.parse().map_err(|_| wrong())?, b.parse().map_err(|_| wrong())?);
            if a > 255 || b > 255 {
                return Err(wrong());
            }
            Ok([a, b])
        };
        let count = |expected: usize| if parts.len() == expected { Ok(()) } else { Err(wrong()) };
        let small = |value: u64| u32::try_from(value).map_err(|_| wrong());
        let id = match kind {
            Kind::Map => {
                count(1)?;
                EntityId::map(small(number(0)?)?)
            }
            Kind::Chunk | Kind::Water => {
                count(if kind == Kind::Chunk { 3 } else { 4 })?;
                let ([x, y], chunk) = (pair(1)?, pair(2)?);
                let (map, tile) = (small(number(0)?)?, TileId { x, y });
                if x > 63 || y > 63 || chunk[0] > 15 || chunk[1] > 15 {
                    return Err(wrong());
                }
                match kind {
                    Kind::Chunk => EntityId::chunk(map, tile, chunk),
                    _ => EntityId::water(map, tile, chunk, small(number(3)?)?),
                }
            }
            Kind::Doodad => {
                count(2)?;
                let map = small(number(0)?)?;
                match parts[1].split_once('@') {
                    Some((id, tile)) => {
                        let unique: u32 = id.parse().map_err(|_| wrong())?;
                        let (x, y) = tile.split_once('_').ok_or_else(wrong)?;
                        let (x, y): (u32, u32) = (x.parse().map_err(|_| wrong())?, y.parse().map_err(|_| wrong())?);
                        if x > 63 || y > 63 {
                            return Err(wrong());
                        }
                        EntityId::doodad_again(map, unique, TileId { x, y })
                    }
                    None => EntityId::doodad(map, small(number(1)?)?),
                }
            }
            Kind::Building => {
                count(2)?;
                EntityId::building(small(number(0)?)?, small(number(1)?)?)
            }
            Kind::Room | Kind::BuildingDoodad => {
                count(3)?;
                let (map, unique, sub) = (small(number(0)?)?, small(number(1)?)?, small(number(2)?)?);
                match kind {
                    Kind::Room => EntityId::room(map, unique, sub),
                    _ => EntityId::building_doodad(map, unique, sub),
                }
            }
            Kind::Live => {
                count(1)?;
                EntityId::live(u64::from_str_radix(parts[0], 16).map_err(|_| wrong())?)
            }
            Kind::LightZone => {
                count(2)?;
                EntityId::light_zone(small(number(0)?)?, small(number(1)?)?)
            }
            _ => {
                count(1)?;
                EntityId::new(kind, 0, number(0)?, 0)
            }
        };
        if !kind.has_map() && id.map != 0 {
            return Err(wrong());
        }
        Ok(id)
    }
}

/// The flags of an entity.
pub mod flags {
    /// Of the files of the client.
    pub const CLIENT: u16 = 0x1;
    /// Of the database of the server.
    pub const SERVER: u16 = 0x2;
    /// Of the world as it runs on the server.
    pub const LIVE: u16 = 0x4;
    /// Never changed by the editor.
    pub const READ_ONLY: u16 = 0x8;
    /// A player, connected with the client of the game.
    pub const PLAYER: u16 = 0x10;
}

/// Names a piece: its provider, its map and its part, a tile (`PieceKey::tile`) or a number of the
/// provider's own, such as a building's unique id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PieceKey {
    pub provider: String,
    pub map: u32,
    pub part: u64,
}

impl PieceKey {
    /// The piece of the tile `tile` of `provider`.
    pub fn tile(provider: &str, map: u32, tile: TileId) -> Self {
        Self {
            provider: provider.to_owned(),
            map,
            part: (1 << 32) | u64::from(tile.x << 8 | tile.y),
        }
    }

    /// A piece numbered `part` by `provider`, below 2³².
    pub fn part(provider: &str, map: u32, part: u32) -> Self {
        Self {
            provider: provider.to_owned(),
            map,
            part: u64::from(part),
        }
    }

    /// The tile of a piece of a tile.
    pub fn as_tile(&self) -> Option<TileId> {
        (self.part >> 32 == 1).then_some(TileId {
            x: (self.part as u32 >> 8) & 0xFF,
            y: self.part as u32 & 0xFF,
        })
    }
}

/// What a provider publishes of an entity, given to `PieceBuilder::push`.
#[derive(Clone, Debug)]
pub struct Entity<'a> {
    pub id: EntityId,
    pub flags: u16,
    /// The row, in the piece, of its game parent: a room's building, a building doodad's room or
    /// its building.
    pub parent: Option<u32>,
    /// The first part of its default name: the stem of its model, a template's name.
    pub label: &'a str,
    /// Its look, its template, its type of liquid: a number of its provider's.
    pub extra: u32,
    /// Its origin in the world.
    pub origin: [f32; 3],
    /// Its bounds in the world, the least corner then the greatest.
    pub bounds: [[f32; 3]; 2],
}

/// The entities of a piece, in columns: built whole by its provider, never changed after.
#[derive(Clone, Debug, Default)]
pub struct Piece {
    /// The tiles whose files store its entities, for the kinds stored in tiles.
    pub tiles: Box<[TileId]>,
    /// The labels of the piece, each once.
    pub labels: Box<[Arc<str>]>,
    pub ids: Box<[EntityId]>,
    pub flags: Box<[u16]>,
    /// The row of each one's game parent, `u32::MAX` for none.
    pub parents: Box<[u32]>,
    pub label: Box<[u32]>,
    pub extra: Box<[u32]>,
    pub origins: Box<[[f32; 3]]>,
    pub bounds: Box<[[[f32; 3]; 2]]>,
}

impl Piece {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn label_of(&self, row: usize) -> &str {
        &self.labels[self.label[row] as usize]
    }

    /// The default name of the entity at `row`, made when shown, never stored.
    pub fn name(&self, row: usize) -> String {
        default_name(&self.ids[row], self.label_of(row))
    }

    /// The bytes it takes, its labels counted once.
    pub fn bytes(&self) -> usize {
        self.len() * (size_of::<EntityId>() + 2 + 4 + 4 + 4 + 12 + 24)
            + self.labels.iter().map(|label| label.len() + 16).sum::<usize>()
            + self.tiles.len() * size_of::<TileId>()
    }
}

/// The default name of the entity `id` of the label `label`: a doodad or a building the stem of its
/// model and its unique id (`ElwynnTree03 #4521`), a building doodad the stem and its place in
/// `MODD`, a room its name or *Room 17*, a chunk *Chunk 3, 15*.
pub fn default_name(id: &EntityId, label: &str) -> String {
    match id.kind {
        Kind::Chunk => {
            let (_, [x, y]) = id.chunk_place().unwrap_or_default();
            format!("Chunk {x}, {y}")
        }
        Kind::Water if label.is_empty() => "Water".to_owned(),
        Kind::Room if label.is_empty() => format!("Room {}", id.sub),
        Kind::Doodad | Kind::Building | Kind::BuildingDoodad | Kind::Creature | Kind::GameObject => {
            format!("{label} #{}", id.number())
        }
        Kind::Light if label.is_empty() => format!("Light {}", id.key),
        _ if label.is_empty() => id.to_string(),
        _ => label.to_owned(),
    }
}

/// Builds a piece row by row, its labels interned.
#[derive(Default)]
pub struct PieceBuilder {
    piece: Piece,
    labels: Vec<Arc<str>>,
    interned: HashMap<Arc<str>, u32>,
    ids: Vec<EntityId>,
    flags: Vec<u16>,
    parents: Vec<u32>,
    label: Vec<u32>,
    extra: Vec<u32>,
    origins: Vec<[f32; 3]>,
    bounds: Vec<[[f32; 3]; 2]>,
}

impl PieceBuilder {
    pub fn new(tiles: Vec<TileId>) -> Self {
        Self {
            piece: Piece {
                tiles: tiles.into(),
                ..Piece::default()
            },
            ..Self::default()
        }
    }

    pub fn with_capacity(tiles: Vec<TileId>, rows: usize) -> Self {
        let mut builder = Self::new(tiles);
        builder.ids.reserve(rows);
        builder.flags.reserve(rows);
        builder.parents.reserve(rows);
        builder.label.reserve(rows);
        builder.extra.reserve(rows);
        builder.origins.reserve(rows);
        builder.bounds.reserve(rows);
        builder
    }

    /// Adds `entity`; its row.
    pub fn push(&mut self, entity: Entity) -> u32 {
        let label = match self.interned.get(entity.label) {
            Some(label) => *label,
            None => {
                let text: Arc<str> = entity.label.into();
                let index = self.labels.len() as u32;
                self.labels.push(text.clone());
                self.interned.insert(text, index);
                index
            }
        };
        let row = self.ids.len() as u32;
        self.ids.push(entity.id);
        self.flags.push(entity.flags);
        self.parents.push(entity.parent.unwrap_or(u32::MAX));
        self.label.push(label);
        self.extra.push(entity.extra);
        self.origins.push(entity.origin);
        self.bounds.push(entity.bounds);
        row
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn build(self) -> Piece {
        Piece {
            labels: self.labels.into(),
            ids: self.ids.into(),
            flags: self.flags.into(),
            parents: self.parents.into(),
            label: self.label.into(),
            extra: self.extra.into(),
            origins: self.origins.into(),
            bounds: self.bounds.into(),
            ..self.piece
        }
    }
}

/// A box of `half` yards around `origin`, for an entity whose bounds are not known yet.
pub fn box_around(origin: [f32; 3], half: f32) -> [[f32; 3]; 2] {
    [origin.map(|v| v - half), origin.map(|v| v + half)]
}

/// The stem of the file `file`: its name without its folder or its extension; a FileDataID as
/// `File 123`.
pub fn stem(file: &FileRef) -> String {
    match file {
        FileRef::Path(path) => {
            let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
            name.rsplit_once('.').map_or(name, |(stem, _)| stem).to_owned()
        }
        FileRef::Id(id) => format!("File {id}"),
    }
}

/// The tile and the chunk, by its index along Y then X as the headers of the chunks give it, of the
/// point `x`, `y` of the world; none off the 64 × 64 tiles.
pub fn chunk_at(x: f32, y: f32) -> Option<(TileId, [u32; 2])> {
    const ORIGIN: f32 = 32.0 * 1600.0 / 3.0;
    const CHUNK: f32 = 1600.0 / 3.0 / 16.0;
    let across = ((ORIGIN - y) / CHUNK).floor();
    let down = ((ORIGIN - x) / CHUNK).floor();
    if !(0.0..1024.0).contains(&across) || !(0.0..1024.0).contains(&down) {
        return None;
    }
    let (across, down) = (across as u32, down as u32);
    Some((
        TileId {
            x: across / 16,
            y: down / 16,
        },
        [across % 16, down % 16],
    ))
}

/// An area of `AreaTable.dbc` as the registry keeps it: its parent, 0 for a zone, its map and its
/// name in the client's locale.
#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub parent: u32,
    pub map: u32,
    pub name: String,
}

/// A piece in the registry: as its provider published it, with the generation of the registry it
/// came in, and the area of each entity, that of the chunk under its origin, 0 for none.
#[derive(Debug)]
pub struct Placed {
    pub key: PieceKey,
    pub generation: u64,
    pub piece: Arc<Piece>,
    pub areas: Box<[u32]>,
    /// The bounds of all its entities, for the broad phase of the picking.
    pub bounds: [[f32; 3]; 2],
}

/// The bounds of every entity of `piece`; a box of nothing at the origin for none.
pub fn piece_bounds(piece: &Piece) -> [[f32; 3]; 2] {
    if piece.is_empty() {
        return [[0.0; 3]; 2];
    }
    piece
        .bounds
        .iter()
        .fold([[f32::MAX; 3], [f32::MIN; 3]], |[low, high], [a, b]| {
            [
                [low[0].min(a[0]), low[1].min(a[1]), low[2].min(a[2])],
                [high[0].max(b[0]), high[1].max(b[1]), high[2].max(b[2])],
            ]
        })
}

/// Where an entity is listed: the slot of its piece and its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Listing {
    pub slot: u32,
    pub row: u32,
}

/// The stable ids of the registry and where each is listed, the first its entity, the others
/// listings of the same entity by other tiles.
pub type Index = HashMap<EntityId, Vec<Listing>>;

/// The area of each chunk of the tiles read, by map and tile.
pub type TileAreas = HashMap<(u32, TileId), Arc<[u32]>>;

/// The registry as a job of `scene` left it, shared between threads, never changed after.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub generation: u64,
    /// The pieces by slot, none where a slot is free.
    pub slots: Arc<Vec<Option<Arc<Placed>>>>,
    /// The slot of each piece.
    pub keys: Arc<BTreeMap<PieceKey, u32>>,
    pub index: Arc<Index>,
    /// The areas of `AreaTable.dbc`, by id.
    pub areas: Arc<HashMap<u32, Area>>,
    /// The area of each chunk of the tiles read, by map and tile, at `y * 16 + x` of its index.
    pub tiles: Arc<TileAreas>,
    /// The entities listed, each once.
    pub entities: usize,
}

/// An entity read from a snapshot.
#[derive(Clone, Copy)]
pub struct EntityRef<'a> {
    pub snapshot: &'a Snapshot,
    pub placed: &'a Placed,
    pub row: usize,
}

impl<'a> EntityRef<'a> {
    pub fn id(&self) -> EntityId {
        self.placed.piece.ids[self.row]
    }

    pub fn name(&self) -> String {
        self.placed.piece.name(self.row)
    }

    pub fn label(&self) -> &'a str {
        self.placed.piece.label_of(self.row)
    }

    pub fn flags(&self) -> u16 {
        self.placed.piece.flags[self.row]
    }

    pub fn extra(&self) -> u32 {
        self.placed.piece.extra[self.row]
    }

    pub fn origin(&self) -> [f32; 3] {
        self.placed.piece.origins[self.row]
    }

    pub fn bounds(&self) -> [[f32; 3]; 2] {
        self.placed.piece.bounds[self.row]
    }

    /// Its area, that of the chunk under its origin; 0 for none.
    pub fn area(&self) -> u32 {
        self.placed.areas.get(self.row).copied().unwrap_or(0)
    }

    pub fn parent(&self) -> Option<EntityId> {
        let parent = self.placed.piece.parents[self.row];
        (parent != u32::MAX).then(|| self.placed.piece.ids[parent as usize])
    }

    /// The tiles whose files store it: those of every piece listing it.
    pub fn tiles(&self) -> Vec<TileId> {
        let mut tiles: Vec<TileId> = self
            .snapshot
            .listings(&self.id())
            .iter()
            .filter_map(|listing| self.snapshot.placed(listing.slot))
            .flat_map(|placed| placed.piece.tiles.iter().copied())
            .collect();
        tiles.sort();
        tiles.dedup();
        tiles
    }

    /// The provider that published it.
    pub fn provider(&self) -> &'a str {
        &self.placed.key.provider
    }
}

impl Snapshot {
    pub fn placed(&self, slot: u32) -> Option<&Placed> {
        self.slots.get(slot as usize)?.as_deref()
    }

    /// Where `id` is listed, its entity first.
    pub fn listings(&self, id: &EntityId) -> &[Listing] {
        self.index.get(id).map_or(&[], Vec::as_slice)
    }

    /// The entity `id`.
    pub fn get(&self, id: &EntityId) -> Option<EntityRef<'_>> {
        let listing = self.listings(id).first()?;
        Some(EntityRef {
            snapshot: self,
            placed: self.placed(listing.slot)?,
            row: listing.row as usize,
        })
    }

    /// Whether the row `row` of the piece in `slot` is its entity, not a listing of it by another
    /// tile.
    pub fn is_first(&self, slot: u32, row: u32, id: &EntityId) -> bool {
        self.listings(id).first() == Some(&Listing { slot, row })
    }

    /// Every entity, each once, piece by piece in the order of their keys.
    pub fn entities(&self) -> impl Iterator<Item = EntityRef<'_>> {
        self.keys
            .values()
            .filter_map(|slot| Some((*slot, self.placed(*slot)?)))
            .flat_map(move |(slot, placed)| {
                (0..placed.piece.len()).filter_map(move |row| {
                    let id = placed.piece.ids[row];
                    self.is_first(slot, row as u32, &id).then_some(EntityRef {
                        snapshot: self,
                        placed,
                        row,
                    })
                })
            })
    }

    /// The area of the chunk at `x`, `y` of the map `map`, among the tiles read; 0 for none.
    pub fn area_at(&self, map: u32, x: f32, y: f32) -> u32 {
        let Some((tile, [across, down])) = chunk_at(x, y) else {
            return 0;
        };
        self.tiles
            .get(&(map, tile))
            .map_or(0, |areas| areas[(down * 16 + across) as usize])
    }

    /// The areas of the chunks `bounds` reach on the map `map`, among the tiles read, each once, in
    /// their order of increasing id; 0 left out.
    pub fn areas_within(&self, map: u32, bounds: [[f32; 3]; 2]) -> Vec<u32> {
        const CHUNK: f32 = 1600.0 / 3.0 / 16.0;
        let [low, high] = bounds;
        let mut found = Vec::new();
        // Some 64 chunks a side at most: a building of a hundred yards reaches six.
        let steps = |from: f32, to: f32| ((to - from) / CHUNK).ceil().clamp(0.0, 64.0) as usize;
        for i in 0..=steps(low[0], high[0]) {
            for j in 0..=steps(low[1], high[1]) {
                let x = (low[0] + i as f32 * CHUNK).min(high[0]);
                let y = (low[1] + j as f32 * CHUNK).min(high[1]);
                let area = self.area_at(map, x, y);
                if area != 0 {
                    found.push(area);
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found
    }

    /// The chain of the area `area`, from its zone down to it; empty for none.
    pub fn place(&self, area: u32) -> Vec<(u32, &str)> {
        let mut chain = Vec::new();
        let mut at = area;
        while at != 0 && chain.len() < 8 {
            let Some(record) = self.areas.get(&at) else {
                break;
            };
            chain.push((at, record.name.as_str()));
            at = record.parent;
        }
        chain.reverse();
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ids_are_written_and_read_back() {
        let tile = TileId { x: 32, y: 48 };
        let ids = [
            (EntityId::map(571), "map:571"),
            (EntityId::area(4395), "area:4395"),
            (EntityId::chunk(0, tile, [3, 15]), "chunk:0:32_48:3_15"),
            (EntityId::water(0, tile, [3, 15], 0), "water:0:32_48:3_15:0"),
            (EntityId::doodad(0, 183214), "doodad:0:183214"),
            (
                EntityId::doodad_again(1, 360868, TileId { x: 39, y: 23 }),
                "doodad:1:360868@39_23",
            ),
            (EntityId::building(0, 4128), "building:0:4128"),
            (EntityId::room(0, 4128, 17), "room:0:4128:17"),
            (EntityId::building_doodad(0, 4128, 256), "building-doodad:0:4128:256"),
            (EntityId::creature(79753), "creature:79753"),
            (EntityId::game_object(5714), "gameobject:5714"),
            (EntityId::live(0xf13000046b0012e3), "live:f13000046b0012e3"),
            (EntityId::light(1203), "light:1203"),
            (EntityId::light_zone(571, 4), "light-zone:571:4"),
            (EntityId::weather(1519), "weather:1519"),
        ];
        for (id, text) in ids {
            assert_eq!(id.to_string(), text);
            assert_eq!(text.parse::<EntityId>(), Ok(id), "{text}");
        }
        assert_eq!(EntityId::chunk(0, tile, [3, 15]).chunk_place(), Some((tile, [3, 15])));
        for wrong in [
            "",
            "tree:1",
            "chunk:0:64_1:0_0",
            "chunk:0:1_1:16_0",
            "doodad:0",
            "doodad:0:1:2",
            "creature:1:2",
            "map:x",
            "room:0:1",
            "doodad:0:4294967296",
        ] {
            assert!(wrong.parse::<EntityId>().is_err(), "{wrong}");
        }
    }

    #[test]
    fn the_default_names_are_made_from_the_label_and_the_number() {
        let tile = TileId { x: 32, y: 48 };
        assert_eq!(
            default_name(&EntityId::doodad(0, 4521), "ElwynnTree03"),
            "ElwynnTree03 #4521"
        );
        assert_eq!(default_name(&EntityId::chunk(0, tile, [3, 15]), ""), "Chunk 3, 15");
        assert_eq!(default_name(&EntityId::room(0, 1, 17), ""), "Room 17");
        assert_eq!(default_name(&EntityId::room(0, 1, 17), "The Inn"), "The Inn");
        assert_eq!(
            default_name(&EntityId::building_doodad(0, 1, 256), "Chair01"),
            "Chair01 #256"
        );
        assert_eq!(
            stem(&FileRef::Path("World\\Azeroth\\Elwynn\\ElwynnTree03.m2".into())),
            "ElwynnTree03"
        );
        assert_eq!(stem(&FileRef::Id(7)), "File 7");
    }

    #[test]
    fn a_point_of_the_world_falls_in_its_tile_and_chunk() {
        const TILE: f32 = 1600.0 / 3.0;
        let tile = TileId { x: 30, y: 21 };
        let [x, y] = tile.centre();
        assert_eq!(chunk_at(x, y), Some((tile, [8, 8])));
        // The corner of the highest X and Y of a tile is its chunk 0, 0.
        let corner = [32.0 * TILE - TILE * 21.0 - 0.01, 32.0 * TILE - TILE * 30.0 - 0.01];
        assert_eq!(chunk_at(corner[0], corner[1]), Some((tile, [0, 0])));
        assert_eq!(chunk_at(40000.0, 0.0), None);
    }

    #[test]
    fn a_selection_changes_by_set_add_toggle_remove_and_clear_the_last_chosen_active() {
        let [a, b, c] = [1, 2, 3].map(|n| EntityId::doodad(0, n));
        let selection = |ids: Vec<EntityId>| Selection { generation: 1, ids };
        let one = selection(vec![a, b]);
        assert_eq!(one.changed(&Select::Set(vec![c])), [c]);
        assert_eq!(one.changed(&Select::Add(vec![a, c])), [b, a, c]);
        assert_eq!(one.changed(&Select::Toggle(vec![a, c])), [b, c]);
        assert_eq!(one.changed(&Select::Remove(vec![b])), [a]);
        assert!(one.changed(&Select::Clear).is_empty());
        assert_eq!(one.active(), Some(b));
    }

    #[test]
    fn a_piece_interns_its_labels_and_keeps_its_parents() {
        let mut builder = PieceBuilder::new(vec![TileId { x: 1, y: 2 }]);
        let entity = |id, parent, label| Entity {
            id,
            flags: flags::CLIENT,
            parent,
            label,
            extra: 0,
            origin: [0.0; 3],
            bounds: box_around([0.0; 3], 1.0),
        };
        let building = builder.push(entity(EntityId::building(0, 9), None, "Inn"));
        builder.push(entity(EntityId::room(0, 9, 0), Some(building), "Kitchen"));
        builder.push(entity(EntityId::building_doodad(0, 9, 3), Some(building), "Inn"));
        let piece = builder.build();
        assert_eq!(piece.labels.len(), 2);
        assert_eq!(piece.name(2), "Inn #3");
        assert_eq!(piece.parents[1], 0);
        assert_eq!(
            PieceKey::tile("doodads", 0, TileId { x: 1, y: 2 }).as_tile(),
            Some(TileId { x: 1, y: 2 })
        );
        assert_eq!(PieceKey::part("buildings", 0, 9).as_tile(), None);
    }
}
