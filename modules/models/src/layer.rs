//! The layer of the models in the 3D view: the instances of every owner whose look is on the GPU,
//! in sight and within the reach of their size, each at the skin its distance chooses; their opaque
//! and alpha-keyed batches first, then the blended ones, the farthest first.
//!
//! With the pool, drawn in the pass at each frame by a few commands: the CPU finds the groups in
//! sight and sorts the instances of looks with blended batches, the farthest first; the GPU chooses
//! each instance of those groups and writes the draws of each state (`choice`). The looks the pool
//! had no room for, and every look without a pool, are drawn as in step 9.4c, each group at its
//! level, with their own buffers and textures: without a pool, in a bundle kept while the groups
//! drawn, their skins and their order stay the same.
//!
//! The vertices of the pool are posed by the bones the thread of the animations wrote (`animator`):
//! the owners it animated are drawn as it saw them, so that the table of their bones fits their
//! instances; the camera of each frame is handed to it for the next. The looks of their own, in the
//! pass, read their instances from the buffer of the frame, where the table of the bones finds
//! them; without the pool, they stay at rest.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::glam::{Mat4, Vec3, Vec4};
use uniwow_api::journal;
use uniwow_api::liquids;
use uniwow_api::models::LookId;
use uniwow_api::viewport::{Drawing, Layer, LayerStats, Phase, Pyramid, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::animator::{Animated, AnimationStats};
use crate::choice::{Blended, Choice, GroupOfFrame, Move, Tables};
use crate::gpu::{CAMERA, Shared, State, camera_values};
use crate::groups::Published;
use crate::loading::{LookGpu, Ready};
use crate::lock;
use crate::pool::Pool;
use crate::service::Service;

/// The distances, in radii of an instance, past which its skin goes to the next one; the share of
/// a limit a level is kept beyond it.
pub const LIMITS: [f32; 3] = [40.0, 80.0, 160.0];
pub const MARGIN: f32 = 0.1;
/// How far apart, at the least and in a share of the distance, two groups of blended batches
/// are before their order is sorted again.
const SWAP_YARDS: f32 = 2.0;
const SWAP_SHARE: f32 = 0.05;

/// What the module hands to the layer: the looks on the GPU, the tables the GPU chooses from, made
/// by a job a frame or so after the looks, how far an instance is drawn, and what the statistics
/// say.
#[derive(Default)]
pub struct Scene {
    pub looks: Arc<HashMap<LookId, Arc<Ready>>>,
    /// Counts the changes of `looks`.
    pub generation: u64,
    pub tables: Option<Arc<Tables>>,
    /// How far an instance is drawn, in radii.
    pub reach: f32,
    pub summary: String,
    pub bytes: u64,
    pub steering: Duration,
    /// The bones the thread of the animations published last, and what it did.
    pub animated: Option<Arc<Animated>>,
    pub animation: AnimationStats,
    /// The camera of the last frame prepared: its view and projection, its view alone, its eye.
    pub camera: Option<(Mat4, Mat4, Vec3)>,
    /// The liquids, by which a blended instance is told beyond the surface of the water or on the
    /// eye's side.
    pub liquids: Option<liquids::Handle>,
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

/// The box of an owner's groups drawn and their largest radius, `owner_bounds`.
pub type OwnerBounds = Option<([Vec3; 2], f32)>;

/// What an owner's groups give a frame, made once for its publication, the looks held and the
/// tables of the GPU: the groups of looks of the pool, given to the GPU whole once the owner is in
/// sight, the GPU choosing each instance; among them, those of looks with blended batches, tested
/// here and sorted instance by instance; and those of looks of their own, tested and drawn here.
#[derive(Debug, Default)]
pub struct OwnerPlan {
    /// The pooled groups, their first instance counted from the owner's.
    pub pooled: Vec<GroupOfFrame>,
    /// Of the pooled groups, those of looks with blended batches: their index and the slot of their
    /// look.
    pub blended: Vec<(usize, u32)>,
    /// The groups of looks of their own, by their index.
    pub own: Vec<usize>,
    /// The instances of the pooled groups.
    pub instances: u64,
    /// The instances its groups take from where its own begin in the arena: to the end of the last.
    pub used: u32,
}

/// The plan of the groups of `published`, the looks held `looks`, the tables `tables`; a group of a
/// look not held gives nothing.
pub fn plan_of(published: &Published, looks: &HashMap<LookId, Arc<Ready>>, tables: Option<&Tables>) -> OwnerPlan {
    let mut plan = OwnerPlan {
        used: published
            .groups
            .iter()
            .map(|group| group.first + group.count)
            .max()
            .unwrap_or(0),
        ..OwnerPlan::default()
    };
    for (index, group) in published.groups.iter().enumerate() {
        if !looks.contains_key(&group.look) {
            continue;
        }
        match tables.and_then(|tables| Some((tables, *tables.slots.get(&group.look)?))) {
            Some((tables, slot)) => {
                plan.pooled.push(GroupOfFrame {
                    first: group.first,
                    count: group.count,
                    slot,
                });
                plan.instances += u64::from(group.count);
                if tables.looks[slot as usize]
                    .blended
                    .iter()
                    .any(|records| !records.is_empty())
                {
                    plan.blended.push((index, slot));
                }
            }
            None => plan.own.push(index),
        }
    }
    plan
}

/// The box of the groups of `published` whose looks are drawn, grown by the largest of their radii
/// at their scales, and that radius; none when none is drawn. None of those groups is in sight
/// or within reach where this box is not.
pub fn owner_bounds(published: &Published, looks: &HashMap<LookId, Arc<Ready>>) -> OwnerBounds {
    let mut found: OwnerBounds = None;
    for group in &published.groups {
        let Some(look) = looks.get(&group.look) else {
            continue;
        };
        let radius = look.radius() * group.scale;
        let ([low, high], largest) = found.unwrap_or(([group.low, group.high], 0.0));
        found = Some(([low.min(group.low), high.max(group.high)], largest.max(radius)));
    }
    found.map(|([low, high], radius)| ([low - Vec3::splat(radius), high + Vec3::splat(radius)], radius))
}

/// Whether the blended groups at `distances`, in the order drawn last, may stay so: none nearer
/// than the next by more than the margin.
pub fn still_ordered(distances: &[f32]) -> bool {
    distances.windows(2).all(|pair| {
        let margin = SWAP_YARDS.max(pair[1] * SWAP_SHARE);
        pair[0] + margin >= pair[1]
    })
}

/// A group of a look of its own drawn this frame.
struct Drawn {
    key: GroupKey,
    look: Arc<Ready>,
    /// The arena of the instances, where its owner's begin, and its instances in its owner's.
    buffer: Arc<wgpu::Buffer>,
    first: u32,
    instances: std::ops::Range<u32>,
    level: usize,
    distance: f32,
    blended: bool,
    /// Beyond the surface of the water from the eye, by the middle of its group.
    beyond: bool,
}

impl Drawn {
    /// Its instances in the arena.
    fn in_arena(&self) -> std::ops::Range<u32> {
        self.first + self.instances.start..self.first + self.instances.end
    }
}

/// The order the blended states are drawn in: alpha and blend add, sorted the farthest first, then
/// those whose order changes nothing among their own draws (add without alpha, add, mod, mod2x).
const BLENDED: [u16; 6] = [2, 7, 3, 4, 5, 6];

/// The order of a state among those drawn.
pub fn rank(state: &State) -> (usize, u16, bool, bool, bool) {
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

/// An instance of a look with blended batches: its owner, the owner's layout and its place there,
/// by which its order is kept from a frame to the next.
type BlendedKey = (u32, u64, u32);

/// What the bundle was recorded with: the generations of the looks and of the arena of the
/// instances, each group with its layout and level, the blended groups in their order, and those
/// beyond the water from the eye.
type Recorded = ([u64; 2], Vec<(GroupKey, u64, usize)>, Vec<GroupKey>, Vec<GroupKey>);

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
    recorded: Recorded,
    version: u64,
    /// When the bundle was recorded, in the last second.
    recordings: VecDeque<Instant>,
    /// With the pool: the choice by the GPU, the bind group the vertex shader of the pool reads and
    /// the generation of the pool it was made at, and the blended instances in the order drawn last.
    choice: Option<Choice>,
    pool_group: Option<(wgpu::BindGroup, (u64, u64))>,
    /// The bind group of the bones, and the bones it was made with.
    skin_group: Option<(wgpu::BindGroup, Option<Arc<Animated>>)>,
    blended: Vec<BlendedKey>,
    /// The bounds of each owner (`owner_bounds`), by its number, with the publication and the
    /// generation of the looks they were made of, the publication held so that its place is not
    /// taken by another.
    owner_bounds: HashMap<u32, (Arc<Published>, u64, OwnerBounds)>,
    /// The plan of each owner (`plan_of`), by its number, with the publication, the generation of the
    /// looks and that of the tables it was made of.
    plans: HashMap<u32, (Arc<Published>, u64, u64, Arc<OwnerPlan>)>,
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
            recorded: ([u64::MAX; 2], Vec::new(), Vec::new(), Vec::new()),
            version: 0,
            recordings: VecDeque::new(),
            choice: None,
            pool_group: None,
            skin_group: None,
            blended: Vec::new(),
            owner_bounds: HashMap::new(),
            plans: HashMap::new(),
            stats: LayerStats::default(),
        }
    }

    fn pool(&self) -> Option<Arc<Pool>> {
        self.shared.as_ref().and_then(|shared| shared.pool.clone())
    }

    /// The choice by the GPU, once the pool draws.
    #[cfg(test)]
    pub fn choice(&mut self) -> Option<&mut Choice> {
        self.choice.as_mut()
    }

    /// The batches of the looks with buffers and textures of their own, `pass` saying which: the
    /// blended ones of the blended groups, the farthest first, or the others.
    fn own(&self, phase: Phase) -> Vec<(&Drawn, &LookGpu)> {
        let at: HashMap<GroupKey, usize> = self
            .drawn
            .iter()
            .enumerate()
            .map(|(index, group)| (group.key, index))
            .collect();
        let groups: Vec<&Drawn> = match phase {
            Phase::Opaque => self.drawn.iter().collect(),
            Phase::Revealed | Phase::Water => Vec::new(),
            Phase::Beyond | Phase::Near => self
                .order
                .iter()
                .map(|key| &self.drawn[at[key]])
                .filter(|group| group.beyond == (phase == Phase::Beyond))
                .collect(),
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
        // Its parts in the journal of the frames, each from the end of the one before.
        let mut part = Instant::now();
        let mut spent = |name: &str| {
            journal::spent(name, part.elapsed());
            part = Instant::now();
        };
        let (looks, generation, tables, reach, summary, bytes, steering, animated, animation, liquids) = {
            let mut scene = journal::lock(&self.scene, "models scene");
            scene.camera = Some((view.view_proj, view.view, view.eye));
            (
                scene.looks.clone(),
                scene.generation,
                scene.tables.clone(),
                scene.reach,
                scene.summary.clone(),
                scene.bytes,
                scene.steering,
                scene.animated.clone(),
                scene.animation.clone(),
                scene.liquids.clone(),
            )
        };
        let surfaces = liquids.map(|liquids| liquids.surfaces());
        let beyond = |point: Vec3| {
            surfaces
                .as_ref()
                .is_some_and(|surfaces| surfaces.phase(view.eye, point) == Phase::Beyond)
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
        let pool = shared.pool.clone();
        if let Some(pool) = &pool
            && self.choice.is_none()
        {
            self.choice = Some(Choice::new(&gpu.device, pool.count));
        }
        if let Some(choice) = &mut self.choice {
            choice.read_back();
            choice.set_tables(tables);
        }
        let tables = self.choice.as_ref().and_then(|choice| choice.tables.as_ref());

        let levels = std::mem::take(&mut self.levels);
        let mut drawn = Vec::new();
        let mut layouts = Vec::new();
        let mut bone_moves: Vec<Move> = Vec::new();
        let mut chosen = Vec::new();
        let mut candidates: Vec<(f32, BlendedKey, Blended, bool)> = Vec::new();
        let (mut instances, mut groups, mut seen) = (0u64, 0usize, 0usize);
        // The owners animated as the thread saw them, with where the table of their bones begins.
        let snapshots: HashMap<u32, (&Arc<crate::groups::Published>, u32, u32)> = animated
            .iter()
            .flat_map(|animated| &animated.owners)
            .map(|(number, published, at, count)| (*number, (published, *at, *count)))
            .collect();
        let mut present = HashSet::new();
        // The arena read after what the owners published: it holds every range they name.
        let published: Vec<_> = self
            .service
            .owners()
            .into_iter()
            .map(|slot| match snapshots.get(&slot.number) {
                Some((published, at, count)) => (slot, (*published).clone(), Some((*at, *count))),
                None => {
                    let published = slot.published();
                    (slot, published, None)
                }
            })
            .collect();
        let arena = self.service.instances().and_then(|arena| arena.buffer());
        let arena_generation = arena.as_ref().map_or(0, |(_, generation)| *generation);
        let tables_generation = tables.map_or(0, |tables| tables.generation);
        spent("models prepare: camera, owners and tables");
        for (slot, published, table) in published {
            let (Some(written), Some((buffer, _))) = (&published.written, &arena) else {
                continue;
            };
            let first = written.first();
            groups += published.groups.len();
            // An owner hidden, then one out of reach or out of sight by its bounds, is not given to
            // the frame, nor its bones copied.
            if !slot.shown.load(Ordering::Relaxed) {
                present.insert(slot.number);
                continue;
            }
            let owner = match self.owner_bounds.get(&slot.number) {
                Some((made_of, made_at, bounds)) if Arc::ptr_eq(made_of, &published) && *made_at == generation => {
                    *bounds
                }
                _ => {
                    let bounds = owner_bounds(&published, &looks);
                    self.owner_bounds
                        .insert(slot.number, (published.clone(), generation, bounds));
                    // Its plan, of the publication before, let go: made again once in sight.
                    self.plans.remove(&slot.number);
                    bounds
                }
            };
            present.insert(slot.number);
            let owner_seen = owner.is_some_and(|(bounds, radius)| {
                nearest(view.eye, bounds) <= reach * radius.max(1.0) && in_sight(view.view_proj, bounds)
            });
            if !owner_seen {
                continue;
            }
            let plan = match self.plans.get(&slot.number) {
                Some((made_of, looks_at, tables_at, plan))
                    if Arc::ptr_eq(made_of, &published)
                        && *looks_at == generation
                        && *tables_at == tables_generation =>
                {
                    plan.clone()
                }
                _ => {
                    let plan = Arc::new(plan_of(&published, &looks, tables.map(|tables| &**tables)));
                    self.plans.insert(
                        slot.number,
                        (published.clone(), generation, tables_generation, plan.clone()),
                    );
                    plan
                }
            };
            let used = plan.used;
            if let Some((at, count)) = table
                && count.min(used) > 0
            {
                bone_moves.push((u64::from(at) * 4, u64::from(first) * 4, u64::from(count.min(used)) * 4));
            }
            // From the pool: every group given to the GPU, which chooses each instance in sight.
            chosen.extend(plan.pooled.iter().map(|group| GroupOfFrame {
                first: first + group.first,
                ..*group
            }));
            seen += plan.pooled.len();
            instances += plan.instances;
            // The instances of looks with blended batches of the groups in sight, sorted here.
            for &(index, look_slot) in &plan.blended {
                let group = &published.groups[index];
                let Some(look) = looks.get(&group.look) else {
                    continue;
                };
                let radius = look.radius() * group.scale;
                let bounds = [group.low - Vec3::splat(radius), group.high + Vec3::splat(radius)];
                if nearest(view.eye, bounds) > reach * radius.max(1.0) || !in_sight(view.view_proj, bounds) {
                    continue;
                }
                for at in group.first..group.first + group.count {
                    let origin = published
                        .instances
                        .get(at as usize)
                        .map_or(group.low, |instance| instance.transform.w_axis.truncate());
                    candidates.push((
                        view.eye.distance(origin),
                        (slot.number, published.layout, at),
                        Blended {
                            index: first + at,
                            slot: look_slot,
                        },
                        beyond(origin),
                    ));
                }
            }
            // The looks of their own, by group in sight.
            for &index in &plan.own {
                let group = &published.groups[index];
                let Some(look) = looks.get(&group.look) else {
                    continue;
                };
                let radius = look.radius() * group.scale;
                let bounds = [group.low - Vec3::splat(radius), group.high + Vec3::splat(radius)];
                let distance = nearest(view.eye, bounds);
                if distance > reach * radius.max(1.0) || !in_sight(view.view_proj, bounds) {
                    continue;
                }
                seen += 1;
                instances += u64::from(group.count);
                let key = (slot.number, group.look, group.tile);
                let level = level(distance / radius.max(0.5), levels.get(&key).copied(), look.levels());
                self.levels.insert(key, level);
                layouts.push((key, published.layout, level));
                drawn.push(Drawn {
                    key,
                    blended: look.batches(level).iter().any(|(state, _)| state.blended()),
                    look: look.clone(),
                    buffer: buffer.clone(),
                    first,
                    instances: group.first..group.first + group.count,
                    level,
                    distance,
                    beyond: beyond((group.low + group.high) * 0.5),
                });
            }
        }
        self.owner_bounds.retain(|number, _| present.contains(number));
        self.plans.retain(|number, _| present.contains(number));
        spent("models prepare: the groups in sight");
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
        // The blended instances of the pool, the farthest first, in the same way.
        let at: HashMap<BlendedKey, usize> = candidates
            .iter()
            .enumerate()
            .map(|(index, (_, key, _, _))| (*key, index))
            .collect();
        let same_set = self.blended.len() == at.len() && self.blended.iter().all(|key| at.contains_key(key));
        let kept = same_set && still_ordered(&self.blended.iter().map(|key| candidates[at[key]].0).collect::<Vec<_>>());
        let blended: Vec<(Blended, bool)> = if kept {
            self.blended
                .iter()
                .map(|key| (candidates[at[key]].2, candidates[at[key]].3))
                .collect()
        } else {
            candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            self.blended = candidates.iter().map(|(_, key, _, _)| *key).collect();
            candidates
                .iter()
                .map(|(_, _, instance, beyond)| (*instance, *beyond))
                .collect()
        };
        // Those beyond the surface of the water from the eye, then those on its side, each in order.
        let (mut beyond_water, mut near_water) = (Vec::new(), Vec::new());
        for (instance, beyond) in blended {
            if beyond {
                beyond_water.push(instance);
            } else {
                near_water.push(instance);
            }
        }

        spent("models prepare: the order of the blended");
        // The groups of looks of their own, as in step 9.4c.
        let mut draws = 0;
        let mut triangles = 0;
        let mut own_commands = 0;
        for group in &self.drawn {
            for (_, count) in group.look.batches(group.level) {
                draws += 1;
                own_commands += 1;
                triangles += u64::from(count / 3) * u64::from(group.instances.len() as u32);
            }
        }
        let drawing = match (&pool, &mut self.choice) {
            (Some(pool), Some(choice)) => {
                let bones = animated.as_ref().map(|animated| (animated.buffer.clone(), bone_moves));
                let made = choice.frame(
                    &gpu.queue,
                    arena.clone(),
                    bones,
                    &chosen,
                    [&beyond_water, &near_water],
                    view.view_proj,
                    view.eye,
                    reach,
                );
                let made_at = pool.generation();
                if made || self.pool_group.as_ref().is_none_or(|(_, at)| *at != made_at) {
                    self.pool_group = choice
                        .buffers()
                        .and_then(|(instances, entries)| pool.bind_group(instances, entries))
                        .map(|group| (group, made_at));
                }
                let same_bones = |kept: &Option<Arc<Animated>>| match (kept, &animated) {
                    (Some(kept), Some(new)) => Arc::ptr_eq(kept, new),
                    (None, None) => true,
                    _ => false,
                };
                if made || self.skin_group.as_ref().is_none_or(|(_, kept)| !same_bones(kept)) {
                    self.skin_group = choice.bone_table().map(|table| {
                        let bones = animated.as_ref().map(|animated| (&*animated.buffer, animated.bones_at));
                        (pool.skin_group(table, bones), animated.clone())
                    });
                }
                let commands = choice.tables.as_ref().map_or(0, |tables| tables.regions.len())
                    + choice.blended_regions.len()
                    + own_commands;
                let gpu_drawn = choice.drawn;
                draws += u64::from(gpu_drawn.draws);
                triangles += u64::from(gpu_drawn.triangles);
                format!(
                    "{commands} commands in the pass; chosen by the GPU: {} pairs, levels {:?}, {} hidden by the depth; {} groups of looks of their own",
                    gpu_drawn.pairs,
                    gpu_drawn.levels,
                    gpu_drawn.hidden,
                    self.drawn.len(),
                )
            }
            _ => {
                let beyond: Vec<GroupKey> = self
                    .drawn
                    .iter()
                    .filter(|group| group.beyond)
                    .map(|group| group.key)
                    .collect();
                let recorded = ([generation, arena_generation], layouts, self.order.clone(), beyond);
                if recorded != self.recorded {
                    self.recorded = recorded;
                    self.version += 1;
                    self.recordings.push_back(Instant::now());
                }
                let mut used = [0usize; LIMITS.len() + 1];
                for group in &self.drawn {
                    used[group.level] += 1;
                }
                format!(
                    "levels {used:?}; bundle recorded {} times in the last second",
                    self.recordings.len()
                )
            }
        };
        spent("models prepare: the frame of the choice");
        while self
            .recordings
            .front()
            .is_some_and(|at| at.elapsed() > Duration::from_secs(1))
        {
            self.recordings.pop_front();
        }
        let animations = format!(
            "animated: {} instances, {} bones, {} slots of materials, {:.1} MB a frame, the thread {:.2} ms on average and {:.2} at most",
            animation.instances,
            animation.bones,
            animation.slots,
            animation.bytes as f64 / (1024.0 * 1024.0),
            animation.spent.as_secs_f64() * 1000.0,
            animation.longest.as_secs_f64() * 1000.0,
        );
        self.stats = LayerStats {
            draws,
            triangles,
            bytes,
            items: format!(
                "{summary}\n  {instances} instances in {seen} groups given of {groups}; {drawing}\n  {animations}"
            ),
            steering,
        };
        spent("models prepare: the statistics");
    }

    fn compute(&mut self, _gpu: &egui_wgpu::RenderState, _view: &View, encoder: &mut wgpu::CommandEncoder) {
        if let Some(choice) = &mut self.choice {
            choice.compute(encoder);
        }
    }

    fn computes(&self) -> bool {
        self.choice.is_some()
    }

    fn occlude(
        &mut self,
        _gpu: &egui_wgpu::RenderState,
        _view: &View,
        pyramid: &Pyramid<'_>,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        if let Some(choice) = &mut self.choice {
            choice.occlude(encoder, pyramid);
        }
    }

    fn occludes(&self) -> bool {
        self.choice.is_some()
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
        phase: Phase,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    ) {
        let Some((_, camera)) = &self.camera else {
            return;
        };
        bundle.set_bind_group(0, camera, &[]);
        let pass = phase.blended();
        for (group, look) in self.own(phase) {
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
                bundle.draw_indexed(batch.indices.clone(), 0, group.in_arena());
            }
        }
    }

    fn draw_pass(
        &mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        phase: Phase,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        let (Some((_, camera)), Some(pool)) = (&self.camera, self.pool()) else {
            return;
        };
        // The looks of their own posed by the table of the bones, once made; at rest before.
        let posing = match (&self.choice, &self.skin_group) {
            (Some(choice), Some((skin, _))) if choice.tables.is_some() => Some(skin),
            _ => None,
        };
        let pooled = match (
            &self.choice,
            &self.pool_group,
            &self.skin_group,
            pool.vertices.buffer(),
            pool.indices.buffer(),
        ) {
            (Some(choice), Some((group, _)), Some((skin, _)), Some((vertices, _)), Some((indices, _))) => {
                Some((choice, group, skin, vertices, indices))
            }
            _ => None,
        };
        let blended = phase.blended();
        {
            if let Some((choice, group, skin, vertices, indices)) = &pooled {
                pass.set_bind_group(0, camera, &[]);
                pass.set_bind_group(1, *group, &[]);
                pass.set_bind_group(2, *skin, &[]);
                pass.set_vertex_buffer(0, vertices.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                choice.draw(pass, phase, &|state| pool.pipeline(state));
            }
            // The looks the pool had no room for, as in step 9.4c.
            for (group, look) in self.own(phase) {
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
                        pass.set_bind_group(2, posing.unwrap_or(&pool.rest), &[]);
                        pass.set_index_buffer(skin.indices.slice(..), skin.format);
                        bound = true;
                    }
                    pass.set_pipeline(&batch.pipeline);
                    pass.set_bind_group(1, &batch.group, &[]);
                    pass.draw_indexed(batch.indices.clone(), 0, group.in_arena());
                }
            }
        }
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
