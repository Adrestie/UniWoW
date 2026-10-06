//! The entities in the 3D view: as their models where the module `models` draws them, as markers
//! otherwise, a coloured shape each by kind; and the names of the nearest written over the view. A
//! thread woken by the frame signal places them where they stand when the next frame is shown,
//! along their splines: it gives the instances of the models to `models` and writes the markers to
//! the GPU itself; the layer draws the markers in one draw, its bundle kept while their buffer
//! stays.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::models::{Extent, Instance as Placed, LookId, Models};
use uniwow_api::server_link::protocol::{DEAD, Entity, GAME_MASTER, Kind};
use uniwow_api::viewport::{Handle, Label, Layer, LayerStats, MAX_FRAME_WAIT, Phase, Target, View};
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::lock;
use crate::looks::{Display, Looks, Resolved};
use crate::world::World;

/// The owner of the instances of the live world in the service `models`.
pub const OWNER: &str = "live-world";

/// Half the height of a marker at the scale 1, in yards: about a person's.
pub const SIZE: f32 = 1.2;
/// The names written: the nearest to the eye, within this many yards.
pub const LABELS: usize = 48;
pub const LABEL_REACH: f32 = 120.0;
/// The fewest markers a buffer holds.
const CAPACITY: u32 = 256;

const SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) colour: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) corner: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) centre: vec3<f32>,
    @location(3) size: f32,
    @location(4) colour: vec4<f32>,
) -> Out {
    // Never smaller than a few pixels: a marker far away grows with its distance. A size of 0 is
    // no marker.
    let reach = select(0.0, max(size, distance(centre, camera.eye.xyz) * 0.006), size > 0.0);
    var out: Out;
    out.position = camera.view_proj * vec4<f32>(centre + corner * vec3<f32>(reach * 0.6, reach * 0.6, reach), 1.0);
    let light = normalize(vec3<f32>(0.4, 0.3, 0.85));
    out.colour = colour.rgb * (0.35 + 0.65 * max(dot(normal, light), 0.0));
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    return vec4<f32>(in.colour, 1.0);
}
"#;

/// A marker as the shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Instance {
    pub centre: [f32; 3],
    /// Half its height, in yards; 0 for none.
    pub size: f32,
    pub colour: [f32; 4],
}

// SAFETY: plain numbers laid out by `repr(C)` without padding, any bit pattern valid.
unsafe impl bytemuck::Zeroable for Instance {}
unsafe impl bytemuck::Pod for Instance {}

/// The colour of `entity`: by its kind, grey when dead.
pub fn colour(entity: &Entity) -> [f32; 4] {
    if entity.flags & DEAD != 0 {
        return [0.5, 0.5, 0.5, 1.0];
    }
    match entity.kind {
        Kind::Creature => [1.0, 0.55, 0.1, 1.0],
        Kind::GameObject => [0.35, 0.65, 1.0, 1.0],
        Kind::Player if entity.flags & GAME_MASTER != 0 => [0.75, 0.45, 1.0, 1.0],
        Kind::Player => [0.3, 0.95, 0.4, 1.0],
    }
}

/// The models the entities are drawn as: the looks of the displays read, and the service.
#[derive(Clone, Copy)]
pub struct Drawn<'a> {
    pub looks: &'a HashMap<Display, Resolved>,
    pub service: &'a dyn Models,
}

/// What a frame gives: the markers, the names, the instances of the models, and the displays not
/// read yet.
#[derive(Default)]
pub struct Frame {
    pub markers: Vec<Instance>,
    pub labels: Vec<Label>,
    pub placed: Vec<Placed>,
    pub wanted: Vec<Display>,
}

/// The entities of `world` at `when`, standing on their positions: as their models where `drawn`
/// draws them, as markers otherwise; and the names of the nearest to `eye`, over either.
pub fn build(world: &World, when: Instant, eye: Option<Vec3>, drawn: Option<Drawn>) -> Frame {
    let mut frame = Frame {
        markers: Vec::with_capacity(world.entities.len()),
        ..Frame::default()
    };
    let mut extents: HashMap<LookId, Option<Extent>> = HashMap::new();
    let mut named: Vec<(f32, Label)> = Vec::new();
    for tracked in world.entities.values() {
        let entity = &tracked.entity;
        let position = Vec3::from(tracked.position_at(when));
        let colour = colour(entity);
        // The top of its model, when it is seen as one: read, drawn, within its reach and drawing
        // something at rest. Its instance is placed all the same, which makes its look load.
        let model_top = drawn.filter(|_| entity.kind != Kind::Player).and_then(|drawn| {
            let display = (entity.kind, entity.display);
            let Some(Resolved::Look { look, scale, alpha }) = drawn.looks.get(&display) else {
                if !drawn.looks.contains_key(&display) {
                    frame.wanted.push(display);
                }
                return None;
            };
            let scale = scale * entity.scale;
            frame.placed.push(Placed {
                id: entity.guid,
                look: *look,
                transform: Mat4::from_scale_rotation_translation(
                    Vec3::splat(scale),
                    tracked.rotation_at(when),
                    position,
                ),
                alpha: *alpha,
                motion: tracked.motion_at(when),
            });
            let extent = *extents.entry(*look).or_insert_with(|| drawn.service.extent(*look));
            extent
                .filter(|extent| {
                    extent.batches > 0 && eye.is_none_or(|eye| eye.distance(position) <= extent.distance(scale))
                })
                .map(|extent| position.z + extent.high.z * scale)
        });
        let top = model_top.unwrap_or_else(|| {
            let size = SIZE * entity.scale.clamp(0.1, 10.0);
            frame.markers.push(Instance {
                centre: [position.x, position.y, position.z + size],
                size,
                colour,
            });
            position.z + 2.0 * size
        });
        if let Some(eye) = eye {
            let top = Vec3::new(position.x, position.y, top);
            let distance = top.distance(eye);
            if distance <= LABEL_REACH && !entity.name.is_empty() {
                named.push((
                    distance,
                    Label {
                        position: top,
                        text: entity.name.clone(),
                        colour: colour.map(|value| (value * 255.0) as u8),
                    },
                ));
            }
        }
    }
    named.sort_by(|a, b| a.0.total_cmp(&b.0));
    named.truncate(LABELS);
    frame.labels = named.into_iter().map(|(_, label)| label).collect();
    frame
}

