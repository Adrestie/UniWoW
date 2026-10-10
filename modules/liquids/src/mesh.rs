//! The meshes of the liquids of a tile and the surfaces of its water: each layer's vertices in the
//! world, those of water apart from those of magma and slime; and the height of the water over each
//! cell it covers, the mean of its corners. A layer whose vertices are all at one height and one
//! depth, its coordinates those by default, as are those without vertices, is drawn by a quad for
//! each rectangle of the cells it covers; another by two triangles a cell over its 9 × 9 vertices.
//! A liquid another module places is drawn as it gives it.

use uniwow_api::formats::{LIQUID_SIDE, LiquidLayer};
use uniwow_api::glam::Vec3;
use uniwow_api::liquids::{CELL, Placed, Surfaces};

/// The kinds of a type of liquid that are not water.
const MAGMA: u32 = 2;
const SLIME: u32 = 3;
/// How far over the surface of a liquid a point still lies in it, in yards (0x9F1968).
const OVER: f32 = 0.01;

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
/// magma and slime into them, and the cells of water with their heights.
#[derive(Debug, Default, PartialEq)]
pub struct Meshes {
    pub vertices: Vec<Vertex>,
    pub water: Vec<u32>,
    pub opaque: Vec<u32>,
    pub surfaces: Vec<([i32; 2], f32)>,
}

/// Whether every vertex of `layer` is at one height and one depth, its coordinates those by
/// default.
fn is_flat(layer: &LiquidLayer) -> bool {
    let same = |values: &[f32]| values.iter().all(|value| *value == values[0]);
    layer.coordinates.is_empty()
        && !layer.heights.is_empty()
        && same(&layer.heights)
        && layer.depths.iter().all(|depth| Some(depth) == layer.depths.first())
}

/// The rectangles of the cells `cells` covers, a bit a cell row by row, each its first row and
/// column and its rows and columns: the runs of each row, each joined to the same run of the row
/// before.
pub fn rectangles(cells: u64) -> Vec<[usize; 4]> {
    let covered = |row: usize, column: usize| cells >> (row * 8 + column) & 1 != 0;
    let mut done = Vec::new();
    let mut open: Vec<[usize; 4]> = Vec::new();
    for row in 0..8 {
        let mut next = Vec::new();
        let mut column = 0;
        while column < 8 {
            if !covered(row, column) {
                column += 1;
                continue;
            }
            let start = column;
            while column < 8 && covered(row, column) {
                column += 1;
            }
            let length = column - start;
            match open
                .iter()
                .position(|rectangle| rectangle[1] == start && rectangle[3] == length)
            {
                Some(at) => {
                    let mut rectangle = open.swap_remove(at);
                    rectangle[2] += 1;
                    next.push(rectangle);
                }
                None => next.push([row, start, 1, length]),
            }
        }
        done.append(&mut open);
        open = next;
    }
    done.append(&mut open);
    done
}

/// The meshes of `layers`, each layer's type given its slot in the table and whether it is water
/// by `kind`; none for a type it does not know.
pub fn meshes(layers: &[LiquidLayer], kind: impl Fn(u16) -> Option<(u32, bool)>) -> Meshes {
    let mut meshes = Meshes::default();
    for layer in layers {
        let Some((slot, water)) = kind(layer.liquid) else {
            continue;
        };
        // The vertex at `row` and `column` of the layer's grid.
        let vertex = |row: usize, column: usize| {
            let at = row * LIQUID_SIDE + column;
            Vertex {
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
            }
        };
        let indices = if water { &mut meshes.water } else { &mut meshes.opaque };
        if is_flat(layer) {
            for [row, column, rows, columns] in rectangles(layer.tiles) {
                let base = meshes.vertices.len() as u32;
                meshes.vertices.extend([
                    vertex(row, column),
                    vertex(row, column + columns),
                    vertex(row + rows, column + columns),
                    vertex(row + rows, column),
                ]);
                indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        } else {
            let base = meshes.vertices.len() as u32;
            for row in 0..LIQUID_SIDE {
                for column in 0..LIQUID_SIDE {
                    meshes.vertices.push(vertex(row, column));
                }
            }
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
                }
            }
        }
        if water {
            for row in 0..8 {
                for column in 0..8 {
                    if layer.tiles >> (row * 8 + column) & 1 == 0 {
                        continue;
                    }
                    let height = [
                        (row, column),
                        (row, column + 1),
                        (row + 1, column + 1),
                        (row + 1, column),
                    ]
                    .iter()
                    .map(|(r, c)| vertex(*r, *c).position[2])
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

/// The type of the first of `layers` the point `at` lies in, as the client finds the liquid the eye
/// is in (0x7A0820, 0x7CE1F0, 0x7CE0B0): the cell of its chunk holding the point covered, and the
/// point under its surface there, read between the heights of the corners of that cell along the
/// columns then the rows, or no more than `OVER` over it.
pub fn liquid_at(layers: &[LiquidLayer], at: [f32; 3]) -> Option<u16> {
    layers.iter().find_map(|layer| {
        let [row, column] = [(layer.corner[0] - at[0]) / CELL, (layer.corner[1] - at[1]) / CELL];
        if !(0.0..8.0).contains(&row) || !(0.0..8.0).contains(&column) {
            return None;
        }
        let [r, c] = [row as usize, column as usize];
        if layer.tiles >> (r * 8 + c) & 1 == 0 {
            return None;
        }
        let height = |r: usize, c: usize| layer.heights.get(r * LIQUID_SIDE + c).copied().unwrap_or_default();
        let [down, across] = [row - r as f32, column - c as f32];
        let near = height(r, c) + (height(r, c + 1) - height(r, c)) * across;
        let far = height(r + 1, c) + (height(r + 1, c + 1) - height(r + 1, c)) * across;
        let surface = near + (far - near) * down;
        (at[2] < surface + OVER).then_some(layer.liquid)
    })
}

/// The meshes of the liquid `placed` by another module, its type given its slot in the table and
/// whether it is water by `kind`; none for a type it does not know. The surfaces of its water by
/// the cells under the middle of each triangle and of its sides, at the heights there.
pub fn placed(placed: &Placed, kind: impl Fn(u16) -> Option<(u32, bool)>) -> Meshes {
    let mut meshes = Meshes::default();
    let Some((slot, water)) = kind(placed.liquid) else {
        return meshes;
    };
    meshes.vertices = placed
        .positions
        .iter()
        .enumerate()
        .map(|(at, position)| Vertex {
            position: *position,
            uv: placed.coordinates.get(at).copied().unwrap_or_default(),
            depth: placed.depths.get(at).copied().unwrap_or(1.0),
            slot,
        })
        .collect();
    let count = meshes.vertices.len() as u32;
    let triangles: Vec<[u32; 3]> = placed
        .triangles
        .as_chunks::<3>()
        .0
        .iter()
        .copied()
        .filter(|corners| corners.iter().all(|corner| *corner < count))
        .collect();
    let indices = if water { &mut meshes.water } else { &mut meshes.opaque };
    indices.extend(triangles.iter().flatten());
    if water {
        for corners in &triangles {
            let [a, b, c] = corners.map(|corner| Vec3::from(placed.positions[corner as usize]));
            for point in [(a + b + c) / 3.0, (a + b) / 2.0, (b + c) / 2.0, (c + a) / 2.0] {
                meshes.surfaces.push((Surfaces::cell(point.x, point.y), point.z));
            }
        }
    }
    meshes
}
