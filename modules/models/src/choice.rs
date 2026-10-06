//! The instances drawn from the pool, chosen by the GPU at each frame (`choice.wgsl`): in sight,
//! within the reach of their size and at the level of skin their distance chooses, each instance
//! by itself; the opaque batches of every look gathered by state into one list of draws each,
//! packed in the order of their records. The blended batches are drawn by templates the CPU writes
//! each frame, an instance at a time the farthest first and, for each, one at each level of its
//! look: the GPU keeps those of the level it chose, in that order. The CPU still finds the groups
//! in sight, to give the GPU only those; the tables of the looks are made by a job of the module
//! when its looks change (`Tables`). What was drawn is read back some frames later.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::models::LookId;
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, wgpu};

use crate::gpu::State;
use crate::layer::{LIMITS, MARGIN, rank};
use crate::loading::Ready;
use crate::pool::{ENTRY, INSTANCE};
use crate::pooled::Record;

/// The levels of skin at most.
pub const LEVELS: usize = LIMITS.len() + 1;
/// The words of a look, a record, a group and a template in the tables (`choice.wgsl`).
const LOOK: usize = 2 + 2 * LEVELS;
const RECORD: usize = 5;
const GROUP: usize = 3;
const TEMPLATE: usize = 7;
/// The bytes of the arguments of a draw.
const ARGS: u64 = 20;
/// The words of the statistics: the draws, the pairs of an instance and a batch, the triangles,
/// unused, and the instances drawn at each level.
const STATS: usize = 8;

/// What the shader of the choice reads of a frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Params {
    planes: [[f32; 4]; 5],
    eye: [f32; 4],
    limits: [f32; 4],
    sizes: [u32; 4],
    regions: [u32; 4],
    statics: [u32; 4],
    frames: [u32; 4],
    work: [u32; 4],
    blocks: [u32; 4],
}

// SAFETY: plain numbers laid out by `repr(C)` without padding, any bit pattern valid.
unsafe impl bytemuck::Zeroable for Params {}
unsafe impl bytemuck::Pod for Params {}

/// The sides of the view of `view_proj` as planes, where a point is in sight when all are positive:
/// x >= -w, x <= w, y >= -w, y <= w, and w > 0.
pub fn planes(view_proj: Mat4) -> [[f32; 4]; 5] {
    let row = |at: usize| view_proj.row(at);
    [
        row(3) + row(0),
        row(3) - row(0),
        row(3) + row(1),
        row(3) - row(1),
        row(3),
    ]
    .map(|plane| plane.to_array())
}

/// A pooled look, as the CPU writes its templates.
pub struct TableLook {
    /// The most references of a level: the most entries an instance writes.
    pub most: u32,
    /// Its blended records at each level.
    pub blended: Vec<Vec<Record>>,
}

/// What the GPU chooses from, made again when the looks drawn change: the looks of the pool by
/// their slot, their opaque records gathered by state in the order drawn, and the regions of those
/// states.
pub struct Tables {
    pub generation: u64,
    pub slots: HashMap<LookId, u32>,
    pub looks: Vec<TableLook>,
    /// Each opaque state, in the order drawn, and where its records begin and how many.
    pub regions: Vec<(State, u32, u32)>,
    pub records: u32,
    buffer: wgpu::Buffer,
    /// Where the regions, the looks, the references and the records begin in its words.
    offsets: [u32; 4],
}