/// What the thread placing the markers shares with the layer drawing them.
#[derive(Default)]
pub struct Drawing {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    buffer: Option<Arc<wgpu::Buffer>>,
    capacity: u32,
    /// Changed with the buffer, so that the layer records its bundle again.
    version: u64,
    count: u32,
    labels: Vec<Label>,
    /// The eye of the last frame drawn, which the names are chosen from.
    eye: Option<Vec3>,
    /// The instances given to `models` at the last frame, the time the thread took to place it,
    /// and the longest since the start of the last second counted.
    models: u32,
    spent: Duration,
    longest: Duration,
    since: Option<Instant>,
}

impl Drawing {
    /// Writes `instances` for the next frame; those drawn before and not now are given a size of 0.
    /// A buffer grown, when they no longer fit, is made with them, then put in place: no frame
    /// draws it empty. Only this thread writes the markers; the layer only reads them.
    pub fn write(&self, device: &wgpu::Device, queue: &wgpu::Queue, instances: &[Instance], labels: Vec<Label>) {
        let needed = instances.len() as u32;
        let (buffer, capacity, before) = {
            let state = lock(&self.state);
            (state.buffer.clone(), state.capacity, state.count)
        };
        let buffer = match buffer {
            Some(buffer) if needed <= capacity => buffer,
            _ => {
                let capacity = needed.next_power_of_two().max(CAPACITY);
                let mut data = instances.to_vec();
                data.resize(capacity as usize, Instance::default());
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("live-world markers"),
                    contents: bytemuck::cast_slice(&data),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                });
                let mut state = lock(&self.state);
                state.buffer = Some(Arc::new(buffer));
                state.capacity = capacity;
                state.version += 1;
                state.count = needed;
                state.labels = labels;
                return;
            }
        };
        // Out of the lock: the layer never waits for the queue of the GPU.
        let mut data = instances.to_vec();
        if (data.len() as u32) < before {
            data.resize(before as usize, Instance::default());
        }
        if !data.is_empty() {
            queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&data));
        }
        let mut state = lock(&self.state);
        state.count = needed;
        state.labels = labels;
    }

    pub fn eye(&self) -> Option<Vec3> {
        lock(&self.state).eye
    }

    /// Notes the instances given to `models` and the time placing the frame took.
    pub fn placed(&self, models: usize, spent: Duration) {
        let now = Instant::now();
        let mut state = lock(&self.state);
        state.models = models as u32;
        state.spent = spent;
        if state.since.is_none_or(|since| now - since > Duration::from_secs(1)) {
            state.since = Some(now);
            state.longest = spent;
        } else {
            state.longest = state.longest.max(spent);
        }
    }
}

/// The octahedron of a marker, from -1 to 1 across and up, each face with its own normal: position
/// then normal.
fn octahedron() -> Vec<[f32; 6]> {
    let mut vertices = Vec::with_capacity(24);
    for [sx, sy, sz] in [
        [1.0, 1.0, 1.0],
        [-1.0, 1.0, 1.0],
        [-1.0, -1.0, 1.0],
        [1.0, -1.0, 1.0],
        [1.0, 1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, -1.0, -1.0],
        [1.0, -1.0, -1.0],
    ] {
        let normal = Vec3::new(sx, sy, sz).normalize();
        for corner in [[sx, 0.0, 0.0], [0.0, sy, 0.0], [0.0, 0.0, sz]] {
            vertices.push([corner[0], corner[1], corner[2], normal.x, normal.y, normal.z]);
        }
    }
    vertices
}

struct Resources {
    pipeline: wgpu::RenderPipeline,
    mesh: wgpu::Buffer,
    camera: wgpu::Buffer,
    group: wgpu::BindGroup,
}

