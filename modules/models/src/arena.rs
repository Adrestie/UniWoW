//! An arena of a buffer of the GPU shared by the models, in units of a fixed size: a range is given
//! from its holes, the first that holds it, and given back to them, merged with its neighbours. A
//! buffer too small for a range is replaced by one twice as large, the one before copied into it,
//! so that every range keeps its place. Its jobs write a range by a copy they submit, under the
//! lock of its holes, after any copy into a larger buffer (the rule of step 9.2f); the frame reads
//! the buffer and the bytes held without that lock.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use uniwow_api::wgpu;
use uniwow_api::wgpu::util::DeviceExt;

use crate::lock;

/// The holes of an arena of `capacity` units, sorted, none touching another.
#[derive(Debug, Default, PartialEq)]
pub struct Holes {
    holes: Vec<Range<u64>>,
    capacity: u64,
}

impl Holes {
    /// A range of `length` units from the first hole holding it.
    pub fn take(&mut self, length: u64) -> Option<Range<u64>> {
        let at = self.holes.iter().position(|hole| hole.end - hole.start >= length)?;
        let hole = &mut self.holes[at];
        let taken = hole.start..hole.start + length;
        hole.start += length;
        if hole.is_empty() {
            self.holes.remove(at);
        }
        Some(taken)
    }

    /// `range` given back, merged with the holes it touches.
    pub fn give(&mut self, range: Range<u64>) {
        if range.is_empty() {
            return;
        }
        let at = self.holes.partition_point(|hole| hole.start < range.start);
        self.holes.insert(at, range);
        if at + 1 < self.holes.len() && self.holes[at].end == self.holes[at + 1].start {
            self.holes[at].end = self.holes[at + 1].end;
            self.holes.remove(at + 1);
        }
        if at > 0 && self.holes[at - 1].end == self.holes[at].start {
            self.holes[at - 1].end = self.holes[at].end;
            self.holes.remove(at);
        }
    }

    /// The arena grown to `capacity` units, the units added a hole.
    pub fn grow(&mut self, capacity: u64) {
        let added = self.capacity..capacity;
        self.capacity = capacity;
        self.give(added);
    }

    pub fn capacity(&self) -> u64 {
        self.capacity
    }
}

pub struct Arena {
    device: wgpu::Device,
    queue: wgpu::Queue,
    label: &'static str,
    usage: wgpu::BufferUsages,
    /// The bytes of a unit, a multiple of 4.
    unit: u64,
    /// The fewest units of a buffer.
    least: u64,
    /// Held by a job while it takes a range and writes it, or grows the buffer.
    pub(crate) holes: Mutex<Holes>,
    /// The buffer now and its generation, counting the buffers: held only to read or replace them,
    /// so that the frame never waits for a job.
    buffer: Mutex<Option<(Arc<wgpu::Buffer>, u64)>>,
    /// The bytes of the buffer and of the units held, read without waiting either.
    held: AtomicU64,
    used: AtomicU64,
}

impl Arena {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        usage: wgpu::BufferUsages,
        unit: u64,
        least: u64,
    ) -> Self {
        Self {
            device: device.clone(),
            queue: queue.clone(),
            label,
            usage: usage | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            unit,
            least,
            holes: Mutex::default(),
            buffer: Mutex::default(),
            held: AtomicU64::new(0),
            used: AtomicU64::new(0),
        }
    }

    /// `data` written to a range of the arena, its units; refused when the buffer it needs is
    /// larger than the device takes.
    pub fn put(&self, data: &[u8]) -> Result<Range<u64>, String> {
        debug_assert_eq!(data.len() as u64 % self.unit, 0);
        let length = data.len() as u64 / self.unit;
        // Filled before taking the lock.
        let source = (!data.is_empty()).then(|| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(self.label),
                contents: data,
                usage: wgpu::BufferUsages::COPY_SRC,
            })
        });
        let mut holes = lock(&self.holes);
        let range = match holes.take(length) {
            Some(range) => range,
            None => {
                self.grow(&mut holes, length)?;
                holes.take(length).expect("grown to hold it")
            }
        };
        if let Some(source) = source {
            let (buffer, _) = self.buffer().expect("grown before");
            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(self.label),
            });
            encoder.copy_buffer_to_buffer(&source, 0, &buffer, range.start * self.unit, data.len() as u64);
            // Under the lock: after any copy into a larger buffer.
            self.queue.submit([encoder.finish()]);
        }
        self.used.fetch_add(length * self.unit, Ordering::AcqRel);
        Ok(range)
    }

    /// A buffer holding `length` more units at least: twice as large, the one before copied into
    /// it.
    fn grow(&self, holes: &mut Holes, length: u64) -> Result<(), String> {
        let capacity = (holes.capacity() * 2).max(holes.capacity() + length).max(self.least);
        let size = capacity * self.unit;
        if size > self.device.limits().max_buffer_size {
            return Err(format!(
                "the {} would need {} MB, more than a buffer of the GPU holds",
                self.label,
                size >> 20
            ));
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size,
            usage: self.usage,
            mapped_at_creation: false,
        });
        let before = self.buffer();
        if let Some((old, _)) = &before {
            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(self.label),
            });
            encoder.copy_buffer_to_buffer(old, 0, &buffer, 0, old.size());
            self.queue.submit([encoder.finish()]);
        }
        let generation = before.map_or(0, |(_, generation)| generation) + 1;
        *lock(&self.buffer) = Some((Arc::new(buffer), generation));
        holes.grow(capacity);
        self.held.store(size, Ordering::Release);
        Ok(())
    }

    /// `range` given back.
    pub fn give(&self, range: Range<u64>) {
        let length = range.end - range.start;
        lock(&self.holes).give(range);
        self.used.fetch_sub(length * self.unit, Ordering::AcqRel);
    }

    /// The buffer now and its generation; none before the first range.
    pub fn buffer(&self) -> Option<(Arc<wgpu::Buffer>, u64)> {
        lock(&self.buffer).clone()
    }

    /// The bytes of its buffer, and of the units it holds.
    pub fn bytes(&self) -> (u64, u64) {
        (self.held.load(Ordering::Acquire), self.used.load(Ordering::Acquire))
    }
}
