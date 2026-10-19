//! The groups and the prefabs of the editor (step 15.9), kept by the module `scene`: a group the
//! entities of a selection, moved as one, listed under it; a prefab the objects of a group, placed
//! again as instances, an instance applied to its prefab (its other instances following) or
//! reverted to it. Each change of them one undo entry; kept in the project.

use std::sync::Arc;

use uniwow_api::scene::{Entity, EntityId, Group, Groups, Piece, PieceBuilder, Prefab, PrefabObject, Snapshot};
use uniwow_api::serde_json::{Value, json};

/// The groups changed, from and to: one undo entry.
pub struct SetGroups {
    pub label: String,
    pub before: Arc<Groups>,
    pub after: Arc<Groups>,
}

/// The middle of the origins of `ids` the snapshot holds; none for none.
pub fn middle(snapshot: &Snapshot, ids: &[EntityId]) -> Option<[f32; 3]> {
    let origins: Vec<[f32; 3]> = ids
        .iter()
        .filter_map(|id| snapshot.get(id))
        .map(|entity| entity.origin())
        .collect();
    if origins.is_empty() {
        return None;
    }
    let count = origins.len() as f32;
    Some([0, 1, 2].map(|axis| origins.iter().map(|origin| origin[axis]).sum::<f32>() / count))
}

/// The groups of the map `map` as entities of the scene: each at the middle of its children, its
/// bounds theirs; a group none of whose children is held left out.
pub fn piece(groups: &Groups, snapshot: &Snapshot, map: u32) -> Piece {
    let mut builder = PieceBuilder::new(Vec::new());
    for group in groups.groups.iter().filter(|group| group.map == map) {
        let children: Vec<_> = group.children.iter().filter_map(|id| snapshot.get(id)).collect();
        let Some(origin) = middle(snapshot, &group.children) else {
            continue;
        };
        let bounds = children
            .iter()
            .fold([[f32::MAX; 3], [f32::MIN; 3]], |[low, high], child| {
                let [a, b] = child.bounds();
                [
                    [low[0].min(a[0]), low[1].min(a[1]), low[2].min(a[2])],
                    [high[0].max(b[0]), high[1].max(b[1]), high[2].max(b[2])],
                ]
            });
        builder.push(Entity {
            id: EntityId::group(group.id),
            flags: 0,
            parent: None,
            label: &group.name,
            extra: group.prefab.unwrap_or(0),
            origin,
            bounds,
        });
    }
    builder.build()
}

/// The objects of a prefab made of the entities read `read` (their components, as `doodads.read`
/// gives them), placed from the middle of their positions.
pub fn objects(read: &[Value]) -> Vec<PrefabObject> {
    let position = |value: &Value| -> Option<[f32; 3]> {
        let values = value["components"]["transform"]["position"].as_array()?;
        Some([0, 1, 2].map(|at| values.get(at).and_then(Value::as_f64).unwrap_or(0.0) as f32))
    };
    let positions: Vec<[f32; 3]> = read.iter().filter_map(position).collect();
    if positions.is_empty() {
        return Vec::new();
    }
    let count = positions.len() as f32;
    let pivot: [f32; 3] = [0, 1, 2].map(|axis| positions.iter().map(|at| at[axis]).sum::<f32>() / count);
    read.iter()
        .filter_map(|value| {
            let at = position(value)?;
            let rotation = value["components"]["transform"]["rotation"].as_array()?;
            Some(PrefabObject {
                file: value["components"]["model"]["file"].as_str()?.to_owned(),
                offset: [0, 1, 2].map(|axis| at[axis] - pivot[axis]),
                rotation: [0, 1, 2].map(|at| rotation.get(at).and_then(Value::as_f64).unwrap_or(0.0) as f32),
                scale: value["components"]["transform"]["scale"].as_f64().unwrap_or(1.0) as f32,
            })
        })
        .collect()
}

/// The model of an object as `doodads.create` takes it: a path, or a FileDataID.
pub fn file(object: &PrefabObject) -> Value {
    match object.file.strip_prefix("FileDataID ") {
        Some(id) => id.parse::<u64>().map_or_else(|_| json!(object.file), |id| json!(id)),
        None => json!(object.file),
    }
}

