use uniwow_api::egui;
use uniwow_api::glam::{Mat4, Vec3};

/// The angle above or below the ground the orbit stops at, in radians.
const PITCH: f32 = 1.5;
/// The nearest and farthest the eye goes from the target.
pub const DISTANCE: [f32; 2] = [0.5, 100_000.0];
/// The narrowest and widest angle of view, in degrees.
pub const FOV: [f32; 2] = [1.0, 170.0];
/// How far from the origin the target goes on each axis; the eye goes as far again, its distance
/// to the target being at most as much.
pub const REACH: f64 = 100_000.0;

/// Camera looking at a target point, which it turns around or flies with. Z is up.
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
    /// The direction from the target to the eye.
    fn back(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        Vec3::new(cos_pitch * cos_yaw, cos_pitch * sin_yaw, sin_pitch)
    }

    pub fn eye(&self) -> Vec3 {
        self.target + self.distance * self.back()
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

    /// Turns the eye around the target by a drag of `delta` points.
    pub fn orbit(&mut self, delta: egui::Vec2) {
        self.yaw -= delta.x * 0.008;
        self.pitch = (self.pitch + delta.y * 0.008).clamp(-PITCH, PITCH);
    }

    /// Turns the view around the eye by a drag of `delta` points: a drag to the right looks to the
    /// right, up looks up. The eye stays, unless the target would go beyond reach of the origin.
    pub fn look(&mut self, delta: egui::Vec2) {
        let eye = self.eye();
        self.orbit(delta);
        self.target = within_reach(eye - self.distance * self.back());
    }

    /// Moves the eye and the target together by `step` yards: forward along the view, right
    /// across it, and up along Z; the target within reach of the origin.
    pub fn fly(&mut self, step: Vec3) {
        let forward = -self.back();
        let right = forward.cross(Vec3::Z).normalize_or_zero();
        self.target = within_reach(self.target + forward * step.x + right * step.y + Vec3::Z * step.z);
    }
}

fn within_reach(point: Vec3) -> Vec3 {
    let reach = Vec3::splat(REACH as f32);
    point.clamp(-reach, reach)
}

#[cfg(test)]
mod tests {
    use uniwow_api::egui;
    use uniwow_api::glam::Vec3;

    use super::{DISTANCE, OrbitCamera, REACH};

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
    fn a_flight_keeps_the_target_within_reach_and_the_eye_within_twice() {
        let mut camera = OrbitCamera::default();
        camera.look_at(Vec3::new(0.0, -100_000.0, 0.0), Vec3::ZERO);
        let reach = REACH as f32;
        for _ in 0..100 {
            camera.fly(Vec3::new(-10_000.0, 10_000.0, 10_000.0));
            assert!(
                camera.target().abs().max_element() <= reach,
                "flown: {}",
                camera.target()
            );
            camera.look(egui::vec2(100.0, 0.0));
            assert!(
                camera.target().abs().max_element() <= reach,
                "looked: {}",
                camera.target()
            );
        }
        assert!(camera.eye().abs().max_element() <= 2.0 * reach, "{}", camera.eye());
    }

    #[test]
    fn flying_moves_the_eye_and_the_target_forward_right_and_up_as_seen() {
        let mut camera = OrbitCamera::default();
        camera.look_at(Vec3::new(0.0, -10.0, 5.0), Vec3::new(0.0, 0.0, 5.0));
        camera.fly(Vec3::new(2.0, 0.0, 0.0));
        assert!(
            close(camera.eye(), Vec3::new(0.0, -8.0, 5.0)),
            "forward: {}",
            camera.eye()
        );
        assert!(close(camera.target(), Vec3::new(0.0, 2.0, 5.0)));
        let before = clip(&camera, 1.5, Vec3::new(0.0, 2.0, 5.0));
        camera.fly(Vec3::new(0.0, 3.0, 1.0));
        assert!(
            close(camera.eye(), Vec3::new(3.0, -8.0, 6.0)),
            "right and up: {}",
            camera.eye()
        );
        let after = clip(&camera, 1.5, Vec3::new(0.0, 2.0, 5.0));
        assert!(
            after.x < before.x && after.y < before.y,
            "what was ahead goes left and down"
        );
    }

    #[test]
    fn looking_keeps_the_eye_and_turns_the_view_the_way_of_the_drag() {
        let mut camera = OrbitCamera::default();
        camera.look_at(Vec3::new(0.0, -10.0, 5.0), Vec3::new(0.0, 0.0, 5.0));
        let ahead = Vec3::new(0.0, 0.0, 5.0);
        camera.look(egui::vec2(30.0, 0.0));
        assert!(close(camera.eye(), Vec3::new(0.0, -10.0, 5.0)), "the eye stays");
        assert!(clip(&camera, 1.5, ahead).x < 0.0, "a drag to the right turns right");
        camera.look(egui::vec2(-30.0, -30.0));
        assert!(close(camera.eye(), Vec3::new(0.0, -10.0, 5.0)));
        assert!(clip(&camera, 1.5, ahead).y < 0.0, "a drag up looks up");
        let target = camera.target();
        camera.orbit(egui::vec2(50.0, 10.0));
        assert!(close(camera.target(), target), "an orbit keeps the target");
        assert!(!close(camera.eye(), Vec3::new(0.0, -10.0, 5.0)));
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
