//! The pose of a model at a moment of a sequence: the matrix of each bone, from the model at rest to
//! the model posed, from the tracks of its bones (`formats::Track`). A bone turns about its pivot:
//! moved to it, translated, rotated, scaled, moved back; then placed by its parent's matrix. A
//! rotation between two keys is a slerp, as the client of 3.3.5a interpolates its quaternions: a
//! normalised lerp would bend the arc between keys far apart. Two sequences are mixed while one
//! blends into the next. A billboard faces the camera.

use uniwow_api::formats::{Animation, Interpolation, Track};
use uniwow_api::glam::{Mat3, Mat4, Quat, Vec3};

/// The flags of a bone facing the camera: spherically, or about one of its axes (taken here as
/// spherically).
const BILLBOARD: u32 = 0x08 | 0x10 | 0x20 | 0x40;

/// Where the tracks are read: the sequence (its place), the milliseconds into it, and the
/// milliseconds since the start of every global sequence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moment {
    pub sequence: usize,
    pub time: u32,
    pub clock: u64,
}

/// A value a track interpolates.
pub trait Value: Copy {
    /// Between `self` at 0 and `other` at 1.
    fn mix(self, other: Self, at: f32) -> Self;
    /// Along a curve of Bézier from `self` to `to`, with the control points `out` and `into`.
    fn bezier(self, out: Self, into: Self, to: Self, at: f32) -> Self;
    /// Along a curve of Hermite from `self` to `to`, with the tangents `out` and `into`.
    fn hermite(self, out: Self, into: Self, to: Self, at: f32) -> Self;
}

impl Value for f32 {
    fn mix(self, other: Self, at: f32) -> Self {
        self + (other - self) * at
    }

    fn bezier(self, out: Self, into: Self, to: Self, at: f32) -> Self {
        let rest = 1.0 - at;
        self * rest * rest * rest + 3.0 * out * at * rest * rest + 3.0 * into * at * at * rest + to * at * at * at
    }

    fn hermite(self, out: Self, into: Self, to: Self, at: f32) -> Self {
        let (two, three) = (at * at, at * at * at);
        self * (2.0 * three - 3.0 * two + 1.0)
            + out * (three - 2.0 * two + at)
            + into * (three - two)
            + to * (3.0 * two - 2.0 * three)
    }
}

impl Value for [f32; 3] {
    fn mix(self, other: Self, at: f32) -> Self {
        std::array::from_fn(|axis| self[axis].mix(other[axis], at))
    }

    fn bezier(self, out: Self, into: Self, to: Self, at: f32) -> Self {
        std::array::from_fn(|axis| self[axis].bezier(out[axis], into[axis], to[axis], at))
    }

    fn hermite(self, out: Self, into: Self, to: Self, at: f32) -> Self {
        std::array::from_fn(|axis| self[axis].hermite(out[axis], into[axis], to[axis], at))
    }
}

/// A quaternion: a slerp, by the shorter arc; its curves taken as slerps, which no bone of the
/// client has.
impl Value for [f32; 4] {
    fn mix(self, other: Self, at: f32) -> Self {
        let [x, y, z, w] = self;
        let [a, b, c, d] = other;
        Quat::from_xyzw(x, y, z, w)
            .normalize()
            .slerp(Quat::from_xyzw(a, b, c, d).normalize(), at)
            .to_array()
    }

    fn bezier(self, _out: Self, _into: Self, to: Self, at: f32) -> Self {
        self.mix(to, at)
    }

    fn hermite(self, _out: Self, _into: Self, to: Self, at: f32) -> Self {
        self.mix(to, at)
    }
}

