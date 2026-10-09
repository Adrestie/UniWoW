//! The instances drawn from the pool, chosen by the GPU at each frame (`choice.wgsl`): in sight,
//! within the reach of their size and at the level of skin their distance chooses, each instance
//! by itself; the opaque batches of every look gathered by state into one list of draws each,
//! packed in the order of their records. Chosen twice: before the first pass of the view, those
//! drawn at the frame before; between the passes, those the pyramid of the depth the first pass
//! left does not hide, drawn in the second pass unless the first drew them. The blended batches are drawn by templates the CPU writes
//! each frame, an instance at a time the farthest first and, for each, one at each level of its
//! look: the GPU keeps those of the level it chose, in that order. The CPU still finds the groups
//! in sight, to give the GPU only those; the tables of the looks are made by a job of the module
//! when its looks change (`Tables`). What was drawn is read back some frames later.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::journal;
use uniwow_api::models::LookId;
use uniwow_api::viewport::{Phase, Pyramid};
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
/// The most workgroups on a side of a dispatch, as wgpu takes by default.
const SIDE: u32 = 65_535;
/// The words of the statistics: the draws, the pairs of an instance and a batch, the triangles,
/// the instances the pyramid hid, and the instances drawn at each level.
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
    view_proj: [[f32; 4]; 4],
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

/// A group given, as the GPU reads it: its first instance in the arena of the instances, its count,
/// the slot of its look.
#[derive(Clone, Copy, Debug)]
pub struct GroupOfFrame {
    pub first: u32,
    pub count: u32,
    pub slot: u32,
}

/// An instance of a look with blended batches, in the order its templates are drawn: its place in
/// the arena of the instances and the slot of its look.
#[derive(Clone, Copy, Debug)]
pub struct Blended {
    pub index: u32,
    pub slot: u32,
}

/// Where the table of an owner's bones goes, in bytes: from, to, how many.
pub type Move = (u64, u64, u64);

const FREE: u8 = 0;
const COPIED: u8 = 1;
const IN_FLIGHT: u8 = 2;
const MAPPED: u8 = 3;

/// A buffer the statistics of a frame are copied to and read back from.
struct Readback {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
}

/// What the GPU drew, read back: the draws, the pairs of an instance and a batch, the triangles, the
/// instances at each level, and those in sight the pyramid hid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drawn {
    pub draws: u32,
    pub pairs: u32,
    pub triangles: u32,
    pub levels: [u32; LEVELS],
    pub hidden: u32,
}

