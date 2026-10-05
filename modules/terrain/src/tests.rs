//! Tests of the terrain on tiles the tests make, and on the device of the software adapter of the
//! system when there is one, skipped otherwise.

use std::collections::HashSet;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use uniwow_api::formats::{
    AreaRecord, Chunk, CreatureDisplay, CreatureModel, FileRef, Formats, Layer, MapRecord, Texture, TextureFormat,
    Tile, Wdt,
};
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::viewport::Target;
use uniwow_api::{bytemuck, egui, egui_wgpu, wgpu};

use crate::gpu::{self, Shared};
use crate::layer::in_sight;
use crate::loading::{self, Kept, Plan};
use crate::mesh::{self, VERTICES, Vertex};
use crate::model::{CHUNK, ORIGIN, STEP, TILE, TileId, TileModel, chunk_bounds};

/// A chunk the tests make: its index, a position that may be wrong, its holes, three layers, the
/// third naming a texture the tile does not have.
fn chunk(index: [u32; 2], position: [f32; 3], holes: u64) -> Chunk {
    let layer = |texture, flags| Layer {
        texture,
        flags,
        effect: 0,
    };
    Chunk {
        index,
        flags: 0,
        position,
        heights: (0..145).map(|v| v as f32 * 0.5).collect(),
        normals: (0..145).map(|v| [(v % 7) as i8, 3, 120]).collect(),
        colours: Vec::new(),
        area: 12,
        holes,
        layers: vec![layer(0, 0), layer(1, 0x100), layer(7, 0x100)],
        alphas: vec![vec![200; 4096], vec![50; 4096]],
        shadow: vec![255; 4096],
        doodad_refs: Vec::new(),
        building_refs: Vec::new(),
    }
}

/// A tile of 256 chunks all at a wrong position, as the tiles of the row 60 of Azeroth; the
/// chunk 9 with a hole.
fn tile() -> Tile {
    Tile {
        chunks: (0..256u32)
            .map(|place| {
                let holes = if place == 9 { 1 << 9 } else { 0 };
                chunk([place % 16, place / 16], [3200.0, 1066.667, 7.0], holes)
            })
            .collect(),
        textures: vec![FileRef::Path("a.blp".to_owned()), FileRef::Path("b.blp".to_owned())],
        doodads: Vec::new(),
        buildings: Vec::new(),
    }
}

#[test]
fn a_chunk_is_placed_by_its_tile_and_index_its_file_s_position_wrong() {
    let id = TileId { x: 10, y: 60 };
    let chunk = chunk([3, 5], [3200.0, 1066.667, 7.0], 0);
    let vertices = mesh::vertices(id, 83, &chunk);
    let corner = [ORIGIN - TILE * 60.0 - CHUNK * 5.0, ORIGIN - TILE * 10.0 - CHUNK * 3.0];
    let expected = [
        (0, [corner[0], corner[1], 7.0]),
        // The inner vertex of the row 0, column 7.
        (16, [corner[0] - 0.5 * STEP, corner[1] - 7.5 * STEP, 7.0 + 8.0]),
        (144, [corner[0] - 8.0 * STEP, corner[1] - 8.0 * STEP, 7.0 + 72.0]),
    ];
    for (vertex, position) in expected {
        let found = vertices[vertex].position;
        assert!(
            found.iter().zip(position).all(|(a, b)| (a - b).abs() < 1e-3),
            "vertex {vertex}: {found:?}, expected {position:?}"
        );
    }
    assert_eq!(vertices[16].uv, [7.5 / 8.0, 0.5 / 8.0]);
    assert_eq!((vertices[0].chunk, vertices[0].colour), (83, [127, 127, 127, 255]));
    let [low, high] = chunk_bounds(id, &chunk);
    assert_eq!([high[0], high[1]], corner, "its bounds by its place too");
    assert_eq!((low[2], high[2]), (7.0, 7.0 + 72.0));
}

#[test]
fn the_triangles_of_a_chunk_face_up_and_leave_its_holes_out() {
    let flat = chunk([0, 0], [0.0; 3], 0);
    let vertices = mesh::vertices(TileId { x: 32, y: 32 }, 0, &flat);
    let whole = mesh::indices(0);
    assert_eq!(whole.len(), 8 * 8 * 12);
    for triangle in whole.as_chunks::<3>().0 {
        let [a, b, c] = triangle.map(|i| Vec3::from(vertices[usize::from(i)].position));
        assert!((b - a).cross(c - a).z > 0.0, "{triangle:?} faces down");
    }
    // The quad of the row 1, column 1, and its inner vertex 27.
    let holed = mesh::indices(1 << 9);
    assert_eq!(holed.len(), whole.len() - 12);
    assert!(!holed.contains(&27));
}

#[test]
fn the_texels_of_blending_carry_three_alpha_maps_and_the_shadow() {
    let texels = mesh::blend(&chunk([0, 0], [0.0; 3], 0));
    assert_eq!(texels.len(), 64 * 64 * 4);
    assert_eq!(&texels[..4], &[200, 50, 0, 255]);
}

