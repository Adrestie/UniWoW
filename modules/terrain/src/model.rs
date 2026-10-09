//! The model of the terrain: the tiles loaded, kept chunk by chunk as they will be edited, each
//! chunk and tile able to be marked changed; and where the client places them.

use uniwow_api::formats::{Chunk, Tile};

/// The side of a tile, of a chunk, and the distance between two rows of vertices, in yards.
pub const TILE: f32 = 1600.0 / 3.0;
pub const CHUNK: f32 = TILE / 16.0;
pub const STEP: f32 = CHUNK / 8.0;
/// The highest X and Y of a map.
pub const ORIGIN: f32 = 32.0 * TILE;

/// A tile of a map by its place, as its file names it: `<map>_<x>_<y>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub x: u32,
    pub y: u32,
}

impl TileId {
    /// Its corner of highest X and Y in the world.
    pub fn corner(self) -> [f32; 2] {
        [ORIGIN - TILE * self.y as f32, ORIGIN - TILE * self.x as f32]
    }

    pub fn centre(self) -> [f32; 2] {
        let [x, y] = self.corner();
        [x - TILE / 2.0, y - TILE / 2.0]
    }
}

/// A chunk by its tile and its index: the id it keeps for the selection to come.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkId {
    pub tile: TileId,
    pub index: [u32; 2],
}

/// A tile loaded: its data as read, its alpha maps the share of each layer whatever its map
/// (`read_tile`), and the chunks changed in it, a bit each; none yet, as nothing is edited in this
/// milestone.
pub struct TileModel {
    pub id: TileId,
    pub tile: Tile,
    changed: [u64; 4],
}

impl TileModel {
    pub fn new(id: TileId, tile: Tile) -> Self {
        Self {
            id,
            tile,
            changed: [0; 4],
        }
    }

    pub fn mark_changed(&mut self, chunk: usize) {
        self.changed[chunk / 64] |= 1 << (chunk % 64);
    }

    pub fn is_changed(&self, chunk: usize) -> bool {
        self.changed[chunk / 64] & (1 << (chunk % 64)) != 0
    }

    /// Whether any chunk of the tile is changed.
    pub fn changed(&self) -> bool {
        self.changed.iter().any(|bits| *bits != 0)
    }

    /// What its data takes in memory, its vectors counted by their length.
    pub fn bytes(&self) -> u64 {
        let tile = &self.tile;
        let chunks: usize = tile
            .chunks
            .iter()
            .map(|chunk| {
                size_of::<Chunk>()
                    + chunk.heights.len() * 4
                    + chunk.normals.len() * 3
                    + chunk.colours.len() * 4
                    + chunk.layers.len() * size_of::<uniwow_api::formats::Layer>()
                    + chunk.alphas.iter().map(Vec::len).sum::<usize>()
                    + chunk.shadow.len()
                    + (chunk.doodad_refs.len() + chunk.building_refs.len()) * 4
            })
            .sum();
        let placed = tile.doodads.len() * size_of::<uniwow_api::formats::Doodad>()
            + tile.buildings.len() * size_of::<uniwow_api::formats::Building>();
        (size_of::<Self>() + chunks + placed) as u64
    }

    pub fn chunk_id(&self, chunk: usize) -> ChunkId {
        ChunkId {
            tile: self.id,
            index: self.tile.chunks[chunk].index,
        }
    }
}

/// The corner of a chunk as the client places it: by its tile and its index, the base of its
/// heights only taken from the file, whose position some tiles made by tools give wrong.
pub fn chunk_corner(tile: TileId, chunk: &Chunk) -> [f32; 3] {
    let [x, y] = tile.corner();
    [
        x - CHUNK * chunk.index[1] as f32,
        y - CHUNK * chunk.index[0] as f32,
        chunk.position[2],
    ]
}

/// The place of the vertex `vertex` of a chunk, in rows and columns from its corner: 17 rows of 9
/// outer vertices and 8 inner ones in turn, an inner one in the middle of its quad.
pub fn vertex_place(vertex: usize) -> [f32; 2] {
    let (row, rest) = (vertex / 17, vertex % 17);
    if rest < 9 {
        [row as f32, rest as f32]
    } else {
        [row as f32 + 0.5, (rest - 9) as f32 + 0.5]
    }
}

/// The world position of the vertex `vertex` of a chunk whose corner is `corner`: a row goes down
/// in Y, the rows go down in X.
pub fn vertex_position(corner: [f32; 3], vertex: usize, height: f32) -> [f32; 3] {
    let [row, column] = vertex_place(vertex);
    [corner[0] - row * STEP, corner[1] - column * STEP, corner[2] + height]
}

/// The bounds of a chunk in the world, its lowest corner then its highest.
pub fn chunk_bounds(tile: TileId, chunk: &Chunk) -> [[f32; 3]; 2] {
    let corner = chunk_corner(tile, chunk);
    let (low, high) = chunk
        .heights
        .iter()
        .fold((f32::MAX, f32::MIN), |(low, high), h| (low.min(*h), high.max(*h)));
    [
        [corner[0] - CHUNK, corner[1] - CHUNK, corner[2] + low],
        [corner[0], corner[1], corner[2] + high],
    ]
}
