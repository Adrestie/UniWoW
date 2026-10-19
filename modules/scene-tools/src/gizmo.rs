//! The gizmos of the 3D view (step 15.6), kept simple: drawn over the image as lines projected,
//! picked on the screen, about the middle of the selection, in the axes of the world. *Move* (W):
//! three arrows; *Rotate* (E): the ring of the vertical, turning each entity about its own origin;
//! *Scale* (R): a handle scaling each one. Ctrl snaps to the yard, to 15 degrees and to tenths. The
//! change is written when the drag is let go, as the Inspector writes, one gesture.

use uniwow_api::egui;
use uniwow_api::glam::Vec3;
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

/// A handle of a gizmo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// An arrow of *Move*, along the axis of the world 0, 1 or 2.
    Axis(usize),
    /// The ring of *Rotate*, about the vertical.
    Ring,
    /// The handle of *Scale*.
    Scale,
}

/// The length of an arrow and the radius of the ring on the screen, in points; how near the pointer
/// must be to grab a handle.
pub const LENGTH: f32 = 90.0;
const GRAB: f32 = 7.0;
/// The steps of the snapping: a yard, 15 degrees, a tenth.
pub const STEPS: [f32; 3] = [1.0, 15.0, 0.1];

/// The colour of each axis: X red, Y green, Z blue.
pub fn colour(axis: usize) -> egui::Color32 {
    [
        egui::Color32::from_rgb(230, 70, 70),
        egui::Color32::from_rgb(90, 200, 90),
        egui::Color32::from_rgb(80, 140, 255),
    ][axis.min(2)]
}

/// The axes of the world.
fn axis(index: usize) -> Vec3 {
    [Vec3::X, Vec3::Y, Vec3::Z][index]
}

/// The yards a point of the screen covers along the axis `index` at `centre`, and the direction of
/// that axis on the screen; none where it points at the eye.
pub fn along(seen: &Seen, centre: Vec3, index: usize) -> Option<(f32, egui::Vec2)> {
    let [x0, y0] = seen.project(centre)?;
    let [x1, y1] = seen.project(centre + axis(index))?;
    let screen = egui::vec2(x1 - x0, y1 - y0);
    let pixels = screen.length();
    (pixels > 1e-3).then(|| (pixels, screen / pixels))
}

/// The points of the ring about the vertical through `centre`, on the screen from the top left of
/// the view, of `LENGTH` points of radius about its middle.
pub fn ring(seen: &Seen, centre: Vec3) -> Vec<egui::Pos2> {
    let Some((pixels, _)) = along(seen, centre, 0).or_else(|| along(seen, centre, 1)) else {
        return Vec::new();
    };
    let radius = LENGTH / pixels;
    (0..=48)
        .filter_map(|step| {
            let angle = step as f32 / 48.0 * std::f32::consts::TAU;
            let point = centre + Vec3::new(angle.cos(), angle.sin(), 0.0) * radius;
            seen.project(point).map(|[x, y]| egui::pos2(x, y))
        })
        .collect()
}

