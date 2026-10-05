//! The statistics of the view: the time between frames; what the interface thread spent preparing
//! the layers, recording their bundles and submitting; what the GPU spent drawing, timed by its
//! timestamps when the device has them; what the process takes in memory; and what each layer drew.
//! Averaged over the last second, with the longest, as the view shows them over itself.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use uniwow_api::viewport::LayerStats;
use uniwow_api::wgpu;

/// What the samples cover.
const WINDOW: Duration = Duration::from_secs(1);

/// What a layer cost the interface thread in a frame, and what it drew.
#[derive(Clone, Debug, Default)]
pub struct LayerTiming {
    pub owner: String,
    pub prepare: Duration,
    /// None when its bundle was kept.
    pub record: Option<Duration>,
    pub stats: LayerStats,
}

impl LayerTiming {
    /// All it cost the interface thread: its steering, its preparing and its recording.
    fn interface(&self) -> Duration {
        self.stats.steering + self.prepare + self.record.unwrap_or_default()
    }
}

/// A frame of the view.
#[derive(Clone, Debug, Default)]
pub struct Sample {
    /// Since the frame before; none after a pause of the view.
    pub interval: Option<Duration>,
    pub prepare: Duration,
    pub record: Duration,
    pub submit: Duration,
    pub layers: Vec<LayerTiming>,
}

#[derive(Default)]
pub struct Stats {
    samples: VecDeque<(Instant, Sample)>,
    gpu: VecDeque<(Instant, f64)>,
}

/// What the process takes in memory, in bytes: its working set and its private bytes; none where
/// the system does not tell it.
pub fn process_memory() -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        use uniwow_api::windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        };
        use uniwow_api::windows::Win32::System::Threading::GetCurrentProcess;
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: the pseudo handle of this process, and counters of the size given.
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        }
        .ok()?;
        Some((counters.WorkingSetSize as u64, counters.PrivateUsage as u64))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The mean and the longest of `values`, in milliseconds.
fn spread(values: impl Iterator<Item = f64>) -> (f64, f64) {
    let (mut sum, mut longest, mut count) = (0.0, 0.0f64, 0usize);
    for value in values {
        sum += value;
        longest = longest.max(value);
        count += 1;
    }
    (sum / count.max(1) as f64, longest)
}

