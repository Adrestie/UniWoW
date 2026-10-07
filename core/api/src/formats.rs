//! The contract of the formats of the client's files, offered by the module `assets`: their content
//! parsed, from any thread at once (T3): the rows of the tables of 3.3.5a the next steps need, the
//! texts in the client's locale; the terrain of a map; the textures.
//!
//! World coordinates, as the client's: X towards the north, Y towards the west, Z up, in yards;
//! a map is 64 × 64 tiles of 533⅓ yards, the tile `<map>_<x>_<y>` reaching from
//! `17066⅔ − 533⅓ x` down in Y and from `17066⅔ − 533⅓ y` down in X.

use std::collections::HashSet;
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

use crate::ServiceKey;
use crate::glam::{Mat4, Quat, Vec3};

/// The side of a tile, in yards.
pub const TILE: f32 = 1600.0 / 3.0;
/// The world's X and Y at the corner of the tile 0 0.
pub const ORIGIN: f32 = 32.0 * TILE;

/// A tile of a map by its place, as its file names it: `<map>_<x>_<y>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub fn centre(self) -> [f32; 2] {
        [
            ORIGIN - TILE * (self.y as f32 + 0.5),
            ORIGIN - TILE * (self.x as f32 + 0.5),
        ]
    }

    /// How far from the point `eye` its centre lies, on the ground, in tiles.
    pub fn distance(self, eye: [f32; 2]) -> f32 {
        let [x, y] = self.centre();
        (x - eye[0]).hypot(y - eye[1]) / TILE
    }
}

/// The tiles of a map, `tiles` at `y * 64 + x`, around `eye` within `distance` tiles, the nearest
/// first: those whose centre lies within half a tile more, as the terrain chooses its own, and
/// those `held` within a whole tile more, so that a camera going to and fro over a border does not
/// read them again.
pub fn tiles_around(tiles: &[bool], eye: [f32; 2], distance: u32, held: &HashSet<TileId>) -> Vec<TileId> {
    let mut around: Vec<(TileId, f32)> = tiles
        .iter()
        .enumerate()
        .filter(|(_, exists)| **exists)
        .map(|(index, _)| {
            let tile = TileId {
                x: index as u32 % 64,
                y: index as u32 / 64,
            };
            (tile, tile.distance(eye))
        })
        .filter(|(tile, away)| {
            let reach = if held.contains(tile) { 1.0 } else { 0.5 };
            *away <= distance as f32 + reach
        })
        .collect();
    around.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    around.into_iter().map(|(tile, _)| tile).collect()
}

/// From the model of a placement of a tile, a doodad's or a building's, to the world. The axes of
/// its file, Y up, are those of the client's tiles: the world's X is `ORIGIN` less its Z, its Y
/// `ORIGIN` less its X, its Z its Y. Its rotation, in degrees, as Noggit applies it in those axes,
/// comes in the world's to turns about Z by its Y less a quarter, about X by its X, about Y by
/// less its Z, after the quarter turn back about Z that the model takes.
pub fn placement(position: [f32; 3], rotation: [f32; 3], scale: f32) -> Mat4 {
    let [x, y, z] = position;
    let [a, b, c] = rotation.map(f32::to_radians);
    let turned = Quat::from_rotation_z(b - FRAC_PI_2)
        * Quat::from_rotation_x(a)
        * Quat::from_rotation_y(-c)
        * Quat::from_rotation_z(-FRAC_PI_2);
    Mat4::from_scale_rotation_translation(Vec3::splat(scale), turned, Vec3::new(ORIGIN - z, ORIGIN - x, y))
}

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
    /// Its skin baked into one texture: its file name, in `Textures\BakedNpcTextures`.
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

/// An animation of `AnimationData.dbc`: its id, as the sequences of a model name it, its name, and
/// the animation played in its place by a model that lacks it (*Walk* and *Run* fall back on
/// *Stand*, 0; *Stand* on *Closed*, 147, the state of a door).
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationRecord {
    pub id: u32,
    pub name: String,
    pub fallback: u32,
}

