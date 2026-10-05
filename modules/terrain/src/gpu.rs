//! The GPU side of the terrain: the layouts and the pipeline every tile shares, the textures
//! shared between tiles, and the resources of a tile, created and uploaded by the job that loads
//! it (T5), which submits its uploads itself: wgpu starts a transfer only at a submission.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use uniwow_api::formats::{FileRef, Formats, Texture, TextureFormat};
use uniwow_api::viewport::Target;
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, egui_wgpu, log, parallel_for, wgpu};

use crate::mesh::{self, VERTICES, Vertex};
use crate::model::{TileId, TileModel, chunk_bounds};

const SHADER: &str = include_str!("terrain.wgsl");

/// The chunks of a tile.
pub const CHUNKS: usize = 256;

/// Floats of the shader's `Camera`.
pub const CAMERA: usize = 20;

/// A texture on the GPU, and what it takes there.
pub struct GpuTexture {
    pub view: wgpu::TextureView,
    pub bytes: u64,
}

/// A texture of the cache: loaded once, none when it cannot be read.
type Slot = Arc<OnceLock<Option<Arc<GpuTexture>>>>;

/// The textures on the GPU by the file they come from, shared between tiles: each loaded by the
/// first job asking for it, the others asking meanwhile waiting for it.
#[derive(Default)]
pub struct TextureCache {
    entries: Mutex<HashMap<FileRef, Slot>>,
}

impl TextureCache {
    /// The texture `file`; none when it cannot be read, which is said once in the log.
    pub fn get(&self, shared: &Shared, formats: &dyn Formats, file: &FileRef) -> Option<Arc<GpuTexture>> {
        let cell = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(file.clone())
            .or_default()
            .clone();
        cell.get_or_init(|| match load_texture(shared, formats, file) {
            Ok(texture) => Some(Arc::new(texture)),
            Err(reason) => {
                log::warn!("a texture of the terrain is left out: {reason}");
                None
            }
        })
        .clone()
    }

    /// Forgets the textures no tile holds any more.
    pub fn purge(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, cell| match cell.get() {
                Some(Some(texture)) => Arc::strong_count(texture) > 1,
                // A texture being loaded, or one that could not be: kept, not to be read again.
                _ => true,
            });
    }

    /// What the textures kept take on the GPU.
    pub fn bytes(&self) -> u64 {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter_map(|cell| cell.get().cloned().flatten())
            .map(|texture| texture.bytes)
            .sum()
    }
}

/// The levels of `texture` that can go to the GPU: those of BC at least 4 texels wide and high.
fn levels(texture: &Texture) -> usize {
    match texture.format {
        TextureFormat::Rgba8 => texture.levels.len(),
        _ => (0..texture.levels.len())
            .take_while(|level| (texture.width >> level) >= 4 && (texture.height >> level) >= 4)
            .count(),
    }
}

