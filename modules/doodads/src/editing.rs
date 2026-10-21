//! The doodads changed in memory (step 15.5): their places as the user set them, laid over what the
//! tiles list, so that a tile left and read again shows them still; read and written by the
//! Inspector through the commands `doodads.read` and `doodads.write`. Nothing is written to the
//! files.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use uniwow_api::formats::{Doodad, FileRef, Placements, TileId, placement};
use uniwow_api::glam::Mat4;
use uniwow_api::scene::{self, EntityId, Kind};
use uniwow_api::serde_json::{Value, json};

/// The place of a doodad as its file keeps it: its position and its rotation in the axes of the
/// file, its scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: f32,
}

/// The least and the greatest scale a doodad's file keeps, in 1024ths on 16 bits.
const SCALES: [f32; 2] = [1.0 / 1024.0, 65_535.0 / 1024.0];

impl Spot {
    pub fn of(doodad: &Doodad) -> Self {
        Self {
            position: doodad.position,
            rotation: doodad.rotation,
            scale: doodad.scale,
        }
    }

    pub fn transform(&self) -> Mat4 {
        placement(self.position, self.rotation, self.scale)
    }
}

/// A doodad the user created, kept in memory: its model and its place.
#[derive(Clone, Debug, PartialEq)]
pub struct Added {
    pub file: FileRef,
    pub spot: Spot,
}

impl Added {
    /// Its tile, the one under its origin.
    pub fn tile(&self) -> Option<TileId> {
        let [x, y, _] = scene::world_of_file(self.spot.position);
        scene::chunk_at(x, y).map(|(tile, _)| tile)
    }

    pub fn doodad(&self, unique_id: u32) -> Doodad {
        Doodad {
            file: self.file.clone(),
            unique_id,
            position: self.spot.position,
            rotation: self.spot.rotation,
            scale: self.spot.scale,
            flags: 0,
        }
    }
}

/// The changes the user made to the doodads, kept in memory: those moved, created and deleted, and
/// the unique ids given, by map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub moved: HashMap<EntityId, Spot>,
    pub added: HashMap<EntityId, Added>,
    pub deleted: HashSet<EntityId>,
    /// The highest unique id given by map, so that none is given twice.
    pub given: HashMap<u32, u32>,
    /// The textures of their models the user replaced, by doodad: by the index of the texture.
    pub textures: HashMap<EntityId, BTreeMap<u32, FileRef>>,
}

impl Changes {
    /// The changes as the project keeps them, in order.
    pub fn section(&self) -> Value {
        let file = |file: &FileRef| match file {
            FileRef::Path(path) => json!(path),
            FileRef::Id(id) => json!(id),
        };
        let mut moved: Vec<(&EntityId, &Spot)> = self.moved.iter().collect();
        moved.sort_by_key(|(id, _)| **id);
        let mut added: Vec<(&EntityId, &Added)> = self.added.iter().collect();
        added.sort_by_key(|(id, _)| **id);
        let mut deleted: Vec<&EntityId> = self.deleted.iter().collect();
        deleted.sort();
        let given: std::collections::BTreeMap<String, u32> =
            self.given.iter().map(|(map, id)| (map.to_string(), *id)).collect();
        let mut textures: Vec<(&EntityId, &BTreeMap<u32, FileRef>)> = self.textures.iter().collect();
        textures.sort_by_key(|(id, _)| **id);
        let textures: Vec<Value> = textures
            .into_iter()
            .flat_map(|(id, replaced)| {
                replaced
                    .iter()
                    .map(move |(index, to)| json!({ "id": id.to_string(), "texture": index, "file": file(to) }))
            })
            .collect();
        json!({
            "moved": moved.into_iter().map(|(id, spot)| json!({
                "id": id.to_string(), "position": spot.position, "rotation": spot.rotation, "scale": spot.scale,
            })).collect::<Vec<_>>(),
            "created": added.into_iter().map(|(id, added)| json!({
                "id": id.to_string(), "file": file(&added.file), "position": added.spot.position,
                "rotation": added.spot.rotation, "scale": added.spot.scale,
            })).collect::<Vec<_>>(),
            "deleted": deleted.into_iter().map(ToString::to_string).collect::<Vec<_>>(),
            "unique ids given": given,
            "textures": textures,
        })
    }