/// A section of a character's textures, `CharSections.dbc`: its skin (0), face (1), facial hair
/// (2), hair (3) or underwear (4), of a variation and a colour.
#[derive(Clone, Debug, PartialEq)]
pub struct CharSection {
    pub race: u32,
    pub sex: u32,
    pub section: u32,
    pub variation: u32,
    pub colour: u32,
    /// Its textures, by path; empty where none.
    pub textures: [String; 3],
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

/// What stands on a tile: its doodads and its buildings, without its terrain.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placements {
    pub doodads: Vec<Doodad>,
    pub buildings: Vec<Building>,
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

/// A building (WMO, of version 17) at rest: its root and its groups at their finest level. Its
/// positions in its own axes, Z up, as those of a model; its colours red, green, blue and alpha.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wmo {
    /// 0x1 its vertex colours not attenuated by the portals, 0x2 lit as one, 0x4 the types of its
    /// liquids those of `LiquidType.dbc`, 0x8 the alpha of its vertex colours not fixed, 0x10 its
    /// groups in levels of detail.
    pub flags: u16,
    pub ambient: [u8; 4],
    /// Its id in `WMOAreaTable.dbc`.
    pub id: u32,
    pub bounds: [[f32; 3]; 2],
    /// The model of the sky seen from inside it; none when it names none.
    pub skybox: Option<FileRef>,
    pub materials: Vec<WmoMaterial>,
    pub groups: Vec<WmoGroup>,
    pub portals: Vec<Portal>,
    pub portal_refs: Vec<PortalRef>,
    pub lights: Vec<WmoLight>,
    pub doodad_sets: Vec<DoodadSet>,
    pub doodads: Vec<WmoDoodad>,
    pub fogs: Vec<WmoFog>,
    /// What was left out of it, and why: what refers to what it does not have, a group missing or
    /// that does not hold together.
    pub faults: Vec<String>,
}

/// A material of a building: its flags (0x01 unlit, 0x02 unfogged, 0x04 two-sided, 0x08 lit as
/// outside, 0x10 its emissive colour lit at night, 0x20 a window, 0x40 clamped across, 0x80
/// clamped up and down), its shader (0 diffuse, 1 specular, 2 metal, 3 environment, 4 opaque, 5
/// environment metal, 6 two layers…), its blending (`EGxBlend`: 0 opaque, 1 alpha key, 2 alpha,
/// 3 add, 4 mod, 5 mod2x…), its textures and its colours.
#[derive(Clone, Debug, PartialEq)]
pub struct WmoMaterial {
    pub flags: u32,
    pub shader: u32,
    pub blending: u32,
    /// None where it names none.
    pub textures: [Option<FileRef>; 3],
    pub emissive: [u8; 4],
    pub diffuse: [u8; 4],
    /// Its third colour, of the shaders of later clients.
    pub colour: [u8; 4],
    /// Its ground, of `TerrainType.dbc`.
    pub ground: u32,
}

/// A group of a building at its finest level: its name, its flags (0x1 a BSP tree, 0x4 vertex
/// colours, 0x8 outside, 0x40 lit as outside, 0x200 lights, 0x800 doodads, 0x1000 a liquid,
/// 0x2000 inside, 0x40000 the sky shown, 0x1000000 a second set of colours blending its textures,
/// 0x2000000 a second set of coordinates), its bounds and its geometry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WmoGroup {
    pub name: String,
    pub flags: u32,
    pub bounds: [[f32; 3]; 2],
    /// Its portals: `Wmo::portal_refs` from the first, so many.
    pub portals: [u16; 2],
    /// How many of its batches, in that order, are of a transition, inside and outside.
    pub batch_counts: [u16; 3],
    /// Its fogs, of `Wmo::fogs`.
    pub fogs: [u8; 4],
    /// The type of its liquid.
    pub liquid_type: u32,
    /// Its id in `WMOAreaTable.dbc`.
    pub id: u32,
    pub vertices: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Its sets of coordinates of textures, a pair a vertex each; one at least.
    pub coordinates: Vec<Vec<[f32; 2]>>,
    /// Its sets of vertex colours, a colour a vertex each: none, one, or a second that blends its
    /// textures.
    pub colours: Vec<Vec<[u8; 4]>>,
    /// Three vertices a triangle.
    pub triangles: Vec<u16>,
    /// The flags and the material of each triangle.
    pub faces: Vec<WmoFace>,
    pub batches: Vec<WmoBatch>,
    /// The doodads it holds, of `Wmo::doodads`.
    pub doodad_refs: Vec<u16>,
    pub liquid: Option<WmoLiquid>,
    /// Its BSP tree, its root first; none where it has none, or one that does not hold together.
    pub bsp: Vec<BspNode>,
    /// The triangles the leaves of its tree hold, by their number among its triangles.
    pub bsp_faces: Vec<u16>,
}

