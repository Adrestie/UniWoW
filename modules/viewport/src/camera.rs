use uniwow_api::egui;
use uniwow_api::glam::{Mat4, Vec3};

/// The angle above or below the ground the orbit stops at, in radians.
const PITCH: f32 = 1.5;
/// The nearest and farthest the eye goes from the target.
pub const DISTANCE: [f32; 2] = [0.5, 100_000.0];
/// The narrowest and widest angle of view, in degrees.
pub const FOV: [f32; 2] = [1.0, 170.0];

/// Camera turning around a target point. Z is up.
pub struct OrbitCamera {
    target: Vec3,
    /// Angle around Z, in radians.
    yaw: f32,
    /// Angle above the ground, in radians.
    pitch: f32,
    distance: f32,
    /// Vertical angle of view, in degrees.
    fov: f32,
    /// Width over height of the view last drawn.
    aspect: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::new(0.0, 0.0, 1.0),
            yaw: -2.2,
            pitch: 0.45,
            distance: 14.0,
            fov: 45.0,
            aspect: 16.0 / 9.0,
        }
    }
}

impl OrbitCamera {
    pub fn eye(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        self.target + self.distance * Vec3::new(cos_pitch * cos_yaw, cos_pitch * sin_yaw, sin_pitch)
    }

    pub fn target(&self) -> Vec3 {
        self.target
    }

    pub fn fov(&self) -> f32 {
        self.fov
    }

    /// The eye at `position`, looking at the target.
    pub fn set_position(&mut self, position: Vec3) {
        self.look_at(position, self.target);
    }

    /// Looks at `target` from where the eye is.
    pub fn set_target(&mut self, target: Vec3) {
        self.look_at(self.eye(), target);
    }

    pub fn set_fov(&mut self, degrees: f32) {
        self.fov = degrees.clamp(FOV[0], FOV[1]);
    }

    /// The width over the height of the view drawn, which `frame` fits a box in.
    pub fn set_aspect(&mut self, aspect: f32) {
        if aspect.is_finite() && aspect > 0.0 {
            self.aspect = aspect;
        }
    }

    /// The eye at `position`, looking at `target`, within the limits of the orbit: its angle above
    /// the ground and its distance. An eye on the target keeps the direction it had.
    pub fn look_at(&mut self, position: Vec3, target: Vec3) {
        let offset = position - target;
        let distance = offset.length();
        if distance > f32::EPSILON {
            self.yaw = offset.y.atan2(offset.x);
            self.pitch = (offset.z / distance).clamp(-1.0, 1.0).asin().clamp(-PITCH, PITCH);
        }
        self.target = target;
        self.distance = distance.clamp(DISTANCE[0], DISTANCE[1]);
    }

    /// Fits the box from `min` to `max` in the view, seen from the same direction: within the
    /// narrower of its angles, the vertical one or the horizontal one.
    pub fn frame(&mut self, min: Vec3, max: Vec3) {
        let radius = ((max - min).length() / 2.0).max(DISTANCE[0]);
        let vertical = self.fov.to_radians() / 2.0;
        let horizontal = (vertical.tan() * self.aspect).atan();
        self.target = (min + max) / 2.0;
        self.distance = (radius / vertical.min(horizontal).sin()).clamp(DISTANCE[0], DISTANCE[1]);
    }

