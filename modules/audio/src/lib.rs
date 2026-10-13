//! The sounds of the editor: the engine of the runtime (`uniwow_api::audio`), started as the module
//! registers, its device opened by its own thread, given to the other modules through the service
//! `audio`; the master volume and those of the categories from the settings of *Audio*, taken at
//! each frame; every sound silenced while the editor is minimised, as the settings say. Named
//! commands play a file of the client's archives for the modules of every language. Its panel says
//! the device, or why there is none, and how many sounds play.

#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use uniwow_api::audio::{self, Category, Clip, Engine, Sound, SoundId, State};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::vfs::{self, Vfs, VfsState};
use uniwow_api::{Context, DockArea, Module, Registrar, SettingSpec, egui};

/// The settings: the master volume and that of each category, in hundredths; whether every sound is
/// silenced while the editor is minimised.
const MASTER: &str = "master";
const MUTE_MINIMISED: &str = "mute_minimised";

/// The key of the setting of the volume of `category`.
fn key(category: Category) -> &'static str {
    match category {
        Category::Effects => "effects",
        Category::Ambience => "ambience",
        Category::Music => "music",
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The settings of the category *Audio*: the editor's volumes by default, the ambience and the
/// music lower than the sounds.
fn settings() -> Vec<SettingSpec> {
    vec![
        SettingSpec::integer(MASTER, "Master volume, in hundredths", [0, 100], 80),
        SettingSpec::integer(key(Category::Effects), "Sounds, in hundredths", [0, 100], 100),
        SettingSpec::integer(
            key(Category::Ambience),
            "Ambience (the weather), in hundredths",
            [0, 100],
            70,
        ),
        SettingSpec::integer(key(Category::Music), "Music, in hundredths", [0, 100], 50),
        SettingSpec::integer(
            MUTE_MINIMISED,
            "Silent while the editor is minimised (1) or not (0)",
            [0, 1],
            1,
        ),
    ]
}

/// The value of the setting `key`.
fn setting(ctx: &Context, key: &str) -> i64 {
    settings()
        .iter()
        .find(|spec| spec.key == key)
        .expect("declared")
        .value(ctx.setting(key).as_ref())
}

/// The category named `name`.
fn category(name: &str) -> Option<Category> {
    Category::ALL.into_iter().find(|category| category.name() == name)
}

/// The seconds `value` gives, none for a value that is not a number of seconds.
fn seconds(value: Option<&Value>) -> Duration {
    value
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map_or(Duration::ZERO, |seconds| Duration::from_secs_f64(seconds.min(3_600.0)))
}

/// The sound named by `arguments`, none for a number that is not one.
fn sound_id(arguments: &Value) -> Result<SoundId, String> {
    arguments
        .get("sound")
        .and_then(Value::as_u64)
        .map(SoundId)
        .ok_or_else(|| "no sound given".to_owned())
}

/// The command `audio.play`: the file of the client's archives `file` decoded on the calling thread,
/// played in its category at its volume, looped or once, faded in.
fn play(engine: &Engine, files: Option<&Arc<dyn Vfs>>, arguments: &Value) -> Result<Value, String> {
    let file = arguments
        .get("file")
        .and_then(Value::as_str)
        .ok_or_else(|| "no file given".to_owned())?;
    let files = files.ok_or_else(|| "no service vfs: the client's archives are not read".to_owned())?;
    if !matches!(files.state(), VfsState::Ready { .. }) {
        return Err("the client's archives are not open".to_owned());
    }
    let bytes = files.read(file)?.ok_or_else(|| format!("no file {file}"))?;
    let clip = Clip::decode(bytes).map_err(|why| format!("{file}: {why}"))?;
    let category = match arguments.get("category").and_then(Value::as_str) {
        Some(name) => category(name).ok_or_else(|| format!("no category {name}: effects, ambience or music"))?,
        None => Category::Effects,
    };
    let id = engine.play(Sound {
        clip,
        category,
        volume: arguments.get("volume").and_then(Value::as_f64).unwrap_or(1.0) as f32,
        looped: arguments.get("loop").and_then(Value::as_bool).unwrap_or(false),
        fade: seconds(arguments.get("fade")),
    });
    Ok(json!({ "sound": id.0 }))
}

/// Where the output stands, in the answer of `audio.state`.
fn state(engine: &Engine) -> Value {
    match engine.state() {
        State::Opening => json!({ "state": "opening", "playing": engine.count() }),
        State::Playing(device) => json!({ "state": "playing", "device": device, "playing": engine.count() }),
        State::Silent(why) => json!({ "state": "silent", "reason": why, "playing": 0 }),
    }
}

#[derive(Default)]
pub struct AudioModule {
    /// The engine, started as the module registers unless a test gave one.
    engine: Option<Arc<Engine>>,
    /// The client's files the commands read, as the frame last found them.
    files: Arc<Mutex<Option<Arc<dyn Vfs>>>>,
    /// Whether the sounds are silenced, because the editor is minimised.
    muted: bool,
}

impl AudioModule {
    /// The module playing through `engine`.
    pub fn with_engine(engine: Engine) -> Self {
        Self {
            engine: Some(Arc::new(engine)),
            ..Default::default()
        }
    }

    fn engine(&mut self) -> Arc<Engine> {
        self.engine.get_or_insert_with(|| Arc::new(Engine::start())).clone()
    }
}

impl Module for AudioModule {
    fn register(&mut self, reg: &mut Registrar) {
        let engine = self.engine();
        let given: audio::Handle = engine.clone();
        reg.panel("audio", "Audio", DockArea::Right)
            .settings("Audio", settings())
            .provide(audio::SERVICE, given);
        // The panel only tells where the output stands: shown when asked for.
        if let Some(panel) = reg.panels.last_mut() {
            panel.open_by_default = false;
        }
        let files = self.files.clone();
        let playing = engine.clone();
        reg.command_on_caller(
            "audio.play",
            "Plays the file file of the client's archives (WAV or MP3), decoded on the calling thread, in the category category (effects, the default, ambience or music) at the volume volume (from 0 to 1, 1 by default), looped when loop is true, faded in over fade seconds; the number of the sound. Nothing is heard without an audio device (audio.state).",
            json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string" },
                    "category": { "type": "string", "enum": ["effects", "ambience", "music"] },
                    "volume": { "type": "number", "minimum": 0, "maximum": 1 },
                    "loop": { "type": "boolean" },
                    "fade": { "type": "number", "minimum": 0 },
                },
                "required": ["file"],
            }),
            json!({ "type": "object", "properties": { "sound": { "type": "integer" } } }),
            Arc::new(move |arguments| play(&playing, lock(&files).as_ref(), &arguments)),
        );
        let changing = engine.clone();
        reg.command_on_caller(
            "audio.volume",
            "Takes the sound sound to the volume volume (from 0 to 1) over fade seconds.",
            json!({
                "type": "object",
                "properties": {
                    "sound": { "type": "integer" },
                    "volume": { "type": "number", "minimum": 0, "maximum": 1 },
                    "fade": { "type": "number", "minimum": 0 },
                },
                "required": ["sound", "volume"],
            }),
            json!({ "type": "object" }),
            Arc::new(move |arguments| {
                let volume = arguments.get("volume").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                changing.set_volume(sound_id(&arguments)?, volume, seconds(arguments.get("fade")));
                Ok(json!({}))
            }),
        );
        let stopping = engine.clone();
        reg.command_on_caller(
            "audio.stop",
            "Stops the sound sound once faded out over fade seconds.",
            json!({
                "type": "object",
                "properties": { "sound": { "type": "integer" }, "fade": { "type": "number", "minimum": 0 } },
                "required": ["sound"],
            }),
            json!({ "type": "object" }),
            Arc::new(move |arguments| {
                stopping.stop(sound_id(&arguments)?, seconds(arguments.get("fade")));
                Ok(json!({}))
            }),
        );
        reg.command_on_caller(
            "audio.state",
            "Where the output of the sounds stands: state opening, playing (with its device) or silent (with the reason, as when there is no audio device), and how many sounds play.",
            json!({ "type": "object" }),
            json!({ "type": "object" }),
            Arc::new(move |_| Ok(state(&engine))),
        );
    }

    fn windows_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        // The only call at every frame, whatever panel is shown.
        let Some(engine) = self.engine.clone() else {
            return;
        };
        engine.set_master(setting(ctx, MASTER) as f32 / 100.0);
        for category in Category::ALL {
            engine.set_category(category, setting(ctx, key(category)) as f32 / 100.0);
        }
        let minimised = egui.input(|input| input.viewport().minimized).unwrap_or(false);
        self.muted = minimised && setting(ctx, MUTE_MINIMISED) == 1;
        engine.set_muted(self.muted);
        *lock(&self.files) = ctx.service(vfs::SERVICE);
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, _ctx: &mut Context) {
        let state = self.engine.as_ref().map(|engine| (engine.state(), engine.count()));
        match state.unwrap_or((State::Silent("stopped".to_owned()), 0)) {
            (State::Opening, _) => {
                ui.label("Opening the audio device…");
            }
            (State::Playing(device), count) => {
                ui.label(format!("Playing on {device}."));
                ui.label(format!("Sounds playing: {count}."));
            }
            (State::Silent(why), _) => {
                ui.colored_label(ui.visuals().warn_fg_color, format!("No sound: {why}."));
            }
        }
        if self.muted {
            ui.label("Silent while the editor is minimised.");
        }
        ui.weak("The volumes are in Edit > Settings > Audio.");
    }

    fn shutdown(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
    }
}

uniwow_api::export_module!(AudioModule::default());
