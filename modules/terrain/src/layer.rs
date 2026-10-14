//! The layers of the terrain in the 3D view: the sky painted first, before the world, then the tiles
//! in sight a draw each at their level of detail; and the horizon in one draw, its tiles drawn in
//! detail left out, painted over the sky, its models and its bodies, in a layer of its own
//! (`HorizonLayer`, `Stage::Horizon`). Drawn in the pass at each frame: the tiles in sight by blocks
//! of `BLOCK` × `BLOCK` tiles of the map, each block a render bundle of its own, kept while the tiles
//! of the block in sight, their levels and the arrays of textures stay the same, and recorded again
//! alone once one of them changes; the sky and the horizon drawn in the pass as they are. The
//! camera, which moves at every frame, is written in `prepare` into its buffer, which the bundles
//! and the horizon read.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use uniwow_api::glam::{Mat3, Mat4, Vec3, Vec4};
use uniwow_api::journal;
use uniwow_api::viewport::{Drawing, Layer, LayerStats, Phase, Stage, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::gpu::SLOTS;
use crate::gpu::{CAMERA, Shared, TileGpu};
use crate::horizon::{self, HorizonGpu};
use crate::loading::Kind;
use crate::mesh;
use crate::model::{TILE, TileId};

/// What the module hands to its layer: the tiles, the horizon and the map shown, and what its
/// statistics say.
#[derive(Default)]
pub struct Scene {
    pub tiles: Vec<Arc<TileGpu>>,
    /// Counts the changes of `tiles` and of `horizon`.
    pub generation: u64,
    pub horizon: Option<Arc<HorizonGpu>>,
    /// The bounds of the map shown on the ground, its lowest corner then its highest; none before a
    /// map is shown.
    pub map: Option<[[f32; 2]; 2]>,
    /// How far around the camera the tiles load, on the ground, in yards.
    pub reach: f32,
    /// When the budget holds fewer tiles than are wanted, the reach it leaves, in tiles.
    pub limited: Option<f32>,
    /// What the module spent steering at the last frame, what the terrain takes on the GPU, and
    /// what the models of its full tiles take in memory.
    pub steering: Duration,
    pub bytes: u64,
    pub models: u64,
}

pub fn lock<T>(shared: &Mutex<T>) -> MutexGuard<'_, T> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the layer of the terrain hands to that of the horizon at each frame: what the terrain
/// shares, its pipeline among it, the bind groups of the camera and of the tiles drawn in detail,
/// and the horizon of the map shown.
#[derive(Clone, Default)]
pub struct HorizonFrame {
    pub shared: Option<Arc<Shared>>,
    pub camera: Option<wgpu::BindGroup>,
    pub mask: Option<wgpu::BindGroup>,
    pub horizon: Option<Arc<HorizonGpu>>,
}

/// The horizon of the map in the colour of the fog, painted over the sky, its models, the sun, the
/// moon and the clouds, before the world, as Noggit paints it: what the layer of the terrain
/// prepared for the frame.
pub struct HorizonLayer {
    frame: Arc<Mutex<HorizonFrame>>,
    stats: LayerStats,
}

impl Layer for HorizonLayer {
    fn stage(&self) -> Stage {
        Stage::Horizon
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
        if phase != Phase::Sky {
            return;
        }
        // Taken once the layer of the terrain has prepared the frame, which every layer does before
        // any draws.
        let frame = lock(&self.frame).clone();
        let (Some(shared), Some(camera), Some(mask), Some(horizon)) =
            (frame.shared, frame.camera, frame.mask, frame.horizon)
        else {
            self.stats = LayerStats::default();
            return;
        };
        self.stats = LayerStats {
            draws: 1,
            triangles: u64::from(horizon.count / 3),
            items: "the horizon".to_owned(),
            ..LayerStats::default()
        };
        pass.set_pipeline(&shared.horizon_pipeline);
        pass.set_bind_group(0, &camera, &[]);
        pass.set_bind_group(1, &mask, &[]);
        pass.set_vertex_buffer(0, horizon.vertices.slice(..));
        pass.set_index_buffer(horizon.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..horizon.count, 0, 0..1);
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}

/// Whether the box `bounds` lies wholly beyond the end of the fog of `view`, past which nothing is
/// drawn (`beyond_fog` of the shaders): its nearest corner, along the view, past the end of the fog
/// of the lights under the game's fog, never short of its end; its nearest point on the ground past
/// the end of the editor's.
pub fn wholly_beyond_fog(view: &View, bounds: [[f32; 3]; 2]) -> bool {
    let fog = view.fog;
    if fog.rate > 0.0 {
        let nearest = (0..8)
            .map(|i| {
                let pick = |axis: usize| bounds[(i >> axis) & 1][axis];
                (view.view_proj * Vec4::new(pick(0), pick(1), pick(2), 1.0)).w
            })
            .fold(f32::INFINITY, f32::min);
        return nearest > fog.middle.max(fog.end);
    }
    let outside = |axis: usize| {
        let eye = view.eye[axis];
        (bounds[0][axis] - eye).max(eye - bounds[1][axis]).max(0.0)
    };
    outside(0).hypot(outside(1)) > fog.end
}

/// Whether the box `bounds` may be in sight through `view_proj`: no side of the view has its
/// eight corners all beyond it, nor all behind the eye.
pub fn in_sight(view_proj: Mat4, bounds: [[f32; 3]; 2]) -> bool {
    let corners: [Vec4; 8] = std::array::from_fn(|i| {
        let pick = |axis: usize| bounds[(i >> axis) & 1][axis];
        view_proj * Vec4::new(pick(0), pick(1), pick(2), 1.0)
    });
    let all = |beyond: &dyn Fn(&Vec4) -> bool| corners.iter().all(beyond);
    !(all(&|c| c.w <= 0.0)
        || all(&|c| c.x < -c.w)
        || all(&|c| c.x > c.w)
        || all(&|c| c.y < -c.w)
        || all(&|c| c.y > c.w))
}

/// How far `eye` is from the nearest point of the box `bounds`, in tiles.
pub fn tiles_away(eye: Vec3, bounds: [[f32; 3]; 2]) -> f32 {
    let [low, high] = bounds.map(Vec3::from);
    let outside = (low - eye).max(eye - high).max(Vec3::ZERO);
    outside.length() / TILE
}

/// The farthest corner of `map` from `eye`, on the ground.
pub fn farthest(eye: Vec3, map: [[f32; 2]; 2]) -> f32 {
    (0..4)
        .map(|corner| {
            let x = map[corner & 1][0] - eye.x;
            let y = map[corner >> 1][1] - eye.y;
            (x * x + y * y).sqrt()
        })
        .fold(0.0, f32::max)
}

/// The camera of the shaders: the view, the sun, the fog and the sky of `view`, the direction a
/// place on the screen is seen in from the eye by the inverse of what `view_proj` does to a direction,
/// its columns of x, y and w without its translation, so that it keeps its precision anywhere.
pub fn camera_values(view: &View) -> [f32; CAMERA] {
    let mut values = [0f32; CAMERA];
    values[..16].copy_from_slice(&view.view_proj.to_cols_array());
    values[16..19].copy_from_slice(&view.sun.direction);
    values[20..23].copy_from_slice(&view.sun.colour);
    values[24..27].copy_from_slice(&view.sun.ambient);
    values[28..31].copy_from_slice(&view.eye.to_array());
    values[32..35].copy_from_slice(&view.fog.colour);
    values[36..39].copy_from_slice(&[view.fog.start, view.fog.middle, view.fog.end]);
    values[39] = view.fog.rate;
    let [x, y, z, _] = view.view_proj.to_cols_array_2d();
    let seen = |column: [f32; 4]| Vec3::new(column[0], column[1], column[3]);
    let sky_from_screen = Mat3::from_cols(seen(x), seen(y), seen(z)).inverse();
    values[40..56].copy_from_slice(&Mat4::from_mat3(sky_from_screen).to_cols_array());
    if let Some(sky) = view.sky {
        for (at, colour) in sky.iter().enumerate() {
            values[56 + 4 * at..59 + 4 * at].copy_from_slice(colour);
            values[59 + 4 * at] = 1.0;
        }
    }
    values
}

/// The side of a block of tiles drawn by one bundle, in tiles of the map.
pub const BLOCK: u32 = 4;

/// The block of the tile `id`.
pub fn block_of(id: TileId) -> [u32; 2] {
    [id.x / BLOCK, id.y / BLOCK]
}

/// The bundle of a block of tiles: what it was recorded with, the tiles of the block in sight, by
/// their place, with their levels, and the generation of the arrays of textures; the tiles, held so
/// that no other takes their place meanwhile.
struct Block {
    recorded: (Vec<(usize, usize)>, u64),
    tiles: Vec<Arc<TileGpu>>,
    bundle: wgpu::RenderBundle,
}

/// What the tiles `tiles` in sight with their levels are recorded with, the arrays of textures of
/// the generation `arrays`: each tile by the place of its resources, so that a tile loaded again
/// is another.
fn recorded_with(tiles: &[(Arc<TileGpu>, usize)], arrays: u64) -> (Vec<(usize, usize)>, u64) {
    let tiles = tiles
        .iter()
        .map(|(tile, lod)| (Arc::as_ptr(tile) as usize, *lod))
        .collect();
    (tiles, arrays)
}

/// The bundle drawing `tiles` at their levels, for the target of `shared`.
fn record(
    shared: &Shared,
    camera: &wgpu::BindGroup,
    arrays: &wgpu::BindGroup,
    tiles: &[(Arc<TileGpu>, usize)],
) -> wgpu::RenderBundle {
    let target = &shared.target;
    let mut bundle = shared
        .device
        .create_render_bundle_encoder(&wgpu::RenderBundleEncoderDescriptor {
            label: Some("terrain block"),
            color_formats: &[Some(target.color_format)],
            depth_stencil: Some(wgpu::RenderBundleDepthStencil {
                format: target.depth_format,
                depth_read_only: false,
                stencil_read_only: true,
            }),
            sample_count: target.sample_count,
            multiview: None,
        });
    bundle.set_pipeline(&shared.pipeline);
    bundle.set_bind_group(0, camera, &[]);
    bundle.set_bind_group(2, arrays, &[]);
    for (tile, lod) in tiles {
        bundle.set_bind_group(1, &tile.tile_group, &[]);
        bundle.set_vertex_buffer(0, tile.vertices.slice(..));
        bundle.set_index_buffer(tile.indices.slice(..), wgpu::IndexFormat::Uint16);
        bundle.draw_indexed(tile.lods[*lod].clone(), 0, 0..1);
    }
    bundle.finish(&wgpu::RenderBundleDescriptor {
        label: Some("terrain block"),
    })
}

pub struct TerrainLayer {
    /// Where the job building the pipelines leaves what the terrain shares.
    incoming: Arc<Mutex<Option<Arc<Shared>>>>,
    shared: Option<Arc<Shared>>,
    scene: Arc<Mutex<Scene>>,
    camera: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    /// The bits of the tiles drawn in detail, and the generation of the scene they were written for.
    mask: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    masked: Option<u64>,
    /// The bind group of the arrays of textures, and their generation.
    arrays: Option<(u64, wgpu::BindGroup)>,
    /// The tiles in sight with their levels of detail, and whether a map is shown.
    drawn: Vec<(Arc<TileGpu>, usize)>,
    sky: bool,
    /// What the layer of the horizon draws at each frame.
    horizon: Arc<Mutex<HorizonFrame>>,
    /// The level of each tile in sight at the last frame.
    lods: HashMap<TileId, usize>,
    /// The bundles of the blocks, kept while their tiles are in the scene, those of the blocks in
    /// sight at this frame, the generation of the scene the blocks were last let go for, and the
    /// bundles recorded at this frame and since the layer was made.
    blocks: HashMap<[u32; 2], Block>,
    shown: Vec<[u32; 2]>,
    kept_for: Option<u64>,
    recorded_now: usize,
    recordings: u64,
    stats: LayerStats,
}

impl TerrainLayer {
    pub fn new(incoming: Arc<Mutex<Option<Arc<Shared>>>>, scene: Arc<Mutex<Scene>>) -> Self {
        Self {
            incoming,
            shared: None,
            scene,
            camera: None,
            mask: None,
            masked: None,
            arrays: None,
            drawn: Vec::new(),
            sky: false,
            horizon: Arc::default(),
            lods: HashMap::new(),
            blocks: HashMap::new(),
            shown: Vec::new(),
            kept_for: None,
            recorded_now: 0,
            recordings: 0,
            stats: LayerStats::default(),
        }
    }

    /// The layer of the horizon this one prepares the frames of.
    pub fn horizon_layer(&self) -> HorizonLayer {
        HorizonLayer {
            frame: self.horizon.clone(),
            stats: LayerStats::default(),
        }
    }

    /// How many bundles of blocks it recorded since it was made, and how many it keeps.
    #[cfg(test)]
    pub fn bundles(&self) -> (u64, usize) {
        (self.recordings, self.blocks.len())
    }

    /// The bundles of the blocks of the tiles in sight, those recorded again first, where a tile in
    /// sight, its level or the arrays of textures changed; those out of sight recorded with other
    /// arrays let go.
    fn record_blocks(&mut self, shared: &Shared) {
        let (Some((_, camera)), Some((arrays, arrays_group))) = (&self.camera, &self.arrays) else {
            return;
        };
        let mut by_block: BTreeMap<[u32; 2], Vec<(Arc<TileGpu>, usize)>> = BTreeMap::new();
        for (tile, lod) in &self.drawn {
            by_block
                .entry(block_of(tile.id))
                .or_default()
                .push((tile.clone(), *lod));
        }
        self.shown.clear();
        self.recorded_now = 0;
        for (key, tiles) in by_block {
            let recorded = recorded_with(&tiles, *arrays);
            if self.blocks.get(&key).is_none_or(|block| block.recorded != recorded) {
                let bundle = record(shared, camera, arrays_group, &tiles);
                let tiles = tiles.into_iter().map(|(tile, _)| tile).collect();
                self.blocks.insert(
                    key,
                    Block {
                        recorded,
                        tiles,
                        bundle,
                    },
                );
                self.recorded_now += 1;
                self.recordings += 1;
            }
            self.shown.push(key);
        }
        // The blocks out of sight recorded with the arrays of textures before let go: their
        // bundles would keep those arrays on the GPU once grown or purged.
        let arrays = *arrays;
        self.blocks.retain(|_, block| block.recorded.1 == arrays);
    }

    /// A uniform buffer of `size` bytes and its bind group of `layout`.
    fn uniform(
        shared: &Shared,
        label: &str,
        size: usize,
        layout: &wgpu::BindGroupLayout,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buffer = shared.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = shared.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        (buffer, group)
    }
}

impl Layer for TerrainLayer {
    fn stage(&self) -> Stage {
        Stage::Ground
    }

    fn drawing(&self) -> Drawing {
        Drawing::Pass
    }

    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        if self.shared.is_none() {
            self.shared = lock(&self.incoming).clone();
        }
        let Some(shared) = self.shared.clone() else {
            return;
        };
        let (camera, _) = self
            .camera
            .get_or_insert_with(|| Self::uniform(&shared, "terrain camera", CAMERA * 4, &shared.camera_layout));
        let (mask, _) = self
            .mask
            .get_or_insert_with(|| Self::uniform(&shared, "terrain horizon", 128 * 4, &shared.mask_layout));
        let arrays = shared.textures.generation();
        if self.arrays.as_ref().is_none_or(|(generation, _)| *generation != arrays) {
            let (generation, views) = shared.textures.views();
            self.arrays = Some((generation, shared.arrays_group(&views)));
        }

        let scene = journal::lock(&self.scene, "terrain scene");
        let lods = std::mem::take(&mut self.lods);
        self.drawn = scene
            .tiles
            .iter()
            .filter(|tile| in_sight(view.view_proj, tile.bounds) && !wholly_beyond_fog(view, tile.bounds))
            .map(|tile| {
                let lod = mesh::lod(tiles_away(view.eye, tile.bounds), lods.get(&tile.id).copied());
                (tile.clone(), lod)
            })
            .collect();
        for (tile, lod) in &self.drawn {
            self.lods.insert(tile.id, *lod);
        }
        // The blocks holding a tile the scene no longer has let go, its resources with them.
        if self.kept_for != Some(scene.generation) {
            let held: HashSet<*const TileGpu> = scene.tiles.iter().map(Arc::as_ptr).collect();
            self.blocks
                .retain(|_, block| block.tiles.iter().all(|tile| held.contains(&Arc::as_ptr(tile))));
            self.kept_for = Some(scene.generation);
        }
        if self.masked != Some(scene.generation) {
            let bits = horizon::mask(scene.tiles.iter().map(|tile| tile.id));
            gpu.queue.write_buffer(mask, 0, bytemuck::cast_slice(&bits));
            self.masked = Some(scene.generation);
        }
        let horizon = scene.horizon.clone();
        self.sky = scene.map.is_some();
        gpu.queue
            .write_buffer(camera, 0, bytemuck::cast_slice(&camera_values(view)));
        *lock(&self.horizon) = HorizonFrame {
            shared: Some(shared.clone()),
            camera: self.camera.as_ref().map(|(_, group)| group.clone()),
            mask: self.mask.as_ref().map(|(_, group)| group.clone()),
            horizon,
        };

        let tile_triangles: u64 = self
            .drawn
            .iter()
            .map(|(tile, lod)| u64::from(tile.lods[*lod].len() as u32 / 3))
            .sum();
        let full = scene.tiles.iter().filter(|tile| tile.kind == Kind::Full).count();
        let textures = shared.textures.counts();
        let limited = scene.limited.map_or_else(String::new, |reach| {
            format!("; reach limited by the budget: {reach:.0} tiles")
        });
        let (steering, bytes, models, loaded) = (scene.steering, scene.bytes, scene.models, scene.tiles.len());
        drop(scene);
        self.record_blocks(&shared);
        self.stats = LayerStats {
            draws: self.drawn.len() as u64 + u64::from(self.sky),
            triangles: tile_triangles + u64::from(self.sky),
            bytes,
            items: format!(
                "{} tiles in sight of {loaded} loaded ({full} full, {} light), levels {:?}, in {} blocks, {} recorded again\n  models {:.0} MB in memory; textures {} placed, {} unreadable, {} waiting for room, {} of {} arrays{limited}",
                self.drawn.len(),
                loaded - full,
                (0..mesh::LODS)
                    .map(|level| self.drawn.iter().filter(|(_, lod)| *lod == level).count())
                    .collect::<Vec<_>>(),
                self.shown.len(),
                self.recorded_now,
                models as f64 / (1024.0 * 1024.0),
                textures.placed,
                textures.unreadable,
                textures.no_room,
                textures.arrays,
                SLOTS
            ),
            steering,
        };
    }

    fn draw_pass(
        &mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        phase: Phase,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        let (Some(shared), Some((_, camera))) = (&self.shared, &self.camera) else {
            return;
        };
        // The sky first of all, before the world; the tiles in the opaque phase, over it.
        match phase {
            Phase::Sky if self.sky => {
                pass.set_pipeline(&shared.sky_pipeline);
                pass.set_bind_group(0, camera, &[]);
                pass.draw(0..3, 0..1);
            }
            Phase::Opaque => {
                let blocks = &self.blocks;
                pass.execute_bundles(
                    self.shown
                        .iter()
                        .filter_map(|key| blocks.get(key).map(|block| &block.bundle)),
                );
            }
            _ => {}
        }
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
