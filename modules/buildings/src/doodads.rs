//! The doodads of the buildings through the service `models`, an owner for the doodads a building's
//! groups hold together (`buildings/<map>/<unique id>/<groups>`), each drawn while one of those
//! groups is seen: its set 0 always, and the set its placement names when that is another, each at
//! the transform of the building times its own, a doodad of the rooms lit by its colour (`MODD`) as
//! the client lights it. What the jobs placing them and those taking them away share is kept under
//! a lock, so that a building's doodads are placed only while it is kept, and taken away once it is
//! not.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use uniwow_api::formats::{DoodadSet, FileRef, WmoDoodad, WmoGroup};
use uniwow_api::glam::{Mat4, Quat, Vec3};
use uniwow_api::models::{Geosets, Instance, Interior, Look, Models, Motion};

/// The flags of a group: outside, lit as outside, inside.
const OUTSIDE: u32 = 0x8;
const LIT_OUTSIDE: u32 = 0x40;
const INSIDE: u32 = 0x2000;
/// The least the diffuse of a doodad inside is brightened to, and the most its light everywhere is
/// darkened to, in 255ths of its brightest channel.
const LEAST_DIFFUSE: f32 = 112.0 / 255.0;
const MOST_AMBIENT: f32 = 96.0 / 255.0;

/// The doodads of a building that its groups `.0` hold, drawn while the flag `.1` is set.
pub type Part = (Vec<u16>, Arc<AtomicBool>);

/// The owner of the doodads of the building `id` of the map `directory` that its groups `groups`
/// hold; `-` for those no group holds.
pub fn owner(directory: &str, id: u32, groups: &[u16]) -> String {
    let groups = if groups.is_empty() {
        "-".to_owned()
    } else {
        groups.iter().map(u16::to_string).collect::<Vec<_>>().join("+")
    };
    format!("buildings/{directory}/{id}/{groups}")
}

/// The groups holding each of `doodads` doodads, in their order.
pub fn holders(groups: &[WmoGroup], doodads: usize) -> Vec<Vec<u16>> {
    let mut holders = vec![Vec::new(); doodads];
    for (index, group) in groups.iter().enumerate() {
        for doodad in &group.doodad_refs {
            if let Some(held) = holders.get_mut(usize::from(*doodad))
                && !held.contains(&(index as u16))
            {
                held.push(index as u16);
            }
        }
    }
    holders
}

/// Whether each doodad, held by the groups `holders` of `groups`, is lit as the rooms are: held by a
/// group, and by none lit as outside (outside, lit as outside, or not inside), as the public
/// implementation WebWowViewerCpp decides it (`loadDoodads`, `getDoodad`, read for its facts); one
/// no group holds is lit by the light of the view.
pub fn lit_inside(groups: &[WmoGroup], holders: &[Vec<u16>]) -> Vec<bool> {
    let outside = |group: &u16| {
        groups
            .get(usize::from(*group))
            .is_none_or(|group| group.flags & (OUTSIDE | LIT_OUTSIDE) != 0 || group.flags & INSIDE == 0)
    };
    holders
        .iter()
        .map(|held| !held.is_empty() && !held.iter().any(outside))
        .collect()
}

/// The light of a doodad of the rooms of the colour `colour` (`MODD`, red first; its alpha left
/// out), as WebWowViewerCpp gives it to a doodad without flags (`applyLightingParamsToDoodad`, its
/// `fixDirectColor` and `fixAmbient1`): its diffuse the colour brightened, its brightest channel
/// raised to 112 of 255 at least, its hue kept; its light everywhere the colour darkened, its
/// brightest channel lowered to 96 of 255 at most.
pub fn interior(colour: [u8; 4]) -> Interior {
    let [r, g, b, _] = colour.map(|channel| f32::from(channel) / 255.0);
    let rgb = [r, g, b];
    let brightest = r.max(g).max(b).max(1.0 / 255.0);
    let scaled = |by: f32| rgb.map(|channel| (channel * by).min(1.0));
    Interior {
        diffuse: if brightest < LEAST_DIFFUSE {
            scaled(LEAST_DIFFUSE / brightest)
        } else {
            rgb
        },
        ambient: if brightest > MOST_AMBIENT {
            scaled(MOST_AMBIENT / brightest)
        } else {
            rgb
        },
    }
}

