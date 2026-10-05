//! The layer of the terrain in the 3D view: the tiles in sight a draw each at their level of detail,
//! the horizon in one draw, its tiles drawn in detail left out, and last the sky in the colour of the
//! fog, where nothing was drawn. Its bundle is kept while the tiles drawn, their levels and the arrays of textures stay the
//! same; the camera, which moves at every frame, is written in `prepare` into its buffer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use uniwow_api::glam::{Mat4, Vec3, Vec4};
use uniwow_api::viewport::{Layer, LayerStats, Target, View};
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

/// The camera of the shaders: the view, the sun and the fog of `view`.
pub fn camera_values(view: &View) -> [f32; CAMERA] {
    let mut values = [0f32; CAMERA];
    values[..16].copy_from_slice(&view.view_proj.to_cols_array());
    values[16..19].copy_from_slice(&view.sun.direction);
    values[20..23].copy_from_slice(&view.sun.colour);
    values[24..27].copy_from_slice(&view.sun.ambient);
    values[28..31].copy_from_slice(&view.eye.to_array());
    values[32..35].copy_from_slice(&view.fog.colour);
    values[36..39].copy_from_slice(&[view.fog.start, view.fog.middle, view.fog.end]);
    values
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
    /// The tiles in sight with their levels of detail, the horizon and whether a map is shown.
    drawn: Vec<(Arc<TileGpu>, usize)>,
    horizon: Option<Arc<HorizonGpu>>,
    sky: bool,
    /// The level of each tile in sight at the last frame.
    lods: HashMap<TileId, usize>,
    /// What the bundle was recorded with: the generation of the scene, that of the arrays, and the
    /// tiles in sight with their levels.
    recorded: (u64, u64, Vec<(TileId, usize)>),
    version: u64,
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
            horizon: None,
            sky: false,
            lods: HashMap::new(),
            recorded: (u64::MAX, u64::MAX, Vec::new()),
            version: 0,
            stats: LayerStats::default(),
        }
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

        let scene = lock(&self.scene);
        let lods = std::mem::take(&mut self.lods);
        self.drawn = scene
            .tiles
            .iter()
            .filter(|tile| in_sight(view.view_proj, tile.bounds))
            .map(|tile| {
                let lod = mesh::lod(tiles_away(view.eye, tile.bounds), lods.get(&tile.id).copied());
                (tile.clone(), lod)
            })
            .collect();
        for (tile, lod) in &self.drawn {
            self.lods.insert(tile.id, *lod);
        }
        if self.masked != Some(scene.generation) {
            let bits = horizon::mask(scene.tiles.iter().map(|tile| tile.id));
            gpu.queue.write_buffer(mask, 0, bytemuck::cast_slice(&bits));
            self.masked = Some(scene.generation);
        }
        self.horizon = scene.horizon.clone();
        self.sky = scene.map.is_some();
        gpu.queue
            .write_buffer(camera, 0, bytemuck::cast_slice(&camera_values(view)));

        let recorded = (
            scene.generation,
            arrays,
            self.drawn.iter().map(|(tile, lod)| (tile.id, *lod)).collect(),
        );
        if recorded != self.recorded {
            self.recorded = recorded;
            self.version += 1;
        }
        let tile_triangles: u64 = self
            .drawn
            .iter()
            .map(|(tile, lod)| u64::from(tile.lods[*lod].len() as u32 / 3))
            .sum();
        let horizon_triangles = self.horizon.as_ref().map_or(0, |horizon| u64::from(horizon.count / 3));
        let full = scene.tiles.iter().filter(|tile| tile.kind == Kind::Full).count();
        let textures = shared.textures.counts();
        let limited = scene.limited.map_or_else(String::new, |reach| {
            format!("; reach limited by the budget: {reach:.0} tiles")
        });
        self.stats = LayerStats {
            draws: self.drawn.len() as u64 + u64::from(self.horizon.is_some()) + u64::from(self.sky),
            triangles: tile_triangles + horizon_triangles + u64::from(self.sky),
            bytes: scene.bytes,
            items: format!(
                "{} tiles in sight of {} loaded ({full} full, {} light), levels {:?}\n  models {:.0} MB in memory; textures {} placed, {} unreadable, {} waiting for room, {} of {} arrays{limited}",
                self.drawn.len(),
                scene.tiles.len(),
                scene.tiles.len() - full,
                (0..mesh::LODS)
                    .map(|level| self.drawn.iter().filter(|(_, lod)| *lod == level).count())
                    .collect::<Vec<_>>(),
                scene.models as f64 / (1024.0 * 1024.0),
                textures.placed,
                textures.unreadable,
                textures.no_room,
                textures.arrays,
                SLOTS
            ),
            steering: scene.steering,
        };
    }

    fn version(&self) -> Option<u64> {
        Some(self.version)
    }

    fn draw<'a>(
        &'a mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    ) {
        let (Some(shared), Some((_, camera)), Some((_, mask)), Some((_, arrays))) =
            (&self.shared, &self.camera, &self.mask, &self.arrays)
        else {
            return;
        };
        if !self.drawn.is_empty() {
            bundle.set_pipeline(&shared.pipeline);
            bundle.set_bind_group(0, camera, &[]);
            bundle.set_bind_group(2, arrays, &[]);
            for (tile, lod) in &self.drawn {
                bundle.set_bind_group(1, &tile.tile_group, &[]);
                bundle.set_vertex_buffer(0, tile.vertices.slice(..));
                bundle.set_index_buffer(tile.indices.slice(..), wgpu::IndexFormat::Uint16);
                bundle.draw_indexed(tile.lods[*lod].clone(), 0, 0..1);
            }
        }
        if let Some(horizon) = &self.horizon {
            bundle.set_pipeline(&shared.horizon_pipeline);
            bundle.set_bind_group(0, camera, &[]);
            bundle.set_bind_group(1, mask, &[]);
            bundle.set_vertex_buffer(0, horizon.vertices.slice(..));
            bundle.set_index_buffer(horizon.indices.slice(..), wgpu::IndexFormat::Uint32);
            bundle.draw_indexed(0..horizon.count, 0, 0..1);
        }
        // Last, where nothing was drawn: the depth is still at infinity there only.
        if self.sky {
            bundle.set_pipeline(&shared.sky_pipeline);
            bundle.set_bind_group(0, camera, &[]);
            bundle.draw(0..3, 0..1);
        }
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
