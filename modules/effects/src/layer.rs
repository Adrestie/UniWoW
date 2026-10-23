//! The effect drawn in the 3D view: at each frame its time moves on (when it plays), its particles
//! and ribbons are brought to that time, and its triangles built on the CPU are written to one
//! buffer, drawn batch by batch, the opaque ones with the opaque world, the blended ones after it,
//! far first. The bones and the attachment points of the reference are drawn as dots over it.

use std::sync::{Arc, Mutex};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::vfx::anim::{self, Playback};
use uniwow_api::vfx::frame::{self, Eye, Vertex};
use uniwow_api::vfx::sim::Simulation;
use uniwow_api::viewport::{Drawing, Layer, LayerStats, Phase, Target, View};
use uniwow_api::{egui_wgpu, wgpu};

use crate::gpu::{self, Gpu, State};
use crate::{Stage, lock};

/// What is drawn of a frame: a range of the buffer with its state and texture.
struct Draw {
    state: State,
    /// The model's texture, none for white.
    texture: Option<usize>,
    range: std::ops::Range<u32>,
    blended: bool,
}

pub struct EffectLayer {
    stage: Arc<Mutex<Stage>>,
    incoming: Arc<Mutex<Option<Arc<Gpu>>>>,
    gpu: Option<Arc<Gpu>>,
    vertices: Option<(wgpu::Buffer, usize)>,
    sim: Option<(u64, Simulation)>,
    draws: Vec<Draw>,
    textures: Option<Arc<Vec<Option<wgpu::BindGroup>>>>,
    /// The view's time at the last frame, for the time the effect plays.
    last: Option<f32>,
    stats: LayerStats,
}

impl EffectLayer {
    pub fn new(stage: Arc<Mutex<Stage>>, incoming: Arc<Mutex<Option<Arc<Gpu>>>>) -> Self {
        Self {
            stage,
            incoming,
            gpu: None,
            vertices: None,
            sim: None,
            draws: Vec::new(),
            textures: None,
            last: None,
            stats: LayerStats::default(),
        }
    }
}

/// A dot facing the eye, `radius` yards, in the model's space.
fn dot(out: &mut Vec<Vertex>, at: Vec3, eye: &Eye, radius: f32, color: [f32; 4]) {
    let (r, u) = (eye.right * radius, eye.up * radius);
    let corners = [at - r + u, at - r - u, at + r - u, at + r + u];
    for k in [0, 1, 2, 0, 2, 3] {
        out.push(Vertex {
            pos: corners[k].to_array(),
            uv: [0.5, 0.5],
            color,
        });
    }
}

