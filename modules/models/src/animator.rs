//! The thread of the animations (*Threads*, step 9.5), woken at each frame. For each instance of a
//! look whose bones or materials move, it chooses the sequence its motion asks for (*Stand*,
//! *Walk*, *Run*, through the fallbacks of `AnimationData.dbc`), plays it at the speed of the
//! instance over that of the sequence at its scale, each instance from a moment of its own,
//! blending into the next for its time of blending. It computes the bones of the instances of the
//! groups in sight, and before them the slots of their materials that move (`dress`), split with
//! `parallel_for`, and writes them, with for each owner a table of where each instance's bones
//! are, into one buffer published whole (`Animated`). An instance out of sight goes on in time,
//! its bones not computed; a static one costs nothing.
//!
//! The buffers are written in turn, three of them: the signal of a frame comes once it is
//! submitted, so the buffer the layer may still read (that of the last publication) is never the
//! one written.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::formats::{Animation, Formats};
use uniwow_api::glam::{Mat3, Mat4, Vec3};
use uniwow_api::journal;
use uniwow_api::models::{LookId, Motion};
use uniwow_api::parallel::parallel_for;
use uniwow_api::viewport::Handle;
use uniwow_api::wgpu::WriteOnly;
use uniwow_api::{bytemuck, wgpu};

use crate::dress;
use crate::groups::Published;
use crate::layer::{Scene, in_sight, nearest};
use crate::loading::Ready;
use crate::lock;
use crate::pose::{self, Moment, Posing};
use crate::service::Service;

/// The animations played: *Stand*, *Walk* and *Run*.
const STAND: u16 = 0;
const WALK: u16 = 4;
const RUN: u16 = 5;
/// The vectors of four floats of a bone and of a slot of a material, as the shader reads them, and
/// where the bones begin, in words: a multiple of the alignment of a storage binding.
const BONE: usize = 3;
const SLOT: usize = dress::SLOT / 4;
const ALIGN_WORDS: usize = 64;
/// How far past the view of the frame before a group is still animated: by this many yards, and
/// with the view widened by a quarter on each side, so that what the camera turns or moves into
/// sight between two frames is posed.
const MARGIN: f32 = 8.0;
const WIDER: f32 = 1.25;
/// The longest a frame is waited for.
const WAIT: Duration = Duration::from_millis(100);

/// What the thread published: its buffer; for each owner animated, its number, what it published
/// when its bones were computed, where its table begins in words and how many instances it has;
/// where the bones and slots begin, in bytes.
pub struct Animated {
    pub buffer: Arc<wgpu::Buffer>,
    pub owners: Vec<(u32, Arc<Published>, u32, u32)>,
    pub bones_at: u64,
}

/// What the thread did, for the statistics: the instances, bones and slots of materials of its
/// last frame, the bytes it wrote, and its time on average and at most over the last second.
#[derive(Clone, Debug, Default)]
pub struct AnimationStats {
    pub instances: usize,
    pub bones: usize,
    pub slots: usize,
    pub bytes: u64,
    pub spent: Duration,
    pub longest: Duration,
}

/// What an instance plays: the look whose sequences it counts, the animation wanted, the sequence
/// and the milliseconds into it, the loops done; while it blends, the sequence before, its time,
/// and the milliseconds of blending left of the whole.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playing {
    pub look: LookId,
    pub wanted: u16,
    pub sequence: usize,
    pub time: f32,
    pub loops: u32,
    pub before: Option<(usize, f32, f32, f32)>,
}

/// `motion` of an instance of `scale`, its speed in yards of its model.
pub fn scaled(motion: Motion, scale: f32) -> Motion {
    let scale = scale.max(1e-3);
    match motion {
        Motion::Standing => Motion::Standing,
        Motion::Walking(speed) => Motion::Walking(speed / scale),
        Motion::Moving(speed) => Motion::Moving(speed / scale),
    }
}

