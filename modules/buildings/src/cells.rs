//! The groups of a building seen from inside, in its own axes. The group the camera is in: among
//! the groups inside whose bounds hold it, those with a triangle of their BSP tree under it facing
//! up and one over it facing down, or for one open to the sky its floor near under it; the one
//! whose floor is the nearest under it. A camera over a roof within the bounds of a room, or high
//! over a street, is not in it. From it, the groups seen through the portals: a
//! portal passed from the side of the group listing it, clipped by what the camera sees through the
//! portals before it, which then sees through its sides only; to a group outside, the outside is
//! seen, and through the portals of the groups outside the groups inside them.

use std::ops::Range;

use uniwow_api::formats::{BspNode, Wmo};
use uniwow_api::glam::{Vec3, Vec4, Vec4Swizzles};

/// The flags of a group: outside; lit as outside, inside but open to the sky, as the streets of a
/// city are. The flag of a leaf of a BSP tree.
const OUTSIDE: u32 = 0x8;
const OPEN: u32 = 0x40;
const LEAF: u16 = 0x4;
/// The most portals passed one after the other.
const DEPTH: usize = 8;
/// Nearer its plane than this, in yards, a portal lets the camera see as it does, unclipped.
const NEAR: f32 = 0.5;
/// How far the bounds of a group are taken past their faces, in yards.
const MARGIN: f32 = 0.1;
/// The most the camera rises over the floor of a group open to the sky and is in it, in yards:
/// below the eaves of the houses along a street, whose portals reach 15 to 19 yards over it.
const OPEN_HEIGHT: f32 = 8.0;

/// A group: its bounds, whether outside, whether open to the sky, its portals among the
/// references, and the triangles of its tree for one inside.
pub struct Cell {
    pub bounds: [Vec3; 2],
    pub outside: bool,
    pub open: bool,
    pub portals: Range<usize>,
    pub floor: Option<Floor>,
}

/// The BSP tree of a group inside and the triangles it places, with the normals of their vertices.
pub struct Floor {
    pub nodes: Vec<BspNode>,
    pub faces: Vec<u16>,
    pub vertices: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub triangles: Vec<u16>,
}

/// A portal: its polygon and its plane, a point on it where `n·p + d` is 0; none passed when its
/// plane is not finite.
pub struct Gate {
    pub polygon: Vec<Vec3>,
    pub plane: Vec4,
}

/// A portal as a group lists it: the portal, the group on its other side, and the side of its plane
/// the group listing it lies on.
#[derive(Clone, Copy)]
pub struct Passage {
    pub gate: usize,
    pub group: usize,
    pub side: f32,
}

/// The groups of a building, its portals and how they join.
pub struct Cells {
    pub groups: Vec<Cell>,
    pub gates: Vec<Gate>,
    pub passages: Vec<Passage>,
}

impl Floor {
    /// The height of the nearest triangle of the tree under `eye` facing up, and whether one over
    /// it faces down.
    pub fn around(&self, eye: Vec3) -> (Option<f32>, bool) {
        let (mut under, mut over): (Option<f32>, bool) = (None, false);
        let mut nodes = vec![0usize];
        while let Some(index) = nodes.pop() {
            let Some(node) = self.nodes.get(index) else {
                continue;
            };
            if node.flags & LEAF != 0 {
                let first = node.first as usize;
                for face in self.faces.iter().skip(first).take(usize::from(node.count)) {
                    let Some((height, up)) = self.height(usize::from(*face), eye) else {
                        continue;
                    };
                    if height <= eye.z && up > 0.0 {
                        under = Some(under.map_or(height, |at| at.max(height)));
                    } else if height > eye.z && up < 0.0 {
                        over = true;
                    }
                }
                continue;
            }
            // The vertical line through the eye: on the side of the eye of a plane across X or Y,
            // both where it lies on it; on both sides of a plane across Z.
            let [negative, positive] = node.children;
            let axis = usize::from(node.flags & 0x3).min(2);
            let at = eye[axis] - node.distance;
            if (axis == 2 || at <= 0.0) && negative >= 0 {
                nodes.push(negative as usize);
            }
            if (axis == 2 || at >= 0.0) && positive >= 0 {
                nodes.push(positive as usize);
            }
        }
        (under, over)
    }

