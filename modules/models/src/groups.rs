//! The instances of each owner, kept until it gives others, grouped by look and by tile: a group is
//! an owner, a look and a tile, its instances a range of its owner's buffer. Only the thread of the
//! owner writes that buffer. A set whose groups keep their places, as moving instances do, is
//! written in place, out of the lock the layer takes; one whose groups change goes to a new buffer
//! made filled, then published with its groups under that lock, so that no frame draws new groups
//! over an old layout, nor an empty buffer.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use uniwow_api::glam::Vec3;
use uniwow_api::models::{Instance, LookId};
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, wgpu};

use crate::gpu::InstanceGpu;
use crate::lock;

/// The side of a tile, in yards: the instances of a look are grouped by it.
pub const TILE: f32 = 1600.0 / 3.0;
/// The fewest instances a buffer holds.
const CAPACITY: u32 = 64;

/// A group: the instances of an owner of one look in one tile.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub look: LookId,
    pub tile: [i32; 2],
    /// Its instances in the buffer of its owner.
    pub first: u32,
    pub count: u32,
    /// The box of the origins of its instances, and the largest scale among them.
    pub low: Vec3,
    pub high: Vec3,
    pub scale: f32,
}

/// What the thread of an owner hands the layer: its buffer, its groups, the number of its layout,
/// changed with the buffer, and its instances, in the order of the buffer.
#[derive(Default)]
pub struct Published {
    pub buffer: Option<Arc<wgpu::Buffer>>,
    pub groups: Vec<Group>,
    pub layout: u64,
    pub instances: Vec<Instance>,
}

/// The tile of `position`.
pub fn tile(position: Vec3) -> [i32; 2] {
    [(position.x / TILE).floor() as i32, (position.y / TILE).floor() as i32]
}

/// `instance` as the shader reads it.
pub fn as_gpu(instance: &Instance) -> InstanceGpu {
    let transform = instance.transform;
    InstanceGpu {
        rows: std::array::from_fn(|row| transform.row(row).to_array()),
        extra: [instance.alpha, 0.0, 0.0, 0.0],
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

/// What the thread of an owner keeps: its set, its buffer and its groups.
#[derive(Default)]
struct Kept {
    instances: Vec<Instance>,
    buffer: Option<Arc<wgpu::Buffer>>,
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
        lock(&self.published).clone()
    }

    /// Changes the set with `update`, then writes it, from the calling thread: none before the
    /// view has its device, when the set is kept and written by the next change.
    pub fn update(&self, gpu: Option<(&wgpu::Device, &wgpu::Queue)>, update: impl FnOnce(&mut Vec<Instance>)) {
        let mut kept = lock(&self.kept);
        update(&mut kept.instances);
        let groups = group(&mut kept.instances);
        let Some((device, queue)) = gpu else {
            kept.buffer = None;
            kept.groups = groups;
            return;
        };
        let data: Vec<InstanceGpu> = kept.instances.iter().map(as_gpu).collect();
        let instances = kept.instances.clone();
        if let Some(buffer) = kept.buffer.clone().filter(|_| same_places(&kept.groups, &groups)) {
            // Out of the lock of the layer: it never waits for the queue of the GPU.
            if !data.is_empty() {
                queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&data));
            }
            kept.groups = groups.clone();
            let layout = kept.layout;
            *lock(&self.published) = Arc::new(Published {
                buffer: Some(buffer),
                groups,
                layout,
                instances,
            });
            return;
        }
        let capacity = (data.len() as u32).next_power_of_two().max(CAPACITY);
        let mut filled = data;
        filled.resize(capacity as usize, InstanceGpu::default());
        let buffer = Arc::new(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("models instances"),
            contents: bytemuck::cast_slice(&filled),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        }));
        kept.layout += 1;
        kept.buffer = Some(buffer.clone());
        kept.groups = groups.clone();
        let layout = kept.layout;
        *lock(&self.published) = Arc::new(Published {
            buffer: Some(buffer),
            groups,
            layout,
            instances,
        });
    }
}
