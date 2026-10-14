//! The layers of the sky of the module: the sun, the moon and the clouds, painted over the models of
//! the sky before the world (`Stage::Celestial` of the phase of the sky), and the glare of the sun or
//! of the moon, over
//! everything (`Stage::Glare`), as the client draws it last (`0x4f9213` in 12340), additive, where
//! the depth is that of the sky, its envelope following how much of its body the depth of the first
//! pass leaves in sight, as the client follows the share of its body an occlusion query finds
//! drawn (`0x7e5220`, 1.12.1). Their pipelines are built by a job, the images of the bodies read and
//! sent by another; the texels of the clouds are written by the interface thread as the module makes
//! them.

use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::formats::{FileRef, Formats, Texture, TextureFormat};
use uniwow_api::glam::{Mat3, Mat4, Vec3};
use uniwow_api::viewport::{Drawing, Layer, LayerStats, Phase, Pyramid, Stage, Target, View};
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, egui_wgpu, journal, uploads, wgpu};

use crate::clouds::SIDE;
use crate::heavens::{self, Body};
use crate::light;

/// Floats of the shader's `Heavens`.
pub const UNIFORM: usize = 80;

/// The images of the bodies and of their glare: the disc of the sun, its glare, the moon and its
/// glare (`Textures\SunCenter.blp`… as the client names them).
pub const IMAGES: [&str; 4] = [
    "Textures\\SunCenter.blp",
    "Textures\\SunGlare.blp",
    "Textures\\Moon.blp",
    "Textures\\MoonGlare.blp",
];

pub fn lock<T>(shared: &Mutex<T>) -> MutexGuard<'_, T> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// The glare of a body: the side of its quad and its alpha before its envelope, from the view
/// turned 45° away from its body to straight at it (`heavens::view_onto`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Glare {
    pub sides: [f32; 2],
    pub alphas: [f32; 2],
}

/// What the layers draw at a frame: the sun and the moon; how much of the sky they and the clouds
/// are drawn over; the glare of each, and what its envelope goes towards before the share of its
/// body in sight; the flash of lightning lighting the clouds, from 0 to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Heavens {
    pub sun: Body,
    pub moon: Body,
    pub share: f32,
    pub glares: [Glare; 2],
    pub flares: [f32; 2],
    pub flash: f32,
}

/// What the module hands its layers: the heavens of the frame, none without the light of a map
/// (nothing drawn, the glares fading); the texels of the clouds with their generation; the images
/// of the bodies with theirs.
#[derive(Default)]
pub struct Drawn {
    pub heavens: Option<Heavens>,
    pub clouds: Option<(u64, Arc<Vec<[u8; 4]>>)>,
    pub images: Option<(u64, Arc<Images>)>,
}

/// The images of the bodies, in the order of `IMAGES`, none for one that could not be read.
pub struct Images(pub [Option<wgpu::TextureView>; 4]);

/// The view of the frame as the shader's `Heavens`, with `heavens`, the seconds `seconds` since the
/// frame before: the matrices, the axes of the camera, the bodies, their glares by how much the
/// view looks onto them, the envelopes, and the colour of a flash of lightning with how far it turns
/// the clouds to it (`light::with_flash`, as it turns the sky); the direction a place on the screen is seen in from the
/// eye by the inverse of what `view_proj` does to a direction, its columns of x, y and w without its
/// translation, as the terrain finds it for its sky.
pub fn values(view: &View, heavens: &Heavens, seconds: f32) -> [f32; UNIFORM] {
    let mut values = [0f32; UNIFORM];
    values[..16].copy_from_slice(&view.view_proj.to_cols_array());
    let [x, y, z, _] = view.view_proj.to_cols_array_2d();
    let seen = |column: [f32; 4]| Vec3::new(column[0], column[1], column[3]);
    let sky_from_screen = Mat3::from_cols(seen(x), seen(y), seen(z)).inverse();
    values[16..32].copy_from_slice(&Mat4::from_mat3(sky_from_screen).to_cols_array());
    for (at, row) in [32, 36, 40].into_iter().zip(0..3) {
        values[at..at + 3].copy_from_slice(&view.view.row(row).truncate().normalize_or_zero().to_array());
    }
    values[35] = heavens.share;
    let forward = (-view.view.row(2).truncate()).normalize_or_zero().to_array();
    for (at, body) in [44, 52].into_iter().zip([&heavens.sun, &heavens.moon]) {
        values[at..at + 3].copy_from_slice(&body.towards);
        values[at + 3] = body.side;
        values[at + 4..at + 7].copy_from_slice(&body.colour);
        values[at + 7] = body.alpha;
    }
    for (at, (glare, body)) in [60, 64]
        .into_iter()
        .zip(heavens.glares.iter().zip([&heavens.sun, &heavens.moon]))
    {
        let onto = heavens::view_onto(forward, body.towards);
        values[at] = glare.sides[0] + onto * (glare.sides[1] - glare.sides[0]);
        values[at + 1] = (glare.alphas[0] + onto * (glare.alphas[1] - glare.alphas[0])) * body.alpha;
    }
    values[68..70].copy_from_slice(&heavens.flares);
    values[70..72].copy_from_slice(&heavens::FLARE_RISE);
    values[72] = heavens::FLARE_FALL;
    values[73] = seconds;
    values[76..79].copy_from_slice(&light::FLASH_COLOUR);
    values[79] = light::FLASH_SKY * heavens.flash.clamp(0.0, 1.0);
    values
}

