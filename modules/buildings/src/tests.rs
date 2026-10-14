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
    Tile, TileId, Wdl, Wdt, Wmo, WmoBatch, WmoDoodad, WmoGroup, WmoLight, WmoLiquid, WmoMaterial,
};
use uniwow_api::glam::{Mat3, Mat4, Quat, Vec3};
use uniwow_api::liquids::{Liquids, Surfaces};
use uniwow_api::models::{Extent, Instance, Look, LookId, LookState, Models};
use uniwow_api::viewport::{Layer, Phase, Pyramid, Target, View, Water};
use uniwow_api::{egui, egui_wgpu, wgpu};

use crate::budget;
use crate::colours;
use crate::doodads::{self, Owners};
use crate::gpu::{self, Shared, State};
use crate::keeping::Kept;
use crate::layer::{self, BuildingsLayer, Placed, Scene};
use crate::liquid;

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
    // The second group of a triangle and a half: its indices made even.
    let mut third = wmo.groups[0].clone();
    third.triangles = vec![0, 1, 2];
    third.batches[0].count = 3;
    wmo.groups.push(third);
    let gpu::Geometry {
        vertices,
        indices,
        starts,
        bases,
    } = gpu::geometry(&mut wmo);
    assert_eq!((vertices.len(), starts, bases), (12, vec![0, 6, 12], vec![0, 4, 8]));
    assert_eq!(
        indices[6..],
        [0, 1, 2, 0, 2, 3, 0, 1, 2, 0],
        "each group's into its own vertices, the last made even"
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
    let inside = vec![false; doodads.len()];
    let only_first = doodads::instances(&models, &transform, &sets, &doodads, &holders, &inside, &[], 0);
    assert_eq!(ids(&only_first), [0], "the empty name left out");
    let named = doodads::instances(&models, &transform, &sets, &doodads, &holders, &inside, &[], 2);
    assert_eq!(ids(&named), [0, 3]);
    assert_eq!(
        ids(&doodads::instances(
            &models,
            &transform,
            &sets,
            &doodads,
            &holders,
            &inside,
            &[],
            9
        )),
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
fn a_doodad_of_the_rooms_is_lit_by_its_colour_as_the_client_lights_it() {
    // Held by groups inside only: lit inside; by one outside, lit as outside or not inside, or by
    // none: by the light of the view.
    let group = |flags: u32| WmoGroup {
        flags,
        ..WmoGroup::default()
    };
    let groups = [group(0x2000), group(0x2000 | 0x40), group(0x8), group(0x2000), group(0)];
    let holders = [vec![0], vec![0, 3], vec![0, 1], vec![2], vec![], vec![4], vec![9]];
    assert_eq!(
        doodads::lit_inside(&groups, &holders),
        [true, true, false, false, false, false, false]
    );
    // A dim warm colour: its diffuse brightened to 112 of 255 in its brightest channel, its hue
    // kept; its light everywhere as it is, under 96.
    let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5);
    let dim = doodads::interior([56, 28, 14, 255]);
    assert!(
        close(dim.diffuse, [112.0 / 255.0, 56.0 / 255.0, 28.0 / 255.0]),
        "{dim:?}"
    );
    assert!(
        close(dim.ambient, [56.0 / 255.0, 28.0 / 255.0, 14.0 / 255.0]),
        "{dim:?}"
    );
    // A bright one: its diffuse as it is, its light everywhere darkened to 96 of 255.
    let bright = doodads::interior([200, 100, 0, 7]);
    assert!(close(bright.diffuse, [200.0 / 255.0, 100.0 / 255.0, 0.0]), "{bright:?}");
    assert!(close(bright.ambient, [96.0 / 255.0, 48.0 / 255.0, 0.0]), "{bright:?}");
    // Black stays black.
    let black = doodads::interior([0, 0, 0, 255]);
    assert_eq!((black.diffuse, black.ambient), ([0.0; 3], [0.0; 3]));
    // Given to the instances of those of the rooms only.
    let models = FakeModels::default();
    let mut coloured = doodad("a.mdx", [0.0; 3]);
    coloured.colour = [56, 28, 14, 255];
    let parts = doodads::instances(
        &models,
        &Mat4::IDENTITY,
        &[set(0, 2)],
        &[coloured.clone(), coloured],
        &[vec![0], vec![2]],
        &[true, false],
        &[],
        0,
    );
    let interiors: Vec<_> = parts
        .iter()
        .flat_map(|(_, instances)| instances.iter().map(|instance| (instance.id, instance.interior)))
        .collect();
    assert_eq!(interiors, [(0, Some(dim)), (1, None)]);
}

/// A light of a building of the kind `kind` at `position`, white at the intensity `intensity`,
/// fading out from `attenuation[0]` to `attenuation[1]` yards.
fn lamp(kind: u8, position: [f32; 3], intensity: f32, attenuation: [f32; 2]) -> WmoLight {
    WmoLight {
        kind,
        attenuated: true,
        colour: [255, 128, 0, 255],
        position,
        intensity,
        attenuation,
    }
}

#[test]
fn a_doodad_of_the_rooms_is_lit_too_by_the_lights_its_groups_name_folded_into_its_light() {
    // The group 0 names the light 0, the group 1 the lights 1 and 0, the group 2 the lights 2 and 3;
    // the doodad 0, held by the groups 0 and 1, at the origin; the doodad 1 by the group 2, the
    // doodad 2 by none.
    let named = |refs: Vec<u16>| WmoGroup {
        flags: 0x2000,
        light_refs: refs,
        ..WmoGroup::default()
    };
    let groups = [named(vec![0]), named(vec![1, 0]), named(vec![2, 3])];
    let lights = [
        // 4 yards over the origin, whole within 5: whole.
        lamp(0, [0.0, 0.0, 4.0], 2.0, [5.0, 10.0]),
        // 6 yards along +X, fading from 2 to 10: weighing a half.
        lamp(0, [6.0, 0.0, 0.0], 1.0, [2.0, 10.0]),
        // A spot, which lights no doodad.
        lamp(1, [0.0, 0.0, 1.0], 1.0, [5.0, 10.0]),
        // Past its second distance: nothing.
        lamp(0, [0.0, 20.0, 0.0], 1.0, [5.0, 10.0]),
    ];
    let doodads = [
        doodad("a.mdx", [0.0; 3]),
        doodad("b.mdx", [0.0; 3]),
        doodad("c.mdx", [0.0; 3]),
    ];
    let holders = [vec![0, 1], vec![2], vec![]];
    let lamps = doodads::lamps(&groups, &lights, &doodads, &holders);
    assert_eq!(lamps.len(), 3);
    assert_eq!(
        lamps[0],
        [
            ([0.0, 0.0, 1.0], [2.0, 256.0 / 255.0, 0.0]),
            ([1.0, 0.0, 0.0], [0.5, 64.0 / 255.0, 0.0]),
        ],
        "each light named once, by its weight"
    );
    assert!(lamps[1].is_empty() && lamps[2].is_empty());
    // Folded into its light: its diffuse and each light, from the mean of their directions weighed
    // by their brightness, each by how much it faces that mean, each channel at most 1.
    let base = doodads::interior([56, 28, 14, 255]);
    let lit = doodads::lit_by(base, &lamps[0], Mat3::IDENTITY);
    let towards = Vec3::from(lit.towards.expect("a light of its own"));
    let brightness = |colour: [f32; 3]| colour.iter().sum::<f32>() / 3.0;
    let mean = (Vec3::new(0.30822, 0.30822, 0.9) * brightness(base.diffuse)
        + Vec3::Z * brightness(lamps[0][0].1)
        + Vec3::X * brightness(lamps[0][1].1))
    .normalize();
    assert!(towards.abs_diff_eq(mean, 1e-5), "{towards} {mean}");
    let facing = |direction: Vec3| direction.dot(mean).max(0.0);
    let red =
        (base.diffuse[0] * facing(Vec3::new(0.30822, 0.30822, 0.9)) + 2.0 * facing(Vec3::Z) + 0.5 * facing(Vec3::X))
            .min(1.0);
    let green = base.diffuse[1] * facing(Vec3::new(0.30822, 0.30822, 0.9))
        + 256.0 / 255.0 * facing(Vec3::Z)
        + 64.0 / 255.0 * facing(Vec3::X);
    assert!(
        (lit.diffuse[0] - red).abs() < 1e-5 && (lit.diffuse[1] - green.min(1.0)).abs() < 1e-5,
        "{lit:?}"
    );
    assert_eq!(lit.diffuse[0], 1.0, "bounded to 1");
    assert_eq!(lit.ambient, base.ambient, "its light everywhere as it is");
    // Turned with the building: a quarter turn about the vertical, the light along +X now along +Y.
    let turned = doodads::lit_by(base, &lamps[0][1..], Mat3::from_rotation_z(std::f32::consts::FRAC_PI_2));
    assert!(turned.towards.unwrap()[1] > 0.5, "{turned:?}");
    // A light from behind the mean of the others adds nothing, rather than taking away.
    let behind = [([1.0, 0.0, 0.0], [0.5; 3]), ([-1.0, 0.0, 0.0], [0.2; 3])];
    let backed = doodads::lit_by(base, &behind, Mat3::IDENTITY);
    let mean = Vec3::from(backed.towards.unwrap());
    let facing = |direction: Vec3| direction.dot(mean).max(0.0);
    let blue = base.diffuse[2] * facing(Vec3::new(0.30822, 0.30822, 0.9)) + 0.5 * facing(Vec3::X);
    assert!((backed.diffuse[2] - blue).abs() < 1e-5, "{backed:?}");
    // Without lights, as it is.
    assert_eq!(doodads::lit_by(base, &[], Mat3::IDENTITY), base);
    // Given to its instance.
    let models = FakeModels::default();
    let parts = doodads::instances(
        &models,
        &Mat4::IDENTITY,
        &[set(0, 3)],
        &doodads,
        &holders,
        &[true, true, false],
        &lamps,
        0,
    );
    let interiors: Vec<_> = parts
        .iter()
        .flat_map(|(_, instances)| instances.iter().map(|instance| (instance.id, instance.interior)))
        .collect();
    let plain = doodads::interior([0, 0, 0, 0]);
    assert!(
        interiors.contains(&(0, Some(doodads::lit_by(plain, &lamps[0], Mat3::IDENTITY)))),
        "{interiors:?}"
    );
    assert!(interiors.contains(&(1, Some(plain))));
    assert!(interiors.contains(&(2, None)));
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
        &[false],
        &[],
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
        &[false],
        &[],
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
            // Grey and opaque; green and opaque.
            FileRef::Path(path) if path == "grey.blp" => Ok(Texture {
                width: 4,
                height: 4,
                format: TextureFormat::Rgba8,
                levels: vec![[128, 128, 128, 255].repeat(16)],
            }),
            FileRef::Path(path) if path == "green.blp" => Ok(Texture {
                width: 4,
                height: 4,
                format: TextureFormat::Rgba8,
                levels: vec![[0, 64, 0, 255].repeat(16)],
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
    color_format: wgpu::TextureFormat::Rgba8Unorm,
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
        sky: None,
        water: None,
        night: 0.0,
    }
}

/// The middle pixel of what `layer` draws seen from `eye` towards the origin, cleared to black.
fn middle(gpu: &egui_wgpu::RenderState, layer: &mut BuildingsLayer, eye: Vec3) -> [u8; 4] {
    middle_in(gpu, layer, eye, &Phase::ALL)
}

/// The middle pixel of what `layer` draws in `phases` seen from `eye` towards the origin.
fn middle_in(gpu: &egui_wgpu::RenderState, layer: &mut BuildingsLayer, eye: Vec3, phases: &[Phase]) -> [u8; 4] {
    row_in(gpu, layer, eye, phases)[16]
}

/// The middle row of what `layer` draws seen from `eye` towards the origin, from the left.
fn row(gpu: &egui_wgpu::RenderState, layer: &mut BuildingsLayer, eye: Vec3) -> Vec<[u8; 4]> {
    row_in(gpu, layer, eye, &Phase::ALL)
}

/// The middle row of what `layer` draws in `phases` seen from `eye` towards the origin.
fn row_in(gpu: &egui_wgpu::RenderState, layer: &mut BuildingsLayer, eye: Vec3, phases: &[Phase]) -> Vec<[u8; 4]> {
    row_of(gpu, layer, &view(eye), phases)
}

/// The middle row of what `layer` draws in `phases` seen by `view`.
fn row_of(gpu: &egui_wgpu::RenderState, layer: &mut BuildingsLayer, view: &View, phases: &[Phase]) -> Vec<[u8; 4]> {
    row_behind(gpu, layer, view, phases, 0.0)
}

/// Counts the pyramids `row_behind` makes, a new one each frame as when the view changes size.
static PYRAMIDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A pyramid of the depth of a view of `size` × `size`, its first level at the depths `halves` on
/// its left half and its right one, each next level the least of the 2 × 2 texels under each.
fn pyramid_at(gpu: &egui_wgpu::RenderState, size: u32, halves: [f32; 2]) -> (wgpu::TextureView, u32) {
    let levels = u32::BITS - size.leading_zeros();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut texels: Vec<f32> = (0..size * size)
        .map(|at| halves[usize::from(at % size >= size / 2)])
        .collect();
    for level in 0..levels {
        let side = size >> level;
        if level > 0 {
            let under = &texels;
            texels = (0..side * side)
                .map(|at| {
                    let (x, y) = (2 * (at % side), 2 * (at / side));
                    let texel = |x: u32, y: u32| under[(y * 2 * side + x) as usize];
                    texel(x, y)
                        .min(texel(x + 1, y))
                        .min(texel(x, y + 1))
                        .min(texel(x + 1, y + 1))
                })
                .collect();
        }
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            uniwow_api::bytemuck::cast_slice(&texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * side),
                rows_per_image: Some(side),
            },
            wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: 1,
            },
        );
    }
    (texture.create_view(&Default::default()), levels)
}

