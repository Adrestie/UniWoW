//! The weather in the 3D view: that the server sends its players in the zone of the camera, through
//! the live world (`live-world.weather`), or one chosen in the panel, applied in the editor alone,
//! nothing sent to the server. Given through the service `weather`, by which the light of the map
//! mixes in the params of storm of its lights and thickens its fog, and drawn falling around the
//! eye (`fall`); in a thunderstorm, its lightning (`storm`), whose flashes the service gives the
//! light. The zone of the camera is the zone of the area of the chunk of terrain under it
//! (`terrain.area`), by `AreaTable.dbc`, read by a job once the client's archives are open.

mod fall;
mod storm;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use uniwow_api::formats::AreaRecord;
use uniwow_api::serde_json::{Value, json};
use uniwow_api::vfs::{self, VfsState};
use uniwow_api::weather::{self, Effect, Kind};
use uniwow_api::{
    Context, DockArea, JobId, JobOutcome, Module, PropertyValue, Registrar, SettingSpec, buildings, egui, formats,
    liquids, log, viewport,
};

use fall::{FallLayer, Falling, Gpu};
use storm::Storm;

/// The settings: whether the weather is none (0), the server's (1) or the one chosen (2); the
/// one chosen, by its place in `Kind::ALL`, and its intensity, in hundredths; whether lightning
/// flashes in a thunderstorm.
const MODE: &str = "mode";
const KIND: &str = "kind";
const INTENSITY: &str = "intensity";
const LIGHTNING: &str = "lightning";

const NONE: i64 = 0;
const SERVER: i64 = 1;
const CHOSEN: i64 = 2;

/// How often the weather of the server is asked of the live world.
const ASK_EVERY: Duration = Duration::from_millis(500);
/// The deepest an area is looked up through its parents for its zone.
const DEPTH: usize = 8;

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The settings of the category *Weather*.
fn settings() -> Vec<SettingSpec> {
    vec![
        SettingSpec::integer(
            MODE,
            "Weather: none (0), the server's (1) or the one chosen (2)",
            [0, 2],
            SERVER,
        ),
        SettingSpec::integer(
            KIND,
            "Weather chosen: rain, snow, sandstorm, thunderstorm, black rain, black snow, fog (0 to 6)",
            [0, 6],
            0,
        ),
        SettingSpec::integer(
            INTENSITY,
            "Intensity of the weather chosen, in hundredths",
            [0, 100],
            50,
        ),
        SettingSpec::integer(
            LIGHTNING,
            "Lightning in a thunderstorm: none (0) or flashes (1), the editor's",
            [0, 1],
            1,
        ),
    ]
}

/// The zone of `area` among `areas`: the area itself for a zone (no parent), its parents followed
/// otherwise; none for an area unknown.
pub fn zone_of(areas: &HashMap<u32, AreaRecord>, area: u32) -> Option<&AreaRecord> {
    let mut found = areas.get(&area)?;
    for _ in 0..DEPTH {
        match areas.get(&found.parent).filter(|_| found.parent != 0) {
            Some(parent) => found = parent,
            None => break,
        }
    }
    Some(found)
}

/// What the server says of the weather of a zone, by the answer of `live-world.weather`: its state,
/// grade and age, or why it is not known.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerWeather {
    Known { state: u32, grade: f32, age: u64 },
    Unknown(String),
}

impl ServerWeather {
    /// The answer `answer` of the live world, or its absence `why`.
    pub fn of(answer: Result<Value, String>) -> Self {
        let answer = match answer {
            Ok(answer) => answer,
            Err(why) => return ServerWeather::Unknown(format!("no live world ({why})")),
        };
        let number = |key: &str| answer.get(key).and_then(Value::as_f64);
        match (number("state"), number("grade")) {
            (Some(state), Some(grade)) if answer.get("known") == Some(&json!(true)) => ServerWeather::Known {
                state: state as u32,
                grade: grade as f32,
                age: number("age").unwrap_or(0.0) as u64,
            },
            _ => ServerWeather::Unknown(
                answer
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("not known")
                    .to_owned(),
            ),
        }
    }

