//! What falls around the eye: rain, snow or sand, as many particles as the intensity of the weather
//! asks, drawn in one instanced draw whose every particle the vertex shader places and moves from
//! its number and the time (`fall.wgsl`); the CPU writes one uniform a frame, whatever their count.
//! Drawn blended on the eye's side of the water, after it; nothing while the eye is under the water
//! or in a room of a building, nor under the surface of the water under the eye.

use std::sync::{Arc, Mutex};

use uniwow_api::viewport::{self, Drawing, Layer, LayerStats, Phase, Target, View};
use uniwow_api::weather::{Effect, Kind};
use uniwow_api::{buildings, bytemuck, egui_wgpu, liquids, wgpu};

use crate::lock;

/// How a weather falls: the colour of a particle in gamma and its alpha, its width and length in
/// yards, whether it is a streak along its fall or a flake facing the eye, how far it sways, the
/// wind and the speed of its fall in yards a second, and how many fall at its full intensity. The
/// editor's choices, to compare with the game: no public description of the particles of the
/// client was found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fall {
    pub colour: [f32; 4],
    pub size: [f32; 2],
    pub streak: bool,
    pub sway: f32,
    pub wind: [f32; 2],
    pub speed: f32,
    pub count: u32,
}

/// What falls of the weather `kind`; nothing for the fog.
pub fn fall(kind: Kind) -> Option<Fall> {
    let rain = Fall {
        colour: [0.72, 0.76, 0.84, 0.35],
        size: [0.025, 0.7],
        streak: true,
        sway: 0.0,
        wind: [1.2, 0.6],
        speed: 18.0,
        count: 8_000,
    };
    let snow = Fall {
        colour: [0.95, 0.95, 1.0, 0.85],
        size: [0.08, 0.08],
        streak: false,
        sway: 0.4,
        wind: [0.6, 0.3],
        speed: 2.0,
        count: 6_000,
    };
    Some(match kind {
        Kind::Fog => return None,
        Kind::Rain => rain,
        Kind::Thunders => Fall {
            speed: 22.0,
            count: 10_000,
            ..rain
        },
        Kind::BlackRain => Fall {
            colour: [0.08, 0.08, 0.1, 0.5],
            ..rain
        },
        Kind::Snow => snow,
        Kind::BlackSnow => Fall {
            colour: [0.12, 0.1, 0.1, 0.85],
            ..snow
        },
        Kind::Sandstorm => Fall {
            colour: [0.78, 0.62, 0.42, 0.45],
            size: [0.05, 0.05],
            streak: false,
            sway: 1.0,
            wind: [9.0, 4.0],
            speed: 0.5,
            count: 6_000,
        },
    })
}

/// Half the side of the box of the particles around the eye, and its height, in yards.
pub const HALF_SIDE: f32 = 25.0;
pub const HEIGHT: f32 = 30.0;
/// The time is taken within this many seconds, so that it stays precise however long the view
/// runs; the particles jump once each time it starts again.
const PERIOD: f64 = 600.0;
/// Floats of the shader's `Camera`.
pub const CAMERA: usize = 64;

/// The particles drawn for `effect`: as many as its intensity asks of its fall.
pub fn count(effect: &Effect) -> u32 {
    fall(effect.kind).map_or(0, |fall| {
        (fall.count as f32 * effect.intensity.clamp(0.0, 1.0)).round() as u32
    })
}

/// The camera of the shader for `view`, the particles of `fall`, nothing drawn under `floor`.
pub fn camera_values(view: &View, fall: &Fall, floor: f32) -> [f32; CAMERA] {
    let mut values = [0f32; CAMERA];
    values[..16].copy_from_slice(&view.view_proj.to_cols_array());
    values[16..19].copy_from_slice(&view.sun.direction);
    values[20..23].copy_from_slice(&view.sun.colour);
    values[24..27].copy_from_slice(&view.sun.ambient);
    values[28..31].copy_from_slice(&view.eye.to_array());
    values[31] = (f64::from(view.time) % PERIOD) as f32;
    values[32..35].copy_from_slice(&view.fog.colour);
    values[36..40].copy_from_slice(&[view.fog.start, view.fog.middle, view.fog.end, view.fog.rate]);
    for (at, row) in [40, 44].into_iter().zip(0..2) {
        values[at..at + 3].copy_from_slice(&view.view.row(row).truncate().normalize_or_zero().to_array());
    }
    values[48..52].copy_from_slice(&fall.colour);
    values[52..56].copy_from_slice(&[fall.size[0], fall.size[1], f32::from(u8::from(fall.streak)), fall.sway]);
    values[56..60].copy_from_slice(&[fall.wind[0], fall.wind[1], fall.speed, floor]);
    values[60] = HALF_SIDE;
    values[61] = HEIGHT;
    values
}

