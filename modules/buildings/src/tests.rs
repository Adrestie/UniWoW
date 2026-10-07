//! Tests of the buildings: kept by their unique id, their vertex colours fixed as the client fixes
//! them, their geometry, their doodads against a fake service `models`, and the buildings drawn on
//! the software adapter of the system when it has one.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use uniwow_api::arena::Refusal;
use uniwow_api::formats::{
    AnimationRecord, AreaRecord, Building, CharSection, CreatureDisplay, CreatureLook, CreatureModel, DoodadSet,
    FacialHair, FileRef, Formats, GameObjectDisplay, HairGeoset, MapRecord, Model, ORIGIN, Texture, TextureFormat,
    Tile, TileId, Wdl, Wdt, Wmo, WmoBatch, WmoDoodad, WmoGroup, WmoMaterial,
};
use uniwow_api::glam::{Mat4, Quat, Vec3};
use uniwow_api::liquids::{Liquids, Surfaces};
use uniwow_api::models::{Extent, Instance, Look, LookId, LookState, Models};
use uniwow_api::viewport::{Layer, Phase, Target, View};
use uniwow_api::{egui, egui_wgpu, wgpu};

use crate::budget;
use crate::colours;
use crate::doodads::{self, Owners};
use crate::gpu::{self, Shared, State};
use crate::keeping::Kept;
use crate::layer::{self, BuildingsLayer, Placed, Scene};

fn building(unique_id: u32, file: &str) -> Building {
    Building {
        file: FileRef::Path(file.to_owned()),
        unique_id,
        position: [0.0; 3],
        rotation: [0.0; 3],
        bounds: [[0.0; 3]; 2],
        scale: 1.0,
        flags: 0,
        doodad_set: 0,
        name_set: 0,
    }
}

const A: TileId = TileId { x: 1, y: 1 };
const B: TileId = TileId { x: 2, y: 1 };

#[test]
fn a_building_listed_by_several_tiles_is_kept_once_while_one_of_them_is_held() {
    let mut kept = Kept::default();
    let none: Vec<u32> = Vec::new();
    assert_eq!(
        kept.hold(
            A,
            vec![building(1, "a.wmo"), building(2, "b.wmo"), building(1, "a.wmo")]
        ),
        (vec![1, 2], none.clone())
    );
    assert_eq!(
        kept.hold(B, vec![building(2, "b.wmo"), building(3, "c.wmo")]),
        (vec![3], none.clone()),
        "2 kept already"
    );
    assert_eq!(kept.len(), 3);
    let mut gone = kept.release(A);
    gone.sort();
    assert_eq!(gone, [1], "2 still listed by B");
    assert!(kept.get(2).is_some() && kept.holds(B) && !kept.holds(A));
    // Held again with what it lists now: what it brings, and what no tile lists any more.
    assert_eq!(
        kept.hold(B, vec![building(3, "c.wmo"), building(4, "d.wmo")]),
        (vec![4], vec![2])
    );
    assert!(kept.get(2).is_none(), "no longer listed");
    let mut all = kept.clear();
    all.sort();
    assert_eq!(all, [3, 4]);
    assert_eq!((kept.len(), kept.tiles().len()), (0, 0));
}

/// A group of four vertices, of a batch of transition over the first two when `transition`, its
/// colours `colours`.
fn coloured(outside: bool, transition: bool, colours: Vec<[u8; 4]>) -> WmoGroup {
    WmoGroup {
        flags: 0x4 | if outside { colours::OUTSIDE } else { 0x2000 },
        batch_counts: [u16::from(transition), 1, 0],
        batches: vec![WmoBatch {
            first: 0,
            count: 3,
            vertices: [0, 1],
            flags: 0,
            material: 0,
        }],
        colours: vec![colours],
        vertices: vec![[0.0; 3]; 4],
        ..WmoGroup::default()
    }
}

#[test]
fn the_vertex_colours_are_fixed_as_the_client_fixes_them() {
    let ambient = [20, 40, 60, 255];
    let colours = vec![[120, 120, 120, 128]; 4];
    let dark = |value: f32, ambient: f32| ((value - ambient) * (1.0 - 128.0 / 255.0) / 2.0).round() as u8;
    let light = |value: f32, ambient: f32| ((value * 128.0 / 64.0 + value - ambient) / 2.0).round() as u8;
    // The first two of transition: less the ambient, darkened by their alpha, halved, their alpha
    // kept; the others brightened by their alpha, less the ambient, halved, their alpha that of a
    // group inside.
    let mut group = coloured(false, true, colours.clone());
    colours::fix(&mut group, 0, ambient);
    assert_eq!(
        group.colours[0][0],
        [dark(120.0, 20.0), dark(120.0, 40.0), dark(120.0, 60.0), 128]
    );
    assert_eq!(
        group.colours[0][2],
        [light(120.0, 20.0), light(120.0, 40.0), light(120.0, 60.0), 0]
    );
    // Without transition, every vertex brightened; outside, its alpha 255.
    let mut outside = coloured(true, false, colours.clone());
    colours::fix(&mut outside, 0, ambient);
    assert_eq!(
        outside.colours[0][0],
        [light(120.0, 20.0), light(120.0, 40.0), light(120.0, 60.0), 255]
    );
    // Lit as one: the ambient taken as none.
    let mut one = coloured(false, false, colours.clone());
    colours::fix(&mut one, 0x2, ambient);
    let none = light(120.0, 0.0);
    assert_eq!(one.colours[0][1], [none, none, none, 0]);
    // Not fixed: the colours kept, the alpha after the transition set.
    let mut kept = coloured(true, true, colours.clone());
    colours::fix(&mut kept, 0x8, ambient);
    assert_eq!(
        kept.colours[0],
        [
            [120, 120, 120, 128],
            [120, 120, 120, 128],
            [120, 120, 120, 255],
            [120, 120, 120, 255]
        ]
    );
    // Held to a byte.
    let mut bright = coloured(false, false, vec![[255, 0, 255, 255]; 4]);
    colours::fix(&mut bright, 0, [0, 50, 0, 0]);
    assert_eq!(bright.colours[0][0], [255, 0, 255, 0]);
}

