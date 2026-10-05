//! The textures of the terrain in arrays, one for each class of texture (format, size and levels),
//! so that a tile is drawn in one draw whatever textures its chunks have: a texture is a layer of
//! an array of its class, read once and shared between the tiles. An array grows as textures come,
//! a layer no tile holds is given to the next texture, and an array that holds none is dropped.
//! The interface thread never waits for the jobs placing textures: it reads the views of the arrays
//! as they last published them, their generation and their bytes, without their lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use uniwow_api::formats::{FileRef, Formats, Texture, TextureFormat};
use uniwow_api::{log, wgpu};

/// The arrays the shader binds at once.
pub const SLOTS: usize = 12;

/// What a chunk without a layer has in its place: drawn white.
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
type Views = Arc<Vec<Option<wgpu::TextureView>>>;

/// A texture of the cache: placed once, none when it cannot be.
type Cell = Arc<OnceLock<Option<Arc<Placed>>>>;

pub struct TextureArrays {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Whether the textures stored as BC go to the GPU so.
    block_compression: bool,
    max_layers: u32,
    arrays: Mutex<Arrays>,
    entries: Mutex<HashMap<FileRef, Cell>>,
    /// Published by the jobs at each change of the arrays, under their lock: their views, what they
    /// take on the GPU, and the count of the changes, after which the bind group of the terrain is
    /// made again.
    published: Mutex<Views>,
    bytes: AtomicU64,
    generation: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The capacity an array of `capacity` layers grows to, at most `max`: twice, then 32 more.
fn grown(capacity: u32, max: u32) -> u32 {
    (if capacity < 32 { capacity * 2 } else { capacity + 32 }).min(max)
}

impl TextureArrays {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let mut arrays = Arrays::default();
        arrays.slots.resize_with(SLOTS, || None);
        Self {
            device: device.clone(),
            queue: queue.clone(),
            block_compression: device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            max_layers: device.limits().max_texture_array_layers,
            arrays: Mutex::new(arrays),
            entries: Mutex::default(),
            published: Mutex::new(Arc::new(vec![None; SLOTS])),
            bytes: AtomicU64::new(0),
            generation: AtomicU64::new(0),
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
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn block_compression(&self) -> bool {
        self.block_compression
    }

    /// The texture `file`, placed in an array; none when it cannot be read or placed, which is said
    /// once in the log. The first job asking reads it, the others asking meanwhile wait for it.
    pub fn get(&self, formats: &dyn Formats, file: &FileRef) -> Option<Arc<Placed>> {
        let cell = {
            let mut entries = lock(&self.entries);
            let cell = entries.entry(file.clone()).or_default();
            // Held before the lock is given back, so that a purge does not free it meanwhile.
            if let Some(placed) = cell.get() {
                return placed.clone();
            }
            cell.clone()
        };
        cell.get_or_init(|| match self.load(formats, file) {
            Ok(placed) => Some(Arc::new(placed)),
            Err(reason) => {
                log::warn!("a texture of the terrain is left out: {reason}");
                None
            }
        })
        .clone()
    }

    /// The texture `file` read, as BC when the device takes it and the file stores it so, else as
    /// RGBA, then written to a layer of the array of its class.
    fn load(&self, formats: &dyn Formats, file: &FileRef) -> Result<Placed, String> {
        let mut texture = if self.block_compression {
            formats.texture(file)?
        } else {
            formats.texture_rgba(file)?
        };
        if texture.format != TextureFormat::Rgba8 && (texture.width % 4 != 0 || texture.height % 4 != 0) {
            texture = formats.texture_rgba(file)?;
        }
        self.place(&texture)
    }

    /// Writes `texture` to a free layer of an array of its class, growing one or making one when
    /// none is free.
    pub fn place(&self, texture: &Texture) -> Result<Placed, String> {
        let format = match texture.format {
            TextureFormat::Rgba8 => wgpu::TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Bc1 => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
            TextureFormat::Bc2 => wgpu::TextureFormat::Bc2RgbaUnormSrgb,
            TextureFormat::Bc3 => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
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
        let mut arrays = lock(&self.arrays);
        let (slot, layer) = self.free_layer(&mut arrays, class).ok_or_else(|| {
            format!(
                "{SLOTS} arrays of textures are full, the last of {}×{} {format:?}",
                class.width, class.height
            )
        })?;
        let array = arrays.slots[slot].as_mut().expect("the layer is in an array");
        for (level, data) in texture.levels.iter().take(levels).enumerate() {
            let (row, rows) = class.level_layout(level as u32);
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &array.texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(rows),
                },
                wgpu::Extent3d {
                    width: (class.width >> level).max(1),
                    height: (class.height >> level).max(1),
                    depth_or_array_layers: 1,
                },
            );
        }
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
        Some((slot, layer as u32))
    }

    /// An array of `class` with `capacity` layers, those of `from` copied into it.
    fn create(&self, class: Class, capacity: u32, from: Option<&Array>) -> Array {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terrain textures"),
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
                label: Some("terrain textures grown"),
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
            // After the writes to the old array queued so far, which a submission sends first.
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

    /// Forgets the textures no tile holds any more, their layers given back, and drops the arrays
    /// that hold none. It waits for the jobs placing textures: a job of its own runs it.
    pub fn purge(&self) {
        let mut freed = Vec::new();
        lock(&self.entries).retain(|_, cell| match cell.get() {
            // Neither a tile nor a job asking for it holds it.
            Some(Some(placed)) if Arc::strong_count(placed) == 1 && Arc::strong_count(cell) == 1 => {
                freed.push((placed.slot, placed.layer));
                false
            }
            // A texture being read, or one that could not be: kept, not to be read again.
            _ => true,
        });
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
    /// group of the terrain.
    pub fn views(&self) -> (u64, Views) {
        let generation = self.generation();
        (generation, lock(&self.published).clone())
    }

    /// Counts the changes of the arrays.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// The texture of an array and how many layers it has, by its slot, for the tests.
    #[cfg(test)]
    pub fn array(&self, slot: u32) -> Option<(wgpu::Texture, usize)> {
        let arrays = lock(&self.arrays);
        let array = arrays.slots[slot as usize].as_ref()?;
        Some((array.texture.clone(), array.used.len()))
    }
}
