//! Curves of keys with tangents, as the animation curves of Unity, shared by the module that edits
//! them and by the modules that evaluate them.

use serde_json::{Map, Value, json};

/// How the tangents of a key are set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TangentMode {
    /// Smooth, never overshooting the neighbouring keys.
    ClampedAuto,
    /// Smooth; may overshoot.
    Auto,
    /// Set by hand, the same slope on both sides.
    FreeSmooth,
    Flat,
    /// Each side on its own, as its `SideMode` says.
    Broken,
}

/// A side of a broken key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideMode {
    /// Set by hand.
    Free,
    /// Pointing at the neighbouring key.
    Linear,
    /// The value holds until the next key.
    Constant,
}

/// One side of a key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Side {
    /// Used when the key is broken.
    pub mode: SideMode,
    pub slope: f64,
    /// The length of the handle when weighted: a share of the time to the neighbouring key, from 0
    /// to 1.
    pub weight: Option<f64>,
}

/// The length of a handle that is not weighted.
pub const DEFAULT_WEIGHT: f64 = 1.0 / 3.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveKey {
    pub time: f64,
    pub value: f64,
    pub mode: TangentMode,
    pub left: Side,
    pub right: Side,
}

/// Keys sorted by time; between two keys, a cubic shaped by their tangents.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Curve {
    pub keys: Vec<CurveKey>,
}

const FREE: Side = Side {
    mode: SideMode::Free,
    slope: 0.0,
    weight: None,
};

impl CurveKey {
    /// A key in Clamped Auto mode.
    pub fn new(time: f64, value: f64) -> Self {
        Self {
            time,
            value,
            mode: TangentMode::ClampedAuto,
            left: FREE,
            right: FREE,
        }
    }

    /// Whether the value holds from this key to the next.
    fn holds_after(&self) -> bool {
        self.mode == TangentMode::Broken && self.right.mode == SideMode::Constant
    }

    fn holds_before(&self) -> bool {
        self.mode == TangentMode::Broken && self.left.mode == SideMode::Constant
    }
}

impl Curve {
    /// Sets the value of the key at `time`, or adds one there; returns its index.
    pub fn set_key(&mut self, time: f64, value: f64) -> usize {
        let index = match self.keys.iter().position(|key| key.time == time) {
            Some(index) => {
                self.keys[index].value = value;
                index
            }
            None => {
                let index = self.keys.partition_point(|key| key.time < time);
                self.keys.insert(index, CurveKey::new(time, value));
                index
            }
        };
        self.update_tangents();
        index
    }

    /// Puts the keys back in time order, after their times changed.
    pub fn sort(&mut self) {
        self.keys.sort_by(|a, b| a.time.total_cmp(&b.time));
    }

    /// Computes again the slopes the modes set: the automatic ones, Flat, and the Linear sides.
    pub fn update_tangents(&mut self) {
        for index in 0..self.keys.len() {
            let previous = index.checked_sub(1).map(|p| self.keys[p]);
            let next = self.keys.get(index + 1).copied();
            let key = &mut self.keys[index];
            let towards = |other: Option<CurveKey>| {
                other.map_or(0.0, |other| (other.value - key.value) / (other.time - key.time))
            };
            match key.mode {
                TangentMode::ClampedAuto => {
                    let slope = clamped_slope(previous, *key, next);
                    key.left.slope = slope;
                    key.right.slope = slope;
                }
                TangentMode::Auto => {
                    let slope = match (previous, next) {
                        (Some(p), Some(n)) => (n.value - p.value) / (n.time - p.time),
                        _ => 0.0,
                    };
                    key.left.slope = slope;
                    key.right.slope = slope;
                }
                TangentMode::Flat => {
                    key.left.slope = 0.0;
                    key.right.slope = 0.0;
                }
                TangentMode::FreeSmooth => key.right.slope = key.left.slope,
                TangentMode::Broken => {
                    if key.left.mode == SideMode::Linear {
                        key.left.slope = towards(previous);
                    }
                    if key.right.mode == SideMode::Linear {
                        key.right.slope = towards(next);
                    }
                }
            }
        }
    }

    /// The value at `time`: the first key's before it, the last key's after it.
    pub fn evaluate(&self, time: f64) -> f64 {
        let (Some(first), Some(last)) = (self.keys.first(), self.keys.last()) else {
            return 0.0;
        };
        if time <= first.time {
            return first.value;
        }
        if time >= last.time {
            return last.value;
        }
        let index = self.keys.partition_point(|key| key.time <= time) - 1;
        segment(&self.keys[index], &self.keys[index + 1], time)
    }

    pub fn to_json(&self) -> Value {
        json!({ "keys": self.keys.iter().map(key_json).collect::<Vec<_>>() })
    }

