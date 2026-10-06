//! What the models share on the GPU to be drawn by a few commands: the vertices of every model in
//! one arena and their indices in another, the materials of every look in a table, their textures
//! in arrays of `core/api` by class, read as stored; and the pipelines of the states of their
//! materials, whose shader reads, for each instance drawn, its entry (the instance among those of
//! the frame, and its material), its instance and its material from storage buffers, and where its
//! bones begin among those the thread of the animations wrote, posing its vertices. Made only on a
//! device that offers what it needs (`Pool::new`): the path of step 9.4c draws the models otherwise.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use uniwow_api::texture_arrays::TextureArrays;
use uniwow_api::viewport::Target;
use uniwow_api::{bytemuck, wgpu};

use crate::arena::Arena;
use crate::gpu::{State, Vertex};
use crate::lock;

/// The arrays of textures the shader binds at once.
pub const SLOTS: usize = 64;

/// A material as the shader reads it: what a batch tells it (`gpu::BatchParams`), then its two
/// textures, their codes, how each wraps, and the sizes of their classes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MaterialGpu {
    pub colour: [f32; 4],
    pub flags: [f32; 4],
    pub model: [f32; 4],
    pub combine: [u32; 4],
    /// The codes of the first and the second texture (`Placed::code`, `NONE` for white), and how
    /// they wrap, the first in the two low bits, the second in the next two.
    pub textures: [u32; 4],
    pub sizes: [f32; 4],
}

// SAFETY: plain numbers laid out by `repr(C)` without padding, any bit pattern valid.
unsafe impl bytemuck::Zeroable for MaterialGpu {}
unsafe impl bytemuck::Pod for MaterialGpu {}

/// The bytes of an instance and of an entry as the shader reads them.
pub const INSTANCE: u64 = 64;
pub const ENTRY: u64 = 8;

/// The shader of the pool, after `common.wgsl` and `skin.wgsl`: its bindings, its arrays read by
/// slot, its entry points.
fn shader(slots: usize) -> String {
    let mut source = String::from(include_str!("common.wgsl"));
    source.push_str(include_str!("skin.wgsl"));
    source.push_str(include_str!("pool.wgsl"));
    for slot in 0..slots {
        let _ = writeln!(
            source,
            "@group(1) @binding({}) var array{slot}: texture_2d_array<f32>;",
            4 + slot
        );
    }
    source.push_str(
        "\n// The texel of `layer` of the array of `slot` at `at`, its gradients given.\n\
         fn sampled(slot: u32, layer: i32, at: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec4<f32> {\n    switch slot {\n",
    );
    for slot in 0..slots {
        let _ = writeln!(
            source,
            "        case {slot}u: {{ return textureSampleGrad(array{slot}, layer_sampler, at, layer, ddx, ddy); }}"
        );
    }
    source.push_str("        default: { return vec4<f32>(1.0); }\n    }\n}\n");
    source
}

/// The bind group of `layout` binding `table` and the bones of `bones` from its offset.
fn skin_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    table: &wgpu::Buffer,
    (bones, offset): (&wgpu::Buffer, u64),
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("models bones"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: table.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: bones,
                    offset,
                    size: None,
                }),
            },
        ],
    })
}

pub struct Pool {
    pub device: wgpu::Device,
    pub vertices: Arena,
    pub indices: Arena,
    pub materials: Arena,
    pub arrays: TextureArrays,
    pub layout: wgpu::BindGroupLayout,
    /// The bones of a frame: where those of each instance begin, and the bones; the looks of their
    /// own read them too.
    pub skin_layout: wgpu::BindGroupLayout,
    /// What the bones bind before the thread of the animations writes any.
    no_bones: wgpu::Buffer,
    /// Every instance at rest: a table of zeros, what a look of its own binds before the frame
    /// has its table.
    pub rest: wgpu::BindGroup,
    pipeline_layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    target: Target,
    sampler: wgpu::Sampler,
    /// What an empty slot binds: an array of one white texel.
    empty: wgpu::TextureView,
    /// Whether the GPU counts the draws it packs (`MULTI_DRAW_INDIRECT_COUNT`, not on Direct3D 12).
    pub count: bool,
    pub(crate) pipelines: Mutex<HashMap<State, Arc<wgpu::RenderPipeline>>>,
}

