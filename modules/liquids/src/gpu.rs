//! What the liquids share on the GPU: the vertices of every tile in one arena and their indices in
//! another, the textures of their types in arrays of `core/api`, a table of the types the shader
//! reads (the codes of their frames, their kind, their animation), and the two pipelines: water
//! blended without writing the depth, magma and slime opaque. A tile is put there by the job that
//! reads it, and its ranges given back when it is dropped. The frames of a type and their
//! animation as Noggit draws them, read for the facts only: a procedural water (its material 3)
//! takes the frames of `lake_a`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uniwow_api::arena::{Arena, Refusal};
use uniwow_api::formats::{FileRef, Formats, LiquidTypeRecord};
use uniwow_api::glam::Vec3;
use uniwow_api::texture_arrays::{Placed, TextureArrays};
use uniwow_api::viewport::{self, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::mesh::{self, Meshes, Vertex};

/// The arrays of textures the shader binds at once.
pub const SLOTS: usize = 16;
/// The types of liquid the table holds, and the frames of a type.
pub const TYPES: usize = 64;
pub const FRAMES: usize = 32;
/// Floats of the shader's `Camera`.
pub const CAMERA: usize = 28;
/// The material of the procedural water, and the frames it is drawn with.
const PROCEDURAL: u32 = 3;
const PROCEDURAL_FRAMES: &str = r"XTextures\river\lake_a.%d.blp";
/// The frames a texture of liquid has at most.
const MOST_FRAMES: usize = 30;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A type of liquid as the shader reads it: the codes of its frames (`Placed::code`); how many
/// frames, whether water, its material; the two numbers of its animation.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypeGpu {
    pub frames: [u32; FRAMES],
    pub info: [u32; 4],
    pub animation: [f32; 4],
}

unsafe impl bytemuck::Zeroable for TypeGpu {}
unsafe impl bytemuck::Pod for TypeGpu {}

/// The names of the frames of `record`'s texture, in their order, and its animation: of a
/// procedural water those of `lake_a`, turning without moving; otherwise its first texture, its
/// frames where it names them.
pub fn frames(record: &LiquidTypeRecord) -> (Vec<String>, [f32; 2]) {
    let (name, animation) = if record.material == PROCEDURAL {
        (PROCEDURAL_FRAMES.to_owned(), [1.0, 0.0])
    } else {
        (record.textures[0].clone(), record.animation)
    };
    let names = if name.is_empty() {
        Vec::new()
    } else if name.contains("%d") {
        (1..=MOST_FRAMES)
            .map(|frame| name.replace("%d", &frame.to_string()))
            .collect()
    } else {
        vec![name]
    };
    (names, animation)
}

/// The camera of the shader: the view, its eye and the time of the frame, its fog.
pub fn camera_values(view: &View) -> [f32; CAMERA] {
    let mut values = [0f32; CAMERA];
    values[..16].copy_from_slice(&view.view_proj.to_cols_array());
    values[16..19].copy_from_slice(&view.eye.to_array());
    values[19] = view.time;
    values[20..23].copy_from_slice(&view.fog.colour);
    values[24..27].copy_from_slice(&[view.fog.start, view.fog.middle, view.fog.end]);
    values[27] = view.fog.rate;
    values
}

/// The shader, after `liquids.wgsl`: its arrays read by slot.
fn shader(slots: usize) -> String {
    let mut source = [viewport::LINEAR_WGSL, viewport::FOG_WGSL, include_str!("liquids.wgsl")].concat();
    for slot in 0..slots {
        let _ = writeln!(
            source,
            "@group(1) @binding({}) var array{slot}: texture_2d_array<f32>;",
            2 + slot
        );
    }
    source.push_str(
        "\n// The texel of `layer` of the array of `slot` at `at`, its gradients given.\n\
         fn sampled(slot: u32, layer: i32, at: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec4<f32> {\n    switch slot {\n",
    );
    for slot in 0..slots {
        let _ = writeln!(
            source,
            "        case {slot}u: {{ return textureSampleGrad(array{slot}, liquid_sampler, at, layer, ddx, ddy); }}"
        );
    }
    source.push_str("        default: { return vec4<f32>(1.0); }\n    }\n}\n");
    source
}

