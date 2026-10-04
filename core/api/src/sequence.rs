//! A sequence: its frame rate, its length, and one track per animated property, holding a curve
//! per number of the property, kept in a readable JSON file.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::curve::{Curve, CurveKey, LIMIT};
use crate::{PropertyKind, PropertyValue};

/// The curves of one animated property: one per number (x, y and z of a position), each with keys
/// of its own, at whole frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// The property's path, `<module>/<name>`.
    pub property: String,
    pub kind: PropertyKind,
    pub curves: Vec<Curve>,
}

/// A key of a sequence: its track's property, the number its curve stands for, and its frame.
pub type KeyId = (String, usize, u32);

#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    /// Frames per second.
    pub frame_rate: u32,
    /// In frames.
    pub length: u32,
    pub tracks: Vec<Track>,
}

impl Default for Sequence {
    fn default() -> Self {
        Self {
            frame_rate: 30,
            length: 120,
            tracks: Vec::new(),
        }
    }
}

/// The frame of a key, whose time is a whole frame.
pub fn frame_of(key: &CurveKey) -> u32 {
    key.time.round().max(0.0) as u32
}

impl Track {
    pub fn new(property: &str, kind: PropertyKind) -> Self {
        Self {
            property: property.to_owned(),
            kind,
            curves: vec![Curve::default(); kind.components()],
        }
    }

    /// Sets a key at `frame` on every number, to the numbers of `value`.
    pub fn set_key(&mut self, frame: u32, value: PropertyValue) {
        for (curve, number) in self.curves.iter_mut().zip(value.components()) {
            curve.set_key(f64::from(frame), number);
        }
    }

    /// The frames holding a key of `number`, or of any number.
    pub fn key_frames(&self, number: Option<usize>) -> BTreeSet<u32> {
        self.curves
            .iter()
            .enumerate()
            .filter(|(index, _)| number.is_none_or(|n| n == *index))
            .flat_map(|(_, curve)| curve.keys.iter().map(frame_of))
            .collect()
    }

    /// The value at `frame`, which is fractional while playing; a number without keys keeps its
    /// value in `current`. `None` without any key.
    pub fn evaluate(&self, frame: f64, current: Option<PropertyValue>) -> Option<PropertyValue> {
        if self.curves.iter().all(|curve| curve.keys.is_empty()) {
            return None;
        }
        let base = current.map(|value| value.components()).unwrap_or_default();
        let numbers: Vec<f64> = self
            .curves
            .iter()
            .enumerate()
            .map(|(index, curve)| match curve.keys.first() {
                None => base.get(index).copied().unwrap_or(0.0),
                // A boolean takes the value of the key before.
                Some(first) if self.kind == PropertyKind::Boolean => {
                    curve
                        .keys
                        .iter()
                        .rev()
                        .find(|key| key.time <= frame)
                        .unwrap_or(first)
                        .value
                }
                Some(_) => curve.evaluate(frame),
            })
            .collect();
        Some(PropertyValue::from_components(self.kind, &numbers))
    }

    /// Whether a number has no key: the property's current value then shows through.
    pub fn has_bare_number(&self) -> bool {
        self.curves.iter().any(|curve| curve.keys.is_empty())
    }
}

/// The names of the numbers of a property of `kind`.
pub fn number_names(kind: PropertyKind) -> &'static [&'static str] {
    match kind {
        PropertyKind::Vector => &["x", "y", "z"],
        PropertyKind::Colour => &["red", "green", "blue"],
        PropertyKind::Number | PropertyKind::Boolean => &["value"],
    }
}

/// The colour the keys and the curve of a number are drawn in: red, green and blue for the
/// numbers of a vector or a colour, orange for a lone number.
pub fn number_colour(kind: PropertyKind, number: usize) -> [u8; 3] {
    match (kind.components(), number) {
        (3, 0) => [220, 70, 60],
        (3, 1) => [80, 170, 60],
        (3, _) => [60, 120, 230],
        _ => [230, 160, 40],
    }
}

/// Whether a value may go into a sequence: the rules of `Curve::check`.
pub fn admissible(value: f64) -> bool {
    value.is_finite() && value.abs() <= LIMIT
}