/// A building of one group: a square facing +X, 2 yards a side, its two triangles counterclockwise
/// seen from there, of the material `material`; inside or outside, its vertex colours `colours`.
fn square(material: WmoMaterial, inside: bool, colours: Option<[u8; 4]>) -> Wmo {
    let group = WmoGroup {
        flags: if inside { 0x2000 } else { 0x8 } | if colours.is_some() { 0x4 } else { 0 },
        bounds: [[0.0, -1.0, -1.0], [0.0, 1.0, 1.0]],
        batch_counts: [0, u16::from(inside), u16::from(!inside)],
        vertices: vec![[0.0, -1.0, -1.0], [0.0, 1.0, -1.0], [0.0, 1.0, 1.0], [0.0, -1.0, 1.0]],
        normals: vec![[1.0, 0.0, 0.0]; 4],
        coordinates: vec![vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]],
        colours: colours.map(|colour| vec![vec![colour; 4]]).unwrap_or_default(),
        triangles: vec![0, 1, 2, 0, 2, 3],
        batches: vec![WmoBatch {
            first: 0,
            count: 6,
            vertices: [0, 3],
            flags: 0,
            material: 0,
        }],
        ..WmoGroup::default()
    };
    Wmo {
        bounds: group.bounds,
        materials: vec![material],
        groups: vec![group],
        ..Wmo::default()
    }
}

fn material(flags: u32, blending: u32) -> WmoMaterial {
    WmoMaterial {
        flags,
        shader: 0,
        blending,
        textures: [None, None, None],
        emissive: [0; 4],
        diffuse: [0; 4],
        colour: [0; 4],
        ground: 0,
    }
}

#[test]
fn the_geometry_of_a_building_puts_its_groups_one_after_the_other() {
    let mut wmo = square(material(0, 0), false, Some([10, 20, 30, 40]));
    let mut second = wmo.groups[0].clone();
    second.colours.clear();
    second.coordinates.push(vec![[0.5, 0.5]; 4]);
    wmo.groups.push(second);
    let (vertices, indices, starts) = gpu::geometry(&mut wmo);
    assert_eq!((vertices.len(), starts), (8, vec![0, 6]));
    assert_eq!(
        indices[6..],
        [4, 5, 6, 4, 6, 7],
        "the second group's after the first's vertices"
    );
    assert_eq!(vertices[0].normal, [127, 0, 0, 0]);
    assert_eq!(vertices[2].uv, [[1.0, 1.0], [0.0, 0.0]]);
    assert_eq!(vertices[5].uv[1], [0.5, 0.5]);
    // Fixed as the client fixes them, outside: brightened, halved, alpha 255; none, black.
    let light = ((10.0 * 40.0 / 64.0 + 10.0) / 2.0f32).round() as u8;
    assert_eq!(vertices[0].colours[0][0], light);
    assert_eq!(vertices[0].colours[0][3], 255);
    assert_eq!(vertices[4].colours, [[0, 0, 0, 255]; 2]);
}

#[test]
fn a_box_is_in_sight_when_it_reaches_inside_every_side_of_the_view() {
    let view_proj = Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1)
        * Mat4::look_at_rh(Vec3::new(10.0, 0.0, 0.0), Vec3::ZERO, Vec3::Z);
    let planes = layer::planes(&view_proj);
    let unit = |at: Vec3| [at - Vec3::ONE, at + Vec3::ONE];
    assert!(layer::in_sight(&planes, &unit(Vec3::ZERO)));
    assert!(
        layer::in_sight(&planes, &unit(Vec3::new(-1000.0, 0.0, 0.0))),
        "far, the view endless"
    );
    assert!(
        !layer::in_sight(&planes, &unit(Vec3::new(20.0, 0.0, 0.0))),
        "behind the eye"
    );
    assert!(!layer::in_sight(&planes, &unit(Vec3::new(0.0, 30.0, 0.0))), "aside");
    assert!(!layer::in_sight(&planes, &unit(Vec3::new(0.0, 0.0, -30.0))), "below");
    assert!(
        layer::in_sight(&planes, &[Vec3::new(0.0, -30.0, -1.0), Vec3::new(0.0, 30.0, 1.0)]),
        "across it"
    );
    assert!(
        layer::in_sight(&planes, &[Vec3::new(-1000.0, -1.0, -1.0), Vec3::new(1000.0, 1.0, 1.0)]),
        "along it, from behind the eye"
    );
    // A box turned a quarter about Z: its bounds those of its corners moved.
    let turned = Mat4::from_rotation_translation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2), Vec3::X);
    let bounds = layer::world_bounds(&turned, &[[0.0, 0.0, 0.0], [2.0, 1.0, 1.0]]);
    assert!(bounds[0].abs_diff_eq(Vec3::new(0.0, 0.0, 0.0), 1e-5), "{bounds:?}");
    assert!(bounds[1].abs_diff_eq(Vec3::new(1.0, 2.0, 1.0), 1e-5), "{bounds:?}");
}

