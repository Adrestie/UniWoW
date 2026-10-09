//! The mesh of a tile, built from the model alone, so that a chunk changed is built again by
//! itself: the 145 vertices of each chunk, the skirts under the sides of the tile, the triangles of
//! each level of detail, those of its holes left out, and the texels of blending of each chunk; and
//! for a light tile, only the vertices of its coarsest level and its blending reduced.

use std::ops::Range;

use uniwow_api::bytemuck::{Pod, Zeroable};
use uniwow_api::formats::{Chunk, Tile};

use crate::model::{TileId, chunk_corner, vertex_place, vertex_position};

/// The chunks of a tile, and the vertices of a chunk.
pub const CHUNKS: usize = 256;
pub const VERTICES: usize = 145;

/// The vertices of the skirts, after those of the chunks: under the 9 outer vertices of each chunk
/// along each of the four sides of the tile.
pub const SKIRT_VERTICES: usize = 4 * 16 * 9;
pub const TILE_VERTICES: usize = CHUNKS * VERTICES + SKIRT_VERTICES;

/// How far a skirt hangs under the side of a tile, in yards: it fills what opens between two tiles
/// drawn at two levels of detail.
pub const SKIRT_DEPTH: f32 = 40.0;

/// The levels of detail: all the vertices; the outer ones; one outer vertex in two; the corners of
/// each chunk.
pub const LODS: usize = 4;

/// The quads of a chunk a pair of triangles of each level covers, a side.
const STEPS: [u16; LODS] = [1, 1, 2, 8];

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

/// The sides of a tile: its first row of vertices (highest X), its last row, its first column
/// (highest Y), its last column.
const SIDES: usize = 4;

/// The place along the side `side` of a chunk at `index`, when it lies on that side.
fn along(side: usize, index: [u32; 2]) -> Option<usize> {
    let [column, row] = index.map(|i| i as usize);
    match side {
        0 => (row == 0).then_some(column),
        1 => (row == 15).then_some(column),
        2 => (column == 0).then_some(row),
        _ => (column == 15).then_some(row),
    }
}

/// The outer vertex `k` of a chunk along the side `side`, by its index in the chunk.
fn edge_vertex(side: usize, k: u16) -> u16 {
    match side {
        0 => k,
        1 => 8 * 17 + k,
        2 => k * 17,
        _ => k * 17 + 8,
    }
}

/// The index in the tile of the skirt vertex under the outer vertex `k` of the chunk at `at` along
/// `side`.
fn skirt_vertex(side: usize, at: usize, k: u16) -> u16 {
    (CHUNKS * VERTICES + (side * 16 + at) * 9) as u16 + k
}

/// The skirt vertices of the chunk `place` of the tile `tile`, nine for each side of the tile it
/// lies on, after the index in the tile of the first: copies of its outer vertices along that
/// side, lowered by `SKIRT_DEPTH`.
pub fn skirt(tile: TileId, place: usize, chunk: &Chunk) -> Vec<(usize, Vec<Vertex>)> {
    let mut found = Vec::new();
    let mut outer: Option<Vec<Vertex>> = None;
    for side in 0..SIDES {
        let Some(at) = along(side, chunk.index) else {
            continue;
        };
        let outer = outer.get_or_insert_with(|| vertices(tile, place, chunk));
        let lowered = (0..9)
            .map(|k| {
                let mut vertex = outer[edge_vertex(side, k) as usize];
                vertex.position[2] -= SKIRT_DEPTH;
                vertex
            })
            .collect();
        found.push((skirt_vertex(side, at, 0) as usize, lowered));
    }
    found
}

/// The triangles of a chunk at the level `lod`, by the vertices of its tile, those of the chunk
/// starting at `base`: a pair a block of quads, facing up; with all the vertices, four a quad
/// around its inner vertex. A quad of its holes, a bit each in `holes`, is left out, and a block
/// whose quads are all holes.
fn surface(lod: usize, base: u16, holes: u64, indices: &mut Vec<u16>) {
    let outer = |r: u16, c: u16| base + r * 17 + c;
    let hole = |r: u16, c: u16| holes & (1 << (r * 8 + c)) != 0;
    let step = STEPS[lod];
    for row in (0..8u16).step_by(step as usize) {
        for column in (0..8u16).step_by(step as usize) {
            if (row..row + step).all(|r| (column..column + step).all(|c| hole(r, c))) {
                continue;
            }
            let (a, b) = (outer(row, column), outer(row, column + step));
            let (c, d) = (outer(row + step, column + step), outer(row + step, column));
            // Counter-clockwise seen from above: the row goes down in Y, the rows down in X.
            if lod == 0 {
                let centre = base + row * 17 + 9 + column;
                indices.extend_from_slice(&[centre, b, a, centre, c, b, centre, d, c, centre, a, d]);
            } else {
                indices.extend_from_slice(&[d, b, a, d, c, b]);
            }
        }
    }
}

/// The triangles of the skirt under the chunk at `at` along `side`, at the level `lod`, from the
/// vertices of its edge, those of the chunk starting at `base`, facing out of the tile.
fn skirt_triangles(lod: usize, side: usize, at: usize, base: u16, indices: &mut Vec<u16>) {
    let step = STEPS[lod];
    for k in (0..8u16).step_by(step as usize) {
        let (mut a, mut b) = (base + edge_vertex(side, k), base + edge_vertex(side, k + step));
        let (mut low_a, mut low_b) = (skirt_vertex(side, at, k), skirt_vertex(side, at, k + step));
        // Along the last row and the first column, the order that faces out is the other one.
        if side == 1 || side == 2 {
            (a, b, low_a, low_b) = (b, a, low_b, low_a);
        }
        indices.extend_from_slice(&[a, b, low_a, b, low_b, low_a]);
    }
}