/// The depth of a point `distance` yards before the eye, in reverse Z with the near plane of `view`.
fn depth_at(distance: f32) -> f32 {
    0.1 / distance
}

/// The middle row of what `layer` draws in `phases` seen by `view`, as the view draws it: the opaque
/// phase in a first pass, the layer's computing before it; its testing against a pyramid of the
/// depth `wall` (in reverse Z, 0 the farthest) between the passes; the other phases in a second.
fn row_behind(
    gpu: &egui_wgpu::RenderState,
    layer: &mut BuildingsLayer,
    view: &View,
    phases: &[Phase],
    wall: f32,
) -> Vec<[u8; 4]> {
    row_behind_halves(gpu, layer, view, phases, [wall; 2])
}

/// What `row_behind` draws, the wall at the depths `halves` on the left half of the view and on its
/// right one.
fn row_behind_halves(
    gpu: &egui_wgpu::RenderState,
    layer: &mut BuildingsLayer,
    view: &View,
    phases: &[Phase],
    halves: [f32; 2],
) -> Vec<[u8; 4]> {
    let size = 32u32;
    let view = *view;
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
    let (colour_view, depth_view) = (
        colour.create_view(&Default::default()),
        depth.create_view(&Default::default()),
    );
    let (wall, levels) = pyramid_at(gpu, size, halves);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    if layer.computes() {
        layer.compute(gpu, &view, &mut encoder);
    }
    for first in [true, false] {
        if !first && layer.occludes() {
            let pyramid = Pyramid {
                view: &wall,
                size: [size, size],
                levels,
                generation: PYRAMIDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            };
            layer.occlude(gpu, &view, &pyramid, &mut encoder);
        }
        let load = |clear| {
            if first {
                wgpu::LoadOp::Clear(clear)
            } else {
                wgpu::LoadOp::Load
            }
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &colour_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if first {
                        wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: load(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for phase in phases.iter().filter(|phase| phase.first_pass() == first) {
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
    (0..size as usize)
        .map(|x| {
            let at = 16 * 256 + x * 4;
            [read[at], read[at + 1], read[at + 2], read[at + 3]]
        })
        .collect()
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
        liquids: None,
    }]
    .into();
    Some(BuildingsLayer::new(shared, scene))
}

#[test]
fn a_group_hidden_by_the_depth_is_drawn_one_frame_more_then_no_longer_and_again_once_in_sight() {
    let Some(gpu) = device() else {
        return;
    };
    let Some(mut layer) = bench(&gpu, square(material(0x1, 0), false, None)) else {
        eprintln!("skipped: the software adapter does not draw buildings");
        return;
    };
    if !layer.occludes() {
        eprintln!("skipped: the software adapter does not compute");
        return;
    }
    let seen = view(Vec3::new(5.0, 0.0, 0.0));
    let white = [255, 255, 255, 255];
    let black = [0, 0, 0, 255];
    let frame = |layer: &mut BuildingsLayer, wall: f32| row_behind(&gpu, layer, &seen, &Phase::ALL, wall)[16];
    // The square 5 yards away; a wall at 2 yards hides it, one at 10 does not.
    let (near, far) = (depth_at(2.0), depth_at(10.0));
    assert_eq!(frame(&mut layer, far), white, "in sight");
    assert_eq!(
        frame(&mut layer, near),
        white,
        "seen at the frame before: drawn first, one frame more"
    );
    assert_eq!(frame(&mut layer, near), black, "then hidden");
    assert_eq!(frame(&mut layer, near), black);
    assert!(
        layer.stats().items.contains("1 hidden by the depth"),
        "{}",
        layer.stats().items
    );
    assert_eq!(
        frame(&mut layer, far),
        white,
        "found in sight: drawn by the second pass at once"
    );
    assert_eq!(
        row_behind(&gpu, &mut layer, &seen, &[Phase::Opaque], far)[16],
        white,
        "and by the first at the next frame"
    );
    // Hidden again, then the buildings given anew: every group seen, drawn first at that frame.
    frame(&mut layer, near);
    assert_eq!(frame(&mut layer, near), black);
    let scene = layer.scene();
    let placed: Vec<Placed> = scene.lock().unwrap().placed.iter().cloned().collect();
    scene.lock().unwrap().placed = placed.into();
    assert_eq!(frame(&mut layer, near), white, "the buildings changed: all drawn first");
    assert_eq!(frame(&mut layer, near), black);
}

#[test]
fn the_groups_of_two_buildings_are_told_apart_one_hidden_the_other_not() {
    let Some(gpu) = device() else {
        return;
    };
    let Some(mut layer) = bench(&gpu, square(material(0x1, 0), false, None)) else {
        eprintln!("skipped: the software adapter does not draw buildings");
        return;
    };
    if !layer.occludes() {
        eprintln!("skipped: the software adapter does not compute");
        return;
    }
    // The square twice, to the left of the view and to its right.
    let scene = layer.scene();
    let wmo = scene.lock().unwrap().placed[0].wmo.clone();
    let at = |y: f32| Placed {
        transform: Mat4::from_translation(Vec3::new(0.0, y, 0.0)),
        wmo: wmo.clone(),
        parts: None,
        liquids: None,
    };
    scene.lock().unwrap().placed = vec![at(-1.5), at(1.5)].into();
    let seen = view(Vec3::new(5.0, 0.0, 0.0));
    let frame = |layer: &mut BuildingsLayer, halves: [f32; 2]| {
        let row = row_behind_halves(&gpu, layer, &seen, &Phase::ALL, halves);
        [row[8], row[24]]
    };
    let (near, far) = (depth_at(2.0), depth_at(10.0));
    let [white, black] = [[255, 255, 255, 255], [0, 0, 0, 255]];
    assert_eq!(frame(&mut layer, [far, far]), [white, white]);
    assert_eq!(
        frame(&mut layer, [near, far]),
        [white, white],
        "the left one drawn one frame more"
    );
    assert_eq!(frame(&mut layer, [near, far]), [black, white], "then hidden alone");
    assert_eq!(frame(&mut layer, [near, far]), [black, white]);
}

#[test]
fn a_blended_group_is_drawn_only_while_found_in_sight() {
    let Some(gpu) = device() else {
        return;
    };
    let Some(mut layer) = bench(&gpu, square(material(0x1, 2), false, None)) else {
        eprintln!("skipped: the software adapter does not draw buildings");
        return;
    };
    if !layer.occludes() {
        eprintln!("skipped: the software adapter does not compute");
        return;
    }
    let seen = view(Vec3::new(5.0, 0.0, 0.0));
    let frame = |layer: &mut BuildingsLayer, wall: f32| row_behind(&gpu, layer, &seen, &Phase::ALL, wall)[16];
    let drawn = frame(&mut layer, depth_at(10.0));
    assert!(drawn[0] > 100, "in sight: {drawn:?}");
    assert_eq!(
        frame(&mut layer, depth_at(2.0)),
        [0, 0, 0, 255],
        "hidden at once, though seen before"
    );
    assert_eq!(frame(&mut layer, depth_at(10.0)), drawn);
}

#[test]
fn each_group_is_drawn_from_where_its_vertices_begin_its_indices_of_16_bits() {
    let Some(gpu) = device() else {
        return;
    };
    // Another building first, a triangle, its indices made even, so that neither the vertices nor
    // the indices of the second begin their arenas; the second of two groups, the left one and the
    // right one.
    let mut triangle = square(material(0x1, 0), false, None);
    triangle.groups[0].triangles.truncate(3);
    triangle.groups[0].batches[0].count = 3;
    let Some(mut layer) = bench(&gpu, triangle) else {
        eprintln!("skipped: the software adapter does not draw buildings");
        return;
    };
    let mut wmo = square(material(0x1, 0), false, None);
    let mut right = wmo.groups[0].clone();
    for vertex in &mut right.vertices {
        vertex[1] += 1.5;
    }
    right.bounds = [[0.0, 0.5, -1.0], [0.0, 2.5, 1.0]];
    for vertex in &mut wmo.groups[0].vertices {
        vertex[1] -= 1.5;
    }
    wmo.groups[0].bounds = [[0.0, -2.5, -1.0], [0.0, -0.5, 1.0]];
    wmo.groups.push(right);
    wmo.bounds = [[0.0, -2.5, -1.0], [0.0, 2.5, 1.0]];
    let scene = layer.scene();
    let shared = layer.shared();
    let uploaded = Arc::new(gpu::upload(&shared, &NoFiles, wmo).unwrap());
    assert_eq!(
        uploaded
            .groups
            .iter()
            .map(|group| group.base_vertex)
            .collect::<Vec<_>>(),
        [4, 8],
        "after the 4 vertices of the first building, each group's own"
    );
    scene.lock().unwrap().placed = vec![Placed {
        transform: Mat4::IDENTITY,
        wmo: uploaded,
        parts: None,
        liquids: None,
    }]
    .into();
    let drawn = row(&gpu, &mut layer, Vec3::new(5.0, 0.0, 0.0));
    let white = [255, 255, 255, 255];
    // Each square whole, both its triangles, across the middle row.
    assert!(
        drawn[4..=12].iter().chain(&drawn[20..=28]).all(|pixel| *pixel == white),
        "{drawn:?}"
    );
    assert_eq!(drawn[16], [0, 0, 0, 255], "nothing between them");
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
fn a_building_is_drawn_where_the_buildings_last_given_place_it() {
    let Some(gpu) = device() else {
        return;
    };
    let Some(shared) = Shared::new(&gpu, &TARGET).ok().map(Arc::new) else {
        eprintln!("skipped: the software adapter does not draw buildings");
        return;
    };
    let wmo = Arc::new(gpu::upload(&shared, &NoFiles, square(material(0x1, 0), false, None)).unwrap());
    let placed = |transforms: &[Mat4]| -> Arc<[Placed]> {
        transforms
            .iter()
            .map(|transform| Placed {
                transform: *transform,
                wmo: wmo.clone(),
                parts: None,
                liquids: None,
            })
            .collect()
    };
    let (white, black) = ([255, 255, 255, 255], [0, 0, 0, 255]);
    let scene = Arc::new(Mutex::new(Scene::default()));
    let first = placed(&[Mat4::IDENTITY]);
    scene.lock().unwrap().placed = first.clone();
    let mut layer = BuildingsLayer::new(shared.clone(), scene.clone());
    let front = Vec3::new(5.0, 0.0, 0.0);
    // The middle, then on the right where a building moved by 2.5 yards stands.
    let seen = |layer: &mut BuildingsLayer| {
        let row = row(&gpu, layer, front);
        [row[16], row[28]]
    };
    assert_eq!(seen(&mut layer), [white, black]);
    assert_eq!(seen(&mut layer), [white, black], "the same again");
    // Moved aside, still in sight. Those drawn before held by the layer until it prepares the
    // next frame.
    let aside = Mat4::from_translation(Vec3::new(0.0, 2.5, 0.0));
    scene.lock().unwrap().placed = placed(&[aside]);
    assert_eq!(Arc::strong_count(&first), 2, "held by the layer");
    assert_eq!(seen(&mut layer), [black, white]);
    assert_eq!(Arc::strong_count(&first), 1, "let go");
    // Another in the middle: the instances outgrow their buffer.
    scene.lock().unwrap().placed = placed(&[aside, Mat4::IDENTITY]);
    assert_eq!(seen(&mut layer), [white, white]);
    scene.lock().unwrap().placed = placed(&[Mat4::IDENTITY]);
    assert_eq!(seen(&mut layer), [white, black], "back");
    // Five at once, none listed at first, looking away: their entries outgrow their buffer the
    // frame they are seen, their instances not written again; the first aside, so that an entry
    // of a buffer not bound again would draw there.
    let scene = Arc::new(Mutex::new(Scene::default()));
    scene.lock().unwrap().placed = placed(&[aside, Mat4::IDENTITY, Mat4::IDENTITY, Mat4::IDENTITY, Mat4::IDENTITY]);
    let mut layer = BuildingsLayer::new(shared, scene);
    let mut away = view(front);
    away.view = Mat4::look_at_rh(front, front * 2.0, Vec3::Z);
    away.view_proj = Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1) * away.view;
    layer.prepare(&gpu, &away);
    assert_eq!(layer.stats().triangles, 0, "none in sight");
    assert_eq!(seen(&mut layer), [white, white]);
    assert_eq!(layer.stats().triangles, 10);
}

#[test]
fn outside_a_face_is_lit_in_gamma_by_the_ambient_light_turned_and_the_diffuse_then_fogged() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    let Some(mut lit) = bench(&gpu, square(material(0, 0), false, None)) else {
        return;
    };
    // White, outside: 1.1 times the ambient light plus the diffuse facing the sun, 0.9 times the
    // ambient light from behind, in gamma, as the target stores them.
    let sun = |towards: f32| uniwow_api::viewport::Sun {
        direction: [towards, 0.0, 0.0],
        colour: [0.2; 3],
        ambient: [0.4; 3],
    };
    let mut seen = view(front);
    seen.sun = sun(1.0);
    let facing = row_of(&gpu, &mut lit, &seen, &Phase::ALL)[16];
    assert!(facing[0].abs_diff(163) <= 1 && facing[0] == facing[2], "{facing:?}");
    seen.sun = sun(-1.0);
    let behind = row_of(&gpu, &mut lit, &seen, &Phase::ALL)[16];
    assert!(behind[0].abs_diff(92) <= 1, "{behind:?}");
    // So dark that it rounds to black.
    seen.sun.ambient = [0.0011; 3];
    let dark = row_of(&gpu, &mut lit, &seen, &Phase::ALL)[16];
    assert!(dark[0] <= 1, "{dark:?}");
    // The fog of the game, red, from 2 to 10 yards at the rate 2: at 5 yards from an eye 3 yards up,
    // 1 − (5 / 8)² of it, by the depth along the view, not on the ground; mixed in gamma as the
    // client, over a face lit past white, bounded first. The pixel read, half a pixel off the
    // middle, lies a little deeper: 97 of 255, not 100.
    let mut fogged = view(Vec3::new(4.0, 0.0, 3.0));
    fogged.sun = uniwow_api::viewport::Sun {
        colour: [1.0; 3],
        ambient: [1.0; 3],
        ..sun(1.0)
    };
    fogged.fog = uniwow_api::viewport::Fog {
        colour: [1.0, 0.0, 0.0],
        start: 2.0,
        middle: 6.0,
        end: 10.0,
        rate: 2.0,
    };
    let pixel = row_of(&gpu, &mut lit, &fogged, &Phase::ALL)[16];
    assert!(
        pixel[0] == 255 && pixel[1].abs_diff(97) <= 2 && pixel[1] == pixel[2],
        "{pixel:?}"
    );
    // Unlit and unfogged: its white within the fog of the game; beyond its end at 4.5 yards from the
    // eye, not on the ground, not drawn; under the fog of the editor, measured on the ground, within
    // its end: drawn.
    let mut unfogged = bench(&gpu, square(material(0x3, 0), false, None)).unwrap();
    assert_eq!(row_of(&gpu, &mut unfogged, &fogged, &Phase::ALL)[16], [255; 4]);
    fogged.fog.end = 4.5;
    fogged.fog.middle = 4.5;
    assert_eq!(row_of(&gpu, &mut unfogged, &fogged, &Phase::ALL)[16], [0, 0, 0, 255]);
    fogged.fog.rate = 0.0;
    assert_eq!(row_of(&gpu, &mut unfogged, &fogged, &Phase::ALL)[16], [255; 4]);
    // Beyond the end, nothing drawn, fogged or not, lit or not, as Noggit draws nothing there.
    fogged.fog.rate = 2.0;
    for flags in [0x0, 0x1, 0x2] {
        let mut other = bench(&gpu, square(material(flags, 0), false, None)).unwrap();
        let pixel = row_of(&gpu, &mut other, &fogged, &Phase::ALL)[16];
        assert_eq!(pixel, [0, 0, 0, 255], "flags {flags:#x}");
    }
}

/// A sun of the view facing +X, its diffuse 0.2 and its ambient light 0.4: a face facing it lit
/// 1.1 times the ambient light plus the diffuse, 0.64.
fn facing_sun() -> uniwow_api::viewport::Sun {
    uniwow_api::viewport::Sun {
        direction: [1.0, 0.0, 0.0],
        colour: [0.2; 3],
        ambient: [0.4; 3],
    }
}

/// How much the light everywhere of a face facing +X is turned towards the light of the insides,
/// (0.30822, 0.30822, 0.9): 0.9 plus 0.2 times its cosine.
const TURNED_INSIDE: f32 = 0.9 + 0.2 * 0.30822;

/// Whether `pixel` is `expected`, from 0 to 1 in gamma, within two 255ths on each channel.
fn near(pixel: [u8; 4], expected: [f32; 3]) -> bool {
    pixel[..3]
        .iter()
        .zip(expected)
        .all(|(value, expected)| (f32::from(*value) - expected.clamp(0.0, 1.0) * 255.0).abs() <= 2.0)
}

#[test]
fn inside_a_building_is_lit_by_its_ambient_colour_and_its_vertex_colours_turned_to_the_light_inside() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    // A red vertex colour inside, halved by the fix and doubled back, its ambient colour taken out of
    // it by the fix and added again: 200 in red, the ambient colour in green and blue; turned towards
    // the light of the insides by 0.96, the sun of the view no part of it.
    let mut wmo = square(material(0, 0), true, Some([200, 0, 0, 0]));
    wmo.ambient = [30, 60, 90, 255];
    let Some(mut red) = bench(&gpu, wmo.clone()) else {
        return;
    };
    let mut seen = view(front);
    seen.sun = facing_sun();
    let lit = row_of(&gpu, &mut red, &seen, &Phase::ALL)[16];
    let expected = [200.0, 60.0, 90.0].map(|value: f32| value / 255.0 * TURNED_INSIDE);
    assert!(near(lit, expected), "{lit:?} against {expected:?}");
    // A floor, facing up: 1.08 times.
    for group in &mut wmo.groups {
        group.normals = vec![[0.0, 0.0, 1.0]; 4];
    }
    let mut floor = bench(&gpu, wmo).unwrap();
    let lit = row_of(&gpu, &mut floor, &seen, &Phase::ALL)[16];
    let expected = [200.0, 60.0, 90.0].map(|value: f32| value / 255.0 * (0.9 + 0.2 * 0.9));
    assert!(near(lit, expected), "{lit:?} against {expected:?}");
}

#[test]
fn outside_a_building_is_lit_by_the_view_its_vertex_colours_added_to_the_ambient_light() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    // A group outside of red vertex colours, made 200 again: added to the ambient light of the view
    // turned to the sun, 1.1 times, plus the diffuse.
    let Some(mut outside) = bench(&gpu, square(material(0, 0), false, Some([200, 0, 0, 0]))) else {
        return;
    };
    let mut seen = view(front);
    seen.sun = facing_sun();
    let lit = row_of(&gpu, &mut outside, &seen, &Phase::ALL)[16];
    assert!(near(lit, [1.0, 0.64, 0.64]), "{lit:?}");
    // A group inside lit as outside (0x40), as the streets of a city open to the sky: the same.
    let mut street = square(material(0, 0), true, Some([200, 0, 0, 0]));
    street.groups[0].flags |= 0x40;
    let mut street = bench(&gpu, street).unwrap();
    let lit = row_of(&gpu, &mut street, &seen, &Phase::ALL)[16];
    assert!(near(lit, [1.0, 0.64, 0.64]), "{lit:?}");
    // Without colours, the light of the view alone.
    let mut plain = bench(&gpu, square(material(0, 0), false, None)).unwrap();
    let lit = row_of(&gpu, &mut plain, &seen, &Phase::ALL)[16];
    assert!(near(lit, [0.64; 3]), "{lit:?}");
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
            liquids: None,
        },
        Placed {
            transform: Mat4::from_translation(Vec3::new(0.0, 500.0, 0.0)),
            wmo: uploaded,
            parts: None,
            liquids: None,
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
    // The outer wall of a room: a group inside, its batch outside, its vertex colours red, left out.
    let mut wall = square(material(0, 0), true, Some([200, 0, 0, 255]));
    wall.groups[0].batch_counts = [0, 0, 1];
    let Some(mut layer) = bench(&gpu, wall.clone()) else {
        return;
    };
    let lit = middle(&gpu, &mut layer, front);
    assert!(lit[0] == lit[1] && lit[1] == lit[2] && lit[0] > 50, "{lit:?}");
    let mut seen = view(front);
    seen.sun = facing_sun();
    let lit = row_of(&gpu, &mut layer, &seen, &Phase::ALL)[16];
    assert!(near(lit, [0.64; 3]), "{lit:?}");
    // A material lit as outside (0x8), whatever its batch: the same.
    let mut lit_outside = square(material(0x8, 0), true, Some([200, 0, 0, 0]));
    lit_outside.groups[0].batch_counts = [0, 1, 0];
    let mut layer = bench(&gpu, lit_outside).unwrap();
    let lit = row_of(&gpu, &mut layer, &seen, &Phase::ALL)[16];
    assert!(near(lit, [0.64; 3]), "{lit:?}");
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
    // Added as it is, in gamma: a red of 30 adds 30.
    let mut faint = square(material(0, 0), true, Some([2, 2, 2, 255]));
    faint.flags = 0xF;
    faint.ambient = [30, 0, 0, 255];
    let mut layer = bench(&gpu, faint).unwrap();
    let added = middle(&gpu, &mut layer, Vec3::new(5.0, 0.0, 0.0));
    assert!(
        (i32::from(added[0]) - i32::from(lit[0]) - 30).abs() <= 2,
        "{added:?} against {lit:?}"
    );
}