/// Why nothing falls where the eye is, if so: under the water, or in a room of a building.
pub fn sheltered(
    liquids: Option<&liquids::Handle>,
    buildings: Option<&buildings::Handle>,
    eye: [f32; 3],
) -> (Option<&'static str>, f32) {
    let surface = liquids.and_then(|liquids| liquids.surfaces().surface(eye[0], eye[1]));
    if surface.is_some_and(|surface| eye[2] < surface) {
        return (Some("the eye is under the water"), f32::MIN);
    }
    let among = buildings.map(|buildings| buildings.eye());
    if among
        .as_ref()
        .and_then(|eye| eye.group.as_ref())
        .is_some_and(|group| group.room())
    {
        return (Some("the eye is in a room of a building"), f32::MIN);
    }
    // Nor in a liquid of a building the eye is in.
    if among.and_then(|eye| eye.liquid).flatten().is_some() {
        return (Some("the eye is in a liquid of a building"), f32::MIN);
    }
    (None, surface.unwrap_or(f32::MIN))
}

/// The pipeline and the uniform of the particles, built in a job then handed to the layer.
pub struct Gpu {
    pipeline: wgpu::RenderPipeline,
    camera: wgpu::Buffer,
    group: wgpu::BindGroup,
}

/// The resources of the particles on `device`, drawing into `target`.
pub fn create(device: &wgpu::Device, target: &Target) -> Gpu {
    let camera = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("weather camera"),
        size: (CAMERA * 4) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("weather"),
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
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("weather"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: camera.as_entire_binding(),
        }],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("weather"),
        source: wgpu::ShaderSource::Wgsl(
            [viewport::FOG_WGSL, viewport::LIGHT_WGSL, include_str!("fall.wgsl")]
                .concat()
                .into(),
        ),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("weather"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("weather"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        // Tested against the depth, behind what is drawn opaque, writing none.
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
            targets: &[Some(wgpu::ColorTargetState {
                format: target.color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    Gpu {
        pipeline,
        camera,
        group,
    }
}

/// What the module hands the layer: the weather applied, and the services that tell where the eye
/// is sheltered from it; and to the service, the flash of lightning now.
#[derive(Default)]
pub struct Falling {
    pub effect: Option<Effect>,
    pub flash: f32,
    pub liquids: Option<liquids::Handle>,
    pub buildings: Option<buildings::Handle>,
}

pub struct FallLayer {
    falling: Arc<Mutex<Falling>>,
    incoming: Arc<Mutex<Option<Gpu>>>,
    gpu: Option<Gpu>,
    /// The particles of the frame prepared.
    count: u32,
    stats: LayerStats,
}

impl FallLayer {
    pub fn new(falling: Arc<Mutex<Falling>>, incoming: Arc<Mutex<Option<Gpu>>>) -> Self {
        Self {
            falling,
            incoming,
            gpu: None,
            count: 0,
            stats: LayerStats::default(),
        }
    }
}

impl Layer for FallLayer {
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        if self.gpu.is_none() {
            self.gpu = lock(&self.incoming).take();
        }
        self.count = 0;
        let (effect, liquids, buildings) = {
            let falling = lock(&self.falling);
            (falling.effect, falling.liquids.clone(), falling.buildings.clone())
        };
        let (Some(resources), Some(effect)) = (&self.gpu, effect) else {
            self.stats = LayerStats::default();
            return;
        };
        let Some(fall) = fall(effect.kind) else {
            self.stats = LayerStats::default();
            return;
        };
        let (sheltered, floor) = sheltered(liquids.as_ref(), buildings.as_ref(), view.eye.to_array());
        self.count = if sheltered.is_some() { 0 } else { count(&effect) };
        gpu.queue.write_buffer(
            &resources.camera,
            0,
            bytemuck::cast_slice(&camera_values(view, &fall, floor)),
        );
        self.stats = LayerStats {
            draws: u64::from(self.count > 0),
            triangles: 2 * u64::from(self.count),
            bytes: (CAMERA * 4) as u64,
            items: format!("{} particles of {}", self.count, effect.kind.name()),
            ..LayerStats::default()
        };
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
        // On the eye's side of the water, after it.
        let Some(resources) = self.gpu.as_ref().filter(|_| phase == Phase::Near && self.count > 0) else {
            return;
        };
        pass.set_pipeline(&resources.pipeline);
        pass.set_bind_group(0, &resources.group, &[]);
        pass.draw(0..6, 0..self.count);
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}

#[cfg(test)]
mod wgsl_tests {

    #[test]
    fn its_shaders_parse_and_validate() {
        uniwow_api::viewport::check_wgsl(
            &[
                uniwow_api::viewport::FOG_WGSL,
                uniwow_api::viewport::LIGHT_WGSL,
                include_str!("fall.wgsl"),
            ]
            .concat(),
        )
        .unwrap_or_else(|error| panic!("weather: {error}"));
    }
}