/// The triangles of a tile at the level `lod`, with its skirts, added to `indices`.
fn lod_indices(tile: &Tile, lod: usize, indices: &mut Vec<u16>) {
    for (place, chunk) in tile.chunks.iter().enumerate() {
        let base = (place * VERTICES) as u16;
        surface(lod, base, chunk.holes, indices);
        for side in 0..SIDES {
            if let Some(at) = along(side, chunk.index) {
                skirt_triangles(lod, side, at, base, indices);
            }
        }
    }
}

/// The triangles of a tile, the levels of detail one after the other, each with its skirts, and
/// the range of each.
pub fn indices(tile: &Tile) -> (Vec<u16>, [Range<u32>; LODS]) {
    let mut indices = Vec::new();
    let ranges = std::array::from_fn(|lod| {
        let start = indices.len() as u32;
        lod_indices(tile, lod, &mut indices);
        start..indices.len() as u32
    });
    (indices, ranges)
}

/// Every vertex of the tile `tile` at `id`, those of the skirts after those of the chunks.
pub fn tile_vertices(id: TileId, tile: &Tile) -> Vec<Vertex> {
    let mut all: Vec<Vertex> = tile
        .chunks
        .iter()
        .enumerate()
        .flat_map(|(place, chunk)| vertices(id, place, chunk))
        .collect();
    all.resize(TILE_VERTICES, Vertex::zeroed());
    for (place, chunk) in tile.chunks.iter().enumerate() {
        for (first, skirt) in skirt(id, place, chunk) {
            all[first..first + skirt.len()].copy_from_slice(&skirt);
        }
    }
    all
}

/// The mesh of a light tile: its coarsest level only, with the vertices it uses, numbered again.
pub fn light(id: TileId, tile: &Tile) -> (Vec<Vertex>, Vec<u16>) {
    let all = tile_vertices(id, tile);
    let mut indices = Vec::new();
    lod_indices(tile, LODS - 1, &mut indices);
    let mut renumbered = vec![u16::MAX; all.len()];
    let mut kept = Vec::new();
    for index in &mut indices {
        let old = usize::from(*index);
        if renumbered[old] == u16::MAX {
            renumbered[old] = kept.len() as u16;
            kept.push(all[old]);
        }
        *index = renumbered[old];
    }
    (kept, indices)
}

/// The level of detail of a tile whose nearest point is `distance` tiles away, `previous` its level
/// so far: a level changes only once the distance is past its limit by a margin, so that a camera
/// on a limit does not make it change at each frame.
pub fn lod(distance: f32, previous: Option<usize>) -> usize {
    const LIMITS: [f32; LODS - 1] = [1.5, 3.0, 6.0];
    const MARGIN: f32 = 0.15;
    let plain = LIMITS.iter().take_while(|limit| distance >= **limit).count();
    match previous {
        Some(previous) if plain > previous && distance < LIMITS[plain - 1] + MARGIN => plain - 1,
        Some(previous) if plain < previous && distance > LIMITS[plain] - MARGIN => plain + 1,
        _ => plain,
    }
}

/// The side of the texels of blending of a chunk of a light tile, from those of its 64.
pub const LIGHT_BLEND: usize = 16;

/// The texels of blending of a chunk of a light tile, `LIGHT_BLEND` a side: each the mean of a square
/// of those of `blend`.
pub fn light_blend(chunk: &Chunk) -> Vec<u8> {
    let full = blend(chunk);
    let step = 64 / LIGHT_BLEND;
    let mut texels = vec![0u8; LIGHT_BLEND * LIGHT_BLEND * 4];
    for row in 0..LIGHT_BLEND {
        for column in 0..LIGHT_BLEND {
            for channel in 0..4 {
                let sum: usize = (0..step)
                    .flat_map(|r| (0..step).map(move |c| ((row * step + r) * 64 + column * step + c) * 4 + channel))
                    .map(|at| usize::from(full[at]))
                    .sum();
                texels[(row * LIGHT_BLEND + column) * 4 + channel] = (sum / (step * step)) as u8;
            }
        }
    }
    texels
}

/// The alpha maps `alphas` of a chunk's layers after the first, each covering the layers before it
/// as the maps of 4 bits do, made the share of each layer, as the maps of 8 bits are: from the
/// last, each takes its alpha of what the layers after it leave.
pub fn shares(alphas: &mut [Vec<u8>]) {
    for texel in 0..64 * 64 {
        let mut left = 255u32;
        for map in alphas.iter_mut().rev() {
            if let Some(value) = map.get_mut(texel) {
                let share = (u32::from(*value) * left + 127) / 255;
                left -= share;
                *value = share as u8;
            }
        }
    }
}

/// The texels of blending of a chunk, 64 × 64, row by row: the share of each of its layers after the
/// first in red, green and blue, its alpha maps made shares already when its map stores them
/// otherwise (`shares`, by `read_tile`); its shadow in alpha.
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
