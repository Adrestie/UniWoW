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
    /// Its opacity, 0 to 255.
    pub alpha: u32,
    /// Its skins, the textures replacing those of its model, by name without folder nor extension.
    pub textures: [String; 3],
    /// Its looks of a character, of `CreatureDisplayInfoExtra.dbc`, 0 for none.
    pub extra: u32,
    /// The variants of its model it shows, a nibble a group (`creature_geosets`), 0 for none.
    pub geosets: u32,
}

/// The look of a character a creature has, `CreatureDisplayInfoExtra.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct CreatureLook {
    pub id: u32,
    pub race: u32,
    pub sex: u32,
    pub skin: u32,
    pub face: u32,
    pub hair_style: u32,
    pub hair_colour: u32,
    pub facial_hair: u32,
    /// What it wears, of `ItemDisplayInfo.dbc`: helm, shoulders, shirt, chest, belt, legs, boots,
    /// wrists, gloves, tabard, cape.
    pub items: [u32; 11],
    pub flags: u32,
    /// Its skin baked into one texture, by name without folder nor extension.
    pub baked: String,
}

/// The hair of a style, `CharHairGeosets.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct HairGeoset {
    pub race: u32,
    pub sex: u32,
    pub variation: u32,
    /// Its submesh, of the group 0; 0 for none.
    pub geoset: u32,
    pub scalp: bool,
}

/// The facial hair of a style, `CharacterFacialHairStyles.dbc`: its variants of the groups 100,
/// 300, 200, 1600 and 1700, 0 for none.
#[derive(Clone, Debug, PartialEq)]
pub struct FacialHair {
    pub race: u32,
    pub sex: u32,
    pub variation: u32,
    pub geosets: [u32; 5],
}

/// What a game object looks like, `GameObjectDisplayInfo.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct GameObjectDisplay {
    pub id: u32,
    /// Its model, as the table names it: an M2 (`.mdx`, `.mdl`) or a WMO.
    pub path: String,
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

/// The heights of a map at low resolution, from its WDL, which the client draws as the horizon
/// beyond the tiles it has loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Wdl {
    /// The heights of the tile `<map>_<x>_<y>`, at `y * 64 + x`, none where the WDL has none: 17 × 17,
    /// row by row as the vertices of a chunk, a row going down in Y and the rows down in X, from the
    /// corner of highest X and Y of the tile; 32 yards and a third apart.
    pub tiles: Vec<Option<Vec<i16>>>,
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

/// A vertex of a model, at rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [[f32; 2]; 2],
    pub bone_weights: [u8; 4],
    pub bone_indices: [u8; 4],
}

/// Where a texture of a model comes from: its file, or the display that fills it (11 to 13 the
/// skins of a creature, 1 the skin of a character, 2 a cape, 6 the hair...); some models of the
/// client name no file for a texture of their own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelTextureSource {
    File(FileRef),
    Filled(u32),
    Unnamed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelTexture {
    pub source: ModelTextureSource,
    /// 0x1 wrapped across, 0x2 wrapped up and down; clamped otherwise.
    pub flags: u32,
}

/// How a batch draws: its render flags (0x01 unlit, 0x02 unfogged, 0x04 two-sided, 0x08 without
/// depth test, 0x10 without depth write) and its blending (0 opaque, 1 alpha key, 2 alpha, 3 add
/// without alpha, 4 add, 5 mod, 6 mod2x, 7 blend add).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Material {
    pub flags: u16,
    pub blending: u16,
}

/// A submesh of a skin: its id (its group by hundreds, its variant within), and its triangles, a
/// range of the triangles of its skin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Submesh {
    pub id: u16,
    pub start: u32,
    pub count: u32,
    pub centre: [f32; 3],
    pub radius: f32,
}

/// A batch of a skin: a submesh drawn with a material and its textures, as the `.skin` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Batch {
    pub flags: u8,
    pub priority: i8,
    /// Its shader: when the model has combiners (its flag 0x08) and this is under 0x8000, where
    /// the combiners of its textures start in `Model::combiner_combos`; a shader already chosen,
    /// as a later client does, from 0x8000.
    pub shader: u16,
    pub submesh: u16,
    /// Its colour and transparency, of `Model::colours`; none for 0xFFFF.
    pub colour: Option<u16>,
    pub material: u16,
    pub layer: u16,
    /// Its textures: `texture_count` from `texture_combo` in `Model::texture_combos`; their
    /// coordinates from `uv_combo` in `Model::uv_combos`: 1 the second set, 0xFFFF the
    /// environment, any other value the first, as the client's choice of shader reads them; chosen
    /// by its shader when the model has none, as since Cataclysm. Its weight at `weight_combo` in
    /// `Model::weight_combos`; its transforms from `transform_combo` in `Model::transform_combos`.
    /// Every one of them is there, for each of its textures.
    pub texture_count: u16,
    pub texture_combo: u16,
    pub uv_combo: u16,
    pub weight_combo: u16,
    pub transform_combo: u16,
}

