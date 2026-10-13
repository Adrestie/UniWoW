//! Tests of the weather: the zone of the camera, the weather of the server, the weather applied as
//! the settings say and given through the service, and what falls, drawn on the software adapter of
//! the system when it has one.

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::Duration;

use uniwow_api::audio::{Audio, Sound, SoundId, State};
use uniwow_api::buildings::{Buildings, Eye, EyeGroup};
use uniwow_api::formats::{
    AnimationRecord, AreaRecord, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair, FileRef,
    Formats, GameObjectDisplay, HairGeoset, MapRecord, Model, SoundEntryRecord, Texture, Tile, Wdl, Wdt, WeatherRecord,
    Wmo,
};
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::liquids::{Liquids, Surfaces};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::vfs::{Vfs, VfsState};
use uniwow_api::viewport::{Drawing, Fog, Layer, Phase, Sun, Target, View};
use uniwow_api::weather::{Effect, Kind, Weather, state};
use uniwow_api::{
    CallId, CommandInfo, Context, Editor, EditorBackend, Event, JobContext, JobFn, JobId, JobOutcome, Module,
    PropertyValue, Registrar, egui, egui_wgpu, wgpu,
};

use crate::fall::{self, FallLayer, Falling};
use crate::storm::{self, Storm, Strike};
use crate::{ServerWeather, WeatherModule, lock, zone_of};

fn area(id: u32, parent: u32, name: &str) -> AreaRecord {
    AreaRecord {
        id,
        map: 0,
        parent,
        flags: 0,
        name: name.to_owned(),
    }
}

/// Elwynn Forest (12), Goldshire in it (87), the Lion's Pride Inn in Goldshire (5000), Durotar (14).
fn areas() -> Vec<AreaRecord> {
    vec![
        area(12, 0, "Elwynn Forest"),
        area(14, 0, "Durotar"),
        area(87, 12, "Goldshire"),
        area(5000, 87, "Lion's Pride Inn"),
        area(6000, 6001, "a loop"),
        area(6001, 6000, "a loop"),
    ]
}

#[test]
fn the_zone_of_an_area_is_its_first_parent_without_one() {
    let areas: HashMap<u32, AreaRecord> = areas().into_iter().map(|area| (area.id, area)).collect();
    assert_eq!(zone_of(&areas, 12).map(|zone| zone.id), Some(12), "a zone");
    assert_eq!(zone_of(&areas, 87).map(|zone| zone.id), Some(12));
    assert_eq!(
        zone_of(&areas, 5000).map(|zone| zone.name.as_str()),
        Some("Elwynn Forest")
    );
    assert_eq!(zone_of(&areas, 99), None, "an area unknown");
    assert!(zone_of(&areas, 6000).is_some(), "a loop of parents ends");
}

#[test]
fn the_weather_of_the_server_is_read_from_the_live_world_or_why_not() {
    let rain = ServerWeather::of(Ok(
        json!({ "zone": 12, "known": true, "state": 5, "grade": 0.85, "age": 42 }),
    ));
    assert_eq!(
        rain,
        ServerWeather::Known {
            state: state::HEAVY_RAIN,
            grade: 0.85,
            age: 42,
        }
    );
    assert_eq!(
        rain.effect(),
        Some(Effect {
            kind: Kind::Rain,
            intensity: 0.85,
        })
    );
    let fine = ServerWeather::of(Ok(
        json!({ "zone": 12, "known": true, "state": 0, "grade": 0.1, "age": 1 }),
    ));
    assert_eq!(fine.effect(), None, "fine weather");
    let unknown = ServerWeather::of(Ok(json!({ "zone": 12, "known": false, "reason": "no player there" })));
    assert_eq!(unknown, ServerWeather::Unknown("no player there".to_owned()));
    assert_eq!(unknown.effect(), None);
    assert_eq!(
        ServerWeather::of(Err("unknown".to_owned())),
        ServerWeather::Unknown("no live world (unknown)".to_owned())
    );
}

/// The surface of the water at `height` all over the tile of the map holding 0, 0, nothing else.
struct Pond(Option<f32>);

impl Liquids for Pond {
    fn surfaces(&self) -> Arc<Surfaces> {
        Arc::new(match self.0 {
            Some(height) => {
                let cell = Surfaces::cell(0.0, 0.0);
                let tile = Surfaces::tile(cell);
                let side = uniwow_api::liquids::SIDE;
                Surfaces::from_cells(
                    (0..side * side).map(|at| ([tile[0] * side + at / side, tile[1] * side + at % side], height)),
                )
            }
            None => Surfaces::default(),
        })
    }
}

/// Buildings where the eye stands as the test sets it.
#[derive(Default)]
struct Rooms(Mutex<Eye>);

impl Buildings for Rooms {
    fn eye(&self) -> Eye {
        self.0.lock().unwrap().clone()
    }
}

fn group(flags: u32) -> EyeGroup {
    EyeGroup {
        flags,
        fog: None,
        exit: 0.0,
        skybox: None,
    }
}

#[test]
fn as_many_fall_as_the_intensity_asks_and_none_where_the_eye_is_sheltered() {
    let rain = |intensity| Effect {
        kind: Kind::Rain,
        intensity,
    };
    assert_eq!(fall::count(&rain(1.0)), 8_000);
    assert_eq!(fall::count(&rain(0.25)), 2_000);
    assert_eq!(fall::count(&rain(2.0)), 8_000, "within 0 and 1");
    assert_eq!(fall::count(&rain(0.0)), 0);
    assert_eq!(
        fall::count(&Effect {
            kind: Kind::Fog,
            intensity: 1.0,
        }),
        0,
        "the fog falls not"
    );
    assert!(Kind::ALL.iter().all(|kind| fall::fall(*kind).is_some() == kind.falls()));
    let water: uniwow_api::liquids::Handle = Arc::new(Pond(Some(10.0)));
    let rooms = Arc::new(Rooms::default());
    let buildings: uniwow_api::buildings::Handle = rooms.clone();
    // Over the water: falling down to its surface; under it, nothing.
    assert_eq!(fall::sheltered(Some(&water), None, [0.0, 0.0, 15.0]), (None, 10.0));
    assert_eq!(
        fall::sheltered(Some(&water), None, [0.0, 0.0, 5.0]).0,
        Some("the eye is under the water")
    );
    // In a room of a building, nothing; in a street lit as outside, it falls.
    rooms.0.lock().unwrap().group = Some(group(0x2000));
    assert!(fall::sheltered(None, Some(&buildings), [0.0; 3]).0.is_some());
    rooms.0.lock().unwrap().group = Some(group(0x2000 | 0x40));
    assert_eq!(fall::sheltered(None, Some(&buildings), [0.0; 3]), (None, f32::MIN));
    // In a liquid of a building, nothing.
    rooms.0.lock().unwrap().liquid = Some(Some(5));
    assert!(fall::sheltered(None, Some(&buildings), [0.0; 3]).0.is_some());
    // Without the services: it falls.
    assert_eq!(fall::sheltered(None, None, [0.0; 3]), (None, f32::MIN));
}

