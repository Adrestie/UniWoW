//! The Effect view: the effect alone in a panel of its own, drawn apart from the world, with its
//! own camera turning about it. A neutral ground with its grid (sloped as asked, for what is laid
//! on the ground and what cuts into it), the reference character and its attachment points, a
//! background of the colour asked, and a gizmo moving the part chosen. The effect is the same as
//! the 3D view's, in memory, played at the same time.

use std::sync::{Arc, Mutex};

use uniwow_api::egui::{self, Color32, Pos2, Sense, Stroke, Vec2};
use uniwow_api::glam::{Mat4, Vec3, Vec4Swizzles};
use uniwow_api::vfx::anim::{self, Clock, Playback};
use uniwow_api::vfx::frame::{self, Eye, Ground, Vertex};
use uniwow_api::vfx::m2::{Model, Skin};
use uniwow_api::vfx::sim::Simulation;
use uniwow_api::vfx::texgen::Image;
use uniwow_api::viewport::Target;
use uniwow_api::{egui_wgpu, wgpu};

use crate::gpu::{self, Gpu, State};
use crate::{Stage, lock};

/// The reference character, drawn by the view itself: its model, its skin cut to its default
/// submeshes, the groups of its textures.
pub struct Character {
    pub model: Model,
    pub skin: Skin,
    pub groups: Vec<Option<wgpu::BindGroup>>,
    /// Its attachment points at the start of its Stand, by id.
    pub points: Vec<(u32, Vec3)>,
}

/// What a draw takes its texture from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Texture {
    Effect(usize),
    Character(usize),
    Grid,
    White,
}

struct Draw {
    state: State,
    texture: Texture,
    range: std::ops::Range<u32>,
}

struct Targets {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    resolved: wgpu::Texture,
    depth: wgpu::TextureView,
    texture_id: egui::TextureId,
}

/// A move of the gizmo: along an axis of the model, so many yards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moved {
    pub axis: usize,
    pub yards: f32,
    /// The drag let go.
    pub done: bool,
}

pub struct EffectView {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target: Vec3,
    /// The ground's slope, in degrees, rising along X.
    pub slope: f32,
    pub background: [f32; 3],
    pub ground: bool,
    pub character: bool,
    pub points: bool,
    targets: Option<Targets>,
    globals: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    vertices: Option<(wgpu::Buffer, usize)>,
    sim: Option<(u64, Simulation)>,
    grid: Option<wgpu::BindGroup>,
    still: Option<Simulation>,
    draws: Vec<Draw>,
    /// The axis of the gizmo held, and where the pointer took it.
    held: Option<(usize, Pos2)>,
}

impl Default for EffectView {
    fn default() -> Self {
        Self {
            yaw: 0.6,
            pitch: 0.45,
            distance: 7.0,
            target: Vec3::new(0.0, 0.0, 1.0),
            slope: 0.0,
            background: [0.16, 0.17, 0.19],
            ground: true,
            character: true,
            points: true,
            targets: None,
            globals: None,
            vertices: None,
            sim: None,
            grid: None,
            still: None,
            draws: Vec::new(),
            held: None,
        }
    }
}

/// What the user did in the view: the gizmo moved, a part clicked (its place among the points given).
pub struct Shown {
    pub moved: Option<Moved>,
    pub picked: Option<usize>,
}

/// How the camera looks at the effect.
#[derive(Clone, Copy, Debug)]
pub enum Angle {
    Grazing,
    Top,
    Three,
}

impl EffectView {
    pub fn angle(&mut self, angle: Angle) {
        let (yaw, pitch, distance) = match angle {
            Angle::Grazing => (0.0, 0.05, 7.0),
            Angle::Top => (0.0, 1.55, 9.0),
            Angle::Three => (0.785, 0.55, 8.0),
        };
        (self.yaw, self.pitch, self.distance) = (yaw, pitch, distance);
        self.target = Vec3::new(0.0, 0.0, if matches!(angle, Angle::Top) { 0.0 } else { 1.0 });
    }

