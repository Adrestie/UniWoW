//! Tests of the liquids: their meshes, flat layers by rectangles, and the surfaces of their water,
//! the frames of a type, the tiles read and let go; and, on the software adapter of the system when
//! it has one, a tile read giving the water over it by its place in the world, a tile refused for
//! want of room put on the GPU once a range is given back, the water
//! drawn over the magma under it, a blended batch under its surface seen through it and one over it
//! drawn over it, from over the water and from under it, the water writing no depth.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use uniwow_api::arena::Refusal;
use uniwow_api::formats::{
    AnimationRecord, AreaRecord, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair, FileRef,
    Formats, GameObjectDisplay, HairGeoset, LIQUID_SIDE, LiquidLayer, LiquidTypeRecord, MapRecord, Model, ORIGIN, TILE,
    Texture, TextureFormat, Tile, TileId, Wdl, Wdt, Wmo,
};
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::liquids::{CELL, Liquids, Placed, Surfaces};
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
    // The surfaces of the water alone, a cell each.
    assert_eq!(meshes.surfaces.len(), 2);
    let surfaces = Surfaces::from_cells(meshes.surfaces.iter().copied());
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

#[test]
fn a_flat_layer_is_a_quad_for_each_rectangle_of_the_cells_it_covers() {
    assert_eq!(mesh::rectangles(u64::MAX), [[0, 0, 8, 8]]);
    // Two rows of two, then one cell, then a run of two under it: three rectangles.
    let bits = |cells: &[(usize, usize)]| {
        cells
            .iter()
            .fold(0u64, |bits, (row, column)| bits | 1 << (row * 8 + column))
    };
    let cells = bits(&[(0, 0), (0, 1), (1, 0), (1, 1), (2, 4), (3, 4), (3, 5)]);
    assert_eq!(mesh::rectangles(cells), [[0, 0, 2, 2], [2, 4, 1, 1], [3, 4, 1, 2]]);
    assert_eq!(mesh::rectangles(0), Vec::<[usize; 4]>::new());
    let kind = |liquid: u16| (liquid == 2).then_some((0, true));
    // An ocean without vertices, over all its chunk: a quad, its 64 cells at its height.
    let ocean = mesh::meshes(&[layer(2, [100.0, 200.0], -1.0, u64::MAX, 255)], kind);
    assert_eq!(
        (ocean.vertices.len(), ocean.water.as_slice()),
        (4, [0, 1, 2, 0, 2, 3].as_slice())
    );
    assert_eq!(
        ocean.vertices.iter().map(|vertex| vertex.position).collect::<Vec<_>>(),
        [
            [100.0, 200.0, -1.0],
            [100.0, 200.0 - 8.0 * CELL, -1.0],
            [100.0 - 8.0 * CELL, 200.0 - 8.0 * CELL, -1.0],
            [100.0 - 8.0 * CELL, 200.0, -1.0]
        ]
    );
    assert_eq!((ocean.vertices[2].uv, ocean.vertices[2].depth), ([2.0, 2.0], 1.0));
    assert_eq!(ocean.surfaces.len(), 64);
    assert!(ocean.surfaces.iter().all(|(_, height)| *height == -1.0));
    // Partly covered: a quad a rectangle.
    let shore = mesh::meshes(&[layer(2, [100.0, 200.0], -1.0, cells, 255)], kind);
    assert_eq!(
        (shore.vertices.len(), shore.water.len(), shore.surfaces.len()),
        (12, 18, 7)
    );
    // A depth apart: its 9 × 9 vertices.
    let mut deep = layer(2, [100.0, 200.0], -1.0, u64::MAX, 255);
    deep.depths[40] = 10;
    let deep = mesh::meshes(&[deep], kind);
    assert_eq!((deep.vertices.len(), deep.water.len()), (81, 384));
}

