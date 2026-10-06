//! What moves in an M2 (`formats::Animation`): its sequences, global sequences and bones, and the
//! tracks of its colours, weights and texture transforms, as the description of the format
//! (wowdev.wiki) lays them out, the same in 3.3.5a (version 264) and in the modern models up to
//! version 274. The keys are read only for the sequences played (`PLAYED`) whose keys are in the
//! model, and for the global sequences; the others' are left, the `.anim` files unread. A track
//! whose times and values do not match, a bone whose parent is not a bone or is its own
//! descendant, is said in the faults and left at rest; what lies out of the file refuses the model.

use uniwow_api::formats::{Animation, Bone, Interpolation, Keys, Sequence, TextureTransform, Track};

use crate::m2::{array, items};
use crate::terrain::{f32_at, f32s, u16_at, u32_at};

/// The animations played, as `AnimationData.dbc` numbers them: *Stand*, *Walk* and *Run*.
pub const PLAYED: [u16; 3] = [0, 4, 5];
/// The flags of a sequence: its keys held in the model, an alias of another, its times of blending
/// two fields.
const EMBEDDED: u32 = 0x20;
const ALIAS: u32 = 0x40;
const BLEND_TWO: u32 = 0x200;
/// The sizes of a sequence, a bone, a track, a colour and a texture transform.
const SEQUENCE: usize = 64;
const BONE: usize = 88;
const TRACK: usize = 20;
const COLOUR: usize = 40;
const TRANSFORM: usize = 60;
/// What a fixed-point number of a track is divided by.
const FIXED16: f32 = 32767.0;

