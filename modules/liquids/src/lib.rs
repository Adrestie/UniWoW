//! The liquids of the tiles of the map the terrain shows, around the camera of the 3D view within
//! the distance of the terrain: the layers of each tile read by a job, the nearest first, their
//! meshes put on the GPU and drawn by the layer of the module; the surfaces of their water given
//! through the service `liquids`, by which the other layers tell what they blend beyond the water
//! from what is on the eye's side. Nothing is changed: no undo entry, no file written.

mod gpu;
mod layer;
mod mesh;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use uniwow_api::formats::{self, TILE, TileId, Wdt};
use uniwow_api::liquids::{self, Liquids, Surfaces};
use uniwow_api::serde_json::json;
use uniwow_api::viewport::Demand;
use uniwow_api::{Context, DockArea, JobId, JobOutcome, Module, PropertyValue, Registrar, egui, log, viewport};

use gpu::{Shared, TileGpu};
use layer::{LiquidsLayer, Scene};

/// The distance of the terrain, in tiles, until it says it.
const DEFAULT_DISTANCE: u32 = 3;
/// The refusals of tiles the panel keeps.
const REFUSALS: usize = 8;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The surfaces of the water held, given to the other modules.
#[derive(Default)]
struct Water {
    surfaces: Mutex<Arc<Surfaces>>,
}

impl Liquids for Water {
    fn surfaces(&self) -> Arc<Surfaces> {
        lock(&self.surfaces).clone()
    }
}

/// The liquids of a tile read: on the GPU, none where it has none; the tiles of its water with
/// their heights.
struct Held {
    gpu: Option<Arc<TileGpu>>,
    surfaces: Vec<([i32; 2], f32)>,
}

/// A read of a tile, or why it could not be read.
type Read = Result<Held, String>;

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
    Ok(Held {
        gpu: gpu::upload(shared, &meshes)?.map(Arc::new),
        surfaces: meshes.surfaces,
    })
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
    /// The tiles read, the reads running, by tile and by job, the tiles refused.
    held: HashMap<TileId, Held>,
    reading: HashMap<TileId, JobId>,
    jobs: HashMap<JobId, TileId>,
    refused: HashSet<TileId>,
    refusals: VecDeque<String>,
    /// Whether the tiles held changed since the layer and the surfaces were given them.
    changed: bool,
    told_budget: Option<Demand>,
    /// The last frame signal seen.
    frame: u64,
}

impl LiquidsModule {
    /// Shows the liquids of the map `map`, by its folder, or of none.
    fn show(&mut self, map: Option<String>, ctx: &mut Context) {
        for job in self.reading.values() {
            ctx.cancel(*job);
        }
        self.reading.clear();
        self.jobs.clear();
        self.held.clear();
        self.refused.clear();
        self.refusals.clear();
        self.wdt = None;
        self.reading_wdt = None;
        self.map = map;
        self.changed = true;
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
    /// reads the tiles the camera wants, the nearest first, as many at once as the workers but one,
    /// lets go those it left, gives the layer and the surfaces the tiles held when they change, and
    /// tells the budget of the view what they take.
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
        let held: HashSet<TileId> = self.held.keys().copied().collect();
        let wanted = formats::tiles_around(&wdt.tiles, eye, self.distance, &held);
        let set: HashSet<TileId> = wanted.iter().copied().collect();
        let left: Vec<TileId> = self.held.keys().filter(|tile| !set.contains(tile)).copied().collect();
        for tile in left {
            self.held.remove(&tile);
            self.changed = true;
        }
        let stale: Vec<TileId> = self
            .reading
            .keys()
            .filter(|tile| !set.contains(tile))
            .copied()
            .collect();
        for tile in stale {
            if let Some(job) = self.reading.remove(&tile) {
                ctx.cancel(job);
            }
        }
        let slots = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
            .max(1);
        for tile in &wanted {
            if self.reading.len() >= slots {
                break;
            }
            if self.held.contains_key(tile) || self.reading.contains_key(tile) || self.refused.contains(tile) {
                continue;
            }
            let (formats, shared, directory, tile) = (formats.clone(), shared.clone(), directory.clone(), *tile);
            let job = ctx.spawn(
                &format!("Read the liquids of the tile {} {} of {directory}", tile.x, tile.y),
                move |job| -> Option<Read> {
                    if job.is_cancelled() {
                        return None;
                    }
                    Some(read(&*formats, &shared, &directory, tile))
                },
            );
            self.reading.insert(tile, job);
            self.jobs.insert(job, tile);
        }
        // What the tiles held take, each in the band of its distance.
        let mut demand = Demand {
            fixed: shared.arrays.bytes() + shared.table.size(),
            ..Demand::default()
        };
        for (tile, held) in &self.held {
            if let Some(gpu) = &held.gpu {
                let band = Demand::band((tile.distance(eye) - 0.5).max(0.0) * TILE);
                demand.held[band] += gpu.bytes;
                demand.wanted[band] += gpu.bytes;
            }
        }
        if self.told_budget.as_ref() != Some(&demand) {
            view.tell_budget(ctx.module_id(), demand.clone());
            self.told_budget = Some(demand);
        }
        self.publish();
        lock(&self.scene).steering = start.elapsed();
    }

    /// Gives the layer and the surfaces the tiles held, when they changed.
    fn publish(&mut self) {
        if !self.changed {
            return;
        }
        self.changed = false;
        let mut surfaces = Surfaces::default();
        for held in self.held.values() {
            for (cell, height) in &held.surfaces {
                surfaces.add(*cell, *height);
            }
        }
        *lock(&self.water.surfaces) = Arc::new(surfaces);
        lock(&self.scene).tiles = self.held.values().filter_map(|held| held.gpu.clone()).collect();
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
            "{map}: {} tiles read within {} tiles of the camera, {with} with liquids, {} reading, {} refused; \
             {surfaces} tiles of water under the surfaces given",
            self.held.len(),
            self.distance,
            self.reading.len(),
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
        if self.reading.get(&tile) != Some(&job) {
            return;
        }
        self.reading.remove(&tile);
        match outcome {
            JobOutcome::Panicked(message) => self.refuse(tile, &message),
            JobOutcome::Cancelled => {}
            outcome => match outcome.take::<Option<Read>>() {
                Some(Some(Ok(held))) => {
                    self.held.insert(tile, held);
                    self.changed = true;
                }
                Some(Some(Err(reason))) => self.refuse(tile, &reason),
                _ => {}
            },
        }
    }
}

uniwow_api::export_module!(LiquidsModule::default());
