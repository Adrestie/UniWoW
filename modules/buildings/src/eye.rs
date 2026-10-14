//! What the light of the map takes from a building where the eye stands, in the building's axes:
//! the fog of the building at the eye in the group it is in, as the public implementation
//! WebWowViewerCpp mixes the fogs of a building (`checkFog`, read for its facts), how far the eye
//! is from the portals of that group, the sky the group shows, and the liquid the eye lies in, read
//! in the grid of the liquid of a group as the client reads that of the terrain (0x7CE0B0).

use uniwow_api::formats::{FileRef, FogBand, Wmo, WmoFog};
use uniwow_api::glam::{Vec3, Vec4Swizzles};
use uniwow_api::liquids::{CELL, Immersed};

use crate::cells::Cells;
use crate::liquid;

/// The flags of a fog: its radii left out, every fog of the group weighed as a mean, and with the
/// last the fogs of the building left out (`F_IEBLEND`, `F_FOGVOLUME`).
const ENDLESS: u32 = 0x1;
const VOLUME: u32 = 0x1000;
const NO_FOG: u32 = 0x10000;
/// The flags of a group: outside, lit as outside, the sky shown.
const OUTSIDE: u32 = 0x8;
const LIT_OUTSIDE: u32 = 0x40;
const SKY: u32 = 0x40000;
/// The flag of a cell of liquid not drawn.
const NOT_DRAWN: u8 = 0x8;
/// How far over the surface of a liquid a point is still in it, in yards, as the client takes it.
const OVER: f32 = 0.01;

/// The surface of the liquid of a group: its corner, its vertices and its cells across X and along
/// Y, the height of each vertex, and the type of each cell, none where it is not drawn.
pub struct Surface {
    corner: Vec3,
    vertices: [usize; 2],
    cells: [usize; 2],
    heights: Vec<f32>,
    types: Vec<Option<u16>>,
}

impl Surface {
    /// The liquid `point` lies in, and how deep under it: the cell under it drawn, and the point
    /// under its surface there, read between the heights of the corners of the cell along X then Y,
    /// or no more than a hundredth of a yard over it.
    pub fn liquid_at(&self, point: Vec3) -> Option<Immersed> {
        let [x, y] = [(point.x - self.corner.x) / CELL, (point.y - self.corner.y) / CELL];
        if !(0.0..self.cells[0] as f32).contains(&x) || !(0.0..self.cells[1] as f32).contains(&y) {
            return None;
        }
        let [column, row] = [x as usize, y as usize];
        let kind = (*self.types.get(row * self.cells[0] + column)?)?;
        let height = |row: usize, column: usize| {
            self.heights
                .get(row * self.vertices[0] + column)
                .copied()
                .unwrap_or(self.corner.z)
        };
        let [along, across] = [y - row as f32, x - column as f32];
        let near = height(row, column) + (height(row, column + 1) - height(row, column)) * across;
        let far = height(row + 1, column) + (height(row + 1, column + 1) - height(row + 1, column)) * across;
        let surface = near + (far - near) * along;
        (point.z < surface + OVER).then_some(Immersed {
            liquid: kind,
            depth: surface - point.z,
        })
    }
}

/// What the light of the map takes from a group: its flags, the fogs of its building it names, and
/// the surface of its liquid.
pub struct GroupLights {
    pub flags: u32,
    pub fogs: [u8; 4],
    pub surface: Option<Surface>,
}

/// What the light of the map takes from a building: its fogs, its groups, and the model of its sky.
pub struct Lights {
    pub fogs: Vec<WmoFog>,
    pub groups: Vec<GroupLights>,
    pub skybox: Option<FileRef>,
}

/// The weight of a fog `distance` yards from the eye, of the radii `radii`: whole within the first,
/// falling to nothing at the second.
fn weight(distance: f32, radii: [f32; 2]) -> f32 {
    let [smaller, larger] = radii;
    let distance = distance.max(0.0).min(larger);
    if distance < smaller || larger <= smaller {
        1.0
    } else {
        1.0 - (distance - smaller) / (larger - smaller)
    }
}

