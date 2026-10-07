//! An arena of a buffer of the GPU shared by what a module draws, in units of a fixed size, as the
//! models and the buildings keep their vertices, indices and materials: a range is given
//! from its holes, the first that holds it, and given back to them, merged with its neighbours. A
//! buffer too small for a range is replaced by one twice as large, or as large as the device takes
//! a buffer of its use, the one before copied into it, so that every range keeps its place; a range
//! that even that would not hold is refused for want of room, which a range given back can make.
//! Its jobs write a range by `Queue::write_buffer`, under the lock of its holes, without a buffer of
//! their own; the frame reads the buffer and the bytes held without that lock.

use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::wgpu;

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

    /// The units of the hole that ends the arena, which a growth joins to those it adds.
    pub fn tail(&self) -> u64 {
        self.holes
            .last()
            .filter(|hole| hole.end == self.capacity)
            .map_or(0, |hole| hole.end - hole.start)
    }
}

/// A range refused for want of room: the arena holding it would pass the largest buffer of its use
/// the device takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoRoom {
    pub label: &'static str,
    /// The bytes the arena would need, and the most it can have.
    pub needed: u64,
    pub most: u64,
}

impl fmt::Display for NoRoom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the {} would need {} MB, more than the {} MB a buffer of the GPU holds",
            self.label,
            self.needed >> 20,
            self.most >> 20
        )
    }
}

impl From<NoRoom> for String {
    fn from(refused: NoRoom) -> Self {
        refused.to_string()
    }
}

/// Why something was not put on the GPU: for want of room in an arena, which a range given back or
/// another place of the camera can make, or for good.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    NoRoom(NoRoom),
    Failed(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NoRoom(refused) => refused.fmt(f),
            Refusal::Failed(reason) => f.write_str(reason),
        }
    }
}

impl From<NoRoom> for Refusal {
    fn from(refused: NoRoom) -> Self {
        Refusal::NoRoom(refused)
    }
}

impl From<String> for Refusal {
    fn from(reason: String) -> Self {
        Refusal::Failed(reason)
    }
}

impl From<&str> for Refusal {
    fn from(reason: &str) -> Self {
        Refusal::Failed(reason.to_owned())
    }
}

/// How far from the eye what is wanted fits in an arena of `most` bytes filled to `share` of it,
/// the nearest first, each its distance and the bytes it takes there: the distance of the first
/// that does not, so that those nearer fit; infinity when all do.
pub fn reach(mut wanted: Vec<(f32, u64)>, most: u64, share: f64) -> f32 {
    wanted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let room = (most as f64 * share) as u64;
    let mut taken = 0u64;
    for (distance, bytes) in wanted {
        taken += bytes;
        if taken > room {
            return distance;
        }
    }
    f32::INFINITY
}

/// How far from the eye what is wanted fits in arenas of `most` bytes each filled to `share` of
/// them, the nearest first: each item its distance and the bytes it takes in each arena once on
/// the GPU, `expected` until then.
pub fn room<const N: usize>(wanted: &[(f32, Option<[u64; N]>)], most: [u64; N], expected: [u64; N], share: f64) -> f32 {
    (0..N)
        .map(|arena| {
            let items = wanted
                .iter()
                .map(|(away, taken)| (*away, taken.map_or(expected[arena], |bytes| bytes[arena])))
                .collect();
            reach(items, most[arena], share)
        })
        .fold(f32::INFINITY, f32::min)
}

/// How far the camera moves, in yards, before what was refused for want of room is tried again: a
/// chunk of terrain.
pub const MOVED: f32 = 1600.0 / 3.0 / 16.0;

/// Whether room may have been made for what was refused for want of room when its arenas had given
/// back `then` ranges and the camera stood at `at`: they gave more back since (`given` now), or the
/// camera moved to `eye` by more than `MOVED`.
pub fn room_made(then: u64, at: [f32; 2], given: u64, eye: [f32; 2]) -> bool {
    given != then || (eye[0] - at[0]).hypot(eye[1] - at[1]) > MOVED
}