    /// Reads a curve; its keys are sorted and their slopes computed.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let mut curve = Curve {
            keys: value["keys"]
                .as_array()
                .ok_or("a curve has a 'keys' list")?
                .iter()
                .map(key_from_json)
                .collect::<Result<_, _>>()?,
        };
        curve.sort();
        curve.update_tangents();
        Ok(curve)
    }
}

/// The slope of Clamped Auto: flat at either end and at each peak or trough; else the mean of
/// Fritsch and Butland, which keeps the curve between the keys' values.
fn clamped_slope(previous: Option<CurveKey>, key: CurveKey, next: Option<CurveKey>) -> f64 {
    let (Some(p), Some(n)) = (previous, next) else {
        return 0.0;
    };
    let (h0, h1) = (key.time - p.time, n.time - key.time);
    let (d0, d1) = ((key.value - p.value) / h0, (n.value - key.value) / h1);
    if d0 * d1 <= 0.0 {
        return 0.0;
    }
    3.0 * (h0 + h1) / ((2.0 * h1 + h0) / d0 + (h1 + 2.0 * h0) / d1)
}

/// The value at `time` between keys `a` and `b`: a cubic Bézier in time and value whose inner
/// points follow the slopes, at the handles' lengths. Without weights it is the cubic of Hermite.
fn segment(a: &CurveKey, b: &CurveKey, time: f64) -> f64 {
    if a.holds_after() || b.holds_before() {
        return a.value;
    }
    let width = b.time - a.time;
    let w0 = a.right.weight.unwrap_or(DEFAULT_WEIGHT).clamp(0.0, 1.0) * width;
    let w1 = b.left.weight.unwrap_or(DEFAULT_WEIGHT).clamp(0.0, 1.0) * width;
    let xs = [a.time, a.time + w0, b.time - w1, b.time];
    let ys = [
        a.value,
        a.value + a.right.slope * w0,
        b.value - b.left.slope * w1,
        b.value,
    ];
    // Without weights the time grows evenly with u: the cubic of Hermite, exactly.
    if a.right.weight.is_none() && b.left.weight.is_none() {
        return bezier(ys, (time - a.time) / width);
    }
    // Else it grows with u as long as the handles stay within the segment: bisection finds it.
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..60 {
        let middle = (low + high) / 2.0;
        if bezier(xs, middle) < time {
            low = middle;
        } else {
            high = middle;
        }
    }
    bezier(ys, (low + high) / 2.0)
}

fn bezier(points: [f64; 4], u: f64) -> f64 {
    let v = 1.0 - u;
    v * v * v * points[0] + 3.0 * v * v * u * points[1] + 3.0 * v * u * u * points[2] + u * u * u * points[3]
}

const MODES: [(TangentMode, &str); 5] = [
    (TangentMode::ClampedAuto, "clamped_auto"),
    (TangentMode::Auto, "auto"),
    (TangentMode::FreeSmooth, "free_smooth"),
    (TangentMode::Flat, "flat"),
    (TangentMode::Broken, "broken"),
];

const SIDES: [(SideMode, &str); 3] = [
    (SideMode::Free, "free"),
    (SideMode::Linear, "linear"),
    (SideMode::Constant, "constant"),
];

/// A key as JSON, without what its mode computes: the slopes are written for the modes set by
/// hand, the sides' modes for a broken key, the weights when there are some.
fn key_json(key: &CurveKey) -> Value {
    let mut object = Map::new();
    object.insert("time".into(), json!(key.time));
    object.insert("value".into(), json!(key.value));
    if key.mode != TangentMode::ClampedAuto {
        let name = MODES
            .iter()
            .find(|(mode, _)| *mode == key.mode)
            .map_or("", |(_, name)| name);
        object.insert("mode".into(), json!(name));
    }
    let by_hand = |side: &Side| {
        key.mode == TangentMode::FreeSmooth || (key.mode == TangentMode::Broken && side.mode == SideMode::Free)
    };
    for (name, side) in [("left", &key.left), ("right", &key.right)] {
        let mut written = Map::new();
        if key.mode == TangentMode::Broken && side.mode != SideMode::Free {
            let mode = SIDES
                .iter()
                .find(|(mode, _)| *mode == side.mode)
                .map_or("", |(_, name)| name);
            written.insert("mode".into(), json!(mode));
        }
        if by_hand(side) {
            written.insert("slope".into(), json!(side.slope));
        }
        if let Some(weight) = side.weight {
            written.insert("weight".into(), json!(weight));
        }
        if !written.is_empty() {
            object.insert(name.into(), Value::Object(written));
        }
    }
    Value::Object(object)
}

