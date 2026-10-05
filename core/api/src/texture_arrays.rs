//! Textures in arrays, one for each class of texture (format, size and levels), so that what draws
//! many textures binds a few arrays and draws them in one draw: a texture is a layer of an array of
//! its class, read once and shared between those holding it. An array grows as textures come, a
//! layer nobody holds is given to the next texture, and an array that holds none is dropped. The
//! arrays are a fixed count of slots, which a shader binds at once. The interface thread never
//! waits for the jobs placing textures: it reads the views of the arrays as they last published
//! them, their generation, their bytes and their counts, without their lock. A texture that cannot
//! be read is not read again; one refused for want of room is tried again. A layer is filled by a
//! copy the job submits, never by `Queue::write_texture`: wgpu-core 30 holds the state of
//! initialisation of the texture written while it takes its trackers, a submission takes them the
//! other way round, and the frame drawing the array would lock up with the job writing to it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use crate::formats::{FileRef, Formats, Texture, TextureFormat};
use crate::wgpu::util::DeviceExt;
use crate::{log, wgpu};

/// What a texture without a layer has in its place, as a code: drawn white by its shader.
pub const NONE: u32 = u32::MAX;

/// Where a texture is: the slot of its array and its layer there.
#[derive(Debug, PartialEq, Eq)]
pub struct Placed {
    pub slot: u32,
    pub layer: u32,
}

impl Placed {
    /// As the shader reads it: the slot in the high 16 bits, the layer in the low ones.
    pub fn code(&self) -> u32 {
        (self.slot << 16) | self.layer
    }
}

/// What the textures of an array have in common.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Class {
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    levels: u32,
}

impl Class {
    /// The bytes of a row of the level `level`, and its rows: of blocks of 4 × 4 texels for BC.
    fn level_layout(&self, level: u32) -> (u32, u32) {
        let (width, height) = ((self.width >> level).max(1), (self.height >> level).max(1));
        match self.format.block_copy_size(None) {
            Some(block) if self.format.is_compressed() => (width.div_ceil(4) * block, height.div_ceil(4)),
            _ => (width * 4, height),
        }
    }

    /// The bytes of a layer, every level counted.
    fn layer_bytes(&self) -> u64 {
        (0..self.levels)
            .map(|level| {
                let (row, rows) = self.level_layout(level);
                u64::from(row * rows)
            })
            .sum()
    }
}

struct Array {
    class: Class,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Whether each layer holds a texture.
    used: Vec<bool>,
}

/// The arrays by slot.
#[derive(Default)]
struct Arrays {
    slots: Vec<Option<Array>>,
}

/// The views of the arrays by slot, as last published.
pub type Views = Arc<Vec<Option<wgpu::TextureView>>>;

/// A texture of the cache, as its last reading left it.
enum Entry {
    Unread,
    Placed(Arc<Placed>),
    /// It could not be read: not read again.
    Unreadable,
    /// Every slot held a full array: tried again when asked again.
    NoRoom,
}

/// A texture of the cache; the job reading it holds its lock, the others asking wait for it.
type Cell = Arc<Mutex<Entry>>;

/// Why a texture is not placed.
enum Failure {
    Unreadable(String),
    NoRoom(String),
}

/// The textures of the arrays, as the statistics give them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub placed: usize,
    /// Those that could not be read.
    pub unreadable: usize,
    /// Those refused for want of room, until room is made.
    pub no_room: usize,
    /// The slots holding an array.
    pub arrays: usize,
    /// The layers of the arrays holding a texture, and all their layers.
    pub layers: usize,
    pub capacity: usize,
}

