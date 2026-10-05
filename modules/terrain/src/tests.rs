//! Tests of the terrain on tiles the tests make, and on the device of the software adapter of the
//! system when there is one, skipped otherwise.

use std::collections::HashSet;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use uniwow_api::formats::{
    AreaRecord, Chunk, CreatureDisplay, CreatureModel, FileRef, Formats, Layer, MapRecord, Texture, TextureFormat,
    Tile, Wdl, Wdt,
};
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::viewport::{Layer as _, Target, View};
use uniwow_api::{bytemuck, egui, egui_wgpu, wgpu};

use crate::gpu::{self, Shared};
use crate::horizon;
use crate::layer::{Scene, TerrainLayer, in_sight, tiles_away};
use crate::loading::{self, Kept, Plan};
use crate::mesh::{self, CHUNKS, LODS, SKIRT_DEPTH, TILE_VERTICES, VERTICES, Vertex};
use crate::model::{CHUNK, ORIGIN, STEP, TILE, TileId, TileModel, chunk_bounds};
use crate::textures::{NONE, SLOTS};

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

/// Every vertex of the tile of `model`, those of the skirts after those of the chunks.
fn tile_vertices(model: &TileModel) -> Vec<Vertex> {
    let mut vertices: Vec<Vertex> = (0..CHUNKS)
        .flat_map(|place| mesh::vertices(model.id, place, &model.tile.chunks[place]))
        .collect();
    vertices.resize(TILE_VERTICES, vertices[0]);
    for place in 0..CHUNKS {
        for (first, skirt) in mesh::skirt(model.id, place, &model.tile.chunks[place]) {
            vertices[first..first + skirt.len()].copy_from_slice(&skirt);
        }
    }
    vertices
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
fn each_level_of_detail_faces_up_leaves_its_holes_out_and_hangs_its_skirts_outward() {
    let model = TileModel::new(TileId { x: 32, y: 32 }, tile());
    let vertices = tile_vertices(&model);
    let (indices, lods) = mesh::indices(&model.tile);
    assert_eq!(lods[0].start, 0);
    assert_eq!(lods[LODS - 1].end as usize, indices.len());
    // Without holes: 256, 128, 32 and 2 triangles a chunk; the skirts, a pair each step along the
    // 64 chunk sides of the tile. The hole of the chunk 9 takes 4 triangles, then 2; a block of
    // quads that are not all holes stays.
    let skirts = |step: u32| 64 * (8 / step) * 2;
    let expected = [
        256 * 256 - 4 + skirts(1),
        256 * 128 - 2 + skirts(1),
        256 * 32 + skirts(2),
        256 * 2 + skirts(8),
    ];
    let [centre_x, centre_y] = model.id.centre();
    for (lod, range) in lods.iter().enumerate() {
        assert_eq!(range.len() as u32 / 3, expected[lod], "level {lod}");
        for triangle in indices[range.start as usize..range.end as usize].as_chunks::<3>().0 {
            let [a, b, c] = triangle.map(|i| Vec3::from(vertices[usize::from(i)].position));
            let normal = (b - a).cross(c - a);
            if triangle.iter().any(|i| usize::from(*i) >= CHUNKS * VERTICES) {
                // A skirt: upright, facing away from the middle of the tile.
                let middle = (a + b + c) / 3.0;
                let out = Vec3::new(middle.x - centre_x, middle.y - centre_y, 0.0);
                assert!(
                    normal.z.abs() < 1e-3 && normal.dot(out) > 0.0,
                    "level {lod}: {triangle:?}"
                );
            } else {
                assert!(normal.z > 0.0, "level {lod}: {triangle:?} faces down");
            }
        }
    }
    // The inner vertex of the quad of the row 1, column 1 of the chunk 9 is left with its hole.
    assert!(!indices[..lods[0].end as usize].contains(&(9 * VERTICES as u16 + 27)));
    // A skirt vertex hangs under the vertex of the side it copies.
    let skirt = mesh::skirt(model.id, 0, &model.tile.chunks[0]);
    assert_eq!(skirt.len(), 2, "the corner chunk lies on two sides");
    let top = mesh::vertices(model.id, 0, &model.tile.chunks[0])[0];
    assert_eq!(skirt[0].1[0].position[2], top.position[2] - SKIRT_DEPTH);
    assert_eq!(&skirt[0].1[0].position[..2], &top.position[..2]);
}

#[test]
fn a_level_of_detail_follows_the_distance_and_changes_only_past_a_margin() {
    assert_eq!(mesh::lod(0.5, None), 0);
    assert_eq!(mesh::lod(2.0, None), 1);
    assert_eq!(mesh::lod(4.0, None), 2);
    assert_eq!(mesh::lod(10.0, None), 3);
    assert_eq!(mesh::lod(1.6, Some(0)), 0, "just past its limit: kept");
    assert_eq!(mesh::lod(1.7, Some(0)), 1);
    assert_eq!(mesh::lod(1.4, Some(1)), 1, "just back: kept");
    assert_eq!(mesh::lod(1.3, Some(1)), 0);
    assert_eq!(mesh::lod(7.0, Some(0)), 3, "far past: at once");
    let bounds = [[0.0, 0.0, 0.0], [TILE, TILE, 10.0]];
    assert_eq!(tiles_away(Vec3::new(10.0, 10.0, 5.0), bounds), 0.0, "inside");
    assert!((tiles_away(Vec3::new(3.0 * TILE, 0.0, 0.0), bounds) - 2.0).abs() < 1e-4);
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
fn the_budget_by_default_is_half_the_memory_of_the_gpu_when_told() {
    assert_eq!(crate::default_budget(Some(12 << 30)), 6144, "a GPU of 12 GB");
    assert_eq!(crate::default_budget(None), 1024, "not told");
    assert_eq!(crate::default_budget(Some(64 << 20)), 64, "never under the least");
    assert_eq!(crate::default_budget(Some(1 << 40)), 65_536, "nor over the most");
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

/// The height of a tile of a WDL at a row and a column.
type Height = Box<dyn Fn(usize, usize) -> i16>;

/// A WDL with the tiles `present`, the heights of each `height`.
fn wdl(present: &[((u32, u32), Height)]) -> Wdl {
    let mut tiles = vec![None; 4096];
    for ((x, y), height) in present {
        tiles[(y * 64 + x) as usize] = Some((0..17 * 17).map(|i| height(i / 17, i % 17)).collect());
    }
    Wdl { tiles }
}

#[test]
fn the_horizon_is_a_mesh_of_the_heights_of_the_wdl_facing_up_its_tiles_drawn_in_detail_masked() {
    let two = wdl(&[
        ((32, 48), Box::new(|row, _| row as i16 * 10)),
        ((5, 1), Box::new(|_, _| 3)),
    ]);
    let (vertices, indices) = horizon::mesh(&two);
    assert_eq!(vertices.len(), 2 * 17 * 17);
    assert_eq!(indices.len(), 2 * 16 * 16 * 6);
    for triangle in indices.as_chunks::<3>().0 {
        let [a, b, c] = triangle.map(|i| Vec3::from(vertices[i as usize].position));
        assert!((b - a).cross(c - a).z > 0.0, "{triangle:?} faces down");
    }
    // The tile 5 1 comes first, at y * 64 + x; then 32 48, its first height at its corner.
    assert_eq!((vertices[0].tile, vertices[289].tile), (64 + 5, 48 * 64 + 32));
    let corner = id(32, 48).corner();
    assert_eq!(vertices[289].position, [corner[0], corner[1], 0.0]);
    let last = vertices[289 + 288].position;
    assert!((last[0] - (corner[0] - TILE)).abs() < 1e-3 && (last[1] - (corner[1] - TILE)).abs() < 1e-3);
    assert_eq!(last[2], 160.0);
    // Rising row by row, down in X: the ground faces towards higher X.
    let normal = vertices[289 + 5 * 17 + 5].normal;
    assert!(normal[0] > 10 && normal[1] == 0 && normal[2] > 100, "{normal:?}");

    // Alone, the tile fades into the fog: all of it at its sides, two thirds in its middle, half a
    // tile from them; in the middle of a block of 7 × 7 tiles, not at all.
    assert_eq!(vertices[289].normal[3], 127);
    assert_eq!(vertices[289 + 8 * 17 + 8].normal[3], 85);
    let block: Vec<((u32, u32), Height)> = (0..49)
        .map(|i| ((10 + i % 7, 20 + i / 7), Box::new(|_, _| 0i16) as Height))
        .collect();
    let (block, _) = horizon::mesh(&wdl(&block));
    let middle = &block[24 * 289..25 * 289];
    assert_eq!(middle[8 * 17 + 8].normal[3], 0);
    assert!(
        block[..289].iter().all(|vertex| vertex.normal[3] > 0),
        "the corner tile fades"
    );

    let bits = horizon::mask([id(5, 1), id(32, 48), id(63, 63)].into_iter());
    assert_eq!(bits[2], 1 << 5, "the tile 69, the bit 5 of the word 2");
    assert_eq!(bits[(48 * 64 + 32) / 32], 1);
    assert_eq!(bits[127], 1 << 31);
    assert_eq!(bits.iter().map(|word| word.count_ones()).sum::<u32>(), 3);
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
    fn wdl(&self, _directory: &str) -> Result<Option<Wdl>, String> {
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
            FileRef::Path(path) if path == "red.blp" => plain(4, [255, 0, 0, 255]),
            FileRef::Path(path) if path == "green.blp" => plain(8, [0, 255, 0, 255]),
            _ => rgba(),
        })
    }
    fn texture_rgba(&self, _file: &FileRef) -> Result<Texture, String> {
        self.decoded.fetch_add(1, Ordering::Relaxed);
        Ok(rgba())
    }
}

/// A texture of RGBA `side` texels a side, all `colour`, in one level.
fn plain(side: u32, colour: [u8; 4]) -> Texture {
    Texture {
        width: side,
        height: side,
        format: TextureFormat::Rgba8,
        levels: vec![colour.repeat((side * side) as usize)],
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

/// The texels of the layer `layer` of the first level of the array `texture`, of RGBA 64 texels wide.
fn read_layer(gpu: &egui_wgpu::RenderState, texture: &wgpu::Texture, layer: u32) -> Vec<u8> {
    let rows = texture.height();
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read back"),
        size: u64::from(256 * rows),
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(rows),
            },
        },
        wgpu::Extent3d {
            width: 64,
            height: rows,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read_back(gpu, &buffer)
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
    assert!(built.bounds[1][0] <= ORIGIN - TILE * 60.0 + 1e-3, "placed by its tile");
    assert_eq!(built.bounds[0][2], 7.0 - SKIRT_DEPTH, "its skirts within its bounds");
    // Nothing drawn: the uploads were submitted by the job itself.
    let uploaded: Vec<Vertex> = bytemuck::cast_slice(&read_back(&gpu, &built.vertices)).to_vec();
    assert_eq!(uploaded, tile_vertices(&model));

    model.tile.chunks[5].heights[0] += 10.0;
    model.mark_changed(5);
    assert!(model.changed() && model.is_changed(5) && !model.is_changed(6));
    gpu::write_chunk(&shared.queue, &built.vertices, &built.blend, &model, 5);
    let rebuilt: Vec<Vertex> = bytemuck::cast_slice(&read_back(&gpu, &built.vertices)).to_vec();
    assert_eq!(
        rebuilt,
        tile_vertices(&model),
        "the chunk 5 and its skirt written again alone"
    );
    assert_ne!(rebuilt[5 * VERTICES], uploaded[5 * VERTICES]);
    assert_eq!(
        rebuilt[6 * VERTICES..CHUNKS * VERTICES],
        uploaded[6 * VERTICES..CHUNKS * VERTICES]
    );

    assert!(
        gpu::build_tile(&shared, &formats, &model, &|| true).unwrap().is_none(),
        "cancelled"
    );
}

#[test]
fn the_textures_are_layers_of_arrays_by_class_read_once_for_all_tiles_and_dropped_once_unheld() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let shared = Shared::new(&gpu, &TARGET);
    let bc = shared.textures.block_compression();
    assert_eq!(
        bc,
        gpu.device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    );
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
    // Two classes with BC (a block of DXT1 in one level, RGBA in three), one without; four layers an
    // array at first.
    let a = shared
        .textures
        .get(&formats, &FileRef::Path("a.blp".to_owned()))
        .unwrap();
    let b = shared
        .textures
        .get(&formats, &FileRef::Path("b.blp".to_owned()))
        .unwrap();
    assert_eq!(a.slot != b.slot, bc);
    assert_eq!(shared.textures.bytes(), if bc { 4 * 8 + 4 * 84 } else { 4 * 84 });
    let codes = gpu::layer_codes(&model, &[Some(a.clone()), Some(b.clone())]);
    assert_eq!(
        codes[0],
        [a.code(), b.code(), NONE, NONE],
        "the third names no texture of the tile"
    );
    drop((first, second, a, b));
    shared.textures.purge();
    assert_eq!(shared.textures.bytes(), 0, "no tile holds them: the arrays dropped");
}

#[test]
fn an_array_grows_keeping_its_layers_and_the_arrays_end_with_their_slots() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let shared = Shared::new(&gpu, &TARGET);
    let generation = shared.textures.generation();
    let placed: Vec<_> = (0..5u8)
        .map(|n| shared.textures.place(&plain(64, [n * 40, 1, 2, 255])).unwrap())
        .collect();
    assert!(placed.iter().all(|p| p.slot == placed[0].slot), "one class, one array");
    assert_eq!(placed.iter().map(|p| p.layer).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
    let (texture, layers) = shared.textures.array(placed[0].slot).unwrap();
    assert_eq!(layers, 8, "grown from four to eight");
    assert!(shared.textures.generation() > generation);
    for (n, placed) in placed.iter().enumerate() {
        let texels = read_layer(&gpu, &texture, placed.layer);
        assert_eq!(
            &texels[..4],
            &[n as u8 * 40, 1, 2, 255],
            "the layer {n} kept by the growth"
        );
    }
    // A class each, until every slot holds an array.
    let others: Vec<_> = (1..SLOTS as u32)
        .map(|n| shared.textures.place(&plain(4 * n, [9; 4])))
        .collect();
    assert!(others.iter().all(Result::is_ok));
    let refused = shared.textures.place(&plain(4 * SLOTS as u32, [9; 4])).err().unwrap();
    assert!(refused.contains("full"), "{refused}");
}

/// A tile of chunks with one texture each, `red.blp` (4 texels a side) for the columns of chunks
/// below 8, `green.blp` (8 a side, another class) for the others; flat at the height 7, unshadowed.
fn two_textures() -> Tile {
    let mut tile = tile();
    tile.textures = vec![
        FileRef::Path("red.blp".to_owned()),
        FileRef::Path("green.blp".to_owned()),
    ];
    for chunk in &mut tile.chunks {
        chunk.holes = 0;
        chunk.heights = vec![0.0; 145];
        chunk.normals = vec![[0, 0, 127]; 145];
        chunk.layers = vec![Layer {
            texture: u32::from(chunk.index[0] >= 8),
            flags: 0,
            effect: 0,
        }];
        chunk.alphas.clear();
        chunk.shadow.clear();
    }
    tile
}

/// What the layer draws seen from `eye` looking at `target`, into 64 × 64 pixels of RGBA, cleared to
/// black: the pixels, a row after the other.
fn render(gpu: &egui_wgpu::RenderState, layer: &mut TerrainLayer, target: &Target, eye: Vec3, look: Vec3) -> Vec<u8> {
    let size = [64u32, 64];
    let view = View {
        view_proj: Mat4::perspective_infinite_reverse_rh(90f32.to_radians(), 1.0, 0.1)
            * Mat4::look_at_rh(eye, look, Vec3::Z),
        eye,
        size,
        time: 0.0,
    };
    layer.prepare(gpu, &view);
    let mut bundle = gpu
        .device
        .create_render_bundle_encoder(&wgpu::RenderBundleEncoderDescriptor {
            label: None,
            color_formats: &[Some(target.color_format)],
            depth_stencil: Some(wgpu::RenderBundleDepthStencil {
                format: target.depth_format,
                depth_read_only: false,
                stencil_read_only: true,
            }),
            sample_count: 1,
            multiview: None,
        });
    layer.draw(gpu, target, &view, &mut bundle);
    let bundle = bundle.finish(&wgpu::RenderBundleDescriptor { label: None });
    let texture = |format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
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
        target.color_format,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let depth = texture(target.depth_format, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
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
        pass.execute_bundles([&bundle]);
    }
    gpu.queue.submit([encoder.finish()]);
    read_layer(gpu, &colour, 0)
}

/// How many of the 64 × 64 `pixels` are mostly red, mostly green, mostly blue, of the ground of the
/// horizon (red and green alike, more than blue), and black.
fn counts(pixels: &[u8]) -> [usize; 5] {
    let mut counts = [0; 5];
    for pixel in pixels.as_chunks::<4>().0 {
        let [r, g, b, _] = pixel.map(i32::from);
        let kind = if r + g + b < 10 {
            4
        } else if r > 2 * g && r > 2 * b {
            0
        } else if g > 2 * r && g > 2 * b {
            1
        } else if b > r && b > g {
            2
        } else {
            3
        };
        counts[kind] += 1;
    }
    counts
}

#[test]
fn a_tile_is_one_draw_its_chunks_textured_from_two_arrays_and_the_horizon_beyond_it_masked() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let target = Target {
        sample_count: 1,
        ..TARGET
    };
    let shared = Arc::new(Shared::new(&gpu, &target));
    let formats = Fake::default();
    let model = TileModel::new(id(32, 32), two_textures());
    let tile = Arc::new(gpu::build_tile(&shared, &formats, &model, &|| false).unwrap().unwrap());
    // The WDL of the tile, above it, which would hide it unless left out; and of the tiles around
    // it, 7 × 7, at the ground, so that its neighbours do not fade into the fog.
    let block: Vec<((u32, u32), Height)> = (0..49)
        .map(|i| {
            let tile = (29 + i % 7, 29 + i / 7);
            let height: Height = if tile == (32, 32) {
                Box::new(|_, _| 100)
            } else {
                Box::new(|_, _| 0)
            };
            (tile, height)
        })
        .collect();
    let horizon = Arc::new(horizon::build(&shared, &wdl(&block)).unwrap());
    let scene = Arc::new(Mutex::new(Scene {
        tiles: vec![tile.clone()],
        horizon: Some(horizon.clone()),
        reach: 100_000.0,
        ..Scene::default()
    }));
    let mut layer = TerrainLayer::new(Arc::new(Mutex::new(Some(shared.clone()))), scene.clone());

    // Above the middle of the tile: its two halves, each of its texture.
    let [x, y] = id(32, 32).centre();
    let above = render(
        &gpu,
        &mut layer,
        &target,
        Vec3::new(x + 1.0, y, 250.0),
        Vec3::new(x, y, 0.0),
    );
    let [red, green, _, ground, _] = counts(&above);
    assert!(red > 1200 && green > 1200, "red {red}, green {green}, ground {ground}");
    let stats = layer.stats();
    assert_eq!(stats.draws, 2, "the tile in one draw, and the horizon");
    assert_eq!(
        stats.triangles,
        u64::from(tile.lods[0].len() as u32 / 3 + horizon.count / 3),
        "the tile at its finest, near"
    );

    // From high above both: the neighbour drawn as the horizon, the tile still seen through its own.
    let wide = render(
        &gpu,
        &mut layer,
        &target,
        Vec3::new(x + 1.0, 0.0, 1200.0),
        Vec3::new(x, 0.0, 0.0),
    );
    let [red, green, _, ground, _] = counts(&wide);
    // A tile is some 200 pixels from there; the neighbour as many.
    assert!(
        red > 80 && green > 80 && ground > 150,
        "red {red}, green {green}, ground {ground}"
    );

    // A map shown: the sky in the colour of the fog where nothing is drawn.
    scene.lock().unwrap().map = Some([[-10.0 * TILE; 2], [10.0 * TILE; 2]]);
    let sky = render(
        &gpu,
        &mut layer,
        &target,
        Vec3::new(x, y, 50.0),
        Vec3::new(x + 100.0, y, 60.0),
    );
    let [_, _, blue, _, black] = counts(&sky);
    assert!(blue > 500 && black == 0, "sky {blue}, black {black}");
    assert_eq!(layer.stats().draws, 3, "and the sky");
}