/// The animation `motion`, in yards of the model, asks of `animation`, and the speed it moves at:
/// walking as its flags say; moving, *Walk* or *Run* by whichever of their speeds is nearer its
/// own, *Walk* when the model has no *Run*.
pub fn wanted(animation: &Animation, motion: Motion) -> (u16, Option<f32>) {
    match motion {
        Motion::Standing => (STAND, None),
        Motion::Walking(speed) => (WALK, Some(speed)),
        Motion::Moving(speed) => {
            let pace = |id| {
                animation
                    .sequences
                    .iter()
                    .find(|sequence| sequence.id == id && sequence.speed > 0.0)
                    .map(|sequence| sequence.speed)
            };
            let walks = match (pace(WALK), pace(RUN)) {
                (Some(walk), Some(run)) => (speed - walk).abs() <= (run - speed).abs(),
                (walk, _) => walk.is_some(),
            };
            (if walks { WALK } else { RUN }, Some(speed))
        }
    }
}

/// A number of `id` and `round` the same at every run, to pick among variations and start apart.
pub fn roll(id: u64, round: u32) -> u32 {
    let hasher: BuildHasherDefault<DefaultHasher> = BuildHasherDefault::default();
    hasher.hash_one((id, round)) as u32
}

/// The sequence `animation` plays for `wanted`: a variation picked by `roll` in proportion to their
/// frequencies, the one its aliases lead to, whose keys are kept; through the fallbacks of
/// `fallbacks` when it has none; none at all, it stays at rest.
pub fn choose(animation: &Animation, wanted: u16, roll: u32, fallbacks: &HashMap<u16, u16>) -> Option<usize> {
    let sequences = &animation.sequences;
    let held = |mut at: usize| {
        for _ in 0..sequences.len() {
            match sequences[at].alias {
                Some(next) if usize::from(next) < sequences.len() => at = usize::from(next),
                _ => break,
            }
        }
        sequences[at].kept.then_some(at)
    };
    let mut id = wanted;
    let mut tried = HashSet::new();
    while tried.insert(id) {
        let variations: Vec<usize> = (0..sequences.len()).filter(|at| sequences[*at].id == id).collect();
        let total: u32 = variations
            .iter()
            .map(|at| u32::from(sequences[*at].frequency.max(0) as u16))
            .sum();
        let mut picked = variations.first().copied();
        if total > 0 {
            let mut left = roll % total;
            for at in &variations {
                let frequency = u32::from(sequences[*at].frequency.max(0) as u16);
                if left < frequency {
                    picked = Some(*at);
                    break;
                }
                left -= frequency;
            }
        }
        if let Some(found) = picked
            .and_then(held)
            .or_else(|| variations.iter().find_map(|at| held(*at)))
        {
            return Some(found);
        }
        id = *fallbacks.get(&id)?;
    }
    None
}

/// `playing` of `animation`, moved on by `elapsed` milliseconds as `motion` moves, in yards of the
/// model: a new animation wanted starts from its beginning, the one before blended out for the
/// time of blending of the new; a sequence ended starts again, a variation picked again.
pub fn advance(
    playing: &mut Playing,
    animation: &Animation,
    motion: Motion,
    elapsed: f32,
    id: u64,
    fallbacks: &HashMap<u16, u16>,
) {
    let (want, speed) = wanted(animation, motion);
    if want != playing.wanted
        && let Some(next) = choose(animation, want, roll(id, playing.loops), fallbacks)
    {
        let blend = f32::from(animation.sequences[next].blend[0]);
        playing.before = (blend > 0.0).then_some((playing.sequence, playing.time, blend, blend));
        *playing = Playing {
            wanted: want,
            sequence: next,
            time: 0.0,
            ..*playing
        };
    }
    if let Some((sequence, time, left, whole)) = playing.before {
        let left = left - elapsed;
        let duration = animation.sequences[sequence].duration.max(1) as f32;
        playing.before = (left > 0.0).then_some((sequence, (time + elapsed) % duration, left, whole));
    }
    let sequence = &animation.sequences[playing.sequence];
    let rate = match speed {
        Some(speed) if sequence.speed > 0.0 => speed / sequence.speed,
        _ => 1.0,
    };
    playing.time += elapsed * rate;
    let duration = sequence.duration.max(1) as f32;
    if !playing.time.is_finite() {
        playing.time = 0.0;
    }
    // The loops ended counted at once, a variation picked once for the last.
    if playing.time >= duration {
        let ended = (playing.time / duration).floor();
        playing.time -= ended * duration;
        playing.loops = playing.loops.wrapping_add(ended as u32);
        if let Some(next) = choose(animation, playing.wanted, roll(id, playing.loops), fallbacks) {
            playing.sequence = next;
        }
    }
}

