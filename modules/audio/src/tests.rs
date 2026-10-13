//! Tests of the module: the service and the settings it declares, the volumes and the silence
//! while minimised taken at each frame, its commands, its panel; the engine playing into a mixer
//! the tests read, at one channel and 1,000 frames a second.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use uniwow_api::audio::{BLOCK, Category, Clip, Engine, Output, Sound, State};
use uniwow_api::rodio::mixer::{self, MixerSource};
use uniwow_api::rodio::{ChannelCount, SampleRate};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::vfs::{Vfs, VfsState};
use uniwow_api::{CallId, Context, Editor, JobFn, JobId, Module, Registrar, RunsOn, egui, egui_wgpu};

use crate::AudioModule;

/// A WAV of 16-bit PCM of one channel at 1,000 frames a second.
fn wav(samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    for value in [16u32, 1 | (1 << 16), 1_000, 2_000, 2 | (16 << 16)] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// The client's files: a sound, and a file that is not one.
struct Files;

impl Vfs for Files {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(match path {
            "Sound\\Rain.wav" => Some(wav(&[16_384; 8])),
            "Sound\\Broken.wav" => Some(b"not a sound".to_vec()),
            _ => None,
        })
    }
    fn exists(&self, path: &str) -> bool {
        self.read(path).ok().flatten().is_some()
    }
    fn files_under(&self, _folder: &str) -> Vec<String> {
        Vec::new()
    }
    fn path_of(&self, _file_data_id: u32) -> Option<String> {
        None
    }
    fn state(&self) -> VfsState {
        VfsState::Ready { archives: 1, files: 2 }
    }
}

/// A host offering `vfs` when the test gives it, its settings those the test sets.
#[derive(Default)]
struct Host {
    files: Option<Arc<dyn Vfs>>,
    settings: HashMap<String, Value>,
}

impl uniwow_api::Host for Host {
    fn publish(&mut self, _source: &str, _topic: &str, _payload: Value) {}
    fn execute(&mut self, _owner: &str, _command: Box<dyn uniwow_api::Command>) {}
    fn forget_document(&mut self, _owner: &str, _document: &str) {}
    fn service(&self, id: &str) -> Option<&(dyn Any + Send + Sync)> {
        match id {
            "vfs" => self.files.as_ref().map(|files| files as &(dyn Any + Send + Sync)),
            _ => None,
        }
    }
    fn service_provider(&self, _id: &str) -> Option<String> {
        None
    }
    fn gpu(&self) -> Option<&egui_wgpu::RenderState> {
        None
    }
    fn gpu_memory(&self) -> Option<u64> {
        None
    }
    fn draw_panel(&mut self, _owner: &str, _objects: &uniwow_api::ui::SharedUi, _panel: &str, _ui: &mut egui::Ui) {}
    fn draw_dialogs(&mut self, _owner: &str, _objects: &uniwow_api::ui::SharedUi, _egui: &egui::Context) {}
    fn adopt_objects(&mut self, _owner: &str, _objects: &uniwow_api::ui::SharedUi) {}
    fn setting(&self, _module: &str, key: &str) -> Option<Value> {
        self.settings.get(key).cloned()
    }
    fn set_setting(&mut self, _module: &str, key: &str, value: Value) {
        self.settings.insert(key.to_owned(), value);
    }
    fn report_failure(&mut self, _reporter: &str, _culprit: &str, _message: &str) {}
    fn spawn(&mut self, _owner: &str, _label: &str, _job: JobFn) -> JobId {
        JobId(1)
    }
    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId {
        self.spawn(owner, label, job)
    }
    fn cancel(&mut self, _owner: &str, _job: JobId) {}
    fn call(&mut self, _caller: &str, _name: &str, _arguments: Value) -> CallId {
        CallId(1)
    }
    fn editor(&self, _caller: &str) -> Editor {
        unimplemented!("no editor in these tests")
    }
}

/// The module playing into a mixer of one channel at 1,000 frames a second, which the test reads,
/// registered; its output open.
fn module() -> (AudioModule, Registrar, MixerSource, Arc<Engine>) {
    let (mixer, source) = mixer::mixer(ChannelCount::new(1).unwrap(), SampleRate::new(1_000).unwrap());
    let engine = Engine::with_output(Box::new(move || {
        Ok(Output {
            mixer,
            device: "the test's".to_owned(),
            keep: Box::new(()),
        })
    }));
    let since = Instant::now();
    while engine.state() == State::Opening {
        assert!(since.elapsed() < Duration::from_secs(10));
        std::thread::yield_now();
    }
    let mut module = AudioModule::with_engine(engine);
    let mut reg = Registrar::default();
    module.register(&mut reg);
    let engine = module.engine.clone().unwrap();
    (module, reg, source, engine)
}

