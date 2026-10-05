//! The horizon of a map, as the client draws it beyond the tiles it has loaded: the heights of its
//! WDL, 17 × 17 a tile, in one mesh drawn in one draw, built by a job when the map is shown. The
//! tiles drawn in detail are left out of it by a bit each, which the layer writes. Near the tiles
//! the WDL has no heights for, the horizon fades into the fog, where it would otherwise end on the
//! sky.

use std::sync::OnceLock;

use uniwow_api::bytemuck::{Pod, Zeroable};
use uniwow_api::formats::Wdl;
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, parallel_for, wgpu};

use crate::gpu::Shared;
use crate::model::{ORIGIN, TILE, TileId};

/// The heights of a tile a side, and how far apart they are, in yards.
const SIDE: usize = 17;
const SPACING: f32 = TILE / 16.0;

/// How far from a tile without heights the horizon fades into the fog, in tiles.
const FADE: f32 = 1.5;

/// A vertex of the horizon, as its shader reads it: 20 bytes, without padding.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HorizonVertex {
    pub position: [f32; 3],
    /// The normal, 127 for 1, then how much the vertex fades into the fog by its nearness to a tile
    /// without heights, 127 for all.
    pub normal: [i8; 4],
    /// The tile, at `y * 64 + x`.
    pub tile: u32,
}

// SAFETY: plain numbers laid out by `repr(C)` without padding, any bit pattern valid.
unsafe impl Zeroable for HorizonVertex {}
unsafe impl Pod for HorizonVertex {}

/// How much the point `x`, `y` of the world fades into the fog, from 0 to 1: all of it on the side
/// of a tile without heights, `present` saying which have them, and nothing from `FADE` tiles away.
fn fade(x: f32, y: f32, tile: TileId, present: &[bool]) -> f32 {
    // In tiles, as their names count them: x down Y, y down X.
    let (along_x, along_y) = ((ORIGIN - y) / TILE, (ORIGIN - x) / TILE);
    let reach = FADE.ceil() as i32;
    let mut nearest = FADE;
    for ty in tile.y as i32 - reach..=tile.y as i32 + reach {
        for tx in tile.x as i32 - reach..=tile.x as i32 + reach {
            let inside = (0..64).contains(&tx) && (0..64).contains(&ty);
            if inside && present[(ty * 64 + tx) as usize] {
                continue;
            }
            let dx = (tx as f32 - along_x).max(along_x - (tx + 1) as f32).max(0.0);
            let dy = (ty as f32 - along_y).max(along_y - (ty + 1) as f32).max(0.0);
            nearest = nearest.min((dx * dx + dy * dy).sqrt());
        }
    }
    1.0 - nearest / FADE
}

/// The vertices of the tile `tile` of the WDL, its heights `heights`, row by row as those of a chunk:
/// a row going down in Y, the rows down in X; its normals from the slopes around each, and how much
/// each fades into the fog, `present` saying which tiles have heights.
fn tile_vertices(tile: TileId, heights: &[i16], present: &[bool]) -> Vec<HorizonVertex> {
    let [x, y] = tile.corner();
    let height = |row: usize, column: usize| f32::from(heights[row.min(SIDE - 1) * SIDE + column.min(SIDE - 1)]);
    (0..SIDE * SIDE)
        .map(|index| {
            let (row, column) = (index / SIDE, index % SIDE);
            let (before, after) = (row.saturating_sub(1), (row + 1).min(SIDE - 1));
            // The next row is lower in X: the slope up X is the fall towards the next row.
            let up_x = (height(before, column) - height(after, column)) / ((after - before) as f32 * SPACING);
            let (before, after) = (column.saturating_sub(1), (column + 1).min(SIDE - 1));
            let up_y = (height(row, before) - height(row, after)) / ((after - before) as f32 * SPACING);
            let normal = uniwow_api::glam::Vec3::new(-up_x, -up_y, 1.0).normalize() * 127.0;
            let position = [x - row as f32 * SPACING, y - column as f32 * SPACING];
            let fade = (fade(position[0], position[1], tile, present) * 127.0).round() as i8;
            HorizonVertex {
                position: [position[0], position[1], height(row, column)],
                normal: [normal.x as i8, normal.y as i8, normal.z as i8, fade],
                tile: tile.y * 64 + tile.x,
            }
        })
        .collect()
}

