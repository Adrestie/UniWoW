//! A sequence: its frame rate, its length, and one track of keys per animated property, kept in a
//! readable JSON file.

use std::collections::BTreeSet;

use uniwow_api::serde_json::{Value, json};
use uniwow_api::{PropertyKind, PropertyValue};

use crate::curve;

/// A key of a track: a whole frame and a value of the track's type.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub frame: u32,
    pub value: PropertyValue,
}

/// The keys of one animated property.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// The property's path, `<module>/<name>`.
    pub property: String,
    pub kind: PropertyKind,
    /// Sorted by frame, at most one per frame.
    pub keys: Vec<Key>,
}

/// A key of a sequence: its track's property and its frame.
pub type KeyId = (String, u32);

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

impl Track {
    pub fn new(property: &str, kind: PropertyKind) -> Self {
        Self {
            property: property.to_owned(),
            kind,
            keys: Vec::new(),
        }
    }

    /// Sets the key of `frame`, replacing the one there.
    pub fn set_key(&mut self, frame: u32, value: PropertyValue) {
        let key = Key { frame, value };
        match self.keys.binary_search_by_key(&frame, |key| key.frame) {
            Ok(index) => self.keys[index] = key,
            Err(index) => self.keys.insert(index, key),
        }
    }

    pub fn key_at(&self, frame: u32) -> Option<&Key> {
        self.keys.iter().find(|key| key.frame == frame)
    }

    /// The value at `frame`, which is fractional while playing; `None` without keys.
    pub fn evaluate(&self, frame: f64) -> Option<PropertyValue> {
        let first = self.keys.first()?;
        if self.kind == PropertyKind::Boolean {
            let held = self.keys.iter().rev().find(|key| f64::from(key.frame) <= frame);
            return Some(held.unwrap_or(first).value);
        }
        let numbers: Vec<f64> = (0..self.kind.components())
            .map(|component| {
                let points: Vec<(f64, f64)> = self
                    .keys
                    .iter()
                    .map(|key| (f64::from(key.frame), key.value.components()[component]))
                    .collect();
                curve::sample(&points, frame)
            })
            .collect();
        Some(PropertyValue::from_components(self.kind, &numbers))
    }
}

impl Sequence {
    pub fn track(&self, property: &str) -> Option<&Track> {
        self.tracks.iter().find(|track| track.property == property)
    }

    pub fn track_mut(&mut self, property: &str) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|track| track.property == property)
    }

    /// Every frame holding a key, of every track.
    pub fn key_frames(&self) -> BTreeSet<u32> {
        self.tracks
            .iter()
            .flat_map(|track| track.keys.iter().map(|key| key.frame))
            .collect()
    }

    pub fn remove_keys(&mut self, keys: &BTreeSet<KeyId>) {
        for track in &mut self.tracks {
            track
                .keys
                .retain(|key| !keys.contains(&(track.property.clone(), key.frame)));
        }
    }

    /// Moves `keys` by `offset` frames, none before frame 0; a moved key replaces a key that is
    /// not moved at its new frame. Returns where the keys went.
    pub fn move_keys(&mut self, keys: &BTreeSet<KeyId>, offset: i64) -> BTreeSet<KeyId> {
        let lowest = keys.iter().map(|(_, frame)| *frame).min().unwrap_or(0);
        let offset = offset.max(-i64::from(lowest));
        let mut moved = BTreeSet::new();
        for track in &mut self.tracks {
            let (going, staying): (Vec<Key>, Vec<Key>) = std::mem::take(&mut track.keys)
                .into_iter()
                .partition(|key| keys.contains(&(track.property.clone(), key.frame)));
            track.keys = staying;
            for key in going {
                let frame = u32::try_from(i64::from(key.frame) + offset).unwrap_or(0);
                track.set_key(frame, key.value);
                moved.insert((track.property.clone(), frame));
            }
        }
        moved
    }

    /// The text of its file: JSON with each track's property first and one key per line.
    pub fn to_text(&self) -> String {
        let tracks: Vec<String> = self
            .tracks
            .iter()
            .map(|track| {
                let keys: Vec<String> = track
                    .keys
                    .iter()
                    .map(|key| {
                        format!(
                            "        {}",
                            json!({ "frame": key.frame, "value": key.value.to_json() })
                        )
                    })
                    .collect();
                format!(
                    "    {{\n      \"property\": {},\n      \"kind\": \"{}\",\n      \"keys\": [{}]\n    }}",
                    json!(track.property),
                    track.kind.name(),
                    lines(&keys, "      ")
                )
            })
            .collect();
        format!(
            "{{\n  \"frame_rate\": {},\n  \"length\": {},\n  \"tracks\": [{}]\n}}\n",
            self.frame_rate,
            self.length,
            lines(&tracks, "  ")
        )
    }

    pub fn from_json(value: &Value) -> Result<Self, String> {
        let number = |value: &Value, name: &str, low: u32, high: u32| -> Result<u32, String> {
            value[name]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| (low..=high).contains(n))
                .ok_or_else(|| format!("'{name}' must be a whole number from {low} to {high}"))
        };
        let mut sequence = Self {
            frame_rate: number(value, "frame_rate", 1, MAX_FRAME_RATE)?,
            length: number(value, "length", 1, MAX_LENGTH)?,
            tracks: Vec::new(),
        };
        for track in value["tracks"].as_array().ok_or("'tracks' must be a list")? {
            let property = track["property"].as_str().ok_or("a track has no 'property'")?;
            let kind = track["kind"]
                .as_str()
                .and_then(PropertyKind::from_name)
                .ok_or_else(|| format!("the track of '{property}' has no valid 'kind'"))?;
            let mut read = Track::new(property, kind);
            for key in track["keys"]
                .as_array()
                .ok_or_else(|| format!("the track of '{property}' has no 'keys' list"))?
            {
                let frame = number(key, "frame", 0, u32::MAX)?;
                let value = PropertyValue::from_json(kind, &key["value"])
                    .map_err(|error| format!("'{property}' at frame {frame}: {error}"))?;
                read.set_key(frame, value);
            }
            if sequence.track(property).is_some() {
                return Err(format!("'{property}' has two tracks"));
            }
            sequence.tracks.push(read);
        }
        Ok(sequence)
    }
}

