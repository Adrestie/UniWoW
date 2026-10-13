//! The light of the map the terrain shows, at the place of the camera on it and at the hour of the
//! settings, from the tables of the client and the zones of light of its Wow.exe (`light`), the eye
//! in a liquid or not as the services `buildings` and `liquids` say: computed at each frame, given
//! to the view (`Viewport::set_light`) and said in the panel; taken back when the module stops.
//! They are read by a job once the client's archives are open, and again whenever they change. The
//! models of the sky the lights name, and that of the building whose group the eye is in, are
//! placed at the eye through the service `models`, which draws them around it.

mod light;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use uniwow_api::formats::FileRef;
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::models::{self, Geosets, Instance, Look, LookState, Motion};
use uniwow_api::serde_json::json;
use uniwow_api::vfs::{self, VfsState};
use uniwow_api::viewport::{self, MapLight};
use uniwow_api::{Context, DockArea, JobId, JobOutcome, Module, PropertyValue, Registrar, SettingSpec, egui, log};
use uniwow_api::{buildings, formats, liquids};

use light::{COLOURS, DAY, FOLLOWS_THE_HOUR, Immersion, Mixed, NUMBERS, Tables, WHOLE_INSIDE};

/// The settings: the hour, in minutes from midnight; how many minutes of the game pass in a second;
/// whether the zones of light and the local lights are mixed in; whether the fog is the game's or
/// the editor's; the far clip of the game, in yards, which bounds its fog (its `farclip`, within
/// the bounds Wow.exe gives it); whether the hour is the server's, through the live world, when it
/// is known.
const HOUR: &str = "hour";
const SPEED: &str = "speed";
const LOCAL: &str = "local_lights";
const FOG: &str = "game_fog";
const FAR: &str = "far_clip";
const SERVER_HOUR: &str = "server_hour";

/// The id of the module, as its manifest gives it: the owner of the light it gives the view.
const OWNER: &str = "lighting";

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
        SettingSpec::integer(
            LOCAL,
            "Zones of light and local lights mixed in (1) or not (0)",
            [0, 1],
            1,
        ),
        SettingSpec::integer(FOG, "Fog of the game (1) or of the editor (0)", [0, 1], 1),
        SettingSpec::integer(FAR, "Far clip of the game, in yards (its farclip)", [184, 2000], 1277),
        SettingSpec::integer(
            SERVER_HOUR,
            "Hour of the server through the live world when it is known (1), or of the settings (0)",
            [0, 1],
            0,
        ),
    ]
}