#[test]
fn a_transition_mixes_the_light_inside_and_outside_by_the_alpha_of_its_colours() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    let mut seen = view(front);
    seen.sun = facing_sun();
    // Wholly outside: its red colour darkened to nothing by the fix, lit by the view alone.
    let mut door = square(material(0, 0), true, Some([200, 0, 0, 255]));
    door.groups[0].batch_counts = [1, 0, 0];
    let Some(mut layer) = bench(&gpu, door) else {
        return;
    };
    let lit = row_of(&gpu, &mut layer, &seen, &Phase::ALL)[16];
    assert!(near(lit, [0.64; 3]), "{lit:?}");
    // Half: its colour as the fix left it, darkened by half, doubled back, in both lights; inside
    // turned to the light of the insides, outside added to the ambient light of the view turned to
    // the sun, with its diffuse; mixed by its alpha.
    let mut half = square(material(0, 0), true, Some([200, 0, 0, 128]));
    half.groups[0].batch_counts = [1, 0, 0];
    let mut fixed = half.groups[0].clone();
    colours::fix(&mut fixed, half.flags, half.ambient);
    let [red, _, _, alpha] = fixed.colours[0][0];
    let vertex = f32::from(red) * 2.0 / 255.0;
    let share = f32::from(alpha) / 255.0;
    let inside = [vertex * TURNED_INSIDE, 0.0, 0.0];
    let outside = [(0.4 + vertex) * 1.1 + 0.2, 0.64, 0.64];
    let expected: [f32; 3] = std::array::from_fn(|at| inside[at] + (outside[at] - inside[at]) * share);
    let mut layer = bench(&gpu, half).unwrap();
    let lit = row_of(&gpu, &mut layer, &seen, &Phase::ALL)[16];
    assert!(near(lit, expected), "{lit:?} against {expected:?}");
}

