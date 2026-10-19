//! The buildings changed in memory (step 15.5): their placements as the user set them, read and
//! written by the Inspector through the commands `buildings.read` and `buildings.write`. Nothing is
//! written to the files.

use uniwow_api::formats::{Building, FileRef};
use uniwow_api::scene::{self, EntityId};
use uniwow_api::serde_json::{Value, json};

fn file_text(file: &FileRef) -> String {
    match file {
        FileRef::Path(path) => path.clone(),
        FileRef::Id(id) => format!("FileDataID {id}"),
    }
}

/// The components of the building `id` placed as `building` for the Inspector: its *Transform*
/// (its position in the world, its rotation in degrees about the axes of its file; no scale), its
/// *Model* (its file, its doodad set and name set), and its raw data as its tile stores it.
pub fn read(id: &EntityId, building: &Building, changed: bool) -> Value {
    json!({
        "id": id.to_string(),
        "changed": changed,
        "components": {
            "transform": {
                "position": scene::world_of_file(building.position),
                "rotation": building.rotation,
            },
            "model": {
                "file": file_text(&building.file),
                "doodad set": building.doodad_set,
                "name set": building.name_set,
            },
            "raw": {
                "unique id": building.unique_id,
                "position": building.position,
                "rotation": building.rotation,
                "bounds": building.bounds,
                "scale": building.scale,
                "flags": format!("0x{:04X}", building.flags),
            },
        },
    })
}

/// `building` with its `field` (position or rotation) changed as `change` says (`scene::
/// changed_value`), on the axis `axis` or every axis; the position in the axes of the world, the
/// bounds following it. A building is not scaled: the client of 3.3.5a reads no scale.
pub fn written(building: &Building, field: &str, axis: Option<usize>, change: &str) -> Result<Building, String> {
    let axes: Vec<usize> = match axis {
        Some(axis) if axis < 3 => vec![axis],
        Some(axis) => return Err(format!("no axis {axis}")),
        None => vec![0, 1, 2],
    };
    let mut changed = building.clone();
    match field {
        "position" => {
            let mut world = scene::world_of_file(building.position);
            for axis in axes {
                world[axis] = scene::changed_value(world[axis], change)?;
            }
            changed.position = scene::file_of_world(world);
            let shift: [f32; 3] = std::array::from_fn(|axis| changed.position[axis] - building.position[axis]);
            for corner in &mut changed.bounds {
                for (value, by) in corner.iter_mut().zip(shift) {
                    *value += by;
                }
            }
        }
        "rotation" => {
            for axis in axes {
                changed.rotation[axis] = scene::changed_value(building.rotation[axis], change)?;
            }
        }
        "scale" => return Err("a building is not scaled: the client of 3.3.5a reads no scale".to_owned()),
        other => return Err(format!("a building has no field '{other}'")),
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_building_is_moved_with_its_bounds_and_turned_never_scaled() {
        let building = Building {
            file: FileRef::Path("World\\Inn.wmo".to_owned()),
            unique_id: 9,
            position: [100.0, 20.0, 200.0],
            rotation: [0.0, 45.0, 0.0],
            bounds: [[90.0, 10.0, 190.0], [110.0, 40.0, 210.0]],
            scale: 1.0,
            flags: 0,
            doodad_set: 1,
            name_set: 0,
        };
        let up = written(&building, "position", Some(2), "+5").unwrap();
        assert_eq!(up.position, [100.0, 25.0, 200.0]);
        assert_eq!(up.bounds, [[90.0, 15.0, 190.0], [110.0, 45.0, 210.0]]);
        assert_eq!(
            written(&building, "rotation", Some(1), "-=45").unwrap().rotation,
            [0.0, 0.0, 0.0]
        );
        assert!(written(&building, "scale", None, "2").is_err());
        let read = read(&EntityId::building(0, 9), &up, true);
        assert_eq!(read["components"]["model"]["doodad set"], json!(1));
        assert_eq!(read["changed"], json!(true));
    }
}