/// The texture `file`, as BC when the device takes it and the file stores it so, else as RGBA.
fn load_texture(shared: &Shared, formats: &dyn Formats, file: &FileRef) -> Result<GpuTexture, String> {
    let mut texture = if shared.block_compression {
        formats.texture(file)?
    } else {
        formats.texture_rgba(file)?
    };
    if texture.format != TextureFormat::Rgba8 && (texture.width % 4 != 0 || texture.height % 4 != 0) {
        texture = formats.texture_rgba(file)?;
    }
    let (format, block_bytes) = match texture.format {
        TextureFormat::Rgba8 => (wgpu::TextureFormat::Rgba8UnormSrgb, 0),
        TextureFormat::Bc1 => (wgpu::TextureFormat::Bc1RgbaUnormSrgb, 8),
        TextureFormat::Bc2 => (wgpu::TextureFormat::Bc2RgbaUnormSrgb, 16),
        TextureFormat::Bc3 => (wgpu::TextureFormat::Bc3RgbaUnormSrgb, 16),
    };
    let count = levels(&texture);
    let gpu = shared.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain texture"),
        size: wgpu::Extent3d {
            width: texture.width,
            height: texture.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: count as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut bytes = 0;
    for (level, data) in texture.levels.iter().take(count).enumerate() {
        let (width, height) = ((texture.width >> level).max(1), (texture.height >> level).max(1));
        let (row, rows) = if block_bytes == 0 {
            (width * 4, height)
        } else {
            (width.div_ceil(4) * block_bytes, height.div_ceil(4))
        };
        shared.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        bytes += u64::from(row * rows);
    }
    Ok(GpuTexture {
        view: gpu.create_view(&wgpu::TextureViewDescriptor::default()),
        bytes,
    })
}

/// What every tile shares: the device, the layouts and the pipeline, the samplers, a blank texture
/// for the layers a chunk does not have, the textures, and whether the device takes BC.
pub struct Shared {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub camera_layout: wgpu::BindGroupLayout,
    tile_layout: wgpu::BindGroupLayout,
    chunk_layout: wgpu::BindGroupLayout,
    pub pipeline: wgpu::RenderPipeline,
    blend_sampler: wgpu::Sampler,
    layer_sampler: wgpu::Sampler,
    blank: GpuTexture,
    pub block_compression: bool,
    pub textures: TextureCache,
}

fn texture_entry(binding: u32, dimension: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

impl Shared {
    /// What the tiles share on the device of `gpu`, their pipeline for `target`.
    pub fn new(gpu: &egui_wgpu::RenderState, target: &Target) -> Self {
        let device = &gpu.device;
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terrain camera"),
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
        let tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terrain tile"),
            entries: &[texture_entry(0, wgpu::TextureViewDimension::D2Array), sampler_entry(1)],
        });
        let chunk_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terrain chunk"),
            entries: &[
                texture_entry(0, wgpu::TextureViewDimension::D2),
                texture_entry(1, wgpu::TextureViewDimension::D2),
                texture_entry(2, wgpu::TextureViewDimension::D2),
                texture_entry(3, wgpu::TextureViewDimension::D2),
                sampler_entry(4),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terrain"),
            bind_group_layouts: &[Some(&camera_layout), Some(&tile_layout), Some(&chunk_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3, 1 => Snorm8x4, 2 => Unorm8x4, 3 => Float32x2, 4 => Uint32
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
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
        let sampler = |address: wgpu::AddressMode, mipmaps: wgpu::MipmapFilterMode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("terrain"),
                address_mode_u: address,
                address_mode_v: address,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: mipmaps,
                ..Default::default()
            })
        };
        let blank_texture = device.create_texture_with_data(
            &gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("terrain blank"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &[255, 255, 255, 255],
        );
        Self {
            device: device.clone(),
            queue: gpu.queue.clone(),
            camera_layout,
            tile_layout,
            chunk_layout,
            pipeline,
            blend_sampler: sampler(wgpu::AddressMode::ClampToEdge, wgpu::MipmapFilterMode::Nearest),
            layer_sampler: sampler(wgpu::AddressMode::Repeat, wgpu::MipmapFilterMode::Linear),
            blank: GpuTexture {
                view: blank_texture.create_view(&wgpu::TextureViewDescriptor::default()),
                bytes: 4,
            },
            block_compression: device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            textures: TextureCache::default(),
        }
    }
}

/// The resources of a tile on the GPU.
pub struct TileGpu {
    pub id: TileId,
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    /// The triangles of each chunk in `indices`, its vertices starting at `VERTICES` times its place.
    pub ranges: Vec<Range<u32>>,
    /// The texels of blending of its chunks, kept to write a chunk changed again by itself.
    pub blend: wgpu::Texture,
    pub tile_group: wgpu::BindGroup,
    pub chunk_groups: Vec<wgpu::BindGroup>,
    /// The textures its chunks draw with, kept while it is.
    _textures: Vec<Option<Arc<GpuTexture>>>,
    pub bounds: [[f32; 3]; 2],
    /// What its own buffers and textures take on the GPU.
    pub bytes: u64,
}

/// Writes the vertices and the texels of blending of the chunk `place` of `model`, as built from
/// the model alone: what loading does for each chunk, and what a chunk changed needs.
pub fn write_chunk(
    queue: &wgpu::Queue,
    vertices: &wgpu::Buffer,
    blend: &wgpu::Texture,
    model: &TileModel,
    place: usize,
) {
    let chunk = &model.tile.chunks[place];
    let data = mesh::vertices(model.id, place, chunk);
    queue.write_buffer(
        vertices,
        (place * VERTICES * size_of::<Vertex>()) as u64,
        bytemuck::cast_slice(&data),
    );
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: blend,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: place as u32,
            },
            aspect: wgpu::TextureAspect::All,
        },
        &mesh::blend(chunk),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(64 * 4),
            rows_per_image: Some(64),
        },
        wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
}

