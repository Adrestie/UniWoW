//! The doodads of the tiles as the service `models` draws them: the tiles the camera wants, each
//! doodad's instance at the transform of its file, and the owners of the tiles, a doodad listed by
//! several tiles placed once, by the first of them placed, as the client keeps one by its unique id.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::f32::consts::FRAC_PI_2;

use uniwow_api::formats::{Doodad, Placements};
use uniwow_api::glam::{Mat4, Quat, Vec3};
use uniwow_api::models::{Geosets, Instance, Look, Models, Motion};

/// The side of a tile, in yards.
pub const TILE: f32 = 1600.0 / 3.0;
/// The world's X and Y at the corner of the tile 0 0.
const ORIGIN: f32 = 32.0 * TILE;

/// A tile of a map by its place, as its file names it: `<map>_<x>_<y>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub fn centre(self) -> [f32; 2] {
        [
            ORIGIN - TILE * (self.y as f32 + 0.5),
            ORIGIN - TILE * (self.x as f32 + 0.5),
        ]
    }

    /// How far from the point `eye` its centre lies, on the ground, in tiles.
    fn distance(self, eye: [f32; 2]) -> f32 {
        let [x, y] = self.centre();
        (x - eye[0]).hypot(y - eye[1]) / TILE
    }
}

/// The tiles of the map, `tiles` at `y * 64 + x`, wanted around `eye` within `distance` tiles, the
/// nearest first: those whose centre lies within half a tile more, as the terrain chooses its own,
/// and those `held` within a whole tile more, so that a camera going to and fro over a border does
/// not read them again.
pub fn wanted(tiles: &[bool], eye: [f32; 2], distance: u32, held: &HashSet<TileId>) -> Vec<TileId> {
    let mut wanted: Vec<(TileId, f32)> = tiles
        .iter()
        .enumerate()
        .filter(|(_, exists)| **exists)
        .map(|(index, _)| {
            let tile = TileId {
                x: index as u32 % 64,
                y: index as u32 / 64,
            };
            (tile, tile.distance(eye))
        })
        .filter(|(tile, away)| {
            let reach = if held.contains(tile) { 1.0 } else { 0.5 };
            *away <= distance as f32 + reach
        })
        .collect();
    wanted.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    wanted.into_iter().map(|(tile, _)| tile).collect()
}

/// The owner of the doodads the tile `tile` of the map `directory` places.
pub fn owner(directory: &str, tile: TileId) -> String {
    format!("doodads/{directory}/{}_{}", tile.x, tile.y)
}

/// From the model of `doodad` to the world. The axes of its file, Y up, are those of the client's
/// tiles: the world's X is `ORIGIN` less its Z, its Y `ORIGIN` less its X, its Z its Y. Its
/// rotation, in degrees, as Noggit applies it in those axes, comes in the world's to turns about Z
/// by its Y less a quarter, about X by its X, about Y by less its Z, after the quarter turn back
/// about Z that the model takes.
pub fn transform(doodad: &Doodad) -> Mat4 {
    let [x, y, z] = doodad.position;
    let [a, b, c] = doodad.rotation.map(f32::to_radians);
    let rotation = Quat::from_rotation_z(b - FRAC_PI_2)
        * Quat::from_rotation_x(a)
        * Quat::from_rotation_y(-c)
        * Quat::from_rotation_z(-FRAC_PI_2);
    Mat4::from_scale_rotation_translation(
        Vec3::splat(doodad.scale),
        rotation,
        Vec3::new(ORIGIN - z, ORIGIN - x, y),
    )
}

