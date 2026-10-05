//! The contract of the formats of the client's files, offered by the module `assets`: their content
//! parsed, from any thread at once (T3): the rows of the tables of 3.3.5a the next steps need, the
//! texts in the client's locale; the terrain of a map; the textures.
//!
//! World coordinates, as the client's: X towards the north, Y towards the west, Z up, in yards;
//! a map is 64 × 64 tiles of 533⅓ yards, the tile `<map>_<x>_<y>` reaching from
//! `17066⅔ − 533⅓ x` down in Y and from `17066⅔ − 533⅓ y` down in X.

use std::sync::Arc;

use crate::ServiceKey;

/// A map of `Map.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct MapRecord {
    pub id: u32,
    /// The folder of its files, below `World\Maps`.
    pub directory: String,
    /// 0 a continent, 1 a dungeon, 2 a raid, 3 a battleground, 4 an arena.
    pub instance_type: u32,
    pub name: String,
    /// Its area of `AreaTable.dbc`.
    pub area: u32,
}

/// An area of `AreaTable.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct AreaRecord {
    pub id: u32,
    pub map: u32,
    /// The area it lies in, 0 for a zone.
    pub parent: u32,
    pub flags: u32,
    pub name: String,
}

/// What a creature looks like, `CreatureDisplayInfo.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct CreatureDisplay {
    pub id: u32,
    /// Its model, of `CreatureModelData.dbc`.
    pub model: u32,
    pub scale: f32,
    /// Its skins, the textures replacing those of its model, by name without folder nor extension.
    pub textures: [String; 3],
    /// Its looks of a character, of `CreatureDisplayInfoExtra.dbc`, 0 for none.
    pub extra: u32,
}

/// A model of `CreatureModelData.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct CreatureModel {
    pub id: u32,
    pub flags: u32,
    /// Its file, as the table names it.
    pub path: String,
    pub scale: f32,
}

/// A file a tile names: by its path, or, for a modern one, by its FileDataID, which
/// `vfs::Vfs::path_of` turns into a path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FileRef {
    Path(String),
    Id(u32),
}

/// The tiles of a map, from its WDT.
#[derive(Clone, Debug, PartialEq)]
pub struct Wdt {
    /// The flags of `MPHD`: 0x1 a map made of one building, 0x2 vertex colours in its tiles, 0x4
    /// alpha maps of 8 bits.
    pub flags: u32,
    /// Whether the tile `<map>_<x>_<y>` exists, at `y * 64 + x`.
    pub tiles: Vec<bool>,
}

/// A terrain tile: its 16 × 16 chunks, row by row, the textures its chunks name, and the doodads
/// and buildings placed on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub chunks: Vec<Chunk>,
    pub textures: Vec<FileRef>,
    pub doodads: Vec<Doodad>,
    pub buildings: Vec<Building>,
}

/// A chunk of terrain, 33⅓ yards square, and its 145 vertices: 17 rows, of 9 outer vertices and of
/// 8 inner ones in turn. A row goes down in Y, the rows go down in X, 4⅙ yards apart.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    /// Its column and row in the tile.
    pub index: [u32; 2],
    pub flags: u32,
    /// Its corner of highest X and Y, in world coordinates, as the file gives it. The client places
    /// a chunk by its tile and its index, which some tiles made by tools contradict, and takes
    /// only the base of its heights from here.
    pub position: [f32; 3],
    /// The height of each vertex over `position[2]`.
    pub heights: Vec<f32>,
    /// The normal of each vertex in world axes, 127 for 1.
    pub normals: Vec<[i8; 3]>,
    /// The colour of each vertex, red, green, blue and alpha, 127 for 1; empty when it has none.
    pub colours: Vec<[u8; 4]>,
    /// Its area of `AreaTable.dbc`.
    pub area: u32,
    /// A bit per quad of its 8 × 8, row by row from bit 0, set for a hole.
    pub holes: u64,
    pub layers: Vec<Layer>,
    /// The alpha map of each layer after the first: 64 × 64 bytes, row by row.
    pub alphas: Vec<Vec<u8>>,
    /// The shadow baked in it: 64 × 64 bytes, 255 in shadow; empty when it has none.
    pub shadow: Vec<u8>,
    /// What stands on the chunk: indices into `Tile::doodads` and `Tile::buildings`.
    pub doodad_refs: Vec<u32>,
    pub building_refs: Vec<u32>,
}

/// A texture layer of a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layer {
    /// Its index into `Tile::textures`.
    pub texture: u32,
    pub flags: u32,
    /// Its ground effect, of `GroundEffectTexture.dbc`.
    pub effect: u32,
}

/// A doodad placed on a tile. Its position and rotation are as the file has them, in its own axes:
/// world = (17066⅔ − z, 17066⅔ − x, y); the rotation in degrees about those axes.
#[derive(Clone, Debug, PartialEq)]
pub struct Doodad {
    pub file: FileRef,
    pub unique_id: u32,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: f32,
    pub flags: u16,
}

/// A building placed on a tile, in the axes of `Doodad`, with its bounds in them.
#[derive(Clone, Debug, PartialEq)]
pub struct Building {
    pub file: FileRef,
    pub unique_id: u32,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub bounds: [[f32; 3]; 2],
    pub scale: f32,
    pub flags: u16,
    pub doodad_set: u16,
    pub name_set: u16,
}

/// How the levels of a texture are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormat {
    /// Red, green, blue and alpha, a byte each.
    Rgba8,
    /// The block compressions DXT1, DXT3 and DXT5.
    Bc1,
    Bc2,
    Bc3,
}

/// A texture: its size, and its levels from the largest, each half the one before.
#[derive(Clone, Debug, PartialEq)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub levels: Vec<Vec<u8>>,
}

/// The formats of the client's files, offered by the module `assets`. Each table is read once, by
/// the first thread asking for it; the others asking meanwhile wait for it: ask from a job. In
/// debug, a format asked from the interface thread is said once in the log.
pub trait Formats: Send + Sync {
    /// The maps, by increasing id.
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String>;
    /// The areas, by increasing id.
    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String>;
    /// The looks of creatures, by increasing id.
    fn creature_displays(&self) -> Result<Arc<Vec<CreatureDisplay>>, String>;
    /// The models of creatures, by increasing id.
    fn creature_models(&self) -> Result<Arc<Vec<CreatureModel>>, String>;
    /// The WDT of the map whose folder, below `World\Maps`, is `directory`, read once.
    fn wdt(&self, directory: &str) -> Result<Arc<Wdt>, String>;
    /// The tile `<directory>_<x>_<y>`, of 3.3.5a, or split in a root, a `_tex0` and an `_obj0` as
    /// WarcraftXL loads it; none when the WDT of its map has no such tile.
    fn tile(&self, directory: &str, x: u32, y: u32) -> Result<Option<Tile>, String>;
    /// The BLP `file`, its levels as they are stored: DXT kept, the others as `Rgba8`.
    fn texture(&self, file: &FileRef) -> Result<Texture, String>;
    /// The BLP `file`, every level as `Rgba8`.
    fn texture_rgba(&self, file: &FileRef) -> Result<Texture, String>;
}

/// The service of the formats of the client's files.
pub const SERVICE: ServiceKey<Arc<dyn Formats>> = ServiceKey::new("formats");
