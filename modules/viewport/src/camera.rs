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
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::new(0.0, 0.0, 1.0),
            yaw: -2.2,
            pitch: 0.45,
            distance: 14.0,
            fov: 45.0,
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

    /// Fits the box from `min` to `max` in the view, seen from the same direction.
    pub fn frame(&mut self, min: Vec3, max: Vec3) {
        let radius = ((max - min).length() / 2.0).max(DISTANCE[0]);
        self.target = (min + max) / 2.0;
        self.distance = (radius / (self.fov.to_radians() / 2.0).sin()).clamp(DISTANCE[0], DISTANCE[1]);
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Z);
        let proj = Mat4::perspective_rh(self.fov.to_radians(), aspect, 0.1, 5000.0);
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
