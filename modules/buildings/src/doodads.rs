//! The doodads of the buildings through the service `models`, an owner a building
//! (`buildings/<map>/<unique id>`): its set 0 always, and the set its placement names when that is
//! another, each at the transform of the building times its own. What the jobs placing them and
//! those taking them away share is kept under a lock, so that a building's doodads are placed only
//! while it is kept, and taken away once it is not.

use std::collections::HashSet;

use uniwow_api::formats::{DoodadSet, FileRef, WmoDoodad};
use uniwow_api::glam::{Mat4, Quat, Vec3};
use uniwow_api::models::{Geosets, Instance, Look, Models, Motion};

/// The owner of the doodads of the building `id` of the map `directory`.
pub fn owner(directory: &str, id: u32) -> String {
    format!("buildings/{directory}/{id}")
}

/// The instances of the doodads of a building at `transform`, of its sets `sets` and its doodads
/// `doodads`, its placement naming the set `named`: by their place among the doodads.
pub fn instances(
    models: &dyn Models,
    transform: &Mat4,
    sets: &[DoodadSet],
    doodads: &[WmoDoodad],
    named: u16,
) -> Vec<Instance> {
    let mut chosen = vec![0usize];
    if named != 0 && usize::from(named) < sets.len() {
        chosen.push(usize::from(named));
    }
    let mut instances = Vec::new();
    for set in chosen.iter().filter_map(|index| sets.get(*index)) {
        let first = set.first as usize;
        for (index, doodad) in doodads.iter().enumerate().skip(first).take(set.count as usize) {
            if matches!(&doodad.file, FileRef::Path(path) if path.is_empty()) {
                continue;
            }
            let [x, y, z, w] = doodad.rotation;
            let own = Mat4::from_scale_rotation_translation(
                Vec3::splat(doodad.scale),
                Quat::from_xyzw(x, y, z, w).normalize(),
                Vec3::from(doodad.position),
            );
            instances.push(Instance {
                id: index as u64,
                look: models.look(&Look {
                    model: doodad.file.clone(),
                    textures: Vec::new(),
                    geosets: Geosets::All,
                }),
                transform: *transform * own,
                alpha: 1.0,
                motion: Motion::Standing,
            });
        }
    }
    instances
}

/// The buildings whose doodads are wanted, of the map `map`, and those placed.
#[derive(Default)]
pub struct Owners {
    map: String,
    wanted: HashSet<u32>,
    placed: HashSet<u32>,
}

impl Owners {
    /// Makes `wanted` the buildings whose doodads are wanted, of the map `map`. Those of another map
    /// placed before are taken away at once; whether some building placed is no longer wanted, for
    /// `settle` to take away.
    pub fn want(&mut self, models: &dyn Models, map: &str, wanted: HashSet<u32>) -> bool {
        if map != self.map {
            for id in self.placed.drain() {
                models.clear(&owner(&self.map, id));
            }
            self.map = map.to_owned();
        }
        self.wanted = wanted;
        self.placed.iter().any(|id| !self.wanted.contains(id))
    }

    /// Places the doodads `instances` of the building `id` of the map `map` while it is wanted;
    /// whether they are placed.
    pub fn place(&mut self, models: &dyn Models, map: &str, id: u32, instances: &[Instance]) -> bool {
        if map != self.map || !self.wanted.contains(&id) {
            return false;
        }
        models.place(&owner(map, id), instances);
        self.placed.insert(id);
        true
    }

    /// Takes away the doodads of the buildings placed that are no longer wanted.
    pub fn settle(&mut self, models: &dyn Models) {
        let left: Vec<u32> = self
            .placed
            .iter()
            .filter(|id| !self.wanted.contains(id))
            .copied()
            .collect();
        for id in left {
            models.clear(&owner(&self.map, id));
            self.placed.remove(&id);
        }
    }

    /// How many buildings have their doodads placed.
    pub fn placed(&self) -> usize {
        self.placed.len()
    }
}
