//! The gizmos of the 3D view (step 15.6), drawn and handled as Unity's, of the same size on the
//! screen wherever they stand. *Move* (W): three arrows, the squares of the planes towards the eye,
//! the view's plane with Shift; *Rotate* (E): the three rings about the axes, their halves towards
//! the eye, the ring of the view around them, a free turn inside; *Scale* (R): three axes ended by
//! cubes and the cube of the middle. The handle under the pointer is lit, the one dragged yellow;
//! an axis pointing at the eye fades out. About the middle of the selection or the active one's
//! pivot, in the axes of the world or of the active one. Ctrl snaps to the yard, to 15 degrees and
//! to tenths.

use uniwow_api::egui::{self, Color32, Pos2, Vec2, pos2};
use uniwow_api::glam::{Quat, Vec3};
use uniwow_api::viewport::Seen;

/// The gizmo shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Select,
    Move,
    Rotate,
    Scale,
}

/// About what the gizmo stands, as Unity's toggle: the middle of the bounds of the selection, each
/// one turned and scaled about it; or the pivot of the active one, each one about its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pivot {
    #[default]
    Center,
    Pivot,
}

/// The axes of the gizmo: the world's, or those of the active entity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Space {
    #[default]
    Global,
    Local,
}

/// A handle of a gizmo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// An arrow of *Move*, along the axis 0, 1 or 2 of the gizmo.
    Axis(usize),
    /// The square of *Move* of the plane across the axis 0, 1 or 2.
    Plane(usize),
    /// The square of *Move* in the middle, with Shift: in the plane of the view.
    Screen,
    /// A ring of *Rotate*, about the axis 0, 1 or 2.
    Ring(usize),
    /// The ring of *Rotate* around the others: about the axis of the view.
    Outer,
    /// Inside the rings of *Rotate*: turned freely, as a ball.
    Free,
    /// An axis of *Scale*, ended by its cube.
    Stretch(usize),
    /// The cube of *Scale* in the middle: every axis.
    Uniform,
}

/// The length of an arrow and the radius of the rings on the screen, in points; how near the
/// pointer must be to grab a handle.
pub const LENGTH: f32 = 90.0;
const GRAB: f32 = 7.0;
/// The steps of the snapping: a yard, 15 degrees, a tenth.
pub const STEPS: [f32; 3] = [1.0, 15.0, 0.1];

/// The colour of the handle dragged and of one under the pointer, as Unity's.
pub const ACTIVE: Color32 = Color32::from_rgb(246, 226, 50);
pub const HOVER: Color32 = Color32::from_rgb(255, 242, 150);
/// The colour of what is about the view: the ring of the view, the middle of *Scale*.
const VIEW: Color32 = Color32::from_rgb(204, 204, 204);

/// The colour of each axis, as Unity's: X red, Y green, Z blue.
pub fn colour(axis: usize) -> Color32 {
    [
        Color32::from_rgb(219, 62, 29),
        Color32::from_rgb(154, 243, 72),
        Color32::from_rgb(58, 122, 237),
    ][axis.min(2)]
}

/// Where the gizmo stands and how it is seen: its middle, its axes, the yards of an arrow, and the
/// directions of the view in the world (towards the eye, to the right of the screen and up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub centre: Vec3,
    pub axes: [Vec3; 3],
    pub size: f32,
    pub to_eye: Vec3,
    pub right: Vec3,
    pub up: Vec3,
}

impl Frame {
    /// The gizmo at `centre`, its axes those of the world turned by `turn`, seen in `seen`; none
    /// where it is behind the eye.
    pub fn new(seen: &Seen, centre: Vec3, turn: Quat) -> Option<Frame> {
        let to_eye = (seen.eye - centre).try_normalize()?;
        let right = (-to_eye).cross(Vec3::Z).try_normalize().unwrap_or(Vec3::X);
        let up = right.cross(-to_eye);
        let [x0, y0] = seen.project(centre)?;
        let points = |yards: f32| {
            let [x1, y1] = seen.project(centre + right * yards)?;
            Some((x1 - x0).hypot(y1 - y0))
        };
        // The yards of `LENGTH` points, measured again at that length, the view not quite linear.
        let pixels = points(1.0)?;
        if pixels <= 1e-4 {
            return None;
        }
        let guess = LENGTH / pixels;
        let size = guess * LENGTH / points(guess).filter(|measured| *measured > 1e-4)?;
        Some(Frame {
            centre,
            axes: [turn * Vec3::X, turn * Vec3::Y, turn * Vec3::Z],
            size,
            to_eye,
            right,
            up,
        })
    }

    /// The gizmo moved by `by`, its axes turned by `turn`, as it follows what it drags.
    pub fn moved(self, by: Vec3, turn: Quat) -> Frame {
        Frame {
            centre: self.centre + by,
            axes: self.axes.map(|axis| turn * axis),
            ..self
        }
    }
}