#[test]
fn the_tiles_are_read_within_the_budget_and_the_room_of_the_arenas_and_let_go_beyond() {
    use crate::Stand::{Held, NoRoom, Reading, Waiting};
    let tile = |x| TileId { x, y: 0 };
    let wanted = [
        (tile(0), 0.0, Held),
        (tile(1), 100.0, Reading),
        (tile(2), 200.0, NoRoom),
        (tile(3), 300.0, Waiting),
        (tile(4), 400.0, Waiting),
        (tile(5), 500.0, Held),
        (tile(6), 600.0, Waiting),
    ];
    let all = [f32::INFINITY; 2];
    let steps = |budget, room, slots| crate::steps(&wanted, budget, room, slots);
    assert_eq!(
        steps(all, all, 9),
        (vec![tile(3), tile(4), tile(6)], vec![]),
        "all, but the tile waiting for room"
    );
    assert_eq!(
        steps(all, all, 3),
        (vec![tile(3), tile(4)], vec![]),
        "one being read already"
    );
    // The budget lets load to 400 and keep to 450: tile 5 let go.
    assert_eq!(steps([400.0, 450.0], all, 9), (vec![tile(3), tile(4)], vec![tile(5)]));
    // The arenas have room to load before 400 and to keep before 500.
    assert_eq!(steps(all, [400.0, 500.0], 9), (vec![tile(3)], vec![tile(5)]));
    // Room to keep only before 100: the tile being read let go, and its slot free again.
    assert_eq!(steps(all, [50.0, 100.0], 1), (vec![], vec![tile(1), tile(5)]));
    assert_eq!(steps(all, [400.0, 100.0], 1), (vec![tile(3)], vec![tile(1), tile(5)]));
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
    fn liquid_types(&self) -> Result<Arc<Vec<LiquidTypeRecord>>, String> {
        Ok(Arc::new(vec![
            record(5, 1, 3, r"XTextures\procWater\basicReflectionMap.blp"),
            record(7, 2, 2, r"XTextures\lava\magma0.blp"),
        ]))
    }
    /// A slow water at 2.5 over the first chunk of the tile, flat.
    fn liquids(&self, _directory: &str, x: u32, y: u32) -> Result<Option<Vec<LiquidLayer>>, String> {
        let corner = [ORIGIN - y as f32 * TILE, ORIGIN - x as f32 * TILE];
        Ok(Some(vec![layer(5, corner, 2.5, u64::MAX, 0)]))
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
    device_with(wgpu::Limits::default().max_buffer_size)
}

/// A device of the software adapter of the system, its buffers of `largest` bytes at most; or none.
fn device_with(largest: u64) -> Option<egui_wgpu::RenderState> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: true,
        ..Default::default()
    }))?
    .ok()?;
    let mut limits = wgpu::Limits::default();
    limits.max_sampled_textures_per_shader_stage = adapter.limits().max_sampled_textures_per_shader_stage.min(128);
    limits.max_buffer_size = largest;
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
    let surfaces = Surfaces::from_cells(meshes.surfaces.iter().copied());
    let scene = Arc::new(Mutex::new(Scene {
        tiles: tile.into_iter().collect(),
        ..Scene::default()
    }));
    (LiquidsLayer::new(shared, scene), surfaces)
}

#[test]
fn a_tile_read_gives_the_water_over_it_by_its_place_in_the_world() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let shared = Arc::new(Shared::new(&gpu, &TARGET).expect("the liquids on the device"));
    let tile = TileId { x: 31, y: 49 };
    let held = crate::read(&Liquid, &shared, "Azeroth", tile).unwrap();
    assert_eq!(
        held.gpu.as_ref().map(|gpu| (gpu.water.len(), gpu.opaque.len())),
        Some((6, 0))
    );
    let surfaces = crate::surfaces(&std::collections::HashMap::from([(tile, held)]));
    let corner = [ORIGIN - 49.0 * TILE, ORIGIN - 31.0 * TILE];
    assert_eq!(surfaces.surface(corner[0] - 1.0, corner[1] - 1.0), Some(2.5));
    assert_eq!(
        surfaces.surface(corner[0] - 1.0, corner[1] - CELL * 8.0 - 1.0),
        None,
        "the chunk beside"
    );
}