/// What the layers share on the device: its pipelines and their layouts, the sampler, an image of
/// nothing for a body without its own, the texture of the clouds and the envelopes of the glares.
pub struct Gpu {
    heavens_layout: wgpu::BindGroupLayout,
    glare_layout: wgpu::BindGroupLayout,
    seen_layout: wgpu::BindGroupLayout,
    heavens_pipeline: wgpu::RenderPipeline,
    glare_pipeline: wgpu::RenderPipeline,
    seen_pipeline: wgpu::ComputePipeline,
    sampler: wgpu::Sampler,
    nothing: wgpu::TextureView,
    clouds: wgpu::Texture,
    clouds_view: wgpu::TextureView,
    envelopes: wgpu::Buffer,
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

fn texture_entry(binding: u32, visibility: wgpu::ShaderStages, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_entry(binding: u32, visibility: wgpu::ShaderStages, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

const SAMPLER: wgpu::BindGroupLayoutEntry = wgpu::BindGroupLayoutEntry {
    binding: 4,
    visibility: wgpu::ShaderStages::FRAGMENT,
    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
    count: None,
};

/// A texture of RGBA texels in gamma, its levels `levels` from the largest, sent by a copy the call
/// submits, never written in place: what the jobs send passes the budget of the uploads first.
fn texture_of(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    size: [u32; 2],
    levels: &[Vec<u8>],
) -> wgpu::Texture {
    let [width, height] = size;
    let mut bytes = Vec::new();
    let mut copies = Vec::new();
    for (level, data) in levels.iter().enumerate() {
        let (columns, rows) = ((width >> level).max(1), (height >> level).max(1));
        let row = columns * 4;
        let stride = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        copies.push((bytes.len() as u64, stride, columns, rows));
        let start = bytes.len();
        bytes.resize(start + (stride * rows) as usize, 0);
        for (line, texels) in data.chunks(row as usize).take(rows as usize).enumerate() {
            let at = start + line * stride as usize;
            bytes[at..at + texels.len()].copy_from_slice(texels);
        }
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len().max(1) as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    uploads::pass(bytes.len() as u64);
    journal::uploaded(bytes.len() as u64);
    let source = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: &bytes,
        usage: wgpu::BufferUsages::COPY_SRC,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    for (level, (offset, stride, columns, rows)) in copies.into_iter().enumerate() {
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
                width: columns,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
    }
    queue.submit([encoder.finish()]);
    texture
}

/// The image `texture`, named `file`, on the device, its levels as RGBA texels in gamma, read raw.
pub fn texture_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    file: &str,
    texture: &Texture,
) -> Result<wgpu::TextureView, String> {
    if texture.format != TextureFormat::Rgba8 || texture.levels.is_empty() || texture.width == 0 || texture.height == 0
    {
        return Err(format!("{file}: not an image of RGBA texels"));
    }
    let size = [texture.width, texture.height];
    Ok(texture_of(device, queue, file, size, &texture.levels).create_view(&wgpu::TextureViewDescriptor::default()))
}

/// The image `file` of `formats` on the device.
pub fn image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    formats: &dyn Formats,
    file: &str,
) -> Result<wgpu::TextureView, String> {
    let texture = formats.texture_rgba(&FileRef::Path(file.to_owned()))?;
    texture_view(device, queue, file, &texture)
}

/// The images of the bodies of `formats` on the device, and why those that could not be read were
/// not.
pub fn images(device: &wgpu::Device, queue: &wgpu::Queue, formats: &dyn Formats) -> (Images, Vec<String>) {
    let mut unread = Vec::new();
    let views = IMAGES.map(|file| match image(device, queue, formats, file) {
        Ok(view) => Some(view),
        Err(why) => {
            unread.push(why);
            None
        }
    });
    (Images(views), unread)
}

impl Gpu {
    /// The pipelines of the sky for `target`, and what they share, on the device of `gpu`.
    pub fn new(gpu: &egui_wgpu::RenderState, target: &Target) -> Self {
        let device = &gpu.device;
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let heavens_layout = layout(
            "sky heavens",
            &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                texture_entry(1, fragment, true),
                texture_entry(2, fragment, true),
                texture_entry(3, fragment, true),
                SAMPLER,
            ],
        );
        let glare_layout = layout(
            "sky glare",
            &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                SAMPLER,
                storage_entry(5, fragment, true),
                texture_entry(7, fragment, true),
                texture_entry(8, fragment, true),
            ],
        );
        let seen_layout = layout(
            "sky seen",
            &[
                uniform_entry(0, wgpu::ShaderStages::COMPUTE),
                storage_entry(6, wgpu::ShaderStages::COMPUTE, false),
                texture_entry(9, wgpu::ShaderStages::COMPUTE, false),
            ],
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sky.wgsl").into()),
        });
        // A pipeline of the sky drawn by `entries` through `bind`, blended by `blend`, where the depth
        // passes `depth`, writing none.
        let render = |label,
                      bind: &wgpu::BindGroupLayout,
                      entries: (&str, &str),
                      blend: wgpu::BlendState,
                      depth: wgpu::CompareFunction| {
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(bind)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(entries.0),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: target.depth_format,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(depth),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: target.sample_count,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entries.1),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target.color_format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let heavens_pipeline = render(
            "sky heavens",
            &heavens_layout,
            ("vs_heavens", "fs_heavens"),
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            // Painted before the world, testing nothing.
            wgpu::CompareFunction::Always,
        );
        // Added to what is drawn, as the client adds its glare (`D3DBLEND_SRCALPHA`, `D3DBLEND_ONE`).
        let added = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let kept = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let glare_pipeline = render(
            "sky glare",
            &glare_layout,
            ("vs_glare", "fs_glare"),
            wgpu::BlendState {
                color: added,
                alpha: kept,
            },
            // Last, where the depth is still that of the clearing: over the sky only.
            wgpu::CompareFunction::Equal,
        );
        let seen_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky seen"),
            bind_group_layouts: &[Some(&seen_layout)],
            immediate_size: 0,
        });
        let seen_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("sky seen"),
            layout: Some(&seen_pipeline_layout),
            module: &shader,
            entry_point: Some("cs_seen"),
            compilation_options: Default::default(),
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sky"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let nothing = texture_of(device, &gpu.queue, "sky nothing", [1, 1], &[vec![0; 4]])
            .create_view(&wgpu::TextureViewDescriptor::default());
        let clouds = texture_of(
            device,
            &gpu.queue,
            "sky clouds",
            [SIDE as u32; 2],
            &[vec![0; SIDE * SIDE * 4]],
        );
        let clouds_view = clouds.create_view(&wgpu::TextureViewDescriptor::default());
        let envelopes = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sky envelopes"),
            contents: bytemuck::cast_slice(&[0f32; 2]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        Self {
            heavens_layout,
            glare_layout,
            seen_layout,
            heavens_pipeline,
            glare_pipeline,
            seen_pipeline,
            sampler,
            nothing,
            clouds,
            clouds_view,
            envelopes,
        }
    }

    /// The image `at` of `images`, or the image of nothing.
    fn view<'a>(&'a self, images: Option<&'a Images>, at: usize) -> &'a wgpu::TextureView {
        images.and_then(|images| images.0[at].as_ref()).unwrap_or(&self.nothing)
    }
}

