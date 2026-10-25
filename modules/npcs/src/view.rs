//! The NPC view (step 18.2): a figure alone, drawn apart from the world as the Effect view draws an
//! effect, with its own camera turning about it: a neutral ground with its grid, the figure standing
//! and breathing (its Stand), its weapons on the points of its hands. One for the panel *NPC view*,
//! one for the window choosing a model.

use std::sync::Arc;

use uniwow_api::egui::{self, Sense, Vec2};
use uniwow_api::glam::{Mat4, Vec3, Vec4Swizzles};
use uniwow_api::vfx::frame::{self, Eye, Vertex};
use uniwow_api::vfx::sim::Simulation;
use uniwow_api::vfx::texgen::Image;
use uniwow_api::viewport::Target;
use uniwow_api::{egui_wgpu, wgpu};

use crate::figure::{self, Figure};
use crate::gpu::{self, Gpu, State};

/// What a draw takes its texture from: a texture of a part, the grid, white.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Texture {
    Part(usize, usize),
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

pub struct NpcView {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target: Vec3,
    pub background: [f32; 3],
    pub ground: bool,
    /// Its Stand played, else held at its start.
    pub playing: bool,
    started: f64,
    targets: Option<Targets>,
    globals: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    vertices: Option<(wgpu::Buffer, usize)>,
    grid: Option<wgpu::BindGroup>,
    draws: Vec<Draw>,
    /// The figure framed last: framed again once another is shown.
    framed: Option<usize>,
}

impl Default for NpcView {
    fn default() -> Self {
        Self {
            yaw: 0.9,
            pitch: 0.25,
            distance: 5.0,
            target: Vec3::new(0.0, 0.0, 1.0),
            background: [0.16, 0.17, 0.19],
            ground: true,
            playing: true,
            started: 0.0,
            targets: None,
            globals: None,
            vertices: None,
            grid: None,
            draws: Vec::new(),
            framed: None,
        }
    }
}

/// The height of a figure: its model's bounds (the client's box), its scale times, 2 yards without
/// one.
pub fn height_of(figure: &Figure) -> f32 {
    let height = figure.parts.first().map_or(2.0, |body| {
        let bound = |at: usize| {
            body.model
                .boxes
                .get(at..at + 4)
                .map_or(0.0, |b| f32::from_le_bytes(b.try_into().expect("four")))
        };
        // the box's lowest and highest Z: the 3rd and the 6th floats
        let (low, high) = (bound(8), bound(20));
        if (high - low).is_finite() && high > low {
            high - low
        } else {
            2.0
        }
    });
    (height * figure.scale).clamp(0.3, 60.0)
}