/// A node of the BSP tree of a group: its flags (its axis in the two low bits, X, Y or Z; 0x4 a
/// leaf), its children on the negative and on the positive side of its plane (none where
/// negative), the triangles of a leaf (`WmoGroup::bsp_faces` from the first, so many), and where
/// its plane cuts its axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspNode {
    pub flags: u16,
    pub children: [i16; 2],
    pub first: u32,
    pub count: u16,
    pub distance: f32,
}

/// A triangle of a group: its flags, and its material, of `Wmo::materials`; none for a triangle
/// only collided with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WmoFace {
    pub flags: u16,
    pub material: Option<u16>,
}

/// A batch of a group: its triangles, from the index `first` of `WmoGroup::triangles`, `count`
/// indices; the first and the last vertex they use; its flags and its material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WmoBatch {
    pub first: u32,
    pub count: u32,
    pub vertices: [u16; 2],
    pub flags: u8,
    pub material: u16,
}

/// The liquid of a group: a grid of `size` vertices from its corner, `tiles` between them, its
/// material; each vertex its height and its data (its flow for water, coordinates for magma),
/// each tile its flags (its liquid in the low nibble, 0x0F not drawn).
#[derive(Clone, Debug, PartialEq)]
pub struct WmoLiquid {
    pub size: [u32; 2],
    pub tiles: [u32; 2],
    pub corner: [f32; 3],
    pub material: u16,
    pub heights: Vec<f32>,
    pub data: Vec<[u8; 4]>,
    pub tile_flags: Vec<u8>,
}

/// A portal of a building: its polygon, and its plane (a normal, then its distance).
#[derive(Clone, Debug, PartialEq)]
pub struct Portal {
    pub vertices: Vec<[f32; 3]>,
    pub plane: [f32; 4],
}

/// A portal seen from a group: the side of its plane the group lies on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalRef {
    pub portal: u16,
    pub group: u16,
    pub side: i16,
}

/// A light of a building: its kind (0 a point, 1 a spot, 2 directed, 3 ambient), whether it fades,
/// and between which distances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WmoLight {
    pub kind: u8,
    pub attenuated: bool,
    pub colour: [u8; 4],
    pub position: [f32; 3],
    pub intensity: f32,
    pub attenuation: [f32; 2],
}

/// A set of the doodads of a building: `Wmo::doodads` from the first, so many.
#[derive(Clone, Debug, PartialEq)]
pub struct DoodadSet {
    pub name: String,
    pub first: u32,
    pub count: u32,
}

/// A doodad of a building, in its axes: its model, its flags, its rotation (a quaternion x, y, z,
/// w), its scale and its colour.
#[derive(Clone, Debug, PartialEq)]
pub struct WmoDoodad {
    pub file: FileRef,
    pub flags: u8,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: f32,
    pub colour: [u8; 4],
}

/// A fog of a building: where it lies, between two radii, its fog in the air and under water.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WmoFog {
    pub flags: u32,
    pub position: [f32; 3],
    pub radii: [f32; 2],
    pub fog: FogBand,
    pub underwater: FogBand,
}

/// Where a fog ends, where it starts as a fraction of its end, and its colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogBand {
    pub end: f32,
    pub start: f32,
    pub colour: [u8; 4],
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

/// How a track goes from a key to the next: holding it, in a line (a rotation normalised), or along
/// a Bézier or a Hermite curve by the tangents of its keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Interpolation {
    Step,
    #[default]
    Linear,
    Bezier,
    Hermite,
}

/// The keys of a track in one sequence: their times, in milliseconds from its start, and their
/// values; on a curve, the tangents in and out of each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keys<T> {
    pub times: Vec<u32>,
    pub values: Vec<T>,
    pub tangents: Vec<[T; 2]>,
}

