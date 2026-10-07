//! What the models share on the GPU: the layouts, the samplers, a white texture, and the pipelines
//! of the states of their materials, each made once, by the first job that needs it. A model's
//! vertices and indices go to buffers made filled; a texture to a texture filled by a copy the job
//! submits, never by `Queue::write_texture` (the lock of wgpu-core 30 found in step 9.2f).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uniwow_api::formats::{self, Texture, TextureFormat};
use uniwow_api::journal;
use uniwow_api::viewport::{Target, View};
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::lock;
use crate::pool::Pool;

/// A vertex of a model as the shader reads it: position, normal, its two sets of coordinates, and
/// the four bones that move it with their weights.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [[f32; 2]; 2],
    pub bones: [u8; 4],
    pub weights: [u8; 4],
}

impl Vertex {
    /// `vertex` of a model of `bones` bones: a bone past them weighs nothing.
    pub fn of(vertex: &formats::ModelVertex, bones: usize) -> Self {
        let mut weights = vertex.bone_weights;
        for (weight, bone) in weights.iter_mut().zip(vertex.bone_indices) {
            if usize::from(bone) >= bones {
                *weight = 0;
            }
        }
        Self {
            position: vertex.position,
            normal: vertex.normal,
            uv: vertex.uv,
            bones: vertex.bone_indices,
            weights,
        }
    }
}

/// The radius `model` moves in: that of its header, or of a sequence kept when larger.
pub fn moving_radius(model: &formats::Model) -> f32 {
    model
        .animation
        .sequences
        .iter()
        .filter(|sequence| sequence.kept)
        .fold(model.radius, |radius, sequence| radius.max(sequence.radius))
}

/// An instance as the shader reads it: the rows of its transform, then its alpha.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InstanceGpu {
    pub rows: [[f32; 4]; 3],
    pub extra: [f32; 4],
}

/// What a batch tells its shader: its colour at rest, its flags, the radius of its model, and its
/// pixel shader with where its two textures take their coordinates (`shaders`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BatchParams {
    pub colour: [f32; 4],
    pub flags: [f32; 4],
    pub model: [f32; 4],
    pub combine: [u32; 4],
}

// SAFETY: plain numbers laid out by `repr(C)` without padding, any bit pattern valid.
unsafe impl bytemuck::Zeroable for Vertex {}
unsafe impl bytemuck::Pod for Vertex {}
unsafe impl bytemuck::Zeroable for InstanceGpu {}
unsafe impl bytemuck::Pod for InstanceGpu {}
unsafe impl bytemuck::Zeroable for BatchParams {}
unsafe impl bytemuck::Pod for BatchParams {}

/// Floats of the shader's `Camera`.
pub const CAMERA: usize = 52;

/// The share of its alpha under which a pixel of an alpha-keyed batch is not drawn, in WotLK.
pub const ALPHA_KEY: f32 = 224.0 / 255.0;

/// The render flags of a material.
const UNLIT: u16 = 0x01;
const UNFOGGED: u16 = 0x02;
const TWO_SIDED: u16 = 0x04;
const NO_DEPTH_TEST: u16 = 0x08;
const NO_DEPTH_WRITE: u16 = 0x10;

/// The blending modes of 3.3.5a.
pub const OPAQUE: u16 = 0;
pub const ALPHA_KEYED: u16 = 1;
const ADD_WITHOUT_ALPHA: u16 = 3;
const ADD: u16 = 4;
const MOD: u16 = 5;
const MOD2X: u16 = 6;
const BLEND_ADD: u16 = 7;

/// The state of a pipeline a material needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub blending: u16,
    pub two_sided: bool,
    pub depth_test: bool,
    pub depth_write: bool,
}

impl State {
    /// The state of `material`: a blended one never writes the depth, as the client draws it; an
    /// unknown blending is opaque.
    pub fn of(material: &formats::Material) -> Self {
        let blending = if material.blending <= BLEND_ADD {
            material.blending
        } else {
            OPAQUE
        };
        Self {
            blending,
            two_sided: material.flags & TWO_SIDED != 0,
            depth_test: material.flags & NO_DEPTH_TEST == 0,
            depth_write: material.flags & NO_DEPTH_WRITE == 0 && blending <= ALPHA_KEYED,
        }
    }

    pub fn blended(&self) -> bool {
        self.blending > ALPHA_KEYED
    }

    /// The pipeline of this state drawing into `target`, by `shader` laid out by `layout`, its
    /// vertices read from `buffers`.
    pub fn pipeline(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        shader: &wgpu::ShaderModule,
        target: &Target,
        buffers: &[Option<wgpu::VertexBufferLayout>],
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("models"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers,
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: (!self.two_sided).then_some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: target.depth_format,
                depth_write_enabled: Some(self.depth_write),
                // Or equal, as the client tests the depth: the layers of a submesh lie on its first.
                depth_compare: Some(if self.depth_test {
                    or_equal(target.depth_compare)
                } else {
                    wgpu::CompareFunction::Always
                }),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: target.sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target.color_format,
                    blend: self.blend(),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
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
            ADD_WITHOUT_ALPHA => both(One, One),
            ADD => both(SrcAlpha, One),
            MOD => both(Dst, Zero),
            MOD2X => both(Dst, Src),
            BLEND_ADD => both(One, OneMinusSrcAlpha),
            _ => Some(wgpu::BlendState::ALPHA_BLENDING),
        }
    }
}

