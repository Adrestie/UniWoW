//! What the modules of every language declare and publish through commands (step 15.12): their
//! components, and their entities as pieces of their own.

use uniwow_api::scene::components::Component;
use uniwow_api::scene::{self, Entity, EntityId, Piece, PieceBuilder, PieceKey};
use uniwow_api::serde_json::Value;

/// The declarations `before` with those of `module` replaced by the components `arguments` lists.
pub fn declared(before: &[Component], module: &str, arguments: &Value) -> Result<Vec<Component>, String> {
    let components = arguments["components"]
        .as_array()
        .ok_or("'components' is a list of components")?
        .iter()
        .map(|component| Component::of_json(module, component))
        .collect::<Result<Vec<_>, _>>()?;
    let mut names: Vec<&str> = components.iter().map(|component| component.name.as_str()).collect();
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("two components of one name".to_owned());
    }
    let mut after: Vec<Component> = before
        .iter()
        .filter(|component| component.module != module)
        .cloned()
        .collect();
    if let Some(taken) = after
        .iter()
        .find(|other| components.iter().any(|component| component.name == other.name))
    {
        return Err(format!(
            "the component '{}' is declared by {}",
            taken.name, taken.module
        ));
    }
    after.extend(components);
    Ok(after)
}

/// The declarations `before` without those of `module`; none when it had none.
pub fn dropped(before: &[Component], module: &str) -> Option<Vec<Component>> {
    before.iter().any(|component| component.module == module).then(|| {
        before
            .iter()
            .filter(|component| component.module != module)
            .cloned()
            .collect()
    })
}

/// The key of a piece of `module` that `arguments` names: its map and its part, 0 by default.
pub fn key(module: &str, arguments: &Value) -> Result<PieceKey, String> {
    let map = arguments["map"].as_u64().ok_or("'map' is the id of a map")? as u32;
    let part = arguments["part"].as_u64().unwrap_or(0) as u32;
    Ok(PieceKey::part(module, map, part))
}

/// The piece of the entities `arguments` lists: `[{"id", "label", "origin", "extra", "size"}]`, each
/// a box of its size (a yard by default) about its origin.
pub fn piece(arguments: &Value) -> Result<Piece, String> {
    let entities = arguments["entities"].as_array().ok_or("'entities' is a list")?;
    let mut parsed = Vec::with_capacity(entities.len());
    for entity in entities {
        let id: EntityId = entity["id"]
            .as_str()
            .ok_or("an entity has an 'id' as text")?
            .parse()
            .map_err(|reason| format!("{}: {reason:?}", entity["id"]))?;
        let origin = entity["origin"]
            .as_array()
            .filter(|origin| origin.len() == 3)
            .and_then(|origin| {
                let number = |at: usize| {
                    origin[at]
                        .as_f64()
                        .filter(|value| value.is_finite())
                        .map(|value| value as f32)
                };
                Some([number(0)?, number(1)?, number(2)?])
            })
            .ok_or_else(|| format!("{id}: 'origin' is three numbers"))?;
        let size = entity["size"].as_f64().filter(|size| *size > 0.0).unwrap_or(1.0) as f32;
        parsed.push((
            id,
            entity["label"].as_str().unwrap_or_default().to_owned(),
            entity["extra"].as_u64().unwrap_or(0) as u32,
            origin,
            size,
        ));
    }
    let mut builder = PieceBuilder::with_capacity(Vec::new(), parsed.len());
    for (id, label, extra, origin, size) in &parsed {
        builder.push(Entity {
            id: *id,
            flags: 0,
            parent: None,
            label,
            extra: *extra,
            origin: *origin,
            bounds: scene::box_around(*origin, *size),
        });
    }
    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::serde_json::json;

    fn tag() -> Value {
        json!({ "components": [{ "name": "tag", "kinds": ["doodad"], "fields": [{ "name": "tag" }], "read": "cs.tag_read" }] })
    }

    #[test]
    fn a_module_declares_its_components_again_in_place_of_its_own_and_they_go_with_it() {
        let declared_once = declared(&[], "sample-csharp", &tag()).unwrap();
        let notes = json!({ "components": [{ "name": "note", "fields": [], "read": "notes.read" }] });
        let both = declared(&declared_once, "notes", &notes).unwrap();
        assert_eq!(both.len(), 2);
        let again = declared(&both, "sample-csharp", &tag()).unwrap();
        assert_eq!(again.len(), 2, "declared again, not twice");
        assert!(declared(&again, "other", &tag()).unwrap_err().contains("sample-csharp"));
        let doubled = json!({ "components": [tag()["components"][0], tag()["components"][0]] });
        assert!(declared(&[], "m", &doubled).is_err());
        let left = dropped(&again, "sample-csharp").unwrap();
        assert_eq!(
            left.iter().map(|component| component.name.as_str()).collect::<Vec<_>>(),
            ["note"]
        );
        assert_eq!(dropped(&left, "sample-csharp"), None, "nothing of it left");
    }

    #[test]
    fn the_entities_a_module_publishes_are_a_piece_of_its_own() {
        let key = key("sample-csharp", &json!({ "map": 0, "part": 2 })).unwrap();
        assert_eq!(key, PieceKey::part("sample-csharp", 0, 2));
        let made = piece(&json!({ "entities": [
            { "id": "doodad:0:900001", "label": "Marker", "origin": [1.0, 2.0, 3.0], "size": 2.0 },
        ] }))
        .unwrap();
        assert_eq!(*made.ids, [EntityId::doodad(0, 900_001)]);
        assert_eq!(made.name(0), "Marker #900001");
        for wrong in [
            json!({ "entities": [{ "id": "tree:1", "origin": [0, 0, 0] }] }),
            json!({ "entities": [{ "id": "doodad:0:1", "origin": [0, 0] }] }),
            json!({}),
        ] {
            assert!(piece(&wrong).is_err(), "{wrong}");
        }
    }
}
