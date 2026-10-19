//! The doodads of the tiles as the service `models` draws them: the tiles the camera wants, each
//! doodad's instance at the transform of its file, and the owners of the tiles, a doodad listed by
//! several tiles placed once, by the first of them placed, as the client keeps one by its unique id.

use std::collections::{BTreeMap, HashMap, HashSet};

use uniwow_api::formats::{Doodad, FileRef, Placements, TileId, placement};
use uniwow_api::models::{Geosets, Instance, Look, Models, Motion};
use uniwow_api::scene::{self, Entity, EntityId, Piece, PieceBuilder, flags};

/// The owner of the doodads the tile `tile` of the map `directory` places.
pub fn owner(directory: &str, tile: TileId) -> String {
    format!("doodads/{directory}/{}_{}", tile.x, tile.y)
}

/// The instance of `doodad`: the model of its file with its own textures and all its submeshes.
pub fn instance(doodad: &Doodad, models: &dyn Models) -> Instance {
    Instance {
        id: u64::from(doodad.unique_id),
        look: models.look(&Look {
            model: doodad.file.clone(),
            textures: Vec::new(),
            geosets: Geosets::All,
        }),
        transform: placement(doodad.position, doodad.rotation, doodad.scale),
        alpha: 1.0,
        motion: Motion::Standing,
        interior: None,
    }
}

/// The instances of the doodads of a tile, by their unique id: of a doodad the tile lists twice,
/// the first.
pub fn instances(placements: &Placements, models: &dyn Models) -> Vec<Instance> {
    let mut seen = HashSet::new();
    placements
        .doodads
        .iter()
        .filter(|doodad| seen.insert(doodad.unique_id))
        .map(|doodad| instance(doodad, models))
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

    /// The doodads of the tile `tile` placed that another tile placed first with another look or
    /// place: listed again under their unique id with other data, entities of their own.
    pub fn differing(&self, tile: TileId) -> HashSet<u64> {
        let Some(listed) = self.tiles.get(&tile) else {
            return HashSet::new();
        };
        listed
            .iter()
            .filter(|(id, instance)| {
                let Some(placer) = self.placer.get(id).filter(|placer| **placer != tile) else {
                    return false;
                };
                let Some(first) = self.tiles.get(placer).and_then(|listed| listed.get(id)) else {
                    return false;
                };
                first.look != instance.look || !first.transform.abs_diff_eq(instance.transform, 0.01)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Moves the doodad `id` of the map `map` to `transform` in every tile listing it, its instance
    /// placed again by the tile placing it.
    pub fn moved(&mut self, models: &dyn Models, map: &str, id: u64, transform: uniwow_api::glam::Mat4) {
        if map != self.map {
            return;
        }
        for (tile, listed) in &mut self.tiles {
            if let Some(instance) = listed.get_mut(&id) {
                instance.transform = transform;
                if self.placer.get(&id) == Some(tile) {
                    models.change(&owner(map, *tile), &[*instance], &[]);
                }
            }
        }
    }

    /// Puts `instance`, a doodad the user created, in the tile `tile` placed, its instance placed
    /// there unless another tile placed it first.
    pub fn put(&mut self, models: &dyn Models, map: &str, tile: TileId, instance: Instance) {
        if map != self.map {
            return;
        }
        let Some(listed) = self.tiles.get_mut(&tile) else {
            return;
        };
        listed.insert(instance.id, instance);
        if *self.placer.entry(instance.id).or_insert(tile) == tile {
            models.change(&owner(map, tile), &[instance], &[]);
        }
    }

    /// Takes the doodad `id` away from every tile placed.
    pub fn remove(&mut self, models: &dyn Models, map: &str, id: u64) {
        if map != self.map {
            return;
        }
        for (tile, listed) in &mut self.tiles {
            if listed.remove(&id).is_some() && self.placer.get(&id) == Some(tile) {
                models.change(&owner(map, *tile), &[], &[id]);
            }
        }
        self.placer.remove(&id);
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

/// The model and the transform of each doodad of a tile, by its unique id, for the picking.
pub type Placed = HashMap<u64, (FileRef, uniwow_api::glam::Mat4)>;

/// The doodads of `placements`, each listing once, by their unique id.
pub fn placed(placements: &Placements) -> Placed {
    let mut found = Placed::new();
    for doodad in &placements.doodads {
        found.entry(u64::from(doodad.unique_id)).or_insert_with(|| {
            (
                doodad.file.clone(),
                placement(doodad.position, doodad.rotation, doodad.scale),
            )
        });
    }
    found
}

/// The entities of the doodads of the tile `tile` of the map `map`, for the scene: each listing
/// once by its unique id, those of `again` as entities of their own; each named by the stem of its
/// model, `path_of` giving the paths of the FileDataIDs; its bounds a yard around its origin, its
/// scale counted, until the extents of the looks are known.
pub fn piece(
    placements: &Placements,
    map: u32,
    tile: TileId,
    again: &HashSet<u64>,
    path_of: &dyn Fn(u32) -> Option<String>,
) -> Piece {
    let mut seen = HashSet::new();
    let mut builder = PieceBuilder::with_capacity(vec![tile], placements.doodads.len());
    let mut stems: HashMap<FileRef, String> = HashMap::new();
    for doodad in placements.doodads.iter().filter(|doodad| seen.insert(doodad.unique_id)) {
        let id = match again.contains(&u64::from(doodad.unique_id)) {
            true => EntityId::doodad_again(map, doodad.unique_id, tile),
            false => EntityId::doodad(map, doodad.unique_id),
        };
        let label = stems.entry(doodad.file.clone()).or_insert_with(|| match &doodad.file {
            FileRef::Id(file) => {
                path_of(*file).map_or_else(|| scene::stem(&doodad.file), |path| scene::stem(&FileRef::Path(path)))
            }
            file => scene::stem(file),
        });
        let origin = placement(doodad.position, doodad.rotation, doodad.scale)
            .w_axis
            .truncate()
            .to_array();
        builder.push(Entity {
            id,
            flags: flags::CLIENT,
            parent: None,
            label,
            extra: 0,
            origin,
            bounds: scene::box_around(origin, doodad.scale.max(0.1)),
        });
    }
    builder.build()
}