/// The groups and the prefabs as the project keeps them, in order.
pub fn section(groups: &Groups) -> Value {
    let mut sorted = groups.groups.clone();
    sorted.sort_by_key(|group| group.id);
    let mut prefabs = groups.prefabs.clone();
    prefabs.sort_by_key(|prefab| prefab.id);
    json!({
        "groups": sorted.iter().map(|group| json!({
            "id": group.id,
            "name": group.name,
            "map": group.map,
            "children": group.children.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "prefab": group.prefab,
        })).collect::<Vec<_>>(),
        "prefabs": prefabs.iter().map(|prefab| json!({
            "id": prefab.id,
            "name": prefab.name,
            "objects": prefab.objects.iter().map(|object| json!({
                "file": object.file, "offset": object.offset, "rotation": object.rotation, "scale": object.scale,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// The groups and the prefabs kept in `section`.
pub fn of_section(section: &Value) -> Groups {
    let vector = |value: &Value| -> [f32; 3] {
        let values = value.as_array().cloned().unwrap_or_default();
        [0, 1, 2].map(|at| values.get(at).and_then(Value::as_f64).unwrap_or(0.0) as f32)
    };
    let list = |key: &str| section[key].as_array().cloned().unwrap_or_default();
    Groups {
        generation: 1,
        groups: list("groups")
            .iter()
            .filter_map(|group| {
                Some(Group {
                    id: group["id"].as_u64()? as u32,
                    name: group["name"].as_str().unwrap_or("Group").to_owned(),
                    map: group["map"].as_u64().unwrap_or(0) as u32,
                    children: group["children"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|id| id.as_str()?.parse().ok())
                        .collect(),
                    prefab: group["prefab"].as_u64().map(|id| id as u32),
                })
            })
            .collect(),
        prefabs: list("prefabs")
            .iter()
            .filter_map(|prefab| {
                Some(Prefab {
                    id: prefab["id"].as_u64()? as u32,
                    name: prefab["name"].as_str().unwrap_or("Prefab").to_owned(),
                    objects: prefab["objects"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|object| {
                            Some(PrefabObject {
                                file: object["file"].as_str()?.to_owned(),
                                offset: vector(&object["offset"]),
                                rotation: vector(&object["rotation"]),
                                scale: object["scale"].as_f64().unwrap_or(1.0) as f32,
                            })
                        })
                        .collect(),
                })
            })
            .collect(),
    }
}

/// The next id above those of `groups`, for a group or a prefab.
pub fn next_id(groups: &Groups) -> u32 {
    groups
        .groups
        .iter()
        .map(|group| group.id)
        .chain(groups.prefabs.iter().map(|prefab| prefab.id))
        .max()
        .unwrap_or(0)
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefab_is_made_of_its_objects_from_their_middle_and_kept_in_the_project() {
        let read = |x: f64, file: &str| {
            json!({ "components": {
                "transform": { "position": [x, 10.0, 0.0], "rotation": [0.0, 90.0, 0.0], "scale": 1.0 },
                "model": { "file": file },
            }})
        };
        let objects = objects(&[read(0.0, "Tent.m2"), read(10.0, "Fire.m2"), read(20.0, "FileDataID 5")]);
        assert_eq!(objects.len(), 3);
        assert_eq!(objects[0].offset, [-10.0, 0.0, 0.0]);
        assert_eq!(objects[2].offset, [10.0, 0.0, 0.0]);
        assert_eq!(file(&objects[2]), json!(5));
        assert_eq!(file(&objects[0]), json!("Tent.m2"));
        let groups = Groups {
            generation: 1,
            groups: vec![Group {
                id: 1,
                name: "Camp".to_owned(),
                map: 0,
                children: vec![EntityId::doodad(0, 7)],
                prefab: Some(2),
            }],
            prefabs: vec![Prefab {
                id: 2,
                name: "Camp".to_owned(),
                objects,
            }],
        };
        assert_eq!(of_section(&section(&groups)), groups);
        assert_eq!(next_id(&groups), 3);
        assert_eq!(
            of_section(&Value::Null),
            Groups {
                generation: 1,
                ..Groups::default()
            }
        );
    }
}
