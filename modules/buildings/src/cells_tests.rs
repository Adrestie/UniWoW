//! Tests of the groups of a building seen from inside: the group the camera is in, by the triangles
//! of its tree under and over it, and the groups seen through the portals.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use uniwow_api::formats::{BspNode, Portal, PortalRef, Wmo, WmoGroup};
use uniwow_api::glam::{Mat4, Quat, Vec3, Vec4};

use crate::cells::{self, Cells, Floor};
use crate::gpu::{self, Shared};
use crate::layer::{self, Placed};
use crate::tests::{NoFiles, TARGET, device};

const INSIDE: u32 = 0x2000;
const OUTSIDE: u32 = 0x8;
const OPEN: u32 = 0x40;

/// The sides of the view from `eye` towards `target`.
fn sides(eye: Vec3, target: Vec3) -> [Vec4; 5] {
    let view_proj =
        Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1) * Mat4::look_at_rh(eye, target, Vec3::Z);
    layer::planes(&view_proj)
}

/// A leaf holding the triangles `first` to `first + count` of its group's leaves.
fn leaf(first: u32, count: u16) -> BspNode {
    BspNode {
        flags: 0x4,
        children: [-1, -1],
        first,
        count,
        distance: 0.0,
    }
}

/// A room inside between `low` and `high`: its floor facing up and its ceiling facing down, two
/// triangles each, in one leaf; its portals `portals` among the references.
fn room(low: [f32; 3], high: [f32; 3], portals: [u16; 2]) -> WmoGroup {
    let ([x0, y0, z0], [x1, y1, z1]) = (low, high);
    WmoGroup {
        flags: INSIDE,
        bounds: [low, high],
        portals,
        vertices: vec![
            [x0, y0, z0],
            [x1, y0, z0],
            [x1, y1, z0],
            [x0, y1, z0],
            [x0, y0, z1],
            [x1, y0, z1],
            [x1, y1, z1],
            [x0, y1, z1],
        ],
        normals: [[0.0, 0.0, 1.0]; 4].into_iter().chain([[0.0, 0.0, -1.0]; 4]).collect(),
        triangles: vec![0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6],
        bsp: vec![leaf(0, 4)],
        bsp_faces: vec![0, 1, 2, 3],
        ..WmoGroup::default()
    }
}

#[test]
fn the_camera_is_in_the_group_inside_whose_floor_is_the_nearest_under_it_under_its_ceiling() {
    // A street lit as outside, open to the sky, and a room without a ceiling.
    let mut street = room([20.0, 0.0, 0.0], [30.0, 10.0, 4.0], [0, 0]);
    street.bsp = vec![leaf(0, 2)];
    street.bsp_faces = vec![0, 1];
    street.flags |= OPEN;
    street.bounds[1][2] = 20.0;
    let mut roofless = street.clone();
    roofless.flags = INSIDE;
    roofless.bounds = [[30.0, 0.0, 0.0], [40.0, 10.0, 4.0]];
    roofless.vertices.iter_mut().for_each(|vertex| vertex[0] += 10.0);
    // A room whose bounds reach past its roof, as a sloped roof's do.
    let mut attic = room([50.0, 0.0, 0.0], [60.0, 10.0, 4.0], [0, 0]);
    attic.bounds[1][2] = 8.0;
    let mut outside = room([0.0, 0.0, 0.0], [10.0, 10.0, 12.0], [0, 0]);
    outside.flags = OUTSIDE;
    let wmo = Wmo {
        groups: vec![
            room([0.0, 0.0, 0.0], [10.0, 10.0, 4.0], [0, 0]),
            room([0.0, 0.0, 4.0], [10.0, 10.0, 8.0], [0, 0]),
            // A platform in the first room.
            room([2.0, 2.0, 2.0], [8.0, 8.0, 4.0], [0, 0]),
            outside,
            street,
            roofless,
            attic,
        ],
        ..Wmo::default()
    };
    let cells = Cells::new(&wmo);
    let holding = |x, y, z| cells.holding(Vec3::new(x, y, z));
    assert_eq!(holding(5.0, 5.0, 1.0), Some(0));
    assert_eq!(
        holding(5.0, 5.0, 3.0),
        Some(2),
        "its floor the nearest under the camera"
    );
    assert_eq!(holding(1.0, 1.0, 3.0), Some(0), "beside the platform");
    assert_eq!(holding(5.0, 5.0, 6.0), Some(1));
    assert_eq!(
        holding(5.0, 5.0, 4.05),
        Some(1),
        "over the ceiling of the first, facing down"
    );
    assert_eq!(holding(25.0, 5.0, 1.0), Some(4), "a street open to the sky");
    assert_eq!(holding(25.0, 5.0, 7.5), Some(4));
    assert_eq!(holding(25.0, 5.0, 12.0), None, "high over the street");
    assert_eq!(holding(35.0, 5.0, 1.0), None, "a room without a ceiling");
    assert_eq!(holding(55.0, 5.0, 2.0), Some(6));
    assert_eq!(holding(55.0, 5.0, 6.0), None, "over its roof");
    assert_eq!(holding(15.0, 5.0, 1.0), None);
    assert_eq!(holding(5.0, 5.0, 10.0), None, "in a group outside only");
}