/// A level of detail of a model, of its `.skin` file.
#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    /// Three vertices of the model a triangle.
    pub triangles: Vec<u32>,
    pub submeshes: Vec<Submesh>,
    pub batches: Vec<Batch>,
}

/// An M2 model at rest, of 3.3.5a or modern; its bones and animations come with step 9.5.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub version: u32,
    pub flags: u32,
    pub vertices: Vec<ModelVertex>,
    pub textures: Vec<ModelTexture>,
    pub materials: Vec<Material>,
    /// The lookups a batch indexes: textures, their coordinates, weights and transforms.
    pub texture_combos: Vec<u16>,
    pub uv_combos: Vec<u16>,
    pub weight_combos: Vec<u16>,
    pub transform_combos: Vec<u16>,
    /// The combiners of two textures, when the flag 0x08 says the model has them.
    pub combiner_combos: Vec<u16>,
    /// The colours, red, green, blue and alpha, and the weights of the textures, at rest.
    pub colours: Vec<[f32; 4]>,
    pub weights: Vec<f32>,
    pub bounds: [[f32; 3]; 2],
    pub radius: f32,
    /// Its levels of detail, the finest first.
    pub skins: Vec<Skin>,
    /// What was left out of it, and why: its batches referring to what it does not have, its
    /// skins from the first that is missing or does not hold together.
    pub faults: Vec<String>,
}

/// Which of the submeshes `ids` a creature draws whose display has `geosets`
/// (`CreatureDisplayInfo.CreatureGeosetData`): every one when 0; else none of 1 to 899 but, for
/// the nibble `n` from the lowest of value `v`, the submesh (`n` + 1) × 100 + `v` (the client's
/// `ApplyMonsterGeosets`, as wowdev describes it).
pub fn creature_geosets(ids: &[u16], geosets: u32) -> Vec<bool> {
    if geosets == 0 {
        return vec![true; ids.len()];
    }
    let chosen: Vec<u32> = (0..8)
        .map(|nibble| (nibble + 1) * 100 + ((geosets >> (4 * nibble)) & 0xF))
        .collect();
    ids.iter()
        .map(|id| {
            let id = u32::from(*id);
            !(1..900).contains(&id) || chosen.contains(&id)
        })
        .collect()
}

/// Which of the submeshes `ids` the look of a character draws, without its items, as WoW Model
/// Viewer dresses it: the body (0); its hair, the submesh `hair` (of `CharHairGeosets`) gives in
/// the group 0, or the scalp (1) when it gives none; its facial hair, the five values of `facial`
/// (of `CharacterFacialHairStyles`) the variants of the groups 100, 300, 200, 1600 and 1700, 0
/// for none; its ears (702); every other group at its first variant (x01): bare hands, feet and
/// legs.
pub fn look_geosets(ids: &[u16], hair: Option<&HairGeoset>, facial: Option<&FacialHair>) -> Vec<bool> {
    let hair = hair.map_or(1, |hair| hair.geoset.max(1));
    let facial = facial.map_or([0; 5], |facial| facial.geosets);
    ids.iter()
        .map(|id| {
            let (group, variant) = (u32::from(id / 100), u32::from(id % 100));
            match group {
                _ if *id == 0 => true,
                0 => variant == hair,
                1 => variant == facial[0],
                3 => variant == facial[1],
                2 => variant == facial[2],
                16 => variant == facial[3],
                17 => variant == facial[4],
                7 => variant == 2,
                _ => variant == 1,
            }
        })
        .collect()
}

