//! The instances of each owner, kept until it gives others, grouped by look and by tile: a group is
//! an owner, a look and a tile, its instances a range of its owner's range in the arena of the
//! instances, which all owners share. Only the thread of the owner writes its range. A set whose
//! groups keep their places, as moving instances do, is written in place, out of the lock the layer
//! takes; one whose groups change goes to a new range written whole, then published with its groups
//! under that lock, so that no frame draws new groups over an old layout, nor an empty range. A
//! range is given back to the arena once nothing published names it, the layer holding what a
//! frame draws until the next.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use uniwow_api::arena::Arena;
use uniwow_api::glam::Vec3;
use uniwow_api::journal;
use uniwow_api::models::{Instance, LookId};
use uniwow_api::{bytemuck, log};

use crate::gpu::InstanceGpu;
use crate::lock;

/// The side of a tile, in yards: the instances of a look are grouped by it.
pub const TILE: f32 = 1600.0 / 3.0;
/// The fewest instances a range holds.
const CAPACITY: u32 = 64;

/// A group: the instances of an owner of one look in one tile.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub look: LookId,
    pub tile: [i32; 2],
    /// Its instances in the range of its owner.
    pub first: u32,
    pub count: u32,
    /// The box of the origins of its instances, and the largest scale among them.
    pub low: Vec3,
    pub high: Vec3,
    pub scale: f32,
}

/// The range of an owner's instances in the arena of the instances, given back when dropped.
pub struct Written {
    pub arena: Arc<Arena>,
    range: Range<u64>,
}

impl Written {
    /// Its first instance in the arena.
    pub fn first(&self) -> u32 {
        self.range.start as u32
    }
}

impl Drop for Written {
    fn drop(&mut self) {
        self.arena.give(self.range.clone());
    }
}

/// What the thread of an owner hands the layer: its range, its groups, the number of its layout,
/// changed with the range, and its instances, in the order of the range.
#[derive(Default)]
pub struct Published {
    pub written: Option<Arc<Written>>,
    pub groups: Vec<Group>,
    pub layout: u64,
    pub instances: Vec<Instance>,
}

/// The tile of `position`.
pub fn tile(position: Vec3) -> [i32; 2] {
    [(position.x / TILE).floor() as i32, (position.y / TILE).floor() as i32]
}

/// A colour from 0 to 1 in one float the shader reads back whole (`unpacked`): its red, green and
/// blue in 255ths to the nearest, red the lowest of three bytes, under 2^24, which a float holds
/// exactly.
pub fn packed(colour: [f32; 3]) -> f32 {
    let [r, g, b] = colour.map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u32);
    (r | g << 8 | b << 16) as f32
}

/// The steps of each of the two coordinates of a direction folded on an octahedron (`towards`).
const OCTAHEDRON: f32 = 2047.0;

/// A unit direction in one float the shader reads back (`towards_inside`): folded on an octahedron,
/// its two coordinates in 2048 steps each, the first in the low 11 bits, plus 2, under 2^24.
pub fn towards(direction: [f32; 3]) -> f32 {
    let [x, y, z] = direction;
    let sum = (x.abs() + y.abs() + z.abs()).max(1e-6);
    let (x, y, z) = (x / sum, y / sum, z / sum);
    let sign = |value: f32| if value < 0.0 { -1.0 } else { 1.0 };
    let (u, v) = if z < 0.0 {
        ((1.0 - y.abs()) * sign(x), (1.0 - x.abs()) * sign(y))
    } else {
        (x, y)
    };
    let step = |value: f32| ((value.clamp(-1.0, 1.0) * 0.5 + 0.5) * OCTAHEDRON).round() as u32;
    (2 + (step(u) | step(v) << 11)) as f32
}

/// `instance` as the shader reads it: the rows of its transform, then its alpha and its light inside
/// a building, its light everywhere and its diffuse packed, and 1 for the light of the insides of the
/// client, the direction of its light (`towards`) otherwise; 0 for the light of the view.
pub fn as_gpu(instance: &Instance) -> InstanceGpu {
    let transform = instance.transform;
    let [ambient, diffuse, inside] = match instance.interior {
        Some(interior) => [
            packed(interior.ambient),
            packed(interior.diffuse),
            interior.towards.map_or(1.0, towards),
        ],
        None => [0.0; 3],
    };
    InstanceGpu {
        rows: std::array::from_fn(|row| transform.row(row).to_array()),
        extra: [instance.alpha, ambient, diffuse, inside],
    }
}