/// How much an axis pointing nearly at the eye is seen, from 1 to nothing, as Unity fades it.
fn facing_fade(axis: Vec3, to_eye: Vec3) -> f32 {
    let along = axis.dot(to_eye).abs();
    ((0.985 - along) / (0.985 - 0.9)).clamp(0.0, 1.0)
}

/// How much a plane seen nearly edge on is seen, from 1 to nothing.
fn edge_fade(normal: Vec3, to_eye: Vec3) -> f32 {
    ((normal.dot(to_eye).abs() - 0.05) / (0.2 - 0.05)).clamp(0.0, 1.0)
}

/// The shape of a handle on the screen, in points from the top left of the view.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// Lines, grabbed near them.
    Lines(Vec<Vec<Pos2>>, f32),
    /// A convex polygon, filled, grabbed inside; with its outline when given.
    Area(Vec<Pos2>, Option<f32>),
    /// A disc, faintly filled, grabbed inside.
    Disc(Pos2, f32),
}

/// A part of a gizmo: its handle, its shape, its colour, how much it is seen, and whether the
/// pointer takes it last, only where nothing else is.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub handle: Handle,
    pub shape: Shape,
    pub colour: Color32,
    pub seen: f32,
    pub last: bool,
}

fn project(seen: &Seen, point: Vec3) -> Option<Pos2> {
    seen.project(point).map(|[x, y]| pos2(x, y))
}

/// The convex hull of `points`, counterclockwise on the screen.
fn hull(mut points: Vec<Pos2>) -> Vec<Pos2> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let cross = |o: Pos2, a: Pos2, b: Pos2| (a - o).x * (b - o).y - (a - o).y * (b - o).x;
    let mut lower: Vec<Pos2> = Vec::new();
    for &point in &points {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], point) <= 0.0 {
            lower.pop();
        }
        lower.push(point);
    }
    let mut upper: Vec<Pos2> = Vec::new();
    for &point in points.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], point) <= 0.0 {
            upper.pop();
        }
        upper.push(point);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Two axes across `axis`, of length 1.
fn across(axis: Vec3) -> (Vec3, Vec3) {
    let first = axis.any_orthonormal_vector();
    (first, axis.cross(first))
}

/// The hull on the screen of a cone from the circle of `radius` about `base` across `axis` to `tip`.
fn cone(seen: &Seen, base: Vec3, tip: Vec3, axis: Vec3, radius: f32) -> Vec<Pos2> {
    let (u, v) = across(axis);
    let mut points: Vec<Pos2> = (0..12)
        .filter_map(|step| {
            let angle = step as f32 / 12.0 * std::f32::consts::TAU;
            project(seen, base + (u * angle.cos() + v * angle.sin()) * radius)
        })
        .collect();
    points.extend(project(seen, tip));
    hull(points)
}

/// The hull on the screen of a cube of `half` yards about `middle`, turned as `axes`.
fn cube(seen: &Seen, middle: Vec3, axes: [Vec3; 3], half: f32) -> Vec<Pos2> {
    let points = (0..8)
        .filter_map(|corner| {
            let sign = |bit: usize| if corner & (1 << bit) == 0 { -half } else { half };
            project(seen, middle + axes[0] * sign(0) + axes[1] * sign(1) + axes[2] * sign(2))
        })
        .collect();
    hull(points)
}

/// The points of the ring about `axis` through the middle of `frame`, of `radius` yards, split
/// where it goes behind: its halves towards the eye alone.
fn ring(seen: &Seen, frame: &Frame, axis: Vec3, radius: f32) -> Vec<Vec<Pos2>> {
    let (u, v) = across(axis);
    let mut lines: Vec<Vec<Pos2>> = vec![Vec::new()];
    for step in 0..=64 {
        let angle = step as f32 / 64.0 * std::f32::consts::TAU;
        let out = (u * angle.cos() + v * angle.sin()) * radius;
        let front = out.dot(frame.to_eye) >= -1e-3 * radius;
        match project(seen, frame.centre + out).filter(|_| front) {
            Some(point) => lines.last_mut().expect("one line at least").push(point),
            None if lines.last().is_some_and(|line| !line.is_empty()) => lines.push(Vec::new()),
            None => {}
        }
    }
    // The circle closed across its first point.
    if lines.len() > 1
        && let (Some(first), Some(last)) = (lines.first(), lines.last())
        && first.first() == last.last()
    {
        let first = lines.remove(0);
        lines
            .last_mut()
            .expect("one line at least")
            .extend(first.into_iter().skip(1));
    }
    lines.retain(|line| line.len() >= 2);
    lines
}

/// The circle of `radius` points about `middle` on the screen.
fn circle(middle: Pos2, radius: f32) -> Vec<Pos2> {
    (0..=64)
        .map(|step| {
            let angle = step as f32 / 64.0 * std::f32::consts::TAU;
            middle + Vec2::angled(angle) * radius
        })
        .collect()
}

