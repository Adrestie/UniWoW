//! The liquids of the tiles of the map the terrain shows, around the camera of the 3D view within
//! the distance of the terrain: the layers of each tile read by a job, the nearest first, within
//! the reach the budget of the view lets load and the room of the arenas holding them, let go
//! beyond the reach it lets keep; their meshes put on the GPU and drawn by the layer of the module;
//! the surfaces of their water given through the service `liquids`, by which the other layers tell
//! what they blend beyond the water from what is on the eye's side. A tile refused for want of room
//! is read again once the arenas gave a range back or the camera moved. The liquids other modules
//! place through the service, as `buildings` those of its groups, are put on the GPU by jobs, drawn
//! while their owners show them, and their water added to the surfaces. Nothing is changed: no undo
//! entry, no file written.

mod gpu;
mod layer;
mod mesh;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use uniwow_api::arena::{self, Refusal};
use uniwow_api::formats::{self, TILE, TileId, Wdt};
use uniwow_api::glam::Vec3;
use uniwow_api::journal;
use uniwow_api::liquids::{self, Grid, Liquids, Placed, Surfaces};
use uniwow_api::serde_json::json;
use uniwow_api::viewport::Demand;
use uniwow_api::{Context, DockArea, JobId, JobOutcome, Module, PropertyValue, Registrar, egui, log, viewport};

use gpu::{Shared, TileGpu};
use layer::{LiquidsLayer, Poured, Scene};

/// The distance of the terrain, in tiles, until it says it.
const DEFAULT_DISTANCE: u32 = 3;
/// The refusals of tiles the panel keeps.
const REFUSALS: usize = 8;
/// What a tile not yet read is expected to take on the GPU, in its arenas of vertices and of
/// indices, while none is held: about a tile of open sea.
const EXPECTED: [u64; 2] = [48 << 10, 16 << 10];
/// The share of an arena the tiles read fill at most; those held keep it whole.
const LOAD_SHARE: f64 = 0.9;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The liquids an owner placed, each with its flag.
type Pouring = Vec<(Placed, Arc<AtomicBool>)>;
type Pour = Arc<Pouring>;

/// The surfaces of the water held, given to the other modules; and the liquids they placed or took
/// away since the module last took them, by owner, none for those taken away.
#[derive(Default)]
struct Water {
    surfaces: Mutex<Arc<Surfaces>>,
    placed: Mutex<HashMap<String, Option<Pouring>>>,
}

impl Liquids for Water {
    fn surfaces(&self) -> Arc<Surfaces> {
        journal::lock(&self.surfaces, "liquids surfaces").clone()
    }

    fn place(&self, owner: &str, liquids: Vec<Placed>) -> Vec<Arc<AtomicBool>> {
        let flags: Vec<Arc<AtomicBool>> = liquids.iter().map(|_| Arc::new(AtomicBool::new(true))).collect();
        lock(&self.placed).insert(
            owner.to_owned(),
            Some(liquids.into_iter().zip(flags.iter().cloned()).collect()),
        );
        flags
    }

    fn clear(&self, owner: &str) {
        lock(&self.placed).insert(owner.to_owned(), None);
    }
}

/// The liquids of an owner on the GPU, the surfaces of their water, and what they take.
struct PouredOwner {
    liquids: Vec<Poured>,
    surfaces: Surfaces,
    bytes: u64,
}

/// Where the liquids an owner placed stand: waiting to be put on the GPU, refused for want of room
/// when the arenas had given back so many ranges and the camera stood there; being put there by a
/// job, the arenas having given back so many ranges when it started; on it; or refused.
enum Owned {
    Waiting(Pour, Option<(u64, [f32; 2])>),
    Pouring(JobId, u64, Pour),
    Held(PouredOwner),
    Refused,
}