/// `compare`, letting an equal depth pass too.
fn or_equal(compare: wgpu::CompareFunction) -> wgpu::CompareFunction {
    match compare {
        wgpu::CompareFunction::Greater => wgpu::CompareFunction::GreaterEqual,
        wgpu::CompareFunction::Less => wgpu::CompareFunction::LessEqual,
        other => other,
    }
}

/// The flags a batch of `material` gives its shader: its alpha key, whether unlit and unfogged,
/// and the colour of its fog, black for the added ones, white for mod, grey for mod2x.
pub fn flags(material: &formats::Material) -> [f32; 4] {
    let state = State::of(material);
    let key = if state.blending == ALPHA_KEYED { ALPHA_KEY } else { 0.0 };
    let unlit = material.flags & UNLIT != 0 || matches!(state.blending, MOD | MOD2X);
    let fog = match state.blending {
        ADD_WITHOUT_ALPHA | ADD | BLEND_ADD => 1.0,
        MOD => 2.0,
        MOD2X => 3.0,
        _ => 0.0,
    };
    let unfogged = material.flags & UNFOGGED != 0;
    [key, f32::from(u8::from(unlit)), f32::from(u8::from(unfogged)), fog]
}

/// The camera of the shader: the view, its sun and its fog, how far an instance is drawn, and the
/// axes of the camera, across, up and back, which the environment is mapped by.
pub fn camera_values(view: &View, reach: f32) -> [f32; CAMERA] {
    let mut values = [0f32; CAMERA];
    values[..16].copy_from_slice(&view.view_proj.to_cols_array());
    values[16..19].copy_from_slice(&view.sun.direction);
    values[20..23].copy_from_slice(&view.sun.colour);
    values[24..27].copy_from_slice(&view.sun.ambient);
    values[28..31].copy_from_slice(&view.eye.to_array());
    values[31] = reach;
    values[32..35].copy_from_slice(&view.fog.colour);
    values[36..39].copy_from_slice(&[view.fog.start, view.fog.middle, view.fog.end]);
    for (at, row) in [40, 44, 48].into_iter().zip(0..3) {
        values[at..at + 3].copy_from_slice(&view.view.row(row).truncate().normalize_or_zero().to_array());
    }
    values
}

/// A texture of a model, on the GPU.
pub struct TextureGpu {
    pub view: wgpu::TextureView,
    pub bytes: u64,
}

pub struct Shared {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub camera_layout: wgpu::BindGroupLayout,
    pub batch_layout: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    target: Target,
    /// By how they wrap: across (1) and up and down (2).
    pub samplers: [wgpu::Sampler; 4],
    /// What a batch without a texture of its own draws with.
    pub white: wgpu::TextureView,
    pub block_compression: bool,
    pipelines: Mutex<HashMap<State, Arc<wgpu::RenderPipeline>>>,
    /// What the looks drawn by a few commands share; none on a device without what it needs, or
    /// when not asked for.
    pub pool: Option<Arc<Pool>>,
}