/// A value that changes with time: how it goes from a key to the next, the global sequence it
/// loops on, and its keys in each sequence of its model, by their place (none for a sequence not
/// kept), or in its global sequence alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track<T> {
    pub interpolation: Interpolation,
    pub global: Option<u16>,
    pub keys: Vec<Keys<T>>,
}

/// A bone of a model: its flags (0x08 to 0x40 a billboard), its parent, the point it turns about,
/// and its translation, rotation (a quaternion x, y, z, w) and scale.
#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    pub key_bone: i32,
    pub flags: u32,
    pub parent: Option<u16>,
    pub pivot: [f32; 3],
    pub translation: Track<[f32; 3]>,
    pub rotation: Track<[f32; 4]>,
    pub scale: Track<[f32; 3]>,
}

/// A sequence of a model: the animation it plays (`AnimationRecord::id`), which of its variations,
/// its length in milliseconds, the speed it moves at in yards a second, its flags (0x20 its keys in
/// the model, 0x40 an alias), how often among its variations, how many times it plays again, its
/// times of blending in and out, the bounds it moves in, its next variation and the sequence it is
/// an alias of; and whether its keys are kept.
#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub id: u16,
    pub variation: u16,
    pub duration: u32,
    pub speed: f32,
    pub flags: u32,
    pub frequency: i16,
    pub replay: [u32; 2],
    pub blend: [u16; 2],
    pub bounds: [[f32; 3]; 2],
    pub radius: f32,
    pub next: Option<u16>,
    pub alias: Option<u16>,
    pub kept: bool,
}

/// How a texture's coordinates move: their translation, rotation and scale.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextureTransform {
    pub translation: Track<[f32; 3]>,
    pub rotation: Track<[f32; 4]>,
    pub scale: Track<[f32; 3]>,
}