    /// The weather it is, none for fine weather, an unknown state or none known.
    pub fn effect(&self) -> Option<Effect> {
        match self {
            ServerWeather::Known { state, grade, .. } => Effect::of_state(*state, *grade),
            ServerWeather::Unknown(_) => None,
        }
    }
}

/// The weather applied, as the service gives it to any thread.
struct Applied(Arc<Mutex<Falling>>);

impl weather::Weather for Applied {
    fn current(&self) -> Option<Effect> {
        lock(&self.0).effect
    }

    fn flash(&self) -> f32 {
        lock(&self.0).flash
    }
}

/// Where the camera is: its area and its zone, none where the terrain has none under it.
#[derive(Clone, Debug, PartialEq)]
struct Place {
    area: Option<u32>,
    zone: Option<(u32, String)>,
}

#[derive(Default)]
struct WeatherModule {
    /// The weather applied and what shelters the eye from it, shared with the layer and the service.
    falling: Arc<Mutex<Falling>>,
    /// Where the job building the resources of the particles leaves them, and that job.
    incoming: Arc<Mutex<Option<Gpu>>>,
    setup: Option<JobId>,
    drawn: bool,
    /// The client's archives as open, the areas read from them or why not, and the job reading
    /// them.
    client: Option<(usize, usize)>,
    areas: Option<Result<Arc<HashMap<u32, AreaRecord>>, String>>,
    reading: Option<JobId>,
    place: Option<Place>,
    /// The weather of the server of the zone of the camera, its zone, and when it was asked.
    server: Option<(u32, ServerWeather, Instant)>,
    /// The lightning of a thunderstorm, its time counted from `started`, and why nothing falls on
    /// the eye, if so.
    storm: Storm,
    started: Option<Instant>,
    sheltered: Option<&'static str>,
}

