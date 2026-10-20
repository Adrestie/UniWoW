//! The groups of the buildings tested against the depth on the GPU (step 9.6f), as the instances of
//! the models are: the opaque batches of the groups the CPU lists that were seen at the frame before
//! drawn in the first pass; every group listed tested between the passes against the pyramid of the
//! depth the first pass left, those found in sight and not drawn yet drawn in the second pass
//! (`Phase::Revealed`), the blended batches of those found in sight only; those found in sight kept
//! for the next frame. Each group of the buildings drawn has a place in two tables of those seen,
//! one read and one written at each frame in turn; all of them seen once the buildings change, so
//! that the frame after a change draws them all first, as without the depth.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use uniwow_api::viewport::Pyramid;
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

/// The threads of a workgroup, and the most workgroups a side of a dispatch takes.
const THREADS: u32 = 64;
const ROW: u32 = 65_535;

/// What every layer of the buildings shares to test its groups: the layouts and the pipelines of
/// the three dispatches.
pub struct Testing {
    layout: wgpu::BindGroupLayout,
    pyramid_layout: wgpu::BindGroupLayout,
    first: wgpu::ComputePipeline,
    test: wgpu::ComputePipeline,
    second: wgpu::ComputePipeline,
}

impl Testing {
    /// The pipelines on the device of `gpu`; none when it runs no compute shaders or binds too few
    /// storage buffers at once, the buildings then drawn without the depth.
    pub fn new(gpu: &egui_wgpu::RenderState) -> Option<Self> {
        let device = &gpu.device;
        let downlevel = gpu.adapter.get_downlevel_capabilities().flags;
        if !downlevel.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
            || device.limits().max_storage_buffers_per_shader_stage < 7
        {
            return None;
        }
        let buffer = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let read = wgpu::BufferBindingType::Storage { read_only: true };
        let write = wgpu::BufferBindingType::Storage { read_only: false };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("buildings occlusion"),
            entries: &[
                buffer(0, wgpu::BufferBindingType::Uniform),
                buffer(1, read),
                buffer(2, read),
                buffer(3, read),
                buffer(4, read),
                buffer(5, write),
                buffer(6, write),
                buffer(7, write),
            ],
        });
        let pyramid_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("buildings occlusion pyramid"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("buildings occlusion"),
            source: wgpu::ShaderSource::Wgsl(include_str!("occlusion.wgsl").into()),
        });
        let pipeline = |entry: &str, layouts: &[Option<&wgpu::BindGroupLayout>]| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Some(Self {
            first: pipeline("first", &[Some(&layout)]),
            test: pipeline("test", &[Some(&layout), Some(&pyramid_layout)]),
            second: pipeline("second", &[Some(&layout)]),
            layout,
            pyramid_layout,
        })
    }
}

/// The dispatch of `count` threads, a workgroup of `THREADS` over rows of at most `ROW`.
fn dispatch(pass: &mut wgpu::ComputePass<'_>, count: u32) {
    let workgroups = count.div_ceil(THREADS);
    if workgroups > 0 {
        pass.dispatch_workgroups(workgroups.min(ROW), workgroups.div_ceil(ROW), 1);
    }
}

/// A buffer made again larger, `usage` its uses, once it no longer holds `size` bytes; whether it
/// was.
fn sized(
    device: &wgpu::Device,
    buffer: &mut Option<wgpu::Buffer>,
    label: &str,
    size: u64,
    usage: wgpu::BufferUsages,
) -> bool {
    let size = size.max(64);
    if buffer.as_ref().is_some_and(|buffer| buffer.size() >= size) {
        return false;
    }
    *buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.next_power_of_two(),
        usage,
        mapped_at_creation: false,
    }));
    true
}

/// Whether a readback of the hidden groups is free, copied, being mapped or mapped.
const FREE: u8 = 0;
const COPIED: u8 = 1;
const IN_FLIGHT: u8 = 2;
const MAPPED: u8 = 3;

/// A buffer the count of the hidden groups is copied to and read back from.
struct Readback {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
}

