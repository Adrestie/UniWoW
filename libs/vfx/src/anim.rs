//! A model in time, as the client plays it: the keys of a track read at a moment of a sequence (or
//! of a global sequence), the bones posed, and the phases of a state model played one after the
//! other (birth once, hold looping, decay once).

use crate::m2::{Bone, FBlock, Model, Track};
use glam::{Mat4, Quat, Vec3};

/// A moment of the model: the sequence playing, the time inside it, and the time since the model
/// appeared (the global sequences run on it).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Clock {
    pub sequence: usize,
    /// Milliseconds inside the sequence.
    pub time: f64,
    /// Milliseconds since the model appeared.
    pub global: f64,
}

/// A value read between two keys.
pub trait Lerp: Copy {
    fn lerp(a: Self, b: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        a + (b - a) * t
    }
}

impl<const N: usize> Lerp for [f32; N] {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t)
    }
}

impl Lerp for i16 {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as i16
    }
}

/// Values the client does not blend: they step from key to key.
macro_rules! step {
    ($($t:ty),*) => {$(
        impl Lerp for $t {
            fn lerp(a: Self, b: Self, t: f32) -> Self {
                if t < 1.0 { a } else { b }
            }
        }
    )*};
}

step!(u8, u16, u32);

/// The key set and the time a track reads at a moment.
fn place<T>(track: &Track<T>, clock: &Clock, loops: &[u32]) -> (usize, f64) {
    if track.global_sequence >= 0 {
        let length = loops.get(track.global_sequence as usize).copied().unwrap_or(0);
        let time = if length > 0 {
            clock.global.rem_euclid(f64::from(length))
        } else {
            0.0
        };
        (0, time)
    } else {
        (clock.sequence, clock.time)
    }
}

/// Where a time falls among keys: the key before, the one after, and how far between them.
fn between(times: &[u32], t: f64) -> (usize, usize, f32) {
    let last = times.len() - 1;
    if t <= f64::from(times[0]) {
        return (0, 0, 0.0);
    }
    if t >= f64::from(times[last]) {
        return (last, last, 0.0);
    }
    let next = times.partition_point(|k| f64::from(*k) <= t);
    let (a, b) = (next - 1, next);
    let span = f64::from(times[b] - times[a]);
    let f = if span > 0.0 {
        (t - f64::from(times[a])) / span
    } else {
        0.0
    };
    (a, b, f as f32)
}

/// The value of a track at a moment; `None` when it has no key for it.
pub fn sample<T: Lerp>(track: &Track<T>, clock: &Clock, loops: &[u32]) -> Option<T> {
    let (set, t) = place(track, clock, loops);
    let times = track.timestamps.get(set)?;
    let values = track.values.get(set)?;
    if times.is_empty() || values.len() < times.len() {
        return None;
    }
    let (a, b, f) = between(times, t);
    if track.interpolation == 0 || a == b {
        return Some(values[a]);
    }
    Some(T::lerp(values[a], values[b], f))
}

/// The float a stored quaternion component stands for.
pub fn component(v: i16) -> f32 {
    let v = f32::from(v);
    if v < 0.0 {
        (v + 32768.0) / 32767.0
    } else {
        (v - 32767.0) / 32767.0
    }
}

pub fn quat(q: [i16; 4]) -> Quat {
    Quat::from_xyzw(component(q[0]), component(q[1]), component(q[2]), component(q[3])).normalize()
}

/// A rotation track at a moment.
pub fn sample_rotation(track: &Track<[i16; 4]>, clock: &Clock, loops: &[u32]) -> Option<Quat> {
    let (set, t) = place(track, clock, loops);
    let times = track.timestamps.get(set)?;
    let values = track.values.get(set)?;
    if times.is_empty() || values.len() < times.len() {
        return None;
    }
    let (a, b, f) = between(times, t);
    let qa = quat(values[a]);
    if track.interpolation == 0 || a == b {
        return Some(qa);
    }
    Some(qa.slerp(quat(values[b]), f))
}

/// A particle's value at a fraction of its life (0 to 1).
pub fn sample_life<T: Lerp>(block: &FBlock<T>, life: f32) -> Option<T> {
    if block.values.is_empty() || block.timestamps.len() != block.values.len() {
        return block.values.first().copied();
    }
    let t = f64::from(life.clamp(0.0, 1.0)) * 32767.0;
    let times: Vec<u32> = block.timestamps.iter().map(|v| (*v).max(0) as u32).collect();
    let (a, b, f) = between(&times, t);
    Some(if a == b {
        block.values[a]
    } else {
        T::lerp(block.values[a], block.values[b], f)
    })
}

/// The bone flag that turns it to face the camera.
pub const BILLBOARD: u32 = 0x8;

/// The matrix of a bone at a moment, its parent's not applied.
pub fn local(bone: &Bone, clock: &Clock, loops: &[u32]) -> Mat4 {
    let pivot = Vec3::from(bone.pivot);
    let translation = sample(&bone.translation, clock, loops).map_or(Vec3::ZERO, Vec3::from);
    let rotation = sample_rotation(&bone.rotation, clock, loops).unwrap_or(Quat::IDENTITY);
    let scale = sample(&bone.scale, clock, loops).map_or(Vec3::ONE, Vec3::from);
    Mat4::from_translation(pivot + translation)
        * Mat4::from_quat(rotation)
        * Mat4::from_scale(scale)
        * Mat4::from_translation(-pivot)
}

