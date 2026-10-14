//! The layers of the models of the sky (step 9.7e): the instances of the looks drawn as the sky,
//! the models of `LightSkybox.dbc` and those of the buildings (`MOSB`, `Geosets::Sky`) and the stars
//! (`Geosets::Stars`), drawn around the eye, painted before the world over the sky of the ground: the
//! stars under the sun, the moon and the clouds (`Stage::Stars`), the others over them (`Stage::Sky`),
//! as the client draws them, each model in the order its owner gave it, its batches in theirs. The
//! layer of the models finds them and poses them with the looks of their own; these draw what it
//! found for the frame, from the pass.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use uniwow_api::viewport::{Drawing, Layer, LayerStats, Phase, Stage, Target, View};
use uniwow_api::{egui_wgpu, wgpu};

use crate::loading::Ready;
use crate::lock;

/// A group of instances of a model of the sky: its look, the arena of the instances, its instances
/// there, and whether it is the stars.
#[derive(Clone)]
pub struct SkyGroup {
    pub look: Arc<Ready>,
    pub buffer: Arc<wgpu::Buffer>,
    pub instances: Range<u32>,
    pub stars: bool,
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
    /// The groups of the stars, or the others, in their order.
    pub fn groups(&self, stars: bool) -> impl Iterator<Item = &SkyGroup> {
        self.groups.iter().filter(move |group| group.stars == stars)
    }

    /// The draws and triangles of the groups of the stars, or of the others: a draw per batch of
    /// each group at its finest level.
    pub fn counts(&self, stars: bool) -> (u64, u64) {
        let mut counts = (0, 0);
        for group in self.groups(stars).filter(|group| group.look.levels() > 0) {
            for (_, indices) in group.look.batches(0) {
                counts.0 += 1;
                counts.1 += u64::from(indices / 3) * u64::from(group.instances.len() as u32);
            }
        }
        counts
    }
}

/// A layer of the models of the sky: the stars when `stars` says, the others otherwise.
pub struct SkyLayer {
    frame: Arc<Mutex<SkyFrame>>,
    stars: bool,
    stats: LayerStats,
}

impl SkyLayer {
    pub fn new(frame: Arc<Mutex<SkyFrame>>, stars: bool) -> Self {
        Self {
            frame,
            stars,
            stats: LayerStats::default(),
        }
    }
}

impl Layer for SkyLayer {
    fn stage(&self) -> Stage {
        if self.stars { Stage::Stars } else { Stage::Sky }
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
        if phase != Phase::Sky {
            return;
        }
        // Taken once the layer of the models has prepared the frame, which every layer does before
        // any draws.
        let frame = lock(&self.frame).clone();
        let (draws, triangles) = frame.counts(self.stars);
        let what = if self.stars { "of the stars" } else { "of the sky" };
        self.stats = LayerStats {
            draws,
            triangles,
            items: format!("{} models {what}", frame.groups(self.stars).count()),
            ..LayerStats::default()
        };
        let Some(camera) = &frame.camera else {
            return;
        };
        for group in frame.groups(self.stars) {
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
