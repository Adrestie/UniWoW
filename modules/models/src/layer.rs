//! The layer of the models in the 3D view: the groups of every owner whose look is on the GPU, in
//! sight and within the reach of their size, each at the skin its distance chooses; their opaque
//! and alpha-keyed batches first, then the blended ones, the groups the farthest first.
//!
//! With the pool, drawn in the pass at each frame by a few commands: the instances of every owner
//! copied into one buffer of the frame, and, for each batch of each group drawn, the arguments of
//! its draw and the entries of its instances (the instance among those of the frame, and its
//! material), written by the CPU; one `multi_draw_indexed_indirect` for the batches of each state
//! of pipeline, the blended states in a fixed order. The looks the pool had no room for, and every
//! look without a pool, are drawn as in step 9.4c, with their own buffers and textures: without a
//! pool, in a bundle kept while the groups drawn, their skins and that order stay the same.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::glam::{Mat4, Vec3, Vec4};
use uniwow_api::models::LookId;
use uniwow_api::viewport::{Drawing, Layer, LayerStats, Target, View};
use uniwow_api::wgpu::util::DrawIndexedIndirectArgs;
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::gpu::{CAMERA, Shared, State, camera_values};
use crate::loading::{LookGpu, Ready};
use crate::lock;
use crate::pool::{ENTRY, INSTANCE, Pool};
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
    pub looks: Arc<HashMap<LookId, Arc<Ready>>>,
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
    look: Arc<Ready>,
    buffer: Arc<wgpu::Buffer>,
    instances: std::ops::Range<u32>,
    level: usize,
    distance: f32,
    blended: bool,
}

/// The order the blended states are drawn in: alpha and blend add, sorted the farthest first, then
/// those whose order changes nothing among their own draws (add without alpha, add, mod, mod2x).
const BLENDED: [u16; 6] = [2, 7, 3, 4, 5, 6];

/// The order of a state among those drawn.
fn rank(state: &State) -> (usize, u16, bool, bool, bool) {
    let blended = BLENDED
        .iter()
        .position(|blending| *blending == state.blending)
        .map_or(0, |at| at + 1);
    (
        blended,
        state.blending,
        state.two_sided,
        state.depth_test,
        state.depth_write,
    )
}

/// The buffers of a frame of the pool and the bind group reading them, with what it was made with.
struct FrameBuffers {
    instances: wgpu::Buffer,
    entries: wgpu::Buffer,
    args: wgpu::Buffer,
    group: Option<wgpu::BindGroup>,
    made: (u64, u64),
}

