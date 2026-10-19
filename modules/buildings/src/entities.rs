//! The buildings as entities of the scene: a building, its rooms (its groups), named as the game
//! names them, and the doodads of its sets placed, each under the room holding it, else under the
//! building. A piece a building, published bare once its tile is read, whole once its file is.

use std::collections::HashMap;

use uniwow_api::formats::{Building, FileRef, TileId, Wmo, WmoAreaRecord, placement};
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::scene::{self, Entity, EntityId, Piece, PieceBuilder, flags};

use crate::WmoFile;

/// The middle of the world in the axes of the files.
const ORIGIN: f32 = 32.0 * 1600.0 / 3.0;

/// What the picking needs of a building placed: its file, its transform, and the model and the
/// transform in the world of each doodad of its sets placed, by its place in `MODD`.
pub struct Picked {
    pub file: FileRef,
    pub transform: Mat4,
    pub doodads: HashMap<u32, (FileRef, Mat4)>,
}

/// What the picking needs of `building`, its doodads from its file `file` when it is read.
pub fn picked(building: &Building, file: Option<&WmoFile>) -> Picked {
    let transform = placement(building.position, building.rotation, building.scale);
    let mut doodads = HashMap::new();
    if let Some(file) = file {
        let mut chosen = vec![0usize];
        if building.doodad_set != 0 && usize::from(building.doodad_set) < file.sets.len() {
            chosen.push(usize::from(building.doodad_set));
        }
        for set in chosen.into_iter().filter_map(|set| file.sets.get(set)) {
            let first = set.first as usize;
            for place in first..(first + set.count as usize).min(file.doodads.len()) {
                let doodad = &file.doodads[place];
                let [x, y, z, w] = doodad.rotation;
                let own = Mat4::from_scale_rotation_translation(
                    Vec3::splat(doodad.scale),
                    uniwow_api::glam::Quat::from_xyzw(x, y, z, w).normalize(),
                    Vec3::from(doodad.position),
                );
                doodads.insert(place as u32, (doodad.file.clone(), transform * own));
            }
        }
    }
    Picked {
        file: building.file.clone(),
        transform,
        doodads,
    }
}

/// A group of a building as the scene lists it: its id (`WmoGroup::id`), its name of `MOGN`, and its
/// bounds in the building's axes.
#[derive(Clone, Debug, PartialEq)]
pub struct Room {
    pub id: u32,
    pub name: String,
    pub bounds: [[f32; 3]; 2],
}

/// The rooms of `wmo`, in the order of its groups.
pub fn rooms(wmo: &Wmo) -> Vec<Room> {
    wmo.groups
        .iter()
        .map(|group| Room {
            id: group.id,
            name: group.name.clone(),
            bounds: group.bounds,
        })
        .collect()
}

/// The names of the rooms of `WMOAreaTable.dbc`, by building, name set and group.
pub type RoomNames = HashMap<(u32, i32, i32), String>;

pub fn room_names(rows: &[WmoAreaRecord]) -> RoomNames {
    rows.iter()
        .filter(|row| !row.name.is_empty())
        .map(|row| ((row.wmo, row.name_set, row.group), row.name.clone()))
        .collect()
}

