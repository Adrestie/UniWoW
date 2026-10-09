//! The light of the map the terrain shows, at the place of the camera on it and at the hour of the
//! settings, from the tables of the client (`light`): computed at each frame and said in the panel,
//! not yet given to the view. The tables are read by a job once the client's archives are open, and
//! again whenever they change.

mod light;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use uniwow_api::formats;
use uniwow_api::serde_json::json;
use uniwow_api::vfs::{self, VfsState};
use uniwow_api::{Context, DockArea, JobId, JobOutcome, Module, PropertyValue, Registrar, SettingSpec, egui, log};

use light::{COLOURS, DAY, Mixed, NUMBERS, Tables};

/// The settings: the hour, in minutes from midnight; how many minutes of the game pass in a second;
/// whether the local lights are mixed in.
const HOUR: &str = "hour";
const SPEED: &str = "speed";
const LOCAL: &str = "local_lights";

/// The names of the bands of colours and of numbers, by their place.
const COLOUR_NAMES: [&str; COLOURS] = [
    "Diffuse",
    "Ambient",
    "Sky, top",
    "Sky, middle",
    "Sky, towards the horizon",
    "Sky, over the horizon",
    "Horizon",
    "Fog",
    "Shadows",
    "Sun",
    "Halo of the sun",
    "Edge of the clouds",
    "Clouds",
    "Clouds, second layer",
    "Ocean, shallow",
    "Ocean, deep",
    "River, shallow",
    "River, deep",
];
const NUMBER_NAMES: [&str; NUMBERS] = [
    "Fog, end (yards)",
    "Fog, start (share of its end)",
    "Glow through the clouds",
    "Density of the clouds",
    "Unknown",
    "Unknown",
];

/// The settings of the category *Light*.
fn settings() -> Vec<SettingSpec> {
    vec![
        SettingSpec::integer(HOUR, "Hour, in minutes from midnight", [0, 1439], 720),
        SettingSpec::integer(SPEED, "Minutes of the game in a second", [0, 1440], 0),
        SettingSpec::integer(LOCAL, "Local lights mixed in (1) or not (0)", [0, 1], 1),
    ]
}

/// The hour, in minutes from midnight, `from` turned for `seconds` at `speed` minutes of the game in
/// a second.
fn minutes(from: f64, speed: i64, seconds: f64) -> f64 {
    (from + speed as f64 * seconds).rem_euclid(1440.0)
}

/// The same, in half-minutes.
fn half_minutes(from: f64, speed: i64, seconds: f64) -> f32 {
    (minutes(from, speed, seconds) * 2.0) as f32 % DAY
}

/// What the panel says of the light last computed: the map, by its id and name, the place, the hour
/// in half-minutes, and the light, none when the tables give none.
struct Shown {
    map: u32,
    name: String,
    place: [f32; 2],
    time: f32,
    light: Option<Mixed>,
}

#[derive(Default)]
struct LightingModule {
    /// The client's archives as open, by their count and that of their files, the tables read from
    /// them or why they could not be, and the job reading them.
    client: Option<(usize, usize)>,
    tables: Option<Result<Arc<Tables>, String>>,
    reading: Option<JobId>,
    /// The hour and the speed set, the hour in minutes it turns from, and since when.
    set: Option<(i64, i64, f64, Instant)>,
    shown: Option<Shown>,
}

impl LightingModule {
    /// The hour, in half-minutes: that of the settings, turned at their speed; when the speed alone
    /// changes, on from the hour reached.
    fn time(&mut self, hour: i64, speed: i64) -> f32 {
        if let Some((kept_hour, kept_speed, from, since)) = self.set
            && kept_hour == hour
        {
            let seconds = since.elapsed().as_secs_f64();
            if kept_speed == speed {
                return half_minutes(from, speed, seconds);
            }
            self.set = Some((hour, speed, minutes(from, kept_speed, seconds), Instant::now()));
        } else {
            self.set = Some((hour, speed, hour as f64, Instant::now()));
        }
        let (_, _, from, _) = self.set.expect("set");
        half_minutes(from, speed, 0.0)
    }

