//! The statistics of the view: the time between frames; what the interface thread spent preparing
//! the layers, recording their bundles and submitting; what the GPU spent on a frame, timed by its
//! timestamps when the device has them, and on each layer's computing and drawing where it can time
//! them inside encoders and passes; what the process takes in memory; and what each layer drew.
//! Averaged over the last second, with the longest, as the view shows them over itself.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use uniwow_api::viewport::{self, Allowance, LayerStats};
use uniwow_api::wgpu;

/// What the samples cover.
const WINDOW: Duration = Duration::from_secs(1);

/// What a layer cost the interface thread in a frame, and what it drew.
#[derive(Clone, Debug, Default)]
pub struct LayerTiming {
    pub owner: String,
    /// Its preparing, and its recording of what it computes.
    pub prepare: Duration,
    /// Its recording: of its bundle, none when it was kept, or of its drawing in the pass.
    pub record: Option<Duration>,
    pub stats: LayerStats,
}

impl LayerTiming {
    /// All it cost the interface thread: its steering, its preparing and its recording.
    fn interface(&self) -> Duration {
        self.stats.steering + self.prepare + self.record.unwrap_or_default()
    }
}

/// What the GPU spent on a frame, in milliseconds: on it all, and on each layer timed, its
/// computing and its drawing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuFrame {
    pub total: f64,
    pub layers: Vec<(String, f64, f64)>,
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
    gpu: VecDeque<(Instant, GpuFrame)>,
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

    /// What the GPU spent on a frame, known some frames after it.
    pub fn push_gpu(&mut self, now: Instant, frame: GpuFrame) {
        self.gpu.push_back((now, frame));
        while self.gpu.front().is_some_and(|(at, _)| now.duration_since(*at) > WINDOW) {
            self.gpu.pop_front();
        }
    }

    /// The statistics as the view shows them, a line each: the frames, the interface thread, the
    /// GPU, the memory of the process, then each layer; `timed` says whether the GPU is timed,
    /// `memory` what the process takes, its working set and its private bytes.
    pub fn text(&self, timed: bool, memory: Option<(u64, u64)>, budget: &Allowance) -> String {
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
            let (gpu, longest) = spread(self.gpu.iter().map(|(_, gpu)| gpu.total));
            format!("GPU: {gpu:.2} ms a frame, the longest {longest:.2}")
        } else {
            "GPU: not timed, the device has no timestamps".to_owned()
        });
        let mb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
        if let Some((working, private)) = memory {
            lines.push(format!(
                "process: {:.0} MB in memory, {:.0} MB private",
                mb(working),
                mb(private)
            ));
        }
        if budget.budget != u64::MAX {
            let limited = budget.limited.map_or(String::new(), |reach| {
                format!("; reach limited to {:.1} tiles", reach / (viewport::BAND * 4.0))
            });
            lines.push(format!(
                "GPU budget of the view: {:.0} of {:.0} MB{limited}",
                mb(budget.used),
                mb(budget.budget)
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
            let timed = || {
                self.gpu.iter().filter_map(|(_, frame)| {
                    frame
                        .layers
                        .iter()
                        .find(|(owner, _, _)| *owner == layer.owner)
                        .map(|(_, compute, draw)| (*compute, *draw))
                })
            };
            if timed().next().is_some() {
                let (compute, longest_compute) = spread(timed().map(|(compute, _)| compute));
                let (draw, longest_draw) = spread(timed().map(|(_, draw)| draw));
                lines.push(format!(
                    "  GPU: computing {compute:.2} ms ({longest_compute:.2}), drawing {draw:.2} ({longest_draw:.2})"
                ));
            }
        }
        lines.join("\n")
    }
}

/// A buffer the timestamps of a frame are copied to, where its reading stands, and the layers they
/// time.
struct Readback {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    owners: Vec<String>,
}

const FREE: u8 = 0;
const IN_FLIGHT: u8 = 1;
const MAPPED: u8 = 2;

/// The layers whose computing and drawing a frame times apart, at most.
pub const TIMED_LAYERS: usize = 16;
/// The timestamps of a frame: its pass, then four for each layer timed (its computing begun and
/// ended, its drawing begun and ended).
const QUERIES: u32 = 2 + 4 * TIMED_LAYERS as u32;