/// What a layer of the buildings keeps to test its groups: the tables of those seen, the draws of
/// the two phases, the groups and the counts of the frame, and the bind groups of each turn.
pub struct Occlusion {
    testing: Arc<Testing>,
    device: wgpu::Device,
    params: wgpu::Buffer,
    groups: Option<wgpu::Buffer>,
    seen: [Option<wgpu::Buffer>; 2],
    pub first: Option<wgpu::Buffer>,
    pub second: Option<wgpu::Buffer>,
    hidden: wgpu::Buffer,
    readbacks: Vec<Readback>,
    /// The table read at this frame, the other written.
    turn: usize,
    /// The bind groups of each turn, of the first phase then of the second, made again once a buffer
    /// is, with the buffers of the commands and of the entries they were made with; that of the
    /// pyramid, by its generation.
    bound: Option<([[wgpu::BindGroup; 2]; 2], [wgpu::Buffer; 2])>,
    pyramid: Option<(u64, wgpu::BindGroup)>,
    /// The opaque draws, every draw and the groups listed at this frame.
    counts: [u32; 3],
    /// Whether the draws of each phase were written at this frame.
    pub computed: bool,
    pub occluded: bool,
    /// The groups the depth hid at the last frame read back.
    pub hid: u32,
}

impl Occlusion {
    pub fn new(testing: Arc<Testing>, device: &wgpu::Device) -> Self {
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("buildings occlusion"),
            size: 80,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let hidden = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("buildings hidden"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let readbacks = (0..3)
            .map(|_| Readback {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("buildings hidden read back"),
                    size: 16,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(FREE)),
            })
            .collect();
        Self {
            testing,
            device: device.clone(),
            params,
            groups: None,
            seen: [None, None],
            first: None,
            second: None,
            hidden,
            readbacks,
            turn: 0,
            bound: None,
            pyramid: None,
            counts: [0; 3],
            computed: false,
            occluded: false,
            hid: 0,
        }
    }