pub struct TextureArrays {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Who holds the arrays, for what the log says and the labels of the GPU.
    owner: &'static str,
    slots: usize,
    /// Whether the textures are read as sRGB, made linear by the GPU, or as they are stored.
    srgb: bool,
    /// Whether the textures stored as BC go to the GPU so.
    block_compression: bool,
    max_layers: u32,
    arrays: Mutex<Arrays>,
    entries: Mutex<HashMap<FileRef, Cell>>,
    /// Published by the jobs at each change of the arrays, under their lock: their views, what they
    /// take on the GPU, and the count of the changes, after which the bind group of their owner is
    /// made again.
    published: Mutex<Views>,
    bytes: AtomicU64,
    generation: AtomicU64,
    placed: AtomicUsize,
    unreadable: AtomicUsize,
    no_room: AtomicUsize,
    arrays_used: AtomicUsize,
    layers_used: AtomicUsize,
    capacity: AtomicUsize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The capacity an array of `capacity` layers grows to, at most `max`: twice, then 32 more.
fn grown(capacity: u32, max: u32) -> u32 {
    (if capacity < 32 { capacity * 2 } else { capacity + 32 }).min(max)
}

impl TextureArrays {
    /// The arrays of `owner`, `slots` of them at most, their textures read as sRGB when `srgb` says.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, owner: &'static str, slots: usize, srgb: bool) -> Self {
        let mut arrays = Arrays::default();
        arrays.slots.resize_with(slots, || None);
        Self {
            device: device.clone(),
            queue: queue.clone(),
            owner,
            slots,
            srgb,
            block_compression: device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            max_layers: device.limits().max_texture_array_layers,
            arrays: Mutex::new(arrays),
            entries: Mutex::default(),
            published: Mutex::new(Arc::new(vec![None; slots])),
            bytes: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            placed: AtomicUsize::new(0),
            unreadable: AtomicUsize::new(0),
            no_room: AtomicUsize::new(0),
            arrays_used: AtomicUsize::new(0),
            layers_used: AtomicUsize::new(0),
            capacity: AtomicUsize::new(0),
        }
    }

    /// The slots of the arrays, which a shader binds.
    pub fn slots(&self) -> usize {
        self.slots
    }

    /// The textures placed, refused and waiting for room, and the slots used, without a lock.
    pub fn counts(&self) -> Counts {
        Counts {
            placed: self.placed.load(Ordering::Acquire),
            unreadable: self.unreadable.load(Ordering::Acquire),
            no_room: self.no_room.load(Ordering::Acquire),
            arrays: self.arrays_used.load(Ordering::Acquire),
            layers: self.layers_used.load(Ordering::Acquire),
            capacity: self.capacity.load(Ordering::Acquire),
        }
    }

