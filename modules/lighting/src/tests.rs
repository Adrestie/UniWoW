//! Tests of the light of a place and an hour on tables the tests write; those of the client are
//! read by the tests of `assets`.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::formats::{
    AnimationRecord, AreaRecord, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair, FileRef,
    Formats, GameObjectDisplay, HairGeoset, LightBand, LightParamsRecord, LightRecord, MapRecord, Model, Texture, Tile,
    Wdl, Wdt, Wmo,
};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::vfs::{Vfs, VfsState};
use uniwow_api::{
    CallId, CommandInfo, Context, Editor, EditorBackend, Event, JobFn, JobId, JobOutcome, Module, PropertyValue,
    Registrar, egui, egui_wgpu,
};

use crate::light::{Tables, colour_at, number_at};

/// A band of numbers of `keys`.
fn numbers(id: u32, keys: &[(u32, f32)]) -> LightBand<f32> {
    LightBand {
        id,
        keys: keys.to_vec(),
    }
}

/// A band of colours of `keys`, each a grey.
fn greys(id: u32, keys: &[(u32, u8)]) -> LightBand<[u8; 3]> {
    LightBand {
        id,
        keys: keys.iter().map(|(time, grey)| (*time, [*grey; 3])).collect(),
    }
}

#[test]
fn a_band_is_read_between_its_keys_and_past_midnight() {
    let band = numbers(1, &[(0, 0.0), (1440, 100.0)]);
    assert_eq!(number_at(&band, 720.0), Some(50.0));
    // Past the last key, towards the first of the next day.
    assert_eq!(number_at(&band, 2160.0), Some(50.0));
    assert_eq!(number_at(&band, 2880.0 + 720.0), Some(50.0), "the next day");
    // Before the first key, from the last of the day before.
    let band = numbers(1, &[(720, 10.0), (2160, 30.0)]);
    assert_eq!(number_at(&band, 0.0), Some(20.0));
    assert_eq!(number_at(&band, 720.0), Some(10.0));
    assert_eq!(number_at(&numbers(1, &[(600, 7.0)]), 100.0), Some(7.0), "a single key");
    assert_eq!(number_at(&numbers(1, &[]), 100.0), None, "no key");
    let colour = colour_at(&greys(1, &[(0, 0), (1440, 255)]), 1080.0).unwrap();
    assert!((colour[0] - 0.75).abs() < 1e-6, "{colour:?}");
}

/// A light of the map 0 at `place` in the world, of radii `radii` in yards, its first params
/// `params`, in the units of the file.
fn light(id: u32, place: [f32; 2], radii: [f32; 2], params: [u32; 2]) -> LightRecord {
    let middle = 17_066.666;
    LightRecord {
        id,
        map: 0,
        position: if place == [0.0; 2] {
            [0.0; 3]
        } else {
            [(middle - place[1]) * 36.0, 50.0 * 36.0, (middle - place[0]) * 36.0]
        },
        radii: radii.map(|radius| radius * 36.0),
        params: [params[0], params[1], 0, 0, 0, 0, 0, 0],
    }
}

fn params(id: u32, river: f32) -> LightParamsRecord {
    LightParamsRecord {
        id,
        highlight_sky: false,
        skybox: 0,
        cloud: 0,
        glow: river,
        river_alphas: [river, 1.0],
        ocean_alphas: [0.75, 1.0],
    }
}