/// What the instance `id` of `look`, whose model moves as `animation`, plays when it is first seen
/// with it, moving as `motion`, in yards of the model: its animation from a moment of its own; none
/// when it has no sequence for it, nor a fallback.
pub fn start(
    look: LookId,
    animation: &Animation,
    motion: Motion,
    id: u64,
    fallbacks: &HashMap<u16, u16>,
) -> Option<Playing> {
    let (want, _) = wanted(animation, motion);
    let sequence = choose(animation, want, roll(id, 0), fallbacks)?;
    let duration = animation.sequences[sequence].duration.max(1);
    Some(Playing {
        look,
        wanted: want,
        sequence,
        time: (roll(id, u32::MAX) % duration) as f32,
        loops: 0,
        before: None,
    })
}

/// The axes back, left and up of the camera of `view` in the space of an instance placed by
/// `transform`, which its billboards face.
pub fn facing(transform: Mat4, view: Mat4) -> [Vec3; 3] {
    let turn = Mat3::from_mat4(transform);
    let back = Mat3::from_cols(
        turn.x_axis.normalize(),
        turn.y_axis.normalize(),
        turn.z_axis.normalize(),
    )
    .transpose();
    // The rows of the view are the axes of the camera: across, up, back.
    let axes = Mat3::from_mat4(view).transpose();
    [back * axes.z_axis, back * -axes.x_axis, back * axes.y_axis]
}

/// Whether the bones of `animation` move in the sequences kept or the global ones.
pub fn moves(animation: &Animation) -> bool {
    animation.bones.iter().any(|bone| {
        bone.translation.keys.iter().any(|keys| !keys.times.is_empty())
            || bone.rotation.keys.iter().any(|keys| !keys.times.is_empty())
            || bone.scale.keys.iter().any(|keys| !keys.times.is_empty())
    })
}

/// An instance whose bones are computed this frame: its look, how it is posed, its slots of
/// materials and its bones.
struct Job {
    look: Arc<Ready>,
    posing: Posing,
    slots: usize,
    bones: usize,
}

/// The thread of the animations: its state, kept from a frame to the next.
#[derive(Default)]
pub struct Animator {
    playing: HashMap<(u32, u64), Playing>,
    /// The slots and bones of the last frame, kept for their memory.
    posed: Vec<[f32; 4]>,
    /// Whether each look moves: its bones (`moves`) or its materials.
    moving: HashMap<LookId, bool>,
    fallbacks: Option<HashMap<u16, u16>>,
    buffers: [Option<Arc<wgpu::Buffer>>; 3],
    next: usize,
    last_time: Option<f32>,
    clock: f64,
    times: VecDeque<(Instant, Duration)>,
}