/// Puts the liquids `pour` on the GPU of `shared`, their types read through `formats`.
fn pour(
    formats: &dyn formats::Formats,
    shared: &Arc<Shared>,
    pour: &[(Placed, Arc<AtomicBool>)],
) -> Result<PouredOwner, Refusal> {
    let types = formats.liquid_types()?;
    let mut poured = PouredOwner {
        liquids: Vec::new(),
        surfaces: Surfaces::default(),
        bytes: 0,
    };
    let mut cells = Vec::new();
    for (placed, shown) in pour {
        let meshes = mesh::placed(placed, |liquid| {
            let record = types.iter().find(|record| record.id == u32::from(liquid))?;
            Some((shared.slot(formats, record)?, mesh::is_water(record.kind)))
        });
        cells.extend(meshes.surfaces.iter().copied());
        if let Some(gpu) = gpu::upload(shared, &meshes)? {
            poured.bytes += gpu.bytes;
            let bounds = placed.positions.iter().fold(
                [Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)],
                |[low, high], position| [low.min(Vec3::from(*position)), high.max(Vec3::from(*position))],
            );
            poured.liquids.push(Poured {
                gpu: Arc::new(gpu),
                shown: shown.clone(),
                bounds,
            });
        }
    }
    poured.surfaces = Surfaces::from_cells(cells);
    Ok(poured)
}

/// The liquids of a tile read: on the GPU, none where it has none; the surface of its water.
struct Held {
    gpu: Option<Arc<TileGpu>>,
    grid: Option<Arc<Grid>>,
}

/// A read of a tile, or why it could not be read.
type Read = Result<Held, Refusal>;

/// Reads the liquids of the tile `tile` of the map `directory` through `formats` and puts them on
/// the GPU of `shared`.
fn read(formats: &dyn formats::Formats, shared: &Arc<Shared>, directory: &str, tile: TileId) -> Read {
    let types = formats.liquid_types()?;
    let layers = formats
        .liquids(directory, tile.x, tile.y)?
        .ok_or("named by its WDT, but not read")?;
    let meshes = mesh::meshes(&layers, |liquid| {
        let record = types.iter().find(|record| record.id == u32::from(liquid))?;
        Some((shared.slot(formats, record)?, mesh::is_water(record.kind)))
    });
    let grid = Grid::of([tile.y as i32, tile.x as i32], meshes.surfaces.iter().copied()).map(Arc::new);
    Ok(Held {
        gpu: gpu::upload(shared, &meshes)?.map(Arc::new),
        grid,
    })
}

/// The surfaces of the water of the tiles `held`, their grids shared, not copied.
fn surfaces(held: &HashMap<TileId, Held>) -> Surfaces {
    let mut surfaces = Surfaces::default();
    for (tile, held) in held {
        if let Some(grid) = &held.grid {
            surfaces.insert([tile.y as i32, tile.x as i32], grid.clone());
        }
    }
    surfaces
}

/// Where a tile wanted stands: read, being read, waiting for the budget or its turn, or refused for
/// want of room and none made since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stand {
    Held,
    Reading,
    Waiting,
    NoRoom,
}

/// Of the tiles `wanted`, the nearest first, each with its distance and where it stands: those to
/// read, waiting within the reach the budget lets load, `budget[0]`, and nearer than the room of the
/// arenas to load, `room[0]`, so many that no more than `slots` are read at once; and those held or
/// being read to let go, beyond the reach it lets keep, `budget[1]`, or as far as the room to keep,
/// `room[1]`.
fn steps(
    wanted: &[(TileId, f32, Stand)],
    budget: [f32; 2],
    room: [f32; 2],
    slots: usize,
) -> (Vec<TileId>, Vec<TileId>) {
    let kept = |distance: f32| distance <= budget[1] && distance < room[1];
    let release: Vec<TileId> = wanted
        .iter()
        .filter(|(_, distance, stand)| matches!(stand, Stand::Held | Stand::Reading) && !kept(*distance))
        .map(|(tile, _, _)| *tile)
        .collect();
    let reading = wanted
        .iter()
        .filter(|(_, distance, stand)| *stand == Stand::Reading && kept(*distance))
        .count();
    let start = wanted
        .iter()
        .filter(|(_, distance, stand)| *stand == Stand::Waiting && *distance <= budget[0] && *distance < room[0])
        .take(slots.saturating_sub(reading))
        .map(|(tile, _, _)| *tile)
        .collect();
    (start, release)
}