/// Which of the submeshes `ids` a model draws when nothing chooses: the submesh 0, the group 32xx,
/// and the first variant (x01) of every other group but the eye glows (17xx) and the 35xx, as
/// wow.export chooses them.
pub fn default_geosets(ids: &[u16]) -> Vec<bool> {
    ids.iter()
        .map(|id| {
            let group = id / 100;
            *id == 0 || group == 32 || (*id > 100 && id % 100 == 1 && group != 17 && group != 35)
        })
        .collect()
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
    /// The looks of characters of creatures, by increasing id.
    fn creature_looks(&self) -> Result<Arc<Vec<CreatureLook>>, String>;
    /// The hairs of the styles of characters, by race, sex and variation.
    fn hair_geosets(&self) -> Result<Arc<Vec<HairGeoset>>, String>;
    /// The facial hairs of characters, by race, sex and variation.
    fn facial_hairs(&self) -> Result<Arc<Vec<FacialHair>>, String>;
    /// The looks of game objects, by increasing id.
    fn game_object_displays(&self) -> Result<Arc<Vec<GameObjectDisplay>>, String>;
    /// The M2 `file`, with its skins: of 3.3.5a, its path as a table names it (`.mdx` and `.mdl`
    /// read as `.m2`) and its skins beside it; modern, its skins named by its chunk `SFID`.
    fn model(&self, file: &FileRef) -> Result<Model, String>;
    /// The WDT of the map whose folder, below `World\Maps`, is `directory`, read once.
    fn wdt(&self, directory: &str) -> Result<Arc<Wdt>, String>;
    /// The tile `<directory>_<x>_<y>`, of 3.3.5a, or split in a root, a `_tex0` and an `_obj0` as
    /// WarcraftXL loads it; none when the WDT of its map has no such tile.
    fn tile(&self, directory: &str, x: u32, y: u32) -> Result<Option<Tile>, String>;
    /// The WDL of the map whose folder is `directory`; none when the client has none.
    fn wdl(&self, directory: &str) -> Result<Option<Wdl>, String>;
    /// The BLP `file`, its levels as they are stored: DXT kept, the others as `Rgba8`.
    fn texture(&self, file: &FileRef) -> Result<Texture, String>;
    /// The BLP `file`, every level as `Rgba8`.
    fn texture_rgba(&self, file: &FileRef) -> Result<Texture, String>;
}

/// The service of the formats of the client's files.
pub const SERVICE: ServiceKey<Arc<dyn Formats>> = ServiceKey::new("formats");

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(ids: &[u16], drawn: &[bool]) -> Vec<u16> {
        ids.iter()
            .zip(drawn)
            .filter(|(_, drawn)| **drawn)
            .map(|(id, _)| *id)
            .collect()
    }

    #[test]
    fn a_creature_shows_the_variant_each_nibble_chooses_and_nothing_else_below_900() {
        let ids = [0, 101, 102, 201, 202, 301, 302, 802, 905];
        // Group 100 at 1, 200 at 2, 300 at 0 (none), 800 at 2.
        let drawn = creature_geosets(&ids, 0x2000_0021);
        assert_eq!(shown(&ids, &drawn), [0, 101, 202, 802, 905]);
        assert!(creature_geosets(&ids, 0).iter().all(|drawn| *drawn));
    }

    #[test]
    fn a_look_shows_its_hair_facial_hair_ears_and_bare_variants() {
        let ids = [
            0, 1, 2, 3, 101, 102, 201, 202, 301, 302, 401, 402, 501, 502, 701, 702, 802, 1301, 1302, 1502, 1601, 1702,
            1703, 1801,
        ];
        let hair = HairGeoset {
            race: 1,
            sex: 0,
            variation: 4,
            geoset: 3,
            scalp: false,
        };
        let facial = FacialHair {
            race: 1,
            sex: 0,
            variation: 2,
            geosets: [2, 0, 1, 1, 3],
        };
        let drawn = look_geosets(&ids, Some(&hair), Some(&facial));
        assert_eq!(
            shown(&ids, &drawn),
            [0, 3, 102, 201, 401, 501, 702, 1301, 1601, 1703, 1801]
        );
        // Bald, without facial hair: the scalp.
        let bald = HairGeoset { geoset: 0, ..hair };
        let drawn = look_geosets(&ids, Some(&bald), None);
        assert_eq!(shown(&ids, &drawn), [0, 1, 401, 501, 702, 1301, 1801]);
    }

    #[test]
    fn a_model_left_alone_shows_its_first_variants() {
        let ids = [0, 1, 101, 102, 201, 1701, 1702, 3201, 3205, 3501];
        assert_eq!(shown(&ids, &default_geosets(&ids)), [0, 101, 201, 3201, 3205]);
    }
}