fn tiles(present: &[(u32, u32)]) -> Vec<bool> {
    let mut tiles = vec![false; 4096];
    for (x, y) in present {
        tiles[(y * 64 + x) as usize] = true;
    }
    tiles
}

fn id(x: u32, y: u32) -> TileId {
    TileId { x, y }
}

#[test]
fn the_tiles_wanted_are_those_around_the_camera_the_nearest_first() {
    let all: Vec<(u32, u32)> = (30..35)
        .flat_map(|x| (30..35).map(move |y| (x, y)))
        .filter(|tile| *tile != (33, 32))
        .collect();
    let eye = id(32, 32).centre();
    let wanted = loading::wanted(&tiles(&all), eye, 1);
    assert_eq!(wanted[0], id(32, 32));
    let near: HashSet<TileId> = wanted[1..4].iter().copied().collect();
    assert_eq!(
        near,
        HashSet::from([id(31, 32), id(32, 31), id(32, 33)]),
        "the sides, (33, 32) missing from the WDT"
    );
    assert_eq!(wanted.len(), 8, "and the four corners");
    assert!(loading::wanted(&tiles(&all), [ORIGIN * 4.0, 0.0], 3).is_empty());
}

#[test]
fn the_loads_start_nearest_first_within_their_slots_and_those_left_are_cancelled() {
    let (a, b, c, d, e) = (id(1, 1), id(2, 2), id(3, 3), id(4, 4), id(5, 5));
    let plan = loading::plan(&[b, c, d, e, a], &HashSet::from([a]), &HashSet::from([b]), 3);
    assert_eq!(
        plan,
        Plan {
            start: vec![c, d],
            cancel: Vec::new(),
        }
    );
    let plan = loading::plan(&[d, e], &HashSet::from([a, c]), &HashSet::new(), 3);
    assert_eq!(
        plan,
        Plan {
            start: vec![d, e],
            cancel: vec![a, c],
        },
        "the camera gone, its loads cancelled and their slots free"
    );
}

#[test]
fn beyond_the_budget_the_tiles_out_of_sight_are_released_and_loaded_again_in_sight() {
    let eye = id(32, 32).centre();
    let kept = |x, seen| Kept {
        tile: id(32, x),
        bytes: 100,
        seen,
    };
    let kept = [kept(33, 5), kept(32, 9), kept(34, 3), kept(30, 3)];
    assert!(
        loading::release(&kept, 9, eye, 400, 400).is_empty(),
        "within the budget"
    );
    assert_eq!(
        loading::release(&kept, 9, eye, 400, 250),
        vec![id(32, 30), id(32, 34)],
        "the longest unseen, the farthest of them first"
    );
    assert_eq!(
        loading::release(&kept, 9, eye, 400, 0),
        vec![id(32, 30), id(32, 34), id(32, 33)],
        "the tile in sight kept, even beyond the budget"
    );
    let plan = loading::plan(&[id(32, 34)], &HashSet::new(), &HashSet::from([id(32, 32)]), 2);
    assert_eq!(
        plan.start,
        vec![id(32, 34)],
        "a tile released is loaded again once wanted"
    );
}

#[test]
fn a_box_is_in_sight_ahead_and_not_behind_nor_aside() {
    let view = Mat4::perspective_rh(1.0, 1.0, 1.0, 10_000.0)
        * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 10.0), Vec3::new(100.0, 0.0, 0.0), Vec3::Z);
    assert!(in_sight(view, [[90.0, -10.0, -5.0], [110.0, 10.0, 5.0]]));
    assert!(!in_sight(view, [[-110.0, -10.0, -5.0], [-90.0, 10.0, 5.0]]), "behind");
    let level = Mat4::perspective_rh(1.0, 1.0, 1.0, 10_000.0)
        * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 10.0), Vec3::new(100.0, 0.0, 10.0), Vec3::Z);
    assert!(
        !in_sight(level, [[-2000.0, -5000.0, -5000.0], [-100.0, 5000.0, 5000.0]]),
        "behind, wider than the view on every side: only its depth puts it out"
    );
    assert!(!in_sight(view, [[0.0, 900.0, -5.0], [20.0, 920.0, 5.0]]), "aside");
}

/// The formats of the tests: the texture `a.blp`, a block of DXT1, and the others of RGBA in three
/// levels; how often each read is asked.
#[derive(Default)]
struct Fake {
    stored: AtomicUsize,
    decoded: AtomicUsize,
}

fn rgba() -> Texture {
    Texture {
        width: 4,
        height: 4,
        format: TextureFormat::Rgba8,
        levels: vec![vec![90; 64], vec![90; 16], vec![90; 4]],
    }
}

