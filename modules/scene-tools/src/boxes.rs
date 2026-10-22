//! The bounding boxes of the entities selected, drawn by the GPU: one draw of instanced lines, a box
//! 24 bytes, its 12 edges made by the vertex shader, its buffer written once the selection or the
//! registry changes; tested against the depth, drawn over the blended last.

use uniwow_api::scene::{self, Snapshot};
use uniwow_api::viewport::{Layer, Phase, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

/// The boxes drawn at most: the first selected.
pub const MOST: usize = 10_000;

/// The colour of the selection, in gamma as the target stores it.
pub const COLOUR: [f32; 4] = [1.0, 0.56, 0.12, 1.0];

const SHADER: &str = r#"
struct Globals { view_proj: mat4x4<f32>, colour: vec4<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;

// The corners of each of the 12 edges of a box, by their bits: 1 the greatest X, 2 Y, 4 Z.
const ENDS = array<u32, 24>(0u, 1u, 2u, 3u, 4u, 5u, 6u, 7u, 0u, 2u, 1u, 3u, 4u, 6u, 5u, 7u, 0u, 4u, 1u, 5u, 2u, 6u, 3u, 7u);

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32, @location(0) low: vec3<f32>, @location(1) high: vec3<f32>) -> @builtin(position) vec4<f32> {
    let corner = ENDS[vertex];
    let at = vec3<f32>(
        select(low.x, high.x, (corner & 1u) != 0u),
        select(low.y, high.y, (corner & 2u) != 0u),
        select(low.z, high.z, (corner & 4u) != 0u),
    );
    return globals.view_proj * vec4<f32>(at, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return globals.colour;
}
"#;

/// The bounds of the entities `ids` the snapshot holds, the first `MOST`, each as six floats: of a
/// creature the world as it runs shows, where it runs, not at its spawn's place, moved to where
/// `now` says it stands at this moment; and whether one moves so.
pub fn boxes(
    snapshot: &Snapshot,
    ids: &[scene::EntityId],
    now: impl Fn(&scene::EntityId) -> Option<[f32; 3]>,
) -> (Vec<[f32; 6]>, bool) {
    let mut moving = false;
    let boxes = ids
        .iter()
        .filter_map(|id| Some((id, snapshot.live(id).or_else(|| snapshot.get(id))?)))
        .take(MOST)
        .map(|(id, entity)| {
            let [low, high] = entity.bounds();
            // Moved by where it stands now from where the piece had it.
            let shift = match now(id) {
                Some(at) => {
                    moving = true;
                    let origin = entity.origin();
                    [at[0] - origin[0], at[1] - origin[1], at[2] - origin[2]]
                }
                None => [0.0; 3],
            };
            [
                low[0] + shift[0],
                low[1] + shift[1],
                low[2] + shift[2],
                high[0] + shift[0],
                high[1] + shift[1],
                high[2] + shift[2],
            ]
        })
        .collect();
    (boxes, moving)
}

/// The boxes the gizmos stand about, of the entities `ids`: one written by its provider where its
/// provider has it (a creature at its spawn point, never where it runs now), one only seen as
/// `boxes` draws it.
pub fn written_boxes(
    snapshot: &Snapshot,
    ids: &[scene::EntityId],
    now: impl Fn(&scene::EntityId) -> Option<[f32; 3]>,
) -> Vec<[f32; 6]> {
    ids.iter()
        .take(MOST)
        .flat_map(|id| {
            match snapshot
                .editable(id)
                .filter(|entity| entity.flags() & scene::flags::LIVE == 0)
            {
                Some(entity) => {
                    let [low, high] = entity.bounds();
                    vec![[low[0], low[1], low[2], high[0], high[1], high[2]]]
                }
                None => boxes(snapshot, std::slice::from_ref(id), &now).0,
            }
        })
        .collect()
}

struct Gpu {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
}

pub struct BoxesLayer {
    scene: scene::Handle,
    target: Target,
    gpu: Option<Gpu>,
    /// The generations of the selection and of the registry the boxes were written for, how many,
    /// and the version of the bundle.
    seen: Option<(u64, u64)>,
    /// Whether a box drawn is of an entity that moves: made again at each frame.
    moving: bool,
    count: u32,
    version: u64,
}

impl BoxesLayer {
    pub fn new(scene: scene::Handle, target: Target) -> Self {
        Self {
            scene,
            target,
            gpu: None,
            seen: None,
            moving: false,
            count: 0,
            version: 0,
        }
    }

    fn build(device: &wgpu::Device, target: &Target) -> Gpu {
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("selection boxes globals"),
            size: 80,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("selection boxes"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("selection boxes"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("selection boxes"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("selection boxes"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("selection boxes"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 24,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: target.depth_format,
                depth_write_enabled: Some(false),
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
        Gpu {
            pipeline,
            globals,
            bind_group,
            instances: Self::instances(device, 64),
            capacity: 64,
        }
    }

    fn instances(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("selection boxes"),
            size: (capacity * 24) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }
}

impl Layer for BoxesLayer {
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        let target = self.target;
        let state = self.gpu.get_or_insert_with(|| Self::build(&gpu.device, &target));
        let mut globals = [0.0f32; 20];
        globals[..16].copy_from_slice(&view.view_proj.to_cols_array());
        globals[16..].copy_from_slice(&COLOUR);
        gpu.queue
            .write_buffer(&state.globals, 0, bytemuck::cast_slice(&globals));
        let (selection, snapshot) = (self.scene.selection(), self.scene.snapshot());
        let now = (selection.generation, snapshot.generation);
        // Again at each frame while one of them moves, as its model does.
        if self.seen == Some(now) && !self.moving {
            return;
        }
        self.seen = Some(now);
        let scene = self.scene.clone();
        let (boxes, moving) = boxes(&snapshot, &selection.ids, |id| scene.position_now(id));
        self.moving = moving;
        if boxes.len() > state.capacity {
            state.capacity = boxes.len().next_power_of_two();
            state.instances = Self::instances(&gpu.device, state.capacity);
        }
        if !boxes.is_empty() {
            gpu.queue
                .write_buffer(&state.instances, 0, bytemuck::cast_slice(&boxes));
        }
        self.count = boxes.len() as u32;
        self.version += 1;
    }

    fn version(&self) -> Option<u64> {
        Some(self.version)
    }

    fn draw<'a>(
        &'a mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        phase: Phase,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    ) {
        let Some(state) = &self.gpu else {
            return;
        };
        if phase != Phase::Near || self.count == 0 {
            return;
        }
        bundle.set_pipeline(&state.pipeline);
        bundle.set_bind_group(0, &state.bind_group, &[]);
        bundle.set_vertex_buffer(0, state.instances.slice(..));
        bundle.draw(0..24, 0..self.count);
    }
}

#[cfg(test)]
mod wgsl_tests {
    use super::*;

    #[test]
    fn its_shaders_parse_and_validate() {
        uniwow_api::viewport::check_wgsl(SHADER).unwrap_or_else(|error| panic!("selection boxes: {error}"));
    }
}
