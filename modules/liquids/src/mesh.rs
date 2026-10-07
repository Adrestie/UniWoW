//! The meshes of the liquids of a tile and the surfaces of its water: each layer's vertices in the
//! world, two triangles for each tile of its chunk it covers, those of water apart from those of
//! magma and slime; and the height of the water over each tile it covers, the mean of its corners.

use uniwow_api::formats::{LIQUID_SIDE, LiquidLayer};
use uniwow_api::liquids::{CELL, Surfaces};

/// The kinds of a type of liquid that are not water.
const MAGMA: u32 = 2;
const SLIME: u32 = 3;

/// Whether a type of liquid of `kind` is water, drawn blended, rather than magma or slime.
pub fn is_water(kind: u32) -> bool {
    kind != MAGMA && kind != SLIME
}

/// A vertex of a liquid: its place in the world, its coordinates of texture, its depth from 0 to
/// 1, and the slot of its type in the table of the shader.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub depth: f32,
    pub slot: u32,
}

// SAFETY: plain numbers laid out by `repr(C)` without padding, any bit pattern valid.
unsafe impl uniwow_api::bytemuck::Zeroable for Vertex {}
unsafe impl uniwow_api::bytemuck::Pod for Vertex {}

/// The meshes of the liquids of a tile: their vertices, the indices of the water and those of the
/// magma and slime into them, and the tiles of water with their heights.
#[derive(Debug, Default, PartialEq)]
pub struct Meshes {
    pub vertices: Vec<Vertex>,
    pub water: Vec<u32>,
    pub opaque: Vec<u32>,
    pub surfaces: Vec<([i32; 2], f32)>,
}

/// The meshes of `layers`, each layer's type given its slot in the table and whether it is water
/// by `kind`; none for a type it does not know.
pub fn meshes(layers: &[LiquidLayer], kind: impl Fn(u16) -> Option<(u32, bool)>) -> Meshes {
    let mut meshes = Meshes::default();
    for layer in layers {
        let Some((slot, water)) = kind(layer.liquid) else {
            continue;
        };
        let base = meshes.vertices.len() as u32;
        for row in 0..LIQUID_SIDE {
            for column in 0..LIQUID_SIDE {
                let at = row * LIQUID_SIDE + column;
                meshes.vertices.push(Vertex {
                    position: [
                        layer.corner[0] - row as f32 * CELL,
                        layer.corner[1] - column as f32 * CELL,
                        layer.heights.get(at).copied().unwrap_or_default(),
                    ],
                    // Two repeats of a texture a chunk, where the layer gives no coordinates.
                    uv: layer
                        .coordinates
                        .get(at)
                        .copied()
                        .unwrap_or([column as f32 / 4.0, row as f32 / 4.0]),
                    depth: f32::from(layer.depths.get(at).copied().unwrap_or(255)) / 255.0,
                    slot,
                });
            }
        }
        let indices = if water { &mut meshes.water } else { &mut meshes.opaque };
        for row in 0..8 {
            for column in 0..8 {
                if layer.tiles >> (row * 8 + column) & 1 == 0 {
                    continue;
                }
                let corner = |r: usize, c: usize| base + (r * LIQUID_SIDE + c) as u32;
                let [a, b, c, d] = [
                    corner(row, column),
                    corner(row, column + 1),
                    corner(row + 1, column + 1),
                    corner(row + 1, column),
                ];
                indices.extend([a, b, c, a, c, d]);
                if water {
                    let height = [a, b, c, d]
                        .iter()
                        .map(|index| meshes.vertices[*index as usize].position[2])
                        .sum::<f32>()
                        / 4.0;
                    let x = layer.corner[0] - (row as f32 + 0.5) * CELL;
                    let y = layer.corner[1] - (column as f32 + 0.5) * CELL;
                    meshes.surfaces.push((Surfaces::cell(x, y), height));
                }
            }
        }
    }
    meshes
}
