//! The GPU side of the terrain: the layouts and the pipelines the tiles, the horizon and the sky
//! share, and the resources of a tile, created and uploaded by the job that loads it (T5), which
//! submits its uploads itself: wgpu starts a transfer only at a submission. A tile is drawn in one
//! draw: its textures are layers of the arrays of the terrain, bound once for all the tiles.

use std::ops::Range;
use std::sync::Arc;

use uniwow_api::formats::{Formats, Layer};
use uniwow_api::viewport::Target;
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, egui_wgpu, parallel_for, wgpu};

use crate::horizon::HorizonVertex;
use crate::mesh::{self, CHUNKS, LODS, SKIRT_DEPTH, TILE_VERTICES, VERTICES, Vertex};
use crate::model::{TileId, TileModel, chunk_bounds};
use crate::textures::{NONE, Placed, SLOTS, TextureArrays};

const TILES_SHADER: &str = concat!(include_str!("common.wgsl"), include_str!("terrain.wgsl"));
const HORIZON_SHADER: &str = concat!(include_str!("common.wgsl"), include_str!("horizon.wgsl"));

/// Floats of the shaders' `Camera`.
pub const CAMERA: usize = 28;

/// What the tiles, the horizon and the sky share: the device, the layouts and the pipelines, the
/// samplers, an empty array for the slots without one, and the arrays of textures.
pub struct Shared {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub camera_layout: wgpu::BindGroupLayout,
    tile_layout: wgpu::BindGroupLayout,
    arrays_layout: wgpu::BindGroupLayout,
    pub mask_layout: wgpu::BindGroupLayout,
    pub pipeline: wgpu::RenderPipeline,
    pub horizon_pipeline: wgpu::RenderPipeline,
    pub sky_pipeline: wgpu::RenderPipeline,
    blend_sampler: wgpu::Sampler,
    layer_sampler: wgpu::Sampler,
    empty_array: wgpu::TextureView,
    pub textures: TextureArrays,
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
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

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// How a pipeline of the terrain draws, beyond its shader: its entry points, the layouts of its
/// groups, its vertices, the faces it culls and how it tests and writes the depth.
struct Drawing<'a> {
    entries: (&'a str, &'a str),
    layouts: &'a [Option<&'a wgpu::BindGroupLayout>],
    buffers: &'a [Option<wgpu::VertexBufferLayout<'a>>],
    cull: Option<wgpu::Face>,
    depth_write: bool,
    depth_compare: wgpu::CompareFunction,
}

fn make_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    target: &Target,
    drawing: Drawing,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(drawing.entries.0),
        bind_group_layouts: drawing.layouts,
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(drawing.entries.0),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(drawing.entries.0),
            compilation_options: Default::default(),
            buffers: drawing.buffers,
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: drawing.cull,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: target.depth_format,
            depth_write_enabled: Some(drawing.depth_write),
            depth_compare: Some(drawing.depth_compare),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: target.sample_count,
            ..Default::default()
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(drawing.entries.1),
            compilation_options: Default::default(),
            targets: &[Some(target.color_format.into())],
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl Shared {
    /// What the terrain shares on the device of `gpu`, its pipelines for `target`.
    pub fn new(gpu: &egui_wgpu::RenderState, target: &Target) -> Self {
        let device = &gpu.device;
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let camera_layout = layout(
            "terrain camera",
            &[uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT)],
        );
        let tile_layout = layout(
            "terrain tile",
            &[
                texture_entry(0),
                sampler_entry(1),
                uniform_entry(2, wgpu::ShaderStages::FRAGMENT),
            ],
        );
        let arrays_entries: Vec<wgpu::BindGroupLayoutEntry> = (0..SLOTS as u32)
            .map(texture_entry)
            .chain([sampler_entry(SLOTS as u32)])
            .collect();
        let arrays_layout = layout("terrain textures", &arrays_entries);
        let mask_layout = layout("terrain horizon", &[uniform_entry(0, wgpu::ShaderStages::VERTEX)]);
        let tiles_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain"),
            source: wgpu::ShaderSource::Wgsl(TILES_SHADER.into()),
        });
        let horizon_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain horizon"),
            source: wgpu::ShaderSource::Wgsl(HORIZON_SHADER.into()),
        });
        let pipeline = make_pipeline(
            device,
            &tiles_shader,
            target,
            Drawing {
                entries: ("vs_main", "fs_main"),
                layouts: &[Some(&camera_layout), Some(&tile_layout), Some(&arrays_layout)],
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3, 1 => Snorm8x4, 2 => Unorm8x4, 3 => Float32x2, 4 => Uint32
                    ],
                })],
                cull: Some(wgpu::Face::Back),
                depth_write: true,
                depth_compare: target.depth_compare,
            },
        );
        let horizon_pipeline = make_pipeline(
            device,
            &horizon_shader,
            target,
            Drawing {
                entries: ("vs_horizon", "fs_horizon"),
                layouts: &[Some(&camera_layout), Some(&mask_layout)],
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<HorizonVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Snorm8x4, 2 => Uint32],
                })],
                cull: Some(wgpu::Face::Back),
                depth_write: true,
                depth_compare: target.depth_compare,
            },
        );
        // Behind everything: only where the depth is still at infinity, where it was cleared.
        let sky_pipeline = make_pipeline(
            device,
            &horizon_shader,
            target,
            Drawing {
                entries: ("vs_sky", "fs_sky"),
                layouts: &[Some(&camera_layout)],
                buffers: &[],
                cull: None,
                depth_write: false,
                depth_compare: wgpu::CompareFunction::Equal,
            },
        );
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
        let empty = device.create_texture_with_data(
            &gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("terrain empty array"),
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
            arrays_layout,
            mask_layout,
            pipeline,
            horizon_pipeline,
            sky_pipeline,
            blend_sampler: sampler(wgpu::AddressMode::ClampToEdge, wgpu::MipmapFilterMode::Nearest),
            layer_sampler: sampler(wgpu::AddressMode::Repeat, wgpu::MipmapFilterMode::Linear),
            empty_array: empty.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            }),
            textures: TextureArrays::new(device, &gpu.queue),
        }
    }

    /// The bind group of the arrays of textures `views`, by slot, the empty array where none.
    pub fn arrays_group(&self, views: &[Option<wgpu::TextureView>]) -> wgpu::BindGroup {
        let mut entries: Vec<wgpu::BindGroupEntry> = views
            .iter()
            .enumerate()
            .map(|(slot, view)| wgpu::BindGroupEntry {
                binding: slot as u32,
                resource: wgpu::BindingResource::TextureView(view.as_ref().unwrap_or(&self.empty_array)),
            })
            .collect();
        entries.push(wgpu::BindGroupEntry {
            binding: SLOTS as u32,
            resource: wgpu::BindingResource::Sampler(&self.layer_sampler),
        });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terrain textures"),
            layout: &self.arrays_layout,
            entries: &entries,
        })
    }
}