#[derive(Default)]
struct LiquidsModule {
    shared: Option<Arc<Shared>>,
    /// Why the device does not draw the liquids.
    refused_device: Option<String>,
    scene: Arc<Mutex<Scene>>,
    water: Arc<Water>,
    /// The map the terrain shows, by its folder, and its distance; its WDT once read, or why it
    /// could not be, and the job reading it.
    map: Option<String>,
    distance: u32,
    wdt: Option<Result<Arc<Wdt>, String>>,
    reading_wdt: Option<JobId>,
    /// The tiles read; the reads running, by tile with the ranges the arenas had given back when
    /// they started, and by job; the tiles refused, and those refused for want of room, with the
    /// ranges given back and the camera then.
    held: HashMap<TileId, Held>,
    reading: HashMap<TileId, (JobId, u64)>,
    jobs: HashMap<JobId, TileId>,
    refused: HashSet<TileId>,
    no_room: HashMap<TileId, (u64, [f32; 2])>,
    refusals: VecDeque<String>,
    /// The liquids other modules placed, by owner, and the jobs putting them on the GPU.
    owners: HashMap<String, Owned>,
    owner_jobs: HashMap<JobId, String>,
    /// Whether the tiles or the liquids held changed since the layer and the surfaces were given
    /// them.
    changed: bool,
    told_budget: Option<Demand>,
    /// The last frame signal seen, and the camera then.
    frame: u64,
    eye: [f32; 2],
}

impl LiquidsModule {
    /// Shows the liquids of the map `map`, by its folder, or of none.
    fn show(&mut self, map: Option<String>, ctx: &mut Context) {
        for (job, _) in self.reading.values() {
            ctx.cancel(*job);
        }
        self.reading.clear();
        self.jobs.clear();
        self.held.clear();
        self.refused.clear();
        self.no_room.clear();
        self.refusals.clear();
        self.wdt = None;
        self.reading_wdt = None;
        self.map = map;
        self.changed = true;
        lock(&self.scene).publishing = Duration::ZERO;
    }

    fn refuse(&mut self, tile: TileId, reason: &str) {
        log::warn!("the liquids of the tile {} {} are not drawn: {reason}", tile.x, tile.y);
        self.refused.insert(tile);
        self.refusals.push_back(format!("Tile {} {}: {reason}", tile.x, tile.y));
        if self.refusals.len() > REFUSALS {
            self.refusals.pop_front();
        }
    }

