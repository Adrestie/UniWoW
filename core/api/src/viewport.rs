//! Interface of the "viewport" service: a 3D view to which modules add their drawing.

use std::sync::Arc;
use std::time::Duration;

use crate::{ServiceKey, egui_wgpu, glam, wgpu};

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("viewport");

pub type Handle = Arc<dyn Viewport>;

/// Shared between threads (rule T3): a job may add a layer.
pub trait Viewport: Send + Sync {
    /// Adds a drawing layer. `owner` is the id of the module adding it.
    fn add_layer(&self, owner: &str, layer: Box<dyn Layer>);

    /// Removes every layer added by `owner`.
    fn remove_layers(&self, owner: &str);

    /// Formats of the render target, for modules that build their pipelines ahead, in a job.
    fn target(&self) -> Target;

    /// Waits for the frame signal of a frame after `after`, the number of the last one seen, for
    /// `timeout` at most and never more than `MAX_FRAME_WAIT`, so that a waiting thread checks its
    /// cancellation: the frame to come, or none. The signal is given once the viewport has
    /// submitted a frame, so that what a thread writes then is drawn by the next one; it is not
    /// given while the view is not drawn.
    fn wait_frame(&self, after: u64, timeout: Duration) -> Option<Frame>;
}

/// The longest a thread waits for the frame signal at a time.
pub const MAX_FRAME_WAIT: Duration = Duration::from_millis(100);

/// The frame to come, as the frame signal gives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Counted from 1, the first frame drawn.
    pub number: u64,
    /// Seconds since the viewport started, as `View::time`, estimated from the frames before.
    pub time: f32,
}

/// Formats of the render target a layer draws into; its pipelines must match them.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub color_format: wgpu::TextureFormat,
    pub depth_format: wgpu::TextureFormat,
    pub sample_count: u32,
    /// How a layer's pipeline compares depths. Reverse Z: the depth is 1 at the near plane and
    /// falls towards 0 at infinity, where it is cleared; nearer is greater.
    pub depth_compare: wgpu::CompareFunction,
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

/// Drawn on the interface thread, but may be created on any thread.
pub trait Layer: Send {
    /// Writes what the layer draws with the view of this frame, such as its buffers of camera and
    /// instances, before any bundle is drawn, recording nothing. It runs at each frame, as `draw`
    /// does, inside a validation error scope: a layer that panics or fails here is removed and its
    /// module reported. Nothing by default.
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        let _ = (gpu, view);
    }

    /// The version of what the layer records: its bundle is kept from frame to frame while the
    /// version stays the same, and recorded again when it changes, or when the device is created
    /// again. None, by default, records it at every frame. A layer whose bundle is kept changes
    /// what it draws through its buffers, written in `prepare`, or by a new version.
    fn version(&self) -> Option<u64> {
        None
    }

    /// Records the layer's drawing into its own render bundle, created by the viewport with the
    /// formats and sample count of `target`; create pipelines lazily from `gpu.device` to match.
    ///
    /// The viewport validates the bundle on its own: a layer that panics or records an invalid
    /// command is removed and its module reported as failed, without affecting the others.
    fn draw<'a>(
        &'a mut self,
        gpu: &egui_wgpu::RenderState,
        target: &Target,
        view: &View,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    );

    /// What the layer drew with the view of the last frame, for the statistics of the view. Called
    /// after `prepare` and `draw`; nothing by default.
    fn stats(&self) -> LayerStats {
        LayerStats::default()
    }
}

/// What a layer drew in a frame, as the statistics of the view show it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerStats {
    pub draws: u64,
    pub triangles: u64,
    /// What it holds on the GPU.
    pub bytes: u64,
    /// What it drew, counted its own way, such as `54 tiles`.
    pub items: String,
    /// The time its module spent on the interface thread for it this frame, outside `prepare` and
    /// `draw`, such as steering what it loads.
    pub steering: Duration,
}