/// The mesh of the horizon of `wdl`: the vertices of its tiles one after the other, and their
/// triangles, two a square of heights, facing up. The tiles are built over the threads of the pool.
pub fn mesh(wdl: &Wdl) -> (Vec<HorizonVertex>, Vec<u32>) {
    let tiles: Vec<(TileId, &Vec<i16>)> = wdl
        .tiles
        .iter()
        .enumerate()
        .filter_map(|(index, heights)| {
            let tile = TileId {
                x: index as u32 % 64,
                y: index as u32 / 64,
            };
            heights.as_ref().filter(|h| h.len() >= SIDE * SIDE).map(|h| (tile, h))
        })
        .collect();
    let mut present = vec![false; 64 * 64];
    for (tile, _) in &tiles {
        present[(tile.y * 64 + tile.x) as usize] = true;
    }
    let built: Vec<OnceLock<Vec<HorizonVertex>>> = tiles.iter().map(|_| OnceLock::new()).collect();
    parallel_for(tiles.len(), 16, |range| {
        for index in range {
            let (tile, heights) = tiles[index];
            let _ = built[index].set(tile_vertices(tile, heights, &present));
        }
    });
    let vertices: Vec<HorizonVertex> = built
        .into_iter()
        .flat_map(|cell| cell.into_inner().unwrap_or_default())
        .collect();
    let mut indices = Vec::with_capacity(tiles.len() * 16 * 16 * 6);
    for tile in 0..tiles.len() as u32 {
        let at = |row: u32, column: u32| tile * (SIDE * SIDE) as u32 + row * SIDE as u32 + column;
        for row in 0..16 {
            for column in 0..16 {
                let (a, b, c, d) = (
                    at(row, column),
                    at(row, column + 1),
                    at(row + 1, column + 1),
                    at(row + 1, column),
                );
                indices.extend_from_slice(&[d, b, a, d, c, b]);
            }
        }
    }
    (vertices, indices)
}

/// The horizon on the GPU.
pub struct HorizonGpu {
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    pub count: u32,
    pub bytes: u64,
}

/// The horizon of `wdl` uploaded, its uploads submitted; none when it has no tile.
pub fn build(shared: &Shared, wdl: &Wdl) -> Option<HorizonGpu> {
    let (vertices, indices) = mesh(wdl);
    if indices.is_empty() {
        return None;
    }
    let buffer = |label, contents: &[u8], usage| {
        shared.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage,
        })
    };
    let vertices = buffer(
        "terrain horizon vertices",
        bytemuck::cast_slice(&vertices),
        wgpu::BufferUsages::VERTEX,
    );
    let index_buffer = buffer(
        "terrain horizon indices",
        bytemuck::cast_slice(&indices),
        wgpu::BufferUsages::INDEX,
    );
    shared.queue.submit([]);
    Some(HorizonGpu {
        bytes: vertices.size() + index_buffer.size(),
        vertices,
        indices: index_buffer,
        count: indices.len() as u32,
    })
}

/// The bits of the tiles `drawn` in detail, as the shader of the horizon reads them: the tile at
/// `y * 64 + x` in the bit `x % 32` of the word `(y * 64 + x) / 32`.
pub fn mask(drawn: impl Iterator<Item = TileId>) -> [u32; 128] {
    let mut bits = [0u32; 128];
    for tile in drawn {
        let index = (tile.y * 64 + tile.x) as usize;
        bits[index / 32] |= 1 << (index % 32);
    }
    bits
}