/// The sets of instances of each owner, and their flags, as the service keeps them.
#[derive(Default)]
struct FakeModels {
    looks: Mutex<Vec<Look>>,
    owners: Mutex<BTreeMap<String, Vec<Instance>>>,
    shown: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
}

impl Models for FakeModels {
    fn look(&self, look: &Look) -> LookId {
        let mut looks = self.looks.lock().unwrap();
        if let Some(at) = looks.iter().position(|known| known == look) {
            return LookId(at as u32);
        }
        looks.push(look.clone());
        LookId(looks.len() as u32 - 1)
    }
    fn display(&self, _display: u32) -> Result<(Look, f32), String> {
        Err("not asked".to_owned())
    }
    fn object(&self, _display: u32) -> Result<Option<Look>, String> {
        Err("not asked".to_owned())
    }
    fn place(&self, owner: &str, instances: &[Instance]) {
        self.owners.lock().unwrap().insert(owner.to_owned(), instances.to_vec());
    }
    fn change(&self, _owner: &str, _changed: &[Instance], _removed: &[u64]) {}
    fn clear(&self, owner: &str) {
        self.owners.lock().unwrap().remove(owner);
        self.shown.lock().unwrap().remove(owner);
    }
    fn shown(&self, owner: &str) -> Arc<AtomicBool> {
        self.shown
            .lock()
            .unwrap()
            .entry(owner.to_owned())
            .or_insert_with(|| Arc::new(AtomicBool::new(true)))
            .clone()
    }
    fn state(&self, _look: LookId) -> LookState {
        LookState::Waiting
    }
    fn extent(&self, _look: LookId) -> Option<Extent> {
        None
    }
}

fn doodad(file: &str, position: [f32; 3]) -> WmoDoodad {
    WmoDoodad {
        file: FileRef::Path(file.to_owned()),
        flags: 0,
        position,
        // A quarter turn about Z.
        rotation: [
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ],
        scale: 2.0,
        colour: [0; 4],
    }
}

fn set(first: u32, count: u32) -> DoodadSet {
    DoodadSet {
        name: String::new(),
        first,
        count,
    }
}

#[test]
fn the_doodads_of_a_building_are_those_of_its_set_0_and_of_the_set_it_names() {
    let models = FakeModels::default();
    let sets = [set(0, 2), set(2, 1), set(3, 1)];
    let doodads = vec![
        doodad("a.mdx", [1.0, 0.0, 0.0]),
        doodad("", [0.0; 3]),
        doodad("b.mdx", [0.0, 0.0, 0.0]),
        doodad("c.mdx", [0.0, 0.0, 0.0]),
    ];
    let transform = Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0));
    let holders = vec![Vec::new(); doodads.len()];
    let ids = |parts: &[(Vec<u16>, Vec<Instance>)]| {
        parts
            .iter()
            .flat_map(|(_, instances)| instances.iter().map(|instance| instance.id))
            .collect::<Vec<_>>()
    };
    let only_first = doodads::instances(&models, &transform, &sets, &doodads, &holders, 0);
    assert_eq!(ids(&only_first), [0], "the empty name left out");
    let named = doodads::instances(&models, &transform, &sets, &doodads, &holders, 2);
    assert_eq!(ids(&named), [0, 3]);
    assert_eq!(
        ids(&doodads::instances(&models, &transform, &sets, &doodads, &holders, 9)),
        [0],
        "a set it lacks"
    );
    let named = &named[0].1;
    // At the building's transform times its own: moved, turned a quarter, scaled.
    let placed = named[0].transform;
    assert!(
        placed
            .transform_point3(Vec3::ZERO)
            .abs_diff_eq(Vec3::new(101.0, 0.0, 0.0), 1e-5)
    );
    assert!(
        placed
            .transform_vector3(Vec3::X)
            .abs_diff_eq(Vec3::new(0.0, 2.0, 0.0), 1e-5)
    );
    let looks = models.looks.lock().unwrap().clone();
    assert_eq!(looks[named[1].look.0 as usize].model, FileRef::Path("c.mdx".to_owned()));
}

#[test]
fn the_groups_holding_each_doodad_are_listed_once_in_order() {
    let group = |refs: Vec<u16>| WmoGroup {
        doodad_refs: refs,
        ..WmoGroup::default()
    };
    let groups = [group(vec![0, 1]), group(vec![1, 1, 2])];
    assert_eq!(doodads::holders(&groups, 4), [vec![0], vec![0, 1], vec![1], vec![]]);
    assert_eq!(doodads::owner("Map", 7, &[0, 1]), "buildings/Map/7/0+1");
    assert_eq!(doodads::owner("Map", 7, &[]), "buildings/Map/7/-");
}