/// The parts of the gizmo `mode` about `frame`, in the order they are drawn, the nearest last;
/// `shift` showing the square of the view of *Move*, `stretch` the axes of *Scale* drawn as long as
/// the scale dragged makes them.
pub fn parts(mode: Mode, seen: &Seen, frame: &Frame, shift: bool, stretch: f32) -> Vec<Part> {
    let Some(middle) = project(seen, frame.centre) else {
        return Vec::new();
    };
    let (centre, size) = (frame.centre, frame.size);
    let mut parts = Vec::new();
    match mode {
        Mode::Select => {}
        Mode::Move => {
            for index in 0..3 {
                let normal = frame.axes[index];
                let (j, k) = ((index + 1) % 3, (index + 2) % 3);
                let side = |axis: Vec3| if axis.dot(frame.to_eye) < 0.0 { -1.0 } else { 1.0 };
                let (u, v) = (frame.axes[j] * side(frame.axes[j]), frame.axes[k] * side(frame.axes[k]));
                let corners: Vec<Pos2> = [(0.05, 0.05), (0.3, 0.05), (0.3, 0.3), (0.05, 0.3)]
                    .iter()
                    .filter_map(|(a, b)| project(seen, centre + (u * *a + v * *b) * size))
                    .collect();
                if corners.len() == 4 {
                    parts.push(Part {
                        handle: Handle::Plane(index),
                        shape: Shape::Area(corners, Some(1.0)),
                        colour: colour(index),
                        seen: edge_fade(normal, frame.to_eye),
                        last: false,
                    });
                }
            }
            // The arrows, the farthest first.
            let mut order = [0, 1, 2];
            order.sort_by(|a, b| {
                frame.axes[*a]
                    .dot(frame.to_eye)
                    .total_cmp(&frame.axes[*b].dot(frame.to_eye))
            });
            for index in order {
                let axis = frame.axes[index];
                let fade = facing_fade(axis, frame.to_eye);
                let (Some(start), Some(end)) = (Some(middle), project(seen, centre + axis * size * 0.82)) else {
                    continue;
                };
                parts.push(Part {
                    handle: Handle::Axis(index),
                    shape: Shape::Lines(vec![vec![start, end]], 2.0),
                    colour: colour(index),
                    seen: fade,
                    last: false,
                });
                parts.push(Part {
                    handle: Handle::Axis(index),
                    shape: Shape::Area(
                        cone(
                            seen,
                            centre + axis * size * 0.8,
                            centre + axis * size,
                            axis,
                            size * 0.065,
                        ),
                        None,
                    ),
                    colour: colour(index),
                    seen: fade,
                    last: false,
                });
            }
            if shift {
                let half = Vec2::splat(6.0);
                parts.push(Part {
                    handle: Handle::Screen,
                    shape: Shape::Area(
                        vec![
                            middle - half,
                            middle + Vec2::new(half.x, -half.y),
                            middle + half,
                            middle + Vec2::new(-half.x, half.y),
                        ],
                        Some(1.0),
                    ),
                    colour: VIEW,
                    seen: 1.0,
                    last: false,
                });
            }
        }
        Mode::Rotate => {
            parts.push(Part {
                handle: Handle::Free,
                shape: Shape::Disc(middle, LENGTH),
                colour: VIEW,
                seen: 1.0,
                last: true,
            });
            for index in 0..3 {
                parts.push(Part {
                    handle: Handle::Ring(index),
                    shape: Shape::Lines(ring(seen, frame, frame.axes[index], size), 2.5),
                    colour: colour(index),
                    seen: 1.0,
                    last: false,
                });
            }
            parts.push(Part {
                handle: Handle::Outer,
                shape: Shape::Lines(vec![circle(middle, LENGTH * 1.1)], 2.0),
                colour: VIEW,
                seen: 1.0,
                last: false,
            });
        }
        Mode::Scale => {
            let mut order = [0, 1, 2];
            order.sort_by(|a, b| {
                frame.axes[*a]
                    .dot(frame.to_eye)
                    .total_cmp(&frame.axes[*b].dot(frame.to_eye))
            });
            for index in order {
                let axis = frame.axes[index];
                let fade = facing_fade(axis, frame.to_eye);
                let end = centre + axis * size * stretch;
                let Some(tip) = project(seen, end) else {
                    continue;
                };
                parts.push(Part {
                    handle: Handle::Stretch(index),
                    shape: Shape::Lines(vec![vec![middle, tip]], 2.0),
                    colour: colour(index),
                    seen: fade,
                    last: false,
                });
                parts.push(Part {
                    handle: Handle::Stretch(index),
                    shape: Shape::Area(cube(seen, end, frame.axes, size * 0.05), None),
                    colour: colour(index),
                    seen: fade,
                    last: false,
                });
            }
            parts.push(Part {
                handle: Handle::Uniform,
                shape: Shape::Area(cube(seen, centre, frame.axes, size * 0.07), None),
                colour: VIEW,
                seen: 1.0,
                last: false,
            });
        }
    }
    parts
}