/// The instances of the doodads of a tile, by their unique id: of a doodad the tile lists twice,
/// the first. Each the model of its file with its own textures and all its submeshes.
pub fn instances(placements: &Placements, models: &dyn Models) -> Vec<Instance> {
    let mut seen = HashSet::new();
    placements
        .doodads
        .iter()
        .filter(|doodad| seen.insert(doodad.unique_id))
        .map(|doodad| Instance {
            id: u64::from(doodad.unique_id),
            look: models.look(&Look {
                model: doodad.file.clone(),
                textures: Vec::new(),
                geosets: Geosets::All,
            }),
            transform: transform(doodad),
            alpha: 1.0,
            motion: Motion::Standing,
        })
        .collect()
}

/// What the jobs placing the tiles and those taking them away share: each change made under its
/// lock, so that a tile is placed only while it is wanted, and taken away once it is not.
#[derive(Default)]
pub struct Placing {
    /// The map whose tiles are wanted, by its folder, and those tiles.
    map: String,
    wanted: HashSet<TileId>,
    /// The tiles placed, each with every doodad it lists, by its id.
    tiles: HashMap<TileId, HashMap<u64, Instance>>,
    /// The tile placing each doodad placed.
    placer: HashMap<u64, TileId>,
}

impl Placing {
    /// Makes `wanted` the tiles wanted of the map `map`. The tiles of another map placed before are
    /// taken away at once; whether some tile placed is no longer wanted, for `settle` to take away.
    pub fn want(&mut self, models: &dyn Models, map: &str, wanted: HashSet<TileId>) -> bool {
        if map != self.map {
            for tile in self.tiles.keys() {
                models.clear(&owner(&self.map, *tile));
            }
            self.tiles.clear();
            self.placer.clear();
            self.map = map.to_owned();
        }
        self.wanted = wanted;
        self.tiles.keys().any(|tile| !self.wanted.contains(tile))
    }

    /// Places the doodads `instances` of the tile `tile` of the map `map` while it is wanted, but
    /// those another tile placed first; whether it is placed.
    pub fn place(&mut self, models: &dyn Models, map: &str, tile: TileId, instances: Vec<Instance>) -> bool {
        if map != self.map || !self.wanted.contains(&tile) {
            return false;
        }
        if self.tiles.contains_key(&tile) {
            return true;
        }
        let own: Vec<Instance> = instances
            .iter()
            .filter(|instance| *self.placer.entry(instance.id).or_insert(tile) == tile)
            .copied()
            .collect();
        self.tiles.insert(
            tile,
            instances.into_iter().map(|instance| (instance.id, instance)).collect(),
        );
        models.place(&owner(map, tile), &own);
        true
    }

    /// Takes away the tiles placed that are no longer wanted. Their doodads that a tile still placed
    /// lists go to it, to the first by its place of those listing each.
    pub fn settle(&mut self, models: &dyn Models) {
        let left: Vec<TileId> = self
            .tiles
            .keys()
            .filter(|tile| !self.wanted.contains(tile))
            .copied()
            .collect();
        let mut orphans = Vec::new();
        for tile in left {
            let listed = self.tiles.remove(&tile).unwrap_or_default();
            models.clear(&owner(&self.map, tile));
            for id in listed.keys() {
                if self.placer.get(id) == Some(&tile) {
                    self.placer.remove(id);
                    orphans.push(*id);
                }
            }
        }
        let mut handed: BTreeMap<TileId, Vec<Instance>> = BTreeMap::new();
        for id in orphans {
            let heir = self
                .tiles
                .iter()
                .filter_map(|(tile, listed)| Some((*tile, *listed.get(&id)?)))
                .min_by_key(|(tile, _)| *tile);
            if let Some((tile, instance)) = heir {
                self.placer.insert(id, tile);
                handed.entry(tile).or_default().push(instance);
            }
        }
        for (tile, instances) in handed {
            models.change(&owner(&self.map, tile), &instances, &[]);
        }
    }

    /// The tiles placed, the doodads placed, and those the tiles list.
    pub fn counts(&self) -> [usize; 3] {
        [
            self.tiles.len(),
            self.placer.len(),
            self.tiles.values().map(HashMap::len).sum(),
        ]
    }
}