impl Pool {
    /// The pool of `device`, its pipelines laid out after `camera`, its arrays `slots`; none when
    /// the device does not offer the first instance of an indirect draw, compute shaders, storage
    /// buffers read by vertices, eight a stage, or as many sampled textures a stage as `slots`.
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &wgpu::BindGroupLayout,
        target: &Target,
        slots: usize,
    ) -> Option<Self> {
        let downlevel = adapter.get_downlevel_capabilities().flags;
        let offered = device.features().contains(wgpu::Features::INDIRECT_FIRST_INSTANCE)
            && downlevel.contains(
                wgpu::DownlevelFlags::VERTEX_STORAGE
                    | wgpu::DownlevelFlags::COMPUTE_SHADERS
                    | wgpu::DownlevelFlags::INDIRECT_EXECUTION,
            )
            && device.limits().max_sampled_textures_per_shader_stage as usize >= slots
            && device.limits().max_storage_buffers_per_shader_stage >= 8;
        if !offered {
            return None;
        }
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let mut entries = vec![
            storage(0),
            storage(1),
            storage(2),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        entries.extend((0..slots as u32).map(|slot| wgpu::BindGroupLayoutEntry {
            binding: 4 + slot,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        }));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("models pool"),
            entries: &entries,
        });
        let skin_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("models bones"),
            entries: &[storage(0), storage(1)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("models pool"),
            bind_group_layouts: &[Some(camera), Some(&layout), Some(&skin_layout)],
            immediate_size: 0,
        });
        let zeros = |label, size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            })
        };
        let no_bones = zeros("models no bones", 48);
        let rest = skin_group(device, &skin_layout, &zeros("models at rest", 4), (&no_bones, 0));
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("models pool"),
            source: wgpu::ShaderSource::Wgsl(shader(slots).into()),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("models pool"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let empty = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("models pool empty"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let empty = empty.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        Some(Self {
            vertices: Arena::new(
                device,
                queue,
                "models vertices",
                wgpu::BufferUsages::VERTEX,
                size_of::<Vertex>() as u64,
                1 << 16,
            ),
            indices: Arena::new(device, queue, "models indices", wgpu::BufferUsages::INDEX, 4, 1 << 18),
            materials: Arena::new(
                device,
                queue,
                "models materials",
                wgpu::BufferUsages::STORAGE,
                size_of::<MaterialGpu>() as u64,
                1 << 12,
            ),
            arrays: TextureArrays::new(device, queue, "the models", slots, false),
            device: device.clone(),
            layout,
            skin_layout,
            no_bones,
            rest,
            pipeline_layout,
            shader,
            target: *target,
            sampler,
            empty,
            // Direct3D 12 in wgpu 30 does not give a draw counted by the GPU its first instance:
            // its command signature for `draw_indexed_indirect_count` leaves out the special constants
            // the other draws are given.
            count: device.features().contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT)
                && adapter.get_info().backend != wgpu::Backend::Dx12,
            pipelines: Mutex::default(),
        })
    }

    /// The pipeline of `state`, made by the first asking for it.
    pub fn pipeline(&self, state: State) -> Arc<wgpu::RenderPipeline> {
        if let Some(pipeline) = lock(&self.pipelines).get(&state) {
            return pipeline.clone();
        }
        let pipeline = Arc::new(state.pipeline(
            &self.device,
            &self.pipeline_layout,
            &self.shader,
            &self.target,
            &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x2, 4 => Uint8x4, 5 => Unorm8x4
                ],
            })],
        ));
        lock(&self.pipelines).entry(state).or_insert(pipeline).clone()
    }

    /// The bind group of a frame: its instances and entries, the materials, the sampler and the
    /// arrays as last published; none before the first material.
    pub fn bind_group(&self, instances: &wgpu::Buffer, entries: &wgpu::Buffer) -> Option<wgpu::BindGroup> {
        let (materials, _) = self.materials.buffer()?;
        let (_, views) = self.arrays.views();
        let mut bound = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: instances.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: entries.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: materials.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            },
        ];
        bound.extend(views.iter().enumerate().map(|(slot, view)| wgpu::BindGroupEntry {
            binding: 4 + slot as u32,
            resource: wgpu::BindingResource::TextureView(view.as_ref().unwrap_or(&self.empty)),
        }));
        Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("models pool"),
            layout: &self.layout,
            entries: &bound,
        }))
    }

    /// The bind group of the bones of a frame: `table`, where those of each instance begin, and the
    /// bones of `bones` from its offset; none written yet, every instance at rest.
    pub fn skin_group(&self, table: &wgpu::Buffer, bones: Option<(&wgpu::Buffer, u64)>) -> wgpu::BindGroup {
        skin_group(
            &self.device,
            &self.skin_layout,
            table,
            bones.unwrap_or((&self.no_bones, 0)),
        )
    }

    /// What changes the bind group of a frame: the buffer of the materials and the arrays.
    pub fn generation(&self) -> (u64, u64) {
        (
            self.materials.buffer().map_or(0, |(_, generation)| generation),
            self.arrays.generation(),
        )
    }

    /// The bytes of its arenas: of their buffers, and of the ranges they hold.
    pub fn arenas(&self) -> (u64, u64) {
        [&self.vertices, &self.indices, &self.materials]
            .iter()
            .map(|arena| arena.bytes())
            .fold((0, 0), |(held, used), (more, taken)| (held + more, used + taken))
    }
}
