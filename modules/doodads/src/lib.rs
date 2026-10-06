//! The doodads of the tiles of the map the terrain shows, around the camera of the 3D view, within
//! a distance of their own: the placements of each tile read by a job, the nearest first, and its
//! doodads placed through the service `models`, an owner a tile. Nothing is changed: no undo
//! entry, no file written.

mod placing;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use uniwow_api::formats::{self, TileId, Wdt};
use uniwow_api::serde_json::json;
use uniwow_api::{Context, DockArea, JobId, JobOutcome, Module, PropertyValue, Registrar, egui, log, models, viewport};

use placing::Placing;

/// The setting of how far around the camera tiles are placed, in tiles.
const DISTANCE: &str = "distance";
const DEFAULT_DISTANCE: u32 = 2;
const DISTANCES: [u32; 2] = [1, 4];
/// The refusals of tiles the panel keeps.
const REFUSALS: usize = 8;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A read of the placements of a tile: whether it was placed, or why it could not be read.
type Read = Result<bool, String>;

#[derive(Default)]
struct DoodadsModule {
    distance: u32,
    placing: Arc<Mutex<Placing>>,
    /// The map the terrain shows, by its folder; its WDT once read, or why it could not be, and
    /// the job reading it.
    map: Option<String>,
    wdt: Option<Result<Arc<Wdt>, String>>,
    reading_wdt: Option<JobId>,
    /// The tiles told wanted to `placing`, the reads running, by tile and by job, the tiles placed
    /// and those refused.
    told: HashSet<TileId>,
    reading: HashMap<TileId, JobId>,
    jobs: HashMap<JobId, TileId>,
    placed: HashSet<TileId>,
    refused: HashSet<TileId>,
    refusals: VecDeque<String>,
    /// The jobs taking away the tiles left.
    settling: HashSet<JobId>,
    /// The last frame signal seen.
    frame: u64,
    /// When the map or the distance changed, until the tiles wanted are all placed; then what that
    /// took and how many they are.
    since: Option<Instant>,
    took: Option<(Duration, usize)>,
    /// What `placing` counted last: the tiles placed, the doodads placed and those listed.
    counts: [usize; 3],
}

impl DoodadsModule {
    /// Shows the doodads of the map `map`, by its folder, or of none: what the map before held
    /// taken away.
    fn show(&mut self, map: Option<String>, models: &dyn models::Models, ctx: &mut Context) {
        for job in self.reading.values() {
            ctx.cancel(*job);
        }
        self.reading.clear();
        self.jobs.clear();
        self.told.clear();
        self.placed.clear();
        self.refused.clear();
        self.refusals.clear();
        self.wdt = None;
        self.reading_wdt = None;
        self.took = None;
        self.since = map.is_some().then(Instant::now);
        lock(&self.placing).want(models, map.as_deref().unwrap_or_default(), HashSet::new());
        self.map = map;
    }

    /// Records the refusal of the tile `tile`.
    fn refuse(&mut self, tile: TileId, reason: &str) {
        log::warn!("the doodads of the tile {} {} are not placed: {reason}", tile.x, tile.y);
        self.refused.insert(tile);
        self.refusals.push_back(format!("Tile {} {}: {reason}", tile.x, tile.y));
        if self.refusals.len() > REFUSALS {
            self.refusals.pop_front();
        }
    }

