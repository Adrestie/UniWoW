//! The keys of the part of an effect chosen, as tracks of the Timeline, and back.
//!
//! The Timeline's time is the effect's playback, its hold played once: a looping effect from 0 to
//! its duration; a state model its birth, its hold, its decay, one after the other (its segments).
//! A value over the sequence is a track: one value is a key at the start; keys over the loop sit
//! on the hold (the loop of a state model's looping tracks); keys by phase each on its phase.
//! Every key is linear, as forge writes them (the M2's interpolation 1): the Timeline's tangents
//! are not kept. A particle's values over its life go to their own curves, the life from 0 to 1.

use uniwow_api::curve::{Curve, CurveKey, Side, SideMode, TangentMode};
use uniwow_api::sequence::{Sequence, Track};
use uniwow_api::timeline::{EXTERNAL, Presented, Segment};
use uniwow_api::vfx::json::Value;
use uniwow_api::{PropertyKind, PropertyValue};

use crate::doc::Node;
use crate::schema::{self, Kind};

/// Frames a second of the effect's tracks: the Timeline's finest.
pub const RATE: u32 = 240;
/// The keys of a particle's life, at most (the client's).
pub const LIFE_KEYS: usize = 8;

fn frame(ms: f64) -> u32 {
    (ms * f64::from(RATE) / 1000.0).round().max(0.0) as u32
}

/// The frames of a stretch of the playback: its first, and its last (the frame before the next
/// phase's first, so that a key belongs to one phase alone).
fn span(start: f64, length: f64, phased: bool) -> (f64, f64) {
    let first = f64::from(frame(start));
    let end = f64::from(frame(start + length));
    (
        first,
        if phased {
            (end - 1.0).max(first + 1.0)
        } else {
            end.max(first + 1.0)
        },
    )
}

fn at_frame((first, last): (f64, f64), t: f64) -> f64 {
    (first + t.clamp(0.0, 1.0) * (last - first)).round()
}

fn share((first, last): (f64, f64), frame: f64) -> f64 {
    ((frame - first) / (last - first)).clamp(0.0, 1.0)
}

/// The phases of the playback in the Timeline: (name, start ms, length ms); and the stretch the
/// keys over the loop sit on.
pub struct Layout {
    pub phases: Vec<(String, f64, f64)>,
    pub looped: (f64, f64),
    pub total: f64,
}

pub fn layout(root: &Value) -> Layout {
    let duration = root.get("duration").and_then(Value::as_f64).unwrap_or(2000.0);
    let phases: Vec<(String, f64)> = root
        .get("sequences")
        .and_then(Value::as_map)
        .filter(|m| !m.is_empty())
        .map(|m| {
            ["birth", "hold", "decay"]
                .iter()
                .filter_map(|p| {
                    m.iter()
                        .find(|(k, _)| k == p)
                        .map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(0.0)))
                })
                .collect()
        })
        .unwrap_or_default();
    if phases.is_empty() {
        return Layout {
            phases: Vec::new(),
            looped: (0.0, duration),
            total: duration,
        };
    }
    let mut start = 0.0;
    let mut out = Vec::new();
    let mut looped = (0.0, duration);
    for (name, length) in phases {
        if name == "hold" {
            looped = (start, length);
        }
        out.push((name, start, length));
        start += length;
    }
    Layout {
        phases: out,
        looped,
        total: start,
    }
}

/// The fields of a part that change over the sequence: the Timeline's tracks.
pub fn animated(node: &Node) -> Vec<schema::Field> {
    let fields: &[schema::Field] = match node {
        Node::Effect | Node::Texture(_) => &[],
        Node::Bone(_) => schema::BONE,
        Node::Emitter(_) => schema::EMITTER,
        Node::Ribbon(_) => schema::RIBBON,
        Node::Mesh(_) => schema::MESH,
        Node::Light(_) => schema::LIGHT,
    };
    fields
        .iter()
        .filter(|f| matches!(f.kind, Kind::Animated | Kind::AnimatedColor))
        .copied()
        .collect()
}

fn linear(time: f64, value: f64) -> CurveKey {
    let side = Side {
        mode: SideMode::Linear,
        slope: 0.0,
        weight: None,
    };
    CurveKey {
        time,
        value,
        mode: TangentMode::Broken,
        left: side,
        right: side,
    }
}

