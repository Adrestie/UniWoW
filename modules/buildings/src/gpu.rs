//! What the buildings share on the GPU, to be drawn by a few commands: the vertices of every
//! building in one arena and their indices in another, their materials in a table, their textures
//! in arrays of `core/api`; and the pipelines of the states of their materials. A file of a
//! building is put there by the job that reads it, and its ranges given back when it is dropped.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uniwow_api::arena::{Arena, Refusal};
use uniwow_api::formats::{Formats, Wmo, WmoMaterial};
use uniwow_api::texture_arrays::{NONE, Placed, TextureArrays};
use uniwow_api::viewport::{self, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::cells::Cells;
use crate::colours;

/// The arrays of textures the shader binds at once.
pub const SLOTS: usize = 64;
/// Floats of the shader's `Camera`.
pub const CAMERA: usize = 52;
/// The alpha under which an alpha-keyed batch is not drawn, as the models test it.
pub const ALPHA_KEY: f32 = 224.0 / 255.0;

/// The blendings of a material (`EGxBlend`).
const OPAQUE: u32 = 0;
const ALPHA_KEYED: u32 = 1;
const ALPHA: u32 = 2;
const ADD: u32 = 3;
const MOD: u32 = 4;
const MOD2X: u32 = 5;
/// The flags of a material.
const UNLIT: u32 = 0x1;
const UNFOGGED: u32 = 0x2;
const TWO_SIDED: u32 = 0x4;
const CLAMP_ACROSS: u32 = 0x40;
const CLAMP_UP: u32 = 0x80;
/// The flags of a group.
const HAS_COLOURS: u32 = 0x4;
/// The flag of a building lit as one (its "unified render path"): its vertex colours, unfixed and
/// nearly black, do not light its insides, every batch of it lit as outside, its ambient colour
/// added at the drawing; and the flag its groups give the shader then.
const LIT_AS_ONE: u16 = 0x2;
const GROUP_LIT_AS_ONE: u32 = 0x10;
/// The kind of a batch outside.
const OUTSIDE_BATCH: u32 = 2;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A vertex of a building: its position, normal, both sets of coordinates and of colours.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [i8; 4],
    pub uv: [[f32; 2]; 2],
    pub colours: [[u8; 4]; 2],
}

// SAFETY: plain numbers laid out by `repr(C)` without padding, any bit pattern valid.
unsafe impl bytemuck::Zeroable for Vertex {}
unsafe impl bytemuck::Pod for Vertex {}

/// A material as the shader reads it: the codes of its two textures (`Placed::code`, `NONE` for
/// white), how they wrap (the first in the two low bits, the second in the next two), its shader;
/// the sizes of the classes of its textures; its alpha key, whether unlit and unfogged, the colour
/// of its fog; its flags.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MaterialGpu {
    pub textures: [u32; 4],
    pub sizes: [f32; 4],
    pub shading: [f32; 4],
    pub flags: [u32; 4],
}

unsafe impl bytemuck::Zeroable for MaterialGpu {}
unsafe impl bytemuck::Pod for MaterialGpu {}

/// The state of a pipeline a material needs: its blending, unknown ones opaque, and whether it is
/// drawn from both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct State {
    pub blending: u32,
    pub two_sided: bool,
}

impl State {
    pub fn of(material: &WmoMaterial) -> Self {
        Self {
            blending: if material.blending <= MOD2X {
                material.blending
            } else {
                OPAQUE
            },
            two_sided: material.flags & TWO_SIDED != 0,
        }
    }

    /// Whether it is drawn in the blended phase, without writing the depth.
    pub fn blended(&self) -> bool {
        self.blending > ALPHA_KEYED
    }

    fn blend(&self) -> Option<wgpu::BlendState> {
        use wgpu::BlendFactor::*;
        let both = |src, dst| {
            let component = wgpu::BlendComponent {
                src_factor: src,
                dst_factor: dst,
                operation: wgpu::BlendOperation::Add,
            };
            Some(wgpu::BlendState {
                color: component,
                alpha: component,
            })
        };
        match self.blending {
            OPAQUE | ALPHA_KEYED => None,
            ALPHA => Some(wgpu::BlendState::ALPHA_BLENDING),
            ADD => both(SrcAlpha, One),
            MOD => both(Dst, Zero),
            MOD2X => both(Dst, Src),
            _ => None,
        }
    }
}