fn resources(device: &wgpu::Device, target: &Target) -> Resources {
    let mesh = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("live-world marker"),
        contents: bytemuck::cast_slice(&octahedron()),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let camera = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("live-world camera"),
        size: 80,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("live-world"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("live-world"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: camera.as_entire_binding(),
        }],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("live-world markers"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("live-world markers"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("live-world markers"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[
                Some(wgpu::VertexBufferLayout {
                    array_stride: 24,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
                }),
                Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![2 => Float32x3, 3 => Float32, 4 => Float32x4],
                }),
            ],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: target.depth_format,
            depth_write_enabled: Some(true),
            depth_compare: Some(target.depth_compare),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: target.sample_count,
            ..Default::default()
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(target.color_format.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    Resources {
        pipeline,
        mesh,
        camera,
        group,
    }
}

/// The layer drawing the markers.
pub struct Markers {
    drawing: Arc<Drawing>,
    resources: Option<Resources>,
    /// What the last bundle drew: the buffer and its capacity.
    drawn: Option<(Arc<wgpu::Buffer>, u32)>,
}

impl Markers {
    pub fn new(drawing: Arc<Drawing>) -> Self {
        Self {
            drawing,
            resources: None,
            drawn: None,
        }
    }

    /// The buffer of the markers, once written.
    #[cfg(test)]
    pub fn buffer(&self) -> Option<Arc<wgpu::Buffer>> {
        lock(&self.drawing.state).buffer.clone()
    }
}

impl Layer for Markers {
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        lock(&self.drawing.state).eye = Some(view.eye);
        if let Some(resources) = &self.resources {
            let mut camera = [0f32; 20];
            camera[..16].copy_from_slice(&view.view_proj.to_cols_array());
            camera[16..19].copy_from_slice(&view.eye.to_array());
            gpu.queue
                .write_buffer(&resources.camera, 0, bytemuck::cast_slice(&camera));
        }
    }

    fn version(&self) -> Option<u64> {
        // Recorded again once the resources exist, and with each new buffer.
        let state = lock(&self.drawing.state);
        Some(state.version * 2 + u64::from(self.resources.is_some()))
    }

    fn draw<'a>(
        &'a mut self,
        gpu: &egui_wgpu::RenderState,
        target: &Target,
        _view: &View,
        phase: Phase,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    ) {
        // Nothing blended.
        if phase != Phase::Opaque {
            return;
        }
        let resources = self.resources.get_or_insert_with(|| resources(&gpu.device, target));
        self.drawn = {
            let state = lock(&self.drawing.state);
            state.buffer.clone().map(|buffer| (buffer, state.capacity))
        };
        let Some((buffer, capacity)) = &self.drawn else {
            return;
        };
        bundle.set_pipeline(&resources.pipeline);
        bundle.set_bind_group(0, &resources.group, &[]);
        bundle.set_vertex_buffer(0, resources.mesh.slice(..));
        bundle.set_vertex_buffer(1, buffer.slice(..));
        bundle.draw(0..24, 0..*capacity);
    }

    fn stats(&self) -> LayerStats {
        let state = lock(&self.drawing.state);
        LayerStats {
            draws: u64::from(self.drawn.is_some()),
            triangles: 8 * u64::from(state.count),
            bytes: u64::from(state.capacity) * size_of::<Instance>() as u64,
            items: format!(
                "{} markers, {} models, {} names; placed in {:.2} ms, {:.2} at most",
                state.count,
                state.models,
                state.labels.len(),
                state.spent.as_secs_f64() * 1000.0,
                state.longest.as_secs_f64() * 1000.0
            ),
            steering: Duration::ZERO,
        }
    }

    fn labels(&self) -> Vec<Label> {
        lock(&self.drawing.state).labels.clone()
    }
}

/// Places the entities at each frame signal until `cancelled`, where they stand when the next frame
/// is shown, about a frame from now: their models given to `models` when it is there, as the owner
/// `OWNER`, the looks not read asked of `looks`; the markers written.
pub fn animate(
    viewport: &Handle,
    gpu: &egui_wgpu::RenderState,
    drawing: &Drawing,
    world: &dyn Fn() -> Arc<World>,
    models: Option<(&Looks, &dyn Models)>,
    cancelled: &dyn Fn() -> bool,
) {
    let mut last = 0;
    let mut signalled: Option<Instant> = None;
    let mut interval = Duration::from_micros(16_667);
    while !cancelled() {
        let Some(frame) = viewport.wait_frame(last, MAX_FRAME_WAIT) else {
            continue;
        };
        last = frame.number;
        let now = Instant::now();
        if let Some(before) = signalled.replace(now) {
            let between = now - before;
            if between < Duration::from_secs(1) {
                interval = (interval * 7 + between) / 8;
            }
        }
        let frame = {
            let looks = models.map(|(looks, _)| looks.resolved());
            let drawn = models
                .zip(looks.as_deref())
                .map(|((_, service), looks)| Drawn { looks, service });
            build(&world(), now + interval, drawing.eye(), drawn)
        };
        if let Some((looks, service)) = models {
            looks.want(frame.wanted);
            service.place(OWNER, &frame.placed);
        }
        drawing.write(&gpu.device, &gpu.queue, &frame.markers, frame.labels);
        drawing.placed(frame.placed.len(), now.elapsed());
    }
}