#[test]
fn the_tree_gives_the_nearest_triangle_under_the_camera_facing_up_and_one_over_it_facing_down() {
    // Two floors side by side cut across X at 5; over the right one a ceiling, cut from its floor
    // across Z at 2; and a ramp on the left, rising along X from 1 to 2.
    let floor = Floor {
        nodes: vec![
            BspNode {
                flags: 0,
                children: [1, 2],
                first: 0,
                count: 0,
                distance: 5.0,
            },
            leaf(0, 3),
            BspNode {
                flags: 2,
                children: [3, 4],
                first: 0,
                count: 0,
                distance: 2.0,
            },
            leaf(3, 2),
            leaf(5, 2),
        ],
        faces: vec![0, 1, 6, 2, 3, 4, 5],
        vertices: [
            [0.0, 0.0, 0.0],
            [5.0, 0.0, 0.0],
            [5.0, 10.0, 0.0],
            [0.0, 10.0, 0.0],
            [5.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [10.0, 10.0, 0.0],
            [5.0, 10.0, 0.0],
            [5.0, 0.0, 4.0],
            [10.0, 0.0, 4.0],
            [10.0, 10.0, 4.0],
            [5.0, 10.0, 4.0],
            [0.0, 0.0, 1.0],
            [5.0, 0.0, 2.0],
            [0.0, 10.0, 1.0],
        ]
        .map(Vec3::from)
        .to_vec(),
        normals: [[0.0, 0.0, 1.0]; 8]
            .into_iter()
            .chain([[0.0, 0.0, -1.0]; 4])
            .chain([[-0.2, 0.0, 0.98]; 3])
            .map(Vec3::from)
            .collect(),
        triangles: vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 10, 9, 8, 11, 10, 12, 13, 14],
    };
    let around = |x, y, z| {
        let (under, over) = floor.around(Vec3::new(x, y, z));
        (under.map(|height| (height * 100.0).round() / 100.0), over)
    };
    assert_eq!(around(2.0, 5.0, 3.0), (Some(1.4), false), "the ramp the nearest");
    assert_eq!(around(2.0, 5.0, 0.5), (Some(0.0), false), "the ramp over it facing up");
    assert_eq!(around(7.0, 5.0, 1.0), (Some(0.0), true), "under the ceiling");
    assert_eq!(
        around(7.0, 5.0, 5.0),
        (Some(0.0), false),
        "the ceiling under it facing down"
    );
    assert_eq!(around(7.0, 5.0, -1.0), (None, true));
    // Beside the ramp, past each of its three sides: its floor alone under it, or nothing.
    let ramp = Floor {
        nodes: vec![leaf(0, 1)],
        faces: vec![0],
        vertices: vec![Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), Vec3::new(0.0, 10.0, 0.0)],
        normals: vec![Vec3::Z; 3],
        triangles: vec![0, 1, 2],
    };
    for beside in [
        Vec3::new(8.0, 8.0, 1.0),
        Vec3::new(-1.0, 5.0, 1.0),
        Vec3::new(5.0, -1.0, 1.0),
    ] {
        assert_eq!(ramp.around(beside), (None, false), "{beside}");
    }
    assert_eq!(ramp.around(Vec3::new(2.0, 2.0, 1.0)), (Some(0.0), false));
}