#[test]
fn a_material_glowing_at_night_adds_its_emissive_colour_by_how_much_it_is_night() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    // Outside, white, in a dim light: 0.11 of it; its emissive colour green.
    let mut glowing = material(0x10, 0);
    glowing.emissive = [0, 100, 0, 255];
    let Some(mut layer) = bench(&gpu, square(glowing.clone(), false, None)) else {
        return;
    };
    let mut seen = view(front);
    seen.sun = uniwow_api::viewport::Sun {
        colour: [0.0; 3],
        ambient: [0.1; 3],
        ..facing_sun()
    };
    for (night, green) in [(0.0, 0.11), (0.5, 0.11 + 50.0 / 255.0), (1.0, 0.11 + 100.0 / 255.0)] {
        seen.night = night;
        let lit = row_of(&gpu, &mut layer, &seen, &Phase::ALL)[16];
        assert!(near(lit, [0.11, green, 0.11]), "night {night}: {lit:?}");
    }
    // Without the flag, its emissive colour left out.
    glowing.flags = 0;
    let mut dark = bench(&gpu, square(glowing, false, None)).unwrap();
    let lit = row_of(&gpu, &mut dark, &seen, &Phase::ALL)[16];
    assert!(near(lit, [0.11; 3]), "{lit:?}");
}