impl NpcView {
    /// The whole figure in view, aimed at its middle.
    pub fn frame(&mut self, height: f32) {
        self.distance = (height * 2.2).clamp(2.5, 90.0);
        self.target = Vec3::new(0.0, 0.0, height * 0.55);
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
            "npc view",
            target.color_format,
            target.sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&Default::default());
        let resolved = texture(
            "npc view resolved",
            target.color_format,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let depth = texture(
            "npc view depth",
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

    /// The triangles of the frame: the ground, the body at `seconds` of its Stand, its weapons on
    /// its hands.
    fn build(
        &mut self,
        resources: &Gpu,
        gpu: &egui_wgpu::RenderState,
        figure: Option<&Figure>,
        view: &Mat4,
        seconds: f64,
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
            let half = figure.map_or(6.0, |figure| (height_of(figure) * 2.0).max(6.0).ceil());
            let corner = |x: f32, y: f32| Vertex {
                pos: [x, y, 0.0],
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
        let Some(figure) = figure else {
            return vertices;
        };
        let Some(body) = figure.parts.first() else {
            return vertices;
        };
        let scale = Mat4::from_scale(Vec3::splat(figure.scale));
        let clock = figure::standing(&body.model, if self.playing { seconds } else { 0.0 });
        let pose = figure::pose(&body.model, &clock);
        for (index, part) in figure.parts.iter().enumerate() {
            // the body where it stands, a weapon on its point of the body
            let (placed, part_clock, part_pose) = match part.point {
                None => (scale, clock, pose.clone()),
                Some(point) => {
                    let Some(at) = figure::point(&body.model, &pose, point) else {
                        continue;
                    };
                    let own = figure::standing(&part.model, 0.0);
                    (scale * at, own, figure::pose(&part.model, &own))
                }
            };
            let inverse = placed.inverse();
            let local = Eye {
                position: inverse.transform_point3(eye.position),
                right: inverse.transform_vector3(eye.right).normalize_or(Vec3::X),
                up: inverse.transform_vector3(eye.up).normalize_or(Vec3::Z),
            };
            let still = Simulation::new(&part.model, 0);
            for mut batch in frame::build(&part.model, Some(&part.skin), &still, &part_clock, &part_pose, &local) {
                for v in &mut batch.vertices {
                    v.pos = placed.transform_point3(Vec3::from(v.pos)).to_array();
                }
                let texture = batch.texture.map_or(Texture::White, |t| Texture::Part(index, t));
                push(
                    &mut self.draws,
                    &mut vertices,
                    State::of(batch.blend, batch.material_flags),
                    texture,
                    &batch.vertices,
                );
            }
        }
        vertices
    }

    /// Draws the view into its texture and shows it in `ui`, `size` points large (the room left
    /// when none), turning the camera as the pointer drags and zooming as the wheel turns. The
    /// response of the image, for its clicks.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        (gpu, resources, target): (&egui_wgpu::RenderState, &Arc<Gpu>, &Target),
        figure: Option<&Figure>,
        size: Option<Vec2>,
    ) -> egui::Response {
        let available =
            size.unwrap_or_else(|| (ui.available_size() - Vec2::new(0.0, 22.0)).max(Vec2::new(160.0, 120.0)));
        let pixels = ui.ctx().pixels_per_point();
        let size = [
            (available.x * pixels).round().max(16.0) as u32,
            (available.y * pixels).round().max(16.0) as u32,
        ];
        let now = ui.input(|i| i.time);
        if self.started == 0.0 {
            self.started = now;
        }
        // a new figure framed whole
        let id = figure.map(|figure| figure as *const Figure as usize);
        if id != self.framed {
            self.framed = id;
            if let Some(figure) = figure {
                self.frame(height_of(figure));
            }
        }
        self.targets(gpu, size, target);
        let (view_proj, view) = self.matrices(size);
        let vertices = self.build(resources, gpu, figure, &view, now - self.started);
        self.render(gpu, resources, &vertices, &view_proj, figure);
        let targets = self.targets.as_ref().expect("made above");
        let response = ui.add(egui::Image::new((targets.texture_id, available)).sense(Sense::click_and_drag()));
        if response.dragged_by(egui::PointerButton::Primary) || response.dragged_by(egui::PointerButton::Secondary) {
            let delta = response.drag_delta();
            self.yaw -= delta.x * 0.01;
            self.pitch = (self.pitch + delta.y * 0.01).clamp(-1.5, 1.55);
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            let delta = response.drag_delta() * self.distance * 0.002;
            self.target += -view.row(0).xyz() * delta.x + view.row(1).xyz() * delta.y;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.distance = (self.distance * (1.0 - scroll * 0.002)).clamp(0.5, 200.0);
            }
            if ui.input(|i| i.key_pressed(egui::Key::F))
                && let Some(figure) = figure
            {
                self.frame(height_of(figure));
            }
        }
        ui.ctx().request_repaint();
        response
    }

    fn render(
        &mut self,
        gpu: &egui_wgpu::RenderState,
        resources: &Gpu,
        vertices: &[Vertex],
        view_proj: &Mat4,
        figure: Option<&Figure>,
    ) {
        let device = &gpu.device;
        let (globals, globals_group) = self.globals.get_or_insert_with(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("npc view globals"),
                size: (gpu::GLOBALS * 4) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("npc view globals"),
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
            label: Some("npc view"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("npc view"),
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
                            Texture::Part(part, t) => figure
                                .and_then(|figure| figure.parts.get(part))
                                .and_then(|part| part.groups.get(t))
                                .and_then(Option::as_ref),
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

/// The ground's texture: grey, a line a yard (four cells a texture).
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