#[test]
fn a_portal_clipped_by_the_view_lets_it_see_through_its_sides_only_past_it() {
    let square = |x: f32| {
        vec![
            Vec3::new(x, -1.0, -1.0),
            Vec3::new(x, 1.0, -1.0),
            Vec3::new(x, 1.0, 1.0),
            Vec3::new(x, -1.0, 1.0),
        ]
    };
    let clipped = cells::clip(&square(0.0), Vec4::new(0.0, 1.0, 0.0, -0.5));
    assert_eq!(clipped.len(), 4);
    assert!(clipped.iter().all(|point| point.y >= 0.5 - 1e-6), "{clipped:?}");
    assert!(cells::clip(&square(0.0), Vec4::new(0.0, 1.0, 0.0, -2.0)).is_empty());
    let planes = cells::through(Vec3::ZERO, &square(10.0), Vec4::new(1.0, 0.0, 0.0, -10.0));
    let inside = |point: Vec3| planes.iter().all(|plane| plane.truncate().dot(point) + plane.w >= 0.0);
    assert!(inside(Vec3::new(20.0, 0.0, 0.0)));
    assert!(inside(Vec3::new(20.0, 1.9, 1.9)), "within its sides, twice as far");
    assert!(!inside(Vec3::new(20.0, 2.5, 0.0)));
    assert!(!inside(Vec3::new(5.0, 0.0, 0.0)), "before it");
}

/// Three rooms along X joined by doors at 10 and 20, and a group outside past a door at 30; the
/// door at 20 between `door_y`.
fn row(door_y: [f32; 2]) -> Wmo {
    let door = |x: f32, y: [f32; 2]| Portal {
        vertices: vec![[x, y[0], 0.0], [x, y[1], 0.0], [x, y[1], 3.0], [x, y[0], 3.0]],
        plane: [1.0, 0.0, 0.0, -x],
    };
    let passage = |portal, group, side| PortalRef { portal, group, side };
    let mut outside = room([30.0, -5.0, 0.0], [40.0, 5.0, 4.0], [5, 1]);
    outside.flags = OUTSIDE;
    Wmo {
        groups: vec![
            room([0.0, -5.0, 0.0], [10.0, 5.0, 4.0], [0, 1]),
            room([10.0, -5.0, 0.0], [20.0, 5.0, 4.0], [1, 2]),
            room([20.0, -5.0, 0.0], [30.0, 5.0, 4.0], [3, 2]),
            outside,
        ],
        portals: vec![door(10.0, [-1.0, 1.0]), door(20.0, door_y), door(30.0, [-1.0, 1.0])],
        portal_refs: vec![
            passage(0, 1, -1),
            passage(0, 0, 1),
            passage(1, 2, -1),
            passage(1, 1, 1),
            passage(2, 3, -1),
            passage(2, 2, 1),
        ],
        bounds: [[0.0, -5.0, 0.0], [40.0, 5.0, 4.0]],
        ..Wmo::default()
    }
}