#[test]
fn the_doodads_of_a_building_are_placed_while_it_is_kept_and_taken_away_after() {
    let models = FakeModels::default();
    let mut owners = Owners::default();
    let instances = doodads::instances(
        &models,
        &Mat4::IDENTITY,
        &[set(0, 1)],
        &[doodad("a.mdx", [0.0; 3])],
        &[vec![2, 5]],
        0,
    );
    owners.want(&models, "Map", HashSet::from([1]));
    assert!(!owners.place(&models, "Map", 2, &instances), "not wanted");
    assert!(!owners.place(&models, "Other", 1, &instances), "of another map");
    assert!(owners.place(&models, "Map", 1, &instances));
    assert_eq!(
        models.owners.lock().unwrap().keys().collect::<Vec<_>>(),
        ["buildings/Map/1/2+5"]
    );
    let parts = owners.parts();
    assert_eq!(parts[&1][0].0, [2, 5]);
    assert!(
        Arc::ptr_eq(&parts[&1][0].1, &models.shown("buildings/Map/1/2+5")),
        "its flag"
    );
    // Placed again in other parts: those before taken away.
    let moved = doodads::instances(
        &models,
        &Mat4::IDENTITY,
        &[set(0, 1)],
        &[doodad("a.mdx", [0.0; 3])],
        &[Vec::new()],
        0,
    );
    assert!(owners.place(&models, "Map", 1, &moved));
    assert_eq!(
        models.owners.lock().unwrap().keys().collect::<Vec<_>>(),
        ["buildings/Map/1/-"]
    );
    assert!(
        owners.want(&models, "Map", HashSet::new()),
        "placed and no longer wanted"
    );
    owners.settle(&models);
    assert!(models.owners.lock().unwrap().is_empty());
    owners.want(&models, "Map", HashSet::from([1]));
    owners.place(&models, "Map", 1, &instances);
    assert!(
        !owners.want(&models, "Other", HashSet::from([1])),
        "another map: taken away at once"
    );
    assert!(models.owners.lock().unwrap().is_empty());
    assert_eq!(owners.placed(), 0);
}

// Drawn on the software adapter.

/// The formats of the tests: no file, nor any texture.
pub(crate) struct NoFiles;

impl Formats for NoFiles {
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
    fn texture(&self, file: &FileRef) -> Result<Texture, String> {
        match file {
            // White, its alpha a mask of nothing.
            FileRef::Path(path) if path == "masked.blp" => Ok(Texture {
                width: 4,
                height: 4,
                format: TextureFormat::Rgba8,
                levels: vec![[255, 255, 255, 0].repeat(16)],
            }),
            _ => Err("none".to_owned()),
        }
    }
    fn texture_rgba(&self, file: &FileRef) -> Result<Texture, String> {
        self.texture(file)
    }
}

fn resolved<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// A device of the software adapter of the system, offering what it offers of what the buildings
/// are drawn with; or none.
pub(crate) fn device() -> Option<egui_wgpu::RenderState> {
    device_with(wgpu::Limits::default().max_buffer_size)
}

/// The device of `device`, its buffers of `largest` bytes at most.
fn device_with(largest: u64) -> Option<egui_wgpu::RenderState> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: true,
        ..Default::default()
    }))?
    .ok()?;
    let mut limits = wgpu::Limits::default();
    limits.max_sampled_textures_per_shader_stage = adapter.limits().max_sampled_textures_per_shader_stage.min(128);
    limits.max_buffer_size = largest;
    let (device, queue) = resolved(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: adapter.features() & wgpu::Features::INDIRECT_FIRST_INSTANCE,
        required_limits: limits,
        ..Default::default()
    }))?
    .ok()?;
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

pub(crate) const TARGET: Target = Target {
    color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
    depth_format: wgpu::TextureFormat::Depth32Float,
    sample_count: 1,
    depth_compare: wgpu::CompareFunction::Greater,
};

/// The view from `eye` towards the origin, 32 × 32 pixels.
pub(crate) fn view(eye: Vec3) -> View {
    View {
        view_proj: Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1)
            * Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Z),
        view: Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Z),
        eye,
        size: [32, 32],
        time: 0.0,
        fog: Default::default(),
        sun: Default::default(),
    }
}

/// The middle pixel of what `layer` draws seen from `eye` towards the origin, cleared to black.
fn middle(gpu: &egui_wgpu::RenderState, layer: &mut BuildingsLayer, eye: Vec3) -> [u8; 4] {
    middle_in(gpu, layer, eye, &Phase::ALL)
}

/// The middle pixel of what `layer` draws in `phases` seen from `eye` towards the origin.
fn middle_in(gpu: &egui_wgpu::RenderState, layer: &mut BuildingsLayer, eye: Vec3, phases: &[Phase]) -> [u8; 4] {
    let size = 32u32;
    let view = view(eye);
    layer.prepare(gpu, &view);
    let texture = |format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size,
                height: size,
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
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(256 * size),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &colour.create_view(&Default::default()),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.create_view(&Default::default()),
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for phase in phases {
            layer.draw_pass(gpu, &TARGET, &view, *phase, &mut pass);
        }
    }
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
                bytes_per_row: Some(256),
                rows_per_image: Some(size),
            },
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    pixels.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let read = pixels.slice(..).get_mapped_range().expect("mapped").to_vec();
    let at = (16 * 256 + 16 * 4) as usize;
    [read[at], read[at + 1], read[at + 2], read[at + 3]]
}

