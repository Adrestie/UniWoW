//! An arena of a buffer of the GPU shared by what a module draws, in units of a fixed size, as the
//! models and the buildings keep their vertices, indices and materials: a range is given
//! from its holes, the first that holds it, and given back to them, merged with its neighbours. A
//! buffer too small for a range is replaced by one twice as large, the one before copied into it,
//! so that every range keeps its place. Its jobs write a range by a copy they submit, under the
//! lock of its holes, after any copy into a larger buffer (the rule of step 9.2f); the frame reads
//! the buffer and the bytes held without that lock.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::wgpu;
use crate::wgpu::util::DeviceExt;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{Arena, Holes, lock};
    use crate::{bytemuck, wgpu};

    /// A device of the software adapter of the system, when it has one.
    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster(instance.request_adapter(&wgpu::RequestAdapterOptions {
            force_fallback_adapter: true,
            ..Default::default()
        }))
        .ok()?;
        pollster(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    /// `future` run to its end on this thread: those of wgpu here are ready at once.
    fn pollster<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        loop {
            if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
            std::thread::yield_now();
        }
    }

    /// The first `size` bytes of `buffer`, copied back from the GPU.
    fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, buffer: &wgpu::Buffer, size: u64) -> Vec<u8> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
        queue.submit([encoder.finish()]);
        staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        staging.slice(..).get_mapped_range().expect("mapped").to_vec()
    }

    #[test]
    fn a_range_is_taken_from_the_first_hole_holding_it_and_given_back_merged() {
        let mut holes = Holes::default();
        assert_eq!(holes.take(1), None, "nothing before the arena grows");
        holes.grow(10);
        assert_eq!(holes.take(4), Some(0..4));
        assert_eq!(holes.take(3), Some(4..7));
        assert_eq!(holes.take(5), None, "3 left");
        holes.give(0..4);
        assert_eq!(holes.take(5), None, "two holes, of 4 and 3, apart");
        holes.give(4..7);
        assert_eq!(holes.take(10), Some(0..10), "merged with both its neighbours");
        holes.give(0..10);
        holes.grow(16);
        assert_eq!(
            holes.take(16),
            Some(0..16),
            "the units added joined to the hole before them"
        );
    }

    #[test]
    fn an_arena_keeps_its_ranges_in_place_when_it_grows_and_takes_back_those_given() {
        let Some((device, queue)) = device() else {
            return;
        };
        let arena = Arena::new(&device, &queue, "test arena", wgpu::BufferUsages::STORAGE, 4, 4);
        let first = arena.put(bytemuck::cast_slice(&[1u32, 2, 3, 4])).unwrap();
        let second = arena.put(bytemuck::cast_slice(&[5u32, 6])).unwrap();
        assert_eq!((first.clone(), second), (0..4, 4..6));
        let (buffer, generation) = arena.buffer().unwrap();
        assert_eq!(generation, 2, "grown once");
        let read: Vec<u32> = bytemuck::cast_slice(&read_back(&device, &queue, &buffer, 24)).to_vec();
        assert_eq!(
            read,
            [1, 2, 3, 4, 5, 6],
            "the first range copied into the larger buffer"
        );
        assert_eq!(arena.bytes(), (32, 24));
        arena.give(first);
        assert_eq!(
            arena.put(bytemuck::cast_slice(&[7u32])).unwrap(),
            0..1,
            "a range given back taken again"
        );
    }

    #[test]
    fn the_frame_reads_an_arena_while_a_job_holds_its_holes() {
        let Some((device, queue)) = device() else {
            return;
        };
        let arena = Arena::new(&device, &queue, "test arena", wgpu::BufferUsages::STORAGE, 4, 4);
        arena.put(bytemuck::cast_slice(&[1u32, 2, 3, 4])).unwrap();
        let (held, holding) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (read, reading) = mpsc::channel();
        std::thread::scope(|scope| {
            // A job taking a range or growing the buffer, which can take milliseconds.
            let arena = &arena;
            scope.spawn(move || {
                let _holes = lock(&arena.holes);
                held.send(()).unwrap();
                let _ = released.recv();
            });
            holding.recv().unwrap();
            scope.spawn(move || read.send((arena.bytes(), arena.buffer().map(|(_, generation)| generation))));
            let seen = reading.recv_timeout(Duration::from_secs(5)).ok();
            release.send(()).unwrap();
            assert_eq!(seen, Some(((16, 16), Some(1))), "read without waiting for the job");
        });
    }
}