/// Times the frames of the view on the GPU: the pass by its timestamps at its start and its end,
/// and, where the device writes timestamps inside encoders and passes, each layer's computing and
/// drawing; resolved and copied to a buffer read back some frames later, without waiting. A frame
/// finding no buffer free is not timed.
pub struct GpuTimer {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readbacks: Vec<Readback>,
    /// Nanoseconds a tick.
    period: f64,
    /// The buffer of this frame.
    current: Option<usize>,
    /// Whether the layers are timed apart.
    inside: bool,
    /// The layers timed in this frame, in their order.
    owners: Vec<String>,
}

impl GpuTimer {
    /// None when the device has no timestamps.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        let features = device.features();
        if !features.contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("viewport timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: QUERIES,
        });
        let buffer = |usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("viewport timestamps"),
                size: u64::from(QUERIES) * 8,
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
                    owners: Vec::new(),
                })
                .collect(),
            period: f64::from(queue.get_timestamp_period()),
            current: None,
            inside: features.contains(
                wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES,
            ),
            owners: Vec::new(),
        })
    }

    /// Begins the frame: whether a buffer is free to read it.
    pub fn begin(&mut self) {
        self.owners.clear();
        self.current = self
            .readbacks
            .iter()
            .position(|readback| readback.state.load(Ordering::Acquire) == FREE);
    }

    /// Where the pass of this frame writes its timestamps, when the frame is timed.
    pub fn writes(&self) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.current?;
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.set,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        })
    }

    /// The number of the layer `owner` among those of this frame timed apart; none when the frame
    /// or its layers are not timed, or too many are.
    pub fn layer(&mut self, owner: &str) -> Option<u32> {
        if self.current.is_none() || !self.inside || self.owners.len() >= TIMED_LAYERS {
            return None;
        }
        self.owners.push(owner.to_owned());
        Some(self.owners.len() as u32 - 1)
    }

    /// Writes the beginning or the end of the computing of the layer `layer` into `encoder`.
    pub fn computing(&self, encoder: &mut wgpu::CommandEncoder, layer: u32, end: bool) {
        encoder.write_timestamp(&self.set, 2 + 4 * layer + u32::from(end));
    }

    /// Writes the beginning or the end of the drawing of the layer `layer` into the pass.
    pub fn drawing(&self, pass: &mut wgpu::RenderPass<'_>, layer: u32, end: bool) {
        pass.write_timestamp(&self.set, 2 + 4 * layer + 2 + u32::from(end));
    }

    /// After the pass: the timestamps written copied to the buffer of this frame.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(current) = self.current {
            let written = 2 + 4 * self.owners.len() as u32;
            encoder.resolve_query_set(&self.set, 0..written, &self.resolve, 0);
            encoder.copy_buffer_to_buffer(
                &self.resolve,
                0,
                &self.readbacks[current].buffer,
                0,
                u64::from(written) * 8,
            );
        }
    }

    /// After the submission: the buffer of this frame asked to be read.
    pub fn submitted(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        let readback = &mut self.readbacks[current];
        readback.owners = std::mem::take(&mut self.owners);
        let state = readback.state.clone();
        state.store(IN_FLIGHT, Ordering::Release);
        readback.buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            state.store(if result.is_ok() { MAPPED } else { FREE }, Ordering::Release);
        });
    }

    /// The frames whose timestamps have come back, without waiting.
    pub fn collect(&mut self, device: &wgpu::Device) -> Vec<GpuFrame> {
        let _ = device.poll(wgpu::PollType::Poll);
        let mut frames = Vec::new();
        for readback in &mut self.readbacks {
            if readback.state.load(Ordering::Acquire) != MAPPED {
                continue;
            }
            let ticks: Option<Vec<u64>> = readback.buffer.slice(..).get_mapped_range().ok().map(|data| {
                data.as_chunks::<8>()
                    .0
                    .iter()
                    .take(2 + 4 * readback.owners.len())
                    .map(|bytes| u64::from_le_bytes(*bytes))
                    .collect()
            });
            readback.buffer.unmap();
            readback.state.store(FREE, Ordering::Release);
            let Some(ticks) = ticks else {
                continue;
            };
            let span = |start: usize| ticks[start + 1].saturating_sub(ticks[start]) as f64 * self.period / 1e6;
            frames.push(GpuFrame {
                total: span(0),
                layers: readback
                    .owners
                    .iter()
                    .enumerate()
                    .map(|(layer, owner)| (owner.clone(), span(2 + 4 * layer), span(4 + 4 * layer)))
                    .collect(),
            });
        }
        frames
    }
}