/// Every bone's matrix in the model at a moment, parents first. A billboarded bone is turned to
/// face `eye` (a direction toward the viewer, in the model's space) about its pivot.
pub fn pose(model: &Model, clock: &Clock, eye: Option<Vec3>) -> Vec<Mat4> {
    let mut out: Vec<Mat4> = Vec::with_capacity(model.bones.len());
    for (index, bone) in model.bones.iter().enumerate() {
        let mut m = local(bone, clock, &model.global_loops);
        let parent = usize::try_from(bone.parent).ok().filter(|p| *p < index);
        if let Some(p) = parent {
            m = out[p] * m;
        }
        if let (true, Some(eye)) = (bone.flags & BILLBOARD != 0, eye) {
            // keep the place and the size, turn the axes: X toward the viewer, Z up as far as can be
            let pivot = m.transform_point3(Vec3::from(bone.pivot));
            let size = m.x_axis.truncate().length();
            let x = eye.normalize_or(Vec3::X);
            let y = Vec3::Z.cross(x).normalize_or(Vec3::Y);
            let z = x.cross(y);
            let turn = Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), glam::Vec4::W);
            m = Mat4::from_translation(pivot)
                * turn
                * Mat4::from_scale(Vec3::splat(size))
                * Mat4::from_translation(-Vec3::from(bone.pivot));
        }
        out.push(m);
    }
    out
}

/// How the sequences of a model play in a preview: one looping sequence, or the phases of a
/// state model (birth once, hold looped `holds` times, decay once).
#[derive(Clone, Debug, PartialEq)]
pub struct Playback {
    /// (sequence, its length in milliseconds, times played).
    pub parts: Vec<(usize, f64, u32)>,
}

/// The ids of the phases of a state model.
pub const BIRTH: u16 = 0;
pub const HOLD: u16 = 158;
pub const DECAY: u16 = 159;

impl Playback {
    /// The playback of a model: its phases when it has a hold, its first sequence looping otherwise.
    pub fn of(model: &Model, holds: u32) -> Self {
        let find = |id: u16| model.sequences.iter().position(|s| s.id == id);
        let length = |at: usize| f64::from(model.sequences[at].duration.max(1));
        let mut parts = Vec::new();
        if let Some(hold) = find(HOLD) {
            if let Some(birth) = find(BIRTH) {
                parts.push((birth, length(birth), 1));
            }
            parts.push((hold, length(hold), holds.max(1)));
            if let Some(decay) = find(DECAY) {
                parts.push((decay, length(decay), 1));
            }
        } else if !model.sequences.is_empty() {
            parts.push((0, length(0), holds.max(1)));
        }
        Self { parts }
    }

    /// The length of the whole playback, in milliseconds.
    pub fn length(&self) -> f64 {
        self.parts
            .iter()
            .map(|(_, length, times)| length * f64::from(*times))
            .sum()
    }

    /// Where the playback is at a time since the model appeared; past its end, at the end of its
    /// last part.
    pub fn clock(&self, at: f64) -> Clock {
        let mut start = 0.0;
        for (index, (sequence, length, times)) in self.parts.iter().enumerate() {
            let span = length * f64::from(*times);
            let last = index + 1 == self.parts.len();
            if at < start + span || last {
                let inside = (at - start).max(0.0);
                let time = if last && inside >= span {
                    *length
                } else {
                    inside.rem_euclid(*length)
                };
                return Clock {
                    sequence: *sequence,
                    time,
                    global: at.max(0.0),
                };
            }
            start += span;
        }
        Clock {
            global: at.max(0.0),
            ..Clock::default()
        }
    }

    /// The parts as (sequence, start, end) on the playback's time, each repeat its own.
    pub fn spans(&self) -> Vec<(usize, f64, f64)> {
        let mut out = Vec::new();
        let mut start = 0.0;
        for (sequence, length, times) in &self.parts {
            for _ in 0..*times {
                out.push((*sequence, start, start + length));
                start += length;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::comp;

    #[test]
    fn keys_are_read_between_them() {
        let track = Track::keyed(1, vec![0, 1000], vec![0.0f32, 10.0]);
        let at = |time| Clock {
            sequence: 0,
            time,
            global: time,
        };
        assert_eq!(sample(&track, &at(500.0), &[]), Some(5.0));
        assert_eq!(sample(&track, &at(2000.0), &[]), Some(10.0));
        let step = Track::keyed(0, vec![0, 1000], vec![0.0f32, 10.0]);
        assert_eq!(sample(&step, &at(999.0), &[]), Some(0.0));
        // a global sequence runs on the time since the model appeared
        let global = Track {
            global_sequence: 0,
            ..Track::keyed(1, vec![0, 1000], vec![0.0f32, 10.0])
        };
        let clock = Clock {
            sequence: 2,
            time: 0.0,
            global: 1250.0,
        };
        assert_eq!(sample(&global, &clock, &[1000]), Some(2.5));
    }

    #[test]
    fn a_stored_quaternion_reads_back() {
        for v in [1.0, 0.0, -0.5, 0.25] {
            assert!((component(comp(v)) - v as f32).abs() < 1e-4, "{v}");
        }
    }

    #[test]
    fn the_phases_play_one_after_the_other() {
        let playback = Playback {
            parts: vec![(0, 1000.0, 1), (1, 500.0, 3), (2, 400.0, 1)],
        };
        assert_eq!(playback.length(), 2900.0);
        assert_eq!(playback.clock(1200.0).sequence, 1);
        assert_eq!(playback.clock(1700.0).time, 200.0);
        assert_eq!(playback.clock(2600.0).sequence, 2);
        assert_eq!(playback.clock(5000.0).time, 400.0, "held at the end");
        assert_eq!(playback.spans().len(), 5);
    }
}