#[test]
fn a_tile_refused_for_want_of_room_is_put_on_the_gpu_once_a_range_is_given_back() {
    // Buffers of 32 KB: 1,170 vertices; twelve layers of 9 × 9 vertices hold, not 24.
    let Some(gpu) = device_with(32 << 10) else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let shared = Arc::new(Shared::new(&gpu, &TARGET).expect("the liquids on the device"));
    let mut uneven = layer(5, [100.0, 100.0], 0.0, u64::MAX, 0);
    uneven.depths[40] = 255;
    let tile = mesh::meshes(&vec![uneven; 12], |_| Some((0, true)));
    let first = gpu::upload(&shared, &tile).unwrap().unwrap();
    let given = shared.given();
    assert!(
        matches!(gpu::upload(&shared, &tile), Err(Refusal::NoRoom(_))),
        "no room for a second"
    );
    drop(first);
    assert_eq!(shared.given(), given + 2, "its vertices and its indices given back");
    assert!(gpu::upload(&shared, &tile).unwrap().is_some(), "room made");
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
    // The magma alone: red, opaque and unlit; its tile out of sight, not drawn.
    let (mut magma, _) = bench(&gpu, &[layer(7, corner, -2.0, u64::MAX, 255)]);
    assert_eq!(middle(&gpu, &mut magma, &view(above, down), None), [255, 0, 0, 255]);
    assert!(
        magma.stats().items.starts_with("1 drawn of 1 tiles"),
        "{}",
        magma.stats().items
    );
    let away = view(above + Vec3::new(500.0, 0.0, 0.0), above + Vec3::new(1000.0, 0.0, 0.0));
    middle(&gpu, &mut magma, &away, None);
    assert!(
        magma.stats().items.starts_with("0 drawn of 1 tiles"),
        "{}",
        magma.stats().items
    );
    // In the fog of the game, blue, from 2 to 42 yards at the rate 2: at 22 yards from the eye,
    // 1 − (20 / 40)² of it, mixed in gamma as the client.
    let mut fogged = view(above, down);
    fogged.fog = uniwow_api::viewport::Fog {
        colour: [0.0, 0.0, 1.0],
        start: 2.0,
        middle: 22.0,
        end: 42.0,
        rate: 2.0,
    };
    let seen = middle(&gpu, &mut magma, &fogged, None);
    assert!(
        seen[0].abs_diff(64) <= 2 && seen[1] == 0 && seen[2].abs_diff(191) <= 2,
        "{seen:?}"
    );
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

/// A square liquid of `liquid` over the origin at `height`, placed by another module.
fn square_placed(liquid: u16, height: f32) -> Placed {
    Placed {
        liquid,
        positions: vec![
            [-50.0, -50.0, height],
            [50.0, -50.0, height],
            [50.0, 50.0, height],
            [-50.0, 50.0, height],
        ],
        coordinates: vec![[0.0, 0.0]; 4],
        depths: vec![0.0; 4],
        triangles: vec![0, 1, 2, 0, 2, 3, 0, 1, 9],
    }
}

#[test]
fn a_liquid_placed_is_drawn_as_given_its_water_under_the_surfaces() {
    let kind = |liquid: u16| match liquid {
        5 => Some((0, true)),
        7 => Some((1, false)),
        _ => None,
    };
    let water = mesh::placed(&square_placed(5, 3.0), kind);
    assert_eq!(
        (water.vertices.len(), water.water.as_slice()),
        (4, [0, 1, 2, 0, 2, 3].as_slice()),
        "a triangle out of its vertices left out"
    );
    assert_eq!(water.surfaces.len(), 8);
    assert!(water.surfaces.iter().all(|(_, height)| *height == 3.0));
    let magma = mesh::placed(&square_placed(7, 3.0), kind);
    assert_eq!((magma.opaque.len(), magma.surfaces.len()), (6, 0), "magma no water");
    assert_eq!(mesh::placed(&square_placed(99, 3.0), kind), mesh::Meshes::default());
    // Placed through the service: a flag each, shown; taken away.
    let service = crate::Water::default();
    let flags = service.place(
        "buildings/Azeroth/7",
        vec![square_placed(5, 3.0), square_placed(7, 1.0)],
    );
    assert_eq!(flags.len(), 2);
    assert!(flags.iter().all(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)));
    assert_eq!(
        service.placed.lock().unwrap()["buildings/Azeroth/7"]
            .as_ref()
            .map(Vec::len),
        Some(2)
    );
    service.clear("buildings/Azeroth/7");
    assert!(service.placed.lock().unwrap()["buildings/Azeroth/7"].is_none());
}

#[test]
fn a_liquid_placed_is_put_on_the_gpu_and_drawn_while_its_owner_shows_it() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let shared = Arc::new(Shared::new(&gpu, &TARGET).expect("the liquids on the device"));
    let shown = Arc::new(AtomicBool::new(true));
    let pour = [
        (square_placed(7, -2.0), shown.clone()),
        (square_placed(5, 0.0), Arc::new(AtomicBool::new(false))),
    ];
    let poured = crate::pour(&Liquid, &shared, &pour).unwrap();
    assert_eq!(poured.liquids.len(), 2);
    assert_eq!(
        (
            poured.surfaces.surface(50.0 / 3.0, -50.0 / 3.0),
            poured.surfaces.surface(100.0, 10.0)
        ),
        (Some(0.0), None),
        "the water's alone, under the middle of a triangle"
    );
    assert_eq!(
        poured.liquids[0].bounds,
        [Vec3::new(-50.0, -50.0, -2.0), Vec3::new(50.0, 50.0, -2.0)]
    );
    let scene = Arc::new(Mutex::new(Scene {
        placed: poured.liquids,
        ..Scene::default()
    }));
    let mut layer = LiquidsLayer::new(shared, scene);
    let above = view(Vec3::new(0.0, 0.0, 20.0), Vec3::new(0.0, 0.0, -30.0));
    assert_eq!(
        middle(&gpu, &mut layer, &above, None),
        [255, 0, 0, 255],
        "the magma shown, the water not"
    );
    shown.store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        middle(&gpu, &mut layer, &above, None),
        [0, 0, 0, 255],
        "hidden by its owner"
    );
    shown.store(true, std::sync::atomic::Ordering::Relaxed);
    let away = view(Vec3::new(500.0, 0.0, 20.0), Vec3::new(1000.0, 0.0, 20.0));
    middle(&gpu, &mut layer, &away, None);
    assert!(
        layer
            .stats()
            .items
            .starts_with("0 drawn of 0 tiles, and 0 of the 2 liquids"),
        "out of sight: {}",
        layer.stats().items
    );
}