fn distance_to_segment(point: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((point - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t).distance(point)
}

/// Whether `point` is inside the convex polygon `points`, either way round.
fn inside(points: &[Pos2], point: Pos2) -> bool {
    if points.len() < 3 {
        return false;
    }
    let mut signs = (false, false);
    for (index, a) in points.iter().enumerate() {
        let b = points[(index + 1) % points.len()];
        let cross = (b - *a).x * (point - *a).y - (b - *a).y * (point - *a).x;
        if cross > 0.0 {
            signs.0 = true;
        } else if cross < 0.0 {
            signs.1 = true;
        }
    }
    !(signs.0 && signs.1)
}

/// How far `point` is from the shape, 0 inside it.
fn distance(shape: &Shape, point: Pos2) -> f32 {
    match shape {
        Shape::Lines(lines, _) => lines
            .iter()
            .flat_map(|line| line.windows(2))
            .map(|pair| distance_to_segment(point, pair[0], pair[1]))
            .fold(f32::INFINITY, f32::min),
        Shape::Area(points, _) if inside(points, point) => 0.0,
        Shape::Area(points, _) => points
            .iter()
            .enumerate()
            .map(|(index, a)| distance_to_segment(point, *a, points[(index + 1) % points.len()]))
            .fold(f32::INFINITY, f32::min),
        Shape::Disc(middle, radius) => (middle.distance(point) - radius).max(0.0),
    }
}

/// The handle of `parts` under `point`, in points from the top left of the view: the nearest one
/// seen, the one drawn over the others as near, those taken last only where no other is.
#[cfg(test)]
pub fn grab(parts: &[Part], point: Pos2) -> Option<Handle> {
    nearest(parts, point).map(|(handle, _)| handle)
}

/// How far a handle is from the pointer, to choose between two gizmos: those taken last after the
/// others, then the nearest.
pub type Away = (bool, f32);

/// The handle of `parts` under `point`, as `grab` finds it, and how far it is.
pub fn nearest(parts: &[Part], point: Pos2) -> Option<(Handle, Away)> {
    parts
        .iter()
        .enumerate()
        .filter(|(_, part)| part.seen > 0.1)
        .filter_map(|(drawn, part)| {
            let away = distance(&part.shape, point);
            (away <= GRAB).then_some((part.last, away, drawn, part.handle))
        })
        .min_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(b.2.cmp(&a.2)))
        .map(|(last, away, _, handle)| (handle, (last, away)))
}

/// The gizmo of a spawn point, apart from that of the creature in the world: smaller, of the colour
/// of the circle drawn on it, green as the database has it, red moved and waiting for Apply.
pub const SPAWN_SIZE: f32 = 0.6;

pub fn spawn_colour(pending: bool) -> Color32 {
    let [r, g, b] = if pending {
        uniwow_api::scene::state_colour::PENDING
    } else {
        uniwow_api::scene::state_colour::DATABASE
    };
    Color32::from_rgb(r, g, b)
}

/// `parts` drawn as the gizmo of a spawn point: all of its colour.
pub fn as_spawn_point(mut parts: Vec<Part>, pending: bool) -> Vec<Part> {
    for part in &mut parts {
        part.colour = spawn_colour(pending);
    }
    parts
}

/// What a drag of a gizmo makes, from where it was grabbed: a move in the world, a turn, a factor
/// of scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Amount {
    Shift(Vec3),
    Turn(Quat),
    Scale(f32),
}

impl Amount {
    /// Nothing made yet, by a drag of `handle`.
    pub fn none(handle: Handle) -> Amount {
        match handle {
            Handle::Axis(_) | Handle::Plane(_) | Handle::Screen => Amount::Shift(Vec3::ZERO),
            Handle::Ring(_) | Handle::Outer | Handle::Free => Amount::Turn(Quat::IDENTITY),
            Handle::Stretch(_) | Handle::Uniform => Amount::Scale(1.0),
        }
    }
}

/// A handle grabbed: the gizmo as it was then, where the pointer was, and what the handle needs to
/// follow it.
#[derive(Clone, Copy, Debug)]
pub struct Grab {
    pub handle: Handle,
    pub frame: Frame,
    start: Pos2,
    last: Pos2,
    /// The axis of a turn, the direction from the middle where its ring was grabbed, and the
    /// direction on the screen that turns it forward.
    axis: Vec3,
    from: Vec3,
    tangent: Vec2,
    /// The turn made so far about the view's axis, and freely.
    turned: f32,
    free: Quat,
}