#[test]
fn the_groups_seen_are_those_the_portals_in_sight_lead_to_from_the_side_of_the_camera() {
    let cells = Cells::new(&row([-1.0, 1.0]));
    let seen = |eye: Vec3, target: Vec3| {
        let start = cells.holding(eye).expect("in a room");
        cells.seen(eye, &sides(eye, target), start)
    };
    let (first, second) = (Vec3::new(5.0, 0.0, 2.0), Vec3::new(15.0, 0.0, 2.0));
    assert_eq!(
        seen(first, Vec3::new(100.0, 0.0, 2.0)),
        [true; 4],
        "along the row, to the outside"
    );
    assert_eq!(seen(first, Vec3::new(-100.0, 0.0, 2.0)), [true, false, false, false]);
    assert_eq!(seen(second, Vec3::new(-100.0, 0.0, 2.0)), [true, true, false, false]);
    assert_eq!(seen(second, Vec3::new(100.0, 0.0, 2.0)), [false, true, true, true]);
    // The second door aside: past the first, the view no longer reaches it.
    let aside = Cells::new(&row([4.0, 5.0]));
    let start = aside.holding(first).unwrap();
    assert_eq!(
        aside.seen(first, &sides(first, Vec3::new(100.0, 0.0, 2.0)), start),
        [true, true, false, false]
    );
}

#[test]
fn a_building_the_camera_is_inside_of_draws_and_shows_the_doodads_of_the_groups_its_portals_let_it_see() {
    let Some(gpu) = device() else {
        return;
    };
    let Ok(shared) = Shared::new(&gpu, &TARGET) else {
        return;
    };
    let shared = Arc::new(shared);
    let wmo = Arc::new(gpu::upload(&shared, &NoFiles, row([-1.0, 1.0])).unwrap());
    let flag = || Arc::new(AtomicBool::new(true));
    let parts = vec![
        (vec![0], flag()),
        (vec![2], flag()),
        (vec![1, 3], flag()),
        (vec![], flag()),
    ];
    // Turned a quarter about Z, then moved: X of the building along Y of the world.
    let transform = Mat4::from_rotation_translation(
        Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
        Vec3::new(100.0, 0.0, 0.0),
    );
    let placed = [Placed {
        transform,
        wmo,
        parts: Some(parts.clone().into()),
    }];
    let at = |x: f32, z: f32| transform.transform_point3(Vec3::new(x, 0.0, z));
    let shown = || {
        parts
            .iter()
            .map(|(_, flag)| flag.load(Ordering::Relaxed))
            .collect::<Vec<_>>()
    };
    let view = |eye: Vec3, target: Vec3| {
        let mut view = crate::tests::view(eye);
        view.view_proj = Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1)
            * Mat4::look_at_rh(eye, target, Vec3::Z);
        view
    };
    // In the first room looking back: itself alone, drawn alone.
    let listing = layer::list(&placed, &view(at(5.0, 2.0), at(-100.0, 2.0)), None);
    assert_eq!((listing.inside, listing.through, listing.groups), (1, 1, 1));
    assert_eq!(shown(), [true, false, false, true]);
    // Out of sight: all shown again, for when it is seen from outside.
    let listing = layer::list(&placed, &view(at(-50.0, 2.0), at(-100.0, 2.0)), None);
    assert_eq!(listing.buildings, 0);
    assert_eq!(shown(), [true; 4]);
    layer::list(&placed, &view(at(5.0, 2.0), at(-100.0, 2.0)), None);
    // Along the row: every group.
    let listing = layer::list(&placed, &view(at(5.0, 2.0), at(100.0, 2.0)), None);
    assert_eq!((listing.inside, listing.through), (1, 4));
    assert_eq!(shown(), [true; 4]);
    // From outside the building: all of them, its groups by their bounds.
    layer::list(&placed, &view(at(5.0, 2.0), at(-100.0, 2.0)), None);
    let listing = layer::list(&placed, &view(at(-50.0, 2.0), at(100.0, 2.0)), None);
    assert_eq!((listing.inside, listing.groups), (0, 4));
    assert_eq!(shown(), [true; 4]);
    // The second door aside: the rooms past it in sight, not drawn.
    let aside = [Placed {
        transform,
        wmo: Arc::new(gpu::upload(&shared, &NoFiles, row([4.0, 5.0])).unwrap()),
        parts: None,
    }];
    let listing = layer::list(&aside, &view(at(5.0, 2.0), at(100.0, 2.0)), None);
    assert_eq!((listing.through, listing.groups), (2, 2));
}
