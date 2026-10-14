//! Tests of what the light of the map takes from a building where the eye stands, on buildings the
//! tests make.

use uniwow_api::buildings::Buildings as _;
use uniwow_api::formats::{BspNode, FileRef, FogBand, Portal, PortalRef, Wmo, WmoFog, WmoGroup, WmoLiquid};
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::liquids::Immersed;

use crate::cells::Cells;
use crate::eye::{self, Lights};
use crate::liquid;

/// A fog band ending at `end`, starting at `start` of it, grey of `grey`.
fn band(end: f32, start: f32, grey: u8) -> FogBand {
    FogBand {
        end,
        start,
        colour: [grey, grey, grey, 255],
    }
}

/// A fog at `position` within `radii`, of flags `flags`, its fog in the air `fog` and under the
/// water the same darker.
fn fog(flags: u32, position: [f32; 3], radii: [f32; 2], fog: FogBand) -> WmoFog {
    WmoFog {
        flags,
        position,
        radii,
        fog,
        underwater: FogBand {
            colour: fog.colour.map(|channel| channel / 2),
            ..fog
        },
    }
}

/// A building of one group naming the fogs `named`, of the fogs `fogs`.
fn fogged(fogs: Vec<WmoFog>, named: [u8; 4]) -> Lights {
    Lights::new(&Wmo {
        fogs,
        groups: vec![WmoGroup {
            flags: 0x2000,
            fogs: named,
            ..WmoGroup::default()
        }],
        ..Wmo::default()
    })
}

#[test]
fn the_fog_of_a_building_is_its_first_fog_mixed_with_those_its_group_names_the_farthest_first() {
    let first = fog(0x1, [0.0; 3], [0.0; 2], band(400.0, 0.25, 100));
    // A building of one fog alone gives none.
    assert_eq!(fogged(vec![first], [0; 4]).fog(0, Vec3::ZERO), None);
    // Two more: whole within 10 yards of their centre, falling to nothing at 30.
    let near = fog(0, [0.0; 3], [10.0, 30.0], band(100.0, 0.5, 200));
    let other = fog(0, [40.0, 0.0, 0.0], [10.0, 30.0], band(50.0, 0.0, 0));
    let lights = fogged(vec![first, near, other], [1, 2, 0, 0]);
    // Within the first radius of the near one, out of the other's: wholly the near one.
    let [air, water] = lights.fog(0, Vec3::new(5.0, 0.0, 0.0)).unwrap();
    assert_eq!((air.end, air.start, air.colour[0]), (100.0, 0.5, 200));
    assert_eq!(water.colour[0], 100, "under the water, its own");
    // 20 yards from it, halfway between its radii: half of it, the colours by the bytes; the
    // other 20 yards away too, half of it after (both at the same distance, in their order).
    let [air, _] = lights.fog(0, Vec3::new(20.0, 0.0, 0.0)).unwrap();
    let half_near = 400.0 + (100.0 - 400.0) * 0.5;
    assert!(
        (air.end - (half_near + (50.0 - half_near) * 0.5)).abs() < 1e-3,
        "{air:?}"
    );
    // 100 + (128 · (200 − 100) >> 8) = 150, then 150 + (128 · (0 − 150) >> 8) = 75.
    assert_eq!(air.colour[0], 75);
    // Far from both: the first, whole.
    assert_eq!(lights.fog(0, Vec3::new(500.0, 0.0, 0.0)).unwrap()[0], first.fog);
    // A fog the group does not name, or whose radii are left out (0x1), is not mixed in.
    let lights = fogged(
        vec![first, near, fog(0x1, [0.0; 3], [10.0, 30.0], band(1.0, 0.0, 0))],
        [2, 0, 0, 0],
    );
    assert_eq!(lights.fog(0, Vec3::ZERO).unwrap()[0], first.fog);
    // A group unknown: none.
    assert_eq!(lights.fog(5, Vec3::ZERO), None);
}

#[test]
fn the_fogs_of_a_volume_are_weighed_as_a_mean_the_first_filling_what_they_leave() {
    let first = fog(0x1000, [0.0; 3], [0.0; 2], band(400.0, 0.0, 100));
    let near = fog(0, [0.0; 3], [10.0, 30.0], band(100.0, 0.5, 200));
    // Halfway between its radii: half of it and half of the first.
    let lights = fogged(vec![first, near], [1, 0, 0, 0]);
    let [air, water] = lights.fog(0, Vec3::new(20.0, 0.0, 0.0)).unwrap();
    assert!(
        (air.end - 250.0).abs() < 1e-3 && (air.start - 0.25).abs() < 1e-5,
        "{air:?}"
    );
    assert_eq!((air.colour[0], water.colour[0]), (150, 75));
    // With 0x10000 too, none.
    let lights = fogged(
        vec![fog(0x11000, [0.0; 3], [0.0; 2], band(400.0, 0.0, 100)), near],
        [1, 0, 0, 0],
    );
    assert_eq!(lights.fog(0, Vec3::ZERO), None);
    // A volume of one fog alone: the first.
    assert_eq!(fogged(vec![first], [0; 4]).fog(0, Vec3::ZERO).unwrap()[0], first.fog);
}