/// The envelopes of the glares of `gpu` on the device of `render`, the sun's then the moon's, read
/// back.
#[cfg(test)]
pub fn envelopes(render: &egui_wgpu::RenderState, gpu: &Gpu) -> [f32; 2] {
    let read = render.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 8,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = render.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(&gpu.envelopes, 0, &read, 0, 8);
    render.queue.submit([encoder.finish()]);
    read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    render.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let bytes = read.slice(..).get_mapped_range().expect("mapped").to_vec();
    let values: &[f32] = bytemuck::cast_slice(&bytes);
    [values[0], values[1]]
}

fn uniform_buffer(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (UNIFORM * 4) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// The sun, the moon and the clouds, over the models of the sky.
pub struct HeavensLayer {
    drawn: Arc<Mutex<Drawn>>,
    incoming: Arc<Mutex<Option<Arc<Gpu>>>>,
    gpu: Option<Arc<Gpu>>,
    uniform: Option<wgpu::Buffer>,
    /// The bind group, by the generation of the images it binds.
    group: Option<(u64, wgpu::BindGroup)>,
    clouds: u64,
    drawing: bool,
    stats: LayerStats,
}

impl HeavensLayer {
    pub fn new(drawn: Arc<Mutex<Drawn>>, incoming: Arc<Mutex<Option<Arc<Gpu>>>>) -> Self {
        Self {
            drawn,
            incoming,
            gpu: None,
            uniform: None,
            group: None,
            clouds: 0,
            drawing: false,
            stats: LayerStats::default(),
        }
    }
}

impl Layer for HeavensLayer {
    fn prepare(&mut self, render: &egui_wgpu::RenderState, view: &View) {
        if self.gpu.is_none() {
            self.gpu = lock(&self.incoming).clone();
        }
        self.drawing = false;
        let Some(gpu) = self.gpu.clone() else {
            return;
        };
        let (heavens, clouds, images) = {
            let drawn = lock(&self.drawn);
            (drawn.heavens, drawn.clouds.clone(), drawn.images.clone())
        };
        let uniform = self
            .uniform
            .get_or_insert_with(|| uniform_buffer(&render.device, "sky heavens"));
        if let Some((generation, texels)) = clouds.filter(|(generation, _)| *generation != self.clouds) {
            // On the interface thread, whose frame draws what it writes.
            render.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &gpu.clouds,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(texels.as_slice()),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some((SIDE * 4) as u32),
                    rows_per_image: Some(SIDE as u32),
                },
                wgpu::Extent3d {
                    width: SIDE as u32,
                    height: SIDE as u32,
                    depth_or_array_layers: 1,
                },
            );
            self.clouds = generation;
        }
        let generation = images.as_ref().map_or(0, |(generation, _)| *generation);
        if self.group.as_ref().is_none_or(|(kept, _)| *kept != generation) {
            let images = images.as_ref().map(|(_, images)| images.as_ref());
            let group = render.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sky heavens"),
                layout: &gpu.heavens_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(gpu.view(images, 0)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(gpu.view(images, 2)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&gpu.clouds_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                    },
                ],
            });
            self.group = Some((generation, group));
        }
        let Some(heavens) = heavens.filter(|heavens| heavens.share > 0.0) else {
            self.stats = LayerStats::default();
            return;
        };
        render
            .queue
            .write_buffer(uniform, 0, bytemuck::cast_slice(&values(view, &heavens, 0.0)));
        self.drawing = true;
        self.stats = LayerStats {
            draws: 1,
            triangles: 1,
            bytes: (UNIFORM * 4 + SIDE * SIDE * 4) as u64,
            items: "the sun, the moon and the clouds".to_owned(),
            ..LayerStats::default()
        };
    }

    fn stage(&self) -> Stage {
        Stage::Celestial
    }

    fn drawing(&self) -> Drawing {
        Drawing::Pass
    }

    fn draw_pass(
        &mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        phase: Phase,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        if phase != Phase::Sky || !self.drawing {
            return;
        }
        let (Some(gpu), Some((_, group))) = (&self.gpu, &self.group) else {
            return;
        };
        pass.set_pipeline(&gpu.heavens_pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.draw(0..3, 0..1);
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}

/// The glare of the sun and of the moon, over everything, and how much of each body is in sight.
pub struct GlareLayer {
    drawn: Arc<Mutex<Drawn>>,
    incoming: Arc<Mutex<Option<Arc<Gpu>>>>,
    gpu: Option<Arc<Gpu>>,
    uniform: Option<wgpu::Buffer>,
    group: Option<(u64, wgpu::BindGroup)>,
    /// The bind group of the test of the sight, by the generation of the pyramid it reads.
    seen: Option<(u64, wgpu::BindGroup)>,
    /// The time of the view at the frame before.
    before: Option<f32>,
    stats: LayerStats,
}

impl GlareLayer {
    pub fn new(drawn: Arc<Mutex<Drawn>>, incoming: Arc<Mutex<Option<Arc<Gpu>>>>) -> Self {
        Self {
            drawn,
            incoming,
            gpu: None,
            uniform: None,
            group: None,
            seen: None,
            before: None,
            stats: LayerStats::default(),
        }
    }
}

/// The longest time an envelope moves by in a frame, in seconds.
const LONGEST_FRAME: f32 = 0.25;

impl Layer for GlareLayer {
    fn prepare(&mut self, render: &egui_wgpu::RenderState, view: &View) {
        if self.gpu.is_none() {
            self.gpu = lock(&self.incoming).clone();
        }
        let Some(gpu) = self.gpu.clone() else {
            return;
        };
        let (heavens, images) = {
            let drawn = lock(&self.drawn);
            (drawn.heavens, drawn.images.clone())
        };
        let seconds = self
            .before
            .map_or(0.0, |before| (view.time - before).clamp(0.0, LONGEST_FRAME));
        self.before = Some(view.time);
        // Without the light of a map, the glares fade where the bodies last were.
        let heavens = heavens.unwrap_or_default();
        let uniform = self
            .uniform
            .get_or_insert_with(|| uniform_buffer(&render.device, "sky glare"));
        render
            .queue
            .write_buffer(uniform, 0, bytemuck::cast_slice(&values(view, &heavens, seconds)));
        let generation = images.as_ref().map_or(0, |(generation, _)| *generation);
        if self.group.as_ref().is_none_or(|(kept, _)| *kept != generation) {
            let images = images.as_ref().map(|(_, images)| images.as_ref());
            let group = render.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sky glare"),
                layout: &gpu.glare_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: gpu.envelopes.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(gpu.view(images, 1)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: wgpu::BindingResource::TextureView(gpu.view(images, 3)),
                    },
                ],
            });
            self.group = Some((generation, group));
        }
        self.stats = LayerStats {
            draws: 1,
            triangles: 4,
            bytes: (UNIFORM * 4 + 8) as u64,
            items: "the glare of the sun and of the moon".to_owned(),
            ..LayerStats::default()
        };
    }

    fn occlude(
        &mut self,
        render: &egui_wgpu::RenderState,
        _view: &View,
        pyramid: &Pyramid<'_>,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let (Some(gpu), Some(uniform)) = (&self.gpu, &self.uniform) else {
            return;
        };
        if self
            .seen
            .as_ref()
            .is_none_or(|(generation, _)| *generation != pyramid.generation)
        {
            let group = render.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sky seen"),
                layout: &gpu.seen_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: gpu.envelopes.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 9,
                        resource: wgpu::BindingResource::TextureView(pyramid.view),
                    },
                ],
            });
            self.seen = Some((pyramid.generation, group));
        }
        let Some((_, group)) = &self.seen else {
            return;
        };
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("sky seen"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&gpu.seen_pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.dispatch_workgroups(2, 1, 1);
    }

    fn occludes(&self) -> bool {
        self.gpu.is_some() && self.uniform.is_some()
    }

    fn stage(&self) -> Stage {
        Stage::Glare
    }

    fn drawing(&self) -> Drawing {
        Drawing::Pass
    }

    fn draw_pass(
        &mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        phase: Phase,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        if phase != Phase::Near {
            return;
        }
        let (Some(gpu), Some((_, group))) = (&self.gpu, &self.group) else {
            return;
        };
        pass.set_pipeline(&gpu.glare_pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.draw(0..6, 0..2);
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