/// A layer drawing `wmo` at the origin on the device of `gpu`; none when the device cannot draw
/// buildings.
fn bench(gpu: &egui_wgpu::RenderState, wmo: Wmo) -> Option<BuildingsLayer> {
    let shared = Arc::new(Shared::new(gpu, &TARGET).ok()?);
    let scene = Arc::new(Mutex::new(Scene::default()));
    let uploaded = gpu::upload(&shared, &NoFiles, wmo).unwrap();
    scene.lock().unwrap().placed = vec![Placed {
        transform: Mat4::IDENTITY,
        wmo: Arc::new(uploaded),
        parts: None,
    }];
    Some(BuildingsLayer::new(shared, scene))
}

#[test]
fn a_building_is_drawn_its_back_culled_but_where_two_sided() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    let Some(mut layer) = bench(&gpu, square(material(0x1, 0), false, None)) else {
        eprintln!("skipped: the software adapter does not draw buildings");
        return;
    };
    assert_eq!(
        middle(&gpu, &mut layer, front),
        [255, 255, 255, 255],
        "unlit, white without texture"
    );
    assert_eq!(middle(&gpu, &mut layer, -front), [0, 0, 0, 255], "its back culled");
    let mut two_sided = bench(&gpu, square(material(0x1 | 0x4, 0), false, None)).unwrap();
    assert_eq!(middle(&gpu, &mut two_sided, -front), [255, 255, 255, 255]);
    assert_eq!(
        layer.stats().triangles,
        2,
        "seen from behind: listed, its faces culled by the GPU"
    );
}

#[test]
fn inside_a_building_is_lit_by_its_vertex_colours() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    // A red vertex colour inside: wholly the light inside.
    let Some(mut red) = bench(&gpu, square(material(0, 0), true, Some([200, 0, 0, 0]))) else {
        return;
    };
    let lit = middle(&gpu, &mut red, front);
    assert!(lit[0] > 100 && lit[1] < 10 && lit[2] < 10, "{lit:?}");
    // Outside, lit by the sun of the view whatever its colours.
    let mut outside = bench(&gpu, square(material(0, 0), false, Some([200, 0, 0, 0]))).unwrap();
    let sunlit = middle(&gpu, &mut outside, front);
    assert!(
        sunlit[0] == sunlit[1] && sunlit[1] == sunlit[2] && sunlit[0] > 50,
        "{sunlit:?}"
    );
}

#[test]
fn the_opaque_batches_are_drawn_by_state_and_the_blended_from_the_farthest() {
    let Some(gpu) = device() else {
        return;
    };
    let Some(shared) = Shared::new(&gpu, &TARGET).ok().map(Arc::new) else {
        return;
    };
    // Four groups: two blended at 10 and 30 yards from the eye, an opaque two-sided one and an
    // opaque one.
    let mut wmo = square(material(0, 0), false, None);
    wmo.materials = vec![material(0, 2), material(0x4, 0), material(0, 0)];
    let at = |x: f32, material: u16| {
        let mut group = wmo.groups[0].clone();
        group.bounds = [[x, -1.0, -1.0], [x, 1.0, 1.0]];
        group.batches[0].material = material;
        group
    };
    wmo.groups = vec![at(10.0, 0), at(-10.0, 0), at(0.0, 1), at(0.0, 2)];
    wmo.bounds = [[-10.0, -1.0, -1.0], [10.0, 1.0, 1.0]];
    let uploaded = Arc::new(gpu::upload(&shared, &NoFiles, wmo).unwrap());
    // And another, far aside, out of sight.
    let placed = vec![
        Placed {
            transform: Mat4::IDENTITY,
            wmo: uploaded.clone(),
            parts: None,
        },
        Placed {
            transform: Mat4::from_translation(Vec3::new(0.0, 500.0, 0.0)),
            wmo: uploaded,
            parts: None,
        },
    ];
    let listing = layer::list(&placed, &view(Vec3::new(20.0, 0.0, 0.0)), None);
    let (opaque, blended) = (&listing.opaque, &listing.near);
    assert!(listing.beyond.is_empty(), "no water");
    assert_eq!((listing.buildings, listing.groups), (1, 4));
    assert_eq!(
        opaque.iter().map(|listed| listed.state).collect::<Vec<_>>(),
        [
            State {
                blending: 0,
                two_sided: false
            },
            State {
                blending: 0,
                two_sided: true
            }
        ]
    );
    assert_eq!(
        blended.iter().map(|listed| listed.distance.round()).collect::<Vec<_>>(),
        [30.0, 10.0]
    );
    let runs = layer::runs(opaque, 0);
    assert_eq!((runs.len(), runs[1].first), (2, 1));
    assert_eq!(layer::runs(blended, 2)[0].count, 2, "one state, one run");
}

