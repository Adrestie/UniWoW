//! Tests of the liquids: their meshes and the surfaces of their water, the frames of a type, and,
//! on the software adapter of the system when it has one, the water drawn over the magma under it,
//! a blended batch under its surface seen through it and one over it drawn over it, from over the
//! water and from under it, the water writing no depth.

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use uniwow_api::formats::{
    AnimationRecord, AreaRecord, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair, FileRef,
    Formats, GameObjectDisplay, HairGeoset, LIQUID_SIDE, LiquidLayer, LiquidTypeRecord, MapRecord, Model, Texture,
    TextureFormat, Tile, Wdl, Wdt, Wmo,
};
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::liquids::{CELL, Surfaces};
use uniwow_api::viewport::{Layer, Phase, Target, View};
use uniwow_api::{bytemuck, egui, egui_wgpu, wgpu};

use crate::gpu::{self, Shared};
use crate::layer::{LiquidsLayer, Scene};
use crate::mesh;

/// A layer over the chunk whose corner is `corner`, flat at `height`, covering `tiles`, its depths
/// `depth`.
fn layer(liquid: u16, corner: [f32; 2], height: f32, tiles: u64, depth: u8) -> LiquidLayer {
    let size = LIQUID_SIDE * LIQUID_SIDE;
    LiquidLayer {
        liquid,
        corner,
        tiles,
        heights: vec![height; size],
        depths: vec![depth; size],
        coordinates: Vec::new(),
    }
}

#[test]
fn a_layer_is_two_triangles_a_tile_it_covers_its_water_giving_the_height_of_its_surface() {
    let mut water = layer(5, [100.0, 200.0], 3.0, 1 | 1 << 9, 51);
    // A corner shared by both tiles higher: the surface of each the mean of its corners.
    water.heights[LIQUID_SIDE + 1] = 7.0;
    let mut magma = layer(7, [100.0, 200.0], -1.0, 1 << 63, 255);
    magma.coordinates = vec![[0.5, 0.25]; LIQUID_SIDE * LIQUID_SIDE];
    let meshes = mesh::meshes(
        &[water, magma, layer(99, [0.0; 2], 0.0, u64::MAX, 0)],
        |liquid| match liquid {
            5 => Some((0, true)),
            7 => Some((1, false)),
            _ => None,
        },
    );
    assert_eq!(meshes.vertices.len(), 2 * 81, "an unknown type left out");
    assert!(
        mesh::is_water(0) && mesh::is_water(1) && !mesh::is_water(2) && !mesh::is_water(3),
        "magma and slime no water"
    );
    assert_eq!((meshes.water.len(), meshes.opaque.len()), (12, 6));
    // A row goes down in X, a column down in Y, a tile apart.
    let vertex = meshes.vertices[LIQUID_SIDE + 2];
    assert_eq!(vertex.position, [100.0 - CELL, 200.0 - 2.0 * CELL, 3.0]);
    assert_eq!((vertex.uv, vertex.depth, vertex.slot), ([0.5, 0.25], 0.2, 0));
    assert_eq!(meshes.water[..6], [0, 1, 10, 0, 10, 9]);
    assert_eq!(meshes.opaque[..3], [81 + 70, 81 + 71, 81 + 80]);
    assert_eq!(meshes.vertices[81].uv, [0.5, 0.25], "its own coordinates");
    assert_eq!(meshes.vertices[81].slot, 1);
    // The surfaces of the water alone, a tile each.
    let mut surfaces = Surfaces::default();
    for (cell, height) in &meshes.surfaces {
        surfaces.add(*cell, *height);
    }
    assert_eq!(surfaces.len(), 2);
    let middle = |row: f32, column: f32| (100.0 - (row + 0.5) * CELL, 200.0 - (column + 0.5) * CELL);
    let (x, y) = middle(0.0, 0.0);
    assert_eq!(surfaces.surface(x, y), Some(4.0));
    let (x, y) = middle(1.0, 1.0);
    assert_eq!(surfaces.surface(x, y), Some(4.0));
    let (x, y) = middle(0.0, 1.0);
    assert_eq!(surfaces.surface(x, y), None, "a tile not covered");
    let (x, y) = middle(7.0, 7.0);
    assert_eq!(surfaces.surface(x, y), None, "magma is no water");
}

