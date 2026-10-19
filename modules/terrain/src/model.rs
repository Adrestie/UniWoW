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

/// The tile `id` as the formats name it.
pub fn shared_id(id: TileId) -> uniwow_api::formats::TileId {
    uniwow_api::formats::TileId { x: id.x, y: id.y }
}

/// The chunks of the tile `model` of the map `map` as entities of the scene: each by its tile and
/// index, its origin the middle of its bounds.
pub fn piece(map: u32, model: &TileModel) -> uniwow_api::scene::Piece {
    use uniwow_api::scene::{Entity, EntityId, PieceBuilder, flags};
    let tile = shared_id(model.id);
    let mut builder = PieceBuilder::with_capacity(vec![tile], model.tile.chunks.len());
    for chunk in &model.tile.chunks {
        let bounds = chunk_bounds(model.id, chunk);
        builder.push(Entity {
            id: EntityId::chunk(map, tile, chunk.index),
            flags: flags::CLIENT,
            parent: None,
            label: "",
            extra: chunk.area,
            origin: [0, 1, 2].map(|axis| (bounds[0][axis] + bounds[1][axis]) / 2.0),
            bounds,
        });
    }
    builder.build()
}

/// The heights of a chunk as the picking reads them: its index, its corner and its 145 heights.
pub struct Heights {
    pub index: [u32; 2],
    pub corner: [f32; 3],
    pub heights: Vec<f32>,
}

/// The heights of every chunk of `model`.
pub fn heights(model: &TileModel) -> Vec<Heights> {
    model
        .tile
        .chunks
        .iter()
        .map(|chunk| Heights {
            index: chunk.index,
            corner: chunk_corner(model.id, chunk),
            heights: chunk.heights.clone(),
        })
        .collect()
}

/// Where `ray` meets the ground of the chunk `chunk`, by the four triangles of each of its 8 × 8
/// quads around its inner vertex; none where it misses.
pub fn ray_hits(chunk: &Heights, ray: &uniwow_api::viewport::Ray) -> Option<f32> {
    use uniwow_api::glam::Vec3;
    if chunk.heights.len() < 145 {
        return None;
    }
    let at = |vertex: usize| Vec3::from(vertex_position(chunk.corner, vertex, chunk.heights[vertex]));
    let mut nearest: Option<f32> = None;
    for row in 0..8 {
        for column in 0..8 {
            let outer = |r: usize, c: usize| r * 17 + c;
            let inner = at(row * 17 + 9 + column);
            let corners = [
                outer(row, column),
                outer(row, column + 1),
                outer(row + 1, column + 1),
                outer(row + 1, column),
            ];
            for side in 0..4 {
                let (a, b) = (at(corners[side]), at(corners[(side + 1) % 4]));
                if let Some(distance) = ray.hits_triangle(a, b, inner) {
                    nearest = Some(nearest.map_or(distance, |nearest| nearest.min(distance)));
                }
            }
        }
    }
    nearest
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