    /// At each frame: the tables read once the client's archives are open, again once they change;
    /// the light of the map shown at the place of the camera and at the hour, for the panel.
    fn steer(&mut self, ctx: &mut Context) {
        let (Some(formats), Some(files)) = (ctx.service(formats::SERVICE), ctx.service(vfs::SERVICE)) else {
            return;
        };
        let client = match files.state() {
            VfsState::Ready { archives, files } => Some((archives, files)),
            _ => None,
        };
        if client != self.client {
            if let Some(job) = self.reading.take() {
                ctx.cancel(job);
            }
            self.client = client;
            self.tables = None;
            self.shown = None;
        }
        if self.client.is_none() {
            return;
        }
        let tables = match &self.tables {
            Some(Ok(tables)) => tables.clone(),
            Some(Err(_)) => return,
            None => {
                if self.reading.is_none() {
                    self.reading = Some(ctx.spawn("Read the tables of the lights", move |_| {
                        Ok::<_, String>(Arc::new(Tables::new(
                            &formats.lights()?,
                            &formats.light_params()?,
                            &formats.light_colours()?,
                            &formats.light_numbers()?,
                        )))
                    }));
                }
                return;
            }
        };
        let specs = settings();
        let [hour, speed, local] = [HOUR, SPEED, LOCAL].map(|key| {
            specs
                .iter()
                .find(|spec| spec.key == key)
                .expect("declared")
                .value(ctx.setting(key).as_ref())
        });
        let time = self.time(hour, speed);
        let map = ctx
            .editor()
            .call("terrain.map", json!({}))
            .ok()
            .and_then(|map| Some((map.get("id")?.as_u64()? as u32, map.get("name")?.as_str()?.to_owned())));
        let Some((map, name)) = map else {
            self.shown = None;
            return;
        };
        let Ok(PropertyValue::Vector([x, y, _])) = ctx.read_property("viewport/camera_position") else {
            return;
        };
        let place = [x as f32, y as f32];
        self.shown = Some(Shown {
            map,
            name,
            place,
            time,
            light: tables.light_at(map, place, time, 0, local == 1),
        });
    }
}

/// A colour from 0 to 1 as the panel shows it.
fn colour32(colour: [f32; 3]) -> egui::Color32 {
    let [r, g, b] = colour.map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8);
    egui::Color32::from_rgb(r, g, b)
}

impl Module for LightingModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("lighting", "Lighting", DockArea::Right)
            .settings("Light", settings());
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if ctx.service(formats::SERVICE).is_none() {
            ui.colored_label(ui.visuals().warn_fg_color, "No service formats: no light is read.");
            return;
        }
        if self.client.is_none() {
            ui.label("Waiting for the client's archives.");
            return;
        }
        match &self.tables {
            None => {
                ui.label("Reading the tables of the lights…");
                return;
            }
            Some(Err(reason)) => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("The tables of the lights: {reason}"),
                );
                return;
            }
            Some(Ok(_)) => {}
        }
        let Some(shown) = &self.shown else {
            ui.label("No map shown by the terrain.");
            return;
        };
        let minutes = (shown.time / 2.0) as u32;
        ui.label(format!(
            "{} ({}), at {:.0}, {:.0}, at {:02}:{:02}; not yet given to the view",
            shown.name,
            shown.map,
            shown.place[0],
            shown.place[1],
            minutes / 60,
            minutes % 60
        ));
        let Some(light) = &shown.light else {
            ui.colored_label(ui.visuals().warn_fg_color, "No light for this map.");
            return;
        };
        let used: Vec<String> = light
            .used
            .iter()
            .enumerate()
            .map(|(at, (id, weight))| {
                let kind = match (at, light.fallback) {
                    (0, false) => " (global)",
                    (0, true) => " (the light 1, the map having no global light)",
                    _ => "",
                };
                format!("{id}{kind} {weight:.2}")
            })
            .collect();
        ui.label(format!("Lights mixed, by their weights: {}", used.join(", ")));
        egui::Grid::new("lighting colours").striped(true).show(ui, |ui| {
            for (name, colour) in COLOUR_NAMES.iter().zip(&light.values.colours) {
                ui.label(*name);
                match colour {
                    Some(colour) => {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, colour32(*colour));
                            let [r, g, b] = colour.map(|channel| (channel * 255.0).round());
                            ui.label(format!("{r}, {g}, {b}"));
                        });
                    }
                    None => {
                        ui.label("none");
                    }
                }
                ui.end_row();
            }
            for (at, (name, number)) in NUMBER_NAMES.iter().zip(&light.values.numbers).enumerate() {
                ui.label(*name);
                // The end of the fog is stored in 36ths of a yard.
                let shown = number.map(|number| if at == 0 { number / 36.0 } else { number });
                ui.label(shown.map_or("none".to_owned(), |number| format!("{number:.2}")));
                ui.end_row();
            }
            let [river, ocean] = [light.values.river_alphas, light.values.ocean_alphas];
            ui.label("Alphas, river and ocean");
            ui.label(format!(
                "{:.2}, {:.2}; {:.2}, {:.2}",
                river[0], river[1], ocean[0], ocean[1]
            ));
            ui.end_row();
            ui.label("Glow");
            ui.label(format!("{:.2}", light.values.glow));
            ui.end_row();
        });
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown.
        self.steer(ctx);
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if self.reading != Some(job) {
            return;
        }
        self.reading = None;
        self.tables = Some(match outcome {
            JobOutcome::Panicked(message) => Err(message),
            // Cancelled from the jobs: not read again before the client's archives change.
            JobOutcome::Cancelled => Err("their reading was cancelled".to_owned()),
            outcome => match outcome.take::<Result<Arc<Tables>, String>>() {
                Some(read) => read,
                None => return,
            },
        });
        if let Some(Err(reason)) = &self.tables {
            log::warn!("the tables of the lights are not read: {reason}");
        }
    }
}

uniwow_api::export_module!(LightingModule::default());