/// `to` mixed into `from` by `weight`, each channel by the bytes as the client mixes colours.
fn mix_colour(from: [u8; 4], to: [u8; 4], weight: f32) -> [u8; 4] {
    let share = (weight * 255.0 + 0.5) as i32;
    if share <= 0 {
        return from;
    }
    let mut mixed = from;
    for channel in 0..3 {
        mixed[channel] = if share >= 255 {
            to[channel]
        } else {
            let (a, b) = (i32::from(from[channel]), i32::from(to[channel]));
            (a + ((share * (b - a)) >> 8)) as u8
        };
    }
    mixed
}

/// `to` mixed into `from` by `weight`.
fn mix_band(from: FogBand, to: FogBand, weight: f32) -> FogBand {
    FogBand {
        end: from.end + (to.end - from.end) * weight,
        start: from.start + (to.start - from.start) * weight,
        colour: mix_colour(from.colour, to.colour, weight),
    }
}

impl Lights {
    pub fn new(wmo: &Wmo) -> Self {
        let groups = wmo
            .groups
            .iter()
            .map(|group| GroupLights {
                flags: group.flags,
                fogs: group.fogs,
                surface: group.liquid.as_ref().and_then(|grid| {
                    let vertices = grid.size.map(|side| side as usize);
                    let cells = grid.tiles.map(|side| side as usize);
                    if vertices[0] < 2 || vertices[1] < 2 || cells[0] + 1 > vertices[0] || cells[1] + 1 > vertices[1] {
                        return None;
                    }
                    Some(Surface {
                        corner: Vec3::from(grid.corner),
                        vertices,
                        cells,
                        heights: grid.heights.clone(),
                        types: grid
                            .tile_flags
                            .iter()
                            .map(|cell| {
                                (cell & NOT_DRAWN == 0).then(|| liquid::liquid_type(wmo.flags, group, cell & 0xF))
                            })
                            .collect(),
                    })
                }),
            })
            .collect();
        Self {
            fogs: wmo.fogs.clone(),
            groups,
            skybox: wmo.skybox.clone(),
        }
    }

    /// What it keeps on the CPU.
    pub fn bytes(&self) -> u64 {
        let surfaces: usize = self
            .groups
            .iter()
            .filter_map(|group| group.surface.as_ref())
            .map(|surface| surface.heights.len() * 4 + surface.types.len() * 4)
            .sum();
        (self.fogs.len() * size_of::<WmoFog>() + self.groups.len() * size_of::<GroupLights>() + surfaces) as u64
    }

    /// The fog of the building at `eye` in the group `group`, in the air then under the water, as
    /// WebWowViewerCpp mixes them (`checkFog`): its first fog, then those the group names, each
    /// whole within its first radius and falling to nothing at its second; one after the other,
    /// the farthest first, their colours by the bytes, or, where its first fog says so (0x1000), as
    /// a mean, the first fog filling what the others leave. None for a building of one fog alone,
    /// or whose first fog leaves them out.
    pub fn fog(&self, group: usize, eye: Vec3) -> Option<[FogBand; 2]> {
        let first = self.fogs.first()?;
        let volume = first.flags & VOLUME != 0;
        if (self.fogs.len() == 1 && !volume) || (volume && first.flags & NO_FOG != 0) {
            return None;
        }
        let named: Vec<usize> = self
            .groups
            .get(group)?
            .fogs
            .iter()
            .map(|index| usize::from(*index))
            .filter(|index| *index > 0 && *index < self.fogs.len())
            .collect();
        let distance = |fog: &WmoFog| (Vec3::from(fog.position) - eye).length();
        if volume {
            let mut total = 0.0;
            let mut sums = [[0.0f32; 5]; 2];
            let mut add = |fog: &WmoFog, share: f32| {
                for (sum, band) in sums.iter_mut().zip([fog.fog, fog.underwater]) {
                    let colour = band.colour.map(f32::from);
                    let values = [band.end, band.start, colour[0], colour[1], colour[2]];
                    for (sum, value) in sum.iter_mut().zip(values) {
                        *sum += value * share;
                    }
                }
            };
            for fog in named.iter().map(|index| &self.fogs[*index]) {
                let away = distance(fog);
                if away >= fog.radii[1] || fog.flags & ENDLESS != 0 {
                    continue;
                }
                let share = weight(away, fog.radii);
                total += share;
                add(fog, share);
            }
            if total < 1.0 {
                add(first, 1.0 - total);
                total = 1.0;
            }
            let band = |sums: [f32; 5]| FogBand {
                end: sums[0] / total,
                start: sums[1] / total,
                colour: [
                    (sums[2] / total).round().clamp(0.0, 255.0) as u8,
                    (sums[3] / total).round().clamp(0.0, 255.0) as u8,
                    (sums[4] / total).round().clamp(0.0, 255.0) as u8,
                    255,
                ],
            };
            return Some([band(sums[0]), band(sums[1])]);
        }
        let mut near: Vec<(f32, &WmoFog)> = named
            .iter()
            .map(|index| &self.fogs[*index])
            .map(|fog| (distance(fog), fog))
            .filter(|(away, fog)| *away < fog.radii[1] && fog.flags & ENDLESS == 0)
            .collect();
        near.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut mixed = [first.fog, first.underwater];
        for (away, fog) in near {
            let share = weight(away, fog.radii);
            mixed = [
                mix_band(mixed[0], fog.fog, share),
                mix_band(mixed[1], fog.underwater, share),
            ];
        }
        Some(mixed)
    }