/// Sorts `instances` by look, tile and id, and gives their groups.
pub fn group(instances: &mut [Instance]) -> Vec<Group> {
    let key = |instance: &Instance| (instance.look, tile(instance.transform.w_axis.truncate()));
    instances.sort_by_key(|instance| {
        let (look, [x, y]) = key(instance);
        (look, x, y, instance.id)
    });
    let mut groups: Vec<Group> = Vec::new();
    for (at, instance) in instances.iter().enumerate() {
        let (look, tile) = key(instance);
        let origin = instance.transform.w_axis.truncate();
        let transform = instance.transform;
        let scale = transform
            .x_axis
            .truncate()
            .length()
            .max(transform.y_axis.truncate().length())
            .max(transform.z_axis.truncate().length());
        match groups.last_mut() {
            Some(group) if group.look == look && group.tile == tile => {
                group.count += 1;
                group.low = group.low.min(origin);
                group.high = group.high.max(origin);
                group.scale = group.scale.max(scale);
            }
            _ => groups.push(Group {
                look,
                tile,
                first: at as u32,
                count: 1,
                low: origin,
                high: origin,
                scale,
            }),
        }
    }
    groups
}

/// Whether `new` puts its groups where `old` does.
pub fn same_places(old: &[Group], new: &[Group]) -> bool {
    old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(a, b)| (a.look, a.tile, a.first, a.count) == (b.look, b.tile, b.first, b.count))
}

/// `instances` with `changed` in place of those of the same id, or after them, and without the ids
/// `removed`.
pub fn merge(instances: &[Instance], changed: &[Instance], removed: &[u64]) -> Vec<Instance> {
    let mut merged = instances.to_vec();
    let mut at: HashMap<u64, usize> = merged
        .iter()
        .enumerate()
        .map(|(index, instance)| (instance.id, index))
        .collect();
    for instance in changed {
        match at.get(&instance.id) {
            Some(index) => merged[*index] = *instance,
            None => {
                at.insert(instance.id, merged.len());
                merged.push(*instance);
            }
        }
    }
    if !removed.is_empty() {
        merged.retain(|instance| !removed.contains(&instance.id));
    }
    merged
}

/// What the thread of an owner keeps: its set, its range and its groups.
#[derive(Default)]
struct Kept {
    instances: Vec<Instance>,
    written: Option<Arc<Written>>,
    groups: Vec<Group>,
    layout: u64,
}

/// An owner of instances.
pub struct Slot {
    /// Its number, by which the layer knows its groups.
    pub number: u32,
    /// Whether its instances are drawn, as its owner sets it.
    pub shown: Arc<AtomicBool>,
    kept: Mutex<Kept>,
    published: Mutex<Arc<Published>>,
}

impl Slot {
    pub fn new(number: u32) -> Self {
        Self {
            number,
            shown: Arc::new(AtomicBool::new(true)),
            kept: Mutex::default(),
            published: Mutex::default(),
        }
    }

    /// What the layer draws of this owner.
    pub fn published(&self) -> Arc<Published> {
        journal::lock(&self.published, "models published").clone()
    }

    /// Changes the set with `update`, then writes it into `arena`, from the calling thread: none
    /// before the view has its device, when the set is kept and written by the next change. A set
    /// the arena cannot hold is kept, unpublished, and said in the log.
    pub fn update(&self, arena: Option<&Arc<Arena>>, update: impl FnOnce(&mut Vec<Instance>)) {
        let mut kept = lock(&self.kept);
        update(&mut kept.instances);
        let groups = group(&mut kept.instances);
        let Some(arena) = arena else {
            kept.written = None;
            kept.groups = groups;
            return;
        };
        let data: Vec<InstanceGpu> = kept.instances.iter().map(as_gpu).collect();
        let instances = kept.instances.clone();
        if let Some(written) = kept.written.clone().filter(|_| same_places(&kept.groups, &groups)) {
            // Out of the lock of the layer: it never waits for the queue of the GPU.
            arena.write(written.range.start, bytemuck::cast_slice(&data));
            kept.groups = groups.clone();
            let layout = kept.layout;
            *journal::lock(&self.published, "models published") = Arc::new(Published {
                written: Some(written),
                groups,
                layout,
                instances,
            });
            return;
        }
        let capacity = (data.len() as u32).next_power_of_two().max(CAPACITY);
        let mut filled = data;
        filled.resize(capacity as usize, InstanceGpu::default());
        let range = match arena.put(bytemuck::cast_slice(&filled)) {
            Ok(range) => range,
            Err(reason) => {
                log::warn!("the instances of an owner are not drawn: {reason}");
                kept.written = None;
                kept.groups = groups;
                return;
            }
        };
        let written = Arc::new(Written {
            arena: arena.clone(),
            range,
        });
        kept.layout += 1;
        kept.written = Some(written.clone());
        kept.groups = groups.clone();
        let layout = kept.layout;
        *journal::lock(&self.published, "models published") = Arc::new(Published {
            written: Some(written),
            groups,
            layout,
            instances,
        });
    }
}