/// The animation of the model in `data` (the content of `MD20`); its faults added to `faults`.
pub fn animation(data: &[u8], faults: &mut Vec<String>) -> Result<Animation, String> {
    let mut sequences = items(data, array(data, 0x1C)?, SEQUENCE, "sequences")?
        .as_chunks::<SEQUENCE>()
        .0
        .iter()
        .map(sequence)
        .collect::<Result<Vec<_>, String>>()?;
    // The sequences played whose keys are held, through their aliases.
    for played in 0..sequences.len() {
        if !PLAYED.contains(&sequences[played].id) {
            continue;
        }
        let mut held = played;
        for _ in 0..sequences.len() {
            match sequences[held].alias {
                Some(next) if sequences[held].flags & ALIAS != 0 && usize::from(next) < sequences.len() => {
                    held = usize::from(next);
                }
                _ => break,
            }
        }
        if sequences[held].flags & ALIAS == 0 && sequences[held].flags & EMBEDDED != 0 {
            sequences[held].kept = true;
        }
    }
    let kept: Vec<bool> = sequences.iter().map(|sequence| sequence.kept).collect();
    let globals = items(data, array(data, 0x14)?, 4, "global sequences")?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .collect();
    let mut reader = Reader {
        data,
        kept: &kept,
        faults,
    };
    let (bone_count, bone_offset) = array(data, 0x2C)?;
    items(data, (bone_count, bone_offset), BONE, "bones")?;
    let mut bones = Vec::with_capacity(bone_count);
    for index in 0..bone_count {
        let at = bone_offset + index * BONE;
        let parent = u16_at(data, at + 8)? as i16;
        let parent = match u16::try_from(parent) {
            Ok(parent) if usize::from(parent) < bone_count => Some(parent),
            Ok(parent) => {
                reader.faults.push(format!(
                    "its bone {index}: its parent {parent} of {bone_count} bones left out"
                ));
                None
            }
            Err(_) => None,
        };
        bones.push(Bone {
            key_bone: u32_at(data, at)? as i32,
            flags: u32_at(data, at + 4)?,
            parent,
            pivot: f32s(data, at + 76)?,
            translation: reader.track(at + 16, 12, |value| f32s(value, 0), "a translation")?,
            rotation: reader.track(at + 36, 8, quaternion, "a rotation")?,
            scale: reader.track(at + 56, 12, |value| f32s(value, 0), "a scale")?,
        });
    }
    let order = order(&mut bones, reader.faults);
    let (colour_count, colour_offset) = array(data, 0x48)?;
    items(data, (colour_count, colour_offset), COLOUR, "colours")?;
    let colours = (0..colour_count)
        .map(|index| {
            let at = colour_offset + index * COLOUR;
            Ok((
                reader.track(at, 12, |value| f32s(value, 0), "a colour")?,
                reader.track(at + TRACK, 2, fixed16, "an alpha")?,
            ))
        })
        .collect::<Result<_, String>>()?;
    let (weight_count, weight_offset) = array(data, 0x58)?;
    items(data, (weight_count, weight_offset), TRACK, "weights")?;
    let weights = (0..weight_count)
        .map(|index| reader.track(weight_offset + index * TRACK, 2, fixed16, "a weight"))
        .collect::<Result<_, String>>()?;
    let (transform_count, transform_offset) = array(data, 0x60)?;
    items(
        data,
        (transform_count, transform_offset),
        TRANSFORM,
        "texture transforms",
    )?;
    let transforms = (0..transform_count)
        .map(|index| {
            let at = transform_offset + index * TRANSFORM;
            Ok(TextureTransform {
                translation: reader.track(at, 12, |value| f32s(value, 0), "a translation of a texture")?,
                rotation: reader.track(at + TRACK, 16, |value| f32s(value, 0), "a rotation of a texture")?,
                scale: reader.track(at + 2 * TRACK, 12, |value| f32s(value, 0), "a scale of a texture")?,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Animation {
        sequences,
        globals,
        bones,
        order,
        colours,
        weights,
        transforms,
    })
}

/// A sequence from its 64 bytes, its keys not kept yet.
fn sequence(bytes: &[u8; SEQUENCE]) -> Result<Sequence, String> {
    let flags = u32_at(bytes, 12)?;
    let blend = if flags & BLEND_TWO != 0 {
        [u16_at(bytes, 28)?, u16_at(bytes, 30)?]
    } else {
        let blend = u32_at(bytes, 28)?.min(u32::from(u16::MAX)) as u16;
        [blend, blend]
    };
    let next = u16_at(bytes, 60)? as i16;
    Ok(Sequence {
        id: u16_at(bytes, 0)?,
        variation: u16_at(bytes, 2)?,
        duration: u32_at(bytes, 4)?,
        speed: f32_at(bytes, 8)?,
        flags,
        frequency: u16_at(bytes, 16)? as i16,
        replay: [u32_at(bytes, 20)?, u32_at(bytes, 24)?],
        blend,
        bounds: [f32s(bytes, 32)?, f32s(bytes, 44)?],
        radius: f32_at(bytes, 56)?,
        next: u16::try_from(next).ok(),
        alias: (flags & ALIAS != 0).then_some(u16_at(bytes, 62)?),
        kept: false,
    })
}

/// A rotation of a bone: a quaternion of four numbers of 16 bits, each stored past the middle of
/// its range (wowdev).
fn quaternion(bytes: &[u8]) -> Result<[f32; 4], String> {
    let part = |at| -> Result<f32, String> {
        let stored = i32::from(u16_at(bytes, at)? as i16);
        Ok((if stored < 0 { stored + 32768 } else { stored - 32767 }) as f32 / FIXED16)
    };
    Ok([part(0)?, part(2)?, part(4)?, part(6)?])
}

fn fixed16(bytes: &[u8]) -> Result<f32, String> {
    Ok(f32::from(u16_at(bytes, 0)? as i16) / FIXED16)
}

/// Reads the tracks of a model, the keys of the sequences `kept` only.
struct Reader<'a> {
    data: &'a [u8],
    kept: &'a [bool],
    faults: &'a mut Vec<String>,
}

impl Reader<'_> {
    /// The track at `at`, its values of `size` bytes read by `value`; `what` it is, for its faults.
    fn track<T: Copy + Default>(
        &mut self,
        at: usize,
        size: usize,
        value: impl Fn(&[u8]) -> Result<T, String>,
        what: &str,
    ) -> Result<Track<T>, String> {
        let data = self.data;
        let interpolation = match u16_at(data, at)? {
            0 => Interpolation::Step,
            1 => Interpolation::Linear,
            2 => Interpolation::Bezier,
            3 => Interpolation::Hermite,
            other => {
                self.faults
                    .push(format!("{what} of interpolation {other} left at rest"));
                return Ok(Track::default());
            }
        };
        let global = u16_at(data, at + 2)?;
        let global = (global != 0xFFFF).then_some(global);
        let (time_lists, value_lists) = (array(data, at + 4)?, array(data, at + 12)?);
        let times = items(data, time_lists, 8, "times of a track")?;
        let values = items(data, value_lists, 8, "values of a track")?;
        if time_lists.0 != value_lists.0 {
            self.faults.push(format!(
                "{what} of {} lists of times and {} of values left at rest",
                time_lists.0, value_lists.0
            ));
            return Ok(Track::default());
        }
        let curve = matches!(interpolation, Interpolation::Bezier | Interpolation::Hermite);
        let stride = if curve { 3 * size } else { size };
        let mut keys = Vec::with_capacity(time_lists.0);
        for list in 0..time_lists.0 {
            let wanted = global.is_some() || self.kept.get(list).copied().unwrap_or(false);
            let (time_array, value_array) = (array(times, list * 8)?, array(values, list * 8)?);
            if !wanted {
                keys.push(Keys::default());
                continue;
            }
            if time_array.0 != value_array.0 {
                self.faults.push(format!(
                    "{what}: {} times and {} values in its sequence {list}, left at rest there",
                    time_array.0, value_array.0
                ));
                keys.push(Keys::default());
                continue;
            }
            let read_times = items(data, time_array, 4, "times of a sequence")?
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| u32::from_le_bytes(*bytes))
                .collect();
            let mut read = Keys {
                times: read_times,
                values: Vec::with_capacity(value_array.0),
                tangents: Vec::new(),
            };
            for key in items(data, value_array, stride, "values of a sequence")?.chunks_exact(stride) {
                read.values.push(value(&key[..size])?);
                if curve {
                    read.tangents
                        .push([value(&key[size..2 * size])?, value(&key[2 * size..])?]);
                }
            }
            keys.push(read);
        }
        Ok(Track {
            interpolation,
            global,
            keys,
        })
    }
}

/// The order the bones are computed in, each after its parent; the bone whose parent closes a
/// loop of parents is left without one, and said in `faults`.
fn order(bones: &mut [Bone], faults: &mut Vec<String>) -> Vec<u16> {
    let mut placed = vec![false; bones.len()];
    let mut order = Vec::with_capacity(bones.len());
    for start in 0..bones.len() {
        // The bone and its ancestors not placed yet, the nearest first.
        let mut chain = Vec::new();
        let mut at = start;
        while !placed[at] {
            if chain.contains(&at) {
                let closing = *chain.last().expect("the chain holds its start");
                faults.push(format!(
                    "its bone {closing}: its parent closes a loop, left without one"
                ));
                bones[closing].parent = None;
                break;
            }
            chain.push(at);
            match bones[at].parent {
                Some(parent) => at = usize::from(parent),
                None => break,
            }
        }
        for bone in chain.into_iter().rev() {
            if !placed[bone] {
                placed[bone] = true;
                order.push(bone as u16);
            }
        }
    }
    order
}