/// A frame of the editor, minimised or not.
fn frame(module: &mut AudioModule, host: &mut Host, minimised: bool) {
    let egui = egui::Context::default();
    let mut input = egui::RawInput::default();
    input.viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(minimised);
    let mut output = egui.run_ui(input, |ui| {
        module.windows_ui(ui.ctx(), &mut Context::new(host, "audio"));
    });
    output.textures_delta.clear();
}

/// The samples of the next block once a whole block has passed, as the volumes take a block to
/// move.
fn settled(source: &mut MixerSource) -> Vec<f32> {
    let _: Vec<_> = (0..BLOCK).map(|_| source.next()).collect();
    (0..BLOCK).map(|_| source.next().unwrap_or(f32::NAN)).collect()
}

fn all_near(samples: &[f32], want: f32) -> bool {
    samples.iter().all(|sample| (sample - want).abs() < 1e-4)
}

#[test]
fn the_service_and_the_settings_are_declared_the_volumes_taken_at_each_frame_silent_when_minimised() {
    let (mut module, reg, mut source, engine) = module();
    assert!(reg.services.iter().any(|(id, _)| id == "audio"));
    let category = reg.settings.as_ref().expect("a category");
    assert_eq!(category.title, "Audio");
    let keys: Vec<&str> = category.settings.iter().map(|spec| spec.key.as_str()).collect();
    assert_eq!(keys, ["master", "effects", "ambience", "music", "mute_minimised"]);
    let defaults: Vec<i64> = category.settings.iter().map(|spec| spec.default).collect();
    assert_eq!(defaults, [80, 100, 70, 50, 1]);
    assert!(
        reg.panels.iter().all(|panel| !panel.open_by_default),
        "its panel shown when asked for"
    );
    // A sound of the ambience at 1: the master volume and the ambience by default, 0.8 × 0.7.
    let mut host = Host::default();
    frame(&mut module, &mut host, false);
    engine.play(Sound {
        clip: Clip::new(1, 1_000, vec![1.0]).unwrap(),
        category: Category::Ambience,
        volume: 1.0,
        looped: true,
        fade: Duration::ZERO,
    });
    assert!(all_near(&settled(&mut source), 0.56));
    // The settings changed: taken at the next frame.
    host.settings.insert("master".to_owned(), json!(50));
    host.settings.insert("ambience".to_owned(), json!(20));
    frame(&mut module, &mut host, false);
    assert!(all_near(&settled(&mut source), 0.1));
    host.settings.insert("effects".to_owned(), json!(0));
    frame(&mut module, &mut host, false);
    assert!(all_near(&settled(&mut source), 0.1), "the ambience alone");
    // Minimised: silent; shown again, heard; minimised without the setting, heard.
    frame(&mut module, &mut host, true);
    assert!(all_near(&settled(&mut source), 0.0));
    assert!(texts(&mut module, &mut host).contains(&"Silent while the editor is minimised.".to_owned()));
    frame(&mut module, &mut host, false);
    assert!(all_near(&settled(&mut source), 0.1));
    host.settings.insert("mute_minimised".to_owned(), json!(0));
    frame(&mut module, &mut host, true);
    assert!(all_near(&settled(&mut source), 0.1));
}

/// Calls the command `name` of `reg` with `arguments`, on the calling thread.
fn call(reg: &Registrar, name: &str, arguments: Value) -> Result<Value, String> {
    let command = reg.commands.iter().find(|command| command.name == name).expect(name);
    match &command.runs_on {
        RunsOn::Caller(handler) => handler(arguments),
        _ => panic!("{name} runs on the interface thread"),
    }
}