impl WeatherModule {
    /// The areas read once the client's archives are open, again once they change.
    fn read_areas(&mut self, ctx: &mut Context) -> Option<Arc<HashMap<u32, AreaRecord>>> {
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
            self.areas = None;
        }
        self.client?;
        match &self.areas {
            Some(Ok(areas)) => Some(areas.clone()),
            Some(Err(_)) => None,
            None => {
                if self.reading.is_none() {
                    self.reading = Some(ctx.spawn("Read the areas for the weather", move |_| {
                        formats.areas().map(|areas| {
                            Arc::new(
                                areas
                                    .iter()
                                    .map(|area| (area.id, area.clone()))
                                    .collect::<HashMap<_, _>>(),
                            )
                        })
                    }));
                }
                None
            }
        }
    }

    /// Where the camera is on the map the terrain shows; none without a map or a camera.
    fn locate(&mut self, ctx: &mut Context, eye: Option<[f32; 3]>) -> Option<Place> {
        let areas = self.read_areas(ctx);
        ctx.editor()
            .call("terrain.map", json!({}))
            .ok()
            .filter(|map| !map.is_null())?;
        let [x, y, _] = eye?;
        let area = ctx
            .editor()
            .call("terrain.area", json!({ "x": x, "y": y }))
            .ok()
            .and_then(|area| area.as_u64())
            .map(|area| area as u32);
        let zone = area.and_then(|area| {
            let areas = areas.as_ref()?;
            let zone = zone_of(areas, area)?;
            Some((zone.id, zone.name.clone()))
        });
        Some(Place { area, zone })
    }

    /// The weather of the server in `zone`, asked of the live world twice a second at most.
    fn server_weather(&mut self, ctx: &mut Context, zone: u32) -> ServerWeather {
        if let Some((asked, weather, at)) = &self.server
            && *asked == zone
            && at.elapsed() < ASK_EVERY
        {
            return weather.clone();
        }
        let weather = ServerWeather::of(ctx.editor().call("live-world.weather", json!({ "zone": zone })));
        self.server = Some((zone, weather.clone(), Instant::now()));
        weather
    }

    /// The settings of the category, by their keys.
    fn setting(ctx: &Context, key: &str) -> i64 {
        settings()
            .iter()
            .find(|spec| spec.key == key)
            .expect("declared")
            .value(ctx.setting(key).as_ref())
    }

    /// The weather chosen in the settings.
    fn chosen(ctx: &Context) -> Effect {
        let kind = Kind::ALL[(Self::setting(ctx, KIND).clamp(0, 6)) as usize];
        Effect {
            kind,
            intensity: Self::setting(ctx, INTENSITY) as f32 / 100.0,
        }
    }

    /// At each frame: where the camera is, the weather of the server there, and the weather applied
    /// as the settings say, given to the service and the layer with what shelters the eye; the
    /// lightning of a thunderstorm at the time the module has run.
    fn steer(&mut self, ctx: &mut Context) {
        let now = self.started.get_or_insert_with(Instant::now).elapsed().as_secs_f64();
        self.steer_at(ctx, now);
    }

    /// The same, `now` seconds after the module started.
    fn steer_at(&mut self, ctx: &mut Context, now: f64) {
        let eye = match ctx.read_property("viewport/camera_position") {
            Ok(PropertyValue::Vector(eye)) => Some(eye.map(|value| value as f32)),
            _ => None,
        };
        self.place = self.locate(ctx, eye);
        let zone = self
            .place
            .as_ref()
            .and_then(|place| place.zone.as_ref())
            .map(|(id, _)| *id);
        let server = zone.map(|zone| self.server_weather(ctx, zone));
        if zone.is_none() {
            self.server = None;
        }
        let effect = match Self::setting(ctx, MODE) {
            NONE => None,
            CHOSEN => Some(Self::chosen(ctx)).filter(|effect| effect.intensity > 0.0),
            _ => server.as_ref().and_then(ServerWeather::effect),
        };
        let (liquids, buildings) = (ctx.service(liquids::SERVICE), ctx.service(buildings::SERVICE));
        self.sheltered = eye.and_then(|eye| fall::sheltered(liquids.as_ref(), buildings.as_ref(), eye).0);
        // The lightning of a thunderstorm when the settings draw it, its flashes unseen where nothing
        // falls on the eye.
        let storm = effect
            .filter(|effect| effect.kind.flashes() && Self::setting(ctx, LIGHTNING) == 1)
            .map(|effect| effect.intensity);
        let (flash, _thunder) = self.storm.step(now, storm);
        let mut falling = lock(&self.falling);
        falling.effect = effect;
        falling.flash = if self.sheltered.is_some() { 0.0 } else { flash };
        falling.liquids = liquids;
        falling.buildings = buildings;
    }
}

impl Module for WeatherModule {
    fn register(&mut self, reg: &mut Registrar) {
        let applied: weather::Handle = Arc::new(Applied(self.falling.clone()));
        reg.panel("weather", "Weather", DockArea::Right)
            .settings("Weather", settings())
            .provide(weather::SERVICE, applied);
    }

