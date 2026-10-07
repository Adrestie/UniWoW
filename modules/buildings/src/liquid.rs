//! The liquids of the groups of a building, in its axes: each group's grid of vertices from its
//! corner, a cell of liquid apart (an eighth of a chunk), the cells it covers two triangles each;
//! the `LiquidType` of each cell as the client takes it, read for the facts in the public
//! description of the format and in Noggit: of a building whose flag 0x4 names the types of
//! `LiquidType.dbc`, the group's own type, or below 21 its basic kind; otherwise the basic kind of
//! the group's type, or of each cell's for the type 15. A basic kind (its two low bits) is the water
//! of the buildings (13), or their ocean (14) for a group of the flag 0x80000, their magma (19) or
//! their slime (20). The water's vertices give their depth, those of magma and slime their
//! coordinates.

use uniwow_api::formats::{Wmo, WmoGroup};
use uniwow_api::liquids::CELL;

/// The flag of a building naming the types of `LiquidType.dbc`, and that of a group whose water is
/// an ocean.
const TYPES_OF_THE_TABLE: u16 = 0x4;
const OCEAN: u32 = 0x80000;
/// The first type of `LiquidType.dbc` that is not of a basic kind, and the type read by cell.
const FIRST_NOT_BASIC: u32 = 21;
const BY_CELL: u32 = 15;
/// The flag of a cell not drawn.
const NOT_DRAWN: u8 = 0x8;
/// The types of the water, the ocean, the magma and the slime of the buildings.
const WATER: u16 = 13;
const SEA: u16 = 14;
const MAGMA: u16 = 19;
const SLIME: u16 = 20;

/// The type of `LiquidType.dbc` of the basic kind `kind`, its two low bits, in a group of the ocean
/// when `ocean`.
fn basic(kind: u32, ocean: bool) -> u16 {
    match kind & 3 {
        0 if ocean => SEA,
        0 => WATER,
        1 => SEA,
        2 => MAGMA,
        _ => SLIME,
    }
}

/// The type of `LiquidType.dbc` of a cell of flags `cell` of the liquid of `group`, in a building of
/// flags `flags`.
pub fn liquid_type(flags: u16, group: &WmoGroup, cell: u8) -> u16 {
    let (kind, ocean) = (group.liquid_type, group.flags & OCEAN != 0);
    if flags & TYPES_OF_THE_TABLE != 0 {
        if kind < FIRST_NOT_BASIC {
            basic(kind.wrapping_sub(1), ocean)
        } else {
            kind as u16
        }
    } else if kind == BY_CELL {
        basic(u32::from(cell), ocean)
    } else if kind < u32::from(SLIME) {
        basic(kind, ocean)
    } else {
        (kind + 1) as u16
    }
}

/// The liquid of a group of a building, of one type, in the building's axes: its group, its type,
/// its vertices (place, coordinates, depth) and its triangles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupLiquid {
    pub group: u16,
    pub liquid: u16,
    pub positions: Vec<[f32; 3]>,
    pub coordinates: Vec<[f32; 2]>,
    pub depths: Vec<f32>,
    pub triangles: Vec<u32>,
}

/// The liquids of the groups of `wmo`, a liquid for each type a group's cells drawn have; the water
/// two repeats of its texture a chunk, as the tiles have it.
pub fn liquids(wmo: &Wmo) -> Vec<GroupLiquid> {
    let mut liquids: Vec<GroupLiquid> = Vec::new();
    for (index, group) in wmo.groups.iter().enumerate() {
        let Some(liquid) = &group.liquid else {
            continue;
        };
        let [across, along] = liquid.size.map(|side| side as usize);
        let [cells_across, cells_along] = liquid.tiles.map(|side| side as usize);
        if across < 2 || along < 2 || cells_across + 1 > across || cells_along + 1 > along {
            continue;
        }
        let first = liquids.len();
        for row in 0..cells_along {
            for column in 0..cells_across {
                let Some(&cell) = liquid.tile_flags.get(row * cells_across + column) else {
                    continue;
                };
                if cell & NOT_DRAWN != 0 {
                    continue;
                }
                let kind = liquid_type(wmo.flags, group, cell & 0xF);
                let at = match liquids[first..].iter().position(|made| made.liquid == kind) {
                    Some(at) => first + at,
                    None => {
                        liquids.push(GroupLiquid {
                            group: index as u16,
                            liquid: kind,
                            ..GroupLiquid::default()
                        });
                        liquids.len() - 1
                    }
                };
                let made = &mut liquids[at];
                let base = made.positions.len() as u32;
                for (r, c) in [
                    (row, column),
                    (row, column + 1),
                    (row + 1, column + 1),
                    (row + 1, column),
                ] {
                    let vertex = r * across + c;
                    let height = liquid.heights.get(vertex).copied().unwrap_or(liquid.corner[2]);
                    let data = liquid.data.get(vertex).copied().unwrap_or_default();
                    made.positions.push([
                        liquid.corner[0] + c as f32 * CELL,
                        liquid.corner[1] + r as f32 * CELL,
                        height,
                    ]);
                    if kind == MAGMA || kind == SLIME {
                        made.coordinates.push([
                            f32::from(u16::from_le_bytes([data[0], data[1]])) / 255.0,
                            f32::from(u16::from_le_bytes([data[2], data[3]])) / 255.0,
                        ]);
                        made.depths.push(1.0);
                    } else {
                        made.coordinates.push([c as f32 / 4.0, r as f32 / 4.0]);
                        made.depths.push(f32::from(data[0]) / 255.0);
                    }
                }
                made.triangles
                    .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
    }
    liquids
}