    /// Reverse Z with no far plane: the depth is 1 at the near plane and falls towards 0 at
    /// infinity, so that the whole of a map is seen, with its precision where it is needed.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Z);
        let proj = Mat4::perspective_infinite_reverse_rh(self.fov.to_radians(), aspect, 0.1);
        proj * view
    }

    /// Left drag orbits, right or middle drag pans, the wheel zooms.
    pub fn handle_input(&mut self, ui: &egui::Ui, response: &egui::Response) {
        let delta = response.drag_delta();
        if response.dragged_by(egui::PointerButton::Primary) {
            self.yaw -= delta.x * 0.008;
            self.pitch = (self.pitch + delta.y * 0.008).clamp(-PITCH, PITCH);
        } else if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            let forward = (self.target - self.eye()).normalize();
            let right = forward.cross(Vec3::Z).normalize_or_zero();
            let up = right.cross(forward);
            let scale = self.distance * 0.0015;
            self.target += (-right * delta.x + up * delta.y) * scale;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.distance = (self.distance * (-scroll * 0.0015).exp()).clamp(DISTANCE[0], DISTANCE[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use uniwow_api::glam::Vec3;

    use super::{DISTANCE, OrbitCamera};

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn the_eye_goes_where_it_is_put_looking_at_the_target() {
        let mut camera = OrbitCamera::default();
        camera.look_at(Vec3::new(10.0, -4.0, 6.0), Vec3::new(1.0, 2.0, 0.5));
        assert!(close(camera.eye(), Vec3::new(10.0, -4.0, 6.0)));
        assert!(close(camera.target(), Vec3::new(1.0, 2.0, 0.5)));
        camera.set_target(Vec3::ZERO);
        assert!(close(camera.eye(), Vec3::new(10.0, -4.0, 6.0)), "the eye stays");
        camera.set_position(Vec3::new(0.0, -20.0, 5.0));
        assert!(close(camera.eye(), Vec3::new(0.0, -20.0, 5.0)));
        assert!(close(camera.target(), Vec3::ZERO), "the target stays");
    }

    #[test]
    fn the_eye_keeps_within_the_orbit() {
        let mut camera = OrbitCamera::default();
        camera.look_at(Vec3::ZERO, Vec3::ZERO);
        assert!((camera.eye().distance(Vec3::ZERO) - DISTANCE[0]).abs() < 1e-5);
        camera.look_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        assert!(camera.eye().z < 10.0, "not straight above: the orbit stops before");
        camera.set_fov(500.0);
        assert_eq!(camera.fov(), 170.0);
    }

    /// The clip coordinates of `point`: x, y and the depth, divided by w.
    fn clip(camera: &OrbitCamera, aspect: f32, point: Vec3) -> Vec3 {
        let clip = camera.view_proj(aspect) * point.extend(1.0);
        clip.truncate() / clip.w
    }

    #[test]
    fn far_points_are_seen_and_nearer_is_deeper_in_reverse_z() {
        let camera = OrbitCamera::default();
        let forward = (camera.target() - camera.eye()).normalize();
        let far = clip(&camera, 1.5, camera.eye() + forward * 50_000.0);
        let near = clip(&camera, 1.5, camera.eye() + forward * 10.0);
        assert!(far.z > 0.0 && far.z <= 1.0, "{far}");
        assert!(near.z > far.z, "nearer is greater");
    }

    #[test]
    fn a_large_box_framed_is_all_in_view_even_in_a_tall_view() {
        let mut camera = OrbitCamera::default();
        camera.set_aspect(0.5);
        let (min, max) = (
            Vec3::new(-10_000.0, -10_000.0, 0.0),
            Vec3::new(10_000.0, 10_000.0, 500.0),
        );
        camera.frame(min, max);
        for corner in 0..8 {
            let pick = |bit: usize, low: f32, high: f32| if corner & bit == 0 { low } else { high };
            let point = Vec3::new(pick(1, min.x, max.x), pick(2, min.y, max.y), pick(4, min.z, max.z));
            let seen = clip(&camera, 0.5, point);
            assert!(seen.z > 0.0 && seen.z <= 1.0, "corner {corner}: {seen}");
            assert!(seen.x.abs() <= 1.0 && seen.y.abs() <= 1.0, "corner {corner}: {seen}");
        }
    }

    #[test]
    fn a_framed_box_is_seen_whole_from_the_same_direction() {
        let mut camera = OrbitCamera::default();
        let direction = (camera.eye() - camera.target()).normalize();
        camera.frame(Vec3::new(-10.0, -10.0, 0.0), Vec3::new(10.0, 10.0, 4.0));
        assert!(close(camera.target(), Vec3::new(0.0, 0.0, 2.0)));
        assert!(close((camera.eye() - camera.target()).normalize(), direction));
        let radius = Vec3::new(20.0, 20.0, 4.0).length() / 2.0;
        assert!(camera.eye().distance(camera.target()) >= radius);
    }
}