impl Sequence {
    /// Whether its file would be read back: every curve follows the rules of `Curve::check`, its
    /// keys at whole frames from 0.
    pub fn check(&self) -> Result<(), String> {
        for track in &self.tracks {
            for curve in &track.curves {
                curve
                    .check()
                    .map_err(|error| format!("'{}': {error}", track.property))?;
                if curve.keys.iter().any(|key| key.time < 0.0 || key.time.fract() != 0.0) {
                    return Err(format!("'{}': keys go at whole frames from 0", track.property));
                }
            }
        }
        Ok(())
    }

    pub fn track(&self, property: &str) -> Option<&Track> {
        self.tracks.iter().find(|track| track.property == property)
    }

    pub fn track_mut(&mut self, property: &str) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|track| track.property == property)
    }

    /// Every frame holding a key, of every track.
    pub fn key_frames(&self) -> BTreeSet<u32> {
        self.tracks.iter().flat_map(|track| track.key_frames(None)).collect()
    }

    /// Whether this key exists.
    pub fn has_key(&self, (property, number, frame): &KeyId) -> bool {
        self.track(property)
            .and_then(|track| track.curves.get(*number))
            .is_some_and(|curve| curve.keys.iter().any(|key| frame_of(key) == *frame))
    }

    pub fn remove_keys(&mut self, keys: &BTreeSet<KeyId>) {
        for track in &mut self.tracks {
            for (number, curve) in track.curves.iter_mut().enumerate() {
                curve
                    .keys
                    .retain(|key| !keys.contains(&(track.property.clone(), number, frame_of(key))));
                curve.update_tangents();
            }
        }
    }

    /// Moves `keys` by `offset` frames, none before frame 0, with their tangents; a moved key
    /// replaces a key that is not moved at its new frame. Returns where the keys went.
    pub fn move_keys(&mut self, keys: &BTreeSet<KeyId>, offset: i64) -> BTreeSet<KeyId> {
        let lowest = keys.iter().map(|(_, _, frame)| *frame).min().unwrap_or(0);
        let offset = offset.max(-i64::from(lowest));
        let mut moved = BTreeSet::new();
        for track in &mut self.tracks {
            for (number, curve) in track.curves.iter_mut().enumerate() {
                let (going, staying): (Vec<CurveKey>, Vec<CurveKey>) = std::mem::take(&mut curve.keys)
                    .into_iter()
                    .partition(|key| keys.contains(&(track.property.clone(), number, frame_of(key))));
                curve.keys = staying;
                for mut key in going {
                    let frame = u32::try_from(i64::from(frame_of(&key)) + offset).unwrap_or(0);
                    key.time = f64::from(frame);
                    curve.keys.retain(|other| frame_of(other) != frame);
                    curve.keys.push(key);
                    moved.insert((track.property.clone(), number, frame));
                }
                curve.sort();
                curve.update_tangents();
            }
        }
        moved
    }

    /// The text of its file, version 2: JSON with each track's property first, then its curves,
    /// one key per line.
    pub fn to_text(&self) -> String {
        let tracks: Vec<String> = self
            .tracks
            .iter()
            .map(|track| {
                let curves: Vec<String> = track
                    .curves
                    .iter()
                    .map(|curve| {
                        let keys: Vec<String> = curve.to_json()["keys"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|key| format!("            {key}"))
                            .collect();
                        format!("        {{\"keys\": [{}]}}", lines(&keys, "        "))
                    })
                    .collect();
                format!(
                    "    {{\n      \"property\": {},\n      \"kind\": \"{}\",\n      \"curves\": [{}]\n    }}",
                    json!(track.property),
                    track.kind.name(),
                    lines(&curves, "      ")
                )
            })
            .collect();
        format!(
            "{{\n  \"version\": 2,\n  \"frame_rate\": {},\n  \"length\": {},\n  \"tracks\": [{}]\n}}\n",
            self.frame_rate,
            self.length,
            lines(&tracks, "  ")
        )
    }

    /// Reads a sequence of version 2, or of version 1, whose keys held whole values.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let number = |value: &Value, name: &str, low: u32, high: u32| -> Result<u32, String> {
            value[name]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| (low..=high).contains(n))
                .ok_or_else(|| format!("'{name}' must be a whole number from {low} to {high}"))
        };
        let version = value["version"].as_u64().unwrap_or(1);
        if version > 2 {
            return Err(format!("version {version} is newer than this timeline"));
        }
        let sequence = Self {
            frame_rate: number(value, "frame_rate", 1, MAX_FRAME_RATE)?,
            length: number(value, "length", 1, MAX_LENGTH)?,
            tracks: read_tracks(&value["tracks"], version)?,
        };
        sequence.check()?;
        Ok(sequence)
    }
}