/// The hour of the server, in half-minutes, as the live world gives it (`live-world.time`), or why
/// it is not known.
fn server_time(ctx: &Context) -> Result<f32, String> {
    let answer = ctx
        .editor()
        .call("live-world.time", json!({}))
        .map_err(|why| format!("no live world ({why})"))?;
    match answer.get("time").and_then(|time| time.as_u64()) {
        Some(seconds) => Ok((seconds as f32 / 30.0) % DAY),
        None => Err(answer
            .get("reason")
            .and_then(|reason| reason.as_str())
            .unwrap_or("not known")
            .to_owned()),
    }
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

/// The tables read, and why the zones of light, the lights of the liquids and the models of the sky
/// could not be, if so.
type Read = (Arc<Tables>, Option<String>, Option<String>, Option<String>);

/// A model of the sky drawn around the eye: its model, its alpha, its share of the day when its
/// animation follows the hour, and whose it is, a light's (`LightSkybox.dbc`, by its id) or the
/// building's (`MOSB`).
#[derive(Clone, Debug, PartialEq)]
struct Sky {
    model: FileRef,
    alpha: f32,
    share: Option<f32>,
    light: Option<u32>,
}

/// The id of the instance of the model of the sky of a building, apart from those of the lights,
/// their ids in `LightSkybox.dbc`.
const BUILDING_SKY: u64 = 1 << 32;

impl Sky {
    /// Its instance at `eye`, where the models find it in sight, drawn around the eye wherever it
    /// stands, as the client places a model of the sky on the camera (WebWowViewerCpp, `M2Object::
    /// update`).
    fn instance(&self, look: models::LookId, eye: Vec3) -> Instance {
        Instance {
            id: self.light.map_or(BUILDING_SKY, u64::from),
            look,
            transform: Mat4::from_translation(eye),
            alpha: self.alpha,
            motion: self.share.map_or(Motion::Standing, Motion::Share),
            interior: None,
        }
    }

    /// Its look: its model, drawn as the sky.
    fn look(&self) -> Look {
        Look {
            model: self.model.clone(),
            textures: Vec::new(),
            geosets: Geosets::Sky,
        }
    }
}

/// What the panel says of the light last computed: the map, by its id and name, the place, the hour
/// in half-minutes, the liquid the eye is in and where that puts it, how much the fog of the
/// building whose room the eye is in weighs, and the light, none when the tables give none, with
/// what the view was given.
struct Shown {
    map: u32,
    name: String,
    place: [f32; 2],
    time: f32,
    liquid: Option<u16>,
    immersion: Immersion,
    /// The hour of the server, when asked for: none when it was taken, why not otherwise.
    server_hour: Option<Result<(), String>>,
    building_fog: Option<f32>,
    light: Option<Mixed>,
    given: Option<MapLight>,
    /// The models of the sky drawn around the eye, in their order, at `eye`.
    skies: Vec<Sky>,
    eye: Vec3,
}

#[derive(Default)]
struct LightingModule {
    /// The client's archives as open, by their count and that of their files, the tables read from
    /// them or why they could not be, why the zones of light could not be, and the job reading them.
    client: Option<(usize, usize)>,
    tables: Option<Result<Arc<Tables>, String>>,
    zones_unread: Option<String>,
    /// Why the lights of the types of liquid could not be read, the tables read without them.
    liquids_unread: Option<String>,
    /// Why the models of the sky could not be read, the tables read without them.
    skyboxes_unread: Option<String>,
    /// Whether models of the sky were placed last, taken back once there are none, and the service
    /// they were placed through, cleared when the module stops.
    skies_placed: bool,
    models: Option<models::Handle>,
    reading: Option<JobId>,
    /// The hour and the speed set, the hour in minutes it turns from, and since when.
    set: Option<(i64, i64, f64, Instant)>,
    shown: Option<Shown>,
    /// The view the light is given to, taken back from it when the module stops.
    view: Option<viewport::Handle>,
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

    /// At each frame: the light of the map shown given to the view, none while there is none; its
    /// models of the sky placed at the eye.
    fn steer(&mut self, ctx: &mut Context) {
        let given = self.light(ctx);
        self.view = ctx.service(viewport::SERVICE);
        if let Some(view) = &self.view {
            view.set_light(OWNER, given);
        }
        self.place_skies(ctx);
    }

    /// The models of the sky of the light last computed placed at the eye through the service
    /// `models`, at each frame while there are any, taken back once there are none.
    fn place_skies(&mut self, ctx: &mut Context) {
        self.models = ctx.service(models::SERVICE);
        let Some(models) = &self.models else {
            return;
        };
        let skies = self.shown.as_ref().map_or(&[][..], |shown| &shown.skies[..]);
        if skies.is_empty() && !self.skies_placed {
            return;
        }
        let eye = self.shown.as_ref().map_or(Vec3::ZERO, |shown| shown.eye);
        let instances: Vec<Instance> = skies
            .iter()
            .map(|sky| sky.instance(models.look(&sky.look()), eye))
            .collect();
        models.place(OWNER, &instances);
        self.skies_placed = !instances.is_empty();
    }

    /// The tables read once the client's archives are open, again once they change; the light of
    /// the map shown at the place of the camera and at the hour, for the view and the panel.
    fn light(&mut self, ctx: &mut Context) -> Option<MapLight> {
        let (Some(formats), Some(files)) = (ctx.service(formats::SERVICE), ctx.service(vfs::SERVICE)) else {
            return None;
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
            self.zones_unread = None;
            self.liquids_unread = None;
            self.skyboxes_unread = None;
            self.shown = None;
        }
        self.client?;
        let tables = match &self.tables {
            Some(Ok(tables)) => tables.clone(),
            Some(Err(_)) => return None,
            None => {
                if self.reading.is_none() {
                    self.reading = Some(ctx.spawn("Read the tables of the lights", move |_| {
                        // Without its zones of light, the lights of its liquids or its models of
                        // the sky, the light of the tables.
                        let zones = formats.zone_lights();
                        let liquids = formats.liquid_types();
                        let skyboxes = formats.light_skyboxes();
                        let tables = Tables::new(
                            &formats.lights()?,
                            &formats.light_params()?,
                            &formats.light_colours()?,
                            &formats.light_numbers()?,
                            zones.as_deref().map(Vec::as_slice).unwrap_or_default(),
                        )
                        .with_liquids(liquids.as_deref().map(Vec::as_slice).unwrap_or_default())
                        .with_skyboxes(skyboxes.as_deref().map(Vec::as_slice).unwrap_or_default());
                        Ok::<_, String>((Arc::new(tables), zones.err(), liquids.err(), skyboxes.err()))
                    }));
                }
                return None;
            }
        };
        let specs = settings();
        let [hour, speed, local, fog, far, server] = [HOUR, SPEED, LOCAL, FOG, FAR, SERVER_HOUR].map(|key| {
            specs
                .iter()
                .find(|spec| spec.key == key)
                .expect("declared")
                .value(ctx.setting(key).as_ref())
        });
        // The hour of the server when asked for and known, that of the settings otherwise.
        let server_hour = (server == 1).then(|| server_time(ctx));
        let time = match &server_hour {
            Some(Ok(time)) => *time,
            _ => self.time(hour, speed),
        };
        let map = ctx
            .editor()
            .call("terrain.map", json!({}))
            .ok()
            .and_then(|map| Some((map.get("id")?.as_u64()? as u32, map.get("name")?.as_str()?.to_owned())));
        let Some((map, name)) = map else {
            self.shown = None;
            return None;
        };
        // The camera unread: the light before kept.
        let Ok(PropertyValue::Vector([x, y, z])) = ctx.read_property("viewport/camera_position") else {
            return self.shown.as_ref().and_then(|shown| shown.given);
        };
        let place = [x as f32, y as f32];
        // The liquid the eye is in as the buildings decide it, the terrain's otherwise (0x790920).
        let among = ctx.service(buildings::SERVICE).map(|buildings| buildings.eye());
        let liquid = match among.as_ref().and_then(|eye| eye.liquid) {
            Some(decided) => decided,
            None => ctx
                .service(liquids::SERVICE)
                .and_then(|liquids| liquids.liquid_at([x as f32, y as f32, z as f32])),
        };
        let immersion = tables.immersion(liquid);
        let light = tables.light_in(map, place, time, local == 1, immersion);
        let mut given = light.as_ref().map(|light| {
            let game_fog = (fog == 1).then(|| tables.fog_of_the_game(light, map, immersion, time, far as f32));
            light::map_light(&light.values, time, game_fog.flatten(), immersion != Immersion::Dry)
        });
        // In a room of a building, the fog of the building weighing by how far the eye is from its
        // portals, whole 25 yards in; under the water, its fog under the water; in a liquid with a
        // light of its own, the light of the liquid alone (WebWowViewerCpp leaves the fog of the
        // building out in most liquids).
        let room = among
            .as_ref()
            .and_then(|eye| eye.group.as_ref())
            .filter(|group| group.room())
            .and_then(|group| Some((group.exit, group.fog?)));
        let mut building_fog = None;
        if let (Some(kept), Some((exit, [air, water]))) = (given, room)
            && !matches!(immersion, Immersion::Lit(_))
        {
            let weight = (exit / WHOLE_INSIDE).clamp(0.0, 1.0);
            let band = if immersion == Immersion::Under { water } else { air };
            given = Some(light::with_building_fog(
                kept,
                band,
                weight,
                map,
                immersion != Immersion::Dry,
                far as f32,
            ));
            building_fog = Some(weight);
        }
        // The models of the sky the lights name, then the building's, over them, when the group
        // the eye is in shows it (its flag 0x40000), as WebWowViewerCpp adds it to the models of
        // the sky; none in a liquid, the client drawing no sky then. Those whose animation follows
        // the hour at their share of the day (`setOverrideAnimationPerc(time / 2880)`).
        let mut skies: Vec<Sky> = light
            .as_ref()
            .map(|light| tables.skies(light, immersion))
            .unwrap_or_default()
            .into_iter()
            .map(|sky| Sky {
                model: FileRef::Path(sky.model),
                alpha: sky.alpha,
                share: (sky.flags & FOLLOWS_THE_HOUR != 0).then_some(time / DAY),
                light: Some(sky.id),
            })
            .collect();
        if immersion == Immersion::Dry
            && let Some(model) = among.as_ref().and_then(|eye| eye.group.as_ref()?.skybox.clone())
        {
            skies.push(Sky {
                model,
                alpha: 1.0,
                share: None,
                light: None,
            });
        }
        self.shown = Some(Shown {
            map,
            name,
            place,
            time,
            liquid,
            immersion,
            server_hour: server_hour.map(|hour| hour.map(|_| ())),
            building_fog,
            light,
            given,
            skies,
            eye: Vec3::new(x as f32, y as f32, z as f32),
        });
        given
    }
}

/// The name of `file` as the panel shows it: its path, or its FileDataID.
fn file_name(file: &FileRef) -> String {
    match file {
        FileRef::Path(path) => path.clone(),
        FileRef::Id(id) => format!("#{id}"),
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
            "{} ({}), at {:.0}, {:.0}, at {:02}:{:02}",
            shown.name,
            shown.map,
            shown.place[0],
            shown.place[1],
            minutes / 60,
            minutes % 60
        ));
        if let Some(given) = &shown.given {
            let fog = match given.fog {
                Some([start, end, rate]) => {
                    format!("the game's, from {start:.0} to {end:.0} yards, of the rate {rate:.2}")
                }
                None => "the editor's".to_owned(),
            };
            ui.label(format!(
                "Given to the view: the sun {:.0}° high; the fog {fog}; the glow at night of the buildings {:.2}",
                given.sun.direction[2].asin().to_degrees(),
                given.night
            ));
        }
        let Some(light) = &shown.light else {
            let said = match (shown.liquid, shown.immersion) {
                (Some(liquid), Immersion::Lit(params)) => {
                    format!("The eye in the liquid {liquid}: the params {params} of its light unknown.")
                }
                _ => "No light for this map.".to_owned(),
            };
            ui.colored_label(ui.visuals().warn_fg_color, said);
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
                    (at, _) if at <= light.zones => " (zone)",
                    _ => "",
                };
                format!("{id}{kind} {weight:.2}")
            })
            .collect();
        match (shown.liquid, shown.immersion) {
            (Some(liquid), Immersion::Lit(params)) => {
                ui.label(format!(
                    "The eye in the liquid {liquid}: the light of its params {params}, in place of every light"
                ));
            }
            (Some(liquid), _) => {
                ui.label(format!(
                    "The eye in the liquid {liquid}: the params under the water of each light"
                ));
                ui.label(format!("Lights mixed, by their weights: {}", used.join(", ")));
            }
            (None, _) => {
                ui.label(format!("Lights mixed, by their weights: {}", used.join(", ")));
            }
        }
        match &shown.server_hour {
            Some(Ok(())) => {
                ui.label("The hour of the server, through the live world.");
            }
            Some(Err(why)) => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("The hour of the server is not known ({why}): that of the settings."),
                );
            }
            None => {}
        }
        if let Some(weight) = shown.building_fog {
            ui.label(format!(
                "In a room of a building: the fog of the building weighing {weight:.2} over that of the light"
            ));
        }
        if !shown.skies.is_empty() {
            let named: Vec<String> = shown
                .skies
                .iter()
                .map(|sky| {
                    let whose = if sky.light.is_some() { "" } else { "the building's " };
                    let hour = if sky.share.is_some() {
                        ", following the hour"
                    } else {
                        ""
                    };
                    format!("{whose}{} ({:.2}{hour})", file_name(&sky.model), sky.alpha)
                })
                .collect();
            match &self.models {
                Some(_) => ui.label(format!("Models of the sky: {}", named.join(", "))),
                None => ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Models of the sky not drawn, no service models: {}", named.join(", ")),
                ),
            };
            for sky in &shown.skies {
                if let Some(models) = &self.models
                    && let LookState::Refused(why) = models.state(models.look(&sky.look()))
                {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("{}: {why}", file_name(&sky.model)));
                }
            }
        }
        if let Some(reason) = &self.zones_unread {
            ui.colored_label(ui.visuals().warn_fg_color, format!("No zones of light: {reason}"));
        }
        if let Some(reason) = &self.liquids_unread {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("No lights of the liquids: {reason}"),
            );
        }
        if let Some(reason) = &self.skyboxes_unread {
            ui.colored_label(ui.visuals().warn_fg_color, format!("No models of the sky: {reason}"));
        }
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

    fn shutdown(&mut self) {
        if let Some(view) = self.view.take() {
            view.set_light(OWNER, None);
        }
        if let Some(models) = self.models.take() {
            models.clear(OWNER);
        }
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
            outcome => match outcome.take::<Result<Read, String>>() {
                Some(read) => read.map(|(tables, zones_unread, liquids_unread, skyboxes_unread)| {
                    self.zones_unread = zones_unread;
                    self.liquids_unread = liquids_unread;
                    self.skyboxes_unread = skyboxes_unread;
                    tables
                }),
                None => return,
            },
        });
        if let Some(Err(reason)) = &self.tables {
            log::warn!("the tables of the lights are not read: {reason}");
        }
        if let Some(reason) = &self.zones_unread {
            log::warn!("the zones of light are not read: {reason}");
        }
        if let Some(reason) = &self.liquids_unread {
            log::warn!("the lights of the liquids are not read: {reason}");
        }
        if let Some(reason) = &self.skyboxes_unread {
            log::warn!("the models of the sky are not read: {reason}");
        }
    }
}

uniwow_api::export_module!(LightingModule::default());
