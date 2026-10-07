//! The layer of the buildings: at each frame, each placement tested by its bounds against the view,
//! then each of its groups by its own, those of a building the camera is inside of first by what
//! its portals let it see; the batches of the groups seen listed as indirect draws, the opaque ones
//! by state, the blended ones from the farthest group, and drawn in the pass, a
//! `multi_draw_indexed_indirect` for each run of one state. The doodads of a group are shown while
//! it is seen through the portals, read by `models` at the frame after.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use uniwow_api::glam::{Mat4, Vec3, Vec4};
use uniwow_api::liquids::{self, Surfaces};
use uniwow_api::viewport::{Drawing, Layer, LayerStats, Phase, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::doodads::Part;
use crate::gpu::{CAMERA, Shared, State, WmoGpu, camera_values};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A building drawn: its transform, its file on the GPU, and the flags of its doodads once placed.
#[derive(Clone)]
pub struct Placed {
    pub transform: Mat4,
    pub wmo: Arc<WmoGpu>,
    pub parts: Option<Arc<[Part]>>,
}

/// What the module shares with its layer.
#[derive(Default)]
pub struct Scene {
    pub placed: Vec<Placed>,
    /// The liquids, by which a blended group is told beyond the surface of the water or on the
    /// eye's side.
    pub liquids: Option<liquids::Handle>,
    /// What the files of the buildings keep on the CPU.
    pub cpu: u64,
    /// The time the module spent steering at its last frame.
    pub steering: Duration,
}

/// The bounds in the world of `bounds` moved by `transform`: those of its eight corners.
pub fn world_bounds(transform: &Mat4, bounds: &[[f32; 3]; 2]) -> [Vec3; 2] {
    let [low, high] = bounds.map(Vec3::from);
    let mut world = [Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)];
    for corner in 0..8 {
        let point = Vec3::new(
            if corner & 1 == 0 { low.x } else { high.x },
            if corner & 2 == 0 { low.y } else { high.y },
            if corner & 4 == 0 { low.z } else { high.z },
        );
        let moved = transform.transform_point3(point);
        world = [world[0].min(moved), world[1].max(moved)];
    }
    world
}

/// The planes of the sides of the view of `view_proj`, a point inside where each is positive; its
/// far plane left out, the depth of the view reaching infinity.
pub fn planes(view_proj: &Mat4) -> [Vec4; 5] {
    let [x, y, z, w] = [0, 1, 2, 3].map(|row| view_proj.row(row));
    // Reverse Z: the near plane where the depth is 1.
    [w + x, w - x, w + y, w - y, w - z].map(|plane| plane / plane.truncate().length().max(f32::EPSILON))
}

/// Whether the box `bounds` lies, even in part, on the inner side of every plane.
pub fn in_sight(planes: &[Vec4; 5], bounds: &[Vec3; 2]) -> bool {
    planes.iter().all(|plane| {
        let nearest = Vec3::new(
            if plane.x >= 0.0 { bounds[1].x } else { bounds[0].x },
            if plane.y >= 0.0 { bounds[1].y } else { bounds[0].y },
            if plane.z >= 0.0 { bounds[1].z } else { bounds[0].z },
        );
        plane.truncate().dot(nearest) + plane.w >= 0.0
    })
}

/// A draw listed: its state, its indirect command and its entry, and its distance for the blended.
pub(crate) struct Listed {
    pub state: State,
    pub distance: f32,
    pub command: [u32; 5],
    pub entry: [u32; 4],
}

/// The groups of `building`, its bounds in the world `bounds`, seen from `eye` through its portals
/// within the sides of the view `planes`, when `eye` is in one of its groups inside.
pub fn seen_inside(building: &Placed, eye: Vec3, planes: &[Vec4; 5], bounds: &[Vec3; 2]) -> Option<Vec<bool>> {
    if eye.cmplt(bounds[0]).any() || eye.cmpgt(bounds[1]).any() {
        return None;
    }
    let local = building.transform.inverse().transform_point3(eye);
    let start = building.wmo.cells.holding(local)?;
    // A plane of the world, in the axes of the building.
    let turned = building.transform.transpose();
    let local_planes: Vec<Vec4> = planes.iter().map(|plane| turned * *plane).collect();
    Some(building.wmo.cells.seen(local, &local_planes, start))
}

/// Shows the doodads of `parts` held by a group `seen` says is seen, and those no group holds; all
/// of them without `seen`.
fn show(parts: Option<&Arc<[Part]>>, seen: Option<&[bool]>) {
    for (groups, flag) in parts.iter().flat_map(|parts| parts.iter()) {
        let shown = seen.is_none_or(|seen| {
            groups.is_empty()
                || groups
                    .iter()
                    .any(|group| seen.get(usize::from(*group)).copied().unwrap_or(false))
        });
        flag.store(shown, Ordering::Relaxed);
    }
}