/// Formats that read the areas of the tests and nothing else.
struct Areas;

impl Formats for Areas {
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String> {
        Err("none".to_owned())
    }
    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String> {
        Ok(Arc::new(areas()))
    }
    fn creature_displays(&self) -> Result<Arc<Vec<CreatureDisplay>>, String> {
        Err("none".to_owned())
    }
    fn creature_models(&self) -> Result<Arc<Vec<CreatureModel>>, String> {
        Err("none".to_owned())
    }
    fn creature_looks(&self) -> Result<Arc<Vec<CreatureLook>>, String> {
        Err("none".to_owned())
    }
    fn hair_geosets(&self) -> Result<Arc<Vec<HairGeoset>>, String> {
        Err("none".to_owned())
    }
    fn facial_hairs(&self) -> Result<Arc<Vec<FacialHair>>, String> {
        Err("none".to_owned())
    }
    fn game_object_displays(&self) -> Result<Arc<Vec<GameObjectDisplay>>, String> {
        Err("none".to_owned())
    }
    fn char_sections(&self) -> Result<Arc<Vec<CharSection>>, String> {
        Err("none".to_owned())
    }
    fn animations(&self) -> Result<Arc<Vec<AnimationRecord>>, String> {
        Err("none".to_owned())
    }
    fn weathers(&self) -> Result<Arc<Vec<WeatherRecord>>, String> {
        Ok(Arc::new(weathers()))
    }
    fn sound_entries(&self) -> Result<Arc<Vec<SoundEntryRecord>>, String> {
        Ok(Arc::new(sound_entries()))
    }
    fn model(&self, _file: &FileRef) -> Result<Model, String> {
        Err("none".to_owned())
    }
    fn wmo(&self, _file: &FileRef) -> Result<Wmo, String> {
        Err("none".to_owned())
    }
    fn wdt(&self, _directory: &str) -> Result<Arc<Wdt>, String> {
        Err("none".to_owned())
    }
    fn tile(&self, _directory: &str, _x: u32, _y: u32) -> Result<Option<Tile>, String> {
        Ok(None)
    }
    fn wdl(&self, _directory: &str) -> Result<Option<Wdl>, String> {
        Ok(None)
    }
    fn texture(&self, _file: &FileRef) -> Result<Texture, String> {
        Err("none".to_owned())
    }
    fn texture_rgba(&self, _file: &FileRef) -> Result<Texture, String> {
        Err("none".to_owned())
    }
}

/// The weathers of the tests, as the client's: the rains 3 and 5, the snow 6, the thunders 86 with
/// the sound of the Dark Portal, the black rain 90 and the fog 1 without one.
fn weathers() -> Vec<WeatherRecord> {
    let weather = |id, sound, effect| WeatherRecord {
        id,
        sound,
        effect,
        transition_sky: 1.0,
        colour: [1.0; 3],
        texture: String::new(),
    };
    vec![
        weather(1, 0, 0),
        weather(3, 8_533, 1),
        weather(5, 8_535, 1),
        weather(6, 8_536, 2),
        weather(86, 4_994, 1),
        weather(90, 0, 1),
    ]
}

/// The sounds of the tests, as the client's, and the thunder chosen, of three files.
fn sound_entries() -> Vec<SoundEntryRecord> {
    let sound = |id, name: &str, files: &[&str], directory: &str, volume| SoundEntryRecord {
        id,
        kind: 50,
        name: name.to_owned(),
        files: files.iter().map(|file| ((*file).to_owned(), 1)).collect(),
        directory: directory.to_owned(),
        volume,
        flags: 0,
        distances: [7.0, 30.0],
    };
    let weather = "Sound\\Ambience\\Weather";
    vec![
        sound(
            4_994,
            "DarkPortal",
            &["DarkPortal.wav"],
            "Sound\\Ambience\\ZoneAmbience",
            1.0,
        ),
        sound(8_533, "Weather - RainLight", &["RainLightLoop.wav"], weather, 0.69),
        sound(8_535, "Weather - RainHeavy", &["RainHeavyLoop.wav"], weather, 0.69),
        sound(8_536, "Weather - SnowLight", &["SnowLight.wav"], weather, 0.69),
        sound(
            13_021,
            "StormPeaksLightning",
            &["Bolt1.wav", "Bolt2.wav", "Bolt3.wav"],
            "Sound\\Doodad",
            0.8,
        ),
    ]
}

/// The files of the client's sounds, each a WAV whose count of samples tells which it is.
fn sound_files() -> HashMap<&'static str, usize> {
    HashMap::from([
        ("Sound\\Ambience\\Weather\\RainLightLoop.wav", 10),
        ("Sound\\Ambience\\Weather\\RainHeavyLoop.wav", 20),
        ("Sound\\Ambience\\ZoneAmbience\\DarkPortal.wav", 40),
        ("Sound\\Doodad\\Bolt1.wav", 51),
        ("Sound\\Doodad\\Bolt2.wav", 52),
        ("Sound\\Doodad\\Bolt3.wav", 53),
        ("Sound\\Ambience\\Weather\\SnowLight.wav", 0),
    ])
}

