//! Animatable properties: the values a module lets others, such as the timeline, read and write
//! without the history, as Unity animates any field of a component.

use std::sync::Arc;

use serde_json::{Value, json};

pub use crate::numbers::PropertyKind;

impl PropertyKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::Vector => "vector",
            Self::Colour => "colour",
            Self::Boolean => "boolean",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Number, Self::Vector, Self::Colour, Self::Boolean]
            .into_iter()
            .find(|kind| kind.name() == name)
    }

    /// How many numbers a value of this type has.
    pub fn components(self) -> usize {
        match self {
            Self::Number | Self::Boolean => 1,
            Self::Vector | Self::Colour => 3,
        }
    }
}

/// A value of an animatable property.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PropertyValue {
    Number(f64),
    Vector([f64; 3]),
    Colour([f64; 3]),
    Boolean(bool),
}

impl PropertyValue {
    pub fn kind(&self) -> PropertyKind {
        match self {
            Self::Number(_) => PropertyKind::Number,
            Self::Vector(_) => PropertyKind::Vector,
            Self::Colour(_) => PropertyKind::Colour,
            Self::Boolean(_) => PropertyKind::Boolean,
        }
    }

    /// Its numbers; a boolean is 0 or 1.
    pub fn components(&self) -> Vec<f64> {
        match *self {
            Self::Number(value) => vec![value],
            Self::Vector(values) | Self::Colour(values) => values.to_vec(),
            Self::Boolean(value) => vec![if value { 1.0 } else { 0.0 }],
        }
    }

    /// A value of `kind` from its numbers, the missing ones being 0; a boolean is true from 0.5.
    pub fn from_components(kind: PropertyKind, numbers: &[f64]) -> Self {
        let at = |index: usize| numbers.get(index).copied().unwrap_or(0.0);
        match kind {
            PropertyKind::Number => Self::Number(at(0)),
            PropertyKind::Vector => Self::Vector([at(0), at(1), at(2)]),
            PropertyKind::Colour => Self::Colour([at(0), at(1), at(2)]),
            PropertyKind::Boolean => Self::Boolean(at(0) >= 0.5),
        }
    }

    /// The same value with each number kept within `range`; a number that is not one (NaN) becomes
    /// the value of the range nearest 0.
    pub fn clamped(&self, range: [f64; 2]) -> Self {
        match *self {
            Self::Boolean(_) => *self,
            _ => {
                let numbers: Vec<f64> = self
                    .components()
                    .into_iter()
                    // max and min, unlike clamp, never panic, whatever the range.
                    .map(|n| if n.is_nan() { 0.0 } else { n }.max(range[0]).min(range[1]))
                    .collect();
                Self::from_components(self.kind(), &numbers)
            }
        }
    }

    /// As JSON: a number, an array of three numbers, or a boolean.
    pub fn to_json(&self) -> Value {
        match *self {
            Self::Number(value) => json!(value),
            Self::Vector(values) | Self::Colour(values) => json!(values),
            Self::Boolean(value) => json!(value),
        }
    }

    pub fn from_json(kind: PropertyKind, value: &Value) -> Result<Self, String> {
        let wrong = || format!("{value} is not a {}", kind.name());
        match kind {
            PropertyKind::Number => value.as_f64().map(Self::Number).ok_or_else(wrong),
            PropertyKind::Boolean => value.as_bool().map(Self::Boolean).ok_or_else(wrong),
            PropertyKind::Vector | PropertyKind::Colour => {
                let numbers: Vec<f64> = value
                    .as_array()
                    .filter(|items| items.len() == 3)
                    .ok_or_else(wrong)?
                    .iter()
                    .map(|item| item.as_f64().ok_or_else(wrong))
                    .collect::<Result<_, _>>()?;
                Ok(Self::from_components(kind, &numbers))
            }
        }
    }
}

/// Reads the current value of a property; called from any thread.
pub type ReadProperty = Arc<dyn Fn() -> PropertyValue + Send + Sync>;
/// Writes a value of the property's type, within its range; called from any thread.
pub type WriteProperty = Arc<dyn Fn(PropertyValue) + Send + Sync>;

/// A property a module declares in `Module::register`.
#[derive(Clone)]
pub struct PropertySpec {
    /// Unique within the module; the property's path is `<module>/<name>`.
    pub name: String,
    pub label: String,
    pub kind: PropertyKind,
    /// Lowest and highest value of each number.
    pub range: [f64; 2],
    pub read: ReadProperty,
    pub write: WriteProperty,
}

/// Why `name` cannot be a property's: its path is `<module>/<name>`, which the views split at its
/// last `/`, and shows as the name.
pub fn name_error(name: &str) -> Option<String> {
    if name.is_empty() {
        Some("its name is empty".to_owned())
    } else if name.contains('/') || name.chars().any(char::is_whitespace) {
        Some(format!("its name '{name}' holds a '/' or a space"))
    } else {
        None
    }
}

/// Why a range cannot be a property's: none for two numbers, the lowest first.
pub fn range_error(range: [f64; 2]) -> Option<String> {
    if range[0].is_nan() || range[1].is_nan() {
        Some("its range holds a value that is not a number".to_owned())
    } else if range[0] > range[1] {
        Some(format!("its range goes from {} down to {}", range[0], range[1]))
    } else {
        None
    }
}

/// A property of the catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyInfo {
    /// `<module>/<name>`.
    pub path: String,
    /// The module declaring it.
    pub owner: String,
    pub label: String,
    pub kind: PropertyKind,
    pub range: [f64; 2],
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PropertyKind, PropertyValue};

    #[test]
    fn a_range_upside_down_or_not_a_number_is_refused_and_never_panics() {
        assert!(super::range_error([0.0, 1.0]).is_none());
        assert!(super::range_error([f64::NEG_INFINITY, f64::INFINITY]).is_none());
        assert!(super::range_error([1.0, 0.0]).is_some());
        assert!(super::range_error([f64::NAN, 1.0]).is_some());
        let value = PropertyValue::Number(5.0);
        assert_eq!(value.clamped([1.0, 0.0]), PropertyValue::Number(0.0));
        assert_eq!(value.clamped([f64::NAN, 2.0]), PropertyValue::Number(2.0));
    }

    #[test]
    fn values_cross_as_json_and_keep_within_their_range() {
        let position = PropertyValue::Vector([1.0, -2.5, 3.0]);
        assert_eq!(position.to_json(), json!([1.0, -2.5, 3.0]));
        assert_eq!(
            PropertyValue::from_json(PropertyKind::Vector, &position.to_json()),
            Ok(position)
        );
        assert!(PropertyValue::from_json(PropertyKind::Colour, &json!([1, 2])).is_err());
        assert!(PropertyValue::from_json(PropertyKind::Boolean, &json!(1)).is_err());
        assert_eq!(
            PropertyValue::Colour([1.5, 0.5, -1.0]).clamped([0.0, 1.0]),
            PropertyValue::Colour([1.0, 0.5, 0.0])
        );
        assert_eq!(
            PropertyValue::Vector([f64::NAN, f64::INFINITY, 2.0]).clamped([1.0, 10.0]),
            PropertyValue::Vector([1.0, 10.0, 2.0])
        );
        assert_eq!(
            PropertyValue::from_components(PropertyKind::Boolean, &[0.7]),
            PropertyValue::Boolean(true)
        );
    }
}