impl Tables {
    /// The tables of the pooled looks of `looks`.
    pub fn new(device: &wgpu::Device, generation: u64, looks: &HashMap<LookId, Arc<Ready>>) -> Self {
        let mut pooled: Vec<(LookId, &crate::pooled::PooledLook)> = looks
            .iter()
            .filter_map(|(id, ready)| match &**ready {
                Ready::Pooled(look) => Some((*id, look)),
                Ready::Own(_) => None,
            })
            .collect();
        pooled.sort_by_key(|(id, _)| *id);
        let mut all = Vec::new();
        for (slot, (_, look)) in pooled.iter().enumerate() {
            for (level, records) in look.skins.iter().take(LEVELS).enumerate() {
                for (order, record) in records.iter().enumerate() {
                    if !record.state.blended() {
                        all.push((rank(&record.state), slot, level, order, *record));
                    }
                }
            }
        }
        all.sort_by_key(|(rank, slot, level, order, _)| (*rank, *slot, *level, *order));
        let mut regions: Vec<(State, u32, u32)> = Vec::new();
        let mut record_words = Vec::with_capacity(all.len() * RECORD);
        let mut references = vec![vec![Vec::new(); LEVELS]; pooled.len()];
        for (index, (_, slot, level, _, record)) in all.iter().enumerate() {
            match regions.last_mut() {
                Some((state, _, count)) if *state == record.state => *count += 1,
                _ => regions.push((record.state, index as u32, 1)),
            }
            record_words.extend([
                record.count,
                record.first_index,
                record.base_vertex as u32,
                record.material,
                regions.len() as u32 - 1,
            ]);
            references[*slot][*level].push(index as u32);
        }
        let mut look_words = Vec::with_capacity(pooled.len() * LOOK);
        let mut reference_words = Vec::new();
        let mut table_looks = Vec::with_capacity(pooled.len());
        for (slot, (_, look)) in pooled.iter().enumerate() {
            let levels = look.skins.len().min(LEVELS);
            look_words.extend([look.model.model.radius.to_bits(), levels as u32]);
            for level in &references[slot] {
                look_words.extend([reference_words.len() as u32, level.len() as u32]);
                reference_words.extend(level);
            }
            table_looks.push(TableLook {
                most: references[slot]
                    .iter()
                    .map(|level| level.len() as u32)
                    .max()
                    .unwrap_or(0),
                blended: look
                    .skins
                    .iter()
                    .take(LEVELS)
                    .map(|records| {
                        records
                            .iter()
                            .filter(|record| record.state.blended())
                            .copied()
                            .collect()
                    })
                    .collect(),
            });
        }
        let region_words: Vec<u32> = regions.iter().flat_map(|(_, start, count)| [*start, *count]).collect();
        let offsets = [
            0,
            region_words.len() as u32,
            (region_words.len() + look_words.len()) as u32,
            (region_words.len() + look_words.len() + reference_words.len()) as u32,
        ];
        let mut words = [region_words, look_words, reference_words, record_words].concat();
        words.resize(words.len().max(4), 0);
        Self {
            generation,
            slots: pooled
                .iter()
                .enumerate()
                .map(|(slot, (id, _))| (*id, slot as u32))
                .collect(),
            looks: table_looks,
            records: all.len() as u32,
            regions,
            buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("models choice tables"),
                contents: bytemuck::cast_slice(&words),
                usage: wgpu::BufferUsages::STORAGE,
            }),
            offsets,
        }
    }
}

/// A group in sight, as the GPU reads it: its first instance in the buffer of the frame, its count,
/// the slot of its look.
#[derive(Clone, Copy, Debug)]
pub struct GroupOfFrame {
    pub first: u32,
    pub count: u32,
    pub slot: u32,
}

/// An instance of a look with blended batches, in the order its templates are drawn: its place in
/// the buffer of the frame and the slot of its look.
#[derive(Clone, Copy, Debug)]
pub struct Blended {
    pub index: u32,
    pub slot: u32,
}

/// An owner's instances in the buffer of the frame: its number, its layout, where they begin, how
/// many.
pub type Section = (u32, u64, u32, u32);

/// Where an owner's levels move, in bytes: from, to, how many.
type Move = (u64, u64, u64);

const FREE: u8 = 0;
const COPIED: u8 = 1;
const IN_FLIGHT: u8 = 2;
const MAPPED: u8 = 3;

/// A buffer the statistics of a frame are copied to and read back from.
struct Readback {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
}

/// What the GPU drew, read back: the draws, the pairs of an instance and a batch, the triangles, and
/// the instances at each level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drawn {
    pub draws: u32,
    pub pairs: u32,
    pub triangles: u32,
    pub levels: [u32; LEVELS],
}

/// A buffer of at least `bytes`, kept while large enough, else one twice the size it needs; whether
/// it was made.
fn sized(
    device: &wgpu::Device,
    kept: &mut Option<wgpu::Buffer>,
    bytes: u64,
    usage: wgpu::BufferUsages,
    label: &str,
) -> bool {
    if kept.as_ref().is_some_and(|buffer| buffer.size() >= bytes) {
        return false;
    }
    *kept = Some(device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.next_power_of_two().max(256),
        usage,
        mapped_at_creation: false,
    }));
    true
}

