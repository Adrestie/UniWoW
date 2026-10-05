//! The layer of the terrain in the 3D view: the tiles in sight drawn chunk by chunk, its bundle
//! kept while the tiles loaded and the tiles in sight stay the same; the camera, which moves at
//! every frame, written in `prepare` into its buffer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::glam::{Mat4, Vec4};
use uniwow_api::viewport::{Layer, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::gpu::{CAMERA, Shared, TileGpu};
use crate::mesh::VERTICES;
use crate::model::TileId;

/// The tiles handed to the drawing, and when each was last in sight; shared by the module, which
/// hands them over and releases them, and its layer.
#[derive(Default)]
pub struct Scene {
    pub tiles: Vec<Arc<TileGpu>>,
    /// Counts the changes of `tiles`.
    pub generation: u64,
    /// The frames the layer prepared, and the last of them each tile was in sight.
    pub frame: u64,
    pub seen: HashMap<TileId, u64>,
}

pub fn lock<T>(shared: &Mutex<T>) -> MutexGuard<'_, T> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the box `bounds` may be in sight through `view_proj`: no side of the view has its
/// eight corners all beyond it, nor all behind the eye.
pub fn in_sight(view_proj: Mat4, bounds: [[f32; 3]; 2]) -> bool {
    let corners: Vec<Vec4> = (0..8)
        .map(|i| {
            let pick = |axis: usize| bounds[(i >> axis) & 1][axis];
            view_proj * Vec4::new(pick(0), pick(1), pick(2), 1.0)
        })
        .collect();
    let all = |beyond: &dyn Fn(&Vec4) -> bool| corners.iter().all(beyond);
    !(all(&|c| c.w <= 0.0)
        || all(&|c| c.x < -c.w)
        || all(&|c| c.x > c.w)
        || all(&|c| c.y < -c.w)
        || all(&|c| c.y > c.w))
}

pub struct TerrainLayer {
    /// Where the job building the pipeline leaves what the tiles share.
    incoming: Arc<Mutex<Option<Arc<Shared>>>>,
    shared: Option<Arc<Shared>>,
    scene: Arc<Mutex<Scene>>,
    camera: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    visible: Vec<Arc<TileGpu>>,
    /// What the bundle was recorded with: the generation of the tiles and those in sight.
    recorded: (u64, Vec<TileId>),
    version: u64,
}

impl TerrainLayer {
    pub fn new(incoming: Arc<Mutex<Option<Arc<Shared>>>>, scene: Arc<Mutex<Scene>>) -> Self {
        Self {
            incoming,
            shared: None,
            scene,
            camera: None,
            visible: Vec::new(),
            recorded: (u64::MAX, Vec::new()),
            version: 0,
        }
    }
}

impl Layer for TerrainLayer {
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        if self.shared.is_none() {
            self.shared = lock(&self.incoming).clone();
        }
        let Some(shared) = &self.shared else {
            return;
        };
        let (buffer, _) = self.camera.get_or_insert_with(|| {
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("terrain camera"),
                size: (CAMERA * 4) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("terrain camera"),
                layout: &shared.camera_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            (buffer, group)
        });
        let mut camera = [0f32; CAMERA];
        camera[..16].copy_from_slice(&view.view_proj.to_cols_array());
        let sun = uniwow_api::glam::Vec3::new(0.4, 0.3, 0.85).normalize();
        camera[16..19].copy_from_slice(&sun.to_array());
        gpu.queue.write_buffer(buffer, 0, bytemuck::cast_slice(&camera));

        let mut scene = lock(&self.scene);
        scene.frame += 1;
        let frame = scene.frame;
        let visible: Vec<Arc<TileGpu>> = scene
            .tiles
            .iter()
            .filter(|tile| in_sight(view.view_proj, tile.bounds))
            .cloned()
            .collect();
        for tile in &visible {
            scene.seen.insert(tile.id, frame);
        }
        let recorded = (scene.generation, visible.iter().map(|tile| tile.id).collect());
        if recorded != self.recorded {
            self.recorded = recorded;
            self.version += 1;
        }
        self.visible = visible;
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
        let (Some(shared), Some((_, camera))) = (&self.shared, &self.camera) else {
            return;
        };
        bundle.set_pipeline(&shared.pipeline);
        bundle.set_bind_group(0, camera, &[]);
        for tile in &self.visible {
            bundle.set_bind_group(1, &tile.tile_group, &[]);
            bundle.set_vertex_buffer(0, tile.vertices.slice(..));
            bundle.set_index_buffer(tile.indices.slice(..), wgpu::IndexFormat::Uint16);
            for (place, range) in tile.ranges.iter().enumerate() {
                if range.is_empty() {
                    continue;
                }
                bundle.set_bind_group(2, &tile.chunk_groups[place], &[]);
                bundle.draw_indexed(range.clone(), (place * VERTICES) as i32, 0..1);
            }
        }
    }
}