/// A WAV of 16-bit PCM of one channel, `count` samples of a quarter.
fn wav(count: usize) -> Vec<u8> {
    let data = (count * 2) as u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    for value in [16u32, 1 | (1 << 16), 22_050, 44_100, 2 | (16 << 16)] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for _ in 0..count {
        bytes.extend_from_slice(&8_192i16.to_le_bytes());
    }
    bytes
}

/// The client's files, open: the sounds of `sound_files`, nothing else.
struct Files;

impl Vfs for Files {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(sound_files().get(path).map(|count| wav(*count)))
    }
    fn exists(&self, _path: &str) -> bool {
        false
    }
    fn files_under(&self, _folder: &str) -> Vec<String> {
        Vec::new()
    }
    fn path_of(&self, _file_data_id: u32) -> Option<String> {
        None
    }
    fn state(&self) -> VfsState {
        VfsState::Ready { archives: 1, files: 1 }
    }
}

/// An editor whose terrain shows a map or none, the area `area` under a camera at 0, 0, 20, and
/// whose live world answers `server`, none without a live world.
struct Shown {
    map: Mutex<Value>,
    area: Mutex<Value>,
    server: Mutex<Option<Value>>,
    asked: Mutex<Vec<Value>>,
}

impl EditorBackend for Shown {
    fn commands(&self) -> Vec<CommandInfo> {
        Vec::new()
    }
    fn call(&self, _caller: &str, name: &str, arguments: Value) -> Result<Value, String> {
        match name {
            "terrain.map" => Ok(self.map.lock().unwrap().clone()),
            "terrain.area" => Ok(self.area.lock().unwrap().clone()),
            "live-world.weather" => {
                self.asked.lock().unwrap().push(arguments);
                self.server.lock().unwrap().clone().ok_or_else(|| "unknown".to_owned())
            }
            _ => Err("unknown".to_owned()),
        }
    }
    fn publish(&self, _source: &str, _topic: &str, _payload: Value) -> Result<(), String> {
        Ok(())
    }
    fn subscribe(&self, _caller: &str, _topic: &str) -> Result<u64, String> {
        Err("none".to_owned())
    }
    fn next_event(&self, _caller: &str, _subscription: u64, _timeout: Duration) -> Result<Option<Event>, String> {
        Ok(None)
    }
    fn unsubscribe(&self, _subscription: u64) {}
    fn setting(&self, _caller: &str, _space: &str, _key: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }
    fn set_setting(&self, _caller: &str, _space: &str, _key: &str, _value: Value) -> Result<(), String> {
        Ok(())
    }
    fn begin_group(&self, _caller: &str, _label: &str) -> Result<(), String> {
        Ok(())
    }
    fn end_group(&self, _caller: &str) -> Result<(), String> {
        Ok(())
    }
    fn read_property(&self, _caller: &str, path: &str) -> Result<PropertyValue, String> {
        match path {
            "viewport/camera_position" => Ok(PropertyValue::Vector([0.0, 0.0, 20.0])),
            _ => Err("unread".to_owned()),
        }
    }
}

/// A host offering `formats`, `vfs` and `buildings` when the test sets them, keeping the jobs started
/// for the test to run them.
struct Host {
    formats: Arc<dyn Formats>,
    files: Arc<dyn Vfs>,
    buildings: Option<uniwow_api::buildings::Handle>,
    jobs: Vec<(JobId, JobFn)>,
    spawned: u64,
    audio: Option<uniwow_api::audio::Handle>,
    settings: HashMap<String, Value>,
    editor: Arc<Shown>,
}

fn host() -> Host {
    Host {
        formats: Arc::new(Areas),
        files: Arc::new(Files),
        buildings: None,
        jobs: Vec::new(),
        spawned: 0,
        audio: None,
        settings: HashMap::new(),
        editor: Arc::new(Shown {
            map: Mutex::new(json!({ "id": 0, "name": "Azeroth" })),
            area: Mutex::new(json!(87)),
            server: Mutex::new(None),
            asked: Mutex::new(Vec::new()),
        }),
    }
}