/// What a material gives its shader: its alpha key, whether unlit and unfogged, and the colour of
/// its fog, black for the added ones, white for mod, grey for mod2x, as the models fog them.
pub fn shading(material: &WmoMaterial) -> [f32; 4] {
    let state = State::of(material);
    let key = if state.blending == ALPHA_KEYED { ALPHA_KEY } else { 0.0 };
    let unlit = material.flags & UNLIT != 0 || matches!(state.blending, MOD | MOD2X);
    let fog = match state.blending {
        ADD => 1.0,
        MOD => 2.0,
        MOD2X => 3.0,
        _ => 0.0,
    };
    let unfogged = material.flags & UNFOGGED != 0;
    [key, f32::from(u8::from(unlit)), f32::from(u8::from(unfogged)), fog]
}

/// The camera of the shader: the view, its sun and its fog, and the axes of the camera, across, up
/// and back, which the environment is mapped by.
pub fn camera_values(view: &View) -> [f32; CAMERA] {
    let mut values = [0f32; CAMERA];
    values[..16].copy_from_slice(&view.view_proj.to_cols_array());
    values[16..19].copy_from_slice(&view.sun.direction);
    values[20..23].copy_from_slice(&view.sun.colour);
    values[24..27].copy_from_slice(&view.sun.ambient);
    values[28..31].copy_from_slice(&view.eye.to_array());
    values[32..35].copy_from_slice(&view.fog.colour);
    values[36..39].copy_from_slice(&[view.fog.start, view.fog.middle, view.fog.end]);
    values[39] = view.fog.rate;
    for (at, row) in [40, 44, 48].into_iter().zip(0..3) {
        values[at..at + 3].copy_from_slice(&view.view.row(row).truncate().normalize_or_zero().to_array());
    }
    values
}

/// The shader, after `buildings.wgsl`: its arrays read by slot.
fn shader(slots: usize) -> String {
    let mut source = [
        viewport::LINEAR_WGSL,
        viewport::FOG_WGSL,
        viewport::LIGHT_WGSL,
        include_str!("buildings.wgsl"),
    ]
    .concat();
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

/// What the buildings share on the device of the view.
pub struct Shared {
    pub device: wgpu::Device,
    pub vertices: Arena,
    pub indices: Arena,
    pub materials: Arena,
    pub arrays: TextureArrays,
    pub camera_layout: wgpu::BindGroupLayout,
    pub layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    target: Target,
    sampler: wgpu::Sampler,
    /// What an empty slot binds: an array of one white texel.
    empty: wgpu::TextureView,
    pipelines: Mutex<HashMap<State, Arc<wgpu::RenderPipeline>>>,
}

impl Shared {
    /// What the buildings share on the device of `gpu`, drawing into `target`; refused when the
    /// device does not offer the first instance of an indirect draw, storage buffers read by
    /// vertices, or as many sampled textures a stage as `SLOTS`.
    pub fn new(gpu: &egui_wgpu::RenderState, target: &Target) -> Result<Self, String> {
        let device = &gpu.device;
        let downlevel = gpu.adapter.get_downlevel_capabilities().flags;
        if !device.features().contains(wgpu::Features::INDIRECT_FIRST_INSTANCE)
            || !downlevel.contains(wgpu::DownlevelFlags::VERTEX_STORAGE | wgpu::DownlevelFlags::INDIRECT_EXECUTION)
            || (device.limits().max_sampled_textures_per_shader_stage as usize) < SLOTS
        {
            return Err(format!(
                "the device does not offer what the buildings are drawn with: the first instance of an \
                 indirect draw, storage buffers read by vertices and {SLOTS} textures a stage; there is no \
                 other way to draw them"
            ));
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
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("buildings camera"),
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
        entries.extend((0..SLOTS as u32).map(|slot| wgpu::BindGroupLayoutEntry {
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
            label: Some("buildings"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("buildings"),
            bind_group_layouts: &[Some(&camera_layout), Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("buildings"),
            source: wgpu::ShaderSource::Wgsl(shader(SLOTS).into()),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("buildings"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let empty = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("buildings empty"),
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
                "buildings vertices",
                wgpu::BufferUsages::VERTEX,
                size_of::<Vertex>() as u64,
                1 << 16,
            ),
            indices: Arena::new(
                device,
                queue,
                "buildings indices",
                wgpu::BufferUsages::INDEX,
                4,
                1 << 18,
            ),
            materials: Arena::new(
                device,
                queue,
                "buildings materials",
                wgpu::BufferUsages::STORAGE,
                size_of::<MaterialGpu>() as u64,
                1 << 10,
            ),
            arrays: TextureArrays::new(device, queue, "the buildings", SLOTS, false),
            device: device.clone(),
            camera_layout,
            layout,
            pipeline_layout,
            shader,
            target: *target,
            sampler,
            empty,
            pipelines: Mutex::default(),
        })
    }

    /// The pipeline of `state`, made by the first asking for it.
    pub fn pipeline(&self, state: State) -> Arc<wgpu::RenderPipeline> {
        if let Some(pipeline) = lock(&self.pipelines).get(&state) {
            return pipeline.clone();
        }
        let pipeline = Arc::new(self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("buildings"),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3, 1 => Snorm8x4, 2 => Float32x2, 3 => Float32x2, 4 => Unorm8x4, 5 => Unorm8x4
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: (!state.two_sided).then_some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: self.target.depth_format,
                depth_write_enabled: Some(!state.blended()),
                depth_compare: Some(self.target.depth_compare),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: self.target.sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.target.color_format,
                    blend: state.blend(),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        }));
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
            label: Some("buildings"),
            layout: &self.layout,
            entries: &bound,
        }))
    }

    /// What changes the bind group of a frame: the buffer of the materials and the arrays.
    pub fn generation(&self) -> (u64, u64) {
        (
            self.materials.buffer().map_or(0, |(_, generation)| generation),
            self.arrays.generation(),
        )
    }

    /// What the buildings take on the GPU: the buffers of their arenas and their arrays.
    /// The ranges given back by the arenas since they were made.
    pub fn given(&self) -> u64 {
        self.vertices.given() + self.indices.given() + self.materials.given()
    }

    /// The most bytes the arenas of the vertices and of the indices can have.
    pub fn most(&self) -> [u64; 2] {
        [self.vertices.most(), self.indices.most()]
    }

    pub fn bytes(&self) -> u64 {
        let arenas: u64 = [&self.vertices, &self.indices, &self.materials]
            .iter()
            .map(|arena| arena.bytes().0)
            .sum();
        arenas + self.arrays.bytes()
    }
}