impl Shared {
    /// What the models share on the device of `gpu`, drawing into `target`; the pool with `slots`
    /// arrays of textures when they are given and the device offers what it needs.
    pub fn new(gpu: &egui_wgpu::RenderState, target: &Target, slots: Option<usize>) -> Self {
        let device = gpu.device.clone();
        let uniform = |visibility| wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("models camera"),
            entries: &[uniform(wgpu::ShaderStages::VERTEX_FRAGMENT)],
        });
        let batch_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("models batch"),
            entries: &[
                uniform(wgpu::ShaderStages::VERTEX_FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pool = slots
            .and_then(|slots| Pool::new(&gpu.adapter, &device, &gpu.queue, &camera_layout, target, slots))
            .map(Arc::new);
        // With the pool, the vertices posed by their bones; at rest without.
        let skin = pool.as_ref().map(|pool| &pool.skin_layout);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("models"),
            bind_group_layouts: &[Some(&camera_layout), Some(&batch_layout), skin],
            immediate_size: 0,
        });
        let posing = if skin.is_some() {
            include_str!("skin.wgsl")
        } else {
            include_str!("rest.wgsl")
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("models"),
            source: wgpu::ShaderSource::Wgsl(
                [include_str!("common.wgsl"), posing, include_str!("models.wgsl")]
                    .concat()
                    .into(),
            ),
        });
        let address = |wrap: bool| {
            if wrap {
                wgpu::AddressMode::Repeat
            } else {
                wgpu::AddressMode::ClampToEdge
            }
        };
        let samplers = std::array::from_fn(|wrap| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("models"),
                address_mode_u: address(wrap & 1 != 0),
                address_mode_v: address(wrap & 2 != 0),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                anisotropy_clamp: 8,
                ..Default::default()
            })
        });
        let white = Texture {
            width: 1,
            height: 1,
            format: TextureFormat::Rgba8,
            levels: vec![vec![255; 4]],
        };
        let white = upload(&device, &gpu.queue, &white)
            .expect("a texture of one texel is taken")
            .view;
        Self {
            block_compression: device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            pool,
            device,
            queue: gpu.queue.clone(),
            camera_layout,
            batch_layout,
            layout,
            shader,
            target: *target,
            samplers,
            white,
            pipelines: Mutex::default(),
        }
    }

    /// The pipeline of `state`, made by the first job asking for it.
    pub fn pipeline(&self, state: State) -> Arc<wgpu::RenderPipeline> {
        if let Some(pipeline) = lock(&self.pipelines).get(&state) {
            return pipeline.clone();
        }
        let pipeline = Arc::new(self.make_pipeline(state));
        lock(&self.pipelines).entry(state).or_insert(pipeline).clone()
    }

    fn make_pipeline(&self, state: State) -> wgpu::RenderPipeline {
        state.pipeline(
            &self.device,
            &self.layout,
            &self.shader,
            &self.target,
            &[
                Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x2, 8 => Uint8x4, 9 => Unorm8x4
                    ],
                }),
                Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<InstanceGpu>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4
                    ],
                }),
            ],
        )
    }

    /// A buffer of `usage` made with `contents`.
    pub fn buffer(&self, label: &str, contents: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
        journal::uploaded(contents.len() as u64);
        self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage,
        })
    }

    pub fn texture(&self, texture: &Texture) -> Result<TextureGpu, String> {
        upload(&self.device, &self.queue, texture)
    }
}

/// `texture` on the GPU: its levels larger than the device takes left out, those of BC under 4
/// texels too; filled by a copy this thread submits.
fn upload(device: &wgpu::Device, queue: &wgpu::Queue, texture: &Texture) -> Result<TextureGpu, String> {
    // Read as they are, in gamma: the pixel shaders of WotLK combine them so (mod2x doubles them).
    let format = match texture.format {
        TextureFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
        TextureFormat::Bc1 => wgpu::TextureFormat::Bc1RgbaUnorm,
        TextureFormat::Bc2 => wgpu::TextureFormat::Bc2RgbaUnorm,
        TextureFormat::Bc3 => wgpu::TextureFormat::Bc3RgbaUnorm,
    };
    let largest = device.limits().max_texture_dimension_2d;
    let first = (0..texture.levels.len())
        .find(|level| (texture.width >> level).max(1) <= largest && (texture.height >> level).max(1) <= largest)
        .ok_or_else(|| format!("{}×{}: larger than the GPU takes", texture.width, texture.height))?;
    let block = format.is_compressed();
    let levels: Vec<usize> = (first..texture.levels.len())
        .take_while(|level| !block || ((texture.width >> level) >= 4 && (texture.height >> level) >= 4))
        .collect();
    if levels.is_empty() {
        return Err(format!("{}×{}: no level to draw", texture.width, texture.height));
    }
    let (width, height) = ((texture.width >> first).max(1), (texture.height >> first).max(1));
    let layout = |level: u32| {
        let (w, h) = ((width >> level).max(1), (height >> level).max(1));
        match format.block_copy_size(None) {
            Some(size) if block => (w.div_ceil(4) * size, h.div_ceil(4)),
            _ => (w * 4, h),
        }
    };
    // The rows of each level apart by a multiple of what a copy from a buffer needs.
    let mut bytes = Vec::new();
    let mut copies = Vec::with_capacity(levels.len());
    let mut size = 0u64;
    for (level, data) in levels.iter().map(|level| &texture.levels[*level]).enumerate() {
        let (row, rows) = layout(level as u32);
        size += u64::from(row * rows);
        let stride = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        copies.push((bytes.len() as u64, stride, rows));
        let start = bytes.len();
        bytes.resize(start + (stride * rows) as usize, 0);
        for (line, texels) in data.chunks(row as usize).take(rows as usize).enumerate() {
            let at = start + line * stride as usize;
            bytes[at..at + texels.len()].copy_from_slice(texels);
        }
    }
    let gpu_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("models texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    journal::uploaded(bytes.len() as u64);
    let source = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("models texture"),
        contents: &bytes,
        usage: wgpu::BufferUsages::COPY_SRC,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("models texture"),
    });
    for (level, (offset, stride, rows)) in copies.into_iter().enumerate() {
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &source,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(rows),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &gpu_texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: (width >> level).max(1),
                height: (height >> level).max(1),
                depth_or_array_layers: 1,
            },
        );
    }
    queue.submit([encoder.finish()]);
    Ok(TextureGpu {
        view: gpu_texture.create_view(&wgpu::TextureViewDescriptor::default()),
        bytes: size,
    })
}