/// The types put in the table: each by its id, its slot; their textures held while they are.
#[derive(Default)]
struct Types {
    slots: HashMap<u32, u32>,
    held: Vec<Arc<Placed>>,
}

/// What the liquids share on the device of the view.
pub struct Shared {
    pub device: wgpu::Device,
    queue: wgpu::Queue,
    pub vertices: Arena,
    pub indices: Arena,
    pub arrays: TextureArrays,
    types: Mutex<Types>,
    pub table: wgpu::Buffer,
    pub camera_layout: wgpu::BindGroupLayout,
    pub layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    empty: wgpu::TextureView,
    /// The pipelines of the water and of the magma and slime.
    pub water: wgpu::RenderPipeline,
    pub opaque: wgpu::RenderPipeline,
}

impl Shared {
    /// What the liquids share on the device of `gpu`, drawing into `target`; refused when the
    /// device does not offer as many sampled textures a stage as `SLOTS`.
    pub fn new(gpu: &egui_wgpu::RenderState, target: &Target) -> Result<Self, String> {
        let device = &gpu.device;
        if (device.limits().max_sampled_textures_per_shader_stage as usize) < SLOTS {
            return Err(format!(
                "the device does not offer {SLOTS} textures a stage, which the liquids are drawn with"
            ));
        }
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("liquids camera"),
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
        let mut entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        entries.extend((0..SLOTS as u32).map(|slot| wgpu::BindGroupLayoutEntry {
            binding: 2 + slot,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        }));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("liquids"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("liquids"),
            bind_group_layouts: &[Some(&camera_layout), Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("liquids"),
            source: wgpu::ShaderSource::Wgsl(shader(SLOTS).into()),
        });
        // Seen from over the surface and from under it.
        let pipeline = |blended: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(if blended { "liquids water" } else { "liquids opaque" }),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32, 3 => Uint32],
                    })],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: target.depth_format,
                    depth_write_enabled: Some(!blended),
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
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target.color_format,
                        blend: blended.then_some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let empty = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("liquids empty"),
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
            })
            .create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
        let queue = &gpu.queue;
        Ok(Self {
            vertices: Arena::new(
                device,
                queue,
                "liquids vertices",
                wgpu::BufferUsages::VERTEX,
                size_of::<Vertex>() as u64,
                1 << 14,
            ),
            indices: Arena::new(device, queue, "liquids indices", wgpu::BufferUsages::INDEX, 4, 1 << 16),
            arrays: TextureArrays::new(device, queue, "the liquids", SLOTS, false),
            types: Mutex::default(),
            table: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("liquids types"),
                size: (TYPES * size_of::<TypeGpu>()) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            camera_layout,
            layout,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("liquids"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                anisotropy_clamp: 8,
                ..Default::default()
            }),
            empty,
            water: pipeline(true),
            opaque: pipeline(false),
            device: device.clone(),
            queue: queue.clone(),
        })
    }

    /// The slot of the type `record` in the table, its frames put in the arrays by the first asking
    /// for it, through `formats`; none once the table is full.
    pub fn slot(&self, formats: &dyn Formats, record: &LiquidTypeRecord) -> Option<u32> {
        if let Some(slot) = lock(&self.types).slots.get(&record.id) {
            return Some(*slot);
        }
        let (names, animation) = frames(record);
        let placed: Vec<Arc<Placed>> = names
            .iter()
            .filter_map(|name| self.arrays.get(formats, &FileRef::Path(name.clone())))
            .take(FRAMES)
            .collect();
        let mut types = lock(&self.types);
        if let Some(slot) = types.slots.get(&record.id) {
            return Some(*slot);
        }
        let slot = types.slots.len() as u32;
        if slot as usize >= TYPES {
            return None;
        }
        let mut frames = [uniwow_api::texture_arrays::NONE; FRAMES];
        for (frame, placed) in placed.iter().enumerate() {
            frames[frame] = placed.code();
        }
        let gpu = TypeGpu {
            frames,
            info: [
                placed.len() as u32,
                u32::from(mesh::is_water(record.kind)),
                record.material,
                0,
            ],
            animation: [animation[0], animation[1], 0.0, 0.0],
        };
        self.queue.write_buffer(
            &self.table,
            u64::from(slot) * size_of::<TypeGpu>() as u64,
            bytemuck::bytes_of(&gpu),
        );
        types.slots.insert(record.id, slot);
        types.held.extend(placed);
        Some(slot)
    }

    /// The bind group of the frame: the table, the sampler and the arrays as last published.
    pub fn bind_group(&self) -> wgpu::BindGroup {
        let (_, views) = self.arrays.views();
        let mut bound = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: self.table.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            },
        ];
        bound.extend(views.iter().enumerate().map(|(slot, view)| wgpu::BindGroupEntry {
            binding: 2 + slot as u32,
            resource: wgpu::BindingResource::TextureView(view.as_ref().unwrap_or(&self.empty)),
        }));
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("liquids"),
            layout: &self.layout,
            entries: &bound,
        })
    }

    /// What the liquids take on the GPU: the buffers of their arenas, their arrays and their table.
    pub fn bytes(&self) -> u64 {
        self.vertices.bytes().0 + self.indices.bytes().0 + self.arrays.bytes() + self.table.size()
    }

    /// The ranges given back by the arenas since they were made.
    pub fn given(&self) -> u64 {
        self.vertices.given() + self.indices.given()
    }
}