    /// At each frame: follows the map and the distance of the terrain; then, at each frame signal,
    /// tells the budget what the tiles take and want, reads those it lets load and the arenas have
    /// room for, the nearest first, as many at once as the workers but one, lets go those left or
    /// beyond what it lets keep, and gives the layer and the surfaces the tiles held when they change.
    fn steer(&mut self, ctx: &mut Context) {
        let start = Instant::now();
        let (Some(view), Some(formats), Some(shared)) = (
            ctx.service(viewport::SERVICE),
            ctx.service(formats::SERVICE),
            self.shared.clone(),
        ) else {
            return;
        };
        let shown = ctx.editor().call("terrain.map", json!({})).ok();
        let map = shown
            .as_ref()
            .and_then(|map| Some(map.get("directory")?.as_str()?.to_owned()));
        self.distance = shown
            .as_ref()
            .and_then(|map| map.get("distance")?.as_u64())
            .map_or(DEFAULT_DISTANCE, |distance| distance as u32);
        if map != self.map {
            self.show(map, ctx);
        }
        let Some(directory) = self.map.clone() else {
            self.publish();
            return;
        };
        let wdt = match &self.wdt {
            Some(Ok(wdt)) => wdt.clone(),
            Some(Err(_)) => return,
            None => {
                if self.reading_wdt.is_none() {
                    let (formats, read) = (formats.clone(), directory.clone());
                    self.reading_wdt = Some(
                        ctx.spawn(&format!("Read the WDT of {directory} for its liquids"), move |_| {
                            formats.wdt(&read)
                        }),
                    );
                }
                return;
            }
        };
        let Some(frame) = view.wait_frame(self.frame, Duration::ZERO) else {
            return;
        };
        self.frame = frame.number;
        let eye = match ctx.read_property("viewport/camera_position") {
            Ok(PropertyValue::Vector([x, y, _])) => [x as f32, y as f32],
            _ => return,
        };
        self.eye = eye;
        let held: HashSet<TileId> = self.held.keys().copied().collect();
        let wanted: Vec<TileId> = formats::tiles_around(&wdt.tiles, eye, self.distance, &held)
            .into_iter()
            .filter(|tile| !self.refused.contains(tile))
            .collect();
        let set: HashSet<TileId> = wanted.iter().copied().collect();
        let away = |tile: TileId| (tile.distance(eye) - 0.5).max(0.0) * TILE;

        // What the tiles take and are expected to take, and how far the arenas hold them.
        let taken = |tile: &TileId| -> Option<[u64; 2]> {
            let held = self.held.get(tile)?;
            Some(held.gpu.as_ref().map_or([0, 0], |gpu| gpu.arenas))
        };
        let expected = if self.held.is_empty() {
            EXPECTED
        } else {
            let sum = self
                .held
                .keys()
                .filter_map(taken)
                .fold([0, 0], |sum, bytes| [sum[0] + bytes[0], sum[1] + bytes[1]]);
            sum.map(|bytes| bytes / self.held.len() as u64)
        };
        let sizes: Vec<(f32, Option<[u64; 2]>)> = wanted.iter().map(|tile| (away(*tile), taken(tile))).collect();
        let most = [shared.vertices.most(), shared.indices.most()];
        let (room_load, room_keep) = (
            arena::room(&sizes, most, expected, LOAD_SHARE),
            arena::room(&sizes, most, expected, 1.0),
        );

        // Told to the budget: those held in the band of their distance, those wanted the arenas
        // have room for at what they are expected to take.
        let poured: u64 = self
            .owners
            .values()
            .map(|owned| match owned {
                Owned::Held(poured) => poured.bytes,
                _ => 0,
            })
            .sum();
        let mut demand = Demand {
            fixed: shared.arrays.bytes() + shared.table.size() + poured,
            ..Demand::default()
        };
        for (tile, (distance, _)) in wanted.iter().zip(&sizes) {
            let band = Demand::band(*distance);
            match self.held.get(tile) {
                Some(held) => {
                    let bytes = held.gpu.as_ref().map_or(0, |gpu| gpu.bytes);
                    demand.held[band] += bytes;
                    demand.wanted[band] += bytes;
                }
                None if *distance < room_load => demand.wanted[band] += expected[0] + expected[1],
                None => {}
            }
        }
        let allowance = if self.told_budget.as_ref() == Some(&demand) {
            view.allowance()
        } else {
            self.told_budget = Some(demand.clone());
            view.tell_budget(ctx.module_id(), demand)
        };

        // Those left let go; then those wanted read or let go as the budget and the room say, a tile
        // refused for want of room waiting again once room may have been made.
        let given = shared.given();
        self.no_room
            .retain(|tile, refused| set.contains(tile) && !arena::room_made(refused.0, refused.1, given, eye));
        let left: Vec<TileId> = self
            .held
            .keys()
            .chain(self.reading.keys())
            .filter(|tile| !set.contains(tile))
            .copied()
            .collect();
        let stands: Vec<(TileId, f32, Stand)> = wanted
            .iter()
            .map(|tile| {
                let stand = if self.held.contains_key(tile) {
                    Stand::Held
                } else if self.reading.contains_key(tile) {
                    Stand::Reading
                } else if self.no_room.contains_key(tile) {
                    Stand::NoRoom
                } else {
                    Stand::Waiting
                };
                (*tile, away(*tile), stand)
            })
            .collect();
        let slots = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
            .max(1);
        let (to_read, release) = steps(&stands, [allowance.load, allowance.keep], [room_load, room_keep], slots);
        for tile in left.into_iter().chain(release) {
            if self.held.remove(&tile).is_some() {
                self.changed = true;
            }
            if let Some((job, _)) = self.reading.remove(&tile) {
                ctx.cancel(job);
            }
        }
        for tile in to_read {
            let (formats, shared, directory) = (formats.clone(), shared.clone(), directory.clone());
            let job = ctx.spawn(
                &format!("Read the liquids of the tile {} {} of {directory}", tile.x, tile.y),
                move |job| -> Option<Read> {
                    if job.is_cancelled() {
                        return None;
                    }
                    Some(read(&*formats, &shared, &directory, tile))
                },
            );
            self.reading.insert(tile, (job, given));
            self.jobs.insert(job, tile);
        }
        self.steer_owners(&formats, &shared, given, eye, slots, ctx);
        self.publish();
        lock(&self.scene).steering = start.elapsed();
    }

