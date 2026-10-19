//! The components a module declares for the entities of other modules, in any language (step
//! 15.12): their fields and the commands that read and write them. The Inspector draws them from
//! their fields; a module's declarations go with it when it stops.

use serde_json::Value;

use super::Kind;

/// Topic published by the scene when the declarations change. Payload: `{ "module": <id> }`.
pub const DECLARED_TOPIC: &str = "scene.declared";

/// The kinds of the fields of a component declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Number,
    /// Three numbers, written one axis at a time.
    Vector,
    Text,
    Boolean,
    /// Red, green and blue from 0 to 1, written as `#rrggbb`.
    Colour,
}

impl FieldKind {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "number" => Self::Number,
            "vector" => Self::Vector,
            "text" => Self::Text,
            "boolean" => Self::Boolean,
            "colour" | "color" => Self::Colour,
            _ => return None,
        })
    }
}

/// A field of a component: its name in the values read and written, its label, its kind.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub kind: FieldKind,
    pub read_only: bool,
}

/// A component a module declares: its name and label, the kinds of entity it fits (every kind
/// when none), its fields, the command reading its values (`{"ids": [...]}` answered by
/// `{"entities": [{"id", "fields": {...}}]}`) and the one writing them (`{"ids", "field", "axis",
/// "change"}` answered by `{"changed", "refused"}`, a change as the Inspector types it: a value, or
/// `+n`, `-=n`, `*n`, `/n`; one undo entry of the module, within the Inspector's gesture).
#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub module: String,
    pub name: String,
    pub label: String,
    pub kinds: Vec<Kind>,
    pub fields: Vec<Field>,
    pub read: String,
    pub write: Option<String>,
}

impl Component {
    /// The component `value` declares, of the module `module`: `{"name", "label", "kinds": [...],
    /// "fields": [{"name", "label", "kind", "read_only"}], "read", "write"}`.
    pub fn of_json(module: &str, value: &Value) -> Result<Self, String> {
        let text = |key: &str| value[key].as_str().map(str::to_owned);
        let name = text("name")
            .filter(|name| !name.is_empty())
            .ok_or("a component has a 'name'")?;
        let kinds = value["kinds"]
            .as_array()
            .map(|kinds| {
                kinds
                    .iter()
                    .map(|kind| {
                        kind.as_str()
                            .and_then(Kind::parse)
                            .ok_or_else(|| format!("{name}: no kind {kind}"))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let fields = value["fields"]
            .as_array()
            .ok_or_else(|| format!("{name}: 'fields' is a list"))?
            .iter()
            .map(|field| {
                let field_name = field["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| format!("{name}: a field has a 'name'"))?;
                let kind = field["kind"].as_str().unwrap_or("text");
                Ok(Field {
                    name: field_name.to_owned(),
                    label: field["label"].as_str().unwrap_or(field_name).to_owned(),
                    kind: FieldKind::parse(kind).ok_or_else(|| format!("{name}: no kind of field '{kind}'"))?,
                    read_only: field["read_only"].as_bool().unwrap_or(false),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            module: module.to_owned(),
            label: text("label").unwrap_or_else(|| name.clone()),
            name,
            kinds,
            fields,
            read: text("read").ok_or("a component names its 'read' command")?,
            write: text("write"),
        })
    }

    /// Whether it fits every kind of `kinds`.
    pub fn fits(&self, kinds: impl IntoIterator<Item = Kind>) -> bool {
        let mut kinds = kinds.into_iter().peekable();
        kinds.peek().is_some() && kinds.all(|kind| self.kinds.is_empty() || self.kinds.contains(&kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_component_is_declared_by_its_fields_and_commands_and_fits_its_kinds() {
        let tag = Component::of_json(
            "sample-csharp",
            &json!({
                "name": "tag", "label": "Tag (C#)", "kinds": ["doodad"],
                "fields": [{ "name": "tag", "kind": "text" }, { "name": "weight", "kind": "number" }],
                "read": "cs.tag_read", "write": "cs.tag_write",
            }),
        )
        .unwrap();
        assert_eq!(tag.fields[1].kind, FieldKind::Number);
        assert_eq!(tag.fields[0].label, "tag");
        assert!(tag.fits([Kind::Doodad, Kind::Doodad]));
        assert!(!tag.fits([Kind::Doodad, Kind::Building]));
        assert!(!tag.fits([]), "nothing selected, nothing fits");
        let everywhere = Component::of_json("notes", &json!({ "name": "note", "fields": [], "read": "notes.read" }));
        assert!(everywhere.unwrap().fits([Kind::Creature, Kind::Chunk]));
        for wrong in [
            json!({ "fields": [], "read": "x" }),
            json!({ "name": "a", "read": "x" }),
            json!({ "name": "a", "fields": [], "read": "x", "kinds": ["tree"] }),
            json!({ "name": "a", "fields": [{ "name": "b", "kind": "matrix" }], "read": "x" }),
            json!({ "name": "a", "fields": [] }),
        ] {
            assert!(Component::of_json("m", &wrong).is_err(), "{wrong}");
        }
    }
}