    /// The frame of `view_proj`: the groups listed `groups` (two vectors each, the place of each in
    /// the tables of those seen in the bits of the first's last), `opaque` draws of `all`, the
    /// commands and the entries of the frame. The tables of those seen sized for `slots` groups and
    /// all set seen when `changed` says the buildings changed. The tables then turn.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        view_proj: [f32; 16],
        groups: &[[f32; 4]],
        opaque: u32,
        all: u32,
        slots: u32,
        changed: bool,
        buffers: [&wgpu::Buffer; 2],
    ) {
        self.read_back();
        self.computed = false;
        self.occluded = false;
        self.turn ^= 1;
        let device = self.device.clone();
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let indirect = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT;
        let mut made = sized(
            &device,
            &mut self.groups,
            "buildings groups",
            size_of_val(groups) as u64,
            storage,
        );
        let tables = u64::from(slots) * 4;
        let grown = self
            .seen
            .iter()
            .any(|table| table.as_ref().is_none_or(|table| table.size() < tables.max(64)));
        if grown || changed {
            for table in &mut self.seen {
                *table = None;
                sized(&device, table, "buildings seen", tables, storage);
            }
            // Every group seen: the next frame draws them all in its first pass.
            for table in self.seen.iter().flatten() {
                queue.write_buffer(table, 0, bytemuck::cast_slice(&vec![1u32; (table.size() / 4) as usize]));
            }
            made = true;
        }
        let draws = u64::from(all) * 20;
        made |= sized(&device, &mut self.first, "buildings first draws", draws, indirect);
        made |= sized(&device, &mut self.second, "buildings second draws", draws, indirect);
        let same = self
            .bound
            .as_ref()
            .is_some_and(|(_, with)| with[0] == *buffers[0] && with[1] == *buffers[1]);
        if made || !same {
            self.bound = self.bind_groups(buffers);
        }
        if let Some(buffer) = &self.groups
            && !groups.is_empty()
        {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(groups));
        }
        let mut params = [0u32; 20];
        params[..16].copy_from_slice(bytemuck::cast_slice(&view_proj));
        params[16..19].copy_from_slice(&[opaque, all, groups.len() as u32 / 2]);
        queue.write_buffer(&self.params, 0, bytemuck::cast_slice(&params));
        self.counts = [opaque, all, groups.len() as u32 / 2];
    }

    /// The bind groups of each turn, the commands and the entries `buffers` with the tables and the
    /// draws: the first writing the draws of the first phase, the second those of the second.
    #[allow(clippy::type_complexity)]
    fn bind_groups(&self, buffers: [&wgpu::Buffer; 2]) -> Option<([[wgpu::BindGroup; 2]; 2], [wgpu::Buffer; 2])> {
        let (Some(groups), [Some(one), Some(two)], Some(first), Some(second)) =
            (&self.groups, &self.seen, &self.first, &self.second)
        else {
            return None;
        };
        let group = |before: &wgpu::Buffer, now: &wgpu::Buffer, drawn: &wgpu::Buffer| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("buildings occlusion"),
                layout: &self.testing.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buffers[0].as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: buffers[1].as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: groups.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: before.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: now.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: drawn.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: self.hidden.as_entire_binding(),
                    },
                ],
            })
        };
        // At each turn, the table read is that of the turn, the other written; the first phase and
        // the second each with their draws.
        let turns = [(one, two), (two, one)];
        let groups = turns.map(|(before, now)| [first, second].map(|drawn| group(before, now, drawn)));
        Some((groups, [buffers[0].clone(), buffers[1].clone()]))
    }

    /// The draws of the first phase, before the first pass.
    pub fn compute(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let Some((groups, _)) = &self.bound else {
            return;
        };
        let first = &groups[self.turn][0];
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("buildings first"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.testing.first);
        pass.set_bind_group(0, first, &[]);
        dispatch(&mut pass, self.counts[0]);
        drop(pass);
        self.computed = true;
    }

    /// The groups listed tested against `pyramid`, then the draws of the second phase; the groups
    /// hidden copied to be read back.
    pub fn occlude(&mut self, encoder: &mut wgpu::CommandEncoder, pyramid: &Pyramid<'_>) {
        let Some(second) = self.bound.as_ref().map(|(groups, _)| groups[self.turn][1].clone()) else {
            return;
        };
        let [Some(one), Some(two)] = &self.seen else {
            return;
        };
        if self
            .pyramid
            .as_ref()
            .is_none_or(|(generation, _)| *generation != pyramid.generation)
        {
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("buildings occlusion pyramid"),
                layout: &self.testing.pyramid_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(pyramid.view),
                }],
            });
            self.pyramid = Some((pyramid.generation, group));
        }
        let Some((_, pyramid_group)) = &self.pyramid else {
            return;
        };
        let now = if self.turn == 0 { two } else { one };
        encoder.clear_buffer(now, 0, None);
        encoder.clear_buffer(&self.hidden, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("buildings test"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.testing.test);
            pass.set_bind_group(0, &second, &[]);
            pass.set_bind_group(1, pyramid_group, &[]);
            dispatch(&mut pass, self.counts[2]);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("buildings second"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.testing.second);
            pass.set_bind_group(0, &second, &[]);
            dispatch(&mut pass, self.counts[1]);
        }
        if let Some(readback) = self
            .readbacks
            .iter()
            .find(|readback| readback.state.load(Ordering::Acquire) == FREE)
        {
            encoder.copy_buffer_to_buffer(&self.hidden, 0, &readback.buffer, 0, 16);
            readback.state.store(COPIED, Ordering::Release);
        }
        self.occluded = true;
    }

    /// The groups hidden copied by the frames before, read back without waiting.
    fn read_back(&mut self) {
        let _ = self.device.poll(wgpu::PollType::Poll);
        for readback in &self.readbacks {
            match readback.state.load(Ordering::Acquire) {
                COPIED => {
                    let state = readback.state.clone();
                    state.store(IN_FLIGHT, Ordering::Release);
                    readback.buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                        state.store(if result.is_ok() { MAPPED } else { FREE }, Ordering::Release);
                    });
                }
                MAPPED => {
                    if let Ok(data) = readback.buffer.slice(..).get_mapped_range() {
                        self.hid = bytemuck::cast_slice::<u8, u32>(&data)[0];
                    }
                    readback.buffer.unmap();
                    readback.state.store(FREE, Ordering::Release);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod wgsl_tests {

    #[test]
    fn its_shaders_parse_and_validate() {
        uniwow_api::viewport::check_wgsl(include_str!("occlusion.wgsl"))
            .unwrap_or_else(|error| panic!("occlusion: {error}"));
    }
}