    /// The changes the project kept in `section`; none for none.
    pub fn of_section(section: &Value) -> Self {
        let vector = |value: &Value| -> Option<[f32; 3]> {
            let values = value.as_array()?;
            Some([0, 1, 2].map(|at| values.get(at).and_then(Value::as_f64).unwrap_or(0.0) as f32))
        };
        let spot = |entry: &Value| -> Option<Spot> {
            Some(Spot {
                position: vector(&entry["position"])?,
                rotation: vector(&entry["rotation"])?,
                scale: entry["scale"].as_f64()? as f32,
            })
        };
        let list = |key: &str| section[key].as_array().cloned().unwrap_or_default();
        let id = |entry: &Value| entry["id"].as_str()?.parse::<EntityId>().ok();
        Self {
            moved: list("moved")
                .iter()
                .filter_map(|entry| Some((id(entry)?, spot(entry)?)))
                .collect(),
            added: list("created")
                .iter()
                .filter_map(|entry| {
                    let file = match &entry["file"] {
                        Value::String(path) => FileRef::Path(path.clone()),
                        Value::Number(number) => FileRef::Id(number.as_u64()? as u32),
                        _ => return None,
                    };
                    Some((
                        id(entry)?,
                        Added {
                            file,
                            spot: spot(entry)?,
                        },
                    ))
                })
                .collect(),
            deleted: list("deleted")
                .iter()
                .filter_map(|id| id.as_str()?.parse().ok())
                .collect(),
            given: section["unique ids given"]
                .as_object()
                .map(|given| {
                    given
                        .iter()
                        .filter_map(|(map, id)| Some((map.parse().ok()?, id.as_u64()? as u32)))
                        .collect()
                })
                .unwrap_or_default(),
            textures: {
                let mut textures: HashMap<EntityId, BTreeMap<u32, FileRef>> = HashMap::new();
                for entry in list("textures") {
                    let file = match &entry["file"] {
                        Value::String(path) => FileRef::Path(path.clone()),
                        Value::Number(number) => number.as_u64().map_or(FileRef::Id(0), |id| FileRef::Id(id as u32)),
                        _ => continue,
                    };
                    if let (Some(id), Some(index)) = (id(&entry), entry["texture"].as_u64()) {
                        textures.entry(id).or_default().insert(index as u32, file);
                    }
                }
                textures
            },
        }
    }