#[test]
fn the_commands_play_a_file_of_the_client_change_its_volume_stop_it_and_say_the_state() {
    let (mut module, reg, mut source, engine) = module();
    let mut host = Host::default();
    frame(&mut module, &mut host, false);
    assert_eq!(
        call(&reg, "audio.play", json!({ "file": "Sound\\Rain.wav" })),
        Err("no service vfs: the client's archives are not read".to_owned())
    );
    host.files = Some(Arc::new(Files));
    frame(&mut module, &mut host, false);
    // Played looped, in the ambience at half: 0.5 of the file × 0.5 × 0.8 × 0.7.
    let played = call(
        &reg,
        "audio.play",
        json!({ "file": "Sound\\Rain.wav", "category": "ambience", "volume": 0.5, "loop": true }),
    )
    .unwrap();
    let sound = uniwow_api::audio::SoundId(played["sound"].as_u64().unwrap());
    assert!(engine.playing(sound));
    assert!(all_near(&settled(&mut source), 0.14));
    assert_eq!(
        call(&reg, "audio.state", json!({})),
        Ok(json!({ "state": "playing", "device": "the test's", "playing": 1 }))
    );
    call(&reg, "audio.volume", json!({ "sound": sound.0, "volume": 1.0 })).unwrap();
    assert!(all_near(&settled(&mut source), 0.28));
    call(&reg, "audio.stop", json!({ "sound": sound.0 })).unwrap();
    assert!(!engine.playing(sound));
    assert!(
        settled(&mut source).iter().all(|sample| sample.is_nan()),
        "nothing left to play"
    );
    // Once, faded in: ends with its file.
    let once = call(&reg, "audio.play", json!({ "file": "Sound\\Rain.wav", "fade": 0.004 })).unwrap();
    let samples: Vec<Option<f32>> = (0..9).map(|_| source.next()).collect();
    assert!(samples[0] == Some(0.0) && samples[8].is_none(), "{samples:?}");
    assert!(!engine.playing(uniwow_api::audio::SoundId(once["sound"].as_u64().unwrap())));
    // What cannot be played, said.
    assert_eq!(
        call(&reg, "audio.play", json!({ "file": "Sound\\None.wav" })),
        Err("no file Sound\\None.wav".to_owned())
    );
    assert!(
        call(&reg, "audio.play", json!({ "file": "Sound\\Broken.wav" }))
            .unwrap_err()
            .starts_with("Sound\\Broken.wav: not a sound")
    );
    assert!(
        call(
            &reg,
            "audio.play",
            json!({ "file": "Sound\\Rain.wav", "category": "speech" })
        )
        .is_err()
    );
    assert!(call(&reg, "audio.play", json!({})).is_err());
    assert!(call(&reg, "audio.stop", json!({})).is_err());
}

/// The texts the panel of `module` draws.
fn texts(module: &mut AudioModule, host: &mut Host) -> Vec<String> {
    let egui = egui::Context::default();
    let mut output = egui.run_ui(egui::RawInput::default(), |ui| {
        module.panel_ui("audio", ui, &mut Context::new(host, "audio"));
    });
    output.textures_delta.clear();
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_panel_says_the_device_or_why_none_and_the_module_stopped_stops_every_sound() {
    let (mut module, reg, mut source, engine) = module();
    let mut host = Host::default();
    let said = texts(&mut module, &mut host);
    assert!(said.contains(&"Playing on the test's.".to_owned()), "{said:?}");
    assert!(said.contains(&"Sounds playing: 0.".to_owned()));
    let sound = engine.play(Sound {
        clip: Clip::new(1, 1_000, vec![1.0]).unwrap(),
        category: Category::Music,
        volume: 1.0,
        looped: true,
        fade: Duration::ZERO,
    });
    assert!(texts(&mut module, &mut host).contains(&"Sounds playing: 1.".to_owned()));
    module.shutdown();
    assert!(!engine.playing(sound));
    assert_eq!(
        call(&reg, "audio.state", json!({})),
        Ok(json!({ "state": "silent", "reason": "stopped", "playing": 0 }))
    );
    assert!(settled(&mut source).iter().all(|sample| sample.is_nan()));
    assert!(texts(&mut module, &mut host).contains(&"No sound: stopped.".to_owned()));
    // Without a device: why.
    let mut silent = AudioModule::with_engine(Engine::with_output(Box::new(|| Err("no device here".to_owned()))));
    let since = Instant::now();
    while silent.engine.as_ref().unwrap().state() == State::Opening {
        assert!(since.elapsed() < Duration::from_secs(10));
        std::thread::yield_now();
    }
    assert!(texts(&mut silent, &mut host).contains(&"No sound: no device here.".to_owned()));
}