/// The tracks of a sequence as JSON, as in its file of version 2:
/// `[{"property", "kind", "curves": [{"keys": [...]}]}]`.
pub fn tracks_to_json(tracks: &[Track]) -> Value {
    Value::Array(
        tracks
            .iter()
            .map(|track| {
                json!({
                    "property": track.property,
                    "kind": track.kind.name(),
                    "curves": track.curves.iter().map(Curve::to_json).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// Reads tracks as `tracks_to_json` writes them, with the rules of a file: those of
/// `Curve::check`, keys at whole frames from 0, one track per property.
pub fn tracks_from_json(value: &Value) -> Result<Vec<Track>, String> {
    let sequence = Sequence {
        tracks: read_tracks(value, 2)?,
        ..Sequence::default()
    };
    sequence.check()?;
    Ok(sequence.tracks)
}

/// The tracks of a file of `version`: version 1 held one whole value per key.
fn read_tracks(value: &Value, version: u64) -> Result<Vec<Track>, String> {
    let mut tracks: Vec<Track> = Vec::new();
    for track in value.as_array().ok_or("'tracks' must be a list")? {
        let property = track["property"].as_str().ok_or("a track has no 'property'")?;
        let kind = track["kind"]
            .as_str()
            .and_then(PropertyKind::from_name)
            .ok_or_else(|| format!("the track of '{property}' has no valid 'kind'"))?;
        let mut read = Track::new(property, kind);
        if version == 2 {
            let curves = track["curves"]
                .as_array()
                .filter(|curves| curves.len() == kind.components())
                .ok_or_else(|| format!("the track of '{property}' needs {} curves", kind.components()))?;
            for (slot, curve) in read.curves.iter_mut().zip(curves) {
                *slot = Curve::from_json(curve).map_err(|error| format!("'{property}': {error}"))?;
            }
        } else {
            for key in track["keys"]
                .as_array()
                .ok_or_else(|| format!("the track of '{property}' has no 'keys' list"))?
            {
                let frame = key["frame"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| format!("'frame' must be a whole number from 0 to {}", u32::MAX))?;
                let value = PropertyValue::from_json(kind, &key["value"])
                    .map_err(|error| format!("'{property}' at frame {frame}: {error}"))?;
                read.set_key(frame, value);
            }
            for curve in &read.curves {
                curve.check().map_err(|error| format!("'{property}': {error}"))?;
            }
        }
        if tracks.iter().any(|other| other.property == property) {
            return Err(format!("'{property}' has two tracks"));
        }
        tracks.push(read);
    }
    Ok(tracks)
}

/// Items one per line, the closing bracket at `indent`; nothing when there are none.
fn lines(items: &[String], indent: &str) -> String {
    if items.is_empty() {
        String::new()
    } else {
        format!("\n{}\n{indent}", items.join(",\n"))
    }
}

pub const MAX_FRAME_RATE: u32 = 240;
pub const MAX_LENGTH: u32 = 1_000_000;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Value, json};

    use crate::curve::TangentMode;
    use crate::{PropertyKind, PropertyValue};

    use super::{Sequence, Track};

    #[test]
    fn keys_before_the_first_frame_or_between_frames_are_refused() {
        let file = |time: f64| {
            json!({ "version": 2, "frame_rate": 30, "length": 120, "tracks": [{
                "property": "cube/opacity", "kind": "number",
                "curves": [{ "keys": [{ "time": time, "value": 1.0 }] }],
            }] })
        };
        assert!(Sequence::from_json(&file(-1.0)).is_err());
        assert!(Sequence::from_json(&file(2.5)).is_err());
        assert!(Sequence::from_json(&file(2.0)).is_ok());
    }

    fn sample() -> Sequence {
        let mut position = Track::new("cube/position", PropertyKind::Vector);
        position.set_key(60, PropertyValue::Vector([4.0, 0.0, 1.0]));
        position.set_key(0, PropertyValue::Vector([0.0, 0.0, 1.0]));
        position.curves[0].set_key(30.0, 3.0);
        position.curves[0].keys[1].mode = TangentMode::Flat;
        position.curves[0].update_tangents();
        let mut visible = Track::new("cube/visible", PropertyKind::Boolean);
        visible.set_key(10, PropertyValue::Boolean(false));
        visible.set_key(20, PropertyValue::Boolean(true));
        Sequence {
            tracks: vec![position, visible],
            ..Sequence::default()
        }
    }

    #[test]
    fn a_sequence_crosses_its_file_unchanged() {
        let sequence = sample();
        let text = sequence.to_text();
        assert!(text.contains("\n            {\"time\":0.0,\"value\":0.0}"), "{text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["version"], 2);
        assert_eq!(Sequence::from_json(&value), Ok(sequence));
        let mut wrong = value;
        wrong["frame_rate"] = 0.into();
        assert!(Sequence::from_json(&wrong).is_err());
    }

    #[test]
    fn a_file_of_version_1_is_read_into_curves() {
        let old = json!({
            "frame_rate": 30,
            "length": 120,
            "tracks": [{
                "property": "cube/position",
                "kind": "vector",
                "keys": [
                    { "frame": 0, "value": [0.0, 0.0, 1.0] },
                    { "frame": 60, "value": [4.0, 0.0, 1.0] }
                ]
            }]
        });
        let read = Sequence::from_json(&old).unwrap();
        let position = read.track("cube/position").unwrap();
        assert_eq!(position.curves.len(), 3);
        assert_eq!(
            position.evaluate(30.0, None),
            Some(PropertyValue::Vector([2.0, 0.0, 1.0]))
        );
    }

    #[test]
    fn each_number_has_keys_of_its_own() {
        let sequence = sample();
        let position = sequence.track("cube/position").unwrap();
        assert_eq!(position.key_frames(Some(0)), BTreeSet::from([0, 30, 60]));
        assert_eq!(position.key_frames(Some(1)), BTreeSet::from([0, 60]));
        assert_eq!(
            position.evaluate(30.0, None),
            Some(PropertyValue::Vector([3.0, 0.0, 1.0]))
        );
        let visible = sequence.track("cube/visible").unwrap();
        assert_eq!(visible.evaluate(19.5, None), Some(PropertyValue::Boolean(false)));
        assert_eq!(visible.evaluate(20.0, None), Some(PropertyValue::Boolean(true)));
        let bare = Track::new("x", PropertyKind::Vector);
        assert_eq!(bare.evaluate(0.0, None), None);
        let mut partial = Track::new("x", PropertyKind::Vector);
        partial.curves[2].set_key(0.0, 5.0);
        assert_eq!(
            partial.evaluate(0.0, Some(PropertyValue::Vector([1.0, 2.0, 3.0]))),
            Some(PropertyValue::Vector([1.0, 2.0, 5.0])),
            "the numbers without keys keep their current values"
        );
    }

    #[test]
    fn moved_keys_keep_their_tangents_and_replace_the_keys_they_land_on() {
        let mut sequence = sample();
        let keys = BTreeSet::from([("cube/position".to_owned(), 0, 30)]);
        let moved = sequence.move_keys(&keys, 30);
        assert_eq!(moved, BTreeSet::from([("cube/position".to_owned(), 0, 60)]));
        let x = &sequence.track("cube/position").unwrap().curves[0];
        assert_eq!(x.keys.len(), 2);
        assert_eq!((x.keys[1].value, x.keys[1].mode), (3.0, TangentMode::Flat));
        let back = sequence.move_keys(&moved, -100);
        assert_eq!(
            back,
            BTreeSet::from([("cube/position".to_owned(), 0, 0)]),
            "not before frame 0"
        );
        sequence.remove_keys(&back);
        assert!(sequence.track("cube/position").unwrap().curves[0].keys.is_empty());
    }
}