#[test]
fn the_environment_is_added_after_the_light() {
    let Some(gpu) = device() else {
        return;
    };
    let front = Vec3::new(5.0, 0.0, 0.0);
    // A grey texture of the shader environment (3), green on the environment, in a dim light: the
    // grey lit, then the green added whole by the alpha of the grey; for the environment metal (5),
    // the green times the grey too.
    let environment = |shader| {
        let mut material = material(0, 0);
        material.shader = shader;
        material.textures[0] = Some(FileRef::Path("grey.blp".to_owned()));
        material.textures[1] = Some(FileRef::Path("green.blp".to_owned()));
        square(material, false, None)
    };
    let Some(mut layer) = bench(&gpu, environment(3)) else {
        return;
    };
    let mut seen = view(front);
    seen.sun = uniwow_api::viewport::Sun {
        colour: [0.0; 3],
        ambient: [0.1; 3],
        ..facing_sun()
    };
    let grey = 128.0 / 255.0 * 0.11;
    let lit = row_of(&gpu, &mut layer, &seen, &Phase::ALL)[16];
    assert!(near(lit, [grey, grey + 64.0 / 255.0, grey]), "{lit:?}");
    let mut metal = bench(&gpu, environment(5)).unwrap();
    let lit = row_of(&gpu, &mut metal, &seen, &Phase::ALL)[16];
    let added = 128.0 / 255.0 * 64.0 / 255.0;
    assert!(near(lit, [grey, grey + added, grey]), "{lit:?}");
}