impl uniwow_api::Host for Host {
    fn publish(&mut self, _source: &str, _topic: &str, _payload: Value) {}
    fn execute(&mut self, _owner: &str, _command: Box<dyn uniwow_api::Command>) {}
    fn forget_document(&mut self, _owner: &str, _document: &str) {}
    fn service(&self, id: &str) -> Option<&(dyn Any + Send + Sync)> {
        match id {
            "formats" => Some(&self.formats),
            "vfs" => Some(&self.files),
            "audio" => self.audio.as_ref().map(|audio| audio as &(dyn Any + Send + Sync)),
            "buildings" => self
                .buildings
                .as_ref()
                .map(|buildings| buildings as &(dyn Any + Send + Sync)),
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
    fn spawn(&mut self, _owner: &str, _label: &str, job: JobFn) -> JobId {
        self.spawned += 1;
        self.jobs.push((JobId(self.spawned), job));
        JobId(self.spawned)
    }
    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId {
        self.spawn(owner, label, job)
    }
    fn cancel(&mut self, _owner: &str, _job: JobId) {}
    fn call(&mut self, _caller: &str, _name: &str, _arguments: Value) -> CallId {
        CallId(1)
    }
    fn editor(&self, caller: &str) -> Editor {
        Editor::new(self.editor.clone(), caller)
    }
}

/// The texts the panel of `module` draws.
fn panel(module: &mut WeatherModule, host: &mut Host) -> Vec<String> {
    let egui = egui::Context::default();
    let mut output = egui.run_ui(egui::RawInput::default(), |ui| {
        module.panel_ui("weather", ui, &mut Context::new(host, "weather"));
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
fn the_weather_of_the_server_in_the_zone_of_the_camera_is_applied_or_the_one_chosen() {
    let mut host = host();
    let mut module = WeatherModule::default();
    let mut reg = Registrar::default();
    module.register(&mut reg);
    let egui = egui::Context::default();
    let service = module.falling.clone();
    let current = || crate::Applied(service.clone()).current();
    // The areas read by a job once the client's archives are open.
    module.windows_ui(&egui, &mut Context::new(&mut host, "weather"));
    assert_eq!(host.jobs.len(), 1, "the areas asked for");
    let (id, job) = host.jobs.remove(0);
    let read = job(&JobContext::new(
        Arc::default(),
        Arc::default(),
        Editor::new(host.editor.clone(), "weather"),
    ));
    module.on_job(id, JobOutcome::Done(read), &mut Context::new(&mut host, "weather"));
    // Goldshire under the camera, in Elwynn Forest; no live world: nothing applied, why said.
    module.windows_ui(&egui, &mut Context::new(&mut host, "weather"));
    assert_eq!(current(), None);
    let texts = panel(&mut module, &mut host);
    assert!(
        texts.contains(&"Zone of the camera: Elwynn Forest (12).".to_owned()),
        "{texts:?}"
    );
    assert!(texts.contains(&"Weather of the server unknown: no live world (unknown).".to_owned()));
    assert!(texts.contains(&"Applied: none.".to_owned()));
    assert!(texts.contains(&"In the editor alone: nothing is sent to the server.".to_owned()));
    // The server's heavy rain there, asked for the zone, followed by default.
    *host.editor.server.lock().unwrap() =
        Some(json!({ "zone": 12, "known": true, "state": 5, "grade": 0.85, "age": 3 }));
    module.server = None;
    module.windows_ui(&egui, &mut Context::new(&mut host, "weather"));
    assert_eq!(host.editor.asked.lock().unwrap().last(), Some(&json!({ "zone": 12 })));
    assert_eq!(
        current(),
        Some(Effect {
            kind: Kind::Rain,
            intensity: 0.85,
        })
    );
    let texts = panel(&mut module, &mut host);
    assert!(
        texts.contains(&"Weather of the server: heavy rain, 0.85, sent 3 s ago.".to_owned()),
        "{texts:?}"
    );
    assert!(texts.contains(&"Applied: rain at 0.85.".to_owned()));
    // Asked again twice a second at most.
    let asked = host.editor.asked.lock().unwrap().len();
    module.windows_ui(&egui, &mut Context::new(&mut host, "weather"));
    assert_eq!(host.editor.asked.lock().unwrap().len(), asked);
    // The weather chosen in its place: snow at 0.3; none.
    host.settings.insert("mode".to_owned(), json!(2));
    host.settings.insert("kind".to_owned(), json!(1));
    host.settings.insert("intensity".to_owned(), json!(30));
    module.windows_ui(&egui, &mut Context::new(&mut host, "weather"));
    assert_eq!(
        current(),
        Some(Effect {
            kind: Kind::Snow,
            intensity: 0.3,
        })
    );
    host.settings.insert("mode".to_owned(), json!(0));
    module.windows_ui(&egui, &mut Context::new(&mut host, "weather"));
    assert_eq!(current(), None);
    // The server's again, the camera over no terrain loaded: no zone, nothing.
    host.settings.insert("mode".to_owned(), json!(1));
    *host.editor.area.lock().unwrap() = Value::Null;
    module.windows_ui(&egui, &mut Context::new(&mut host, "weather"));
    assert_eq!(current(), None);
    let texts = panel(&mut module, &mut host);
    assert!(
        texts.contains(&"No zone under the camera: no terrain loaded in full there.".to_owned()),
        "{texts:?}"
    );
    // No map: nothing; the module stopped: nothing given.
    *host.editor.area.lock().unwrap() = json!(12);
    module.windows_ui(&egui, &mut Context::new(&mut host, "weather"));
    assert!(current().is_some());
    module.shutdown();
    assert_eq!(current(), None);
}

fn resolved<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut std::task::Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// A device of the software adapter of the system, or none.
fn device() -> Option<egui_wgpu::RenderState> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: true,
        ..Default::default()
    }))?
    .ok()?;
    let (device, queue) = resolved(adapter.request_device(&wgpu::DeviceDescriptor::default()))?.ok()?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
    Some(egui_wgpu::RenderState {
        adapter,
        available_adapters: Vec::new(),
        instance,
        device,
        queue,
        target_format: format,
        renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
        surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
    })
}

const TARGET: Target = Target {
    color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
    depth_format: wgpu::TextureFormat::Depth32Float,
    sample_count: 1,
    depth_compare: wgpu::CompareFunction::Greater,
};
const SIZE: u32 = 128;

/// What `layer` draws seen from `eye` towards +X at `time`, `SIZE` × `SIZE` pixels of RGBA cleared to
/// black, its depth to infinity, every phase in its order, in the pass.
fn render(gpu: &egui_wgpu::RenderState, layer: &mut FallLayer, eye: Vec3, time: f32) -> Vec<u8> {
    let view_matrix = Mat4::look_at_rh(eye, eye + Vec3::X, Vec3::Z);
    let view = View {
        view_proj: Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1) * view_matrix,
        view: view_matrix,
        eye,
        size: [SIZE, SIZE],
        time,
        fog: Fog::default(),
        sun: Sun::default(),
        sky: None,
        water: None,
        night: 0.0,
    };
    layer.prepare(gpu, &view);
    assert_eq!(layer.drawing(), Drawing::Pass);
    let texture = |format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let colour = texture(
        TARGET.color_format,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let depth = texture(TARGET.depth_format, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let (colour_view, depth_view) = (
        colour.create_view(&Default::default()),
        depth.create_view(&Default::default()),
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &colour_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for phase in Phase::ALL {
            layer.draw_pass(gpu, &TARGET, &view, phase, &mut pass);
        }
    }
    let row = SIZE * 4;
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &colour,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &pixels,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    pixels.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    pixels.slice(..).get_mapped_range().expect("mapped").to_vec()
}

/// The pixels of an image of `render` that are not black.
fn lit(pixels: &[u8]) -> usize {
    pixels
        .chunks(4)
        .filter(|pixel| pixel[..3].iter().any(|value| *value > 8))
        .count()
}

#[test]
fn what_falls_is_drawn_around_the_eye_in_one_draw_moving_with_the_time_not_where_sheltered() {
    let Some(gpu) = device() else {
        return;
    };
    let falling = Arc::new(Mutex::new(Falling::default()));
    let incoming = Arc::new(Mutex::new(Some(fall::create(&gpu.device, &TARGET))));
    let mut layer = FallLayer::new(falling.clone(), incoming);
    let eye = Vec3::new(0.0, 0.0, 20.0);
    // Fine weather: nothing.
    assert_eq!(lit(&render(&gpu, &mut layer, eye, 1.0)), 0);
    for (kind, count) in [(Kind::Rain, 8_000), (Kind::Snow, 6_000), (Kind::Sandstorm, 6_000)] {
        lock(&falling).effect = Some(Effect { kind, intensity: 1.0 });
        let first = render(&gpu, &mut layer, eye, 1.0);
        assert!(lit(&first) > 20, "{kind:?}: {} pixels", lit(&first));
        // One draw of all, two triangles each, whatever their count.
        let stats = layer.stats();
        assert_eq!((stats.draws, stats.triangles), (1, 2 * count), "{kind:?}");
        // The same at the same moment, moved a moment later.
        assert_eq!(render(&gpu, &mut layer, eye, 1.0), first, "{kind:?}");
        assert_ne!(render(&gpu, &mut layer, eye, 1.5), first, "{kind:?}");
        // Half as many at half the intensity.
        lock(&falling).effect = Some(Effect { kind, intensity: 0.5 });
        render(&gpu, &mut layer, eye, 1.0);
        assert_eq!(layer.stats().triangles, count, "{kind:?}");
    }
    // Under the water: nothing; over it, still.
    lock(&falling).effect = Some(Effect {
        kind: Kind::Snow,
        intensity: 1.0,
    });
    lock(&falling).liquids = Some(Arc::new(Pond(Some(30.0))));
    assert_eq!(lit(&render(&gpu, &mut layer, eye, 1.0)), 0, "under the water");
    assert_eq!(layer.stats().draws, 0);
    // The surface at the eye's feet: only what is over it, fewer than without water.
    lock(&falling).liquids = Some(Arc::new(Pond(Some(19.0))));
    let over = lit(&render(&gpu, &mut layer, eye, 1.0));
    lock(&falling).liquids = None;
    let all = lit(&render(&gpu, &mut layer, eye, 1.0));
    assert!(over > 0 && over < all, "{over} of {all}");
    // In a room of a building: nothing.
    let rooms = Arc::new(Rooms::default());
    rooms.0.lock().unwrap().group = Some(group(0x2000));
    lock(&falling).buildings = Some(rooms);
    assert_eq!(lit(&render(&gpu, &mut layer, eye, 1.0)), 0, "in a room");
}

/// The flashes of `storm` and the thunders heard, stepped every `step` seconds for `seconds`, in a
/// storm of `intensity`: the strikes as they fell, and the thunders.
fn run(storm: &mut Storm, intensity: f32, seconds: f64, step: f64) -> (Vec<Strike>, Vec<storm::Thunder>) {
    let (mut strikes, mut thunders) = (Vec::new(), Vec::new());
    let mut now = 0.0;
    while now < seconds {
        let (flash, thunder) = storm.step(now, Some(intensity));
        assert!((0.0..=1.0).contains(&flash), "{flash}");
        if let Some(strike) = storm.strike()
            && strikes.last() != Some(&strike)
        {
            strikes.push(strike);
        }
        thunders.extend(thunder);
        now += step;
    }
    (strikes, thunders)
}

#[test]
fn the_lightning_strikes_seeded_more_often_as_the_storm_grows_each_strike_followed_by_its_thunder() {
    // The same seed, the same storm; another, another.
    let (strikes, thunders) = run(&mut Storm::new(7), 1.0, 600.0, 0.01);
    assert_eq!(
        run(&mut Storm::new(7), 1.0, 600.0, 0.01),
        (strikes.clone(), thunders.clone())
    );
    assert_ne!(run(&mut Storm::new(8), 1.0, 600.0, 0.01).0, strikes);
    // At full intensity, a strike every 6 seconds or so, each after half to one and a half times
    // that; four times as rare at the lowest.
    let mean = |strikes: &[Strike]| 600.0 / strikes.len() as f64;
    assert!((4.5..7.5).contains(&mean(&strikes)), "{}", mean(&strikes));
    assert!(
        strikes[0].at >= 3.0 && strikes[0].at < 9.0,
        "the first after one interval"
    );
    assert!(
        strikes
            .windows(2)
            .all(|pair| (2.99..9.01).contains(&(pair[1].at - pair[0].at))),
        "the intervals"
    );
    let (rare, _) = run(&mut Storm::new(7), 0.0001, 600.0, 0.01);
    assert!((18.0..30.0).contains(&mean(&rare)), "{}", mean(&rare));
    // One to three flashes each, the nearer the brighter; a thunder each, as late as the strike
    // is far, as loud as it is near.
    assert!(strikes.iter().all(|strike| (1..=3).contains(&strike.flashes)));
    assert!(strikes.iter().any(|strike| strike.flashes == 1) && strikes.iter().any(|strike| strike.flashes == 3));
    assert_eq!(thunders.len(), strikes.len());
    for (strike, thunder) in strikes.iter().zip(&thunders) {
        let delay = thunder.at - strike.at;
        let near = (strike.brightness - storm::DIMMEST) / (1.0 - storm::DIMMEST);
        let far = storm::NEAREST + (storm::FARTHEST - storm::NEAREST) * (1.0 - near);
        assert!((delay - f64::from(far / storm::SOUND)).abs() < 1e-3, "{delay}");
        assert!(
            (thunder.loudness - (storm::FAINTEST + (1.0 - storm::FAINTEST) * near)).abs() < 1e-5,
            "{thunder:?}"
        );
        assert!((0.5..4.3).contains(&delay), "{delay}");
    }
    // Heard when due, never before: stepped at its time, a thunder comes at that step.
    let mut storm = Storm::new(7);
    let (strikes, _) = run(&mut storm.clone(), 1.0, 20.0, 0.01);
    let first = strikes[0];
    let mut heard = None;
    let mut now = 0.0;
    while heard.is_none() && now < 20.0 {
        heard = storm.step(now, Some(1.0)).1.map(|thunder| (now, thunder));
        now += 0.01;
    }
    let (at, thunder) = heard.expect("a thunder");
    assert!(at >= thunder.at && at - thunder.at < 0.011 && thunder.at > first.at);
}

#[test]
fn a_strike_flashes_lit_at_once_and_fading_each_flash_dimmer_than_the_one_before() {
    let strike = Strike {
        at: 10.0,
        flashes: 3,
        brightness: 0.8,
    };
    let at = |since: f64| storm::brightness(&strike, 10.0 + since);
    assert_eq!(at(-0.01), 0.0);
    assert!((at(storm::RISE / 2.0) - 0.4).abs() < 1e-5);
    assert!((at(storm::RISE) - 0.8).abs() < 1e-5, "whole at the end of its rise");
    assert!(at(storm::RISE + storm::FADE / 2.0) < 0.8);
    // The second flash, dimmer: at its height, 0.65 of the first.
    let second = at(storm::GAP + storm::RISE);
    assert!((second - 0.8 * 0.65).abs() < 1e-5, "{second}");
    let third = at(2.0 * storm::GAP + storm::RISE);
    assert!(third < second && third > 0.0, "{third}");
    assert_eq!(at(2.0 * storm::GAP + storm::RISE + storm::FADE + 0.01), 0.0, "out");
    // A strike of one flash: dark once it is out.
    let one = Strike { flashes: 1, ..strike };
    assert_eq!(
        storm::brightness(&one, 10.0 + storm::GAP + storm::RISE + storm::FADE),
        0.0
    );
}

#[test]
fn without_a_storm_nothing_strikes_and_the_thunders_waiting_are_forgotten() {
    let mut storm = Storm::new(3);
    for at in 0..1_000 {
        assert_eq!(storm.step(f64::from(at), None), (0.0, None));
        assert_eq!(
            storm.step(f64::from(at) + 0.5, Some(0.0)),
            (0.0, None),
            "an intensity of 0"
        );
    }
    // A strike, then the storm over before its thunder: no thunder, then or later.
    let mut now = 0.0;
    while storm.strike().is_none() {
        storm.step(now, Some(1.0));
        now += 0.01;
    }
    assert_eq!(storm.step(now, None), (0.0, None));
    assert!(storm.strike().is_none());
    // Back, the next strike after a whole interval from its start again; the first thunder heard
    // that of a strike of the new storm, none of the one before.
    let from = now + 100.0;
    let (mut first, mut heard) = (None, None);
    let mut at = from;
    while heard.is_none() && at < from + 100.0 {
        heard = storm.step(at, Some(1.0)).1;
        first = first.or(storm.strike());
        at += 0.01;
    }
    let first = first.expect("a strike");
    assert!(first.at - from >= 2.99, "{first:?}");
    assert!(heard.expect("a thunder").at > first.at, "{heard:?}");
}

#[test]
fn the_flashes_of_a_thunderstorm_are_given_but_where_nothing_falls_or_when_the_settings_draw_none() {
    let mut host = host();
    let mut module = WeatherModule::default();
    let mut reg = Registrar::default();
    module.register(&mut reg);
    let service = module.falling.clone();
    let applied = crate::Applied(service.clone());
    // A thunderstorm chosen at 1: run until a strike, its flash given.
    host.settings.insert("mode".to_owned(), json!(2));
    host.settings.insert("kind".to_owned(), json!(3));
    host.settings.insert("intensity".to_owned(), json!(100));
    let mut now = 0.0;
    let mut brightest: f32 = 0.0;
    while now < 30.0 {
        module.steer_at(&mut Context::new(&mut host, "weather"), now);
        assert_eq!(applied.current().map(|effect| effect.kind), Some(Kind::Thunders));
        brightest = brightest.max(applied.flash());
        now += 0.01;
    }
    assert!(brightest > 0.3, "{brightest}");
    let texts = panel(&mut module, &mut host);
    assert!(
        texts.contains(&"A strike every 6 s or so, its thunder up to 4 s after.".to_owned()),
        "{texts:?}"
    );
    // Lit at each moment as its storm says.
    let flash_at = |module: &mut WeatherModule, host: &mut Host, now: f64| {
        module.steer_at(&mut Context::new(host, "weather"), now);
        applied.flash()
    };
    let mut copy = module.storm.clone();
    let mut lit = 0;
    for _ in 0..1_000 {
        now += 0.01;
        let expected = copy.step(now, Some(1.0)).0;
        assert_eq!(flash_at(&mut module, &mut host, now), expected);
        lit += usize::from(expected > 0.3);
    }
    assert!(lit > 0);
    // In a room of a building: none seen.
    let rooms = Arc::new(Rooms::default());
    rooms.0.lock().unwrap().group = Some(group(0x2000));
    host.buildings = Some(rooms.clone());
    let mut unseen = 0;
    for _ in 0..1_000 {
        now += 0.01;
        let expected = copy.step(now, Some(1.0)).0;
        assert_eq!(flash_at(&mut module, &mut host, now), 0.0, "in a room");
        unseen += usize::from(expected > 0.3);
    }
    assert!(unseen > 0);
    assert!(panel(&mut module, &mut host).contains(&"Nothing falls: the eye is in a room of a building.".to_owned()));
    host.buildings = None;
    // The lightning off in the settings: none, ever.
    host.settings.insert("lightning".to_owned(), json!(0));
    let mut most: f32 = 0.0;
    for step in 0..3_000 {
        most = most.max(flash_at(&mut module, &mut host, now + f64::from(step) * 0.01));
    }
    assert_eq!(most, 0.0);
    // The rain: none.
    host.settings.insert("lightning".to_owned(), json!(1));
    host.settings.insert("kind".to_owned(), json!(0));
    for step in 0..3_000 {
        most = most.max(flash_at(&mut module, &mut host, now + 30.0 + f64::from(step) * 0.01));
    }
    assert_eq!(most, 0.0);
    // Stopped: none.
    host.settings.insert("kind".to_owned(), json!(3));
    module.shutdown();
    assert_eq!(applied.flash(), 0.0);
}

/// What the test hears: each sound played, by its number, with the count of samples of its clip,
/// its volume, its looping and its fade; the volumes and the stops asked.
#[derive(Default)]
struct Ears {
    log: Mutex<Vec<String>>,
    played: Mutex<u64>,
    stopped: Mutex<Vec<u64>>,
    /// Why there is no device, if so.
    silent: Mutex<Option<String>>,
}

impl Ears {
    /// What was heard since `from`, the number of entries before.
    fn since(&self, from: usize) -> Vec<String> {
        self.log.lock().unwrap()[from..].to_vec()
    }
    fn heard(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

impl Audio for Ears {
    fn play(&self, sound: Sound) -> SoundId {
        let mut played = self.played.lock().unwrap();
        *played += 1;
        let looping = if sound.looped { "looped" } else { "once" };
        self.log.lock().unwrap().push(format!(
            "play {}: {} samples at {:.2}, {looping}, faded in over {} s",
            *played,
            sound.clip.samples().len(),
            sound.volume,
            sound.fade.as_secs()
        ));
        SoundId(*played)
    }
    fn set_volume(&self, sound: SoundId, volume: f32, fade: Duration) {
        self.log
            .lock()
            .unwrap()
            .push(format!("volume {}: {volume:.2} over {} s", sound.0, fade.as_secs()));
    }
    fn stop(&self, sound: SoundId, fade: Duration) {
        self.stopped.lock().unwrap().push(sound.0);
        self.log
            .lock()
            .unwrap()
            .push(format!("stop {} over {} s", sound.0, fade.as_secs()));
    }
    fn playing(&self, sound: SoundId) -> bool {
        sound.0 <= *self.played.lock().unwrap() && !self.stopped.lock().unwrap().contains(&sound.0)
    }
    fn state(&self) -> State {
        match self.silent.lock().unwrap().clone() {
            Some(why) => State::Silent(why),
            None => State::Playing("the test's".to_owned()),
        }
    }
}

/// Runs the jobs the module started, handing it their outcomes; how many.
fn run_jobs(module: &mut WeatherModule, host: &mut Host) -> usize {
    let mut ran = 0;
    while !host.jobs.is_empty() {
        let (id, job) = host.jobs.remove(0);
        let outcome = job(&JobContext::new(
            Arc::default(),
            Arc::default(),
            Editor::new(host.editor.clone(), "weather"),
        ));
        module.on_job(id, JobOutcome::Done(outcome), &mut Context::new(host, "weather"));
        ran += 1;
    }
    ran
}

/// A frame of the module `now` seconds after it started.
fn frame(module: &mut WeatherModule, host: &mut Host, now: f64) {
    module.steer_at(&mut Context::new(host, "weather"), now);
}

/// The server sends `state` of `grade` in the zone of the camera, asked again at the next frame.
fn server_sends(module: &mut WeatherModule, host: &mut Host, state: u32, grade: f32) {
    *host.editor.server.lock().unwrap() =
        Some(json!({ "zone": 12, "known": true, "state": state, "grade": grade, "age": 1 }));
    module.server = None;
}

#[test]
fn the_weather_is_heard_its_loop_faded_in_and_out_silent_where_sheltered_its_thunders_after_the_flashes() {
    let mut host = host();
    let ears = Arc::new(Ears::default());
    host.audio = Some(ears.clone());
    let mut module = WeatherModule::default();
    let mut reg = Registrar::default();
    module.register(&mut reg);
    // The areas read; fine weather: nothing heard, nothing read.
    frame(&mut module, &mut host, 0.0);
    assert_eq!(run_jobs(&mut module, &mut host), 1);
    frame(&mut module, &mut host, 0.1);
    assert_eq!((ears.heard(), host.jobs.len()), (0, 0));
    // The heavy rain of the server: the tables read, then its file, then its loop faded in.
    server_sends(&mut module, &mut host, 5, 0.85);
    frame(&mut module, &mut host, 0.2);
    assert!(panel(&mut module, &mut host).contains(&"Nothing heard: reading the sounds of the client.".to_owned()));
    assert_eq!(run_jobs(&mut module, &mut host), 1);
    frame(&mut module, &mut host, 0.3);
    assert!(
        panel(&mut module, &mut host)
            .contains(&"Reading Weather - RainHeavy, Sound\\Ambience\\Weather\\RainHeavyLoop.wav…".to_owned())
    );
    assert_eq!(run_jobs(&mut module, &mut host), 1);
    frame(&mut module, &mut host, 0.4);
    assert_eq!(ears.since(0), ["play 1: 20 samples at 0.69, looped, faded in over 3 s"]);
    assert!(
        panel(&mut module, &mut host)
            .contains(&"Heard: Weather - RainHeavy, Sound\\Ambience\\Weather\\RainHeavyLoop.wav.".to_owned())
    );
    frame(&mut module, &mut host, 0.5);
    assert_eq!(ears.heard(), 1, "played once");
    // A lighter rain: the loop before faded out, the other faded in once read; the sound of the
    // state the server sent, whatever its grade says.
    server_sends(&mut module, &mut host, 3, 0.5);
    frame(&mut module, &mut host, 0.6);
    assert_eq!(ears.since(1), ["stop 1 over 3 s"]);
    run_jobs(&mut module, &mut host);
    frame(&mut module, &mut host, 0.7);
    assert_eq!(ears.since(2), ["play 2: 10 samples at 0.69, looped, faded in over 3 s"]);
    // In a room of a building: faded out; out of it, back.
    let rooms = Arc::new(Rooms::default());
    rooms.0.lock().unwrap().group = Some(group(0x2000));
    host.buildings = Some(rooms.clone());
    frame(&mut module, &mut host, 0.8);
    host.buildings = None;
    frame(&mut module, &mut host, 0.9);
    assert_eq!(ears.since(3), ["volume 2: 0.00 over 1 s", "volume 2: 0.69 over 1 s"]);
    // The snow, whose file is no sound: said, not read again.
    server_sends(&mut module, &mut host, 6, 0.3);
    frame(&mut module, &mut host, 1.0);
    run_jobs(&mut module, &mut host);
    frame(&mut module, &mut host, 1.1);
    frame(&mut module, &mut host, 1.2);
    assert_eq!(
        (ears.since(5), host.jobs.len()),
        (vec!["stop 2 over 3 s".to_owned()], 0)
    );
    let said = panel(&mut module, &mut host);
    assert!(
        said.iter()
            .any(|text| text.starts_with("Nothing heard: Sound\\Ambience\\Weather\\SnowLight.wav: ")),
        "{said:?}"
    );
    // A thunderstorm chosen: the loop of the Dark Portal, as the client's tables say; then a thunder
    // after each strike, as loud as the strike is near, missed while its file is read.
    host.settings.insert("mode".to_owned(), json!(2));
    host.settings.insert("kind".to_owned(), json!(3));
    host.settings.insert("intensity".to_owned(), json!(100));
    frame(&mut module, &mut host, 1.3);
    run_jobs(&mut module, &mut host);
    frame(&mut module, &mut host, 1.4);
    assert_eq!(ears.since(6), ["play 3: 40 samples at 1.00, looped, faded in over 3 s"]);
    let mut copy = module.storm.clone();
    let (mut thunders, mut heard, mut now) = (0, 0, 1.4);
    while now < 120.0 {
        now += 0.01;
        let thunder = copy.step(now, Some(1.0)).1;
        let before = ears.heard();
        frame(&mut module, &mut host, now);
        run_jobs(&mut module, &mut host);
        let said = ears.since(before);
        let Some(thunder) = thunder else {
            assert!(said.is_empty(), "{said:?}");
            continue;
        };
        thunders += 1;
        let [said] = &said[..] else {
            assert!(said.is_empty(), "{said:?}");
            continue;
        };
        heard += 1;
        let samples = said.split(": ").nth(1).unwrap().split(' ').next().unwrap();
        assert!(["51", "52", "53"].contains(&samples), "{said}");
        assert!(
            said.ends_with(&format!(" at {:.2}, once, faded in over 0 s", 0.8 * thunder.loudness)),
            "{said} {thunder:?}"
        );
    }
    assert!(thunders >= 10 && heard + 3 >= thunders, "{heard} of {thunders}");
    assert!(panel(&mut module, &mut host).contains(&"Thunder: StormPeaksLightning.".to_owned()));
    // In a room: the loop faded out, no thunder heard.
    host.buildings = Some(rooms);
    let before = ears.heard();
    for _ in 0..3_000 {
        now += 0.01;
        frame(&mut module, &mut host, now);
    }
    assert_eq!(ears.since(before), ["volume 3: 0.00 over 1 s"]);
    host.buildings = None;
    // The lightning off: the loop back, no thunder.
    host.settings.insert("lightning".to_owned(), json!(0));
    let before = ears.heard();
    for _ in 0..3_000 {
        now += 0.01;
        frame(&mut module, &mut host, now);
    }
    assert_eq!(ears.since(before), ["volume 3: 1.00 over 1 s"]);
    // The black rain, which the client plays nothing for: the loop faded out, nothing heard.
    host.settings.insert("kind".to_owned(), json!(4));
    frame(&mut module, &mut host, now + 0.01);
    assert_eq!(ears.since(before + 1), ["stop 3 over 3 s"]);
    assert!(
        !panel(&mut module, &mut host)
            .iter()
            .any(|text| text.starts_with("Heard"))
    );
    // The rain again.
    host.settings.insert("kind".to_owned(), json!(0));
    frame(&mut module, &mut host, now + 0.02);
    run_jobs(&mut module, &mut host);
    frame(&mut module, &mut host, now + 0.03);
    // The heavy rain the chosen intensity of 1 gives, then the module stopped: its loop stopped
    // at once.
    let before = ears.heard();
    module.shutdown();
    let last = *ears.played.lock().unwrap();
    assert_eq!(
        ears.since(before - 1),
        [
            format!("play {last}: 20 samples at 0.69, looped, faded in over 3 s"),
            format!("stop {last} over 0 s")
        ]
    );
    // Without a device: nothing heard, why said.
    *ears.silent.lock().unwrap() = Some("no device here".to_owned());
    frame(&mut module, &mut host, now + 0.04);
    assert!(panel(&mut module, &mut host).contains(&"Nothing heard: no device here.".to_owned()));
    // Without the service: nothing heard, said; in fine weather, nothing said.
    host.audio = None;
    frame(&mut module, &mut host, now + 0.04);
    assert!(panel(&mut module, &mut host).contains(&"Nothing heard: no service audio.".to_owned()));
    host.settings.insert("mode".to_owned(), json!(0));
    frame(&mut module, &mut host, now + 0.05);
    assert!(
        !panel(&mut module, &mut host)
            .iter()
            .any(|text| text.starts_with("Nothing heard"))
    );
}

#[test]
fn the_loop_is_stopped_on_the_service_it_plays_on_once_that_service_goes_or_changes() {
    let mut host = host();
    let first = Arc::new(Ears::default());
    host.audio = Some(first.clone());
    let mut module = WeatherModule::default();
    let mut reg = Registrar::default();
    module.register(&mut reg);
    frame(&mut module, &mut host, 0.0);
    run_jobs(&mut module, &mut host);
    server_sends(&mut module, &mut host, 5, 0.85);
    let mut now = 0.1;
    while first.heard() == 0 {
        assert!(now < 1.0, "never played");
        frame(&mut module, &mut host, now);
        run_jobs(&mut module, &mut host);
        now += 0.1;
    }
    // Another service, as when the module audio starts again: the loop stopped on the first at
    // once, played on the other.
    let second = Arc::new(Ears::default());
    host.audio = Some(second.clone());
    frame(&mut module, &mut host, now);
    assert_eq!(
        first.since(0),
        [
            "play 1: 20 samples at 0.69, looped, faded in over 3 s",
            "stop 1 over 0 s"
        ]
    );
    assert_eq!(
        second.since(0),
        ["play 1: 20 samples at 0.69, looped, faded in over 3 s"]
    );
    // The service gone: stopped on the one it played on.
    host.audio = None;
    frame(&mut module, &mut host, now + 0.1);
    assert_eq!(second.since(1), ["stop 1 over 0 s"]);
    assert!(panel(&mut module, &mut host).contains(&"Nothing heard: no service audio.".to_owned()));
}
