//! The layer of the models of the sky (step 9.7e): the instances of the looks drawn as the sky
//! (`Geosets::Sky`), the models of `LightSkybox.dbc` and those of the buildings (`MOSB`), drawn
//! around the eye, at infinity, where the depth is still that of the clearing, so behind
//! everything, over the sky of the ground and under every blended batch of the scene: in a stage of
//! its own (`Stage::Sky`), at the start of the first blended phase, each model in the order its
//! owner gave it, its batches in theirs. The layer of the models finds them and poses them with the
//! looks of their own; this one draws what it found for the frame, from the pass.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use uniwow_api::viewport::{Drawing, Layer, LayerStats, Phase, Stage, Target, View};
use uniwow_api::{egui_wgpu, wgpu};

use crate::loading::Ready;
use crate::lock;

/// A group of instances of a model of the sky: its look, the arena of the instances and its
/// instances there.
#[derive(Clone)]
pub struct SkyGroup {
    pub look: Arc<Ready>,
    pub buffer: Arc<wgpu::Buffer>,
    pub instances: Range<u32>,
}

/// What the layer of the models found of the sky for a frame: its camera, the bones of the frame
/// (with the pool; none without, the shader reading none), and the groups, in their order.
#[derive(Clone, Default)]
pub struct SkyFrame {
    pub camera: Option<wgpu::BindGroup>,
    pub posing: Option<wgpu::BindGroup>,
    pub groups: Vec<SkyGroup>,
}

impl SkyFrame {
    /// The draws and triangles of the frame: a draw per batch of each group at its finest level.
    pub fn counts(&self) -> (u64, u64) {
        let mut counts = (0, 0);
        for group in self.groups.iter().filter(|group| group.look.levels() > 0) {
            for (_, indices) in group.look.batches(0) {
                counts.0 += 1;
                counts.1 += u64::from(indices / 3) * u64::from(group.instances.len() as u32);
            }
        }
        counts
    }
}

pub struct SkyLayer {
    frame: Arc<Mutex<SkyFrame>>,
    stats: LayerStats,
}

impl SkyLayer {
    pub fn new(frame: Arc<Mutex<SkyFrame>>) -> Self {
        Self {
            frame,
            stats: LayerStats::default(),
        }
    }
}

impl Layer for SkyLayer {
    fn stage(&self) -> Stage {
        Stage::Sky
    }

    fn drawing(&self) -> Drawing {
        Drawing::Pass
    }

    fn draw_pass(
        &mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        phase: Phase,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        if phase != Phase::Beyond {
            return;
        }
        // Taken once the layer of the models has prepared the frame, which every layer does before
        // any draws.
        let frame = lock(&self.frame).clone();
        let (draws, triangles) = frame.counts();
        self.stats = LayerStats {
            draws,
            triangles,
            items: format!("{} models of the sky", frame.groups.len()),
            ..LayerStats::default()
        };
        let Some(camera) = &frame.camera else {
            return;
        };
        for group in &frame.groups {
            let Ready::Own(look) = &*group.look else {
                continue;
            };
            let (Some(skin), Some(batches)) = (look.model.skins.first(), look.skins.first()) else {
                continue;
            };
            pass.set_bind_group(0, camera, &[]);
            if let Some(posing) = &frame.posing {
                pass.set_bind_group(2, posing, &[]);
            }
            pass.set_vertex_buffer(0, look.model.vertices.slice(..));
            pass.set_vertex_buffer(1, group.buffer.slice(..));
            pass.set_index_buffer(skin.indices.slice(..), skin.format);
            for batch in batches {
                pass.set_pipeline(&batch.pipeline);
                pass.set_bind_group(1, &batch.group, &[]);
                pass.draw_indexed(batch.indices.clone(), 0, group.instances.clone());
            }
        }
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