pub struct Arena {
    device: wgpu::Device,
    queue: wgpu::Queue,
    label: &'static str,
    usage: wgpu::BufferUsages,
    /// The bytes of a unit, a multiple of 4.
    unit: u64,
    /// The fewest units of a buffer, and the most, by what the device takes of a buffer of its use.
    least: u64,
    most: u64,
    /// Held by a job while it takes a range and writes it, or grows the buffer.
    pub(crate) holes: Mutex<Holes>,
    /// The buffer now and its generation, counting the buffers: held only to read or replace them,
    /// so that the frame never waits for a job.
    buffer: Mutex<Option<(Arc<wgpu::Buffer>, u64)>>,
    /// The bytes of the buffer and of the units held, read without waiting either; and the ranges
    /// given back since it was made.
    held: AtomicU64,
    used: AtomicU64,
    given: AtomicU64,
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
        let limits = device.limits();
        let largest = if usage.contains(wgpu::BufferUsages::STORAGE) {
            limits.max_buffer_size.min(limits.max_storage_buffer_binding_size)
        } else {
            limits.max_buffer_size
        };
        Self {
            device: device.clone(),
            queue: queue.clone(),
            label,
            usage: usage | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            unit,
            least,
            most: largest / unit,
            holes: Mutex::default(),
            buffer: Mutex::default(),
            held: AtomicU64::new(0),
            used: AtomicU64::new(0),
            given: AtomicU64::new(0),
        }
    }

    /// `data` written to a range of the arena, its units; refused when the buffer it needs is
    /// larger than the device takes.
    pub fn put(&self, data: &[u8]) -> Result<Range<u64>, NoRoom> {
        debug_assert_eq!(data.len() as u64 % self.unit, 0);
        let length = data.len() as u64 / self.unit;
        let mut holes = lock(&self.holes);
        let range = match holes.take(length) {
            Some(range) => range,
            None => {
                self.grow(&mut holes, length)?;
                holes.take(length).expect("grown to hold it")
            }
        };
        if !data.is_empty() {
            let (buffer, _) = self.buffer().expect("grown before");
            // Under the lock, written by the next submission: a growth submitted after it runs the
            // writes waiting before its copy into the larger buffer.
            self.queue.write_buffer(&buffer, range.start * self.unit, data);
        }
        self.used.fetch_add(length * self.unit, Ordering::AcqRel);
        Ok(range)
    }

    /// A buffer holding `length` more units at least: twice as large, or as large as the device
    /// takes, the one before copied into it; refused when even that does not hold them.
    fn grow(&self, holes: &mut Holes, length: u64) -> Result<(), NoRoom> {
        let needed = holes.capacity() + length - holes.tail();
        if needed > self.most {
            return Err(NoRoom {
                label: self.label,
                needed: needed * self.unit,
                most: self.most * self.unit,
            });
        }
        let capacity = (holes.capacity() * 2).max(needed).max(self.least).min(self.most);
        let size = capacity * self.unit;
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

    /// `data` written again from the unit `at` of a range held, under the lock of the holes, so that
    /// no growth copies the buffer between.
    pub fn write(&self, at: u64, data: &[u8]) {
        let _holes = lock(&self.holes);
        if let Some((buffer, _)) = self.buffer()
            && !data.is_empty()
        {
            self.queue.write_buffer(&buffer, at * self.unit, data);
        }
    }

    /// `range` given back.
    pub fn give(&self, range: Range<u64>) {
        let length = range.end - range.start;
        lock(&self.holes).give(range);
        self.used.fetch_sub(length * self.unit, Ordering::AcqRel);
        self.given.fetch_add(1, Ordering::AcqRel);
    }

    /// How many ranges were given back since it was made: room may have been made since a range
    /// was refused when it counts more.
    pub fn given(&self) -> u64 {
        self.given.load(Ordering::Acquire)
    }

    /// The buffer now and its generation; none before the first range.
    pub fn buffer(&self) -> Option<(Arc<wgpu::Buffer>, u64)> {
        lock(&self.buffer).clone()
    }

    /// The most bytes its buffer can have.
    pub fn most(&self) -> u64 {
        self.most * self.unit
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

    use super::{Arena, Holes, MOVED, NoRoom, lock, reach, room, room_made};
    use crate::{bytemuck, wgpu};

    /// A device of the software adapter of the system, when it has one.
    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        device_with(wgpu::Limits::default())
    }

    /// A device of the software adapter of the system asked for `limits`, when it has one.
    fn device_with(limits: wgpu::Limits) -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster(instance.request_adapter(&wgpu::RequestAdapterOptions {
            force_fallback_adapter: true,
            ..Default::default()
        }))
        .ok()?;
        pollster(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: limits,
            ..Default::default()
        }))
        .ok()
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
    fn an_arena_keeps_its_ranges_in_place_when_it_grows_takes_back_those_given_and_writes_those_held() {
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
        arena.write(4, bytemuck::cast_slice(&[9u32]));
        let (buffer, _) = arena.buffer().unwrap();
        let read: Vec<u32> = bytemuck::cast_slice(&read_back(&device, &queue, &buffer, 24)).to_vec();
        assert_eq!(read, [7, 2, 3, 4, 9, 6], "a range held written again in place");
    }

    #[test]
    fn an_arena_grows_up_to_the_largest_buffer_the_device_takes_and_refuses_past_it() {
        let Some((device, queue)) = device_with(wgpu::Limits {
            max_buffer_size: 1024,
            ..Default::default()
        }) else {
            return;
        };
        let arena = Arena::new(&device, &queue, "test arena", wgpu::BufferUsages::VERTEX, 4, 4);
        assert_eq!(arena.most(), 1024);
        let first = arena.put(&[1; 160 * 4]).unwrap();
        assert_eq!(arena.bytes().0, 640);
        // Twice as large would pass the limit: grown to it, which holds the range.
        let second = arena.put(&[2; 96 * 4]).unwrap();
        assert_eq!((first, second.clone(), arena.bytes()), (0..160, 160..256, (1024, 1024)));
        assert_eq!(
            arena.put(&[3; 4]),
            Err(NoRoom {
                label: "test arena",
                needed: 1028,
                most: 1024
            }),
            "past the limit"
        );
        let given = arena.given();
        arena.give(second);
        assert_eq!(arena.given(), given + 1);
        assert_eq!(arena.put(&[3; 4]).unwrap(), 160..161, "a range given back makes room");
        assert_eq!(arena.bytes().0, 1024, "the buffer as it was");
        // Bound for storage, the smaller of the two limits.
        let Some((device, queue)) = device_with(wgpu::Limits {
            max_buffer_size: 1024,
            max_storage_buffer_binding_size: 512,
            ..Default::default()
        }) else {
            return;
        };
        let storage = Arena::new(&device, &queue, "test arena", wgpu::BufferUsages::STORAGE, 4, 4);
        assert_eq!(storage.most(), 512);
    }

    #[test]
    fn an_arena_growing_counts_the_hole_that_ends_it() {
        let Some((device, queue)) = device_with(wgpu::Limits {
            max_buffer_size: 1024,
            ..Default::default()
        }) else {
            return;
        };
        let arena = Arena::new(&device, &queue, "test arena", wgpu::BufferUsages::VERTEX, 4, 4);
        arena.put(&[1; 100 * 4]).unwrap();
        let second = arena.put(&[2; 20 * 4]).unwrap();
        assert_eq!(arena.bytes().0, 800, "grown to 200 units");
        // Given back, the hole that ends the arena holds 100 units: 150 more need 250, not 350.
        arena.give(second);
        assert_eq!(arena.put(&[3; 150 * 4]).unwrap(), 100..250);
        assert_eq!(arena.bytes().0, 1024);
    }

    #[test]
    fn what_is_wanted_fits_up_to_the_first_that_would_fill_the_arena_past_its_share() {
        let wanted = vec![(30.0, 40), (10.0, 40), (20.0, 15), (40.0, 1)];
        assert_eq!(reach(wanted.clone(), 100, 0.9), 30.0, "10, 20, then 30 past 90");
        assert_eq!(reach(wanted.clone(), 100, 1.0), f32::INFINITY, "all within 100");
        assert_eq!(reach(wanted, 50, 1.0), 20.0, "10, then 20 past 50");
        // In two arenas: one held, two expected at 15 and 4.
        let wanted = [(0.0, Some([10, 4])), (100.0, None), (200.0, None)];
        assert_eq!(room(&wanted, [30, 1_000], [15, 4], 1.0), 200.0);
        assert_eq!(room(&wanted, [30, 1_000], [15, 4], 0.5), 100.0, "half of it");
        assert_eq!(room(&wanted, [1_000, 6], [15, 4], 1.0), 100.0, "the second full first");
        assert_eq!(room(&wanted, [1_000, 1_000], [15, 4], 1.0), f32::INFINITY);
    }

    #[test]
    fn what_was_refused_for_want_of_room_is_tried_again_once_a_range_was_given_back_or_the_camera_moved() {
        assert!(!room_made(5, [0.0, 0.0], 5, [MOVED, 0.0]));
        assert!(room_made(5, [0.0, 0.0], 6, [0.0, 0.0]));
        assert!(room_made(5, [0.0, 0.0], 5, [MOVED + 1.0, 0.0]));
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