impl Formats for Fake {
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String> {
        Err("no maps".to_owned())
    }
    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String> {
        Err("no areas".to_owned())
    }
    fn creature_displays(&self) -> Result<Arc<Vec<CreatureDisplay>>, String> {
        Err("no looks".to_owned())
    }
    fn creature_models(&self) -> Result<Arc<Vec<CreatureModel>>, String> {
        Err("no models".to_owned())
    }
    fn wdt(&self, _directory: &str) -> Result<Arc<Wdt>, String> {
        Err("no WDT".to_owned())
    }
    fn tile(&self, _directory: &str, _x: u32, _y: u32) -> Result<Option<Tile>, String> {
        Ok(None)
    }
    fn texture(&self, file: &FileRef) -> Result<Texture, String> {
        self.stored.fetch_add(1, Ordering::Relaxed);
        Ok(match file {
            FileRef::Path(path) if path == "a.blp" => Texture {
                width: 4,
                height: 4,
                format: TextureFormat::Bc1,
                levels: vec![vec![0x00, 0xF8, 0xE0, 0x07, 0xE4, 0xE4, 0xE4, 0xE4]],
            },
            _ => rgba(),
        })
    }
    fn texture_rgba(&self, _file: &FileRef) -> Result<Texture, String> {
        self.decoded.fetch_add(1, Ordering::Relaxed);
        Ok(rgba())
    }
}

fn resolved<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// A device of the software adapter of the system, with BC when it offers it; or none.
fn device() -> Option<egui_wgpu::RenderState> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: true,
        ..Default::default()
    }))?
    .ok()?;
    let (device, queue) = resolved(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC,
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
    sample_count: 4,
    depth_compare: wgpu::CompareFunction::Greater,
};

/// The bytes of `buffer`, copied back from the GPU.
fn read_back(gpu: &egui_wgpu::RenderState, buffer: &wgpu::Buffer) -> Vec<u8> {
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read back"),
        size: buffer.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, buffer.size());
    gpu.queue.submit([encoder.finish()]);
    staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    staging.slice(..).get_mapped_range().expect("mapped").to_vec()
}

fn vertices_of(model: &TileModel) -> Vec<Vertex> {
    (0..256)
        .flat_map(|place| mesh::vertices(model.id, place, &model.tile.chunks[place]))
        .collect()
}

#[test]
fn a_tile_built_by_its_job_is_uploaded_while_no_view_draws_and_a_chunk_rebuilt_alone() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let shared = Shared::new(&gpu, &TARGET);
    let formats = Fake::default();
    let mut model = TileModel::new(id(10, 60), tile());
    let built = gpu::build_tile(&shared, &formats, &model, &|| false).unwrap().unwrap();
    assert_eq!(built.ranges.len(), 256);
    assert_eq!(
        built.ranges[9].len(),
        built.ranges[8].len() - 12,
        "the hole of the chunk 9"
    );
    assert!(built.bounds[1][0] <= ORIGIN - TILE * 60.0 + 1e-3, "placed by its tile");
    // Nothing drawn: the uploads were submitted by the job itself.
    let uploaded: Vec<Vertex> = bytemuck::cast_slice(&read_back(&gpu, &built.vertices)).to_vec();
    assert_eq!(uploaded, vertices_of(&model));

    model.tile.chunks[5].heights[0] += 10.0;
    model.mark_changed(5);
    assert!(model.changed() && model.is_changed(5) && !model.is_changed(6));
    gpu::write_chunk(&shared.queue, &built.vertices, &built.blend, &model, 5);
    let rebuilt: Vec<Vertex> = bytemuck::cast_slice(&read_back(&gpu, &built.vertices)).to_vec();
    assert_eq!(rebuilt, vertices_of(&model), "the chunk 5 written again alone");
    assert_ne!(rebuilt[5 * VERTICES], uploaded[5 * VERTICES]);
    assert_eq!(rebuilt[6 * VERTICES..], uploaded[6 * VERTICES..]);

    assert!(
        gpu::build_tile(&shared, &formats, &model, &|| true).unwrap().is_none(),
        "cancelled"
    );
}

#[test]
fn the_textures_go_as_stored_when_the_device_takes_bc_shared_between_tiles_until_purged() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let mut shared = Shared::new(&gpu, &TARGET);
    let bc = gpu.device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
    assert_eq!(shared.block_compression, bc);
    let formats = Fake::default();
    let model = TileModel::new(id(32, 32), tile());
    let first = gpu::build_tile(&shared, &formats, &model, &|| false).unwrap().unwrap();
    let second = gpu::build_tile(&shared, &formats, &model, &|| false).unwrap().unwrap();
    let asked = (
        formats.stored.load(Ordering::Relaxed),
        formats.decoded.load(Ordering::Relaxed),
    );
    assert_eq!(
        asked,
        if bc { (2, 0) } else { (0, 2) },
        "each texture read once for both tiles"
    );
    // A block of DXT1 takes 8 bytes; 4 × 4 texels of RGBA in three levels, 84.
    assert_eq!(shared.textures.bytes(), if bc { 8 + 84 } else { 84 + 84 });
    drop((first, second));
    shared.textures.purge();
    assert_eq!(shared.textures.bytes(), 0, "no tile holds them any more");

    shared.block_compression = false;
    let formats = Fake::default();
    gpu::build_tile(&shared, &formats, &model, &|| false).unwrap().unwrap();
    assert_eq!(
        (
            formats.stored.load(Ordering::Relaxed),
            formats.decoded.load(Ordering::Relaxed)
        ),
        (0, 2),
        "without BC, decoded"
    );
}