#[test]
fn a_batch_is_of_a_transition_inside_or_outside_by_its_place_in_its_group() {
    let counts = [2, 3, 1];
    let kinds: Vec<u32> = (0..6).map(|index| gpu::kind(index, counts)).collect();
    assert_eq!(kinds, [0, 0, 1, 1, 1, 2]);
    assert_eq!(gpu::kind(0, [0, 0, 4]), 2);
}

#[test]
fn a_batch_outside_in_a_group_inside_is_lit_by_the_sun() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    // The outer wall of a room: a group inside, its batch outside, its vertex colours red.
    let mut wall = square(material(0, 0), true, Some([200, 0, 0, 255]));
    wall.groups[0].batch_counts = [0, 0, 1];
    let Some(mut layer) = bench(&gpu, wall) else {
        return;
    };
    let lit = middle(&gpu, &mut layer, front);
    assert!(lit[0] == lit[1] && lit[1] == lit[2] && lit[0] > 50, "{lit:?}");
    // Of a transition: blended by the alpha of its colours, here wholly inside.
    let mut door = square(material(0, 0), true, Some([200, 0, 0, 0]));
    door.groups[0].batch_counts = [1, 0, 0];
    let mut layer = bench(&gpu, door).unwrap();
    let lit = middle(&gpu, &mut layer, front);
    assert!(lit[0] > 50 && lit[1] < 10, "{lit:?}");
}

#[test]
fn an_opaque_batch_keeps_its_pixels_whatever_the_alpha_of_its_texture() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    let masked = |blending| {
        let mut material = material(0x1, blending);
        material.textures[0] = Some(FileRef::Path("masked.blp".to_owned()));
        square(material, false, None)
    };
    let Some(mut opaque) = bench(&gpu, masked(0)) else {
        return;
    };
    assert_eq!(middle(&gpu, &mut opaque, front), [255, 255, 255, 255]);
    // Alpha-keyed, not drawn under its key.
    let mut keyed = bench(&gpu, masked(1)).unwrap();
    assert_eq!(middle(&gpu, &mut keyed, front), [0, 0, 0, 255]);
}

#[test]
fn a_building_lit_as_one_is_lit_as_outside_whatever_its_batches() {
    let Some(gpu) = device() else {
        return;
    };
    // Inside, of nearly black colours, as Stormwind's groups are.
    let mut dark = square(material(0, 0), true, Some([2, 2, 2, 255]));
    dark.flags = 0xF;
    let Some(mut layer) = bench(&gpu, dark) else {
        return;
    };
    let lit = middle(&gpu, &mut layer, Vec3::new(5.0, 0.0, 0.0));
    assert!(lit[0] > 50 && lit[0] == lit[1], "{lit:?}");
    // Its ambient colour added at the drawing.
    let mut red = square(material(0, 0), true, Some([2, 2, 2, 255]));
    red.flags = 0xF;
    red.ambient = [120, 0, 0, 255];
    let mut layer = bench(&gpu, red).unwrap();
    let reddened = middle(&gpu, &mut layer, Vec3::new(5.0, 0.0, 0.0));
    assert!(
        reddened[0] > lit[0] && reddened[1] == lit[1],
        "{reddened:?} against {lit:?}"
    );
}

#[test]
fn a_transition_adds_the_light_outside_by_its_alpha_to_its_colours() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    // Wholly outside: its red colour darkened to nothing by the fix, lit by the sun alone.
    let mut door = square(material(0, 0), true, Some([200, 0, 0, 255]));
    door.groups[0].batch_counts = [1, 0, 0];
    let Some(mut layer) = bench(&gpu, door) else {
        return;
    };
    let outside = middle(&gpu, &mut layer, front);
    assert!(outside[0] == outside[1] && outside[0] > 50, "{outside:?}");
    // Half: its colour as the fix left it, whole, and half the sun added, grey.
    let mut half = square(material(0, 0), true, Some([200, 0, 0, 128]));
    half.groups[0].batch_counts = [1, 0, 0];
    let mut fixed = half.groups[0].clone();
    colours::fix(&mut fixed, half.flags, half.ambient);
    let left = (f32::from(fixed.colours[0][0][0]) * 2.0 / 255.0).min(1.0);
    let mut layer = bench(&gpu, half).unwrap();
    let blended = middle(&gpu, &mut layer, front);
    assert!(blended[1] > 20, "{blended:?}");
    let linear = |gamma: f32| {
        if gamma <= 0.04045 {
            gamma / 12.92
        } else {
            ((gamma + 0.055) / 1.055).powf(2.4)
        }
    };
    let [red, green] = [blended[0], blended[1]].map(|value| linear(f32::from(value) / 255.0));
    assert!(
        (red - green - linear(left)).abs() < 0.02,
        "{blended:?}: {} against {}",
        red - green,
        linear(left)
    );
}