/// A building of a room from 0 to 10 on each axis, its floor at 0 and its ceiling at 10, a portal
/// in its wall at X 10, from Y 4 to 6 and Z 0 to 4; a pool in it of water of two cells from 1 on X
/// and Y, its second cell not drawn, its surface at 2; and a group outside beside it, from X 20 to
/// 30, a pool of magma at 1 in it.
fn room() -> Wmo {
    let floor = |z: f32, up: f32, flags: u32| WmoGroup {
        flags,
        bounds: [[0.0; 3], [10.0, 10.0, 10.0]],
        portals: [0, 1],
        vertices: vec![[0.0, 0.0, z], [20.0, 0.0, z], [0.0, 20.0, z]],
        normals: vec![[0.0, 0.0, up]; 3],
        triangles: vec![0, 1, 2],
        bsp: vec![BspNode {
            flags: 0x4,
            children: [-1, -1],
            count: 1,
            first: 0,
            distance: 0.0,
        }],
        bsp_faces: vec![0],
        ..WmoGroup::default()
    };
    let mut inside = floor(0.0, 1.0, 0x2000 | 0x40000);
    // A ceiling over it.
    inside
        .vertices
        .extend([[0.0, 0.0, 10.0], [20.0, 0.0, 10.0], [0.0, 20.0, 10.0]]);
    inside.normals.extend([[0.0, 0.0, -1.0]; 3]);
    inside.triangles.extend([3, 4, 5]);
    inside.bsp[0].count = 2;
    inside.bsp_faces = vec![0, 1];
    inside.fogs = [1, 0, 0, 0];
    inside.liquid = Some(WmoLiquid {
        size: [3, 2],
        tiles: [2, 1],
        corner: [1.0, 1.0, 2.0],
        material: 0,
        heights: vec![2.0; 6],
        data: vec![[0; 4]; 6],
        tile_flags: vec![0x0, 0x8],
    });
    let outside = WmoGroup {
        flags: 0x8,
        bounds: [[20.0, 0.0, 0.0], [30.0, 10.0, 10.0]],
        liquid_type: 2,
        liquid: Some(WmoLiquid {
            size: [3, 3],
            tiles: [2, 2],
            corner: [20.0, 0.0, 1.0],
            material: 0,
            heights: vec![1.0; 9],
            data: vec![[0; 4]; 9],
            tile_flags: vec![0x2; 4],
        }),
        ..WmoGroup::default()
    };
    Wmo {
        bounds: [[0.0; 3], [30.0, 10.0, 10.0]],
        skybox: Some(FileRef::Path(r"Environments\Stars\Room.m2".to_owned())),
        groups: vec![inside, outside],
        portals: vec![Portal {
            vertices: vec![[10.0, 4.0, 0.0], [10.0, 6.0, 0.0], [10.0, 6.0, 4.0], [10.0, 4.0, 4.0]],
            plane: [1.0, 0.0, 0.0, -10.0],
        }],
        portal_refs: vec![PortalRef {
            portal: 0,
            group: 1,
            side: -1,
        }],
        fogs: vec![
            fog(0x1, [0.0; 3], [0.0; 2], band(400.0, 0.25, 100)),
            fog(0, [5.0, 5.0, 5.0], [50.0, 60.0], band(30.0, 0.5, 200)),
        ],
        ..Wmo::default()
    }
}