#[test]
fn the_specular_and_metal_materials_are_highlighted_by_the_colour_of_the_sun_outside() {
    let Some(gpu) = device() else {
        return;
    };
    let shining = |shader, texture: Option<&str>, inside| {
        let mut material = material(0, 0);
        material.shader = shader;
        material.textures[0] = texture.map(|path| FileRef::Path(path.to_owned()));
        square(material, inside, None)
    };
    let Some(mut white) = bench(&gpu, shining(1, None, false)) else {
        return;
    };
    // Seen from the front, the sun of the view behind the eye, the colour of the sun given or not.
    let seen = |sun: Option<[f32; 3]>| View {
        sun: facing_sun(),
        water: sun.map(|sun| Water {
            river: [[0.0; 4]; 2],
            ocean: [[0.0; 4]; 2],
            sun,
        }),
        ..view(Vec3::new(5.0, 0.0, 0.0))
    };
    let highlighted = |layer: &mut BuildingsLayer, on: bool, view: &View| {
        layer.scene().lock().unwrap().highlights = on;
        row_of(&gpu, layer, view, &Phase::ALL)[16]
    };
    let sunny = seen(Some([0.1, 0.05, 0.0]));
    // Straight on the face: 1.25 times the colour of the sun by the alpha of the texel, added in
    // gamma to the white lit, 0.64.
    let lit = highlighted(&mut white, true, &sunny);
    assert!(near(lit, [0.64 + 0.125, 0.64 + 0.0625, 0.64]), "{lit:?}");
    // Off, none; nor without the colour of the sun.
    let off = highlighted(&mut white, false, &sunny);
    assert!(near(off, [0.64; 3]), "{off:?}");
    let unlit = highlighted(&mut white, true, &seen(None));
    assert!(near(unlit, [0.64; 3]), "{unlit:?}");
    // Its texel of no alpha: only what the face reflects of the sun seen straight, nothing.
    let mut masked = bench(&gpu, shining(1, Some("masked.blp"), false)).unwrap();
    let lit = highlighted(&mut masked, true, &sunny);
    assert!(near(lit, [0.64; 3]), "{lit:?}");
    // Grey: the specular by its alpha alone, the metal by its red four times its alpha.
    let grey = 128.0 / 255.0 * 0.64;
    let mut specular = bench(&gpu, shining(1, Some("grey.blp"), false)).unwrap();
    let lit = highlighted(&mut specular, true, &sunny);
    assert!(near(lit, [grey + 0.125, grey + 0.0625, grey]), "{lit:?}");
    let mut metal = bench(&gpu, shining(2, Some("grey.blp"), false)).unwrap();
    let mask = 4.0 * 128.0 / 255.0;
    let lit = highlighted(&mut metal, true, &sunny);
    assert!(near(lit, [grey + 0.125 * mask, grey + 0.0625 * mask, grey]), "{lit:?}");
    // Nor any other shader.
    let mut diffuse = bench(&gpu, shining(0, Some("grey.blp"), false)).unwrap();
    let lit = highlighted(&mut diffuse, true, &sunny);
    assert!(near(lit, [grey; 3]), "{lit:?}");
    // The sun 60° aside: half way between it and the eye, 30° from the normal, 1.25 × cos⁸ 30°,
    // times the cosine of the sun with the face, a half; over the white lit by half its diffuse.
    let mut aside = seen(Some([0.4, 0.2, 0.0]));
    aside.sun.direction = [0.5, 3f32.sqrt() / 2.0, 0.0];
    let shine = 1.25 * (3f32.sqrt() / 2.0).powi(8) * 0.5;
    let lit = highlighted(&mut white, true, &aside);
    assert!(near(lit, [0.5 + 0.4 * shine, 0.5 + 0.2 * shine, 0.5]), "{lit:?}");
    // The eye and the sun 60° either side of the normal: half way, the normal itself; 1.25 times the
    // cosine of the sun with the face, a half, times what a texel of no alpha reflects of a sun 60°
    // from that way, (1 − cos 60°)⁵.
    let mut grazing = View {
        sun: facing_sun(),
        water: seen(Some([1.0; 3])).water,
        ..view(Vec3::new(2.5, 2.5 * 3f32.sqrt(), 0.0))
    };
    grazing.sun.direction = [0.5, -(3f32.sqrt()) / 2.0, 0.0];
    let reflected = 1.25 * 0.5 * 0.5f32.powi(5);
    let lit = highlighted(&mut masked, true, &grazing);
    assert!(near(lit, [0.5 + reflected; 3]), "{lit:?}");
    // Inside, none: what the rooms show with or without it; but in a building lit as one, as outside.
    let mut room = bench(&gpu, shining(1, None, true)).unwrap();
    assert_eq!(
        highlighted(&mut room, true, &sunny),
        highlighted(&mut room, false, &sunny)
    );
    let mut as_one = shining(1, None, true);
    as_one.flags = 0xF;
    let mut as_one = bench(&gpu, as_one).unwrap();
    let off = highlighted(&mut as_one, false, &sunny).map(|value| f32::from(value) / 255.0);
    let lit = highlighted(&mut as_one, true, &sunny);
    assert!(
        near(lit, [off[0] + 0.125, off[1] + 0.0625, off[2]]),
        "{lit:?} against {off:?}"
    );
}