fn distance_to_segment(point: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let t = ((point - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t).distance(point)
}

/// The handle of the gizmo `mode` under `point`, in points from the top left of the view, the
/// gizmo about `centre`.
pub fn grab(mode: Mode, seen: &Seen, centre: Vec3, point: egui::Pos2) -> Option<Handle> {
    let [cx, cy] = seen.project(centre)?;
    let middle = egui::pos2(cx, cy);
    match mode {
        Mode::Select => None,
        Mode::Move => (0..3)
            .filter_map(|index| {
                let (_, direction) = along(seen, centre, index)?;
                let distance = distance_to_segment(point, middle, middle + direction * LENGTH);
                (distance <= GRAB).then_some((distance, index))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, index)| Handle::Axis(index)),
        Mode::Rotate => {
            let points = ring(seen, centre);
            points
                .windows(2)
                .any(|pair| distance_to_segment(point, pair[0], pair[1]) <= GRAB)
                .then_some(Handle::Ring)
        }
        Mode::Scale => {
            let corner = middle + egui::vec2(LENGTH, -LENGTH) * std::f32::consts::FRAC_1_SQRT_2;
            (corner.distance(point) <= GRAB * 1.5 || distance_to_segment(point, middle, corner) <= GRAB / 2.0)
                .then_some(Handle::Scale)
        }
    }
}

/// What a drag of `handle` from `from` to `to` on the screen changes: yards along its axis, degrees
/// about the vertical, or a factor of scale; snapped to its step when `snap`.
pub fn dragged(handle: Handle, seen: &Seen, centre: Vec3, from: egui::Pos2, to: egui::Pos2, snap: bool) -> f32 {
    let step = |value: f32, step: f32| if snap { (value / step).round() * step } else { value };
    match handle {
        Handle::Axis(index) => {
            let Some((pixels, direction)) = along(seen, centre, index) else {
                return 0.0;
            };
            step((to - from).dot(direction) / pixels, STEPS[0])
        }
        Handle::Ring => {
            let Some([cx, cy]) = seen.project(centre) else {
                return 0.0;
            };
            let middle = egui::pos2(cx, cy);
            let angle = |at: egui::Pos2| (-(at.y - middle.y)).atan2(at.x - middle.x);
            let mut turned = (angle(to) - angle(from)).to_degrees();
            if turned > 180.0 {
                turned -= 360.0;
            } else if turned < -180.0 {
                turned += 360.0;
            }
            // Seen from below, the screen turns the other way.
            if seen.eye.z < centre.z {
                turned = -turned;
            }
            step(turned, STEPS[1])
        }
        Handle::Scale => {
            let factor = ((to.x - from.x) - (to.y - from.y)).mul_add(1.0 / 200.0, 0.0).exp();
            step(factor, STEPS[2]).max(STEPS[2])
        }
    }
}

/// The writes of a drag, as the providers take them: the field, the axis, the change.
pub fn writes(handle: Handle, amount: f32) -> Vec<(&'static str, Option<usize>, String)> {
    match handle {
        Handle::Axis(index) => vec![("position", Some(index), format!("+{amount}"))],
        // The vertical of the world is the axis Y of the files; turning about it counterclockwise
        // seen from above raises the angle of the files' Y.
        Handle::Ring => vec![("rotation", Some(1), format!("+{amount}"))],
        Handle::Scale => vec![("scale", None, format!("*{amount}"))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::glam::Mat4;

    /// A view of 400 × 400 points looking down at the origin from 50 yards, X to the right.
    fn above() -> Seen {
        let eye = Vec3::new(0.0, 0.0, 50.0);
        Seen {
            view_proj: Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.5)
                * Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y),
            eye,
            size: [400.0, 400.0],
        }
    }

    #[test]
    fn the_handles_are_grabbed_where_they_are_drawn() {
        let seen = above();
        let middle = egui::pos2(200.0, 200.0);
        assert_eq!(
            grab(Mode::Move, &seen, Vec3::ZERO, middle + egui::vec2(60.0, 1.0)),
            Some(Handle::Axis(0))
        );
        assert_eq!(
            grab(Mode::Move, &seen, Vec3::ZERO, middle + egui::vec2(-2.0, -60.0)),
            Some(Handle::Axis(1))
        );
        assert_eq!(
            grab(Mode::Move, &seen, Vec3::ZERO, middle + egui::vec2(-60.0, 60.0)),
            None
        );
        assert_eq!(
            grab(Mode::Rotate, &seen, Vec3::ZERO, middle + egui::vec2(LENGTH, 0.0)),
            Some(Handle::Ring)
        );
        assert_eq!(grab(Mode::Rotate, &seen, Vec3::ZERO, middle), None);
        let corner = middle + egui::vec2(LENGTH, -LENGTH) * std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(grab(Mode::Scale, &seen, Vec3::ZERO, corner), Some(Handle::Scale));
        assert_eq!(grab(Mode::Select, &seen, Vec3::ZERO, corner), None);
    }

    #[test]
    fn a_drag_moves_along_the_axis_turns_about_the_vertical_and_scales_snapped_with_ctrl() {
        let seen = above();
        let middle = egui::pos2(200.0, 200.0);
        let (pixels, _) = along(&seen, Vec3::ZERO, 0).unwrap();
        let moved = dragged(
            Handle::Axis(0),
            &seen,
            Vec3::ZERO,
            middle,
            middle + egui::vec2(pixels * 2.4, 0.0),
            false,
        );
        assert!((moved - 2.4).abs() < 1e-3, "{moved}");
        let snapped = dragged(
            Handle::Axis(0),
            &seen,
            Vec3::ZERO,
            middle,
            middle + egui::vec2(pixels * 2.4, 0.0),
            true,
        );
        assert_eq!(snapped, 2.0);
        // A quarter turn counterclockwise on the screen, seen from above.
        let turned = dragged(
            Handle::Ring,
            &seen,
            Vec3::ZERO,
            middle + egui::vec2(LENGTH, 0.0),
            middle + egui::vec2(0.0, -LENGTH),
            true,
        );
        assert_eq!(turned, 90.0);
        let scaled = dragged(
            Handle::Scale,
            &seen,
            Vec3::ZERO,
            middle,
            middle + egui::vec2(139.0, 0.0),
            true,
        );
        assert!((scaled - 2.0).abs() < 1e-3, "{scaled}");
        assert_eq!(writes(Handle::Axis(2), 1.5), [("position", Some(2), "+1.5".to_owned())]);
        assert_eq!(writes(Handle::Scale, 2.0), [("scale", None, "*2".to_owned())]);
    }
}