fn record(id: u32, kind: u32, material: u32, texture: &str) -> LiquidTypeRecord {
    LiquidTypeRecord {
        id,
        name: String::new(),
        kind,
        material,
        vertex_format: None,
        textures: [
            texture.to_owned(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ],
        animation: [2.0, 3.0],
    }
}

#[test]
fn the_frames_of_a_type_are_those_its_texture_names_a_procedural_water_those_of_lake_a() {
    let (names, animation) = gpu::frames(&record(1, 1, 3, r"XTextures\procWater\basicReflectionMap.blp"));
    assert_eq!(
        (names.len(), names[0].as_str(), names[29].as_str()),
        (30, r"XTextures\river\lake_a.1.blp", r"XTextures\river\lake_a.30.blp")
    );
    assert_eq!(animation, [1.0, 0.0], "turning without moving");
    let (names, animation) = gpu::frames(&record(3, 2, 2, r"XTextures\lava\lava.%d.blp"));
    assert_eq!(
        (names.len(), names[4].as_str(), animation),
        (30, r"XTextures\lava\lava.5.blp", [2.0, 3.0])
    );
    assert_eq!(
        gpu::frames(&record(7, 2, 2, r"XTextures\lava\magma0.blp")).0,
        [r"XTextures\lava\magma0.blp"]
    );
    assert!(gpu::frames(&record(9, 0, 1, "")).0.is_empty());
}

/// The formats of the tests: the frames of the water black, the magma red.
struct Liquid;

impl Formats for Liquid {
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String> {
        Err("none".to_owned())
    }
    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String> {
        Err("none".to_owned())
    }
    fn creature_displays(&self) -> Result<Arc<Vec<CreatureDisplay>>, String> {
        Err("none".to_owned())
    }
    fn creature_models(&self) -> Result<Arc<Vec<CreatureModel>>, String> {
        Err("none".to_owned())
    }
    fn creature_looks(&self) -> Result<Arc<Vec<CreatureLook>>, String> {
        Err("none".to_owned())
    }
    fn hair_geosets(&self) -> Result<Arc<Vec<HairGeoset>>, String> {
        Err("none".to_owned())
    }
    fn facial_hairs(&self) -> Result<Arc<Vec<FacialHair>>, String> {
        Err("none".to_owned())
    }
    fn game_object_displays(&self) -> Result<Arc<Vec<GameObjectDisplay>>, String> {
        Err("none".to_owned())
    }
    fn char_sections(&self) -> Result<Arc<Vec<CharSection>>, String> {
        Err("none".to_owned())
    }
    fn animations(&self) -> Result<Arc<Vec<AnimationRecord>>, String> {
        Err("none".to_owned())
    }
    fn model(&self, _file: &FileRef) -> Result<Model, String> {
        Err("none".to_owned())
    }
    fn wmo(&self, _file: &FileRef) -> Result<Wmo, String> {
        Err("none".to_owned())
    }
    fn wdt(&self, _directory: &str) -> Result<Arc<Wdt>, String> {
        Err("none".to_owned())
    }
    fn tile(&self, _directory: &str, _x: u32, _y: u32) -> Result<Option<Tile>, String> {
        Ok(None)
    }
    fn wdl(&self, _directory: &str) -> Result<Option<Wdl>, String> {
        Ok(None)
    }
    fn texture(&self, file: &FileRef) -> Result<Texture, String> {
        let colour = match file {
            FileRef::Path(path) if path.contains("lake_a") => [0, 0, 0, 255],
            FileRef::Path(path) if path.contains("magma") => [255, 0, 0, 255],
            _ => return Err("none".to_owned()),
        };
        Ok(Texture {
            width: 4,
            height: 4,
            format: TextureFormat::Rgba8,
            levels: vec![colour.repeat(16)],
        })
    }
    fn texture_rgba(&self, file: &FileRef) -> Result<Texture, String> {
        self.texture(file)
    }
}

fn resolved<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// A device of the software adapter of the system; or none.
fn device() -> Option<egui_wgpu::RenderState> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: true,
        ..Default::default()
    }))?
    .ok()?;
    let mut limits = wgpu::Limits::default();
    limits.max_sampled_textures_per_shader_stage = adapter.limits().max_sampled_textures_per_shader_stage.min(128);
    let (device, queue) = resolved(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits,
        ..Default::default()
    }))?
    .ok()?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
    Some(egui_wgpu::RenderState {
        adapter,
        available_adapters: Vec::new(),
        instance,
        device,
        queue,
        target_format: format,
        renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
        surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
    })
}