#[test]
fn the_bundle_is_recorded_again_when_the_level_of_a_tile_changes_and_kept_otherwise() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let target = Target {
        sample_count: 1,
        ..TARGET
    };
    let shared = Arc::new(Shared::new(&gpu, &target));
    let model = TileModel::new(id(32, 32), two_textures());
    let tile = Arc::new(
        gpu::build_tile(&shared, &Fake::default(), &model, &|| false)
            .unwrap()
            .unwrap(),
    );
    let scene = Arc::new(Mutex::new(Scene {
        tiles: vec![tile],
        reach: 100_000.0,
        ..Scene::default()
    }));
    let mut layer = TerrainLayer::new(Arc::new(Mutex::new(Some(shared))), scene);
    let [x, y] = id(32, 32).centre();
    let mut version_from = |height: f32| {
        render(
            &gpu,
            &mut layer,
            &target,
            Vec3::new(x + 1.0, y, height),
            Vec3::new(x, y, 0.0),
        );
        (layer.version(), layer.stats().triangles)
    };
    let near = version_from(250.0);
    assert_eq!(version_from(260.0).0, near.0, "the same tiles at the same level: kept");
    let far = version_from(10.0 * TILE);
    assert_ne!(far.0, near.0, "farther, at a coarser level");
    assert!(far.1 < near.1 / 10, "{} triangles, then {}", near.1, far.1);
}