impl Stats {
    pub fn push(&mut self, now: Instant, sample: Sample) {
        self.samples.push_back((now, sample));
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// The time the GPU spent on a frame, in milliseconds, known some frames after it.
    pub fn push_gpu(&mut self, now: Instant, milliseconds: f64) {
        self.gpu.push_back((now, milliseconds));
        while self.gpu.front().is_some_and(|(at, _)| now.duration_since(*at) > WINDOW) {
            self.gpu.pop_front();
        }
    }

    /// The statistics as the view shows them, a line each: the frames, the interface thread, the
    /// GPU, the memory of the process, then each layer; `timed` says whether the GPU is timed,
    /// `memory` what the process takes, its working set and its private bytes.
    pub fn text(&self, timed: bool, memory: Option<(u64, u64)>) -> String {
        let samples = || self.samples.iter().map(|(_, sample)| sample);
        let (interval, longest) = spread(samples().filter_map(|s| s.interval.map(ms)));
        let mut lines = vec![format!(
            "{:.0} fps: a frame {interval:.1} ms, the longest {longest:.1}",
            if interval > 0.0 { 1000.0 / interval } else { 0.0 }
        )];
        let (prepare, _) = spread(samples().map(|s| ms(s.prepare)));
        let (record, _) = spread(samples().map(|s| ms(s.record)));
        let (submit, _) = spread(samples().map(|s| ms(s.submit)));
        let (all, longest) = spread(samples().map(|s| ms(s.prepare + s.record + s.submit)));
        lines.push(format!(
            "view, interface thread: {all:.2} ms (prepare {prepare:.2}, record {record:.2}, submit {submit:.2}), the longest {longest:.2}"
        ));
        lines.push(if timed {
            let (gpu, longest) = spread(self.gpu.iter().map(|(_, gpu)| *gpu));
            format!("GPU: {gpu:.2} ms a frame, the longest {longest:.2}")
        } else {
            "GPU: not timed, the device has no timestamps".to_owned()
        });
        if let Some((working, private)) = memory {
            let mb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
            lines.push(format!(
                "process: {:.0} MB in memory, {:.0} MB private",
                mb(working),
                mb(private)
            ));
        }
        let Some((_, last)) = self.samples.back() else {
            return lines.join("\n");
        };
        for layer in &last.layers {
            let of_layer = || samples().filter_map(|sample| sample.layers.iter().find(|l| l.owner == layer.owner));
            let (interface, longest) = spread(of_layer().map(|l| ms(l.interface())));
            let (steering, longest_steering) = spread(of_layer().map(|l| ms(l.stats.steering)));
            let (prepare, longest_prepare) = spread(of_layer().map(|l| ms(l.prepare)));
            let (record, longest_record) = spread(of_layer().map(|l| ms(l.record.unwrap_or_default())));
            let stats = &layer.stats;
            let items = if stats.items.is_empty() {
                String::new()
            } else {
                format!(", {}", stats.items)
            };
            lines.push(format!(
                "{}: {} draws, {:.2} M triangles, {:.0} MB{items}",
                layer.owner,
                stats.draws,
                stats.triangles as f64 / 1e6,
                stats.bytes as f64 / (1024.0 * 1024.0),
            ));
            lines.push(format!(
                "  interface {interface:.2} ms, the longest {longest:.2}: steering {steering:.2} ({longest_steering:.2}), prepare {prepare:.2} ({longest_prepare:.2}), record {record:.2} ({longest_record:.2})"
            ));
        }
        lines.join("\n")
    }
}

/// A buffer the timestamps of a frame are copied to, and where its reading stands.
struct Readback {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
}

const FREE: u8 = 0;
const IN_FLIGHT: u8 = 1;
const MAPPED: u8 = 2;

/// Times the pass of the view on the GPU: its timestamps at its start and its end, resolved and
/// copied to a buffer read back some frames later, without waiting; a frame finding no buffer free
/// is not timed.
pub struct GpuTimer {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readbacks: Vec<Readback>,
    /// Nanoseconds a tick.
    period: f64,
    /// The buffer of this frame.
    current: Option<usize>,
}

impl GpuTimer {
    /// None when the device has no timestamps.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("viewport timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let buffer = |usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("viewport timestamps"),
                size: 16,
                usage,
                mapped_at_creation: false,
            })
        };
        Some(Self {
            set,
            resolve: buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC),
            readbacks: (0..4)
                .map(|_| Readback {
                    buffer: buffer(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST),
                    state: Arc::new(AtomicU8::new(FREE)),
                })
                .collect(),
            period: f64::from(queue.get_timestamp_period()),
            current: None,
        })
    }

    /// Where the pass of this frame writes its timestamps, when a buffer is free to read them.
    pub fn writes(&mut self) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.current = self
            .readbacks
            .iter()
            .position(|readback| readback.state.load(Ordering::Acquire) == FREE);
        self.current?;
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.set,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        })
    }

    /// After the pass: its timestamps copied to the buffer of this frame.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(current) = self.current {
            encoder.resolve_query_set(&self.set, 0..2, &self.resolve, 0);
            encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readbacks[current].buffer, 0, 16);
        }
    }

    /// After the submission: the buffer of this frame asked to be read.
    pub fn submitted(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        let state = self.readbacks[current].state.clone();
        state.store(IN_FLIGHT, Ordering::Release);
        self.readbacks[current]
            .buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                state.store(if result.is_ok() { MAPPED } else { FREE }, Ordering::Release);
            });
    }

    /// The times of the frames whose timestamps have come back, in milliseconds, without waiting.
    pub fn collect(&mut self, device: &wgpu::Device) -> Vec<f64> {
        let _ = device.poll(wgpu::PollType::Poll);
        let mut times = Vec::new();
        for readback in &self.readbacks {
            if readback.state.load(Ordering::Acquire) != MAPPED {
                continue;
            }
            let ticks = readback.buffer.slice(..).get_mapped_range().ok().map(|data| {
                let mut ticks = [0u64; 2];
                for (tick, bytes) in ticks.iter_mut().zip(data.as_chunks::<8>().0) {
                    *tick = u64::from_le_bytes(*bytes);
                }
                ticks
            });
            readback.buffer.unmap();
            readback.state.store(FREE, Ordering::Release);
            if let Some([start, end]) = ticks {
                times.push(end.saturating_sub(start) as f64 * self.period / 1e6);
            }
        }
        times
    }
}