/// The value of `track` at `moment`: by its keys in the sequence, or in its global sequence of the
/// lengths `globals`; none without keys there.
pub fn sample<T: Value>(track: &Track<T>, moment: Moment, globals: &[u32]) -> Option<T> {
    let (keys, time) = match track.global {
        Some(global) => {
            let length = u64::from(globals.get(usize::from(global)).copied().unwrap_or(0).max(1));
            (track.keys.first()?, (moment.clock % length) as u32)
        }
        None => (track.keys.get(moment.sequence)?, moment.time),
    };
    let (times, values) = (&keys.times, &keys.values);
    let last = times.len().checked_sub(1)?;
    if last == 0 || time <= times[0] {
        return values.first().copied();
    }
    if time >= times[last] {
        return values.get(last).copied();
    }
    let next = times.partition_point(|key| *key <= time);
    let (from, to) = (next - 1, next);
    let at = (time - times[from]) as f32 / (times[to] - times[from]).max(1) as f32;
    let (a, b) = (values[from], values[to]);
    Some(match track.interpolation {
        Interpolation::Step => a,
        Interpolation::Linear => a.mix(b, at),
        Interpolation::Bezier => a.bezier(keys.tangents[from][1], keys.tangents[to][0], b, at),
        Interpolation::Hermite => a.hermite(keys.tangents[from][1], keys.tangents[to][0], b, at),
    })
}

/// A bone's translation, rotation and scale at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Local {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
}

impl Local {
    const REST: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    fn mix(self, other: Self, at: f32) -> Self {
        Self {
            translation: self.translation.lerp(other.translation, at),
            rotation: self.rotation.slerp(other.rotation, at),
            scale: self.scale.lerp(other.scale, at),
        }
    }
}

/// How a model is posed: at `moment`, blending from `before` at its weight (1 all `before`, 0
/// none), its billboards facing the camera whose axes back, left and up are given in the space of
/// the model.
#[derive(Clone, Copy, Debug)]
pub struct Posing {
    pub moment: Moment,
    pub before: Option<(Moment, f32)>,
    pub camera: Option<[Vec3; 3]>,
}

/// The matrix of each bone of `animation` posed as `posing` says, written to `out`, in the order of
/// the bones.
pub fn pose(animation: &Animation, posing: &Posing, out: &mut [Mat4]) {
    let globals = &animation.globals;
    for index in &animation.order {
        let index = usize::from(*index);
        let bone = &animation.bones[index];
        let local_at = |moment: Moment| Local {
            translation: sample(&bone.translation, moment, globals).map_or(Vec3::ZERO, Vec3::from),
            rotation: sample(&bone.rotation, moment, globals)
                .map_or(Quat::IDENTITY, |[x, y, z, w]| Quat::from_xyzw(x, y, z, w).normalize()),
            scale: sample(&bone.scale, moment, globals).map_or(Vec3::ONE, Vec3::from),
        };
        let mut local = local_at(posing.moment);
        if let Some((before, weight)) = posing.before {
            local = local.mix(local_at(before), weight);
        }
        let parent = bone.parent.map_or(Mat4::IDENTITY, |parent| out[usize::from(parent)]);
        let pivot = Vec3::from(bone.pivot);
        let mut matrix = if local == Local::REST {
            parent
        } else {
            parent
                * Mat4::from_translation(pivot + local.translation)
                * Mat4::from_rotation_translation(local.rotation, Vec3::ZERO)
                * Mat4::from_scale(local.scale)
                * Mat4::from_translation(-pivot)
        };
        if bone.flags & BILLBOARD != 0
            && let Some([back, left, up]) = posing.camera
        {
            // Its pivot where the bones place it, its axes those of the camera, its scale kept.
            let placed = matrix.transform_point3(pivot);
            let scale = Vec3::new(
                matrix.x_axis.truncate().length(),
                matrix.y_axis.truncate().length(),
                matrix.z_axis.truncate().length(),
            );
            let facing = Mat4::from_mat3(Mat3::from_cols(back, left, up)) * Mat4::from_scale(scale);
            matrix = Mat4::from_translation(placed) * facing * Mat4::from_translation(-pivot);
        }
        out[index] = matrix;
    }
}

/// `matrix` as the shader reads a bone: its first three rows.
pub fn rows(matrix: &Mat4) -> [f32; 12] {
    let mut rows = [0.0; 12];
    for row in 0..3 {
        rows[row * 4..row * 4 + 4].copy_from_slice(&matrix.row(row).to_array());
    }
    rows
}