    /// The whole effect in view: far enough for a large model (up to 60 yards), aimed at its middle.
    pub fn frame(&mut self, radius: f32) {
        self.distance = (radius * 2.6).clamp(4.0, 60.0);
        self.target = Vec3::new(0.0, 0.0, radius * 0.6);
    }

    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        self.target + Vec3::new(cp * cy, cp * sy, sp) * self.distance
    }

    fn matrices(&self, size: [u32; 2]) -> (Mat4, Mat4) {
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Z);
        let aspect = size[0] as f32 / size[1].max(1) as f32;
        (
            Mat4::perspective_infinite_reverse_rh(45f32.to_radians(), aspect, 0.05) * view,
            view,
        )
    }

    /// The ground's height at (x, y): its slope rising along X.
    pub fn height(&self, x: f32) -> f32 {
        x * self.slope.to_radians().tan()
    }

    fn targets(&mut self, gpu: &egui_wgpu::RenderState, size: [u32; 2], target: &Target) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let device = &gpu.device;
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let texture = |label, format, samples, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let msaa = texture(
            "effect view",
            target.color_format,
            target.sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&Default::default());
        let resolved = texture(
            "effect view resolved",
            target.color_format,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let depth = texture(
            "effect view depth",
            target.depth_format,
            target.sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&Default::default());
        let view = resolved.create_view(&Default::default());
        let mut renderer = gpu.renderer.write();
        let texture_id = match &self.targets {
            Some(old) => {
                renderer.update_egui_texture_from_wgpu_texture(device, &view, wgpu::FilterMode::Linear, old.texture_id);
                old.texture_id
            }
            None => renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear),
        };
        self.targets = Some(Targets {
            size,
            msaa,
            resolved,
            depth,
            texture_id,
        });
    }

    /// The triangles of the frame: ground, character, effect, points.
    fn build(
        &mut self,
        resources: &Gpu,
        gpu: &egui_wgpu::RenderState,
        stage: &Stage,
        character: Option<&Character>,
        view: &Mat4,
    ) -> Vec<Vertex> {
        let eye = Eye {
            position: self.eye(),
            right: view.row(0).xyz().normalize_or(Vec3::X),
            up: view.row(1).xyz().normalize_or(Vec3::Z),
        };
        let mut vertices: Vec<Vertex> = Vec::new();
        self.draws.clear();
        let push =
            |draws: &mut Vec<Draw>, vertices: &mut Vec<Vertex>, state: State, texture: Texture, part: &[Vertex]| {
                let start = vertices.len() as u32;
                vertices.extend_from_slice(part);
                draws.push(Draw {
                    state,
                    texture,
                    range: start..vertices.len() as u32,
                });
            };
        if self.ground {
            if self.grid.is_none() {
                self.grid = Some(gpu::texture_group(
                    &gpu.device,
                    &gpu.queue,
                    &resources.texture_layout,
                    &resources.samplers,
                    &grid_image(),
                    3,
                ));
            }
            // a square of 24 yards at least, wider for a large model; a cell a yard
            let radius = stage.effect.as_ref().map_or(3.0, |e| radius_of(&e.model));
            let half = (radius * 2.0).max(12.0).ceil();
            let corner = |x: f32, y: f32| Vertex {
                pos: [x, y, self.height(x)],
                uv: [(x + half) / 4.0, (y + half) / 4.0],
                color: [1.0; 4],
            };
            let quad = [
                corner(-half, -half),
                corner(half, -half),
                corner(half, half),
                corner(-half, -half),
                corner(half, half),
                corner(-half, half),
            ];
            push(&mut self.draws, &mut vertices, State::of(0, 0x4), Texture::Grid, &quad);
        }
        if let (true, Some(character)) = (self.character, character) {
            let still = self.still.get_or_insert_with(|| Simulation::new(&character.model, 0));
            let stand = character.model.sequences.iter().position(|s| s.id == 0).unwrap_or(0);
            let clock = Clock {
                sequence: stand,
                time: 0.0,
                global: 0.0,
            };
            let pose = anim::pose(&character.model, &clock, None);
            for batch in frame::build(&character.model, Some(&character.skin), still, &clock, &pose, &eye) {
                let texture = batch.texture.map_or(Texture::White, Texture::Character);
                push(
                    &mut self.draws,
                    &mut vertices,
                    State::of(batch.blend.min(1), batch.material_flags),
                    texture,
                    &batch.vertices,
                );
            }
            if self.points {
                let mut dots = Vec::new();
                for (id, at) in &character.points {
                    let chosen = stage.attach_id == *id;
                    if crate::ATTACHMENTS.iter().any(|(_, a)| a == id) {
                        dot(
                            &mut dots,
                            *at,
                            &eye,
                            0.05,
                            if chosen {
                                [1.0, 0.8, 0.1, 1.0]
                            } else {
                                [0.3, 0.8, 1.0, 0.8]
                            },
                        );
                    }
                }
                push(
                    &mut self.draws,
                    &mut vertices,
                    State::of(2, 0x4 | 0x8),
                    Texture::White,
                    &dots,
                );
            }
        }
        if let Some(effect) = &stage.effect {
            let model = &effect.model;
            let playback = Playback::of(model, stage.holds);
            if self.sim.as_ref().is_none_or(|(version, _)| *version != effect.version) {
                self.sim = Some((effect.version, Simulation::new(model, 7)));
            }
            let (_, sim) = self.sim.as_mut().expect("made above");
            sim.seek(model, &playback, stage.time);
            let clock = playback.clock(stage.time * 1000.0);
            let pose = anim::pose(model, &clock, Some(eye.position - stage.attach_offset));
            let mut local = eye;
            local.position -= stage.attach_offset;
            let mut batches = frame::build(model, effect.skin.as_ref(), sim, &clock, &pose, &local);
            let ground = self.ground_heights(stage.attach_offset);
            for batch in batches.iter_mut() {
                if batch.projected {
                    frame::drape(batch, &ground, 16);
                }
                for v in &mut batch.vertices {
                    for (k, o) in stage.attach_offset.to_array().iter().enumerate() {
                        v.pos[k] += o;
                    }
                }
            }
            frame::sort(&mut batches);
            for batch in batches {
                let texture = batch.texture.map_or(Texture::White, Texture::Effect);
                push(
                    &mut self.draws,
                    &mut vertices,
                    State::of(batch.blend, batch.material_flags),
                    texture,
                    &batch.vertices,
                );
            }
            if stage.show_bones {
                let mut dots = Vec::new();
                for (bone, m) in model.bones.iter().zip(&pose) {
                    dot(
                        &mut dots,
                        m.transform_point3(Vec3::from(bone.pivot)) + stage.attach_offset,
                        &eye,
                        0.03,
                        [1.0; 4],
                    );
                }
                push(
                    &mut self.draws,
                    &mut vertices,
                    State::of(2, 0x4 | 0x8),
                    Texture::White,
                    &dots,
                );
            }
        }
        vertices
    }

    /// The sloped ground as heights, in the space of an effect standing at `offset`.
    fn ground_heights(&self, offset: Vec3) -> Ground {
        let count = 49;
        let step = 0.5;
        let low = -12.0;
        let heights = (0..count * count)
            .map(|k| {
                let x = low + (k % count) as f32 * step;
                Some(self.height(x + offset.x) - offset.z)
            })
            .collect();
        Ground {
            low: [low, low],
            step,
            count,
            heights,
        }
    }

    /// Draws the view into its texture and shows it in `ui`, turning the camera as the pointer
    /// drags and zooming as the wheel turns; the gizmo at `gizmo` (a point of the world, the part
    /// chosen), its move given back.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        (gpu, resources, target): (&egui_wgpu::RenderState, &Arc<Gpu>, &Target),
        stage: &Arc<Mutex<Stage>>,
        character: Option<&Character>,
        gizmo: Option<Vec3>,
        points: &[Vec3],
    ) -> Shown {
        // room below for the line of help
        let available = (ui.available_size() - Vec2::new(0.0, 22.0)).max(Vec2::new(160.0, 120.0));
        let pixels = ui.ctx().pixels_per_point();
        let size = [
            (available.x * pixels).round().max(16.0) as u32,
            (available.y * pixels).round().max(16.0) as u32,
        ];
        self.targets(gpu, size, target);
        let (view_proj, view) = self.matrices(size);
        let vertices = {
            let mut stage = lock(stage);
            stage.tick();
            self.build(resources, gpu, &stage, character, &view)
        };
        let effect_textures = lock(stage).textures.clone();
        self.render(
            gpu,
            resources,
            &vertices,
            &view_proj,
            effect_textures.as_deref(),
            character,
        );
        let targets = self.targets.as_ref().expect("made above");
        let response = ui.add(egui::Image::new((targets.texture_id, available)).sense(Sense::click_and_drag()));
        let rect = response.rect;
        let project = |p: Vec3| -> Option<Pos2> {
            let clip = view_proj * p.extend(1.0);
            (clip.w > 0.0).then(|| {
                let ndc = clip.xyz() / clip.w;
                Pos2::new(
                    rect.left() + (ndc.x * 0.5 + 0.5) * rect.width(),
                    rect.top() + (0.5 - ndc.y * 0.5) * rect.height(),
                )
            })
        };
        let mut moved = None;
        // the gizmo: an arrow an axis, a yard long
        if let Some(at) = gizmo.and_then(|at| project(at).map(|p| (at, p))) {
            let (origin3, origin) = at;
            let painter = ui.painter_at(rect);
            let colours = [
                Color32::from_rgb(220, 70, 60),
                Color32::from_rgb(80, 170, 60),
                Color32::from_rgb(60, 120, 230),
            ];
            let mut ends = Vec::new();
            for (axis, colour) in colours.iter().enumerate() {
                let mut direction = Vec3::ZERO;
                direction[axis] = 1.0;
                if let Some(end) = project(origin3 + direction * (self.distance * 0.12)) {
                    let held = self.held.is_some_and(|(a, _)| a == axis);
                    painter.line_segment([origin, end], Stroke::new(if held { 4.0 } else { 2.5 }, *colour));
                    painter.circle_filled(end, 5.0, *colour);
                    ends.push((axis, end));
                }
            }
            let pointer = response.interact_pointer_pos();
            if response.drag_started_by(egui::PointerButton::Primary)
                && let Some(p) = pointer
            {
                self.held = ends
                    .iter()
                    .map(|(axis, end)| (*axis, distance_to_segment(p, origin, *end)))
                    .filter(|(_, d)| *d < 8.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(axis, _)| (axis, p));
            }
            if let (Some((axis, from)), Some(p)) = (self.held, pointer)
                && let Some((_, end)) = ends.iter().find(|(a, _)| *a == axis)
            {
                // the pointer's move along the axis drawn, in yards
                let along = *end - origin;
                let length = along.length().max(1.0);
                let yards = (p - from).dot(along / length) / length * (self.distance * 0.12);
                if yards.abs() > 1e-4 {
                    moved = Some(Moved {
                        axis,
                        yards,
                        done: false,
                    });
                    self.held = Some((axis, p));
                }
            }
            if response.drag_stopped()
                && let Some((axis, _)) = self.held.take()
            {
                moved = Some(Moved {
                    axis,
                    yards: 0.0,
                    done: true,
                });
            }
        }
        if self.held.is_none() {
            if response.dragged_by(egui::PointerButton::Primary) || response.dragged_by(egui::PointerButton::Secondary)
            {
                let delta = response.drag_delta();
                self.yaw -= delta.x * 0.01;
                self.pitch = (self.pitch + delta.y * 0.01).clamp(-1.5, 1.55);
            }
            if response.dragged_by(egui::PointerButton::Middle) {
                let delta = response.drag_delta() * self.distance * 0.002;
                let right = view.row(0).xyz();
                let up = view.row(1).xyz();
                self.target += -right * delta.x + up * delta.y;
            }
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.distance = (self.distance * (1.0 - scroll * 0.002)).clamp(0.5, 200.0);
            }
        }
        // the part chosen, ringed; a click on another chooses it
        if let Some(at) = gizmo.and_then(project) {
            let pulse = (ui.input(|i| i.time) * 4.0).sin() as f32 * 2.0;
            ui.painter_at(rect)
                .circle_stroke(at, 12.0 + pulse, Stroke::new(2.0, Color32::from_rgb(255, 200, 40)));
        }
        let mut picked = None;
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            picked = points
                .iter()
                .enumerate()
                .filter_map(|(k, at)| project(*at).map(|s| (k, s.distance(p))))
                .filter(|(_, d)| *d < 24.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(k, _)| k);
        }
        // F frames the whole effect
        if response.hovered() && ui.input(|i| i.key_pressed(egui::Key::F)) {
            let radius = lock(stage).effect.as_ref().map_or(3.0, |e| radius_of(&e.model));
            self.frame(radius);
        }
        ui.weak("drag: turn · middle button: move · wheel: zoom · F: frame · click: choose a part · arrows: move it");
        ui.ctx().request_repaint();
        Shown { moved, picked }
    }

    fn render(
        &mut self,
        gpu: &egui_wgpu::RenderState,
        resources: &Gpu,
        vertices: &[Vertex],
        view_proj: &Mat4,
        effect: Option<&Vec<Option<wgpu::BindGroup>>>,
        character: Option<&Character>,
    ) {
        let device = &gpu.device;
        let (globals, globals_group) = self.globals.get_or_insert_with(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("effect view globals"),
                size: (gpu::GLOBALS * 4) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("effect view globals"),
                layout: &resources.globals_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            (buffer, group)
        });
        gpu.queue
            .write_buffer(globals, 0, gpu::bytes(&gpu::globals_values(view_proj, &Mat4::IDENTITY)));
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
                let buffer = gpu::vertex_buffer(device, floats.len());
                let size = (buffer.size() / 4) as usize;
                self.vertices = Some((buffer, size));
            }
            gpu.queue
                .write_buffer(&self.vertices.as_ref().expect("made above").0, 0, gpu::bytes(&floats));
        }
        let targets = self.targets.as_ref().expect("made before");
        let resolved = targets.resolved.create_view(&Default::default());
        let [r, g, b] = self.background;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("effect view"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("effect view"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa,
                    depth_slice: None,
                    resolve_target: Some(&resolved),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(r),
                            g: f64::from(g),
                            b: f64::from(b),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some((buffer, _)) = &self.vertices {
                pass.set_bind_group(0, &*globals_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                // the opaque ones first, then the blended ones in their order
                for blended in [false, true] {
                    for draw in self
                        .draws
                        .iter()
                        .filter(|d| (d.state.blend >= 2) == blended && !d.range.is_empty())
                    {
                        let Some(pipeline) = resources.pipelines.get(&draw.state) else {
                            continue;
                        };
                        let group = match draw.texture {
                            Texture::Effect(t) => effect.and_then(|all| all.get(t)).and_then(Option::as_ref),
                            Texture::Character(t) => character.and_then(|c| c.groups.get(t)).and_then(Option::as_ref),
                            Texture::Grid => self.grid.as_ref(),
                            Texture::White => None,
                        }
                        .unwrap_or(&resources.white);
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(1, group, &[]);
                        pass.draw(draw.range.clone(), 0..1);
                    }
                }
            }
        }
        gpu.queue.submit([encoder.finish()]);
    }
}