#[test]
fn the_ground_of_a_building_and_how_far_the_eye_lies_from_it() {
    let mut placed = building(1, "a.wmo");
    // In the axes of its file: X from 10 to 30, Z from 100 to 140.
    placed.bounds = [[10.0, 0.0, 100.0], [30.0, 50.0, 140.0]];
    let ground = budget::ground(&placed);
    assert_eq!(
        ground,
        [[ORIGIN - 140.0, ORIGIN - 30.0], [ORIGIN - 100.0, ORIGIN - 10.0]]
    );
    assert_eq!(budget::distance([ORIGIN - 120.0, ORIGIN - 20.0], ground), 0.0, "inside");
    assert_eq!(budget::distance([ORIGIN - 90.0, ORIGIN - 20.0], ground), 10.0);
    assert_eq!(
        budget::distance([ORIGIN - 97.0, ORIGIN - 6.0], ground),
        5.0,
        "from a corner"
    );
}

fn file(key: u32, distance: f32, held: budget::Held) -> budget::File<u32> {
    budget::File {
        key,
        distance,
        held,
        arenas: None,
    }
}

#[test]
fn the_files_are_told_by_bands_and_read_within_what_the_budget_allows() {
    use budget::Held::{Loading, NoRoom, Ready, Refused, Waiting};
    use uniwow_api::viewport::{Allowance, BAND};
    let files = [
        file(1, 10.0, Ready(100)),
        file(2, BAND * 3.5, Waiting),
        file(3, BAND * 1.5, Loading),
        file(4, BAND * 2.0, Refused),
        file(5, BAND * 0.5, Waiting),
        file(6, BAND * 9.0, Ready(50)),
        file(7, BAND * 2.5, Waiting),
        file(8, BAND * 4.0, Ready(30)),
    ];
    // Wanted at what those on the GPU take on average, or at the size expected while none is.
    assert_eq!(budget::expected(&files), 60);
    assert_eq!(budget::expected(&files[1..5]), budget::EXPECTED);
    let demand = budget::demand(&files, 1000, 7, f32::INFINITY);
    assert_eq!(demand.fixed, 1000);
    assert_eq!(
        (demand.held[0], demand.wanted[0]),
        (100, 107),
        "file 1 held, file 5 wanted"
    );
    assert_eq!(
        (demand.held[9], demand.wanted[1], demand.wanted[2], demand.wanted[3]),
        (50, 7, 7, 7)
    );
    assert_eq!(demand.held.iter().sum::<u64>(), 180);
    // Within 3 bands to load, 5 to keep: the nearest waiting read, as many as the slots left; file
    // 2 beyond the reach to load not read, file 8 within the reach to keep kept.
    let allowance = Allowance {
        load: BAND * 3.0,
        keep: BAND * 5.0,
        ..Allowance::default()
    };
    let plan = |start: Vec<u32>, waiting| budget::Plan {
        start,
        release: vec![6],
        waiting,
    };
    let all = [f32::INFINITY; 2];
    assert_eq!(budget::plan(&files, &allowance, all, 9), plan(vec![5, 7], false));
    assert_eq!(budget::plan(&files, &allowance, all, 3), plan(vec![5, 7], false));
    assert_eq!(
        budget::plan(&files, &allowance, all, 2),
        plan(vec![5], true),
        "one loading already, file 7 waiting its turn"
    );
    assert_eq!(budget::plan(&files, &allowance, all, 1), plan(vec![], true));
    // All fits: every waiting read, nothing let go.
    assert_eq!(
        budget::plan(&files, &Allowance::default(), all, 9),
        budget::Plan {
            start: vec![5, 7, 2],
            release: vec![],
            waiting: false
        }
    );
    // The arenas with room for a band to load and four to keep: file 7 not read, file 8 let go,
    // and not wanted from the budget past the room to load.
    assert_eq!(
        budget::plan(&files, &allowance, [BAND, BAND * 4.0], 9),
        budget::Plan {
            start: vec![5],
            release: vec![6, 8],
            waiting: false
        }
    );
    let demand = budget::demand(&files, 1000, 7, BAND);
    assert_eq!((demand.wanted[0], demand.wanted[1..].iter().sum::<u64>()), (107, 80));
    // How far they fit: file 1 in the arenas by what it takes, the others at that on average.
    let mut held = files.clone();
    held[0].arenas = Some([60, 40]);
    assert_eq!(
        budget::room(&held, [100, 1_000], 1.0),
        BAND * 0.5,
        "files 1 and 5 pass 100"
    );
    assert_eq!(
        budget::room(&held, [300, 1_000], 1.0),
        BAND * 4.0,
        "five of 60, then file 8"
    );
    // Refused for want of room: still wanted, not read until room may have been made.
    let room = [file(9, BAND * 0.2, NoRoom)];
    assert_eq!(budget::demand(&room, 0, 7, f32::INFINITY).wanted[0], 7);
    assert_eq!(
        budget::plan(&room, &Allowance::default(), all, 9).start,
        Vec::<u32>::new()
    );
}

#[test]
fn a_file_refused_for_want_of_room_waits_until_a_range_is_given_back_or_the_camera_moves() {
    use uniwow_api::arena::MOVED;
    let refused = crate::FileState::NoRoom(5, [0.0, 0.0]);
    assert_eq!(crate::held(&refused, 5, [MOVED, 0.0]), budget::Held::NoRoom);
    assert_eq!(crate::held(&refused, 6, [0.0, 0.0]), budget::Held::Waiting);
    assert_eq!(crate::held(&refused, 5, [0.0, MOVED + 1.0]), budget::Held::Waiting);
    assert_eq!(
        crate::held(&crate::FileState::Refused, 6, [0.0; 2]),
        budget::Held::Refused
    );
}

