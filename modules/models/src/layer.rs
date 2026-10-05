//! The layer of the models in the 3D view: the groups of every owner whose look is on the GPU, in
//! sight and within the reach of their size, each at the skin its distance chooses; their opaque
//! and alpha-keyed batches first, then the blended ones, the groups the farthest first. Its bundle
//! is kept while the groups drawn, their skins and that order stay the same; the camera, and the
//! instances their owners move, change through buffers.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::glam::{Mat4, Vec3, Vec4};
use uniwow_api::models::LookId;
use uniwow_api::viewport::{Layer, LayerStats, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::gpu::{CAMERA, Shared, camera_values};
use crate::loading::LookGpu;
use crate::lock;
use crate::service::Service;

/// The distances, in radii of an instance, past which its skin goes to the next one; the share of
/// a limit a level is kept beyond it.
pub const LIMITS: [f32; 3] = [40.0, 80.0, 160.0];
pub const MARGIN: f32 = 0.1;
/// How far apart, at the least and in a share of the distance, two groups of blended batches
/// are before their order is sorted again.
const SWAP_YARDS: f32 = 2.0;
const SWAP_SHARE: f32 = 0.05;

/// What the module hands to the layer: the looks on the GPU, how far an instance is drawn, and
/// what the statistics say.
#[derive(Default)]
pub struct Scene {
    pub looks: Arc<HashMap<LookId, Arc<LookGpu>>>,
    /// Counts the changes of `looks`.
    pub generation: u64,
    /// How far an instance is drawn, in radii.
    pub reach: f32,
    pub summary: String,
    pub bytes: u64,
    pub steering: Duration,
}

/// A group, by its owner's number, its look and its tile.
pub type GroupKey = (u32, LookId, [i32; 2]);

/// The level of detail of an instance `ratio` of its radius from the eye: kept at `previous` until
/// past its limit by the margin; never past the last of `levels`.
pub fn level(ratio: f32, previous: Option<usize>, levels: usize) -> usize {
    let plain = LIMITS.iter().filter(|limit| ratio >= **limit).count();
    let level = match previous {
        Some(before) if plain > before && ratio < LIMITS[before] * (1.0 + MARGIN) => before,
        Some(before) if plain < before && ratio > LIMITS[before - 1] * (1.0 - MARGIN) => before,
        _ => plain,
    };
    level.min(levels.saturating_sub(1))
}

/// Whether the box `bounds` may be in sight through `view_proj`: no side of the view has its eight
/// corners all beyond it, nor all behind the eye.
pub fn in_sight(view_proj: Mat4, bounds: [Vec3; 2]) -> bool {
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

/// How far `eye` is from the nearest point of the box `bounds`.
pub fn nearest(eye: Vec3, bounds: [Vec3; 2]) -> f32 {
    let [low, high] = bounds;
    (low - eye).max(eye - high).max(Vec3::ZERO).length()
}

/// Whether the blended groups at `distances`, in the order drawn last, may stay so: none nearer
/// than the next by more than the margin.
pub fn still_ordered(distances: &[f32]) -> bool {
    distances.windows(2).all(|pair| {
        let margin = SWAP_YARDS.max(pair[1] * SWAP_SHARE);
        pair[0] + margin >= pair[1]
    })
}

/// A group drawn this frame.
struct Drawn {
    key: GroupKey,
    look: Arc<LookGpu>,
    buffer: Arc<wgpu::Buffer>,
    instances: std::ops::Range<u32>,
    level: usize,
    distance: f32,
    blended: bool,
}

pub struct ModelsLayer {
    service: Arc<Service>,
    scene: Arc<Mutex<Scene>>,
    /// Where the job building what the models share leaves it.
    incoming: Arc<Mutex<Option<Arc<Shared>>>>,
    shared: Option<Arc<Shared>>,
    camera: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    drawn: Vec<Drawn>,
    /// The blended groups, the farthest first, as last drawn.
    order: Vec<GroupKey>,
    levels: HashMap<GroupKey, usize>,
    /// What the bundle was recorded with: the looks, and each group with its layout and level.
    recorded: (u64, Vec<(GroupKey, u64, usize)>, Vec<GroupKey>),
    version: u64,
    /// When the bundle was recorded, in the last second.
    recordings: VecDeque<Instant>,
    stats: LayerStats,
}

impl ModelsLayer {
    pub fn new(service: Arc<Service>, scene: Arc<Mutex<Scene>>, incoming: Arc<Mutex<Option<Arc<Shared>>>>) -> Self {
        Self {
            service,
            scene,
            incoming,
            shared: None,
            camera: None,
            drawn: Vec::new(),
            order: Vec::new(),
            levels: HashMap::new(),
            recorded: (u64::MAX, Vec::new(), Vec::new()),
            version: 0,
            recordings: VecDeque::new(),
            stats: LayerStats::default(),
        }
    }
}

impl Layer for ModelsLayer {
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        if self.shared.is_none() {
            self.shared = lock(&self.incoming).clone();
        }
        let Some(shared) = self.shared.clone() else {
            return;
        };
        let (looks, generation, reach, summary, bytes, steering) = {
            let scene = lock(&self.scene);
            (
                scene.looks.clone(),
                scene.generation,
                scene.reach,
                scene.summary.clone(),
                scene.bytes,
                scene.steering,
            )
        };
        let (camera, _) = self.camera.get_or_insert_with(|| {
            let buffer = shared.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("models camera"),
                size: (CAMERA * 4) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = shared.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("models camera"),
                layout: &shared.camera_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            (buffer, group)
        });
        gpu.queue
            .write_buffer(camera, 0, bytemuck::cast_slice(&camera_values(view, reach)));

        let levels = std::mem::take(&mut self.levels);
        let mut drawn = Vec::new();
        let mut layouts = Vec::new();
        let (mut instances, mut groups) = (0u64, 0usize);
        for slot in self.service.owners() {
            let published = slot.published();
            let Some(buffer) = published.buffer.clone() else {
                continue;
            };
            groups += published.groups.len();
            for group in &published.groups {
                let Some(look) = looks.get(&group.look) else {
                    continue;
                };
                let radius = look.radius() * group.scale;
                let bounds = [group.low - Vec3::splat(radius), group.high + Vec3::splat(radius)];
                let distance = nearest(view.eye, bounds);
                if distance > reach * radius.max(1.0) || !in_sight(view.view_proj, bounds) {
                    continue;
                }
                let key = (slot.number, group.look, group.tile);
                let level = level(distance / radius.max(0.5), levels.get(&key).copied(), look.skins.len());
                self.levels.insert(key, level);
                instances += u64::from(group.count);
                layouts.push((key, published.layout, level));
                drawn.push(Drawn {
                    key,
                    blended: look.skins[level].iter().any(|batch| batch.state.blended()),
                    look: look.clone(),
                    buffer: buffer.clone(),
                    instances: group.first..group.first + group.count,
                    level,
                    distance,
                });
            }
        }
        // The blended groups, the farthest first: sorted again only when two cross by the margin.
        let distance_of: HashMap<GroupKey, f32> = drawn
            .iter()
            .filter(|group| group.blended)
            .map(|group| (group.key, group.distance))
            .collect();
        let same_set =
            self.order.len() == distance_of.len() && self.order.iter().all(|key| distance_of.contains_key(key));
        let kept = same_set && still_ordered(&self.order.iter().map(|key| distance_of[key]).collect::<Vec<_>>());
        if !kept {
            let mut order: Vec<(GroupKey, f32)> = distance_of.into_iter().collect();
            order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            self.order = order.into_iter().map(|(key, _)| key).collect();
        }
        self.drawn = drawn;

        let recorded = (generation, layouts, self.order.clone());
        if recorded != self.recorded {
            self.recorded = recorded;
            self.version += 1;
            self.recordings.push_back(Instant::now());
        }
        while self
            .recordings
            .front()
            .is_some_and(|at| at.elapsed() > Duration::from_secs(1))
        {
            self.recordings.pop_front();
        }
        let mut draws = 0;
        let mut triangles = 0;
        for group in &self.drawn {
            for batch in &group.look.skins[group.level] {
                draws += 1;
                triangles += u64::from(batch.indices.len() as u32 / 3) * u64::from(group.instances.len() as u32);
            }
        }
        let mut used = [0usize; LIMITS.len() + 1];
        for group in &self.drawn {
            used[group.level] += 1;
        }
        self.stats = LayerStats {
            draws,
            triangles,
            bytes,
            items: format!(
                "{summary}\n  {instances} instances in sight in {} groups of {groups}, levels {used:?}; bundle recorded {} times in the last second",
                self.drawn.len(),
                self.recordings.len()
            ),
            steering,
        };
    }

    fn version(&self) -> Option<u64> {
        Some(self.version * 2 + u64::from(self.camera.is_some()))
    }

    fn draw<'a>(
        &'a mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    ) {
        let Some((_, camera)) = &self.camera else {
            return;
        };
        bundle.set_bind_group(0, camera, &[]);
        let at: HashMap<GroupKey, usize> = self
            .drawn
            .iter()
            .enumerate()
            .map(|(index, group)| (group.key, index))
            .collect();
        let opaque = self.drawn.iter().map(|group| (group, false));
        let blended = self.order.iter().map(|key| (&self.drawn[at[key]], true));
        for (group, pass) in opaque.chain(blended) {
            let model = &group.look.model;
            let skin = &model.skins[group.level];
            let mut bound = false;
            for batch in &group.look.skins[group.level] {
                if batch.state.blended() != pass {
                    continue;
                }
                if !bound {
                    bundle.set_vertex_buffer(0, model.vertices.slice(..));
                    bundle.set_vertex_buffer(1, group.buffer.slice(..));
                    bundle.set_index_buffer(skin.indices.slice(..), skin.format);
                    bound = true;
                }
                bundle.set_pipeline(&batch.pipeline);
                bundle.set_bind_group(1, &batch.group, &[]);
                bundle.draw_indexed(batch.indices.clone(), 0, group.instances.clone());
            }
        }
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