/// The liquids of a tile on the GPU: its vertices, the indices of its water and those of its magma
/// and slime, each a range of the arena of the indices, and where its vertices begin; what it takes
/// in each arena, the vertices' then the indices'.
pub struct TileGpu {
    shared: Arc<Shared>,
    vertices: Range<u64>,
    indices: Range<u64>,
    /// The box of its vertices in the world, which the view tests before drawing it.
    pub bounds: [Vec3; 2],
    pub base_vertex: i32,
    pub water: Range<u32>,
    pub opaque: Range<u32>,
    pub bytes: u64,
    pub arenas: [u64; 2],
}

impl Drop for TileGpu {
    fn drop(&mut self) {
        self.shared.vertices.give(self.vertices.clone());
        self.shared.indices.give(self.indices.clone());
    }
}

/// `meshes` put on the GPU of `shared`; none when they hold nothing.
pub fn upload(shared: &Arc<Shared>, meshes: &Meshes) -> Result<Option<TileGpu>, Refusal> {
    if meshes.water.is_empty() && meshes.opaque.is_empty() {
        return Ok(None);
    }
    let vertices = shared.vertices.put(bytemuck::cast_slice(&meshes.vertices))?;
    let all: Vec<u32> = meshes.water.iter().chain(&meshes.opaque).copied().collect();
    let indices = match shared.indices.put(bytemuck::cast_slice(&all)) {
        Ok(range) => range,
        Err(reason) => {
            shared.vertices.give(vertices);
            return Err(reason.into());
        }
    };
    let first = indices.start as u32;
    let water = first..first + meshes.water.len() as u32;
    let bounds = meshes
        .vertices
        .iter()
        .fold([Vec3::INFINITY, Vec3::NEG_INFINITY], |[low, high], vertex| {
            let at = Vec3::from(vertex.position);
            [low.min(at), high.max(at)]
        });
    Ok(Some(TileGpu {
        shared: shared.clone(),
        bounds,
        base_vertex: vertices.start as i32,
        opaque: water.end..water.end + meshes.opaque.len() as u32,
        water,
        bytes: (meshes.vertices.len() * size_of::<Vertex>() + all.len() * 4) as u64,
        arenas: [
            (meshes.vertices.len() * size_of::<Vertex>()) as u64,
            all.len() as u64 * 4,
        ],
        vertices,
        indices,
    }))
}