    /// The textures replaced of the doodad `id`, as a look names them.
    pub fn replaced(&self, id: &EntityId) -> Vec<(u32, FileRef)> {
        self.textures
            .get(id)
            .map(|replaced| {
                replaced
                    .iter()
                    .map(|(index, file)| (uniwow_api::models::REPLACED | index, file.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every entity these changes name.
    pub fn ids(&self) -> Vec<EntityId> {
        let mut ids: Vec<EntityId> = self
            .moved
            .keys()
            .chain(self.added.keys())
            .chain(self.deleted.iter())
            .chain(self.textures.keys())
            .copied()
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

/// What the tiles placed list as read, the doodads the user changed, and the highest unique id the
/// tiles of each map hold, shared by the jobs, the commands and the reads.
#[derive(Default)]
pub struct Listed {
    pub tiles: HashMap<TileId, Arc<Placements>>,
    pub moved: HashMap<EntityId, Spot>,
    pub added: HashMap<EntityId, Added>,
    pub deleted: HashSet<EntityId>,
    pub given: HashMap<u32, u32>,
    pub highest: HashMap<u32, u32>,
    pub textures: HashMap<EntityId, BTreeMap<u32, FileRef>>,
}

impl Listed {
    /// The changes, to lay over a tile read.
    pub fn changes(&self) -> Changes {
        Changes {
            moved: self.moved.clone(),
            added: self.added.clone(),
            deleted: self.deleted.clone(),
            given: self.given.clone(),
            textures: self.textures.clone(),
        }
    }

    /// A unique id of the map `map` none of its tiles holds and none given before; none while the
    /// highest of its tiles is not read.
    pub fn give(&mut self, map: u32) -> Option<u32> {
        let highest = *self.highest.get(&map)?;
        let given = self.given.entry(map).or_insert(0);
        *given = (*given).max(highest) + 1;
        Some(*given)
    }

    /// The doodad `id` as a tile lists it: the tile it is listed again by, else the first listing
    /// it; and its place, as changed. A doodad created is listed by the tile under it; one deleted
    /// is none.
    pub fn find(&self, id: &EntityId) -> Option<(TileId, Doodad, Spot)> {
        if id.kind != Kind::Doodad || self.deleted.contains(id) {
            return None;
        }
        if let Some(added) = self.added.get(id) {
            let spot = self.moved.get(id).copied().unwrap_or(added.spot);
            return Some((added.tile()?, added.doodad(id.key as u32), spot));
        }
        let unique = id.key as u32;
        let found = |tile: TileId| {
            let placements = self.tiles.get(&tile)?;
            let doodad = placements.doodads.iter().find(|doodad| doodad.unique_id == unique)?;
            Some((tile, doodad.clone()))
        };
        let (tile, doodad) = match id.again() {
            Some(tile) => found(tile)?,
            None => {
                let mut tiles: Vec<TileId> = self.tiles.keys().copied().collect();
                tiles.sort();
                tiles.into_iter().find_map(found)?
            }
        };
        let spot = self.moved.get(id).copied().unwrap_or_else(|| Spot::of(&doodad));
        Some((tile, doodad, spot))
    }

    /// The tiles held listing the unique ids `unique`, or under a doodad created among them, in
    /// their order.
    pub fn tiles_listing(&self, unique: &HashSet<u32>) -> Vec<TileId> {
        let mut tiles: Vec<TileId> = self
            .tiles
            .iter()
            .filter(|(_, placements)| {
                placements
                    .doodads
                    .iter()
                    .any(|doodad| unique.contains(&doodad.unique_id))
            })
            .map(|(tile, _)| *tile)
            .chain(
                self.added
                    .iter()
                    .filter(|(id, _)| unique.contains(&(id.key as u32)))
                    .filter_map(|(_, added)| added.tile())
                    .filter(|tile| self.tiles.contains_key(tile)),
            )
            .collect();
        tiles.sort();
        tiles.dedup();
        tiles
    }
}

/// `placements` of the tile `tile` of the map `map` as the user changed them: the doodads deleted
/// left out, those created under it added, those moved at their places; those of `again` by their
/// ids listed again.
pub fn edited(placements: &Placements, map: u32, tile: TileId, again: &HashSet<u64>, changes: &Changes) -> Placements {
    let id_of = |unique: u32| match again.contains(&u64::from(unique)) {
        true => EntityId::doodad_again(map, unique, tile),
        false => EntityId::doodad(map, unique),
    };
    let mut placements = placements.clone();
    if !changes.deleted.is_empty() {
        placements
            .doodads
            .retain(|doodad| !changes.deleted.contains(&id_of(doodad.unique_id)));
    }
    let mut created: Vec<(&EntityId, &Added)> = changes
        .added
        .iter()
        .filter(|(id, added)| id.map == map && added.tile() == Some(tile) && !changes.deleted.contains(id))
        .collect();
    created.sort_by_key(|(id, _)| **id);
    for (id, added) in created {
        placements.doodads.push(added.doodad(id.key as u32));
    }
    moved(&placements, map, tile, again, &changes.moved)
}

/// `placements` of the tile `tile` of the map `map`, the doodads the user changed at their places
/// as changed; those of `again` by their ids listed again.
pub fn moved(
    placements: &Placements,
    map: u32,
    tile: TileId,
    again: &HashSet<u64>,
    moved: &HashMap<EntityId, Spot>,
) -> Placements {
    let mut placements = placements.clone();
    if moved.is_empty() {
        return placements;
    }
    for doodad in &mut placements.doodads {
        let id = match again.contains(&u64::from(doodad.unique_id)) {
            true => EntityId::doodad_again(map, doodad.unique_id, tile),
            false => EntityId::doodad(map, doodad.unique_id),
        };
        if let Some(spot) = moved.get(&id) {
            doodad.position = spot.position;
            doodad.rotation = spot.rotation;
            doodad.scale = spot.scale;
        }
    }
    placements
}

fn file_text(file: &FileRef) -> String {
    match file {
        FileRef::Path(path) => path.clone(),
        FileRef::Id(id) => format!("FileDataID {id}"),
    }
}

/// The components of the doodad `id` for the Inspector: its *Transform* (its position in the world,
/// its rotation in degrees about the axes of its file, its scale), its *Model*, and its raw data as
/// its tile stores it.
pub fn read(listed: &Listed, id: &EntityId) -> Option<Value> {
    let (tile, doodad, spot) = listed.find(id)?;
    let world = scene::world_of_file(spot.position);
    // The textures replaced, for the materials of its model to be read with them.
    let replaced: Vec<Value> = listed
        .textures
        .get(id)
        .map(|textures| {
            textures
                .iter()
                .map(|(index, file)| json!([index, file_text(file)]))
                .collect()
        })
        .unwrap_or_default();
    Some(json!({
        "id": id.to_string(),
        "changed": listed.moved.contains_key(id) || listed.textures.contains_key(id),
        "replaced": replaced,
        "components": {
            "transform": {
                "position": world,
                "rotation": spot.rotation,
                "scale": spot.scale,
            },
            "model": { "file": file_text(&doodad.file) },
            "raw": {
                "tile": format!("{}_{}", tile.x, tile.y),
                "unique id": doodad.unique_id,
                "position": spot.position,
                "rotation": spot.rotation,
                "scale (1024ths)": (spot.scale * 1024.0).round() as u32,
                "flags": format!("0x{:04X}", doodad.flags),
            },
        },
    }))
}

/// `spot` with its `field` (position, rotation or scale) changed as `change` says (`scene::
/// changed_value`), on the axis `axis` of a vector, every axis without one; the position in the
/// axes of the world.
pub fn written(spot: Spot, field: &str, axis: Option<usize>, change: &str) -> Result<Spot, String> {
    let axes: Vec<usize> = match axis {
        Some(axis) if axis < 3 => vec![axis],
        Some(axis) => return Err(format!("no axis {axis}")),
        None => vec![0, 1, 2],
    };
    let mut spot = spot;
    match field {
        "position" => {
            let mut world = scene::world_of_file(spot.position);
            for axis in axes {
                world[axis] = scene::changed_value(world[axis], change)?;
            }
            spot.position = scene::file_of_world(world);
        }
        "rotation" => {
            if let Some((about, degrees)) = scene::turn_of(change) {
                spot.rotation = uniwow_api::formats::turned(spot.rotation, about.into(), degrees);
                return Ok(spot);
            }
            for axis in axes {
                spot.rotation[axis] = scene::changed_value(spot.rotation[axis], change)?;
            }
        }
        "scale" => {
            let scale = scene::changed_value(spot.scale, change)?;
            if !(SCALES[0]..=SCALES[1]).contains(&scale) {
                return Err(format!(
                    "a doodad's scale is from {:.4} to {:.2}, not {scale}",
                    SCALES[0], SCALES[1]
                ));
            }
            spot.scale = scale;
        }
        other => return Err(format!("a doodad has no field '{other}'")),
    }
    Ok(spot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doodad(unique_id: u32, position: [f32; 3]) -> Doodad {
        Doodad {
            file: FileRef::Path("World\\Tree.m2".to_owned()),
            unique_id,
            position,
            rotation: [0.0, 90.0, 0.0],
            scale: 1.0,
            flags: 0,
        }
    }

    #[test]
    fn a_doodad_created_and_one_deleted_are_laid_over_their_tile_and_ids_are_never_given_twice() {
        let tile = TileId { x: 32, y: 48 };
        let placements = Placements {
            doodads: vec![doodad(7, [100.0, 20.0, 200.0]), doodad(8, [1.0, 2.0, 3.0])],
            buildings: Vec::new(),
        };
        let mut listed = Listed::default();
        listed.tiles.insert(tile, Arc::new(placements.clone()));
        assert_eq!(listed.give(0), None, "the highest of the map not read yet");
        listed.highest.insert(0, 500);
        let given = listed.give(0).unwrap();
        assert_eq!((given, listed.give(0).unwrap()), (501, 502));
        // Created within the tile: its file's X in its column, its Z in its row.
        let id = EntityId::doodad(0, given);
        let mut spot = Spot::of(&placements.doodads[0]);
        spot.position = [17_166.0, 20.0, 25_700.0];
        listed.added.insert(
            id,
            Added {
                file: FileRef::Path("World\\Oak.m2".to_owned()),
                spot,
            },
        );
        listed.deleted.insert(EntityId::doodad(0, 8));
        let edited = edited(&placements, 0, tile, &HashSet::new(), &listed.changes());
        let ids: Vec<u32> = edited.doodads.iter().map(|doodad| doodad.unique_id).collect();
        assert_eq!(ids, [7, 501]);
        assert_eq!(listed.find(&id).unwrap().0, tile);
        assert!(listed.find(&EntityId::doodad(0, 8)).is_none(), "deleted");
        assert_eq!(listed.tiles_listing(&HashSet::from([501])), [tile]);
    }

    #[test]
    fn the_changes_are_kept_in_the_project_and_read_back() {
        let mut changes = Changes::default();
        let spot = Spot {
            position: [1.0, 2.0, 3.0],
            rotation: [0.0, 45.0, 0.0],
            scale: 1.5,
        };
        changes.moved.insert(EntityId::doodad(0, 7), spot);
        changes.added.insert(
            EntityId::doodad(0, 501),
            Added {
                file: FileRef::Path("World\\Oak.m2".to_owned()),
                spot,
            },
        );
        changes.added.insert(
            EntityId::doodad(0, 502),
            Added {
                file: FileRef::Id(12345),
                spot,
            },
        );
        changes.deleted.insert(EntityId::doodad(0, 8));
        changes.given.insert(0, 502);
        changes.textures.insert(
            EntityId::doodad(0, 7),
            BTreeMap::from([(1, FileRef::Path("world\red.blp".to_owned()))]),
        );
        assert_eq!(
            changes.replaced(&EntityId::doodad(0, 7)),
            [(
                uniwow_api::models::REPLACED | 1,
                FileRef::Path("world\red.blp".to_owned())
            )]
        );
        assert!(changes.replaced(&EntityId::doodad(0, 8)).is_empty());
        let section = changes.section();
        assert_eq!(Changes::of_section(&section), changes);
        assert_eq!(Changes::of_section(&Value::Null), Changes::default());
        assert_eq!(changes.ids().len(), 4);
    }

    #[test]
    fn a_doodad_changed_is_read_back_and_laid_over_its_tile_read_again() {
        let tile = TileId { x: 32, y: 48 };
        let placements = Placements {
            doodads: vec![doodad(7, [100.0, 20.0, 200.0]), doodad(8, [1.0, 2.0, 3.0])],
            buildings: Vec::new(),
        };
        let mut listed = Listed::default();
        listed.tiles.insert(tile, Arc::new(placements.clone()));
        let id = EntityId::doodad(0, 7);
        let (_, _, spot) = listed.find(&id).unwrap();
        // Raised by 2 yards in the world: the axis Y of the file.
        let up = written(spot, "position", Some(2), "+2").unwrap();
        assert_eq!(up.position, [100.0, 22.0, 200.0]);
        // Moved along X of the world: the axis Z of the file, the other way.
        let along = written(up, "position", Some(0), "+10").unwrap();
        assert!((along.position[2] - 190.0).abs() < 1e-2);
        assert_eq!(
            written(spot, "rotation", Some(1), "*2").unwrap().rotation,
            [0.0, 180.0, 0.0]
        );
        // Turned by a gizmo about the axis X of the world: that turn after its own.
        let tilted = written(spot, "rotation", None, &scene::turn_change([1.0, 0.0, 0.0], 30.0)).unwrap();
        let wanted =
            uniwow_api::glam::Quat::from_rotation_x(30f32.to_radians()) * uniwow_api::formats::turning(spot.rotation);
        assert!(uniwow_api::formats::turning(tilted.rotation).dot(wanted).abs() > 1.0 - 1e-5);
        assert_eq!(written(spot, "scale", None, "*2").unwrap().scale, 2.0);
        assert!(written(spot, "scale", None, "100").is_err(), "beyond 63.99");
        assert!(written(spot, "colour", None, "1").is_err());
        listed.moved.insert(id, along);
        let value = read(&listed, &id).unwrap();
        assert_eq!(value["changed"], json!(true));
        assert_eq!(value["components"]["raw"]["unique id"], json!(7));
        let again = moved(&placements, 0, tile, &HashSet::new(), &listed.moved);
        assert_eq!(
            again.doodads[0].position, along.position,
            "laid over the tile read again"
        );
        assert_eq!(again.doodads[1], placements.doodads[1]);
        assert_eq!(listed.tiles_listing(&HashSet::from([7])), [tile]);
        assert!(read(&listed, &EntityId::doodad(0, 99)).is_none());
    }
}