/// The bounds in the world of `bounds`, in the axes of the files of the tiles.
fn of_file(bounds: [[f32; 3]; 2]) -> [[f32; 3]; 2] {
    let world = |[x, y, z]: [f32; 3]| [ORIGIN - z, ORIGIN - x, y];
    let (a, b) = (world(bounds[0]), world(bounds[1]));
    [
        [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
        [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
    ]
}

/// The bounds in the world of `bounds`, in the axes `transform` places.
fn placed(transform: &Mat4, bounds: [[f32; 3]; 2]) -> [[f32; 3]; 2] {
    let [low, high] = bounds;
    let mut out = [[f32::MAX; 3], [f32::MIN; 3]];
    for corner in 0..8 {
        let point = Vec3::new(
            if corner & 1 == 0 { low[0] } else { high[0] },
            if corner & 2 == 0 { low[1] } else { high[1] },
            if corner & 4 == 0 { low[2] } else { high[2] },
        );
        let world = transform.transform_point3(point).to_array();
        for axis in 0..3 {
            out[0][axis] = out[0][axis].min(world[axis]);
            out[1][axis] = out[1][axis].max(world[axis]);
        }
    }
    out
}

fn middle(bounds: [[f32; 3]; 2]) -> [f32; 3] {
    [0, 1, 2].map(|axis| (bounds[0][axis] + bounds[1][axis]) / 2.0)
}

fn label(file: &FileRef, path_of: &dyn Fn(u32) -> Option<String>) -> String {
    match file {
        FileRef::Id(id) => path_of(*id).map_or_else(|| scene::stem(file), |path| scene::stem(&FileRef::Path(path))),
        file => scene::stem(file),
    }
}

fn push_building(builder: &mut PieceBuilder, map: u32, building: &Building, name: &str) -> u32 {
    let transform = placement(building.position, building.rotation, building.scale);
    builder.push(Entity {
        id: EntityId::building(map, building.unique_id),
        flags: flags::CLIENT,
        parent: None,
        label: name,
        extra: 0,
        origin: transform.w_axis.truncate().to_array(),
        bounds: of_file(building.bounds),
    })
}

/// The building `building` of the map `map` alone, its file not yet read, stored in `tiles`.
pub fn bare(map: u32, building: &Building, tiles: Vec<TileId>, path_of: &dyn Fn(u32) -> Option<String>) -> Piece {
    let mut builder = PieceBuilder::new(tiles);
    push_building(&mut builder, map, building, &label(&building.file, path_of));
    builder.build()
}

/// The building `building` of the map `map` whole, of its file `file` read: its rooms, named by
/// `names` for its name set, else by their name in the file, else *Room n*; the doodads of the set
/// 0 and of the set it names, each under the first room holding it.
pub fn whole(
    map: u32,
    building: &Building,
    tiles: Vec<TileId>,
    file: &WmoFile,
    names: &RoomNames,
    path_of: &dyn Fn(u32) -> Option<String>,
) -> Piece {
    let transform = placement(building.position, building.rotation, building.scale);
    let mut builder = PieceBuilder::with_capacity(tiles, 1 + file.rooms.len() + file.doodads.len());
    let own = push_building(&mut builder, map, building, &label(&building.file, path_of));
    let set = i32::from(building.name_set);
    let rooms: Vec<u32> = file
        .rooms
        .iter()
        .enumerate()
        .map(|(index, room)| {
            let key = |set| (file.id, set, room.id as i32);
            let name = names
                .get(&key(set))
                .or_else(|| names.get(&key(0)))
                .map(String::as_str)
                .unwrap_or(&room.name);
            let bounds = placed(&transform, room.bounds);
            builder.push(Entity {
                id: EntityId::room(map, building.unique_id, index as u32),
                flags: flags::CLIENT,
                parent: Some(own),
                label: name,
                extra: room.id,
                origin: middle(bounds),
                bounds,
            })
        })
        .collect();
    let mut chosen = vec![0usize];
    if building.doodad_set != 0 && usize::from(building.doodad_set) < file.sets.len() {
        chosen.push(usize::from(building.doodad_set));
    }
    let mut stems: HashMap<&FileRef, String> = HashMap::new();
    for set in chosen.into_iter().filter_map(|set| file.sets.get(set)) {
        let first = set.first as usize;
        for place in first..(first + set.count as usize).min(file.doodads.len()) {
            let doodad = &file.doodads[place];
            let origin = transform.transform_point3(Vec3::from(doodad.position)).to_array();
            let parent = file
                .holders
                .get(place)
                .and_then(|held| held.first())
                .and_then(|group| rooms.get(usize::from(*group)).copied())
                .unwrap_or(own);
            let name = stems
                .entry(&doodad.file)
                .or_insert_with(|| label(&doodad.file, path_of));
            builder.push(Entity {
                id: EntityId::building_doodad(map, building.unique_id, place as u32),
                flags: flags::CLIENT,
                parent: Some(parent),
                label: name,
                extra: 0,
                origin,
                bounds: scene::box_around(origin, doodad.scale.max(0.1)),
            });
        }
    }
    builder.build()
}