#[test]
fn the_eye_is_measured_from_the_portals_of_its_group_and_lies_in_the_liquid_of_its_group() {
    let wmo = room();
    let (cells, lights) = (Cells::new(&wmo), Lights::new(&wmo));
    // 2 yards before the portal, facing its middle; beside it, from its nearest edge.
    assert!((eye::exit(&cells, 0, Vec3::new(8.0, 5.0, 2.0)) - 2.0).abs() < 1e-5);
    let aside = eye::exit(&cells, 0, Vec3::new(7.0, 9.0, 2.0));
    assert!((aside - (9.0f32 + 9.0).sqrt()).abs() < 1e-4, "{aside}");
    assert_eq!(
        eye::exit(&cells, 1, Vec3::ZERO),
        f32::INFINITY,
        "a group without portals"
    );
    // In the pool, under its surface or a hundredth over it: water of the buildings; over it, beside
    // it, or in its cell not drawn, none.
    let kind = |eye: Vec3| lights.liquid(0, eye).map(|found| found.liquid);
    assert_eq!(
        lights.liquid(0, Vec3::new(2.0, 2.0, 1.0)),
        Some(Immersed { liquid: 13, depth: 1.0 }),
        "a yard under its surface"
    );
    assert_eq!(kind(Vec3::new(2.0, 2.0, 2.005)), Some(13));
    assert_eq!(kind(Vec3::new(2.0, 2.0, 2.5)), None);
    assert_eq!(kind(Vec3::new(2.0, 8.0, 1.0)), None);
    assert_eq!(kind(Vec3::new(7.0, 2.0, 1.0)), None, "its cell not drawn");
    // The magma of the group outside holding the eye, as its type gives it.
    let outside = |eye: Vec3| lights.liquid_outside(&cells, eye).map(|found| found.liquid);
    assert_eq!(outside(Vec3::new(21.0, 1.0, 0.5)), Some(19));
    assert_eq!(outside(Vec3::new(2.0, 2.0, 1.0)), None, "in the room");
    // A room, its sky shown; the group outside neither.
    assert!(lights.room(0) && !lights.room(1));
    assert_eq!(lights.skybox(0), wmo.skybox);
    assert_eq!(lights.skybox(1), None);
    // The light of the room for its liquid: the ambient colour of the building; none outside, nor in
    // a building lit as one.
    let mut lit = wmo.clone();
    lit.ambient = [51, 102, 0, 255];
    assert_eq!(liquid::room(&lit, &lit.groups[0]), Some([0.2, 0.4, 0.0]));
    assert_eq!(liquid::room(&lit, &lit.groups[1]), None);
    lit.flags = 0x2;
    assert_eq!(liquid::room(&lit, &lit.groups[0]), None);
    assert!(liquid::liquids(&lit).iter().all(|liquid| liquid.room.is_none()));
}

#[test]
fn the_layer_finds_where_the_eye_stands_among_the_buildings() {
    let wmo = room();
    let Some(gpu) = crate::tests::device() else {
        return;
    };
    let Some(shared) = crate::gpu::Shared::new(&gpu, &crate::tests::TARGET)
        .ok()
        .map(std::sync::Arc::new)
    else {
        return;
    };
    let uploaded = std::sync::Arc::new(crate::gpu::upload(&shared, &crate::tests::NoFiles, wmo).unwrap());
    // The building moved 100 yards along X.
    let placed = vec![crate::layer::Placed {
        transform: Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0)),
        wmo: uploaded,
        parts: None,
        liquids: None,
    }];
    let at = |eye: Vec3| {
        let mut view = crate::tests::view(eye);
        view.view = Mat4::look_at_rh(eye, eye + Vec3::new(-1.0, 0.0, 0.0), Vec3::Z);
        view.view_proj = Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1) * view.view;
        crate::layer::list(&placed, &view, None).eye
    };
    // In the room, in its pool: its group, its fog, its sky, its water; 4 yards from the portal.
    let eye = at(Vec3::new(106.0, 5.0, 1.0));
    let group = eye.group.expect("in the room");
    assert_eq!(group.flags, 0x2000 | 0x40000);
    assert!(group.room());
    assert!((group.exit - 4.0).abs() < 1e-4, "{}", group.exit);
    assert_eq!(group.fog.map(|[air, _]| air.end), Some(30.0));
    assert!(group.skybox.is_some());
    assert_eq!(
        eye.liquid,
        Some(None),
        "in the room, out of its pool: none, not the terrain's"
    );
    let kind = |eye: uniwow_api::buildings::Eye| eye.liquid.map(|found| found.map(|found| found.liquid));
    assert_eq!(
        at(Vec3::new(102.0, 2.0, 1.0)).liquid,
        Some(Some(Immersed { liquid: 13, depth: 1.0 }))
    );
    // In the group outside, in its magma: the magma, no group.
    let eye = at(Vec3::new(121.0, 1.0, 0.5));
    assert_eq!(eye.group, None);
    assert_eq!(kind(eye), Some(Some(19)));
    // Out of the building: nothing, the liquid left to the terrain.
    assert_eq!(at(Vec3::new(50.0, 5.0, 1.0)), uniwow_api::buildings::Eye::default());
    // Given by the service as the layer left it.
    let scene = std::sync::Arc::new(std::sync::Mutex::new(crate::layer::Scene::default()));
    scene.lock().unwrap().eye = at(Vec3::new(102.0, 2.0, 1.0));
    assert_eq!(kind(crate::Seen(scene).eye()), Some(Some(13)));
}