    /// The liquid of the group `group` that `eye` lies in.
    pub fn liquid(&self, group: usize, eye: Vec3) -> Option<Immersed> {
        self.groups.get(group)?.surface.as_ref()?.liquid_at(eye)
    }

    /// The liquid of a group outside whose bounds in `cells` hold `eye`, that `eye` lies in.
    pub fn liquid_outside(&self, cells: &Cells, eye: Vec3) -> Option<Immersed> {
        self.groups.iter().zip(&cells.groups).find_map(|(group, cell)| {
            let [low, high] = cell.bounds;
            if group.flags & OUTSIDE == 0 || eye.cmplt(low).any() || eye.cmpgt(high).any() {
                return None;
            }
            group.surface.as_ref()?.liquid_at(eye)
        })
    }

    /// Whether the group `group` is a room of the building, neither outside nor lit as outside.
    pub fn room(&self, group: usize) -> bool {
        self.groups
            .get(group)
            .is_some_and(|group| group.flags & (OUTSIDE | LIT_OUTSIDE) == 0)
    }

    /// The model of the sky of the building, when the group `group` shows it.
    pub fn skybox(&self, group: usize) -> Option<FileRef> {
        let group = self.groups.get(group)?;
        (group.flags & SKY != 0).then(|| self.skybox.clone()).flatten()
    }
}

/// The distance from `point` to the polygon `polygon`: to its plane where its projection falls
/// within it, to its nearest edge otherwise; none for a polygon of fewer than three points or
/// without a plane.
fn to_polygon(point: Vec3, polygon: &[Vec3]) -> Option<f32> {
    if polygon.len() < 3 {
        return None;
    }
    let normal = (polygon[1] - polygon[0]).cross(polygon[2] - polygon[0]);
    let length = normal.length();
    if length < 1e-6 {
        return None;
    }
    let normal = normal / length;
    let off = normal.dot(point - polygon[0]);
    let projected = point - normal * off;
    let edges = || (0..polygon.len()).map(|at| (polygon[at], polygon[(at + 1) % polygon.len()]));
    if edges().all(|(a, b)| (b - a).cross(projected - a).dot(normal) >= 0.0) {
        return Some(off.abs());
    }
    edges()
        .map(|(a, b)| {
            let along = b - a;
            let length = along.length_squared();
            let share = if length > 0.0 {
                ((point - a).dot(along) / length).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (point - (a + along * share)).length()
        })
        .reduce(f32::min)
}

/// How far `eye` is from the nearest portal of the group `group` of `cells`, in yards, as
/// WebWowViewerCpp measures it (`distanceToPortalPolygon`); infinite for a group without one.
pub fn exit(cells: &Cells, group: usize, eye: Vec3) -> f32 {
    let Some(cell) = cells.groups.get(group) else {
        return f32::INFINITY;
    };
    cells
        .passages
        .get(cell.portals.clone())
        .unwrap_or_default()
        .iter()
        .filter_map(|passage| cells.gates.get(passage.gate))
        .filter(|gate| gate.plane.xyz().is_finite())
        .filter_map(|gate| to_polygon(eye, &gate.polygon))
        .fold(f32::INFINITY, f32::min)
}