fn components(v: &Value, colour: bool) -> Option<Vec<f64>> {
    if colour {
        let rgb = uniwow_api::vfx::forge::hex_rgb(v).ok()?;
        return Some(rgb.iter().map(|c| c / 255.0).collect());
    }
    v.as_f64().map(|x| vec![x])
}

/// The path of the track of `key` of the part.
pub fn property(node: &Node, key: &str) -> String {
    format!("{EXTERNAL}{node:?}.{key}")
}

/// The tracks of the part chosen, its keys at their frames, the phases as segments.
pub fn present(root: &Value, node: &Node, title: &str, version: u64) -> Presented {
    let layout = layout(root);
    let part = node.get(root);
    let mut tracks = Vec::new();
    for field in animated(node) {
        let colour = field.kind == Kind::AnimatedColor;
        let kind = if colour {
            PropertyKind::Colour
        } else {
            PropertyKind::Number
        };
        let mut track = Track::new(&property(node, field.key), kind);
        let given = part.and_then(|p| p.get(field.key));
        let default = uniwow_api::vfx::json::parse(field.default).unwrap_or(Value::Null);
        let value = given.unwrap_or(&default);
        let phased = !layout.phases.is_empty();
        let mut put = |at: f64, v: &Value| {
            if let Some(numbers) = components(v, colour) {
                for (curve, number) in track.curves.iter_mut().zip(numbers) {
                    let time = at;
                    curve.keys.retain(|k| k.time != time);
                    curve.keys.push(linear(time, number));
                    curve.keys.sort_by(|a, b| a.time.total_cmp(&b.time));
                }
            }
        };
        let keys = |v: &Value| -> Vec<(f64, Value)> {
            v.as_list()
                .unwrap_or_default()
                .iter()
                .filter_map(|k| match k.as_list() {
                    Some([t, v, ..]) => Some((t.as_f64()?, v.clone())),
                    _ => None,
                })
                .collect()
        };
        match value {
            Value::Map(phases) => {
                for (name, start, length) in &layout.phases {
                    let stretch = span(*start, *length, phased);
                    match phases.iter().find(|(p, _)| p == name).map(|(_, v)| v) {
                        Some(v) if v.is_keys() => {
                            for (t, v) in keys(v) {
                                put(at_frame(stretch, t), &v);
                            }
                        }
                        Some(v) => put(stretch.0, v),
                        None => {}
                    }
                }
            }
            v if v.is_keys() => {
                let stretch = span(layout.looped.0, layout.looped.1, phased);
                for (t, v) in keys(v) {
                    put(at_frame(stretch, t), &v);
                }
            }
            v => put(0.0, v),
        }
        tracks.push(track);
    }
    let segments = layout
        .phases
        .iter()
        .map(|(name, start, length)| Segment {
            label: name.clone(),
            start: frame(*start),
            end: frame(start + length),
        })
        .collect();
    Presented {
        title: title.to_owned(),
        sequence: Sequence {
            frame_rate: RATE,
            length: frame(layout.total).max(1),
            tracks,
        },
        segments,
        version,
    }
}

/// A number as the description writes it: whole numbers as integers, the rest to 4 places.
fn number(v: f64) -> Value {
    let rounded = (v * 10_000.0).round() / 10_000.0;
    if rounded.fract() == 0.0 && rounded.abs() < 1e9 {
        Value::Int(rounded as i64)
    } else {
        Value::Float(rounded)
    }
}

fn hex(rgb: [f64; 3]) -> Value {
    let c = rgb.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8);
    Value::Str(format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]))
}

/// The keys of a track: (frame, value), the numbers of a colour together.
fn track_keys(track: &Track, colour: bool) -> Vec<(f64, Value)> {
    let mut frames: Vec<f64> = track
        .curves
        .iter()
        .flat_map(|c| c.keys.iter().map(|k| k.time.round()))
        .collect();
    frames.sort_by(f64::total_cmp);
    frames.dedup();
    frames
        .into_iter()
        .map(|f| {
            let value = match track.evaluate(f, None) {
                Some(PropertyValue::Colour(rgb)) if colour => hex(rgb),
                Some(other) => number(other.components()[0]),
                None => Value::Int(0),
            };
            (f, value)
        })
        .collect()
}