fn closest_on_line(ray: &uniwow_api::viewport::Ray, origin: Vec3, axis: Vec3) -> Option<f32> {
    let w = origin - ray.origin;
    let b = axis.dot(ray.direction);
    let denominator = 1.0 - b * b;
    (denominator > 1e-6).then(|| (b * ray.direction.dot(w) - axis.dot(w)) / denominator)
}

fn on_plane(ray: &uniwow_api::viewport::Ray, origin: Vec3, normal: Vec3) -> Option<Vec3> {
    let facing = ray.direction.dot(normal);
    if facing.abs() < 1e-6 {
        return None;
    }
    let distance = (origin - ray.origin).dot(normal) / facing;
    (distance > 0.0).then(|| ray.at(distance))
}

fn snapped(value: f32, step: f32, snap: bool) -> f32 {
    if snap { (value / step).round() * step } else { value }
}

impl Grab {
    /// `handle` of the gizmo `frame` grabbed at `start`, in points from the top left of the view.
    pub fn new(handle: Handle, frame: Frame, seen: &Seen, start: Pos2) -> Grab {
        let mut grab = Grab {
            handle,
            frame,
            start,
            last: start,
            axis: frame.to_eye,
            from: frame.right,
            tangent: Vec2::X,
            turned: 0.0,
            free: Quat::IDENTITY,
        };
        let middle = project(seen, frame.centre).unwrap_or(start);
        match handle {
            Handle::Ring(index) => {
                let axis = frame.axes[index];
                let (u, v) = across(axis);
                // The point of the ring nearest to the pointer, towards the eye.
                let from = (0..128)
                    .map(|step| {
                        let angle = step as f32 / 128.0 * std::f32::consts::TAU;
                        u * angle.cos() + v * angle.sin()
                    })
                    .filter(|out| out.dot(frame.to_eye) >= -1e-3)
                    .filter_map(|out| Some((project(seen, frame.centre + out * frame.size)?.distance(start), out)))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map_or(u, |(_, out)| out);
                let point = frame.centre + from * frame.size;
                let ahead = point + axis.cross(from) * frame.size * 0.1;
                let tangent = match (project(seen, point), project(seen, ahead)) {
                    (Some(a), Some(b)) => (b - a).normalized(),
                    _ => Vec2::X,
                };
                grab.axis = axis;
                grab.from = from;
                grab.tangent = if tangent.is_finite() && tangent != Vec2::ZERO {
                    tangent
                } else {
                    Vec2::X
                };
            }
            Handle::Outer => {
                let out = start - middle;
                grab.from = (frame.right * out.x - frame.up * out.y)
                    .try_normalize()
                    .unwrap_or(frame.right);
            }
            _ => {}
        }
        grab
    }

    /// What the drag makes with the pointer at `now`, snapped to its step when `snap`.
    pub fn amount(&mut self, seen: &Seen, now: Pos2, snap: bool) -> Amount {
        let frame = self.frame;
        let rays = (seen.ray([self.start.x, self.start.y]), seen.ray([now.x, now.y]));
        let middle = project(seen, frame.centre).unwrap_or(self.start);
        let amount = match self.handle {
            Handle::Axis(index) => {
                let axis = frame.axes[index];
                let moved = match rays {
                    (Some(from), Some(to)) => closest_on_line(&to, frame.centre, axis)
                        .zip(closest_on_line(&from, frame.centre, axis))
                        .map_or(0.0, |(to, from)| to - from),
                    _ => 0.0,
                };
                Amount::Shift(axis * snapped(moved, STEPS[0], snap))
            }
            Handle::Plane(_) | Handle::Screen => {
                let (normal, axes) = match self.handle {
                    Handle::Plane(index) => (frame.axes[index], frame.axes),
                    _ => (frame.to_eye, [Vec3::X, Vec3::Y, Vec3::Z]),
                };
                let moved = match rays {
                    (Some(from), Some(to)) => on_plane(&to, frame.centre, normal)
                        .zip(on_plane(&from, frame.centre, normal))
                        .map_or(Vec3::ZERO, |(to, from)| to - from),
                    _ => Vec3::ZERO,
                };
                let shift = if snap {
                    axes.iter()
                        .map(|axis| *axis * snapped(moved.dot(*axis), STEPS[0], true))
                        .sum()
                } else {
                    moved
                };
                Amount::Shift(shift)
            }
            Handle::Ring(_) => {
                let degrees = (now - self.start).dot(self.tangent) / LENGTH * 180.0 / std::f32::consts::PI;
                Amount::Turn(Quat::from_axis_angle(
                    self.axis,
                    snapped(degrees, STEPS[1], snap).to_radians(),
                ))
            }
            Handle::Outer => {
                let angle = |at: Pos2| (-(at.y - middle.y)).atan2(at.x - middle.x);
                let mut turned = (angle(now) - angle(self.last)).to_degrees();
                if turned > 180.0 {
                    turned -= 360.0;
                } else if turned < -180.0 {
                    turned += 360.0;
                }
                self.turned += turned;
                Amount::Turn(Quat::from_axis_angle(
                    frame.to_eye,
                    snapped(self.turned, STEPS[1], snap).to_radians(),
                ))
            }
            Handle::Free => {
                let moved = now - self.last;
                let towards = frame.right * moved.x - frame.up * moved.y;
                if let Some(axis) = frame.to_eye.cross(towards).try_normalize() {
                    self.free = Quat::from_axis_angle(axis, moved.length() / LENGTH) * self.free;
                }
                Amount::Turn(self.free)
            }
            Handle::Stretch(index) => {
                let along = match (
                    Some(middle),
                    project(seen, frame.centre + frame.axes[index] * frame.size),
                ) {
                    (Some(a), Some(b)) => b - a,
                    _ => Vec2::new(LENGTH, 0.0),
                };
                let length = along.length().max(1.0);
                let factor = 1.0 + (now - self.start).dot(along / length) / length;
                Amount::Scale(snapped(factor, STEPS[2], snap).max(if snap { STEPS[2] } else { 0.01 }))
            }
            Handle::Uniform => {
                let moved = now - self.start;
                let factor = 1.0 + (moved.x - moved.y) / LENGTH;
                Amount::Scale(snapped(factor, STEPS[2], snap).max(if snap { STEPS[2] } else { 0.01 }))
            }
        };
        self.last = now;
        amount
    }