#[cfg(test)]
impl EffectView {
    /// The view drawn at `size`, read back as RGBA rows.
    pub fn snapshot(
        &mut self,
        gpu: &egui_wgpu::RenderState,
        resources: &Gpu,
        target: &Target,
        stage: &Stage,
        size: [u32; 2],
    ) -> Vec<u8> {
        self.targets(gpu, size, target);
        let (view_proj, view) = self.matrices(size);
        let vertices = self.build(resources, gpu, stage, None, &view);
        self.render(gpu, resources, &vertices, &view_proj, stage.textures.as_deref(), None);
        let targets = self.targets.as_ref().expect("made above");
        let row = size[0] * 4;
        let stride = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(stride * size[1]),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &targets.resolved,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(size[1]),
                },
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let mapped = buffer.slice(..).get_mapped_range().expect("mapped").to_vec();
        mapped
            .chunks(stride as usize)
            .flat_map(|line| line[..row as usize].to_vec())
            .collect()
    }
}

/// A model's bounding radius (the client culls by it), 3 yards without one.
pub fn radius_of(model: &Model) -> f32 {
    let r = model
        .boxes
        .get(24..28)
        .map_or(3.0, |b| f32::from_le_bytes(b.try_into().expect("four")));
    if r.is_finite() && r > 0.0 { r } else { 3.0 }
}

fn distance_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t).distance(p)
}

/// A dot facing the eye.
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

/// The ground's texture: grey, a line a yard (four cells a texture), its middle lines brighter.
fn grid_image() -> Image {
    let size = 128u32;
    let mut image = Image::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let line = |v: u32| v.is_multiple_of(32) || v % 32 == 31;
            let shade: u8 = if line(x) || line(y) { 120 } else { 62 };
            let at = ((y * size + x) * 4) as usize;
            image.rgba[at..at + 4].copy_from_slice(&[shade, shade, shade.saturating_add(4), 255]);
        }
    }
    image
}