/// What the frame draws of the buildings: the draws of the batches of the groups in sight, the
/// opaque ones by state, the blended ones from the farthest, those beyond the surface of the water
/// from the eye apart from those on its side; how many placements and groups are in sight, how
/// many placements hold the camera in a group inside, and how many groups those see through their
/// portals.
#[derive(Default)]
pub(crate) struct Listing {
    pub opaque: Vec<Listed>,
    pub beyond: Vec<Listed>,
    pub near: Vec<Listed>,
    pub buildings: usize,
    pub groups: usize,
    pub inside: usize,
    pub through: usize,
}

/// What the frame draws of `placed` seen in `view`, a blended group by the centre of its bounds
/// against the surfaces of the water `surfaces`; the doodads of each shown or hidden.
pub(crate) fn list(placed: &[Placed], view: &View, surfaces: Option<&Surfaces>) -> Listing {
    let planes = planes(&view.view_proj);
    let mut listing = Listing::default();
    for (instance, building) in placed.iter().enumerate() {
        let bounds = world_bounds(&building.transform, &building.wmo.bounds);
        if !in_sight(&planes, &bounds) {
            show(building.parts.as_ref(), None);
            continue;
        }
        listing.buildings += 1;
        let seen = seen_inside(building, view.eye, &planes, &bounds);
        if let Some(seen) = &seen {
            listing.inside += 1;
            listing.through += seen.iter().filter(|seen| **seen).count();
        }
        show(building.parts.as_ref(), seen.as_deref());
        for (index, group) in building.wmo.groups.iter().enumerate() {
            if seen
                .as_ref()
                .is_some_and(|seen| !seen.get(index).copied().unwrap_or(false))
            {
                continue;
            }
            let bounds = world_bounds(&building.transform, &group.bounds);
            if !in_sight(&planes, &bounds) {
                continue;
            }
            listing.groups += 1;
            let centre = (bounds[0] + bounds[1]) * 0.5;
            let distance = centre.distance(view.eye);
            let beyond = surfaces.is_some_and(|surfaces| surfaces.phase(view.eye, centre) == Phase::Beyond);
            for batch in &group.batches {
                let listed = Listed {
                    state: batch.state,
                    distance,
                    command: [batch.count, 1, batch.first, building.wmo.base_vertex as u32, 0],
                    entry: [instance as u32, batch.material, group.flags | batch.kind << 8, 0],
                };
                if !batch.state.blended() {
                    listing.opaque.push(listed);
                } else if beyond {
                    listing.beyond.push(listed);
                } else {
                    listing.near.push(listed);
                }
            }
        }
    }
    listing.opaque.sort_by_key(|listed| listed.state);
    for blended in [&mut listing.beyond, &mut listing.near] {
        blended.sort_by(|a, b| b.distance.total_cmp(&a.distance));
    }
    listing
}

/// A run of draws of one state: its first command and how many.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Run {
    pub state: State,
    pub first: u32,
    pub count: u32,
}

/// The runs of one state in `listed`, from the command `first`.
pub(crate) fn runs(listed: &[Listed], first: u32) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (index, draw) in listed.iter().enumerate() {
        match runs.last_mut() {
            Some(run) if run.state == draw.state => run.count += 1,
            _ => runs.push(Run {
                state: draw.state,
                first: first + index as u32,
                count: 1,
            }),
        }
    }
    runs
}

/// A buffer of the frame, made again larger when it no longer holds what is written.
struct Grown {
    buffer: Option<wgpu::Buffer>,
    label: &'static str,
    usage: wgpu::BufferUsages,
}

impl Grown {
    fn new(label: &'static str, usage: wgpu::BufferUsages) -> Self {
        Self {
            buffer: None,
            label,
            usage: usage | wgpu::BufferUsages::COPY_DST,
        }
    }

    /// Writes `data`; whether the buffer was made again.
    fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[u8]) -> bool {
        let size = (data.len() as u64).max(64);
        let made = self.buffer.as_ref().is_none_or(|buffer| buffer.size() < size);
        if made {
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: size.next_power_of_two(),
                usage: self.usage,
                mapped_at_creation: false,
            }));
        }
        if let Some(buffer) = &self.buffer
            && !data.is_empty()
        {
            queue.write_buffer(buffer, 0, data);
        }
        made
    }
}

pub struct BuildingsLayer {
    shared: Arc<Shared>,
    scene: Arc<Mutex<Scene>>,
    camera: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    instances: Grown,
    entries: Grown,
    commands: Grown,
    group: Option<(wgpu::BindGroup, (u64, u64))>,
    /// The buildings drawn this frame, held until the next, and the runs of each phase.
    drawn: Vec<Placed>,
    opaque: Vec<Run>,
    beyond: Vec<Run>,
    near: Vec<Run>,
    stats: LayerStats,
}