/// The instances of the doodads of a building at `transform`, of its sets `sets` and its doodads
/// `doodads`, held by the groups `holders`, those of the rooms `inside` lit by their colour, its
/// placement naming the set `named`: by the groups holding them, each by its place among the
/// doodads.
pub fn instances(
    models: &dyn Models,
    transform: &Mat4,
    sets: &[DoodadSet],
    doodads: &[WmoDoodad],
    holders: &[Vec<u16>],
    inside: &[bool],
    named: u16,
) -> Vec<(Vec<u16>, Vec<Instance>)> {
    let mut chosen = vec![0usize];
    if named != 0 && usize::from(named) < sets.len() {
        chosen.push(usize::from(named));
    }
    let mut parts: BTreeMap<Vec<u16>, Vec<Instance>> = BTreeMap::new();
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
            parts
                .entry(holders.get(index).cloned().unwrap_or_default())
                .or_default()
                .push(Instance {
                    id: index as u64,
                    look: models.look(&Look {
                        model: doodad.file.clone(),
                        textures: Vec::new(),
                        geosets: Geosets::All,
                    }),
                    transform: *transform * own,
                    alpha: 1.0,
                    motion: Motion::Standing,
                    interior: inside
                        .get(index)
                        .copied()
                        .unwrap_or(false)
                        .then(|| interior(doodad.colour)),
                });
        }
    }
    parts.into_iter().collect()
}

/// The buildings whose doodads are wanted, of the map `map`, and those placed with their owners
/// and the flags of their parts.
#[derive(Default)]
pub struct Owners {
    map: String,
    wanted: HashSet<u32>,
    placed: HashMap<u32, (Vec<String>, Arc<[Part]>)>,
}

impl Owners {
    /// Makes `wanted` the buildings whose doodads are wanted, of the map `map`. Those of another map
    /// placed before are taken away at once; whether some building placed is no longer wanted, for
    /// `settle` to take away.
    pub fn want(&mut self, models: &dyn Models, map: &str, wanted: HashSet<u32>) -> bool {
        if map != self.map {
            for (names, _) in self.placed.drain().map(|(_, placed)| placed) {
                for name in names {
                    models.clear(&name);
                }
            }
            self.map = map.to_owned();
        }
        self.wanted = wanted;
        self.placed.keys().any(|id| !self.wanted.contains(id))
    }

    /// Places the doodads `parts` of the building `id` of the map `map` while it is wanted, each
    /// part under its owner; whether they are placed.
    pub fn place(&mut self, models: &dyn Models, map: &str, id: u32, parts: &[(Vec<u16>, Vec<Instance>)]) -> bool {
        if map != self.map || !self.wanted.contains(&id) {
            return false;
        }
        let names: Vec<String> = parts.iter().map(|(groups, _)| owner(map, id, groups)).collect();
        if let Some((before, _)) = self.placed.get(&id) {
            for name in before.iter().filter(|name| !names.contains(name)) {
                models.clear(name);
            }
        }
        let flags: Vec<Part> = parts
            .iter()
            .zip(&names)
            .map(|((groups, instances), name)| {
                models.place(name, instances);
                (groups.clone(), models.shown(name))
            })
            .collect();
        self.placed.insert(id, (names, flags.into()));
        true
    }

    /// Takes away the doodads of the buildings placed that are no longer wanted.
    pub fn settle(&mut self, models: &dyn Models) {
        let wanted = &self.wanted;
        self.placed.retain(|id, (names, _)| {
            let kept = wanted.contains(id);
            if !kept {
                for name in names.iter() {
                    models.clear(name);
                }
            }
            kept
        });
    }

    /// How many buildings have their doodads placed.
    pub fn placed(&self) -> usize {
        self.placed.len()
    }

    /// The parts of the doodads of each building placed, with their flags.
    pub fn parts(&self) -> HashMap<u32, Arc<[Part]>> {
        self.placed
            .iter()
            .map(|(id, (_, parts))| (*id, parts.clone()))
            .collect()
    }
}