    /// Takes the liquids the other modules placed or took away since the last frame; puts those
    /// waiting on the GPU by jobs, no more than `slots` at once, those refused for want of room once
    /// room may have been made, the arenas having given back `given` ranges and the camera at `eye`.
    fn steer_owners(
        &mut self,
        formats: &Arc<dyn formats::Formats>,
        shared: &Arc<Shared>,
        given: u64,
        eye: [f32; 2],
        slots: usize,
        ctx: &mut Context,
    ) {
        let changes: Vec<(String, Option<Pouring>)> = lock(&self.water.placed).drain().collect();
        for (owner, change) in changes {
            match self.owners.remove(&owner) {
                Some(Owned::Pouring(job, ..)) => {
                    ctx.cancel(job);
                    self.owner_jobs.remove(&job);
                }
                Some(Owned::Held(_)) => self.changed = true,
                _ => {}
            }
            if let Some(liquids) = change {
                self.owners.insert(owner, Owned::Waiting(Arc::new(liquids), None));
            }
        }
        let ready: Vec<String> = self
            .owners
            .iter()
            .filter(|(_, owned)| {
                matches!(owned, Owned::Waiting(_, refused)
                    if refused.is_none_or(|(then, at)| arena::room_made(then, at, given, eye)))
            })
            .map(|(owner, _)| owner.clone())
            .take(slots.saturating_sub(self.owner_jobs.len()))
            .collect();
        for owner in ready {
            let Some(Owned::Waiting(liquids, _)) = self.owners.remove(&owner) else {
                continue;
            };
            let (formats, shared, poured) = (formats.clone(), shared.clone(), liquids.clone());
            let job = ctx.spawn(
                &format!("Put the liquids of {owner} on the GPU"),
                move |job| -> Option<Result<PouredOwner, Refusal>> {
                    if job.is_cancelled() {
                        return None;
                    }
                    Some(pour(&*formats, &shared, &poured))
                },
            );
            self.owner_jobs.insert(job, owner.clone());
            self.owners.insert(owner, Owned::Pouring(job, given, liquids));
        }
    }

    /// Gives the layer and the surfaces the tiles held, when they changed: the grids of their water
    /// shared, not copied.
    fn publish(&mut self) {
        if !self.changed {
            return;
        }
        let start = Instant::now();
        self.changed = false;
        let poured: Vec<&PouredOwner> = self
            .owners
            .values()
            .filter_map(|owned| match owned {
                Owned::Held(poured) => Some(poured),
                _ => None,
            })
            .collect();
        let mut surfaces = surfaces(&self.held);
        for poured in &poured {
            surfaces.merge(&poured.surfaces);
        }
        *lock(&self.water.surfaces) = Arc::new(surfaces);
        let mut scene = lock(&self.scene);
        scene.tiles = self.held.values().filter_map(|held| held.gpu.clone()).collect();
        scene.placed = poured
            .iter()
            .flat_map(|poured| poured.liquids.iter().cloned())
            .collect();
        scene.publishing = scene.publishing.max(start.elapsed());
    }
}

impl Module for LiquidsModule {
    fn register(&mut self, reg: &mut Registrar) {
        let water: liquids::Handle = self.water.clone();
        reg.provide(liquids::SERVICE, water)
            .panel("liquids", "Liquids", DockArea::Right);
    }

