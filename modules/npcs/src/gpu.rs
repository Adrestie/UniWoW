//! What the NPC view keeps on the GPU, as the Effect view (module `effects`) does: a pipeline for
//! each blending of the M2 and each way of testing and writing the depth and culling; the textures
//! of a figure, each its group, filled by a copy this thread submits (never `Queue::write_texture`).

use std::collections::HashMap;

use uniwow_api::vfx::{blp, texgen::Image};
use uniwow_api::viewport::Target;
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, journal, uploads, wgpu};

/// Floats of the shader's `Globals`.
pub const GLOBALS: usize = 32;
/// Floats of a vertex: place, texture coordinates, colour.
pub const VERTEX: usize = 9;

/// How a batch is drawn: its blending (0 to 6), whether its back faces are cut, whether it is
/// tested against the depth, whether it writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub blend: u16,
    pub cull: bool,
    pub test: bool,
    pub write: bool,
}

impl State {
    /// The state of an M2 material: its blending and its flags (0x4 two-sided, 0x8 no depth test,
    /// 0x10 no depth write); the blended ones never write the depth.
    pub fn of(blend: u16, flags: u16) -> Self {
        let blend = blend.min(6);
        Self {
            blend,
            cull: flags & 0x4 == 0,
            test: flags & 0x8 == 0,
            write: blend < 2 && flags & 0x10 == 0,
        }
    }

    /// Every state a pipeline is made for.
    pub fn all() -> Vec<Self> {
        let mut out = Vec::new();
        for blend in 0..7 {
            for cull in [false, true] {
                for test in [false, true] {
                    for write in [false, true] {
                        if write && blend >= 2 {
                            continue;
                        }
                        out.push(Self {
                            blend,
                            cull,
                            test,
                            write,
                        });
                    }
                }
            }
        }
        out
    }
}

pub struct Gpu {
    pub pipelines: HashMap<State, wgpu::RenderPipeline>,
    pub globals_layout: wgpu::BindGroupLayout,
    pub texture_layout: wgpu::BindGroupLayout,
    /// The samplers: clamped, repeated across, repeated down, repeated both ways (the texture's
    /// flags 0x1 and 0x2).
    pub samplers: [wgpu::Sampler; 4],
    /// White, for what has no texture.
    pub white: wgpu::BindGroup,
}

fn blend_state(blend: u16) -> Option<wgpu::BlendState> {
    use wgpu::{BlendComponent, BlendFactor as F, BlendOperation, BlendState};
    let both = |src, dst| {
        let c = BlendComponent {
            src_factor: src,
            dst_factor: dst,
            operation: BlendOperation::Add,
        };
        BlendState { color: c, alpha: c }
    };
    match blend {
        0 | 1 => None,
        3 => Some(both(F::One, F::One)),
        4 => Some(both(F::SrcAlpha, F::One)),
        5 => Some(both(F::Dst, F::Zero)),
        6 => Some(both(F::Dst, F::Src)),
        _ => Some(BlendState::ALPHA_BLENDING),
    }
}

fn entry(blend: u16) -> &'static str {
    match blend {
        0 => "fs_opaque",
        1 => "fs_key",
        5 => "fs_mod",
        6 => "fs_mod2x",
        _ => "fs_blend",
    }
}

pub fn create(device: &wgpu::Device, queue: &wgpu::Queue, target: &Target) -> Gpu {
    let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("npcs globals"),
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
    let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("npcs texture"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let sampler = |u: bool, v: bool| {
        let mode = |repeat: bool| {
            if repeat {
                wgpu::AddressMode::Repeat
            } else {
                wgpu::AddressMode::ClampToEdge
            }
        };
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("npcs"),
            address_mode_u: mode(u),
            address_mode_v: mode(v),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        })
    };
    let samplers = [
        sampler(false, false),
        sampler(true, false),
        sampler(false, true),
        sampler(true, true),
    ];
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("npcs"),
        source: wgpu::ShaderSource::Wgsl(include_str!("npcs.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("npcs"),
        bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout)],
        immediate_size: 0,
    });
    let mut pipelines = HashMap::new();
    for state in State::all() {
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("npcs"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (VERTEX * 4) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4],
                })],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: state.cull.then_some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: target.depth_format,
                depth_write_enabled: Some(state.write),
                depth_compare: Some(if state.test {
                    target.depth_compare
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
                module: &shader,
                entry_point: Some(entry(state.blend)),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target.color_format,
                    blend: blend_state(state.blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        pipelines.insert(state, pipeline);
    }
    let white_image = Image {
        width: 1,
        height: 1,
        rgba: vec![255; 4],
    };
    let white = texture_group(device, queue, &texture_layout, &samplers, &white_image, 0);
    Gpu {
        pipelines,
        globals_layout,
        texture_layout,
        samplers,
        white,
    }
}

/// `image` on the GPU with its mipmaps (averaged by alpha, as its BLP), its group sampled as the
/// texture's flags say (0x1 repeat across, 0x2 down). Filled by a copy this thread submits.
pub fn texture_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    samplers: &[wgpu::Sampler; 4],
    image: &Image,
    flags: u32,
) -> wgpu::BindGroup {
    let levels = blp::mipmaps(image);
    let mut bytes = Vec::new();
    let mut copies = Vec::with_capacity(levels.len());
    for level in &levels {
        let row = level.width * 4;
        let stride = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        copies.push((bytes.len() as u64, stride, level.height, level.width));
        let start = bytes.len();
        bytes.resize(start + (stride * level.height) as usize, 0);
        for (line, texels) in level.rgba.chunks(row as usize).enumerate() {
            let at = start + line * stride as usize;
            bytes[at..at + texels.len()].copy_from_slice(texels);
        }
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("npcs texture"),
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    uploads::pass(bytes.len() as u64);
    journal::uploaded(bytes.len() as u64);
    let source = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("npcs texture"),
        contents: &bytes,
        usage: wgpu::BufferUsages::COPY_SRC,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("npcs texture"),
    });
    for (level, (offset, stride, rows, width)) in copies.into_iter().enumerate() {
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
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
    }
    queue.submit([encoder.finish()]);
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("npcs texture"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&samplers[(flags & 3) as usize]),
            },
        ],
    })
}

/// A vertex buffer of at least `floats` floats.
pub fn vertex_buffer(device: &wgpu::Device, floats: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("npcs vertices"),
        size: (floats.next_power_of_two().max(4096) * 4) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub fn globals_values(view_proj: &uniwow_api::glam::Mat4, model: &uniwow_api::glam::Mat4) -> [f32; GLOBALS] {
    let mut values = [0f32; GLOBALS];
    values[..16].copy_from_slice(&view_proj.to_cols_array());
    values[16..].copy_from_slice(&model.to_cols_array());
    values
}

pub fn bytes(values: &[f32]) -> &[u8] {
    bytemuck::cast_slice(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_shader_parses_and_validates() {
        uniwow_api::viewport::check_wgsl(include_str!("npcs.wgsl")).unwrap_or_else(|error| panic!("npcs: {error}"));
    }

    #[test]
    fn a_state_for_every_material() {
        let all = State::all();
        assert_eq!(all.len(), 2 * 2 * 2 * 2 + 5 * 2 * 2);
        for blend in 0..7 {
            for flags in 0..0x20 {
                assert!(all.contains(&State::of(blend, flags)), "{blend} {flags:#x}");
            }
        }
    }
}