impl Layer for EffectLayer {
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        if self.gpu.is_none() {
            self.gpu = lock(&self.incoming).take();
        }
        self.draws.clear();
        let Some(resources) = self.gpu.clone() else {
            return;
        };
        let dt = self.last.map_or(0.0, |last| (view.time - last).max(0.0));
        self.last = Some(view.time);
        let (effect, textures, anchor, time, holds, show_bones, markers, ground) = {
            let mut stage = lock(&self.stage);
            let length = stage
                .effect
                .as_ref()
                .map_or(0.0, |e| Playback::of(&e.model, stage.holds).length() / 1000.0);
            if stage.playing && length > 0.0 {
                stage.time += f64::from(dt) * f64::from(stage.speed);
                if stage.time > length {
                    stage.time = if stage.looping { stage.time % length } else { length };
                    stage.playing &= stage.looping;
                }
            }
            stage.length = length;
            (
                stage.effect.clone(),
                stage.textures.clone(),
                stage.anchor,
                stage.time,
                stage.holds,
                stage.show_bones,
                stage.markers.clone(),
                stage.ground.clone(),
            )
        };
        self.textures = textures;
        let model_matrix = Mat4::from_translation(anchor);
        let eye = Eye {
            position: view.eye - anchor,
            right: view.view.row(0).truncate().normalize_or(Vec3::X),
            up: view.view.row(1).truncate().normalize_or(Vec3::Z),
        };
        let mut vertices: Vec<Vertex> = Vec::new();
        let mut alive = 0;
        if let Some(effect) = &effect {
            let model = &effect.model;
            let playback = Playback::of(model, holds);
            if self.sim.as_ref().is_none_or(|(version, _)| *version != effect.version) {
                self.sim = Some((effect.version, Simulation::new(model, 7)));
            }
            let (_, sim) = self.sim.as_mut().expect("made above");
            sim.seek(model, &playback, time);
            alive = sim.alive();
            let clock = playback.clock(time * 1000.0);
            let pose = anim::pose(model, &clock, Some(eye.position));
            let mut batches = frame::build(model, effect.skin.as_ref(), sim, &clock, &pose, &eye);
            if let Some(ground) = &ground {
                for batch in batches.iter_mut().filter(|b| b.projected) {
                    frame::drape(batch, ground, 16);
                }
            }
            frame::sort(&mut batches);
            for batch in batches {
                let start = vertices.len() as u32;
                vertices.extend_from_slice(&batch.vertices);
                self.draws.push(Draw {
                    state: State::of(batch.blend, batch.material_flags),
                    texture: batch.texture,
                    range: start..vertices.len() as u32,
                    blended: batch.blend >= 2,
                });
            }
            if show_bones {
                let start = vertices.len() as u32;
                for (bone, m) in model.bones.iter().zip(&pose) {
                    dot(
                        &mut vertices,
                        m.transform_point3(Vec3::from(bone.pivot)),
                        &eye,
                        0.03,
                        [1.0, 1.0, 1.0, 0.9],
                    );
                }
                self.draws.push(Draw {
                    state: State::of(2, 0x4 | 0x8),
                    texture: None,
                    range: start..vertices.len() as u32,
                    blended: true,
                });
            }
        }
        if !markers.is_empty() {
            let start = vertices.len() as u32;
            for (at, color) in &markers {
                dot(&mut vertices, Vec3::from(*at) - anchor, &eye, 0.05, *color);
            }
            self.draws.push(Draw {
                state: State::of(2, 0x4 | 0x8),
                texture: None,
                range: start..vertices.len() as u32,
                blended: true,
            });
        }
        let floats: Vec<f32> = vertices
            .iter()
            .flat_map(|v| {
                [
                    v.pos[0], v.pos[1], v.pos[2], v.uv[0], v.uv[1], v.color[0], v.color[1], v.color[2], v.color[3],
                ]
            })
            .collect();
        if !floats.is_empty() {
            if self.vertices.as_ref().is_none_or(|(_, size)| *size < floats.len()) {
                let buffer = gpu::vertex_buffer(&gpu.device, floats.len());
                let size = (buffer.size() / 4) as usize;
                self.vertices = Some((buffer, size));
            }
            let (buffer, _) = self.vertices.as_ref().expect("made above");
            gpu.queue.write_buffer(buffer, 0, gpu::bytes(&floats));
        }
        gpu.queue.write_buffer(
            &resources.globals,
            0,
            gpu::bytes(&gpu::globals_values(&view.view_proj, &model_matrix)),
        );
        self.stats = LayerStats {
            draws: self.draws.len() as u64,
            triangles: vertices.len() as u64 / 3,
            bytes: (floats.len() * 4) as u64,
            items: match &effect {
                Some(e) => format!("{}: {alive} particles", e.name),
                None => "no effect".into(),
            },
            ..LayerStats::default()
        };
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
        let blended = match phase {
            Phase::Opaque => false,
            Phase::Near => true,
            _ => return,
        };
        let (Some(resources), Some((buffer, _))) = (self.gpu.as_ref(), self.vertices.as_ref()) else {
            return;
        };
        pass.set_bind_group(0, &resources.globals_group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..));
        for draw in self
            .draws
            .iter()
            .filter(|d| d.blended == blended && !d.range.is_empty())
        {
            let Some(pipeline) = resources.pipelines.get(&draw.state) else {
                continue;
            };
            let group = draw
                .texture
                .and_then(|t| {
                    self.textures
                        .as_ref()
                        .and_then(|all| all.get(t))
                        .and_then(Option::as_ref)
                })
                .unwrap_or(&resources.white);
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, group, &[]);
            pass.draw(draw.range.clone(), 0..1);
        }
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