    fn init(&mut self, ctx: &mut Context) {
        let (Some(view), Some(gpu)) = (ctx.service(viewport::SERVICE), ctx.gpu().cloned()) else {
            log::info!("no 3D view: the liquids are not drawn");
            return;
        };
        match Shared::new(&gpu, &view.target()) {
            Ok(shared) => {
                let shared = Arc::new(shared);
                view.add_layer(
                    ctx.module_id(),
                    Box::new(LiquidsLayer::new(shared.clone(), self.scene.clone())),
                );
                self.shared = Some(shared);
            }
            Err(reason) => {
                log::warn!("{reason}");
                self.refused_device = Some(reason);
            }
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, _ctx: &mut Context) {
        if let Some(reason) = &self.refused_device {
            ui.colored_label(ui.visuals().warn_fg_color, reason);
            return;
        }
        let Some(map) = &self.map else {
            ui.label("No map shown by the terrain.");
            return;
        };
        if let Some(Err(reason)) = &self.wdt {
            ui.colored_label(ui.visuals().warn_fg_color, format!("{map}: {reason}"));
        }
        let with = self.held.values().filter(|held| held.gpu.is_some()).count();
        let surfaces = lock(&self.water.surfaces).len();
        ui.label(format!(
            "{map}: {} tiles read within {} tiles of the camera, {with} with liquids, {} reading, {} waiting \
             for room, {} refused; {surfaces} tiles of water under the surfaces given",
            self.held.len(),
            self.distance,
            self.reading.len(),
            self.no_room.len(),
            self.refused.len()
        ));
        for refusal in &self.refusals {
            ui.colored_label(ui.visuals().warn_fg_color, refusal);
        }
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown: the tiles are steered here.
        self.steer(ctx);
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if let Some(owner) = self.owner_jobs.remove(&job) {
            let Some(Owned::Pouring(pouring, given, liquids)) = self.owners.get(&owner) else {
                return;
            };
            if *pouring != job {
                return;
            }
            let (given, liquids) = (*given, liquids.clone());
            let refused = |reason: &str| {
                log::warn!("the liquids of {owner} are not drawn: {reason}");
                Owned::Refused
            };
            let owned = match outcome {
                JobOutcome::Panicked(message) => refused(&message),
                JobOutcome::Cancelled => return,
                outcome => match outcome.take::<Option<Result<PouredOwner, Refusal>>>() {
                    Some(Some(Ok(poured))) => {
                        self.changed = true;
                        Owned::Held(poured)
                    }
                    Some(Some(Err(Refusal::NoRoom(_)))) => Owned::Waiting(liquids, Some((given, self.eye))),
                    Some(Some(Err(Refusal::Failed(reason)))) => refused(&reason),
                    _ => return,
                },
            };
            self.owners.insert(owner, owned);
            return;
        }
        if self.reading_wdt == Some(job) {
            self.reading_wdt = None;
            self.wdt = Some(match outcome {
                JobOutcome::Panicked(message) => Err(message),
                outcome => match outcome.take::<Result<Arc<Wdt>, String>>() {
                    Some(read) => read,
                    None => return,
                },
            });
            if let Some(Err(reason)) = &self.wdt {
                log::warn!("the liquids of the map shown are not drawn: {reason}");
            }
            return;
        }
        let Some(tile) = self.jobs.remove(&job) else {
            return;
        };
        let Some((_, given)) = self.reading.get(&tile).copied().filter(|(reading, _)| *reading == job) else {
            return;
        };
        self.reading.remove(&tile);
        match outcome {
            JobOutcome::Panicked(message) => self.refuse(tile, &message),
            JobOutcome::Cancelled => {}
            outcome => match outcome.take::<Option<Read>>() {
                Some(Some(Ok(held))) => {
                    self.held.insert(tile, held);
                    self.changed = true;
                }
                Some(Some(Err(Refusal::NoRoom(_)))) => {
                    self.no_room.insert(tile, (given, self.eye));
                }
                Some(Some(Err(Refusal::Failed(reason)))) => self.refuse(tile, &reason),
                _ => {}
            },
        }
    }
}

uniwow_api::export_module!(LiquidsModule::default());