fn keyed(keys: &[(f64, Value)], stretch: (f64, f64)) -> Value {
    Value::List(
        keys.iter()
            .map(|(f, v)| Value::List(vec![number(share(stretch, *f)), v.clone()]))
            .collect(),
    )
}

/// The description with the part's keys as the Timeline left them; none when nothing changed.
/// A key left out of the description stays out while its track holds its default alone.
pub fn apply(root: &Value, node: &Node, edited: &Sequence) -> Option<Value> {
    let layout = layout(root);
    let phased = !layout.phases.is_empty();
    let mut value = root.clone();
    let part = node.get_mut(&mut value)?;
    for field in animated(node) {
        let colour = field.kind == Kind::AnimatedColor;
        let Some(track) = edited.tracks.iter().find(|t| t.property == property(node, field.key)) else {
            continue;
        };
        let keys = track_keys(track, colour);
        let before = part.get(field.key).cloned();
        let within = |stretch: (f64, f64)| -> Vec<(f64, Value)> {
            keys.iter()
                .filter(|(f, _)| *f >= stretch.0 - 0.5 && *f <= stretch.1 + 0.5)
                .cloned()
                .collect()
        };
        // keys of one value alone: that value
        let simple = |on: &[(f64, Value)], stretch: (f64, f64)| -> Value {
            match on {
                [(_, v)] => v.clone(),
                [(_, first), rest @ ..] if rest.iter().all(|(_, v)| v == first) => first.clone(),
                _ => keyed(on, stretch),
            }
        };
        let new = if keys.is_empty() {
            None
        } else if !phased {
            let stretch = span(0.0, layout.total, false);
            Some(simple(&within(stretch), stretch))
        } else if before.as_ref().is_some_and(Value::is_keys) {
            // keys over the loop: those on the hold
            let stretch = span(layout.looped.0, layout.looped.1, true);
            Some(keyed(&within(stretch), stretch))
        } else {
            let phases: Vec<(String, Value)> = layout
                .phases
                .iter()
                .filter_map(|(name, start, length)| {
                    let stretch = span(*start, *length, true);
                    let on = within(stretch);
                    (!on.is_empty()).then(|| (name.clone(), simple(&on, stretch)))
                })
                .collect();
            let first = phases.first().map(|(_, v)| v.clone());
            match first {
                // one value through every phase: that value, unless keys by phase were asked
                Some(v)
                    if !v.is_keys()
                        && phases.iter().all(|(_, p)| *p == v)
                        && !matches!(before, Some(Value::Map(_))) =>
                {
                    Some(v)
                }
                _ => Some(Value::Map(phases)),
            }
        };
        let default = uniwow_api::vfx::json::parse(field.default).ok();
        match new {
            Some(v) if before.is_none() && default.as_ref() == Some(&v) => {}
            Some(v) if Some(&v) != before.as_ref() => part.set(field.key, v),
            None if before.is_some() => {
                part.remove(field.key);
            }
            _ => {}
        }
    }
    (value != *root).then_some(value)
}
/// A particle's track over its life as curves (life from 0 to 1): one for a value, three for a
/// colour (0 to 1 each).
pub fn life_curves(value: &Value, colour: bool) -> Vec<Curve> {
    let keys: Vec<(f64, Value)> = if value.is_keys() {
        value
            .as_list()
            .unwrap_or_default()
            .iter()
            .filter_map(|k| match k.as_list() {
                Some([t, v, ..]) => Some((t.as_f64()?, v.clone())),
                _ => None,
            })
            .collect()
    } else {
        vec![(0.0, value.clone()), (1.0, value.clone())]
    };
    let count = if colour { 3 } else { 1 };
    let mut curves = vec![Curve::default(); count];
    for (t, v) in keys {
        // a size of two numbers is drawn by its first
        let numbers = match &v {
            Value::List(items) if !colour => items.first().and_then(Value::as_f64).map(|x| vec![x]),
            _ => components(&v, colour),
        };
        if let Some(numbers) = numbers {
            for (curve, n) in curves.iter_mut().zip(numbers) {
                curve.keys.push(linear(t, n));
            }
        }
    }
    curves
}