#[test]
fn the_highlights_are_a_setting_on_by_default() {
    let setting = crate::highlights_setting();
    assert_eq!((setting.range, setting.default), ([0, 1], 1));
    assert_eq!(setting.value(Some(&uniwow_api::serde_json::json!(0))), 0);
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
    assert_eq!(first.arenas(), [480 * 40, 720 * 2], "its indices of 16 bits");
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
        liquids: None,
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
            liquids: None,
        }]
        .into();
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

#[test]
fn the_type_of_a_liquid_of_a_building_is_that_of_the_table_or_of_its_basic_kind() {
    let group = |liquid_type, flags| WmoGroup {
        liquid_type,
        flags,
        ..WmoGroup::default()
    };
    // The building naming the types of the table: below 21 by the basic kind of the type before.
    for (kind, expected) in [(1, 13), (3, 19), (4, 20), (5, 13), (7, 19), (8, 20), (14, 14), (41, 41)] {
        assert_eq!(liquid::liquid_type(0x4, &group(kind, 0), 0), expected, "{kind}");
    }
    assert_eq!(
        liquid::liquid_type(0x4, &group(1, 0x80000), 0),
        14,
        "the water of a group of the ocean"
    );
    // Otherwise the type 15 by each cell's basic kind, the others by their own.
    for (cell, expected) in [(0, 13), (2, 19), (3, 20), (4, 13), (6, 19), (7, 20)] {
        assert_eq!(liquid::liquid_type(0, &group(15, 0), cell), expected, "{cell}");
    }
    assert_eq!(liquid::liquid_type(0, &group(2, 0), 0), 19);
    assert_eq!(liquid::liquid_type(0, &group(17, 0), 0), 14, "a basic kind up to 19");
    assert_eq!(liquid::liquid_type(0, &group(25, 0), 0), 26);
}