/// Tables of a global light (params 1: diffuse 100, ambient 40, fog from 0.25 of 18,000) and of
/// local lights: 2 at 1,000, 0, whole within 100 yards, fading to 300, of params 2 (diffuse 200,
/// no ambient band, no fog), its params under the water 4 (diffuse 60, fog to 100 yards); 3 at
/// 1,100, 0, within 50 to 150 yards, of params 3 (diffuse 0).
fn tables() -> Tables {
    let lights = [
        light(1, [0.0; 2], [0.0; 2], [1, 0]),
        light(2, [1_000.0, 0.0], [100.0, 300.0], [2, 4]),
        light(3, [1_100.0, 0.0], [50.0, 150.0], [3, 0]),
    ];
    let records = [params(1, 0.1), params(2, 0.5), params(3, 0.9), params(4, 0.3)];
    // The bands of the params `p`: colours from 18 p − 17, numbers from 6 p − 5.
    let colours = [
        greys(1, &[(0, 100)]),
        greys(2, &[(0, 40)]),
        greys(19, &[(0, 200)]),
        greys(37, &[(0, 0)]),
        greys(55, &[(0, 60)]),
    ];
    let fog = [
        numbers(1, &[(0, 18_000.0)]),
        numbers(2, &[(0, 0.25)]),
        numbers(7, &[(0, 0.0)]),
        numbers(8, &[(0, 0.0)]),
        numbers(19, &[(0, 3_600.0)]),
        numbers(3, &[(0, 0.0), (1440, 10.0)]),
    ];
    Tables::new(&lights, &records, &colours, &fog)
}

fn diffuse(mixed: &crate::light::Mixed) -> f32 {
    mixed.values.colours[0].unwrap()[0] * 255.0
}

#[test]
fn the_global_light_is_mixed_with_the_local_ones_holding_the_place_the_farthest_first() {
    let tables = tables();
    // Far from the local lights: the global alone.
    let far = tables.light_at(0, [-5_000.0, 0.0], 0.0, 0, true).unwrap();
    assert_eq!(far.used, [(1, 1.0)]);
    assert!((diffuse(&far) - 100.0).abs() < 1e-3);
    assert!((far.values.colours[1].unwrap()[0] * 255.0 - 40.0).abs() < 1e-3);
    assert_eq!(far.values.numbers[0..2], [Some(18_000.0), Some(0.25)]);
    // Within the inner radius of the light 2, its height ignored: its diffuse, the global's ambient
    // and fog, as it has none.
    let within = tables.light_at(0, [920.0, -50.0], 0.0, 0, true).unwrap();
    assert_eq!(within.used, [(1, 1.0), (2, 1.0)]);
    assert!((diffuse(&within) - 200.0).abs() < 1e-3);
    assert!((within.values.colours[1].unwrap()[0] * 255.0 - 40.0).abs() < 1e-3);
    assert_eq!(within.values.numbers[0..2], [Some(18_000.0), Some(0.25)]);
    assert_eq!(within.values.river_alphas[0], 0.5);
    // Halfway between its radii: half of it.
    let between = tables.light_at(0, [800.0, 0.0], 0.0, 0, true).unwrap();
    assert_eq!(between.used.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1, 2]);
    assert!((between.used[1].1 - 0.5).abs() < 1e-4, "{:?}", between.used);
    assert!((diffuse(&between) - 150.0).abs() < 1e-3);
    // Within both 2 and 3: 2, the farthest, then 3, which weighs last.
    let both = tables.light_at(0, [1_090.0, 0.0], 0.0, 0, true).unwrap();
    assert_eq!(both.used.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(diffuse(&both).abs() < 1e-3, "the nearest last");
    // The local lights not mixed in.
    let alone = tables.light_at(0, [1_000.0, 0.0], 0.0, 0, false).unwrap();
    assert_eq!(alone.used, [(1, 1.0)]);
}

#[test]
fn the_params_of_a_slot_are_taken_and_those_of_the_first_where_a_light_has_none() {
    let tables = tables();
    // Under the water: the light 2 gives its params 4, the global its first, having none there.
    let under = tables.light_at(0, [920.0, -50.0], 0.0, 1, true).unwrap();
    assert!((diffuse(&under) - 60.0).abs() < 1e-3);
    assert_eq!(under.values.river_alphas[0], 0.3);
    // A local light's fog given where it has one.
    assert_eq!(under.values.numbers[0], Some(3_600.0));
    // A map without a global light of its own: the light 1.
    assert_eq!(tables.light_at(9, [0.0; 2], 0.0, 0, true).unwrap().used, [(1, 1.0)]);
    // Params unknown: none.
    assert!(
        Tables::new(&[light(1, [0.0; 2], [0.0; 2], [5, 0])], &[], &[], &[])
            .light_at(0, [0.0; 2], 0.0, 0, true)
            .is_none()
    );
}