pub struct Choice {
    device: wgpu::Device,
    layout: wgpu::BindGroupLayout,
    choose: wgpu::ComputePipeline,
    blocks: wgpu::ComputePipeline,
    tops: wgpu::ComputePipeline,
    place: wgpu::ComputePipeline,
    pack: wgpu::ComputePipeline,
    scatter: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    /// Whether the draws are packed and counted by the GPU (`MULTI_DRAW_INDIRECT_COUNT`); every
    /// record drawn otherwise, those without instances drawing none.
    pub packed: bool,
    pub tables: Option<Arc<Tables>>,
    instances: Option<wgpu::Buffer>,
    frames: Option<wgpu::Buffer>,
    work: Option<wgpu::Buffer>,
    entries: Option<wgpu::Buffer>,
    args: Option<wgpu::Buffer>,
    /// The levels, written in turn: `last` the one written by the frame before, of the owners'
    /// sections then.
    levels: [Option<wgpu::Buffer>; 2],
    last: usize,
    sections: Vec<Section>,
    /// When the sections changed: the buffer of the levels before, and where each owner kept
    /// moves (from, to, bytes).
    moved: Option<(wgpu::Buffer, Vec<Move>)>,
    /// The bind groups of the choice, by the buffer of levels written; made again with the buffers.
    bind_groups: Option<[wgpu::BindGroup; 2]>,
    /// What this frame chooses from: its groups, its templates, its blocks of records, the offsets
    /// of the work.
    groups: u32,
    templates: u32,
    blocks_count: u32,
    work_offsets: [u32; 5],
    /// The blended states of this frame, in the order drawn, where their templates begin and how
    /// many.
    pub blended_regions: Vec<(State, u32, u32)>,
    copies: Vec<(Arc<wgpu::Buffer>, u64, u64)>,
    readbacks: Vec<Readback>,
    pub drawn: Drawn,
}

impl Choice {
    /// The choice on `device`, packing the draws when `packed`.
    pub fn new(device: &wgpu::Device, packed: bool) -> Self {
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("models choice"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, true),
                storage(5, false),
                storage(6, false),
                storage(7, false),
                storage(8, false),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("models choice"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("models choice"),
            source: wgpu::ShaderSource::Wgsl(include_str!("choice.wgsl").into()),
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("models choice"),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            device: device.clone(),
            choose: pipeline("choose"),
            blocks: pipeline("blocks"),
            tops: pipeline("tops"),
            place: pipeline("place"),
            pack: pipeline("pack"),
            scatter: pipeline("scatter"),
            layout,
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("models choice"),
                size: size_of::<Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            packed,
            tables: None,
            instances: None,
            frames: None,
            work: None,
            entries: None,
            args: None,
            levels: [None, None],
            last: 0,
            sections: Vec::new(),
            moved: None,
            bind_groups: None,
            groups: 0,
            templates: 0,
            blocks_count: 0,
            work_offsets: [0; 5],
            blended_regions: Vec::new(),
            copies: Vec::new(),
            readbacks: (0..3)
                .map(|_| Readback {
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("models choice statistics"),
                        size: (STATS * 4) as u64,
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    state: Arc::new(AtomicU8::new(FREE)),
                })
                .collect(),
            drawn: Drawn::default(),
        }
    }

    /// The tables the module made last; the bind groups made again with new ones.
    pub fn set_tables(&mut self, tables: Option<Arc<Tables>>) {
        let same = match (&self.tables, &tables) {
            (Some(kept), Some(new)) => Arc::ptr_eq(kept, new),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.tables = tables;
            self.bind_groups = None;
        }
    }