impl Animator {
    /// Animates the frame of `time` seconds: the owners of `service`, the looks and the camera of
    /// `scene`; publishes into `scene`.
    pub fn step(
        &mut self,
        time: f32,
        scene: &Mutex<Scene>,
        service: &Service,
        formats: Option<&dyn Formats>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let started = Instant::now();
        let elapsed = self
            .last_time
            .map_or(0.0, |last| ((time - last) * 1000.0).clamp(0.0, 250.0));
        self.last_time = Some(time);
        self.clock += f64::from(elapsed);
        if self.fallbacks.is_none()
            && let Some(Ok(animations)) = formats.map(|formats| formats.animations())
        {
            self.fallbacks = Some(
                animations
                    .iter()
                    .map(|animation| (animation.id as u16, animation.fallback as u16))
                    .collect(),
            );
        }
        let fallbacks = self.fallbacks.clone().unwrap_or_default();
        let (looks, camera, reach) = {
            let scene = lock(scene);
            (scene.looks.clone(), scene.camera, scene.reach)
        };
        let clock = self.clock as u64;
        let mut seen = HashSet::new();
        let mut owners = Vec::new();
        let mut tables: Vec<u32> = Vec::new();
        let mut jobs = Vec::new();
        let (mut vectors, mut bones, mut slots) = (0usize, 0usize, 0usize);
        for (id, look) in looks.iter() {
            self.moving
                .entry(*id)
                .or_insert_with(|| moves(look.animation()) || !look.moving().is_empty());
        }
        for slot in service.owners() {
            let published = slot.published();
            let used = published.instances.len();
            let at = tables.len();
            let mut any = false;
            tables.resize(at + used, 0);
            for group in &published.groups {
                let (Some(look), Some(true)) = (looks.get(&group.look), self.moving.get(&group.look)) else {
                    continue;
                };
                let look = look.clone();
                let animation = look.animation();
                let radius = look.radius() * group.scale;
                let bounds = [
                    group.low - Vec3::splat(radius + MARGIN),
                    group.high + Vec3::splat(radius + MARGIN),
                ];
                let shown = camera.is_some_and(|(view_proj, _, eye)| {
                    let wider = Mat4::from_scale(Vec3::new(1.0 / WIDER, 1.0 / WIDER, 1.0)) * view_proj;
                    nearest(eye, bounds) <= reach * radius.max(1.0) && in_sight(wider, bounds)
                });
                for index in group.first..group.first + group.count {
                    let Some(instance) = published.instances.get(index as usize) else {
                        continue;
                    };
                    let key = (slot.number, instance.id);
                    seen.insert(key);
                    let motion = scaled(instance.motion, instance.transform.x_axis.truncate().length());
                    // Started again when its look changes, a mount or a morph keeping its id: its
                    // sequences counted in the model of its look.
                    let playing = match self.playing.get_mut(&key) {
                        Some(playing) if playing.look == group.look => {
                            advance(playing, animation, motion, elapsed, instance.id, &fallbacks);
                            *playing
                        }
                        _ => match start(group.look, animation, motion, instance.id, &fallbacks) {
                            Some(playing) => {
                                self.playing.insert(key, playing);
                                playing
                            }
                            None => {
                                self.playing.remove(&key);
                                continue;
                            }
                        },
                    };
                    if !shown {
                        continue;
                    }
                    let moment = |sequence: usize, time: f32| Moment {
                        sequence,
                        time: time as u32,
                        clock,
                    };
                    let camera = camera.map(|(_, view, _)| facing(instance.transform, view));
                    let job = Job {
                        look: look.clone(),
                        posing: Posing {
                            moment: moment(playing.sequence, playing.time),
                            before: playing
                                .before
                                .map(|(sequence, time, left, whole)| (moment(sequence, time), left / whole)),
                            camera,
                        },
                        slots: look.moving().len(),
                        bones: animation.bones.len(),
                    };
                    // Its first bone plus one, in vectors: its slots before it.
                    tables[at + index as usize] = (vectors + job.slots * SLOT) as u32 + 1;
                    vectors += job.slots * SLOT + job.bones * BONE;
                    (bones, slots) = (bones + job.bones, slots + job.slots);
                    jobs.push(job);
                    any = true;
                }
            }
            if any {
                owners.push((slot.number, published.clone(), at as u32, used as u32));
            } else {
                tables.truncate(at);
            }
        }
        self.playing.retain(|key, _| seen.contains(key));

        // The slots then the bones of each instance, computed by a slice into its own part of those
        // kept from a frame to the next: its last slot first, its first just before its bones.
        self.posed.resize(vectors, [0.0; 4]);
        {
            let mut rest = &mut self.posed[..];
            let mut parts = Vec::with_capacity(jobs.len());
            for job in &jobs {
                let (part, after) = rest.split_at_mut(job.slots * SLOT + job.bones * BONE);
                parts.push(Mutex::new(part));
                rest = after;
            }
            parallel_for(jobs.len(), 4, |range| {
                let mut matrices = Vec::new();
                for at in range {
                    let (job, mut part) = (&jobs[at], lock(&parts[at]));
                    let (dressed, posed) = part.split_at_mut(job.slots * SLOT);
                    for (slot, moving) in dressed.chunks_mut(SLOT).rev().zip(job.look.moving()) {
                        let values = dress::dressed(job.look.model(), moving, job.posing.moment);
                        slot.copy_from_slice(bytemuck::cast_slice(&values));
                    }
                    let animation = job.look.animation();
                    matrices.clear();
                    matrices.resize(animation.bones.len(), Mat4::IDENTITY);
                    pose::pose(animation, &job.posing, &mut matrices);
                    for (bone, matrix) in posed.chunks_mut(BONE).zip(&matrices) {
                        bone.copy_from_slice(bytemuck::cast_slice(&pose::rows(matrix)));
                    }
                }
            });
        }
        // After the tables, bound from `bones_at`: never an empty range. Written where the GPU
        // takes them from: the staging memory of the queue, or a buffer grown, made mapped.
        let bones_at = tables.len().div_ceil(ALIGN_WORDS) * ALIGN_WORDS;
        let bytes = ((bones_at + vectors.max(BONE) * 4) * 4) as u64;
        let slot = self.next;
        self.next = (self.next + 1) % self.buffers.len();
        let buffer = match self.buffers[slot].clone().filter(|buffer| buffer.size() >= bytes) {
            Some(buffer) => {
                if let Some(size) = wgpu::BufferSize::new(bytes)
                    && let Some(mut view) = queue.write_buffer_with(&buffer, 0, size)
                {
                    fill(view.slice(..), &tables, bones_at, &self.posed);
                    journal::uploaded(bytes);
                }
                buffer
            }
            None => {
                // Room for a quarter more, so that a city filling up does not make it again at
                // each frame.
                let buffer = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("models bones"),
                    size: (bytes + bytes / 4).next_multiple_of(256),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: true,
                }));
                if let Ok(mut view) = buffer.slice(..bytes).get_mapped_range_mut() {
                    fill(view.slice(..), &tables, bones_at, &self.posed);
                }
                buffer.unmap();
                self.buffers[slot] = Some(buffer.clone());
                buffer
            }
        };
        let spent = started.elapsed();
        let now = Instant::now();
        self.times.push_back((now, spent));
        while self
            .times
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > Duration::from_secs(1))
        {
            self.times.pop_front();
        }
        let count = self.times.len().max(1) as u32;
        let mut scene = lock(scene);
        scene.animated = Some(Arc::new(Animated {
            buffer,
            owners,
            bones_at: (bones_at * 4) as u64,
        }));
        scene.animation = AnimationStats {
            instances: jobs.len(),
            bones,
            slots,
            bytes,
            spent: self.times.iter().map(|(_, spent)| *spent).sum::<Duration>() / count,
            longest: self.times.iter().map(|(_, spent)| *spent).max().unwrap_or_default(),
        };
    }
}