    /// The height of the triangle `face` at the place of `eye` on the ground, where it lies over
    /// it, and how much its normal there points up; none for a triangle standing upright.
    fn height(&self, face: usize, eye: Vec3) -> Option<(f32, f32)> {
        let index = |at: usize| self.triangles.get(face * 3 + at).map(|index| usize::from(*index));
        let (ia, ib, ic) = (index(0)?, index(1)?, index(2)?);
        let (a, b, c) = (
            *self.vertices.get(ia)?,
            *self.vertices.get(ib)?,
            *self.vertices.get(ic)?,
        );
        let up = |at: usize| self.normals.get(at).map_or(0.0, |normal| normal.z);
        let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
        if area.abs() < 1e-6 {
            return None;
        }
        let u = ((b.x - eye.x) * (c.y - eye.y) - (c.x - eye.x) * (b.y - eye.y)) / area;
        let v = ((c.x - eye.x) * (a.y - eye.y) - (a.x - eye.x) * (c.y - eye.y)) / area;
        let w = 1.0 - u - v;
        (u >= 0.0 && v >= 0.0 && w >= 0.0).then(|| (u * a.z + v * b.z + w * c.z, u * up(ia) + v * up(ib) + w * up(ic)))
    }
}

/// The part of `polygon` on the inner side of `plane`, where `plane · (p, 1)` is not negative.
pub fn clip(polygon: &[Vec3], plane: Vec4) -> Vec<Vec3> {
    let side = |point: Vec3| plane.xyz().dot(point) + plane.w;
    let mut kept = Vec::with_capacity(polygon.len() + 1);
    for (index, point) in polygon.iter().enumerate() {
        let next = polygon[(index + 1) % polygon.len()];
        let (here, there) = (side(*point), side(next));
        if here >= 0.0 {
            kept.push(*point);
        }
        if (here >= 0.0) != (there >= 0.0) {
            kept.push(*point + (next - *point) * (here / (here - there)));
        }
    }
    kept
}

/// The planes through `eye` and each side of `polygon`, the polygon on their inner side; and the
/// plane of the polygon itself, `eye` on its outer side, so that only what lies past it is seen.
pub fn through(eye: Vec3, polygon: &[Vec3], plane: Vec4) -> Vec<Vec4> {
    let centre = polygon.iter().copied().sum::<Vec3>() / polygon.len() as f32;
    let mut planes: Vec<Vec4> = polygon
        .iter()
        .enumerate()
        .filter_map(|(index, point)| {
            let next = polygon[(index + 1) % polygon.len()];
            let normal = (*point - eye).cross(next - eye);
            let length = normal.length();
            (length > 1e-6).then(|| {
                let normal = normal / length;
                let normal = if normal.dot(centre - eye) < 0.0 {
                    -normal
                } else {
                    normal
                };
                normal.extend(-normal.dot(eye))
            })
        })
        .collect();
    let beyond = if plane.xyz().dot(eye) + plane.w > 0.0 {
        -plane
    } else {
        plane
    };
    planes.push(beyond);
    planes
}

impl Cells {
    /// The groups of `wmo`, its portals and how they join; the trees of its groups inside, with
    /// their triangles.
    pub fn new(wmo: &Wmo) -> Self {
        let groups = wmo
            .groups
            .iter()
            .map(|group| {
                let [first, count] = group.portals.map(usize::from);
                let first = first.min(wmo.portal_refs.len());
                let outside = group.flags & OUTSIDE != 0;
                Cell {
                    bounds: group.bounds.map(Vec3::from),
                    outside,
                    open: group.flags & OPEN != 0,
                    portals: first..(first + count).min(wmo.portal_refs.len()),
                    floor: (!outside && !group.bsp.is_empty()).then(|| Floor {
                        nodes: group.bsp.clone(),
                        faces: group.bsp_faces.clone(),
                        vertices: group.vertices.iter().map(|vertex| Vec3::from(*vertex)).collect(),
                        normals: group.normals.iter().map(|normal| Vec3::from(*normal)).collect(),
                        triangles: group.triangles.clone(),
                    }),
                }
            })
            .collect();
        let gates = wmo
            .portals
            .iter()
            .map(|portal| Gate {
                polygon: portal.vertices.iter().map(|vertex| Vec3::from(*vertex)).collect(),
                plane: Vec4::from(portal.plane),
            })
            .collect();
        let passages = wmo
            .portal_refs
            .iter()
            .map(|reference| Passage {
                gate: usize::from(reference.portal),
                group: usize::from(reference.group),
                side: f32::from(reference.side.signum()),
            })
            .collect();
        Self {
            groups,
            gates,
            passages,
        }
    }