    /// The statistics copied by the frames before, read back without waiting: those copied asked to
    /// be read, those read kept.
    pub fn read_back(&mut self) {
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
                        let words: &[u32] = bytemuck::cast_slice(&data);
                        self.drawn = Drawn {
                            draws: words[0],
                            pairs: words[1],
                            triangles: words[2],
                            levels: std::array::from_fn(|level| words[4 + level]),
                        };
                    }
                    readback.buffer.unmap();
                    readback.state.store(FREE, Ordering::Release);
                }
                _ => {}
            }
        }
    }

    /// The buffers the vertex shader reads: the instances of the frame and the entries.
    pub fn buffers(&self) -> Option<(&wgpu::Buffer, &wgpu::Buffer)> {
        Some((self.instances.as_ref()?, self.entries.as_ref()?))
    }

    /// The frame: the owners' instances (each its buffer and its section) to copy into one buffer,
    /// the groups in sight, the instances of looks with blended batches in the order they are
    /// drawn; its tables of the frame and its parameters written. Whether a buffer the vertex shader
    /// reads was made again.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        queue: &wgpu::Queue,
        owners: &[(Arc<wgpu::Buffer>, Section)],
        groups: &[GroupOfFrame],
        blended: &[Blended],
        view_proj: Mat4,
        eye: Vec3,
        reach: f32,
    ) -> bool {
        let Some(tables) = &self.tables else {
            return false;
        };
        let instances: u32 = owners
            .iter()
            .map(|(_, (_, _, base, used))| base + used)
            .max()
            .unwrap_or(0);
        self.copies = owners
            .iter()
            .map(|(buffer, (_, _, base, used))| {
                (buffer.clone(), u64::from(*used) * INSTANCE, u64::from(*base) * INSTANCE)
            })
            .collect();
        // The templates: each blended state in the order drawn, its instances in theirs, for each
        // a template at each level.
        let mut states: Vec<State> = Vec::new();
        for instance in blended {
            for records in &tables.looks[instance.slot as usize].blended {
                for record in records {
                    if !states.contains(&record.state) {
                        states.push(record.state);
                    }
                }
            }
        }
        states.sort_by_key(rank);
        let mut template_words: Vec<u32> = Vec::with_capacity(blended.len() * TEMPLATE);
        let mut template_entries: Vec<[u32; 2]> = Vec::new();
        let mut blended_regions = Vec::with_capacity(states.len());
        for (region, state) in states.iter().enumerate() {
            let start = template_entries.len() as u32;
            for instance in blended {
                for (level, records) in tables.looks[instance.slot as usize].blended.iter().enumerate() {
                    for record in records.iter().filter(|record| record.state == *state) {
                        template_words.extend([
                            record.count,
                            record.first_index,
                            record.base_vertex as u32,
                            template_entries.len() as u32,
                            instance.index,
                            level as u32 + 1,
                            region as u32,
                        ]);
                        template_entries.push([instance.index, record.material]);
                    }
                }
            }
            blended_regions.push((*state, start, template_entries.len() as u32 - start));
        }
        let templates = template_entries.len() as u32;
        let records = tables.records;
        let entries: u64 = u64::from(templates)
            + groups
                .iter()
                .map(|group| u64::from(group.count) * u64::from(tables.looks[group.slot as usize].most))
                .sum::<u64>();
        let regions = (tables.regions.len() + blended_regions.len()) as u32;
        let blocks = records.div_ceil(256);
        // The work: the counts, the cursors, the sums of the records, those of the templates, the
        // counts of the draws, the statistics, the sums of the blocks and their total of draws.
        let work_offsets = [
            records,
            2 * records,
            3 * records,
            3 * records + templates,
            3 * records + templates + regions,
        ];
        let blocks_at = work_offsets[4] + STATS as u32;
        let region_words: Vec<u32> = blended_regions
            .iter()
            .flat_map(|(_, start, count)| [*start, *count])
            .collect();
        let mut group_words: Vec<u32> = Vec::with_capacity(groups.len() * GROUP);
        group_words.extend(groups.iter().flat_map(|group| [group.first, group.count, group.slot]));
        let frame_offsets = [
            0,
            region_words.len() as u32,
            (region_words.len() + group_words.len()) as u32,
        ];
        let frame_words = [region_words, group_words, template_words].concat();
        let params = Params {
            planes: planes(view_proj),
            eye: [eye.x, eye.y, eye.z, reach],
            limits: [LIMITS[0], LIMITS[1], LIMITS[2], MARGIN],
            sizes: [records, templates, groups.len() as u32, templates],
            regions: [
                tables.regions.len() as u32,
                blended_regions.len() as u32,
                u32::from(self.packed),
                work_offsets[4],
            ],
            statics: tables.offsets,
            frames: [frame_offsets[0], frame_offsets[1], frame_offsets[2], 0],
            work: [work_offsets[0], work_offsets[1], work_offsets[2], work_offsets[3]],
            blocks: [blocks_at, blocks, blocks_at + 2 * blocks, 0],
        };

        let device = self.device.clone();
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let indirect = wgpu::BufferUsages::INDIRECT;
        let read = sized(
            &device,
            &mut self.instances,
            u64::from(instances.max(1)) * INSTANCE,
            storage,
            "models instances of the frame",
        ) | sized(
            &device,
            &mut self.entries,
            entries.max(1) * ENTRY,
            storage,
            "models entries of the frame",
        );
        let mut made = read;
        made |= sized(
            &device,
            &mut self.frames,
            (frame_words.len().max(4) * 4) as u64,
            storage,
            "models choice of the frame",
        );
        made |= sized(
            &device,
            &mut self.work,
            (u64::from(blocks_at + 2 * blocks) + 1) * 4,
            storage | indirect | wgpu::BufferUsages::COPY_SRC,
            "models choice work",
        );
        made |= sized(
            &device,
            &mut self.args,
            u64::from(records + templates).max(1) * ARGS,
            wgpu::BufferUsages::STORAGE | indirect | wgpu::BufferUsages::COPY_SRC,
            "models draws of the frame",
        );
        // The levels before, where each owner kept had them, when the sections changed.
        let level_bytes = u64::from(instances.max(1)) * 4;
        let before = self.levels[self.last].clone();
        let grown = self
            .levels
            .iter()
            .any(|buffer| buffer.as_ref().is_none_or(|buffer| buffer.size() < level_bytes));
        if grown {
            self.levels = std::array::from_fn(|_| {
                Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("models levels"),
                    size: level_bytes.next_power_of_two().max(256),
                    usage: storage | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }))
            });
            made = true;
        }
        let sections: Vec<Section> = owners.iter().map(|(_, section)| *section).collect();
        self.moved = None;
        if (grown || sections != self.sections)
            && let Some(before) = before
        {
            let moves = sections
                .iter()
                .filter_map(|(number, layout, base, used)| {
                    self.sections
                        .iter()
                        .find(|kept| (kept.0, kept.1, kept.3) == (*number, *layout, *used))
                        .map(|kept| (u64::from(kept.2) * 4, u64::from(*base) * 4, u64::from(*used) * 4))
                })
                .filter(|(_, _, bytes)| *bytes > 0)
                .collect();
            self.moved = Some((before, moves));
        }
        self.sections = sections;
        if made {
            self.bind_groups = None;
        }
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        if !frame_words.is_empty() {
            queue.write_buffer(
                self.frames.as_ref().expect("sized"),
                0,
                bytemuck::cast_slice(&frame_words),
            );
        }
        if !template_entries.is_empty() {
            queue.write_buffer(
                self.entries.as_ref().expect("sized"),
                0,
                bytemuck::cast_slice(&template_entries),
            );
        }
        self.groups = groups.len() as u32;
        self.templates = templates;
        self.blocks_count = blocks;
        self.work_offsets = work_offsets;
        self.blended_regions = blended_regions;
        read
    }

    /// The bind groups of the choice, the buffer of levels `written` written, the other read.
    fn bind_groups(&mut self) -> Option<&[wgpu::BindGroup; 2]> {
        if self.bind_groups.is_none() {
            let tables = self.tables.as_ref()?;
            let levels = [self.levels[0].as_ref()?, self.levels[1].as_ref()?];
            let buffers = [self.instances.as_ref()?, &tables.buffer, self.frames.as_ref()?];
            let (work, entries, args) = (self.work.as_ref()?, self.entries.as_ref()?, self.args.as_ref()?);
            let group = |written: usize| {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("models choice"),
                    layout: &self.layout,
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
                            resource: buffers[2].as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: levels[1 - written].as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: levels[written].as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: work.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 7,
                            resource: entries.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 8,
                            resource: args.as_entire_binding(),
                        },
                    ],
                })
            };
            self.bind_groups = Some([group(0), group(1)]);
        }
        self.bind_groups.as_ref()
    }

    /// The computing of the frame: the owners' instances copied, the levels before put where the
    /// owners are now, the instances chosen, their draws packed and their entries written, the
    /// statistics copied to be read back.
    pub fn compute(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let (Some(instances), Some(work), Some(read), Some(written)) = (
            self.instances.clone(),
            self.work.clone(),
            self.levels[self.last].clone(),
            self.levels[1 - self.last].clone(),
        ) else {
            return;
        };
        for (buffer, bytes, at) in &self.copies {
            if *bytes > 0 {
                encoder.copy_buffer_to_buffer(buffer, 0, &instances, *at, *bytes);
            }
        }
        // The buffer written by the frame before is read; when the owners moved, it is first made
        // of what each owner kept, from where it was.
        let mut reading = self.last;
        if let Some((before, moves)) = self.moved.take() {
            encoder.clear_buffer(&written, 0, None);
            for (from, to, bytes) in moves {
                encoder.copy_buffer_to_buffer(&before, from, &written, to, bytes);
            }
            reading = 1 - self.last;
        }
        let writing = 1 - reading;
        let target = if writing == self.last { &read } else { &written };
        encoder.clear_buffer(target, 0, None);
        encoder.clear_buffer(&work, 0, None);
        self.last = writing;
        let (groups, templates, blocks) = (self.groups, self.templates, self.blocks_count);
        let Some(bind_groups) = self.bind_groups() else {
            return;
        };
        let bind_group = bind_groups[writing].clone();
        if groups > 0 || templates > 0 {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("models choice"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &bind_group, &[]);
            let mut dispatch = |pipeline: &wgpu::ComputePipeline, workgroups: u32| {
                if workgroups > 0 {
                    pass.set_pipeline(pipeline);
                    pass.dispatch_workgroups(workgroups, 1, 1);
                }
            };
            dispatch(&self.choose, groups);
            dispatch(&self.blocks, blocks);
            dispatch(&self.tops, 1);
            dispatch(&self.place, blocks);
            dispatch(&self.pack, blocks);
            dispatch(&self.scatter, groups);
        }
        if let Some(readback) = self
            .readbacks
            .iter()
            .find(|readback| readback.state.load(Ordering::Acquire) == FREE)
        {
            encoder.copy_buffer_to_buffer(
                &work,
                u64::from(self.work_offsets[4]) * 4,
                &readback.buffer,
                0,
                (STATS * 4) as u64,
            );
            readback.state.store(COPIED, Ordering::Release);
        }
    }

    /// The opaque draws, then the blended ones when `blended`, each state by `pipeline`: one command
    /// a state, counted by the GPU when packed; how many.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        blended: bool,
        pipeline: &dyn Fn(State) -> Arc<wgpu::RenderPipeline>,
    ) -> u32 {
        let (Some(tables), Some(args), Some(work)) = (&self.tables, &self.args, &self.work) else {
            return 0;
        };
        let (regions, first, counts): (&[(State, u32, u32)], u32, u32) = if blended {
            (
                &self.blended_regions,
                tables.records,
                self.work_offsets[3] + tables.regions.len() as u32,
            )
        } else {
            (&tables.regions, 0, self.work_offsets[3])
        };
        for (region, (state, start, count)) in regions.iter().enumerate() {
            pass.set_pipeline(&pipeline(*state));
            let offset = u64::from(first + start) * ARGS;
            if self.packed {
                pass.multi_draw_indexed_indirect_count(
                    args,
                    offset,
                    work,
                    u64::from(counts + region as u32) * 4,
                    *count,
                );
            } else {
                pass.multi_draw_indexed_indirect(args, offset, *count);
            }
        }
        regions.len() as u32
    }
}

#[cfg(test)]
impl Choice {
    /// The arguments of the opaque draws and the counts of the draws of their states, read back.
    pub fn written(&self, gpu: &uniwow_api::egui_wgpu::RenderState) -> (Vec<u32>, Vec<u32>) {
        let (Some(tables), Some(args), Some(work)) = (&self.tables, &self.args, &self.work) else {
            return (Vec::new(), Vec::new());
        };
        let words = |buffer: &wgpu::Buffer, bytes: u64| -> Vec<u32> {
            bytemuck::cast_slice(&crate::tests::read_back(gpu, buffer, bytes)).to_vec()
        };
        let args = words(args, u64::from(tables.records) * ARGS);
        let counts = words(
            work,
            (u64::from(self.work_offsets[3]) + tables.regions.len() as u64) * 4,
        );
        (args, counts[self.work_offsets[3] as usize..].to_vec())
    }
}
