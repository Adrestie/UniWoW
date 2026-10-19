//! The doodads changed in memory (step 15.5): their places as the user set them, laid over what the
//! tiles list, so that a tile left and read again shows them still; read and written by the
//! Inspector through the commands `doodads.read` and `doodads.write`. Nothing is written to the
//! files.

use std::collections::{HashMap, HashSet};
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

/// What the tiles placed list as read, and the doodads the user changed, shared by the jobs, the
/// commands and the reads.
#[derive(Default)]
pub struct Listed {
    pub tiles: HashMap<TileId, Arc<Placements>>,
    pub moved: HashMap<EntityId, Spot>,
}

impl Listed {
    /// The doodad `id` as a tile lists it: the tile it is listed again by, else the first listing
    /// it; and its place, as changed.
    pub fn find(&self, id: &EntityId) -> Option<(TileId, Doodad, Spot)> {
        if id.kind != Kind::Doodad {
            return None;
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

    /// The tiles listing the unique ids `unique`, in their order.
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
            .collect();
        tiles.sort();
        tiles
    }
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
    Some(json!({
        "id": id.to_string(),
        "changed": listed.moved.contains_key(id),
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
