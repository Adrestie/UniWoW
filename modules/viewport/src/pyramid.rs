//! The pyramid of the depth the first pass of a frame leaves (Hi-Z, `viewport::Pyramid`): its first
//! level the least of the samples of each pixel, each next level, half the one under it rounded down
//! as the levels of a texture are, the least of the texels under each of its own, built between the
//! two passes by a compute pass of a dispatch a level.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use uniwow_api::wgpu;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const WORKGROUP: u32 = 8;

/// The first level, from the depth of `samples` samples a pixel: the least of its samples.
fn first_source(samples: u32) -> String {
    let (depth, least) = if samples > 1 {
        (
            "texture_depth_multisampled_2d",
            "for (var sample = 0u; sample < textureNumSamples(depth); sample++) {
        least = min(least, textureLoad(depth, id.xy, sample));
    }",
        )
    } else {
        ("texture_depth_2d", "least = textureLoad(depth, id.xy, 0);")
    };
    format!(
        "@group(0) @binding(0) var depth: {depth};
@group(0) @binding(1) var level: texture_storage_2d<r32float, write>;

@compute @workgroup_size({WORKGROUP}, {WORKGROUP})
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    if any(id.xy >= textureDimensions(level)) {{
        return;
    }}
    var least = 1.0;
    {least}
    textureStore(level, id.xy, vec4<f32>(least, 0.0, 0.0, 1.0));
}}
"
    )
}

/// Each next level from the one under it: a texel covers the 2 × 2 under it, and the last of a row
/// or a column the rest of the level under it too, three where that side is odd, so that every
/// texel under it is covered.
fn reduce_source() -> String {
    format!(
        "@group(0) @binding(0) var under: texture_2d<f32>;
@group(0) @binding(1) var level: texture_storage_2d<r32float, write>;

@compute @workgroup_size({WORKGROUP}, {WORKGROUP})
fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
    let size = textureDimensions(level);
    if any(id.xy >= size) {{
        return;
    }}
    let start = id.xy * 2u;
    let end = select(start + vec2<u32>(1u), textureDimensions(under) - vec2<u32>(1u), id.xy == size - vec2<u32>(1u));
    var least = 1.0;
    for (var y = start.y; y <= end.y; y++) {{
        for (var x = start.x; x <= end.x; x++) {{
            least = min(least, textureLoad(under, vec2<u32>(x, y), 0).r);
        }}
    }}
    textureStore(level, id.xy, vec4<f32>(least, 0.0, 0.0, 1.0));
}}
"
    )
}

/// The pipelines building pyramids from a depth of `samples` samples a pixel, made once a device.
pub struct Builder {
    first_layout: wgpu::BindGroupLayout,
    reduce_layout: wgpu::BindGroupLayout,
    first: wgpu::ComputePipeline,
    reduce: wgpu::ComputePipeline,
    /// Counts the pyramids made.
    made: AtomicU64,
}

impl Builder {
    pub fn new(device: &wgpu::Device, samples: u32) -> Self {
        let storage = wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: FORMAT,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let layout = |label, sample_type, multisampled| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled,
                        },
                        count: None,
                    },
                    storage,
                ],
            })
        };
        let first_layout = layout("viewport pyramid first", wgpu::TextureSampleType::Depth, samples > 1);
        let reduce_layout = layout(
            "viewport pyramid reduce",
            wgpu::TextureSampleType::Float { filterable: false },
            false,
        );
        let pipeline = |layout: &wgpu::BindGroupLayout, label, source: String| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(label),
                    bind_group_layouts: &[Some(layout)],
                    immediate_size: 0,
                })),
                module: &module,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        Self {
            first: pipeline(&first_layout, "viewport pyramid first", first_source(samples)),
            reduce: pipeline(&reduce_layout, "viewport pyramid reduce", reduce_source()),
            first_layout,
            reduce_layout,
            made: AtomicU64::new(0),
        }
    }
}

/// The levels of a pyramid of `size`, as those of a texture: halved, rounded down, to one texel.
pub fn levels(size: [u32; 2]) -> u32 {
    u32::BITS - size[0].max(size[1]).max(1).leading_zeros()
}

/// The size of the level `level` of a pyramid of `size`.
pub fn level_size(size: [u32; 2], level: u32) -> [u32; 2] {
    size.map(|side| (side >> level).max(1))
}

/// A pyramid of the depth of a view of `size`, with what builds it from `depth`.
pub struct Pyramid {
    builder: Arc<Builder>,
    /// For the tests reading its levels back.
    #[cfg(test)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: [u32; 2],
    levels: u32,
    generation: u64,
    /// The bind group of each level: the depth or the level under it, and the level written.
    groups: Vec<wgpu::BindGroup>,
}

impl Pyramid {
    pub fn new(device: &wgpu::Device, builder: Arc<Builder>, depth: &wgpu::TextureView, size: [u32; 2]) -> Self {
        let levels = levels(size);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewport pyramid"),
            size: wgpu::Extent3d {
                width: size[0].max(1),
                height: size[1].max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let level = |level| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("viewport pyramid level"),
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let group = |layout, read: &wgpu::TextureView, written: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("viewport pyramid"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(read),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(written),
                    },
                ],
            })
        };
        let views: Vec<wgpu::TextureView> = (0..levels).map(level).collect();
        let groups = (0..levels as usize)
            .map(|at| match at {
                0 => group(&builder.first_layout, depth, &views[0]),
                _ => group(&builder.reduce_layout, &views[at - 1], &views[at]),
            })
            .collect();
        let generation = builder.made.fetch_add(1, Ordering::Relaxed) + 1;
        Self {
            builder,
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            #[cfg(test)]
            texture,
            size,
            levels,
            generation,
            groups,
        }
    }

    /// Records the building of every level into `encoder`, once the first pass is recorded.
    pub fn build(&self, encoder: &mut wgpu::CommandEncoder) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("viewport pyramid"),
            timestamp_writes: None,
        });
        for (level, group) in self.groups.iter().enumerate() {
            let [width, height] = level_size(self.size, level as u32);
            pass.set_pipeline(if level == 0 {
                &self.builder.first
            } else {
                &self.builder.reduce
            });
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(width.div_ceil(WORKGROUP), height.div_ceil(WORKGROUP), 1);
        }
    }

    /// As the layers are given it.
    pub fn given(&self) -> uniwow_api::viewport::Pyramid<'_> {
        uniwow_api::viewport::Pyramid {
            view: &self.view,
            size: self.size,
            levels: self.levels,
            generation: self.generation,
        }
    }

    /// The texture, for the tests reading its levels back.
    #[cfg(test)]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}