/// Items one per line, the closing bracket at `indent`; nothing when there are none.
fn lines(items: &[String], indent: &str) -> String {
    if items.is_empty() {
        String::new()
    } else {
        format!(
            "
{}
{indent}",
            items.join(
                ",
"
            )
        )
    }
}

pub const MAX_FRAME_RATE: u32 = 240;
pub const MAX_LENGTH: u32 = 1_000_000;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use uniwow_api::serde_json::{Value, json};
    use uniwow_api::{PropertyKind, PropertyValue};

    use super::{Sequence, Track};

    fn sample() -> Sequence {
        let mut position = Track::new("cube/position", PropertyKind::Vector);
        position.set_key(60, PropertyValue::Vector([4.0, 0.0, 1.0]));
        position.set_key(0, PropertyValue::Vector([0.0, 0.0, 1.0]));
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
        assert!(
            text.contains("\n        {\"frame\":0,\"value\":[0.0,0.0,1.0]}"),
            "{text}"
        );
        let value: Value = uniwow_api::serde_json::from_str(&text).unwrap();
        assert_eq!(Sequence::from_json(&value), Ok(sequence));
        assert_eq!(
            uniwow_api::serde_json::from_str::<Value>(&Sequence::default().to_text()).unwrap()["tracks"],
            json!([])
        );
        let mut wrong = value;
        wrong["frame_rate"] = 0.into();
        assert!(Sequence::from_json(&wrong).is_err());
    }

    #[test]
    fn tracks_are_evaluated_between_and_beyond_their_keys() {
        let sequence = sample();
        let position = sequence.track("cube/position").unwrap();
        assert_eq!(position.evaluate(30.0), Some(PropertyValue::Vector([2.0, 0.0, 1.0])));
        assert_eq!(position.evaluate(90.0), Some(PropertyValue::Vector([4.0, 0.0, 1.0])));
        let visible = sequence.track("cube/visible").unwrap();
        assert_eq!(visible.evaluate(0.0), Some(PropertyValue::Boolean(false)));
        assert_eq!(visible.evaluate(19.5), Some(PropertyValue::Boolean(false)));
        assert_eq!(visible.evaluate(20.0), Some(PropertyValue::Boolean(true)));
        assert_eq!(Track::new("x", PropertyKind::Number).evaluate(0.0), None);
    }

    #[test]
    fn moved_keys_replace_the_keys_they_land_on() {
        let mut sequence = sample();
        let keys = BTreeSet::from([("cube/position".to_owned(), 0)]);
        let moved = sequence.move_keys(&keys, 60);
        assert_eq!(moved, BTreeSet::from([("cube/position".to_owned(), 60)]));
        let position = sequence.track("cube/position").unwrap();
        assert_eq!(position.keys.len(), 1);
        assert_eq!(position.keys[0].value, PropertyValue::Vector([0.0, 0.0, 1.0]));
        let back = sequence.move_keys(&moved, -100);
        assert_eq!(
            back,
            BTreeSet::from([("cube/position".to_owned(), 0)]),
            "not before frame 0"
        );
        sequence.remove_keys(&back);
        assert!(sequence.track("cube/position").unwrap().keys.is_empty());
    }
}