impl BuildingsLayer {
    pub fn new(shared: Arc<Shared>, scene: Arc<Mutex<Scene>>) -> Self {
        Self {
            shared,
            scene,
            camera: None,
            instances: Grown::new("buildings instances", wgpu::BufferUsages::STORAGE),
            entries: Grown::new("buildings entries", wgpu::BufferUsages::STORAGE),
            commands: Grown::new("buildings commands", wgpu::BufferUsages::INDIRECT),
            group: None,
            drawn: Vec::new(),
            opaque: Vec::new(),
            beyond: Vec::new(),
            near: Vec::new(),
            stats: LayerStats::default(),
        }
    }
}

impl Layer for BuildingsLayer {
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        let shared = self.shared.clone();
        let (placed, steering, cpu, liquids) = {
            let scene = lock(&self.scene);
            (scene.placed.clone(), scene.steering, scene.cpu, scene.liquids.clone())
        };
        let surfaces = liquids.map(|liquids| liquids.surfaces());
        let (camera, _) = self.camera.get_or_insert_with(|| {
            let buffer = shared.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("buildings camera"),
                size: (CAMERA * 4) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = shared.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("buildings camera"),
                layout: &shared.camera_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            (buffer, group)
        });
        gpu.queue
            .write_buffer(camera, 0, bytemuck::cast_slice(&camera_values(view)));

        let Listing {
            opaque,
            beyond,
            near,
            buildings,
            groups,
            inside,
            through,
        } = list(&placed, view, surfaces.as_deref());
        let instances: Vec<[f32; 16]> = placed
            .iter()
            .map(|building| {
                let rows = [0, 1, 2].map(|row| building.transform.row(row).to_array());
                let [r, g, b] = building.wmo.ambient;
                let mut values = [0.0; 16];
                for (at, row) in rows.iter().enumerate() {
                    values[at * 4..at * 4 + 4].copy_from_slice(row);
                }
                values[12..15].copy_from_slice(&[r, g, b]);
                values
            })
            .collect();
        let listed: Vec<&Listed> = opaque.iter().chain(&beyond).chain(&near).collect();
        let commands: Vec<[u32; 5]> = listed
            .iter()
            .enumerate()
            .map(|(index, listed)| {
                let mut command = listed.command;
                command[4] = index as u32;
                command
            })
            .collect();
        let entries: Vec<[u32; 4]> = listed.iter().map(|listed| listed.entry).collect();
        let device = &shared.device;
        let made = self
            .instances
            .write(device, &gpu.queue, bytemuck::cast_slice(&instances))
            | self.entries.write(device, &gpu.queue, bytemuck::cast_slice(&entries));
        self.commands.write(device, &gpu.queue, bytemuck::cast_slice(&commands));
        let generation = shared.generation();
        if made || self.group.as_ref().is_none_or(|(_, at)| *at != generation) {
            self.group = match (&self.instances.buffer, &self.entries.buffer) {
                (Some(instances), Some(entries)) => {
                    shared.bind_group(instances, entries).map(|group| (group, generation))
                }
                _ => None,
            };
        }
        self.opaque = runs(&opaque, 0);
        self.beyond = runs(&beyond, opaque.len() as u32);
        self.near = runs(&near, (opaque.len() + beyond.len()) as u32);
        let triangles: u64 = listed.iter().map(|listed| u64::from(listed.command[0] / 3)).sum();
        let all_groups: usize = placed.iter().map(|building| building.wmo.groups.len()).sum();
        self.stats = LayerStats {
            draws: (self.opaque.len() + self.beyond.len() + self.near.len()) as u64,
            triangles,
            bytes: shared.bytes(),
            items: format!(
                "{buildings} buildings in sight of {}, {groups} groups of {all_groups}, {} batches ({} blended, {} \
                 beyond the water)\n  \
                 the camera inside {inside} of them, {through} groups seen through their portals; the arrays {} \
                 textures; on the CPU {:.1} MB",
                placed.len(),
                listed.len(),
                beyond.len() + near.len(),
                beyond.len(),
                shared.arrays.counts().placed,
                cpu as f64 / (1024.0 * 1024.0)
            ),
            steering,
        };
        self.drawn = placed;
    }

    fn drawing(&self) -> Drawing {
        Drawing::Pass
    }

    fn draw_pass(
        &mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        phase: Phase,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        let runs = match phase {
            Phase::Opaque => &self.opaque,
            Phase::Beyond => &self.beyond,
            Phase::Near => &self.near,
            Phase::Water => return,
        };
        let (Some((_, camera)), Some((group, _)), Some(commands), Some((vertices, _)), Some((indices, _))) = (
            &self.camera,
            &self.group,
            &self.commands.buffer,
            self.shared.vertices.buffer(),
            self.shared.indices.buffer(),
        ) else {
            return;
        };
        if runs.is_empty() {
            return;
        }
        pass.set_bind_group(0, camera, &[]);
        pass.set_bind_group(1, group, &[]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        for run in runs {
            pass.set_pipeline(&self.shared.pipeline(run.state));
            pass.multi_draw_indexed_indirect(commands, u64::from(run.first) * 20, run.count);
        }
    }

    fn stats(&self) -> LayerStats {
        self.stats.clone()
    }
}
