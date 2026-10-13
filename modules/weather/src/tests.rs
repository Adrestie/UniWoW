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

use uniwow_api::buildings::{Buildings, Eye, EyeGroup};
use uniwow_api::formats::{
    AnimationRecord, AreaRecord, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair, FileRef,
    Formats, GameObjectDisplay, HairGeoset, MapRecord, Model, Texture, Tile, Wdl, Wdt, Wmo,
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

/// The client's files, open.
struct Files;

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

/// A host offering `formats` and `vfs`, keeping the jobs started for the test to run them.
struct Host {
    formats: Arc<dyn Formats>,
    files: Arc<dyn Vfs>,
    jobs: Vec<JobFn>,
    settings: HashMap<String, Value>,
    editor: Arc<Shown>,
}

fn host() -> Host {
    Host {
        formats: Arc::new(Areas),
        files: Arc::new(Files),
        jobs: Vec::new(),
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
        self.jobs.push(job);
        JobId(self.jobs.len() as u64)
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
    let job = host.jobs.remove(0);
    let read = job(&JobContext::new(
        Arc::default(),
        Arc::default(),
        Editor::new(host.editor.clone(), "weather"),
    ));
    module.on_job(
        JobId(1),
        JobOutcome::Done(read),
        &mut Context::new(&mut host, "weather"),
    );
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