/// The resources of the tile of `model`, its chunks written over the threads of the pool, its
/// uploads submitted; none when `cancelled` says so once its textures are read.
pub fn build_tile(
    shared: &Shared,
    formats: &dyn Formats,
    model: &TileModel,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<TileGpu>, String> {
    let tile = &model.tile;
    if tile.chunks.len() != CHUNKS {
        return Err(format!("{} chunks, where a tile has {CHUNKS}", tile.chunks.len()));
    }
    let textures: Vec<Option<Arc<GpuTexture>>> = tile
        .textures
        .iter()
        .map(|file| shared.textures.get(shared, formats, file))
        .collect();
    if cancelled() {
        return Ok(None);
    }
    let device = &shared.device;
    let vertices = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("terrain vertices"),
        size: (CHUNKS * VERTICES * size_of::<Vertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let blend = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain blending"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: CHUNKS as u32,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    parallel_for(CHUNKS, 16, |range| {
        for place in range {
            write_chunk(&shared.queue, &vertices, &blend, model, place);
        }
    });
    let mut indices = Vec::new();
    let mut ranges = Vec::with_capacity(CHUNKS);
    for chunk in &tile.chunks {
        let start = indices.len() as u32;
        indices.extend(mesh::indices(chunk.holes));
        ranges.push(start..indices.len() as u32);
    }
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("terrain indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let blend_view = blend.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let tile_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("terrain tile"),
        layout: &shared.tile_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&blend_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&shared.blend_sampler),
            },
        ],
    });
    let view = |layer: Option<&uniwow_api::formats::Layer>| -> &wgpu::TextureView {
        layer
            .and_then(|layer| textures.get(layer.texture as usize))
            .and_then(Option::as_ref)
            .map_or(&shared.blank.view, |texture| &texture.view)
    };
    let chunk_groups = tile
        .chunks
        .iter()
        .map(|chunk| {
            let entries: Vec<wgpu::BindGroupEntry> = (0..4)
                .map(|layer| wgpu::BindGroupEntry {
                    binding: layer as u32,
                    resource: wgpu::BindingResource::TextureView(view(chunk.layers.get(layer))),
                })
                .chain([wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&shared.layer_sampler),
                }])
                .collect();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("terrain chunk"),
                layout: &shared.chunk_layout,
                entries: &entries,
            })
        })
        .collect();
    shared.queue.submit([]);
    let bounds = tile.chunks.iter().map(|chunk| chunk_bounds(model.id, chunk)).fold(
        [[f32::MAX; 3], [f32::MIN; 3]],
        |[low, high], [l, h]| {
            [
                [low[0].min(l[0]), low[1].min(l[1]), low[2].min(l[2])],
                [high[0].max(h[0]), high[1].max(h[1]), high[2].max(h[2])],
            ]
        },
    );
    let bytes = vertices.size() + index_buffer.size() + (64 * 64 * 4 * CHUNKS) as u64;
    Ok(Some(TileGpu {
        id: model.id,
        vertices,
        indices: index_buffer,
        ranges,
        blend,
        tile_group,
        chunk_groups,
        _textures: textures,
        bounds,
        bytes,
    }))
}