/// The resources of a tile on the GPU.
pub struct TileGpu {
    pub id: TileId,
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    /// The triangles of each level of detail in `indices`.
    pub lods: [Range<u32>; LODS],
    /// The texels of blending of its chunks, kept to write a chunk changed again by itself.
    pub blend: wgpu::Texture,
    pub tile_group: wgpu::BindGroup,
    /// The textures its chunks draw with, kept while it is.
    _textures: Vec<Option<Arc<Placed>>>,
    pub bounds: [[f32; 3]; 2],
    /// What its own buffers and textures take on the GPU.
    pub bytes: u64,
}

/// Writes the vertices, those of the skirts, and the texels of blending of the chunk `place` of
/// `model`, as built from the model alone: what loading does for each chunk, and what a chunk
/// changed needs.
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
    for (first, skirt) in mesh::skirt(model.id, place, chunk) {
        queue.write_buffer(
            vertices,
            (first * size_of::<Vertex>()) as u64,
            bytemuck::cast_slice(&skirt),
        );
    }
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

/// The codes of the textures of each chunk, as the shader reads them: four a chunk, `NONE` for a
/// layer it does not have or whose texture is left out.
pub fn layer_codes(model: &TileModel, textures: &[Option<Arc<Placed>>]) -> Vec<[u32; 4]> {
    let code = |layer: Option<&Layer>| {
        layer
            .and_then(|layer| textures.get(layer.texture as usize))
            .and_then(Option::as_ref)
            .map_or(NONE, |placed| placed.code())
    };
    model
        .tile
        .chunks
        .iter()
        .map(|chunk| std::array::from_fn(|layer| code(chunk.layers.get(layer))))
        .collect()
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
    let textures: Vec<Option<Arc<Placed>>> = tile
        .textures
        .iter()
        .map(|file| shared.textures.get(formats, file))
        .collect();
    if cancelled() {
        return Ok(None);
    }
    let device = &shared.device;
    let vertices = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("terrain vertices"),
        size: (TILE_VERTICES * size_of::<Vertex>()) as u64,
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
    let (indices, lods) = mesh::indices(tile);
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("terrain indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let codes = layer_codes(model, &textures);
    let layers = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("terrain layers"),
        contents: bytemuck::cast_slice(&codes),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
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
            wgpu::BindGroupEntry {
                binding: 2,
                resource: layers.as_entire_binding(),
            },
        ],
    });
    shared.queue.submit([]);
    let [mut low, high] = tile.chunks.iter().map(|chunk| chunk_bounds(model.id, chunk)).fold(
        [[f32::MAX; 3], [f32::MIN; 3]],
        |[low, high], [l, h]| {
            [
                [low[0].min(l[0]), low[1].min(l[1]), low[2].min(l[2])],
                [high[0].max(h[0]), high[1].max(h[1]), high[2].max(h[2])],
            ]
        },
    );
    // The skirts hang below.
    low[2] -= SKIRT_DEPTH;
    let bytes = vertices.size() + index_buffer.size() + layers.size() + (64 * 64 * 4 * CHUNKS) as u64;
    Ok(Some(TileGpu {
        id: model.id,
        vertices,
        indices: index_buffer,
        lods,
        blend,
        tile_group,
        _textures: textures,
        bounds: [low, high],
        bytes,
    }))
}