    /// At each frame: follows the map the terrain shows; then, at each frame signal, tells
    /// `placing` the tiles the camera wants, cancels the reads of those it left, has those placed
    /// taken away, and starts the reads of the nearest it wants, as many at once as the workers but
    /// one.
    fn steer(&mut self, ctx: &mut Context) {
        let (Some(view), Some(models), Some(formats)) = (
            ctx.service(viewport::SERVICE),
            ctx.service(models::SERVICE),
            ctx.service(formats::SERVICE),
        ) else {
            return;
        };
        let map = ctx
            .editor()
            .call("terrain.map", json!({}))
            .ok()
            .and_then(|map| Some(map.get("directory")?.as_str()?.to_owned()));
        if map != self.map {
            self.show(map, &*models, ctx);
        }
        let Some(directory) = self.map.clone() else {
            return;
        };
        let wdt = match &self.wdt {
            Some(Ok(wdt)) => wdt.clone(),
            Some(Err(_)) => return,
            None => {
                if self.reading_wdt.is_none() {
                    let (formats, read) = (formats.clone(), directory.clone());
                    self.reading_wdt = Some(
                        ctx.spawn(&format!("Read the WDT of {directory} for its doodads"), move |_| {
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
        let wanted = formats::tiles_around(&wdt.tiles, eye, self.distance, &self.placed);
        let set: HashSet<TileId> = wanted.iter().copied().collect();
        if set != self.told {
            // A job placing a tile holds the lock: told at a frame to come.
            let Ok(mut placing) = self.placing.try_lock() else {
                return;
            };
            let leaving = placing.want(&*models, &directory, set.clone());
            drop(placing);
            for tile in self.told.difference(&set) {
                if let Some(job) = self.reading.remove(tile) {
                    ctx.cancel(job);
                }
                self.placed.remove(tile);
            }
            if leaving {
                let (placing, models) = (self.placing.clone(), models.clone());
                let job = ctx.spawn("Take away the doodads of the tiles left", move |_| {
                    lock(&placing).settle(&*models)
                });
                self.settling.insert(job);
            }
            self.told = set;
        }
        let slots = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
            .max(1);
        for tile in &wanted {
            if self.reading.len() >= slots {
                break;
            }
            if self.placed.contains(tile) || self.reading.contains_key(tile) || self.refused.contains(tile) {
                continue;
            }
            let (formats, models, placing, directory) =
                (formats.clone(), models.clone(), self.placing.clone(), directory.clone());
            let tile = *tile;
            let label = format!("Place the doodads of the tile {} {} of {directory}", tile.x, tile.y);
            let job = ctx.spawn(&label, move |job| -> Read {
                let read = formats
                    .placements(&directory, tile.x, tile.y)?
                    .ok_or("named by its WDT, but not read")?;
                if job.is_cancelled() {
                    return Ok(false);
                }
                let instances = placing::instances(&read, &*models);
                if job.is_cancelled() {
                    return Ok(false);
                }
                Ok(lock(&placing).place(&*models, &directory, tile, instances))
            });
            self.reading.insert(tile, job);
            self.jobs.insert(job, tile);
        }
        let done = self.reading.is_empty()
            && wanted
                .iter()
                .all(|tile| self.placed.contains(tile) || self.refused.contains(tile));
        if done && let Some(since) = self.since.take() {
            let took = since.elapsed();
            self.took = Some((took, wanted.len()));
            log::info!(
                "{directory}: the doodads of the {} tiles within {} tiles placed in {:.1} s",
                wanted.len(),
                self.distance,
                took.as_secs_f32()
            );
        }
        if let Ok(placing) = self.placing.try_lock() {
            self.counts = placing.counts();
        }
    }
}

impl Module for DoodadsModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("doodads", "Doodads", DockArea::Right);
    }

    fn init(&mut self, ctx: &mut Context) {
        self.distance = ctx
            .setting(DISTANCE)
            .and_then(|value| value.as_u64())
            .map_or(DEFAULT_DISTANCE, |value| {
                value.clamp(u64::from(DISTANCES[0]), u64::from(DISTANCES[1])) as u32
            });
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if ctx.service(models::SERVICE).is_none() || ctx.service(viewport::SERVICE).is_none() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "No 3D view or no service models: the doodads are not drawn.",
            );
            return;
        }
        let Some(map) = &self.map else {
            ui.label("No map shown by the terrain.");
            return;
        };
        if let Some(Err(reason)) = &self.wdt {
            ui.colored_label(ui.visuals().warn_fg_color, format!("{map}: {reason}"));
        }
        let [tiles, placed, listed] = self.counts;
        ui.label(format!(
            "{map}: {tiles} tiles placed, {} reading, {} refused; {placed} doodads placed of {listed} listed, \
             those listed by several tiles placed once",
            self.reading.len(),
            self.refused.len()
        ));
        if let Some((took, tiles)) = self.took {
            ui.label(format!(
                "The doodads of all {tiles} tiles within reach placed in {:.1} s",
                took.as_secs_f32()
            ));
        }
        ui.horizontal(|ui| {
            ui.label("Distance (tiles)");
            if ui
                .add(egui::DragValue::new(&mut self.distance).range(DISTANCES[0]..=DISTANCES[1]))
                .changed()
            {
                ctx.set_setting(DISTANCE, json!(self.distance));
                self.since = Some(Instant::now());
                self.took = None;
            }
        });
        for refusal in &self.refusals {
            ui.colored_label(ui.visuals().warn_fg_color, refusal);
        }
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown: the tiles are steered here.
        self.steer(ctx);
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if self.settling.remove(&job) {
            if let JobOutcome::Panicked(message) = outcome {
                log::error!("the doodads of the tiles left could not be taken away: {message}");
            }
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
                log::warn!("the doodads of the map shown are not placed: {reason}");
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
            outcome => match outcome.take::<Read>() {
                Some(Ok(true)) => {
                    self.placed.insert(tile);
                }
                Some(Err(reason)) => self.refuse(tile, &reason),
                _ => {}
            },
        }
    }
}

uniwow_api::export_module!(DoodadsModule::default());