#[test]
fn the_fog_of_a_global_light_that_gives_none_is_noggit_s() {
    let lights = [light(1, [0.0; 2], [0.0; 2], [1, 0])];
    let tables = Tables::new(&lights, &[params(1, 0.1)], &[], &[]);
    let light = tables.light_at(0, [0.0; 2], 0.0, 0, true).unwrap();
    assert_eq!(light.values.numbers[0..2], [Some(6_500.0), Some(0.1)]);
    assert_eq!(light.values.colours[0], None, "a band absent");
    // Given, but 0.
    let zero = [numbers(1, &[(0, 0.0)]), numbers(2, &[(0, 0.0)])];
    let tables = Tables::new(&lights, &[params(1, 0.1)], &[], &zero);
    let light = tables.light_at(0, [0.0; 2], 0.0, 0, true).unwrap();
    assert_eq!(light.values.numbers[0..2], [Some(6_500.0), Some(0.1)]);
}

#[test]
fn a_band_is_read_by_its_keys_in_the_order_of_their_times() {
    let tables = Tables::new(
        &[light(1, [0.0; 2], [0.0; 2], [1, 0])],
        &[params(1, 0.1)],
        &[],
        &[numbers(3, &[(1440, 4.0), (0, 2.0)])],
    );
    let light = tables.light_at(0, [0.0; 2], 720.0, 0, true).unwrap();
    assert_eq!(light.values.numbers[2], Some(3.0));
    // An hour before the day or past it, as within it.
    let band = numbers(1, &[(0, 0.0), (1440, 100.0)]);
    assert_eq!(number_at(&band, -720.0), number_at(&band, 2160.0));
    assert_eq!(number_at(&band, 2880.0 * 3.0 + 720.0), Some(50.0));
}

#[test]
fn a_light_of_equal_radii_holds_the_place_within_them_wholly() {
    let lights = [
        light(1, [0.0; 2], [0.0; 2], [1, 0]),
        light(2, [100.0, 0.0], [50.0, 50.0], [2, 0]),
    ];
    let tables = Tables::new(&lights, &[params(1, 0.1), params(2, 0.5)], &[], &[]);
    assert_eq!(
        tables.light_at(0, [140.0, 0.0], 0.0, 0, true).unwrap().used,
        [(1, 1.0), (2, 1.0)]
    );
    assert_eq!(tables.light_at(0, [160.0, 0.0], 0.0, 0, true).unwrap().used, [(1, 1.0)]);
    // Exactly on them: held, as by Noggit. A light over the middle of the world, its centre exact.
    let mut over = light(3, [0.0; 2], [64.0, 64.0], [2, 0]);
    over.position = [0.0, 36.0, 0.0];
    let middle = 32.0 * uniwow_api::formats::TILE;
    let tables = Tables::new(&[lights[0].clone(), over], &[params(1, 0.1), params(2, 0.5)], &[], &[]);
    assert_eq!(
        tables.light_at(0, [middle + 64.0, middle], 0.0, 0, true).unwrap().used,
        [(1, 1.0), (3, 1.0)]
    );
    // A global light of its own, not the light 1 of another map.
    assert!(!tables.light_at(0, [0.0; 2], 0.0, 0, true).unwrap().fallback);
    assert!(tables.light_at(9, [0.0; 2], 0.0, 0, true).unwrap().fallback);
}

#[test]
fn the_hour_set_turns_at_its_speed_past_midnight() {
    assert_eq!(crate::half_minutes(720.0, 0, 100.0), 1440.0, "still");
    assert_eq!(crate::half_minutes(720.0, 60, 2.0), 1680.0, "two hours in two seconds");
    assert_eq!(crate::half_minutes(1439.0, 1, 2.0), 2.0, "past midnight");
    // The speed alone changed: on from the hour reached, not from the hour set.
    let mut module = crate::LightingModule {
        set: Some((720, 60, 720.0, Instant::now() - Duration::from_secs(10))),
        ..Default::default()
    };
    let time = module.time(720, 0);
    assert!((time - 2640.0).abs() < 2.0, "{time}");
    assert!((module.time(720, 0) - time).abs() < 0.1, "then still");
    // The hour set again: from it.
    assert_eq!(module.time(600, 0), 1200.0);
}

