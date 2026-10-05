//! The mesh of a chunk, built from the model alone, so that a chunk changed is built again by
//! itself: its 145 vertices, its triangles, those of its holes left out, and its texels of blending.

use uniwow_api::bytemuck::{Pod, Zeroable};
use uniwow_api::formats::Chunk;

use crate::model::{TileId, chunk_corner, vertex_place, vertex_position};

/// The vertices of a chunk.
pub const VERTICES: usize = 145;

/// A vertex of the terrain, as its shader reads it: 32 bytes, without padding.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    /// The normal, 127 for 1, then 0.
    pub normal: [i8; 4],
    /// The vertex colour, 127 for 1.
    pub colour: [u8; 4],
    /// The place in the chunk, from 0 to 1 along its rows and its columns.
    pub uv: [f32; 2],
    /// The layer of the chunk in the textures of blending of its tile.
    pub chunk: u32,
}

// SAFETY: plain numbers laid out by `repr(C)` without padding, any bit pattern valid.
unsafe impl Zeroable for Vertex {}
unsafe impl Pod for Vertex {}

/// The vertices of the chunk `place` of the tile `tile`.
pub fn vertices(tile: TileId, place: usize, chunk: &Chunk) -> Vec<Vertex> {
    let corner = chunk_corner(tile, chunk);
    (0..VERTICES)
        .map(|vertex| {
            let [row, column] = vertex_place(vertex);
            let normal = chunk.normals.get(vertex).copied().unwrap_or([0, 0, 127]);
            Vertex {
                position: vertex_position(corner, vertex, chunk.heights.get(vertex).copied().unwrap_or(0.0)),
                normal: [normal[0], normal[1], normal[2], 0],
                colour: chunk.colours.get(vertex).copied().unwrap_or([127, 127, 127, 255]),
                uv: [column / 8.0, row / 8.0],
                chunk: place as u32,
            }
        })
        .collect()
}

/// The triangles of a chunk, by its vertices: four a quad, around its inner vertex, facing up;
/// the quads of its holes, a bit each in `holes`, left out.
pub fn indices(holes: u64) -> Vec<u16> {
    let mut indices = Vec::with_capacity(8 * 8 * 12);
    for row in 0..8u16 {
        for column in 0..8u16 {
            if holes & (1 << (row * 8 + column)) != 0 {
                continue;
            }
            let outer = |r: u16, c: u16| r * 17 + c;
            let centre = row * 17 + 9 + column;
            let (a, b) = (outer(row, column), outer(row, column + 1));
            let (c, d) = (outer(row + 1, column + 1), outer(row + 1, column));
            // Counter-clockwise seen from above: the row goes down in Y, the rows down in X.
            indices.extend_from_slice(&[centre, b, a, centre, c, b, centre, d, c, centre, a, d]);
        }
    }
    indices
}

/// The texels of blending of a chunk, 64 × 64, row by row: the alpha maps of its layers after the
/// first in red, green and blue, its shadow in alpha.
pub fn blend(chunk: &Chunk) -> Vec<u8> {
    let mut texels = vec![0u8; 64 * 64 * 4];
    for (channel, map) in chunk.alphas.iter().take(3).enumerate() {
        for (texel, value) in map.iter().take(64 * 64).enumerate() {
            texels[texel * 4 + channel] = *value;
        }
    }
    for (texel, value) in chunk.shadow.iter().take(64 * 64).enumerate() {
        texels[texel * 4 + 3] = *value;
    }
    texels
}
