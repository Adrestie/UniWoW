use uniwow_api::egui;
use uniwow_api::glam::{Mat4, Vec3};

/// Camera turning around a target point. Z is up.
pub struct OrbitCamera {
    target: Vec3,
    /// Angle around Z, in radians.
    yaw: f32,
    /// Angle above the ground, in radians.
    pitch: f32,
    distance: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::new(0.0, 0.0, 1.0),
            yaw: -2.2,
            pitch: 0.45,
            distance: 14.0,
        }
    }
}

impl OrbitCamera {
    pub fn eye(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        self.target + self.distance * Vec3::new(cos_pitch * cos_yaw, cos_pitch * sin_yaw, sin_pitch)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Z);
        let proj = Mat4::perspective_rh(45f32.to_radians(), aspect, 0.1, 5000.0);
        proj * view
    }

    /// Left drag orbits, right or middle drag pans, the wheel zooms.
    pub fn handle_input(&mut self, ui: &egui::Ui, response: &egui::Response) {
        let delta = response.drag_delta();
        if response.dragged_by(egui::PointerButton::Primary) {
            self.yaw -= delta.x * 0.008;
            self.pitch = (self.pitch + delta.y * 0.008).clamp(-1.5, 1.5);
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
                self.distance = (self.distance * (-scroll * 0.0015).exp()).clamp(0.5, 3000.0);
            }
        }
    }
}