    /// The slice a turn about a ring sweeps, as Unity fills it: its axis, the direction from the
    /// middle it starts at, its radius in yards, and the turn.
    pub fn slice(&self, amount: Amount) -> Option<(Vec3, Vec3, f32, f32)> {
        let Amount::Turn(turn) = amount else {
            return None;
        };
        let radius = match self.handle {
            Handle::Ring(_) => self.frame.size,
            Handle::Outer => self.frame.size * 1.1,
            _ => return None,
        };
        let (axis, angle) = turn.to_axis_angle();
        let signed = if axis.dot(self.axis) < 0.0 { -angle } else { angle };
        Some((self.axis, self.from, radius, signed))
    }
}

/// Draws the parts of a gizmo, offset by `offset` from the top left of the view to the screen:
/// `hovered` lit, `active` yellow and the others faint while it is dragged.
pub fn draw(painter: &egui::Painter, parts: &[Part], offset: Vec2, hovered: Option<Handle>, active: Option<Handle>) {
    for part in parts {
        if part.seen <= 0.02 {
            continue;
        }
        let base = if active == Some(part.handle) {
            ACTIVE
        } else if active.is_none() && hovered == Some(part.handle) {
            HOVER
        } else {
            part.colour
        };
        let faint = if active.is_some_and(|active| active != part.handle) {
            0.45
        } else {
            1.0
        };
        let colour = base.gamma_multiply(part.seen * faint);
        match &part.shape {
            Shape::Lines(lines, width) => {
                for line in lines {
                    let points = line.iter().map(|point| *point + offset).collect();
                    painter.add(egui::Shape::line(points, egui::Stroke::new(*width, colour)));
                }
            }
            Shape::Area(points, outline) => {
                let points: Vec<Pos2> = points.iter().map(|point| *point + offset).collect();
                let (fill, stroke) = match outline {
                    Some(width) => (colour.gamma_multiply(0.35), egui::Stroke::new(*width, colour)),
                    None => (colour, egui::Stroke::NONE),
                };
                painter.add(egui::Shape::convex_polygon(points, fill, stroke));
            }
            Shape::Disc(middle, radius) => {
                let lit = if hovered == Some(part.handle) || active == Some(part.handle) {
                    0.14
                } else {
                    0.07
                };
                painter.circle_filled(*middle + offset, *radius, colour.gamma_multiply(lit));
            }
        }
    }
}