fn key_from_json(value: &Value) -> Result<CurveKey, String> {
    let number = |name: &str| {
        value[name]
            .as_f64()
            .ok_or_else(|| format!("a key has no number '{name}'"))
    };
    let mut key = CurveKey::new(number("time")?, number("value")?);
    if let Some(name) = value["mode"].as_str() {
        key.mode = MODES
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(mode, _)| *mode)
            .ok_or_else(|| format!("unknown tangent mode '{name}'"))?;
    }
    for (name, side) in [("left", &mut key.left), ("right", &mut key.right)] {
        let written = &value[name];
        if let Some(mode) = written["mode"].as_str() {
            side.mode = SIDES
                .iter()
                .find(|(_, known)| *known == mode)
                .map(|(mode, _)| *mode)
                .ok_or_else(|| format!("unknown side mode '{mode}'"))?;
        }
        if let Some(slope) = written["slope"].as_f64() {
            side.slope = slope;
        }
        side.weight = written["weight"].as_f64().map(|w| w.clamp(0.0, 1.0));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::{Curve, CurveKey, SideMode, TangentMode};

    fn curve(points: &[(f64, f64)]) -> Curve {
        let mut curve = Curve::default();
        for (time, value) in points {
            curve.set_key(*time, *value);
        }
        curve
    }

    fn within(curve: &Curve, from: f64, to: f64, low: f64, high: f64) -> bool {
        (0..=100).all(|step| {
            let value = curve.evaluate(from + (to - from) * f64::from(step) / 100.0);
            value >= low - 1e-9 && value <= high + 1e-9
        })
    }

    #[test]
    fn clamped_auto_eases_and_never_overshoots_but_auto_may() {
        let mut keys = curve(&[(0.0, 0.0), (10.0, 9.0), (12.0, 10.0), (40.0, -4.0)]);
        assert_eq!(keys.evaluate(-1.0), 0.0);
        assert_eq!(keys.evaluate(50.0), -4.0);
        assert!(within(&keys, 10.0, 12.0, 9.0, 10.0));
        for key in &mut keys.keys {
            key.mode = TangentMode::Auto;
        }
        keys.update_tangents();
        assert!(!within(&keys, 10.0, 12.0, 9.0, 10.0), "Auto overshoots");
        let two = curve(&[(0.0, 0.0), (60.0, 10.0)]);
        assert!((two.evaluate(30.0) - 5.0).abs() < 1e-9);
        assert!(two.evaluate(6.0) < 1.0, "it starts slowly");
    }

    #[test]
    fn broken_sides_hold_or_point_at_their_neighbours() {
        let mut keys = curve(&[(0.0, 0.0), (10.0, 10.0)]);
        keys.keys[0].mode = TangentMode::Broken;
        keys.keys[0].right.mode = SideMode::Constant;
        assert_eq!(keys.evaluate(9.9), 0.0);
        keys.keys[0].right.mode = SideMode::Linear;
        keys.keys[1].mode = TangentMode::Broken;
        keys.keys[1].left.mode = SideMode::Linear;
        keys.update_tangents();
        assert!((keys.evaluate(2.5) - 2.5).abs() < 1e-6, "a straight segment");
    }

    #[test]
    fn default_weights_change_nothing_and_longer_handles_shape_the_curve() {
        let mut keys = curve(&[(0.0, 0.0), (10.0, 10.0)]);
        for key in &mut keys.keys {
            key.mode = TangentMode::FreeSmooth;
            key.left.slope = 3.0;
        }
        keys.update_tangents();
        let plain = keys.evaluate(2.0);
        keys.keys[0].right.weight = Some(1.0 / 3.0);
        assert!((keys.evaluate(2.0) - plain).abs() < 1e-6);
        keys.keys[0].right.weight = Some(0.9);
        assert!(keys.evaluate(2.0) > plain + 0.1);
    }

    #[test]
    fn a_curve_crosses_json_with_what_was_set_by_hand() {
        let mut keys = curve(&[(0.0, 1.0), (5.0, 2.0), (9.0, 0.5)]);
        keys.keys[1] = CurveKey {
            mode: TangentMode::Broken,
            ..keys.keys[1]
        };
        keys.keys[1].left.mode = SideMode::Constant;
        keys.keys[1].right.slope = 4.0;
        keys.keys[1].right.weight = Some(0.6);
        keys.update_tangents();
        let read = Curve::from_json(&keys.to_json()).unwrap();
        assert_eq!(read, keys);
        assert!(
            keys.to_json()["keys"][0].get("mode").is_none(),
            "Clamped Auto is the default"
        );
        assert!(Curve::from_json(&serde_json::json!({ "keys": [{ "time": 0 }] })).is_err());
    }
}