#[cfg(test)]
impl Animator {
    /// What the instance `id` of the owner `number` plays.
    pub fn playing(&self, number: u32, id: u64) -> Option<Playing> {
        self.playing.get(&(number, id)).copied()
    }
}

/// Writes into `out` the `tables`, zeros up to `bones_at` words, then the `posed` slots and bones,
/// then zeros.
fn fill(out: WriteOnly<'_, [u8]>, tables: &[u32], bones_at: usize, posed: &[[f32; 4]]) {
    let (mut head, rest) = out.split_at(tables.len() * 4);
    head.copy_from_slice(bytemuck::cast_slice(tables));
    let (mut pad, rest) = rest.split_at((bones_at - tables.len()) * 4);
    pad.fill(0);
    let (mut written, mut rest) = rest.split_at(posed.len() * 16);
    written.copy_from_slice(bytemuck::cast_slice(posed));
    rest.fill(0);
}

/// The thread: a step at each frame of `view`, until `cancelled`.
pub fn run(
    view: &Handle,
    scene: &Mutex<Scene>,
    service: &Service,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cancelled: &dyn Fn() -> bool,
) {
    let mut animator = Animator::default();
    let mut last = 0;
    while !cancelled() {
        let Some(frame) = view.wait_frame(last, WAIT) else {
            continue;
        };
        last = frame.number;
        let formats = lock(&service.formats).clone();
        animator.step(frame.time, scene, service, formats.as_deref(), device, queue);
    }
}