#[test]
fn the_light_is_a_category_of_the_settings_noon_by_default_still() {
    let mut module = crate::LightingModule::default();
    let mut reg = Registrar::default();
    module.register(&mut reg);
    let category = reg.settings.expect("declared");
    assert_eq!(category.title, "Light");
    let settings: Vec<_> = category
        .settings
        .iter()
        .map(|spec| (spec.key.as_str(), spec.range, spec.default))
        .collect();
    assert_eq!(
        settings,
        [
            ("hour", [0, 1439], 720),
            ("speed", [0, 1440], 0),
            ("local_lights", [0, 1], 1)
        ]
    );
}

/// Formats that read nothing: the jobs of the tests are never run.
struct NoFormats;

impl Formats for NoFormats {
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String> {
        Err("none".to_owned())
    }
    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String> {
        Err("none".to_owned())
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

/// The client's files, in the state the test sets.
struct Files(Mutex<VfsState>);

impl Vfs for Files {
    fn read(&self, _path: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
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
        self.0.lock().unwrap().clone()
    }
}

/// An editor whose terrain shows the map `map` (none for `null`) and whose camera stands at
/// `camera`, unread when none.
struct Shown {
    map: Mutex<Value>,
    camera: Mutex<Option<[f64; 3]>>,
}

impl EditorBackend for Shown {
    fn commands(&self) -> Vec<CommandInfo> {
        Vec::new()
    }
    fn call(&self, _caller: &str, name: &str, _arguments: Value) -> Result<Value, String> {
        match name {
            "terrain.map" => Ok(self.map.lock().unwrap().clone()),
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
        match (path, *self.camera.lock().unwrap()) {
            ("viewport/camera_position", Some(at)) => Ok(PropertyValue::Vector(at)),
            _ => Err("unread".to_owned()),
        }
    }
}

/// A host offering `formats` and `vfs`, counting the jobs started, never run, and those cancelled,
/// its editor `editor`.
struct Host {
    formats: Arc<dyn Formats>,
    files: Arc<dyn Vfs>,
    started: Vec<String>,
    cancelled: Vec<JobId>,
    settings: HashMap<String, Value>,
    editor: Arc<Shown>,
}

fn host(files: Arc<Files>) -> Host {
    Host {
        formats: Arc::new(NoFormats),
        files,
        started: Vec::new(),
        cancelled: Vec::new(),
        settings: HashMap::new(),
        editor: Arc::new(Shown {
            map: Mutex::new(Value::Null),
            camera: Mutex::new(None),
        }),
    }
}

impl uniwow_api::Host for Host {
    fn publish(&mut self, _source: &str, _topic: &str, _payload: uniwow_api::serde_json::Value) {}
    fn execute(&mut self, _owner: &str, _command: Box<dyn uniwow_api::Command>) {}
    fn forget_document(&mut self, _owner: &str, _document: &str) {}
    fn service(&self, id: &str) -> Option<&(dyn Any + Send + Sync)> {
        match id {
            "formats" => Some(&self.formats),
            "vfs" => Some(&self.files),
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
    fn setting(&self, _module: &str, key: &str) -> Option<uniwow_api::serde_json::Value> {
        self.settings.get(key).cloned()
    }
    fn set_setting(&mut self, _module: &str, key: &str, value: uniwow_api::serde_json::Value) {
        self.settings.insert(key.to_owned(), value);
    }
    fn report_failure(&mut self, _reporter: &str, _culprit: &str, _message: &str) {}
    fn spawn(&mut self, _owner: &str, label: &str, _job: JobFn) -> JobId {
        self.started.push(label.to_owned());
        JobId(self.started.len() as u64)
    }
    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId {
        self.spawn(owner, label, job)
    }
    fn cancel(&mut self, _owner: &str, job: JobId) {
        self.cancelled.push(job);
    }
    fn call(&mut self, _caller: &str, _name: &str, _arguments: uniwow_api::serde_json::Value) -> CallId {
        CallId(1)
    }
    fn editor(&self, caller: &str) -> Editor {
        Editor::new(self.editor.clone(), caller)
    }
}

#[test]
fn the_tables_are_read_once_the_client_is_open_and_again_once_it_changes() {
    let files = Arc::new(Files(Mutex::new(VfsState::Opening)));
    let mut host = host(files.clone());
    let egui = egui::Context::default();
    let mut module = crate::LightingModule::default();
    // While the archives are opened: nothing read.
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert!(host.started.is_empty());
    // Open: read once.
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 3,
        files: 100,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(host.started.len(), 1);
    let refused: Result<Arc<Tables>, String> = Err("refused".to_owned());
    module.on_job(
        JobId(1),
        JobOutcome::Done(Box::new(refused)),
        &mut Context::new(&mut host, "lighting"),
    );
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(host.started.len(), 1, "refused, not read again by the same archives");
    // Other archives: read again.
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 4,
        files: 120,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(host.started.len(), 2);
    // The client closed while they are read: the read cancelled.
    *files.0.lock().unwrap() = VfsState::NoClient("none".to_owned());
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(host.cancelled, [JobId(2)]);
    assert_eq!(host.started.len(), 2);
    // A job of before coming back is not taken.
    let read: Result<Arc<Tables>, String> = Ok(Arc::new(Tables::default()));
    module.on_job(
        JobId(2),
        JobOutcome::Done(Box::new(read)),
        &mut Context::new(&mut host, "lighting"),
    );
    assert!(module.tables.is_none());
    // Cancelled from the jobs, or panicked: not read again by the same archives.
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 5,
        files: 130,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    module.on_job(
        JobId(3),
        JobOutcome::Cancelled,
        &mut Context::new(&mut host, "lighting"),
    );
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert!(matches!(module.tables, Some(Err(_))));
    assert_eq!(host.started.len(), 3);
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 6,
        files: 140,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    module.on_job(
        JobId(4),
        JobOutcome::Panicked("boom".to_owned()),
        &mut Context::new(&mut host, "lighting"),
    );
    assert_eq!(
        module.tables.as_ref().map(|read| read.clone().err()),
        Some(Some("boom".to_owned()))
    );
}

#[test]
fn the_light_is_that_of_the_map_shown_at_the_camera_s_place_on_it() {
    let files = Arc::new(Files(Mutex::new(VfsState::Ready { archives: 1, files: 1 })));
    let mut host = host(files);
    let mut module = crate::LightingModule {
        client: Some((1, 1)),
        tables: Some(Ok(Arc::new(tables()))),
        ..Default::default()
    };
    let egui = egui::Context::default();
    *host.editor.map.lock().unwrap() = json!({ "id": 0, "name": "Azeroth" });
    // Within the light 2 at 1,000, 0 on the map, high above it: its clear params, not those under
    // the water.
    *host.editor.camera.lock().unwrap() = Some([920.0, -50.0, 600.0]);
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    let shown = module.shown.as_ref().expect("a map shown");
    assert_eq!(
        (shown.map, shown.name.as_str(), shown.place),
        (0, "Azeroth", [920.0, -50.0])
    );
    let light = shown.light.as_ref().unwrap();
    assert_eq!(light.used, [(1, 1.0), (2, 1.0)]);
    // At noon by default, in half-minutes, the hour read by the bands.
    assert_eq!(shown.time, 1440.0);
    assert_eq!(light.values.numbers[2], Some(10.0));
    assert!((diffuse(light) - 200.0).abs() < 1e-3);
    // The local lights switched off.
    host.settings.insert("local_lights".to_owned(), json!(0));
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(module.shown.as_ref().unwrap().light.as_ref().unwrap().used, [(1, 1.0)]);
    // The camera unread: the light before kept; no map shown: none.
    *host.editor.camera.lock().unwrap() = None;
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(module.shown.as_ref().map(|shown| shown.place), Some([920.0, -50.0]));
    *host.editor.map.lock().unwrap() = Value::Null;
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert!(module.shown.is_none());
}