#[test]
fn the_liquid_of_a_group_is_two_triangles_a_cell_drawn_by_its_type_in_the_axes_of_the_building() {
    let cell = uniwow_api::liquids::CELL;
    let liquid = |cells: Vec<u8>| WmoGroup {
        liquid_type: 15,
        liquid: Some(WmoLiquid {
            size: [3, 2],
            tiles: [2, 1],
            corner: [10.0, 20.0, 5.0],
            material: 0,
            heights: vec![5.0, 5.0, 6.0, 5.0, 5.0, 6.0],
            data: vec![[51, 0, 0, 0]; 6],
            tile_flags: cells,
        }),
        ..WmoGroup::default()
    };
    // Of its second group: a cell of water, then one of magma.
    let wmo = Wmo {
        groups: vec![WmoGroup::default(), liquid(vec![0x0, 0x2])],
        ..Wmo::default()
    };
    let liquids = liquid::liquids(&wmo);
    assert_eq!(
        liquids
            .iter()
            .map(|liquid| (liquid.group, liquid.liquid))
            .collect::<Vec<_>>(),
        [(1, 13), (1, 19)]
    );
    let water = &liquids[0];
    assert_eq!(
        water.positions,
        [
            [10.0, 20.0, 5.0],
            [10.0 + cell, 20.0, 5.0],
            [10.0 + cell, 20.0 + cell, 5.0],
            [10.0, 20.0 + cell, 5.0]
        ]
    );
    assert_eq!(
        (water.depths[0], water.coordinates[2], water.triangles.as_slice()),
        (0.2, [0.25, 0.25], [0, 1, 2, 0, 2, 3].as_slice()),
        "its depth from its flow, two repeats a chunk"
    );
    let magma = &liquids[1];
    assert_eq!(magma.positions[1], [10.0 + 2.0 * cell, 20.0, 6.0]);
    assert_eq!(
        (magma.coordinates[0], magma.depths[0]),
        ([0.2, 0.0], 1.0),
        "its coordinates from its vertex"
    );
    // Cells not drawn: none.
    let dry = Wmo {
        groups: vec![liquid(vec![0x0F, 0x08])],
        ..Wmo::default()
    };
    assert!(liquid::liquids(&dry).is_empty());
}
