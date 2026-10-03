//! Interface of the "viewport" service: a 3D view to which features add their drawing.

use std::rc::Rc;

use crate::{ServiceKey, egui_wgpu, glam, wgpu};

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("viewport");

pub type Handle = Rc<dyn Viewport>;

pub trait Viewport {
    /// Adds a drawing layer. `owner` is the id of the feature adding it.
    fn add_layer(&self, owner: &str, layer: Box<dyn Layer>);

    /// Removes every layer added by `owner`.
    fn remove_layers(&self, owner: &str);
}

/// Formats of the render target a layer draws into; its pipelines must match them.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub color_format: wgpu::TextureFormat,
    pub depth_format: wgpu::TextureFormat,
    pub sample_count: u32,
}

/// The camera and frame being drawn. World axes: X and Y on the ground, Z up.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub view_proj: glam::Mat4,
    pub eye: glam::Vec3,
    /// Size of the target in pixels.
    pub size: [u32; 2],
    /// Seconds since the viewport started.
    pub time: f32,
}

pub trait Layer {
    /// Records the layer's drawing into its own render bundle, created by the viewport with the
    /// formats and sample count of `target`; create pipelines lazily from `gpu.device` to match.
    ///
    /// The viewport validates the bundle on its own: a layer that panics or records an invalid
    /// command is removed and its feature reported as failed, without affecting the others.
    fn draw<'a>(
        &'a mut self,
        gpu: &egui_wgpu::RenderState,
        target: &Target,
        view: &View,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    );
}