/// What moves in a model: its sequences, the lengths of its global sequences, its bones and the
/// order they are computed in (each after its parent), the colours and alphas, weights and
/// transforms its batches refer to. The keys are kept only for the sequences played (*Stand*,
/// *Walk*, *Run*, held in the model) and the global sequences.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Animation {
    pub sequences: Vec<Sequence>,
    pub globals: Vec<u32>,
    pub bones: Vec<Bone>,
    pub order: Vec<u16>,
    pub colours: Vec<(Track<[f32; 3]>, Track<f32>)>,
    pub weights: Vec<Track<f32>>,
    pub transforms: Vec<TextureTransform>,
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
    /// coordinates from `uv_combo` in `Model::uv_combos`, as the shader WotLK chooses at load
    /// reads them: for one texture, 0 the first set, 0xFFFF the environment when blended, another
    /// value the second; for two, the first and the second sets, 0xFFFF the environment. Chosen by
    /// its shader when the model has none, as since Cataclysm. Its weight at `weight_combo` in
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
    pub animation: Animation,
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
    /// The sections of the characters' textures, by race, sex, section, variation and colour.
    fn char_sections(&self) -> Result<Arc<Vec<CharSection>>, String>;
    /// The animations, by increasing id.
    fn animations(&self) -> Result<Arc<Vec<AnimationRecord>>, String>;
    /// The M2 `file`, with its skins: of 3.3.5a, its path as a table names it (`.mdx` and `.mdl`
    /// read as `.m2`) and its skins beside it; modern, its skins named by its chunk `SFID`.
    fn model(&self, file: &FileRef) -> Result<Model, String>;
    /// The building `file` with its groups at their finest level: of 3.3.5a, its groups beside it
    /// by its name and their index; modern, by its chunk `GFID`.
    fn wmo(&self, file: &FileRef) -> Result<Wmo, String>;
    /// The WDT of the map whose folder, below `World\Maps`, is `directory`, read once.
    fn wdt(&self, directory: &str) -> Result<Arc<Wdt>, String>;
    /// The tile `<directory>_<x>_<y>`, of 3.3.5a, or split in a root, a `_tex0` and an `_obj0` as
    /// WarcraftXL loads it; none when the WDT of its map has no such tile.
    fn tile(&self, directory: &str, x: u32, y: u32) -> Result<Option<Tile>, String>;
    /// What stands on the tile `<directory>_<x>_<y>`, its terrain left unread; none when the WDT of
    /// its map has no such tile. By default, from the whole tile.
    fn placements(&self, directory: &str, x: u32, y: u32) -> Result<Option<Placements>, String> {
        Ok(self.tile(directory, x, y)?.map(|tile| Placements {
            doodads: tile.doodads,
            buildings: tile.buildings,
        }))
    }
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
    use crate::glam::Mat3;

    #[test]
    fn the_tiles_around_the_camera_are_chosen_the_nearest_first() {
        let all = vec![true; 4096];
        // The middle of the tile `_30_40`: X falls with its y, Y with its x.
        let eye = [ORIGIN - 40.5 * TILE, ORIGIN - 30.5 * TILE];
        let around = tiles_around(&all, eye, 1, &HashSet::new());
        assert_eq!(around[0], TileId { x: 30, y: 40 });
        assert_eq!(
            around.len(),
            9,
            "the tiles around it, their centre within 1.5 tiles: {around:?}"
        );
        assert!(
            around[1..5]
                .iter()
                .all(|tile| tile.x.abs_diff(30) + tile.y.abs_diff(40) == 1)
        );
        // Held, kept within a tile more; not beyond.
        let held = HashSet::from([TileId { x: 32, y: 40 }, TileId { x: 33, y: 40 }]);
        let kept = tiles_around(&all, eye, 1, &held);
        assert_eq!(kept.len(), 10);
        assert_eq!(kept[9], TileId { x: 32, y: 40 });
        // A tile the WDT does not name, never.
        let mut holed = all.clone();
        holed[40 * 64 + 30] = false;
        assert!(!tiles_around(&holed, eye, 1, &HashSet::new()).contains(&TileId { x: 30, y: 40 }));
    }

    /// The transform of a placement as Noggit builds it, in its axes of the file, Y up, the model's
    /// vertices turned into them; then brought into the world's.
    fn noggit(position: [f32; 3], rotation: [f32; 3], scale: f32) -> Mat4 {
        let [x, y, z] = rotation.map(f32::to_radians);
        let placed = Mat4::from_translation(Vec3::from(position))
            * Mat4::from_rotation_y(y - FRAC_PI_2)
            * Mat4::from_rotation_z(-x)
            * Mat4::from_rotation_x(z)
            * Mat4::from_scale(Vec3::splat(scale));
        // A vertex of the model (x, y, z), Z up, as Noggit reads it: (x, z, -y).
        let model = Mat4::from_mat3(Mat3::from_cols(Vec3::X, -Vec3::Z, Vec3::Y));
        // A point of the file's axes in the world's: (ORIGIN - z, ORIGIN - x, y).
        let world = Mat4::from_translation(Vec3::new(ORIGIN, ORIGIN, 0.0))
            * Mat4::from_mat3(Mat3::from_cols(-Vec3::Y, Vec3::Z, -Vec3::X));
        world * placed * model
    }

    #[test]
    fn a_placement_stands_where_its_file_places_it_turned_as_noggit_turns_it() {
        let at = placement([100.0, 20.0, 300.0], [0.0; 3], 1.0).transform_point3(Vec3::ZERO);
        assert!(
            at.abs_diff_eq(Vec3::new(ORIGIN - 300.0, ORIGIN - 100.0, 20.0), 1e-2),
            "{at}"
        );
        for rotation in [
            [0.0, 0.0, 0.0],
            [0.0, 90.0, 0.0],
            [0.0, 237.5, 0.0],
            [12.0, 0.0, 0.0],
            [0.0, 0.0, -20.0],
            [7.5, 301.0, -14.0],
            [-33.0, 45.0, 81.0],
        ] {
            let position = [16_000.0, 35.0, 9_000.0];
            let (made, expected) = (placement(position, rotation, 1.75), noggit(position, rotation, 1.75));
            for point in [Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z, Vec3::new(3.0, -2.0, 5.0)] {
                let (got, want) = (made.transform_point3(point), expected.transform_point3(point));
                assert!(
                    got.abs_diff_eq(want, 1e-2),
                    "{rotation:?} {point}: {got} against {want}"
                );
            }
        }
        // Facing along X turned about the vertical, its scale kept.
        let front = placement([0.0; 3], [0.0, 90.0, 0.0], 2.0).transform_vector3(Vec3::X);
        assert!(front.abs_diff_eq(Vec3::new(0.0, -2.0, 0.0), 1e-4), "{front}");
    }

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