    fn init(&mut self, ctx: &mut Context) {
        let Some(view) = ctx.service(viewport::SERVICE) else {
            log::info!("no viewport service: the weather is not drawn");
            return;
        };
        let Some(gpu) = ctx.gpu().cloned() else {
            return;
        };
        view.add_layer(
            ctx.module_id(),
            Box::new(FallLayer::new(self.falling.clone(), self.incoming.clone())),
        );
        self.drawn = true;
        let target = view.target();
        self.setup = Some(ctx.spawn("Build the pipeline of the weather", move |_| {
            fall::create(&gpu.device, &target)
        }));
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown.
        self.steer(ctx);
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if !self.drawn {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "No 3D view: the weather is not drawn, the light still takes it.",
            );
        }
        // Where the camera is, and the weather of the server there.
        match &self.place {
            None => {
                ui.label("No map shown by the terrain.");
            }
            Some(Place {
                zone: Some((id, name)), ..
            }) => {
                ui.label(format!("Zone of the camera: {name} ({id})."));
            }
            Some(Place { area: Some(area), .. }) => {
                ui.label(format!(
                    "Area {area} under the camera, its zone unknown: AreaTable.dbc not read."
                ));
            }
            Some(Place { area: None, .. }) => {
                ui.label("No zone under the camera: no terrain loaded in full there.");
            }
        }
        if let Some(Err(why)) = &self.areas {
            ui.colored_label(ui.visuals().warn_fg_color, format!("No areas: {why}"));
        }
        match self.server.as_ref().map(|(_, weather, _)| weather) {
            Some(ServerWeather::Known { state, grade, age }) => {
                let name = Effect::state_name(*state).map_or_else(|| format!("the state {state}"), str::to_owned);
                ui.label(format!("Weather of the server: {name}, {grade:.2}, sent {age} s ago."));
            }
            Some(ServerWeather::Unknown(why)) => {
                ui.label(format!("Weather of the server unknown: {why}."));
            }
            None => {
                ui.label("Weather of the server unknown: no zone.");
            }
        }
        ui.separator();
        // What is applied, in the editor alone.
        let mut mode = Self::setting(ctx, MODE);
        ui.horizontal(|ui| {
            for (value, label) in [(NONE, "None"), (SERVER, "The server's"), (CHOSEN, "Chosen")] {
                ui.radio_value(&mut mode, value, label);
            }
        });
        if mode != Self::setting(ctx, MODE) {
            ctx.set_setting(MODE, json!(mode));
        }
        if mode == CHOSEN {
            let mut kind = Self::setting(ctx, KIND).clamp(0, 6) as usize;
            egui::ComboBox::from_label("Weather")
                .selected_text(Kind::ALL[kind].name())
                .show_ui(ui, |ui| {
                    for (at, choice) in Kind::ALL.iter().enumerate() {
                        ui.selectable_value(&mut kind, at, choice.name());
                    }
                });
            if kind as i64 != Self::setting(ctx, KIND) {
                ctx.set_setting(KIND, json!(kind));
            }
            let mut intensity = Self::setting(ctx, INTENSITY);
            ui.add(egui::Slider::new(&mut intensity, 0..=100).text("Intensity"));
            if intensity != Self::setting(ctx, INTENSITY) {
                ctx.set_setting(INTENSITY, json!(intensity));
            }
        }
        let effect = lock(&self.falling).effect;
        match effect {
            Some(effect) => {
                ui.label(format!("Applied: {} at {:.2}.", effect.kind.name(), effect.intensity));
                if let Some(why) = self.sheltered {
                    ui.label(format!("Nothing falls: {why}."));
                }
                if effect.kind.flashes() {
                    let mut lightning = Self::setting(ctx, LIGHTNING) == 1;
                    if ui.checkbox(&mut lightning, "Lightning, the editor's").changed() {
                        ctx.set_setting(LIGHTNING, json!(i64::from(lightning)));
                    }
                    if lightning {
                        let mean = storm::LONGEST + (storm::SHORTEST - storm::LONGEST) * effect.intensity;
                        ui.label(format!(
                            "A strike every {mean:.0} s or so, its thunder up to {:.0} s after.",
                            storm::FARTHEST / storm::SOUND
                        ));
                    }
                }
            }
            None => {
                ui.label("Applied: none.");
            }
        }
        ui.weak("In the editor alone: nothing is sent to the server.");
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if Some(job) == self.setup {
            self.setup = None;
            match outcome {
                JobOutcome::Panicked(message) => log::error!("the weather cannot be drawn: {message}"),
                outcome => *lock(&self.incoming) = outcome.take::<Gpu>(),
            }
            return;
        }
        if Some(job) != self.reading {
            return;
        }
        self.reading = None;
        self.areas = Some(match outcome {
            JobOutcome::Panicked(message) => Err(message),
            JobOutcome::Cancelled => Err("their reading was cancelled".to_owned()),
            outcome => match outcome.take::<Result<Arc<HashMap<u32, AreaRecord>>, String>>() {
                Some(read) => read,
                None => return,
            },
        });
        if let Some(Err(why)) = &self.areas {
            log::warn!("the areas are not read: {why}");
        }
    }

    fn shutdown(&mut self) {
        let mut falling = lock(&self.falling);
        falling.effect = None;
        falling.flash = 0.0;
        falling.liquids = None;
        falling.buildings = None;
    }
}

uniwow_api::export_module!(WeatherModule::default());