    /// What it keeps on the CPU.
    pub fn bytes(&self) -> u64 {
        let floors: usize = self
            .groups
            .iter()
            .filter_map(|cell| cell.floor.as_ref())
            .map(|floor| {
                floor.nodes.len() * size_of::<BspNode>()
                    + floor.faces.len() * 2
                    + (floor.vertices.len() + floor.normals.len()) * size_of::<Vec3>()
                    + floor.triangles.len() * 2
            })
            .sum();
        let gates: usize = self
            .gates
            .iter()
            .map(|gate| gate.polygon.len() * size_of::<Vec3>())
            .sum();
        (self.groups.len() * size_of::<Cell>()
            + self.gates.len() * size_of::<Gate>()
            + self.passages.len() * size_of::<Passage>()
            + floors
            + gates) as u64
    }

    /// The group inside that `eye` is in: of those whose bounds hold it, with a triangle of their
    /// tree under it facing up and one over it facing down, or for one open to the sky that
    /// triangle under it within `OPEN_HEIGHT`, the one whose triangle under it is the nearest.
    pub fn holding(&self, eye: Vec3) -> Option<usize> {
        let mut found: Option<(f32, usize)> = None;
        for (index, cell) in self.groups.iter().enumerate() {
            let [low, high] = cell.bounds;
            if eye.cmplt(low - Vec3::splat(MARGIN)).any() || eye.cmpgt(high + Vec3::splat(MARGIN)).any() {
                continue;
            }
            let Some(floor) = &cell.floor else {
                continue;
            };
            if let (Some(under), over) = floor.around(eye)
                && (over || cell.open && eye.z - under <= OPEN_HEIGHT)
            {
                let gap = eye.z - under;
                if found.is_none_or(|(nearest, _)| gap < nearest) {
                    found = Some((gap, index));
                }
            }
        }
        found.map(|(_, index)| index)
    }

    /// The groups seen from `eye` in the group `start`, the view given by `planes`: by the
    /// portals passed from it, and when they lead outside, the groups outside and those inside
    /// their portals.
    pub fn seen(&self, eye: Vec3, planes: &[Vec4], start: usize) -> Vec<bool> {
        let mut seen = vec![false; self.groups.len()];
        if start >= seen.len() {
            return seen;
        }
        seen[start] = true;
        let mut outside = false;
        self.pass(eye, start, planes, &mut Vec::new(), &mut seen, &mut outside);
        if outside {
            let mut again = false;
            for (index, cell) in self.groups.iter().enumerate() {
                if cell.outside {
                    seen[index] = true;
                    self.pass(eye, index, planes, &mut Vec::new(), &mut seen, &mut again);
                }
            }
        }
        seen
    }

    /// Passes the portals of the group `from` seen through `planes`, the portals of `path` passed
    /// before; the groups reached marked in `seen`, `outside` set when one is outside.
    fn pass(
        &self,
        eye: Vec3,
        from: usize,
        planes: &[Vec4],
        path: &mut Vec<usize>,
        seen: &mut [bool],
        outside: &mut bool,
    ) {
        if path.len() >= DEPTH {
            return;
        }
        let Some(cell) = self.groups.get(from) else {
            return;
        };
        for passage in self.passages.get(cell.portals.clone()).unwrap_or_default() {
            let (Some(gate), Some(next)) = (self.gates.get(passage.gate), self.groups.get(passage.group)) else {
                continue;
            };
            if path.contains(&passage.gate) || !gate.plane.is_finite() || gate.polygon.len() < 3 {
                continue;
            }
            // Seen from the side of the group listing it only.
            let side = gate.plane.xyz().dot(eye) + gate.plane.w;
            let near = side.abs() < NEAR;
            if !near && side * passage.side < 0.0 {
                continue;
            }
            let through_it = if near {
                planes.to_vec()
            } else {
                let mut polygon = gate.polygon.clone();
                for plane in planes {
                    polygon = clip(&polygon, *plane);
                    if polygon.len() < 3 {
                        break;
                    }
                }
                if polygon.len() < 3 {
                    continue;
                }
                through(eye, &polygon, gate.plane)
            };
            if next.outside {
                *outside = true;
                continue;
            }
            seen[passage.group] = true;
            path.push(passage.gate);
            self.pass(eye, passage.group, &through_it, path, seen, outside);
            path.pop();
        }
    }
}
