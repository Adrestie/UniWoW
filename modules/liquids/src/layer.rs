//! The layer of the liquids, at the stage of the water: the magma and slime of the tiles held drawn
//! with the opaque, their water in the phase of the water, between what is blended beyond its
//! surface and what is blended on the eye's side, from over it and from under it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use uniwow_api::glam::{Mat4, Vec3, Vec4};

use uniwow_api::viewport::{Drawing, Layer, LayerStats, Phase, Stage, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::gpu::{CAMERA, Shared, TileGpu, camera_values};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A liquid another module placed, on the GPU: drawn while its flag is set and its bounds, in the
/// world, are in sight.
#[derive(Clone)]
pub struct Poured {
    pub gpu: Arc<TileGpu>,
    pub shown: Arc<AtomicBool>,
    pub bounds: [Vec3; 2],
}

/// What the module shares with its layer: the tiles held and the liquids other modules placed, the
/// time it spent steering, and the longest it took to give them since the map was shown.
#[derive(Default)]
pub struct Scene {
    pub tiles: Vec<Arc<TileGpu>>,
    pub placed: Vec<Poured>,
    pub steering: Duration,
    pub publishing: Duration,
}

/// Whether the box `bounds` lies, even in part, within the sides of the view of `view_proj`, its
/// far plane left out.
fn in_sight(view_proj: &Mat4, bounds: &[Vec3; 2]) -> bool {
    let [x, y, z, w] = [0, 1, 2, 3].map(|row| view_proj.row(row));
    // Reverse Z: the near plane where the depth is 1.
    [w + x, w - x, w + y, w - y, w - z].iter().all(|plane: &Vec4| {
        let nearest = Vec3::new(
            if plane.x >= 0.0 { bounds[1].x } else { bounds[0].x },
            if plane.y >= 0.0 { bounds[1].y } else { bounds[0].y },
            if plane.z >= 0.0 { bounds[1].z } else { bounds[0].z },
        );
        plane.truncate().dot(nearest) + plane.w >= 0.0
    })
}

pub struct LiquidsLayer {
    shared: Arc<Shared>,
    scene: Arc<Mutex<Scene>>,
    camera: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    group: Option<(wgpu::BindGroup, u64)>,
    /// The tiles drawn this frame, held until the next.
    drawn: Vec<Arc<TileGpu>>,
    stats: LayerStats,
}

impl LiquidsLayer {
    pub fn new(shared: Arc<Shared>, scene: Arc<Mutex<Scene>>) -> Self {
        Self {
            shared,
            scene,
            camera: None,
            group: None,
            drawn: Vec::new(),
            stats: LayerStats::default(),
        }
    }
}

impl Layer for LiquidsLayer {
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        let shared = self.shared.clone();
        let (mut tiles, placed, steering, publishing) = {
            let scene = lock(&self.scene);
            (
                scene.tiles.clone(),
                scene.placed.clone(),
                scene.steering,
                scene.publishing,
            )
        };
        let tiles_held = tiles.len();
        let shown = placed
            .iter()
            .filter(|poured| poured.shown.load(Ordering::Relaxed) && in_sight(&view.view_proj, &poured.bounds));
        let before = tiles.len();
        tiles.extend(shown.map(|poured| poured.gpu.clone()));
        let drawn_placed = tiles.len() - before;
        let (camera, _) = self.camera.get_or_insert_with(|| {
            let buffer = shared.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("liquids camera"),
                size: (CAMERA * 4) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = shared.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("liquids camera"),
                layout: &shared.camera_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            (buffer, group)
        });
        gpu.queue
            .write_buffer(camera, 0, bytemuck::cast_slice(&camera_values(view)));
        let generation = shared.arrays.generation();
        if self.group.as_ref().is_none_or(|(_, at)| *at != generation) {
            self.group = Some((shared.bind_group(), generation));
        }
        let (water, opaque): (u64, u64) = tiles.iter().fold((0, 0), |(water, opaque), tile| {
            (
                water + u64::from(tile.water.len() as u32),
                opaque + u64::from(tile.opaque.len() as u32),
            )
        });
        self.stats = LayerStats {
            draws: tiles
                .iter()
                .map(|tile| u64::from(!tile.water.is_empty()) + u64::from(!tile.opaque.is_empty()))
                .sum(),
            triangles: (water + opaque) / 3,
            bytes: shared.bytes(),
            items: format!(
                "{tiles_held} tiles, and {drawn_placed} drawn of the {} liquids other modules placed; {} \
                 triangles of water and {} of magma and slime; arenas of {} and {} MB ({} and {} used), of {} MB at \
                 most each; given in {:.2} ms at the longest",
                placed.len(),
                water / 3,
                opaque / 3,
                shared.vertices.bytes().0 >> 20,
                shared.indices.bytes().0 >> 20,
                shared.vertices.bytes().1 >> 20,
                shared.indices.bytes().1 >> 20,
                shared.vertices.most() >> 20,
                publishing.as_secs_f64() * 1000.0
            ),
            steering,
        };
        self.drawn = tiles;
    }

    fn drawing(&self) -> Drawing {
        Drawing::Pass
    }

    fn stage(&self) -> Stage {
        Stage::Water
    }

    fn draw_pass(
        &mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        phase: Phase,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        let (pipeline, water) = match phase {
            Phase::Opaque => (&self.shared.opaque, false),
            Phase::Water => (&self.shared.water, true),
            Phase::Beyond | Phase::Near => return,
        };
        let (Some((_, camera)), Some((group, _)), Some((vertices, _)), Some((indices, _))) = (
            &self.camera,
            &self.group,
            self.shared.vertices.buffer(),
            self.shared.indices.buffer(),
        ) else {
            return;
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, camera, &[]);
        pass.set_bind_group(1, group, &[]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        for tile in &self.drawn {
            let range = if water { &tile.water } else { &tile.opaque };
            if !range.is_empty() {
                pass.draw_indexed(range.clone(), tile.base_vertex, 0..1);
            }
        }
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