/// A batch of a group on the GPU: its indices in the arena, its material in the table, its state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatchGpu {
    pub first: u32,
    pub count: u32,
    pub material: u32,
    pub state: State,
    /// Of a transition (0), inside (1) or outside (2), by its place among the batches of its group.
    pub kind: u32,
}

/// The kind of the batch `index` of a group of `counts` batches of transition, inside and outside.
pub fn kind(index: usize, counts: [u16; 3]) -> u32 {
    let [transition, inside, _] = counts.map(usize::from);
    if index < transition {
        0
    } else if index < transition + inside {
        1
    } else {
        OUTSIDE_BATCH
    }
}

/// A group on the GPU: its bounds, its flags for the shader, its batches.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupGpu {
    pub bounds: [[f32; 3]; 2],
    pub flags: u32,
    pub batches: Vec<BatchGpu>,
}

/// The CPU side of a file of a building: what its placements and their doodads need, and what
/// tells the groups seen from inside.
pub struct WmoGpu {
    shared: Arc<Shared>,
    vertices: Range<u64>,
    indices: Range<u64>,
    materials: Range<u64>,
    /// The textures of its materials, held while it is.
    _textures: Vec<Arc<Placed>>,
    /// Where its vertices begin in their arena.
    pub base_vertex: i32,
    pub groups: Vec<GroupGpu>,
    pub bounds: [[f32; 3]; 2],
    /// Its ambient colour, red, green and blue from 0 to 1.
    pub ambient: [f32; 3],
    /// What it takes on the GPU, its textures apart.
    pub bytes: u64,
    pub cells: Cells,
}

impl WmoGpu {
    /// What it takes in the arenas of the vertices and of the indices.
    pub fn arenas(&self) -> [u64; 2] {
        [
            (self.vertices.end - self.vertices.start) * size_of::<Vertex>() as u64,
            (self.indices.end - self.indices.start) * 4,
        ]
    }

    /// What it keeps on the CPU: its groups and their batches, and its cells.
    pub fn cpu(&self) -> u64 {
        (size_of::<Self>()
            + self
                .groups
                .iter()
                .map(|group| size_of::<GroupGpu>() + group.batches.len() * size_of::<BatchGpu>())
                .sum::<usize>()) as u64
            + self.cells.bytes()
    }
}

impl Drop for WmoGpu {
    fn drop(&mut self) {
        self.shared.vertices.give(self.vertices.clone());
        self.shared.indices.give(self.indices.clone());
        self.shared.materials.give(self.materials.clone());
    }
}