const TARGET: Target = Target {
    color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
    depth_format: wgpu::TextureFormat::Depth32Float,
    sample_count: 1,
    depth_compare: wgpu::CompareFunction::Greater,
};

/// A blended batch over the whole view: its colour at a depth, tested against the depth drawn.
struct Painter {
    pipeline: wgpu::RenderPipeline,
    group: wgpu::BindGroup,
}

impl Painter {
    fn new(gpu: &egui_wgpu::RenderState, colour: [f32; 4], depth: f32) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(
                "struct Painted { colour: vec4<f32>, depth: vec4<f32> };\n\
                 @group(0) @binding(0) var<uniform> painted: Painted;\n\
                 @vertex fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {\n\
                     let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u)) * 2.0 - 1.0;\n\
                     return vec4<f32>(corner, painted.depth.x, 1.0);\n\
                 }\n\
                 @fragment fn fs() -> @location(0) vec4<f32> { return painted.colour; }\n"
                    .into(),
            ),
        });
        let values = [colour[0], colour[1], colour[2], colour[3], depth, 0.0, 0.0, 0.0];
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&values));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: TARGET.depth_format,
                depth_write_enabled: Some(false),
                depth_compare: Some(TARGET.depth_compare),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET.color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        Self { pipeline, group }
    }
}

/// The view from `eye` towards `target`, 16 × 16 pixels.
fn view(eye: Vec3, target: Vec3) -> View {
    let look = Mat4::look_at_rh(eye, target, Vec3::Y);
    View {
        view_proj: Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1) * look,
        view: look,
        eye,
        size: [16, 16],
        time: 0.0,
        fog: Default::default(),
        sun: Default::default(),
    }
}