#[test]
fn a_file_refused_for_want_of_room_is_put_on_the_gpu_once_a_range_is_given_back() {
    // Buffers of 32 KB: 819 vertices; a file of 120 squares, 480 vertices, held once, not twice.
    let Some(gpu) = device_with(32 << 10) else {
        return;
    };
    let Ok(shared) = Shared::new(&gpu, &TARGET) else {
        eprintln!("skipped: the device does not draw buildings");
        return;
    };
    let shared = Arc::new(shared);
    let file = || {
        let mut wmo = square(material(0, 0), false, None);
        wmo.groups = vec![wmo.groups[0].clone(); 120];
        wmo
    };
    let first = gpu::upload(&shared, &NoFiles, file()).unwrap();
    assert_eq!(first.arenas(), [480 * 40, 720 * 4]);
    let given = shared.given();
    assert!(
        matches!(gpu::upload(&shared, &NoFiles, file()), Err(Refusal::NoRoom(_))),
        "no room for a second"
    );
    drop(first);
    assert_eq!(
        shared.given(),
        given + 3,
        "its vertices, indices and materials given back"
    );
    assert!(gpu::upload(&shared, &NoFiles, file()).is_ok(), "room made");
}

#[test]
fn a_blended_group_beyond_the_surface_of_the_water_from_the_eye_is_drawn_before_it() {
    let Some(gpu) = device() else {
        return;
    };
    let Ok(shared) = Shared::new(&gpu, &TARGET) else {
        return;
    };
    // Two blended squares, one under the surface at 0, one over it.
    let mut wmo = square(material(0, 2), false, None);
    let at = |z: f32| {
        let mut group = wmo.groups[0].clone();
        group.bounds = [[0.0, -1.0, z - 1.0], [0.0, 1.0, z + 1.0]];
        group
    };
    wmo.groups = vec![at(-5.0), at(5.0)];
    wmo.bounds = [[0.0, -1.0, -6.0], [0.0, 1.0, 6.0]];
    let placed = [Placed {
        transform: Mat4::IDENTITY,
        wmo: Arc::new(gpu::upload(&Arc::new(shared), &NoFiles, wmo).unwrap()),
        parts: None,
    }];
    let surfaces = Surfaces::from_cells(
        (-40..=40).flat_map(|x| (-10..=10).map(move |y| (Surfaces::cell(x as f32, y as f32), 0.0))),
    );
    // The first index of what is drawn beyond the water and on the eye's side.
    let firsts = |eye: Vec3, surfaces: Option<&Surfaces>| {
        let listing = layer::list(&placed, &view(eye), surfaces);
        let first = |listed: &[layer::Listed]| listed.iter().map(|listed| listed.command[2]).collect::<Vec<_>>();
        (first(&listing.beyond), first(&listing.near))
    };
    let (under, over) = (0, 6);
    let base = firsts(Vec3::new(20.0, 0.0, 10.0), None).1.into_iter().min().unwrap();
    let shift = |(beyond, near): (Vec<u32>, Vec<u32>)| {
        (
            beyond.iter().map(|first| first - base).collect::<Vec<_>>(),
            near.iter().map(|first| first - base).collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        shift(firsts(Vec3::new(20.0, 0.0, 10.0), Some(&surfaces))),
        (vec![under], vec![over]),
        "from over the water"
    );
    assert_eq!(
        shift(firsts(Vec3::new(20.0, 0.0, -10.0), Some(&surfaces))),
        (vec![over], vec![under]),
        "from under it"
    );
    assert_eq!(
        shift(firsts(Vec3::new(20.0, 0.0, 10.0), None)).0,
        Vec::<u32>::new(),
        "without water, all on the eye's side"
    );
    // Drawn in the phase of its part: a white group under the surface, from over the water.
    let shared = Arc::new(Shared::new(&gpu, &TARGET).unwrap());
    let mut white = square(material(0x1, 2), false, None);
    white.groups[0].bounds = [[0.0, -1.0, -6.0], [0.0, 1.0, -4.0]];
    let scene = Arc::new(Mutex::new(Scene::default()));
    {
        let mut scene = scene.lock().unwrap();
        scene.placed = vec![Placed {
            transform: Mat4::IDENTITY,
            wmo: Arc::new(gpu::upload(&shared, &NoFiles, white).unwrap()),
            parts: None,
        }];
        scene.liquids = Some(Arc::new(Pond(Arc::new(surfaces))));
    }
    let mut layer = BuildingsLayer::new(shared, scene);
    let eye = Vec3::new(20.0, 0.0, 1.0);
    assert_eq!(middle_in(&gpu, &mut layer, eye, &[Phase::Beyond]), [255, 255, 255, 255]);
    assert_eq!(middle_in(&gpu, &mut layer, eye, &[Phase::Near]), [0, 0, 0, 255]);
}

/// Water at 0 around the origin.
struct Pond(Arc<Surfaces>);

impl Liquids for Pond {
    fn surfaces(&self) -> Arc<Surfaces> {
        self.0.clone()
    }
}