    /// Publishes the arrays as they are now, under their lock.
    fn publish(&self, arrays: &Arrays) {
        let views = arrays
            .slots
            .iter()
            .map(|array| array.as_ref().map(|a| a.view.clone()))
            .collect();
        let bytes = arrays
            .slots
            .iter()
            .flatten()
            .map(|array| array.class.layer_bytes() * array.used.len() as u64)
            .sum();
        *lock(&self.published) = Arc::new(views);
        self.bytes.store(bytes, Ordering::Release);
        self.arrays_used
            .store(arrays.slots.iter().flatten().count(), Ordering::Release);
        let capacity = arrays.slots.iter().flatten().map(|array| array.used.len()).sum();
        self.capacity.store(capacity, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn block_compression(&self) -> bool {
        self.block_compression
    }

    /// The texture `file`, placed in an array; none when it cannot be read, or placed for want of
    /// room, which is said in the log the first time. The first job asking reads it, the others
    /// asking meanwhile wait for it; one refused for want of room is read again when asked again.
    pub fn get(&self, formats: &dyn Formats, file: &FileRef) -> Option<Arc<Placed>> {
        // The cell is held from the lock of the cache on, so that a purge leaves it meanwhile.
        let cell = lock(&self.entries)
            .entry(file.clone())
            .or_insert_with(|| Arc::new(Mutex::new(Entry::Unread)))
            .clone();
        let mut entry = lock(&cell);
        match &*entry {
            Entry::Placed(placed) => return Some(placed.clone()),
            Entry::Unreadable => return None,
            Entry::Unread | Entry::NoRoom => {}
        }
        let waited = matches!(*entry, Entry::NoRoom);
        match self.load(formats, file) {
            Ok(placed) => {
                let placed = Arc::new(placed);
                *entry = Entry::Placed(placed.clone());
                self.placed.fetch_add(1, Ordering::AcqRel);
                if waited {
                    self.no_room.fetch_sub(1, Ordering::AcqRel);
                }
                Some(placed)
            }
            Err(Failure::Unreadable(reason)) => {
                log::warn!("a texture of {} is left out: {reason}", self.owner);
                *entry = Entry::Unreadable;
                self.unreadable.fetch_add(1, Ordering::AcqRel);
                None
            }
            Err(Failure::NoRoom(reason)) => {
                if !waited {
                    log::warn!("a texture of {} waits for room: {reason}", self.owner);
                    self.no_room.fetch_add(1, Ordering::AcqRel);
                }
                *entry = Entry::NoRoom;
                None
            }
        }
    }

    /// The texture `file` read, as BC when the device takes it and the file stores it so, else as
    /// RGBA, then written to a layer of the array of its class.
    fn load(&self, formats: &dyn Formats, file: &FileRef) -> Result<Placed, Failure> {
        let mut texture = if self.block_compression {
            formats.texture(file).map_err(Failure::Unreadable)?
        } else {
            formats.texture_rgba(file).map_err(Failure::Unreadable)?
        };
        if texture.format != TextureFormat::Rgba8 && (texture.width % 4 != 0 || texture.height % 4 != 0) {
            texture = formats.texture_rgba(file).map_err(Failure::Unreadable)?;
        }
        self.place(&texture).map_err(Failure::NoRoom)
    }

    /// Writes `texture` to a free layer of an array of its class, growing one or making one when
    /// none is free: its levels copied from a buffer of their own, in a submission of its own.
    pub fn place(&self, texture: &Texture) -> Result<Placed, String> {
        let format = match (texture.format, self.srgb) {
            (TextureFormat::Rgba8, true) => wgpu::TextureFormat::Rgba8UnormSrgb,
            (TextureFormat::Bc1, true) => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
            (TextureFormat::Bc2, true) => wgpu::TextureFormat::Bc2RgbaUnormSrgb,
            (TextureFormat::Bc3, true) => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
            (TextureFormat::Rgba8, false) => wgpu::TextureFormat::Rgba8Unorm,
            (TextureFormat::Bc1, false) => wgpu::TextureFormat::Bc1RgbaUnorm,
            (TextureFormat::Bc2, false) => wgpu::TextureFormat::Bc2RgbaUnorm,
            (TextureFormat::Bc3, false) => wgpu::TextureFormat::Bc3RgbaUnorm,
        };
        // The levels of BC at least 4 texels wide and high.
        let levels = match texture.format {
            TextureFormat::Rgba8 => texture.levels.len(),
            _ => (0..texture.levels.len())
                .take_while(|level| (texture.width >> level) >= 4 && (texture.height >> level) >= 4)
                .count(),
        };
        let class = Class {
            format,
            width: texture.width,
            height: texture.height,
            levels: levels as u32,
        };
        // The rows of each level apart by a multiple of what a copy from a buffer needs.
        let mut bytes = Vec::new();
        let mut copies = Vec::with_capacity(levels);
        for (level, data) in texture.levels.iter().take(levels).enumerate() {
            let (row, rows) = class.level_layout(level as u32);
            let stride = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            copies.push((bytes.len() as u64, stride, rows));
            let start = bytes.len();
            bytes.resize(start + (stride * rows) as usize, 0);
            for (line, texels) in data.chunks(row as usize).take(rows as usize).enumerate() {
                let at = start + line * stride as usize;
                bytes[at..at + texels.len()].copy_from_slice(texels);
            }
        }
        let source = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(self.owner),
            contents: &bytes,
            usage: wgpu::BufferUsages::COPY_SRC,
        });
        let mut arrays = lock(&self.arrays);
        let (slot, layer) = self.free_layer(&mut arrays, class).ok_or_else(|| {
            format!(
                "{} arrays of textures are full, the last of {}×{} {format:?}",
                self.slots, class.width, class.height
            )
        })?;
        let array = arrays.slots[slot].as_ref().expect("the layer is in an array");
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some(self.owner),
        });
        for (level, (offset, stride, rows)) in copies.into_iter().enumerate() {
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &source,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(stride),
                        rows_per_image: Some(rows),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &array.texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: (class.width >> level).max(1),
                    height: (class.height >> level).max(1),
                    depth_or_array_layers: 1,
                },
            );
        }
        // Under the lock of the arrays: before any copy of this array into a grown one.
        self.queue.submit([encoder.finish()]);
        Ok(Placed {
            slot: slot as u32,
            layer,
        })
    }

    /// A free layer for a texture of `class`, marked used: in an array of its class, in one grown,
    /// or in a new one; none when every slot holds a full array.
    fn free_layer(&self, arrays: &mut Arrays, class: Class) -> Option<(usize, u32)> {
        for (slot, array) in arrays.slots.iter_mut().enumerate() {
            if let Some(array) = array.as_mut().filter(|array| array.class == class)
                && let Some(layer) = array.used.iter().position(|used| !used)
            {
                array.used[layer] = true;
                self.layers_used.fetch_add(1, Ordering::AcqRel);
                return Some((slot, layer as u32));
            }
        }
        let growing = arrays.slots.iter().position(|array| {
            array
                .as_ref()
                .is_some_and(|array| array.class == class && (array.used.len() as u32) < self.max_layers)
        });
        let slot = match growing {
            Some(slot) => {
                let old = arrays.slots[slot].take().expect("found above");
                arrays.slots[slot] = Some(self.grow(old));
                slot
            }
            None => {
                let slot = arrays.slots.iter().position(Option::is_none)?;
                arrays.slots[slot] = Some(self.create(class, 4.min(self.max_layers), None));
                slot
            }
        };
        self.publish(arrays);
        let array = arrays.slots[slot].as_mut().expect("made above");
        let layer = array.used.iter().position(|used| !used).expect("a layer was added");
        array.used[layer] = true;
        self.layers_used.fetch_add(1, Ordering::AcqRel);
        Some((slot, layer as u32))
    }

    /// An array of `class` with `capacity` layers, those of `from` copied into it.
    fn create(&self, class: Class, capacity: u32, from: Option<&Array>) -> Array {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(self.owner),
            size: wgpu::Extent3d {
                width: class.width,
                height: class.height,
                depth_or_array_layers: capacity,
            },
            mip_level_count: class.levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: class.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut used = vec![false; capacity as usize];
        if let Some(from) = from {
            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(self.owner),
            });
            for level in 0..class.levels {
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &from.texture,
                        mip_level: level,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: level,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: (class.width >> level).max(1),
                        height: (class.height >> level).max(1),
                        depth_or_array_layers: from.used.len() as u32,
                    },
                );
            }
            // After the copies to the old array, each submitted under the same lock.
            self.queue.submit([encoder.finish()]);
            used[..from.used.len()].copy_from_slice(&from.used);
        }
        Array {
            class,
            view: texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            }),
            texture,
            used,
        }
    }

    fn grow(&self, old: Array) -> Array {
        let capacity = grown(old.used.len() as u32, self.max_layers);
        self.create(old.class, capacity, Some(&old))
    }

    /// Forgets the textures nobody holds any more, their layers given back, and drops the arrays
    /// that hold none. It waits for the jobs placing textures: a job of its own runs it.
    pub fn purge(&self) {
        let mut freed = Vec::new();
        lock(&self.entries).retain(|_, cell| {
            // Neither a holder nor a job asking for it holds it; one being read is kept.
            if Arc::strong_count(cell) > 1 {
                return true;
            }
            let entry = match cell.try_lock() {
                Ok(entry) => entry,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => return true,
            };
            match &*entry {
                Entry::Placed(placed) if Arc::strong_count(placed) == 1 => {
                    freed.push((placed.slot, placed.layer));
                    false
                }
                // One that could not be read, or waits for room: kept.
                _ => true,
            }
        });
        self.placed.fetch_sub(freed.len(), Ordering::AcqRel);
        self.layers_used.fetch_sub(freed.len(), Ordering::AcqRel);
        let mut arrays = lock(&self.arrays);
        for (slot, layer) in freed {
            if let Some(array) = arrays.slots[slot as usize].as_mut() {
                array.used[layer as usize] = false;
            }
        }
        let mut dropped = false;
        for slot in &mut arrays.slots {
            if slot.as_ref().is_some_and(|array| !array.used.contains(&true)) {
                *slot = None;
                dropped = true;
            }
        }
        if dropped {
            self.publish(&arrays);
        }
    }

    /// What the arrays take on the GPU, every layer counted, used or not, as last published.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
    }

    /// The generation of the arrays and the view of each slot, as last published, for the bind
    /// group of their owner.
    pub fn views(&self) -> (u64, Views) {
        let generation = self.generation();
        (generation, lock(&self.published).clone())
    }

    /// Counts the changes of the arrays.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// The texture of an array and how many layers it has, by its slot, for the tests of the owners.
    pub fn array(&self, slot: u32) -> Option<(wgpu::Texture, usize)> {
        let arrays = lock(&self.arrays);
        let array = arrays.slots[slot as usize].as_ref()?;
        Some((array.texture.clone(), array.used.len()))
    }
}