/// The track over the life from its curves, 8 keys at most (the client's): none past that.
pub fn life_value(curves: &[Curve], colour: bool) -> Option<Value> {
    let mut times: Vec<f64> = curves
        .iter()
        .flat_map(|c| c.keys.iter().map(|k| k.time.clamp(0.0, 1.0)))
        .collect();
    times.sort_by(f64::total_cmp);
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
    if times.len() > LIFE_KEYS || times.is_empty() {
        return None;
    }
    let at = |curve: &Curve, t: f64| -> f64 {
        let keys = &curve.keys;
        match keys.iter().position(|k| k.time >= t) {
            None => keys.last().map_or(0.0, |k| k.value),
            Some(0) => keys[0].value,
            Some(i) => {
                let (a, b) = (&keys[i - 1], &keys[i]);
                let f = (t - a.time) / (b.time - a.time).max(1e-9);
                a.value + (b.value - a.value) * f
            }
        }
    };
    Some(Value::List(
        times
            .iter()
            .map(|t| {
                let v = if colour {
                    hex([at(&curves[0], *t), at(&curves[1], *t), at(&curves[2], *t)])
                } else {
                    number(at(&curves[0], *t))
                };
                Value::List(vec![number(*t), v])
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::vfx::json;

    #[test]
    fn keys_go_to_the_timeline_and_come_back() {
        let root = json::parse(
            r##"{"name": "a", "duration": 2000, "emitters": [{"rate": [[0, 10], [0.5, 30], [1, 10]], "speed": 2}]}"##,
        )
        .unwrap();
        let node = Node::Emitter(0);
        let presented = present(&root, &node, "a", 1);
        assert_eq!(presented.sequence.length, 480);
        let rate = presented
            .sequence
            .tracks
            .iter()
            .find(|t| t.property.ends_with(".rate"))
            .unwrap();
        let frames: Vec<f64> = rate.curves[0].keys.iter().map(|k| k.time).collect();
        assert_eq!(frames, [0.0, 240.0, 480.0]);
        assert_eq!(
            presented.sequence.tracks.len(),
            3,
            "rate, speed, gravity: each its track"
        );
        // nothing changed: nothing comes back
        assert_eq!(apply(&root, &node, &presented.sequence), None);
        // a key moved: the description follows
        let mut edited = presented.sequence.clone();
        let rate = edited
            .tracks
            .iter_mut()
            .find(|t| t.property.ends_with(".rate"))
            .unwrap();
        rate.curves[0].keys[1].time = 120.0;
        let after = apply(&root, &node, &edited).unwrap();
        let keys = node.get(&after).unwrap().get("rate").unwrap();
        assert_eq!(
            json::write(keys, json::Layout::default()).replace(['\n', ' '], ""),
            "[[0,10],[0.25,30],[1,10]]"
        );
        // a key added to a single value makes keys
        let speed = edited
            .tracks
            .iter_mut()
            .find(|t| t.property.ends_with(".speed"))
            .unwrap();
        speed.curves[0].keys.push(linear(240.0, 5.0));
        let after = apply(&root, &node, &edited).unwrap();
        assert!(node.get(&after).unwrap().get("speed").unwrap().is_keys());
    }

    #[test]
    fn the_phases_are_segments_and_their_keys_stay_in_them() {
        let root = json::parse(
            r#"{"name": "a", "duration": 2400, "sequences": {"birth": 1600, "hold": 2400, "decay": 900},
            "meshes": [{"shape": "plane", "alpha": {"birth": [[0, 0], [1, 0.9]], "hold": 0.9, "decay": [[0, 0.9], [1, 0]]}}]}"#,
        )
        .unwrap();
        let node = Node::Mesh(0);
        let presented = present(&root, &node, "a", 1);
        assert_eq!(presented.segments.len(), 3);
        assert_eq!(presented.segments[1].start, frame(1600.0));
        assert_eq!(
            apply(&root, &node, &presented.sequence),
            None,
            "the same keys, the same description"
        );
    }

    #[test]
    fn a_particle_life_keeps_eight_keys_at_most() {
        let value = json::parse(r##"[[0, "#ff0000"], [1, "#0000ff"]]"##).unwrap();
        let curves = life_curves(&value, true);
        assert_eq!(curves.len(), 3);
        assert_eq!(life_value(&curves, true).unwrap(), value);
        let many: Vec<Curve> = vec![Curve {
            keys: (0..9).map(|k| linear(f64::from(k) / 8.0, 1.0)).collect(),
        }];
        assert_eq!(life_value(&many, false), None);
    }
}