/// The vertices of `wmo`, its groups one after the other, with their colours fixed as the client
/// fixes them; and the indices of each group's triangles into them.
pub fn geometry(wmo: &mut Wmo) -> (Vec<Vertex>, Vec<u32>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    // Where each group's indices begin.
    let mut starts = Vec::with_capacity(wmo.groups.len());
    for group in &mut wmo.groups {
        colours::fix(group, wmo.flags, wmo.ambient);
        let base = vertices.len() as u32;
        starts.push(indices.len() as u32);
        let colour = |set: usize, index: usize| {
            group
                .colours
                .get(set)
                .and_then(|set| set.get(index))
                .copied()
                .unwrap_or([0, 0, 0, 255])
        };
        for (index, position) in group.vertices.iter().enumerate() {
            let normal = group.normals.get(index).copied().unwrap_or([0.0, 0.0, 1.0]);
            let uv = |set: usize| {
                group
                    .coordinates
                    .get(set)
                    .and_then(|set| set.get(index))
                    .copied()
                    .unwrap_or_default()
            };
            vertices.push(Vertex {
                position: *position,
                normal: [
                    (normal[0].clamp(-1.0, 1.0) * 127.0) as i8,
                    (normal[1].clamp(-1.0, 1.0) * 127.0) as i8,
                    (normal[2].clamp(-1.0, 1.0) * 127.0) as i8,
                    0,
                ],
                uv: [uv(0), uv(1)],
                colours: [colour(0, index), colour(1, index)],
            });
        }
        indices.extend(group.triangles.iter().map(|index| base + u32::from(*index)));
    }
    (vertices, indices, starts)
}

/// `wmo` put on the GPU of `shared`, its textures read through `formats`: its geometry in the
/// arenas, its materials in the table, its textures in the arrays.
pub fn upload(shared: &Arc<Shared>, formats: &dyn Formats, mut wmo: Wmo) -> Result<WmoGpu, Refusal> {
    let cells = Cells::new(&wmo);
    let (vertices, indices, starts) = geometry(&mut wmo);
    let mut textures = Vec::new();
    let materials: Vec<MaterialGpu> = wmo
        .materials
        .iter()
        .map(|material| {
            let mut codes = [NONE; 2];
            let mut sizes = [1.0f32; 4];
            for slot in 0..2 {
                let Some(file) = &material.textures[slot] else {
                    continue;
                };
                if let Some(placed) = shared.arrays.get(formats, file) {
                    codes[slot] = placed.code();
                    sizes[slot * 2] = placed.width as f32;
                    sizes[slot * 2 + 1] = placed.height as f32;
                    textures.push(placed);
                }
            }
            let mut wrap = 0;
            if material.flags & CLAMP_ACROSS == 0 {
                wrap |= 1;
            }
            if material.flags & CLAMP_UP == 0 {
                wrap |= 2;
            }
            MaterialGpu {
                textures: [codes[0], codes[1], wrap | wrap << 2, material.shader],
                sizes,
                shading: shading(material),
                flags: [material.flags, material.blending, 0, 0],
            }
        })
        .collect();
    let vertex_range = shared.vertices.put(bytemuck::cast_slice(&vertices))?;
    let index_range = match shared.indices.put(bytemuck::cast_slice(&indices)) {
        Ok(range) => range,
        Err(reason) => {
            shared.vertices.give(vertex_range);
            return Err(reason.into());
        }
    };
    let material_range = match shared.materials.put(bytemuck::cast_slice(&materials)) {
        Ok(range) => range,
        Err(reason) => {
            shared.vertices.give(vertex_range);
            shared.indices.give(index_range);
            return Err(reason.into());
        }
    };
    let groups = wmo
        .groups
        .iter()
        .zip(starts)
        .map(|(group, start)| GroupGpu {
            bounds: group.bounds,
            flags: group.flags & (HAS_COLOURS | colours::OUTSIDE)
                | if wmo.flags & LIT_AS_ONE != 0 {
                    GROUP_LIT_AS_ONE
                } else {
                    0
                },
            batches: group
                .batches
                .iter()
                .enumerate()
                .filter(|(_, batch)| batch.count > 0)
                .map(|(index, batch)| BatchGpu {
                    first: index_range.start as u32 + start + batch.first,
                    count: batch.count,
                    material: material_range.start as u32 + u32::from(batch.material),
                    state: State::of(&wmo.materials[usize::from(batch.material)]),
                    kind: if wmo.flags & LIT_AS_ONE != 0 {
                        OUTSIDE_BATCH
                    } else {
                        kind(index, group.batch_counts)
                    },
                })
                .collect(),
        })
        .collect();
    let [r, g, b, _] = wmo.ambient;
    Ok(WmoGpu {
        shared: shared.clone(),
        base_vertex: vertex_range.start as i32,
        bytes: (vertices.len() * size_of::<Vertex>() + indices.len() * 4 + materials.len() * size_of::<MaterialGpu>())
            as u64,
        vertices: vertex_range,
        indices: index_range,
        materials: material_range,
        _textures: textures,
        groups,
        bounds: wmo.bounds,
        ambient: [r, g, b].map(|value| f32::from(value) / 255.0),
        cells,
    })
}