/// The pipelines of a phase of the choice: its choosing, its templates and its scattering.
struct PhasePipelines {
    choose: wgpu::ComputePipeline,
    tops: wgpu::ComputePipeline,
    scatter: wgpu::ComputePipeline,
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
    /// The pyramid of the depth, which the second phase tests against, and its bind group by the
    /// generation of the pyramid.
    pyramid_layout: wgpu::BindGroupLayout,
    pyramid_group: Option<(u64, wgpu::BindGroup)>,
    first: PhasePipelines,
    second: PhasePipelines,
    blocks: wgpu::ComputePipeline,
    place: wgpu::ComputePipeline,
    pack: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    /// Whether the draws are packed and counted by the GPU (`MULTI_DRAW_INDIRECT_COUNT`); every
    /// record drawn otherwise, those without instances drawing none.
    pub packed: bool,
    pub tables: Option<Arc<Tables>>,
    /// The arena of the owners' instances, read where each owner's are, and its generation: a new
    /// buffer once it grew.
    arena: Option<(Arc<wgpu::Buffer>, u64)>,
    /// For each instance of the arena, its first bone plus one, 0 at rest: copied from the tables
    /// of the owners the thread of the animations wrote, from its buffer.
    bone_table: Option<wgpu::Buffer>,
    bones: Option<(Arc<wgpu::Buffer>, Vec<Move>)>,
    frames: Option<wgpu::Buffer>,
    work: Option<wgpu::Buffer>,
    entries: Option<wgpu::Buffer>,
    args: Option<wgpu::Buffer>,
    /// The levels of the instances of the arena, written in turn: `last` the one written by the
    /// frame before.
    levels: [Option<wgpu::Buffer>; 2],
    last: usize,
    /// Once the arena grew, the levels before, copied into the new buffer read at the next frame.
    carried: Option<wgpu::Buffer>,
    /// The bind groups of the choice, by the buffer of levels written; made again with the buffers.
    bind_groups: Option<[wgpu::BindGroup; 2]>,
    /// What this frame chooses from: its groups, its templates, its blocks of records, the offsets
    /// of the work.
    groups: u32,
    templates: u32,
    blocks_count: u32,
    work_offsets: [u32; 5],
    /// The blended states of this frame, in the order drawn, where their templates begin and how
    /// many: those of the instances beyond the surface of the water from the eye, so many, then
    /// those on the eye's side.
    pub blended_regions: Vec<(State, u32, u32)>,
    beyond_regions: usize,
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
        let pyramid_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("models choice pyramid"),
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("models choice"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let tested_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("models choice against the depth"),
            bind_group_layouts: &[Some(&layout), Some(&pyramid_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("models choice"),
            source: wgpu::ShaderSource::Wgsl(include_str!("choice.wgsl").into()),
        });
        let pipeline_of = |layout: &wgpu::PipelineLayout, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("models choice"),
                layout: Some(layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let pipeline = |entry: &str| pipeline_of(&pipeline_layout, entry);
        Self {
            device: device.clone(),
            first: PhasePipelines {
                choose: pipeline("choose_first"),
                tops: pipeline("tops_first"),
                scatter: pipeline("scatter_first"),
            },
            second: PhasePipelines {
                choose: pipeline_of(&tested_layout, "choose_second"),
                tops: pipeline("tops_second"),
                scatter: pipeline("scatter_second"),
            },
            blocks: pipeline("blocks"),
            place: pipeline("place"),
            pack: pipeline("pack"),
            layout,
            pyramid_layout,
            pyramid_group: None,
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("models choice"),
                size: size_of::<Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            packed,
            tables: None,
            arena: None,
            bone_table: None,
            bones: None,
            frames: None,
            work: None,
            entries: None,
            args: None,
            levels: [None, None],
            last: 0,
            carried: None,
            bind_groups: None,
            groups: 0,
            templates: 0,
            blocks_count: 0,
            work_offsets: [0; 5],
            blended_regions: Vec::new(),
            beyond_regions: 0,
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
                            hidden: words[3],
                        };
                    }
                    readback.buffer.unmap();
                    readback.state.store(FREE, Ordering::Release);
                }
                _ => {}
            }
        }
    }

    /// The buffers the vertex shader reads: the arena of the instances and the entries.
    pub fn buffers(&self) -> Option<(&wgpu::Buffer, &wgpu::Buffer)> {
        Some((&*self.arena.as_ref()?.0, self.entries.as_ref()?))
    }

    /// Where the bones of each instance of the arena begin, plus one.
    pub fn bone_table(&self) -> Option<&wgpu::Buffer> {
        self.bone_table.as_ref()
    }

    /// The frame: the arena of the owners' instances and its generation, each instance read where
    /// its owner's are there; the tables of their bones from the buffer of the animations (where
    /// each owner's begins, where it goes in the arena, its bytes); the groups given, the instances
    /// of looks with blended batches in the order they are drawn; its tables of the frame and its
    /// parameters written. Whether a buffer the vertex shader reads was made again.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        queue: &wgpu::Queue,
        arena: Option<(Arc<wgpu::Buffer>, u64)>,
        bones: Option<(Arc<wgpu::Buffer>, Vec<Move>)>,
        groups: &[GroupOfFrame],
        blended: [&[Blended]; 2],
        view_proj: Mat4,
        eye: Vec3,
        reach: f32,
    ) -> bool {
        let (Some(tables), Some(arena)) = (&self.tables, arena) else {
            return false;
        };
        // Every instance of the arena may be chosen, by its place there.
        let places = (arena.0.size() / INSTANCE) as u32;
        let new_arena = self.arena.as_ref().is_none_or(|(_, generation)| *generation != arena.1);
        self.arena = Some(arena);
        // The templates: the instances beyond the water, then those on the eye's side; of each
        // part each blended state in the order drawn, its instances in theirs, for each a template
        // at each level.
        let mut template_words: Vec<u32> = Vec::with_capacity((blended[0].len() + blended[1].len()) * TEMPLATE);
        let mut template_entries: Vec<[u32; 2]> = Vec::new();
        let mut blended_regions = Vec::new();
        let mut beyond_regions = 0;
        for (part, instances) in blended.iter().enumerate() {
            let mut states: Vec<State> = Vec::new();
            for instance in *instances {
                for records in &tables.looks[instance.slot as usize].blended {
                    for record in records {
                        if !states.contains(&record.state) {
                            states.push(record.state);
                        }
                    }
                }
            }
            states.sort_by_key(rank);
            for state in &states {
                let region = blended_regions.len();
                let start = template_entries.len() as u32;
                for instance in *instances {
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
            if part == 0 {
                beyond_regions = blended_regions.len();
            }
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
            view_proj: view_proj.to_cols_array_2d(),
        };

        let device = self.device.clone();
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let indirect = wgpu::BufferUsages::INDIRECT;
        let read = new_arena
            | sized(
                &device,
                &mut self.entries,
                entries.max(1) * ENTRY,
                storage,
                "models entries of the frame",
            )
            | sized(
                &device,
                &mut self.bone_table,
                u64::from(places.max(1)) * 4,
                storage,
                "models bone table of the arena",
            );
        self.bones = bones;
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
        // The levels of the instances of the arena; once it grew, those before carried over, the
        // places there kept.
        let level_bytes = u64::from(places.max(1)) * 4;
        let before = self.levels[self.last].clone();
        let grown = self
            .levels
            .iter()
            .any(|buffer| buffer.as_ref().is_none_or(|buffer| buffer.size() < level_bytes));
        if grown {
            self.carried = before;
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
        if made {
            self.bind_groups = None;
        }
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        journal::uploaded((size_of_val(&params) + frame_words.len() * 4 + template_entries.len() * 8) as u64);
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
        self.beyond_regions = beyond_regions;
        read
    }

    /// The bind groups of the choice, the buffer of levels `written` written, the other read.
    fn bind_groups(&mut self) -> Option<&[wgpu::BindGroup; 2]> {
        if self.bind_groups.is_none() {
            let tables = self.tables.as_ref()?;
            let levels = [self.levels[0].as_ref()?, self.levels[1].as_ref()?];
            let buffers = [&*self.arena.as_ref()?.0, &tables.buffer, self.frames.as_ref()?];
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

    /// The computing of the frame before its first pass: the tables of the bones put where their
    /// owners' instances are in the arena, the instances drawn at the frame before chosen, their
    /// draws packed and their entries written.
    pub fn compute(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let (Some(work), Some(read), Some(written)) = (
            self.work.clone(),
            self.levels[self.last].clone(),
            self.levels[1 - self.last].clone(),
        ) else {
            return;
        };
        if let Some(before) = self.carried.take() {
            encoder.copy_buffer_to_buffer(&before, 0, &read, 0, before.size().min(read.size()));
        }
        if let Some(table) = &self.bone_table {
            encoder.clear_buffer(table, 0, None);
            if let Some((buffer, moves)) = &self.bones {
                for (from, to, bytes) in moves {
                    encoder.copy_buffer_to_buffer(buffer, *from, table, *to, *bytes);
                }
            }
        }
        // The buffer written by the frame before is read, the other written.
        let writing = 1 - self.last;
        encoder.clear_buffer(&written, 0, None);
        encoder.clear_buffer(&work, 0, None);
        self.last = writing;
        let (groups, templates, blocks) = (self.groups, self.templates, self.blocks_count);
        let Some(bind_groups) = self.bind_groups() else {
            return;
        };
        let bind_group = bind_groups[writing].clone();
        self.choose(encoder, &bind_group, None, groups, templates, blocks);
    }

    /// The phase of the choice of `phase`, the first without a pyramid, the second with its bind
    /// group: its instances chosen, their draws packed and their entries written.
    fn choose(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind_group: &wgpu::BindGroup,
        pyramid: Option<&wgpu::BindGroup>,
        groups: u32,
        templates: u32,
        blocks: u32,
    ) {
        if groups == 0 && templates == 0 {
            return;
        }
        let phase = if pyramid.is_some() { &self.second } else { &self.first };
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("models choice"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, bind_group, &[]);
        if let Some(pyramid) = pyramid {
            pass.set_bind_group(1, pyramid, &[]);
        }
        let mut dispatch = |pipeline: &wgpu::ComputePipeline, [across, down]: [u32; 2]| {
            if across > 0 && down > 0 {
                pass.set_pipeline(pipeline);
                pass.dispatch_workgroups(across, down, 1);
            }
        };
        // A workgroup a group, over rows of the most a side of a dispatch takes (`choice.wgsl`).
        let by_group = [groups.min(SIDE), groups.div_ceil(SIDE)];
        dispatch(&phase.choose, by_group);
        dispatch(&self.blocks, [blocks, 1]);
        dispatch(&phase.tops, [1, 1]);
        dispatch(&self.place, [blocks, 1]);
        dispatch(&self.pack, [blocks, 1]);
        dispatch(&phase.scatter, by_group);
    }

    /// The computing of the frame between its passes: the instances in sight tested against
    /// `pyramid`, those not hidden that the first phase did not draw chosen for the second pass,
    /// the blended with them, and those not hidden kept for the next frame; the statistics of both
    /// phases copied to be read back.
    pub fn occlude(&mut self, encoder: &mut wgpu::CommandEncoder, pyramid: &Pyramid<'_>) {
        let Some(work) = self.work.clone() else {
            return;
        };
        if self
            .pyramid_group
            .as_ref()
            .is_none_or(|(generation, _)| *generation != pyramid.generation)
        {
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("models choice pyramid"),
                layout: &self.pyramid_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(pyramid.view),
                }],
            });
            self.pyramid_group = Some((pyramid.generation, group));
        }
        // The work of the first phase cleared but its statistics, which the second adds to.
        let stats = u64::from(self.work_offsets[4]) * 4;
        encoder.clear_buffer(&work, 0, Some(stats));
        encoder.clear_buffer(&work, stats + (STATS * 4) as u64, None);
        let (groups, templates, blocks, writing) = (self.groups, self.templates, self.blocks_count, self.last);
        let Some(bind_group) = self.bind_groups().map(|groups| groups[writing].clone()) else {
            return;
        };
        let pyramid_group = self.pyramid_group.as_ref().map(|(_, group)| group.clone());
        self.choose(encoder, &bind_group, pyramid_group.as_ref(), groups, templates, blocks);
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

    /// The draws of `phase`: opaque, blended beyond the water or on the eye's side, each state by
    /// `pipeline`: one command a state, counted by the GPU when packed; how many.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        phase: Phase,
        pipeline: &dyn Fn(State) -> Arc<wgpu::RenderPipeline>,
    ) -> u32 {
        let (Some(tables), Some(args), Some(work)) = (&self.tables, &self.args, &self.work) else {
            return 0;
        };
        // Nothing chosen: the choice did not run, its arguments are those of the frame before.
        if self.groups == 0 && self.templates == 0 {
            return 0;
        }
        let blended_counts = self.work_offsets[3] + tables.regions.len() as u32;
        let beyond = self.beyond_regions.min(self.blended_regions.len());
        // The regions of the phase, the first of them by its place among those counted.
        let (regions, first, counts): (&[(State, u32, u32)], u32, u32) = match phase {
            // The opaque the first phase chose, then those the second did.
            Phase::Opaque | Phase::Revealed => (&tables.regions, 0, self.work_offsets[3]),
            Phase::Beyond => (&self.blended_regions[..beyond], tables.records, blended_counts),
            Phase::Near => (
                &self.blended_regions[beyond..],
                tables.records,
                blended_counts + beyond as u32,
            ),
            Phase::Water => return 0,
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
    /// The arguments of the opaque draws of the second phase and the counts of the draws of their
    /// states, read back.
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