/// Fills the slice a turn sweeps, from `from` about `axis`, of `radius` yards about the middle of
/// `frame`, by `angle` radians, as Unity shows the turn being made.
pub fn draw_slice(painter: &egui::Painter, seen: &Seen, frame: &Frame, offset: Vec2, slice: (Vec3, Vec3, f32, f32)) {
    let (axis, from, radius, angle) = slice;
    let Some(middle) = project(seen, frame.centre) else {
        return;
    };
    let steps = ((angle.abs() / std::f32::consts::TAU * 64.0).ceil() as usize).clamp(1, 256);
    let points: Vec<Pos2> = (0..=steps)
        .filter_map(|step| {
            let turned = Quat::from_axis_angle(axis, angle * step as f32 / steps as f32) * from;
            project(seen, frame.centre + turned * radius)
        })
        .collect();
    let mut mesh = egui::Mesh::default();
    let fill = ACTIVE.gamma_multiply(0.3);
    mesh.colored_vertex(middle + offset, fill);
    for point in &points {
        mesh.colored_vertex(*point + offset, fill);
    }
    for index in 1..points.len() as u32 {
        mesh.add_triangle(0, index, index + 1);
    }
    painter.add(mesh);
    let stroke = egui::Stroke::new(1.0, ACTIVE);
    if let (Some(first), Some(last)) = (points.first(), points.last()) {
        painter.line_segment([middle + offset, *first + offset], stroke);
        painter.line_segment([middle + offset, *last + offset], stroke);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::glam::Mat4;

    /// A view of 400 × 400 points of the origin from 40 yards to the south, 15 to the east and 30 up,
    /// the vertical up.
    fn seen() -> Seen {
        let eye = Vec3::new(15.0, -40.0, 30.0);
        Seen {
            view_proj: Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.5)
                * Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Z),
            eye,
            size: [400.0, 400.0],
        }
    }

    fn at(seen: &Seen, point: Vec3) -> Pos2 {
        project(seen, point).unwrap()
    }

    #[test]
    fn the_nearer_of_two_gizmos_is_grabbed_the_spawn_point_s_of_its_own_colour() {
        let seen = seen();
        let parts_at = |centre: Vec3| {
            let frame = Frame::new(&seen, centre, Quat::IDENTITY).unwrap();
            parts(Mode::Move, &seen, &frame, false, 1.0)
        };
        let (here, there) = (
            parts_at(Vec3::ZERO),
            as_spawn_point(parts_at(Vec3::new(3.0, 0.0, 0.0)), false),
        );
        assert!(there.iter().all(|part| part.colour == spawn_colour(false)));
        assert_ne!(spawn_colour(false), spawn_colour(true), "green, red once moved");
        let tip = at(&seen, Vec3::new(3.0, 0.0, 0.0));
        let (_, near) = nearest(&there, tip).expect("its middle under the pointer");
        assert!(nearest(&here, tip).is_none_or(|(_, away)| away.1 >= near.1));
    }

    #[test]
    fn a_gizmo_is_as_long_on_the_screen_wherever_it_stands() {
        let seen = seen();
        for centre in [Vec3::ZERO, Vec3::new(5.0, 20.0, -3.0)] {
            let frame = Frame::new(&seen, centre, Quat::IDENTITY).unwrap();
            let length = at(&seen, centre).distance(at(&seen, centre + frame.right * frame.size));
            assert!((length - LENGTH).abs() < LENGTH * 0.02, "{length}");
        }
    }

    #[test]
    fn the_handles_are_grabbed_where_they_are_drawn() {
        let seen = seen();
        let frame = Frame::new(&seen, Vec3::ZERO, Quat::IDENTITY).unwrap();
        let s = frame.size;
        let moving = parts(Mode::Move, &seen, &frame, false, 1.0);
        assert_eq!(grab(&moving, at(&seen, Vec3::X * s * 0.6)), Some(Handle::Axis(0)));
        assert_eq!(
            grab(&moving, at(&seen, Vec3::X * s * 0.95)),
            Some(Handle::Axis(0)),
            "its cone"
        );
        assert_eq!(grab(&moving, at(&seen, Vec3::Z * s * 0.6)), Some(Handle::Axis(2)));
        // The square of the plane XY towards the eye, to the south.
        assert_eq!(
            grab(&moving, at(&seen, Vec3::new(0.2, -0.2, 0.0) * s)),
            Some(Handle::Plane(2))
        );
        assert_eq!(
            grab(&moving, at(&seen, Vec3::ZERO) + Vec2::new(-50.0, 50.0)),
            None,
            "where nothing is"
        );
        assert_eq!(grab(&moving, at(&seen, Vec3::new(-0.6, 0.0, 0.6) * s)), None);
        let shifted = parts(Mode::Move, &seen, &frame, true, 1.0);
        assert_eq!(
            grab(&shifted, at(&seen, Vec3::ZERO) + Vec2::new(3.0, 3.0)),
            Some(Handle::Screen)
        );

        let turning = parts(Mode::Rotate, &seen, &frame, false, 1.0);
        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(
            grab(&turning, at(&seen, Vec3::new(0.3, -0.954, 0.0) * s)),
            Some(Handle::Ring(2))
        );
        assert_eq!(
            grab(&turning, at(&seen, Vec3::new(0.0, -half, half) * s)),
            Some(Handle::Ring(0))
        );
        assert_eq!(
            grab(&turning, at(&seen, Vec3::new(half, 0.0, half) * s)),
            Some(Handle::Ring(1))
        );
        let middle = at(&seen, Vec3::ZERO);
        assert_eq!(
            grab(&turning, middle + Vec2::new(0.0, LENGTH * 1.1)),
            Some(Handle::Outer)
        );
        assert_eq!(grab(&turning, middle + Vec2::new(30.0, 25.0)), Some(Handle::Free));
        assert_eq!(grab(&turning, middle + Vec2::new(0.0, LENGTH * 1.5)), None);

        let scaling = parts(Mode::Scale, &seen, &frame, false, 1.0);
        assert_eq!(grab(&scaling, at(&seen, Vec3::X * s)), Some(Handle::Stretch(0)));
        assert_eq!(grab(&scaling, at(&seen, Vec3::Y * s * 0.5)), Some(Handle::Stretch(1)));
        assert_eq!(grab(&scaling, middle), Some(Handle::Uniform));
        assert!(parts(Mode::Select, &seen, &frame, false, 1.0).is_empty());
    }

    #[test]
    fn an_axis_pointing_at_the_eye_fades_and_is_not_grabbed() {
        let eye = Vec3::new(0.0, 0.0, 50.0);
        let above = Seen {
            view_proj: Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.5)
                * Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y),
            eye,
            size: [400.0, 400.0],
        };
        let frame = Frame::new(&above, Vec3::ZERO, Quat::IDENTITY).unwrap();
        let moving = parts(Mode::Move, &above, &frame, false, 1.0);
        let z = moving.iter().find(|part| part.handle == Handle::Axis(2)).unwrap();
        assert_eq!(z.seen, 0.0);
        let plane = moving.iter().find(|part| part.handle == Handle::Plane(0)).unwrap();
        assert_eq!(plane.seen, 0.0, "seen edge on");
        assert_eq!(
            moving.iter().find(|part| part.handle == Handle::Plane(2)).unwrap().seen,
            1.0
        );
        assert_ne!(grab(&moving, at(&above, Vec3::ZERO)), Some(Handle::Axis(2)));
    }

    #[test]
    fn a_drag_moves_along_its_axis_and_in_its_plane_snapped_with_ctrl() {
        let seen = seen();
        let frame = Frame::new(&seen, Vec3::ZERO, Quat::IDENTITY).unwrap();
        let s = frame.size;
        let start = at(&seen, Vec3::X * s * 0.5);
        let mut grabbed = Grab::new(Handle::Axis(0), frame, &seen, start);
        let Amount::Shift(moved) = grabbed.amount(&seen, at(&seen, Vec3::X * (s * 0.5 + 2.4)), false) else {
            panic!("a move")
        };
        assert!((moved - Vec3::X * 2.4).length() < 1e-2, "{moved}");
        let Amount::Shift(snapped) = grabbed.amount(&seen, at(&seen, Vec3::X * (s * 0.5 + 2.4)), true) else {
            panic!("a move")
        };
        assert_eq!(snapped, Vec3::X * 2.0);
        let from = Vec3::new(0.2, -0.2, 0.0) * s;
        let mut grabbed = Grab::new(Handle::Plane(2), frame, &seen, at(&seen, from));
        let Amount::Shift(moved) = grabbed.amount(&seen, at(&seen, from + Vec3::new(1.3, 0.6, 0.0)), false) else {
            panic!("a move")
        };
        assert!((moved - Vec3::new(1.3, 0.6, 0.0)).length() < 1e-2, "{moved}");
    }

    #[test]
    fn a_ring_turns_forward_along_it_snapped_by_15_degrees_and_the_scale_follows_the_pointer() {
        let seen = seen();
        let frame = Frame::new(&seen, Vec3::ZERO, Quat::IDENTITY).unwrap();
        let start = at(&seen, -Vec3::Y * frame.size);
        let mut grabbed = Grab::new(Handle::Ring(2), frame, &seen, start);
        // Forward about the vertical, counterclockwise from above: to the east at the south.
        let forward = grabbed.tangent;
        assert!(forward.x > 0.9, "{forward:?}");
        let Amount::Turn(turn) = grabbed.amount(&seen, start + forward * LENGTH * std::f32::consts::FRAC_PI_2, true)
        else {
            panic!("a turn")
        };
        let (axis, angle) = turn.to_axis_angle();
        assert!(
            (axis - Vec3::Z).length() < 1e-4 && (angle.to_degrees() - 90.0).abs() < 1e-3,
            "{axis} {angle}"
        );
        let (_, _, radius, swept) = grabbed.slice(Amount::Turn(turn)).unwrap();
        assert!((radius - frame.size).abs() < 1e-4 && (swept.to_degrees() - 90.0).abs() < 1e-3);

        let middle = at(&seen, Vec3::ZERO);
        let mut grabbed = Grab::new(Handle::Uniform, frame, &seen, middle);
        assert_eq!(
            grabbed.amount(&seen, middle + Vec2::new(LENGTH, 0.0), true),
            Amount::Scale(2.0)
        );
        let end = at(&seen, Vec3::X * frame.size);
        let mut grabbed = Grab::new(Handle::Stretch(0), frame, &seen, end);
        let Amount::Scale(factor) = grabbed.amount(&seen, middle + (end - middle) * 1.5, false) else {
            panic!("a scale")
        };
        assert!((factor - 1.5).abs() < 1e-3, "{factor}");
    }
}