/// A buffer of at least `bytes`, twice the size it needs to grow to.
fn sized(
    device: &wgpu::Device,
    kept: Option<wgpu::Buffer>,
    bytes: u64,
    usage: wgpu::BufferUsages,
    label: &str,
) -> (wgpu::Buffer, bool) {
    match kept {
        Some(buffer) if buffer.size() >= bytes => (buffer, false),
        _ => (
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.next_power_of_two().max(256),
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            true,
        ),
    }
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
    /// With the pool: the buffers of the owners copied into that of the frame (the buffer, its
    /// bytes, where they go), the buffers of the frame, and the draws of each state, opaque then
    /// blended (the state, its first draw, its draws).
    copies: Vec<(Arc<wgpu::Buffer>, u64, u64)>,
    frame: Option<FrameBuffers>,
    buckets: [Vec<(State, u32, u32)>; 2],
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
            copies: Vec::new(),
            frame: None,
            buckets: [Vec::new(), Vec::new()],
            stats: LayerStats::default(),
        }
    }

    fn pool(&self) -> Option<Arc<Pool>> {
        self.shared.as_ref().and_then(|shared| shared.pool.clone())
    }

    /// The draws of the pooled looks drawn this frame, written to the buffers of the frame: the
    /// instances of every owner copied into one, the entries and arguments of each batch of each
    /// group, gathered by state; the commands it takes.
    fn prepare_pool(
        &mut self,
        pool: &Pool,
        gpu: &egui_wgpu::RenderState,
        owners: &[(u32, Arc<wgpu::Buffer>, u32)],
    ) -> u64 {
        let mut base: HashMap<u32, u32> = HashMap::new();
        self.copies.clear();
        let mut total = 0u32;
        for (number, buffer, used) in owners {
            base.insert(*number, total);
            self.copies
                .push((buffer.clone(), u64::from(*used) * INSTANCE, u64::from(total) * INSTANCE));
            total += used;
        }
        let mut entries: Vec<[u32; 2]> = Vec::new();
        let mut gathered: [HashMap<State, Vec<DrawIndexedIndirectArgs>>; 2] = [HashMap::new(), HashMap::new()];
        let at: HashMap<GroupKey, usize> = self
            .drawn
            .iter()
            .enumerate()
            .map(|(index, group)| (group.key, index))
            .collect();
        let opaque = self.drawn.iter().map(|group| (group, false));
        let blended = self.order.iter().map(|key| (&self.drawn[at[key]], true));
        for (group, pass) in opaque.chain(blended) {
            let Ready::Pooled(look) = &*group.look else {
                continue;
            };
            let first = base[&group.key.0] + group.instances.start;
            for record in &look.skins[group.level] {
                if record.state.blended() != pass {
                    continue;
                }
                gathered[usize::from(pass)]
                    .entry(record.state)
                    .or_default()
                    .push(DrawIndexedIndirectArgs {
                        index_count: record.count,
                        instance_count: group.instances.len() as u32,
                        first_index: record.first_index,
                        base_vertex: record.base_vertex,
                        first_instance: entries.len() as u32,
                    });
                entries.extend((0..group.instances.len() as u32).map(|instance| [first + instance, record.material]));
            }
        }
        let mut args: Vec<DrawIndexedIndirectArgs> = Vec::new();
        for (bucket, gathered) in self.buckets.iter_mut().zip(gathered) {
            bucket.clear();
            let mut states: Vec<(State, Vec<DrawIndexedIndirectArgs>)> = gathered.into_iter().collect();
            states.sort_by_key(|(state, _)| rank(state));
            for (state, draws) in states {
                bucket.push((state, args.len() as u32, draws.len() as u32));
                args.extend(draws);
            }
        }
        if args.is_empty() {
            return 0;
        }
        let kept = self.frame.take();
        let (instances, new_instances) = sized(
            &gpu.device,
            kept.as_ref().map(|frame| frame.instances.clone()),
            u64::from(total) * INSTANCE,
            wgpu::BufferUsages::STORAGE,
            "models instances of the frame",
        );
        let (entry_buffer, new_entries) = sized(
            &gpu.device,
            kept.as_ref().map(|frame| frame.entries.clone()),
            entries.len() as u64 * ENTRY,
            wgpu::BufferUsages::STORAGE,
            "models entries of the frame",
        );
        let (arg_buffer, _) = sized(
            &gpu.device,
            kept.as_ref().map(|frame| frame.args.clone()),
            (args.len() * size_of::<DrawIndexedIndirectArgs>()) as u64,
            wgpu::BufferUsages::INDIRECT,
            "models draws of the frame",
        );
        let made = pool.generation();
        let group = kept
            .and_then(|frame| {
                frame
                    .group
                    .filter(|_| frame.made == made && !new_instances && !new_entries)
            })
            .or_else(|| pool.bind_group(&instances, &entry_buffer));
        gpu.queue.write_buffer(&entry_buffer, 0, bytemuck::cast_slice(&entries));
        let arg_bytes: Vec<u8> = args.iter().flat_map(|arg| arg.as_bytes().to_vec()).collect();
        gpu.queue.write_buffer(&arg_buffer, 0, &arg_bytes);
        self.frame = Some(FrameBuffers {
            instances,
            entries: entry_buffer,
            args: arg_buffer,
            group,
            made,
        });
        self.buckets.iter().map(|bucket| bucket.len() as u64).sum()
    }

    /// The batches of the looks with buffers and textures of their own, `pass` saying which: the
    /// blended ones of the blended groups, the farthest first, or the others.
    fn own(&self, blended: bool) -> Vec<(&Drawn, &LookGpu)> {
        let at: HashMap<GroupKey, usize> = self
            .drawn
            .iter()
            .enumerate()
            .map(|(index, group)| (group.key, index))
            .collect();
        let groups: Vec<&Drawn> = if blended {
            self.order.iter().map(|key| &self.drawn[at[key]]).collect()
        } else {
            self.drawn.iter().collect()
        };
        groups
            .into_iter()
            .filter_map(|group| match &*group.look {
                Ready::Own(look) => Some((group, look)),
                Ready::Pooled(_) => None,
            })
            .collect()
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
        let mut owners = Vec::new();
        let (mut instances, mut groups) = (0u64, 0usize);
        for slot in self.service.owners() {
            let published = slot.published();
            let Some(buffer) = published.buffer.clone() else {
                continue;
            };
            groups += published.groups.len();
            let used = published
                .groups
                .iter()
                .map(|group| group.first + group.count)
                .max()
                .unwrap_or(0);
            owners.push((slot.number, buffer.clone(), used));
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
                let level = level(distance / radius.max(0.5), levels.get(&key).copied(), look.levels());
                self.levels.insert(key, level);
                instances += u64::from(group.count);
                layouts.push((key, published.layout, level));
                drawn.push(Drawn {
                    key,
                    blended: look.batches(level).iter().any(|(state, _)| state.blended()),
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

        let mut draws = 0;
        let mut triangles = 0;
        for group in &self.drawn {
            for (_, count) in group.look.batches(group.level) {
                draws += 1;
                triangles += u64::from(count / 3) * u64::from(group.instances.len() as u32);
            }
        }
        let commands = match self.pool() {
            Some(pool) => {
                // A command for each batch of a look with its own buffers.
                let own: u64 = self
                    .drawn
                    .iter()
                    .filter_map(|group| match &*group.look {
                        Ready::Own(look) => Some(look.skins[group.level].len() as u64),
                        Ready::Pooled(_) => None,
                    })
                    .sum();
                self.prepare_pool(&pool, gpu, &owners) + own
            }
            None => {
                let recorded = (generation, layouts, self.order.clone());
                if recorded != self.recorded {
                    self.recorded = recorded;
                    self.version += 1;
                    self.recordings.push_back(Instant::now());
                }
                draws
            }
        };
        while self
            .recordings
            .front()
            .is_some_and(|at| at.elapsed() > Duration::from_secs(1))
        {
            self.recordings.pop_front();
        }
        let mut used = [0usize; LIMITS.len() + 1];
        for group in &self.drawn {
            used[group.level] += 1;
        }
        let drawing = match self.pool() {
            Some(_) => format!("{commands} commands in the pass"),
            None => format!("bundle recorded {} times in the last second", self.recordings.len()),
        };
        self.stats = LayerStats {
            draws,
            triangles,
            bytes,
            items: format!(
                "{summary}\n  {instances} instances in sight in {} groups of {groups}, levels {used:?}; {drawing}",
                self.drawn.len(),
            ),
            steering,
        };
    }

    fn compute(&mut self, _gpu: &egui_wgpu::RenderState, _view: &View, encoder: &mut wgpu::CommandEncoder) {
        let Some(frame) = &self.frame else {
            return;
        };
        if self.buckets.iter().all(Vec::is_empty) {
            return;
        }
        for (buffer, bytes, at) in &self.copies {
            if *bytes > 0 {
                encoder.copy_buffer_to_buffer(buffer, 0, &frame.instances, *at, *bytes);
            }
        }
    }

    fn drawing(&self) -> Drawing {
        if self.pool().is_some() {
            Drawing::Pass
        } else {
            Drawing::Bundle
        }
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
            let Ready::Own(look) = &*group.look else {
                continue;
            };
            let model = &look.model;
            let skin = &model.skins[group.level];
            let mut bound = false;
            for batch in &look.skins[group.level] {
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

    fn draw_pass(
        &mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        let (Some((_, camera)), Some(pool)) = (&self.camera, self.pool()) else {
            return;
        };
        let pooled = match (&self.frame, pool.vertices.buffer(), pool.indices.buffer()) {
            (Some(frame), Some((vertices, _)), Some((indices, _))) => {
                frame.group.as_ref().map(|group| (frame, group, vertices, indices))
            }
            _ => None,
        };
        for blended in [false, true] {
            if let Some((frame, group, vertices, indices)) = &pooled
                && !self.buckets[usize::from(blended)].is_empty()
            {
                pass.set_bind_group(0, camera, &[]);
                pass.set_bind_group(1, *group, &[]);
                pass.set_vertex_buffer(0, vertices.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                for (state, first, count) in &self.buckets[usize::from(blended)] {
                    pass.set_pipeline(&pool.pipeline(*state));
                    pass.multi_draw_indexed_indirect(
                        &frame.args,
                        u64::from(*first) * size_of::<DrawIndexedIndirectArgs>() as u64,
                        *count,
                    );
                }
            }
            // The looks the pool had no room for, as in step 9.4c.
            for (group, look) in self.own(blended) {
                let model = &look.model;
                let skin = &model.skins[group.level];
                let mut bound = false;
                for batch in &look.skins[group.level] {
                    if batch.state.blended() != blended {
                        continue;
                    }
                    if !bound {
                        pass.set_bind_group(0, camera, &[]);
                        pass.set_vertex_buffer(0, model.vertices.slice(..));
                        pass.set_vertex_buffer(1, group.buffer.slice(..));
                        pass.set_index_buffer(skin.indices.slice(..), skin.format);
                        bound = true;
                    }
                    pass.set_pipeline(&batch.pipeline);
                    pass.set_bind_group(1, &batch.group, &[]);
                    pass.draw_indexed(batch.indices.clone(), 0, group.instances.clone());
                }
            }
        }
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