/// The middle pixel of `layer` drawn in `view` over black, with `painted` drawn first in its phase,
/// as a layer of another stage would be.
fn middle(
    gpu: &egui_wgpu::RenderState,
    layer: &mut LiquidsLayer,
    view: &View,
    painted: Option<(&Painter, Phase)>,
) -> [u8; 4] {
    let size = 16u32;
    layer.prepare(gpu, view);
    let texture = |format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let colour = texture(
        TARGET.color_format,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let depth = texture(TARGET.depth_format, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(256 * size),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &colour.create_view(&Default::default()),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.create_view(&Default::default()),
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for phase in Phase::ALL {
            if let Some((painter, at)) = painted
                && at == phase
            {
                pass.set_pipeline(&painter.pipeline);
                pass.set_bind_group(0, &painter.group, &[]);
                pass.draw(0..3, 0..1);
            }
            layer.draw_pass(gpu, &TARGET, view, phase, &mut pass);
        }
    }
    encoder.copy_texture_to_buffer(
        colour.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &pixels,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(size),
            },
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    pixels.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = pixels.slice(..).get_mapped_range().expect("mapped").to_vec();
    let at = (size / 2 * 256 + size / 2 * 4) as usize;
    [data[at], data[at + 1], data[at + 2], data[at + 3]]
}

/// A layer drawing `layers` of a procedural water and a magma, and the surfaces of its water.
fn bench(gpu: &egui_wgpu::RenderState, layers: &[LiquidLayer]) -> (LiquidsLayer, Surfaces) {
    let shared = Arc::new(Shared::new(gpu, &TARGET).expect("the liquids on the device"));
    let water = record(5, 1, 3, r"XTextures\procWater\basicReflectionMap.blp");
    let magma = record(7, 2, 2, r"XTextures\lava\magma0.blp");
    let meshes = mesh::meshes(layers, |liquid| {
        let record = [&water, &magma]
            .into_iter()
            .find(|record| record.id == u32::from(liquid))?;
        Some((shared.slot(&Liquid, record)?, mesh::is_water(record.kind)))
    });
    let tile = gpu::upload(&shared, &meshes).unwrap().map(Arc::new);
    let mut surfaces = Surfaces::default();
    for (cell, height) in &meshes.surfaces {
        surfaces.add(*cell, *height);
    }
    let scene = Arc::new(Mutex::new(Scene {
        tiles: tile.into_iter().collect(),
        ..Scene::default()
    }));
    (LiquidsLayer::new(shared, scene), surfaces)
}

#[test]
fn the_water_is_drawn_over_what_lies_under_it_without_hiding_what_is_blended_beyond_it() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let corner = [100.0, 100.0];
    let (above, below, down, up) = (
        Vec3::new(83.0, 83.0, 20.0),
        Vec3::new(83.0, 83.0, -1.0),
        Vec3::new(83.0, 83.0, -30.0),
        Vec3::new(83.0, 83.0, 30.0),
    );
    // The magma alone: red, opaque and unlit.
    let (mut magma, _) = bench(&gpu, &[layer(7, corner, -2.0, u64::MAX, 255)]);
    assert_eq!(middle(&gpu, &mut magma, &view(above, down), None), [255, 0, 0, 255]);
    // Under the shallow water, seen through it: tinted.
    let (mut liquids, surfaces) = bench(
        &gpu,
        &[
            layer(7, corner, -2.0, u64::MAX, 255),
            layer(5, corner, 0.0, u64::MAX, 0),
        ],
    );
    let through = middle(&gpu, &mut liquids, &view(above, down), None);
    assert!(through[0] > 150 && through[0] < 250 && through[1] > 20, "{through:?}");
    // A green half seen through, under the surface or over it: beyond the water from the eye, drawn
    // before it and tinted; on the eye's side, drawn over it. From under the water, the other way.
    let painter = Painter::new(&gpu, [0.0, 1.0, 0.0, 0.5], 0.5);
    let (under, over) = (Vec3::new(83.0, 83.0, -0.5), Vec3::new(83.0, 83.0, 1.0));
    for (eye, target, beyond, near) in [(above, down, under, over), (below, up, over, under)] {
        assert_eq!(surfaces.phase(eye, beyond), Phase::Beyond);
        assert_eq!(surfaces.phase(eye, near), Phase::Near);
        let view = view(eye, target);
        let first = middle(&gpu, &mut liquids, &view, Some((&painter, surfaces.phase(eye, beyond))));
        let last = middle(&gpu, &mut liquids, &view, Some((&painter, surfaces.phase(eye, near))));
        assert!(last[1] > first[1] + 20, "over the water {last:?}, under it {first:?}");
    }
    // Farther than the surface but nearer than the magma: drawn after the water, it shows, the
    // water having written no depth.
    let behind = Painter::new(&gpu, [0.0, 1.0, 0.0, 0.5], 0.0048);
    let seen = middle(&gpu, &mut liquids, &view(above, down), Some((&behind, Phase::Near)));
    assert!(seen[1] > through[1] + 20, "{seen:?} against {through:?}");
    // Behind the magma, which wrote its depth: hidden.
    let under_magma = Painter::new(&gpu, [0.0, 1.0, 0.0, 0.5], 0.002);
    assert_eq!(
        middle(
            &gpu,
            &mut liquids,
            &view(above, down),
            Some((&under_magma, Phase::Near))
        ),
        through
    );
}
