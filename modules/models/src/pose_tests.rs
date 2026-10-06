//! Tests of the pose of a model from its tracks.

use uniwow_api::formats::{Animation, Bone, Interpolation, Keys, Track};
use uniwow_api::glam::{Mat4, Quat, Vec3};

use crate::pose::{Moment, Posing, Value, pose, rows, sample};

/// A track of `interpolation` with `keys` (time, value) in the sequence 0.
fn track<T: Copy>(interpolation: Interpolation, keys: &[(u32, T)]) -> Track<T> {
    Track {
        interpolation,
        global: None,
        keys: vec![Keys {
            times: keys.iter().map(|(time, _)| *time).collect(),
            values: keys.iter().map(|(_, value)| *value).collect(),
            tangents: Vec::new(),
        }],
    }
}

fn at(time: u32) -> Moment {
    Moment {
        sequence: 0,
        time,
        clock: 0,
    }
}

#[test]
fn a_track_is_read_in_steps_in_a_line_and_along_its_curves() {
    let line = track(Interpolation::Linear, &[(0, 0.0f32), (1000, 10.0)]);
    assert_eq!(sample(&line, at(250), &[]), Some(2.5));
    assert_eq!(sample(&line, at(0), &[]), Some(0.0), "at the first key");
    assert_eq!(sample(&line, at(5000), &[]), Some(10.0), "held past the last");
    let steps = Track {
        interpolation: Interpolation::Step,
        ..line.clone()
    };
    assert_eq!(sample(&steps, at(999), &[]), Some(0.0));
    // A curve: each key with its tangents in and out.
    let mut curve = track(Interpolation::Bezier, &[(0, 0.0f32), (1000, 1.0)]);
    curve.keys[0].tangents = vec![[0.0, 0.0], [1.0, 1.0]];
    assert_eq!(
        sample(&curve, at(500), &[]),
        Some(0.5),
        "a symmetric curve, at its middle"
    );
    curve.interpolation = Interpolation::Hermite;
    curve.keys[0].tangents = vec![[0.0, 0.0], [0.0, 0.0]];
    assert_eq!(sample(&curve, at(500), &[]), Some(0.5));
    assert!(sample(&curve, at(250), &[]).unwrap() < 0.25, "slow at its start");
    // Nothing in another sequence.
    assert_eq!(sample(&line, Moment { sequence: 1, ..at(0) }, &[]), None);
}

#[test]
fn a_track_on_a_global_sequence_loops_on_its_length_whatever_the_sequence() {
    let mut track = track(Interpolation::Linear, &[(0, 0.0f32), (1000, 10.0)]);
    track.global = Some(0);
    let moment = Moment {
        sequence: 5,
        time: 0,
        clock: 2250,
    };
    assert_eq!(sample(&track, moment, &[1000]), Some(2.5));
}

/// The angle about z, in degrees, of the rotation `quaternion`.
fn degrees(quaternion: [f32; 4]) -> f32 {
    let [x, y, z, w] = quaternion;
    let (axis, angle) = Quat::from_xyzw(x, y, z, w).to_axis_angle();
    (angle * axis.z.signum()).to_degrees()
}

#[test]
fn a_rotation_between_keys_far_apart_follows_the_arc_by_a_slerp() {
    let quarter = Quat::from_rotation_z(160f32.to_radians()).to_array();
    let turning = track(
        Interpolation::Linear,
        &[(0, Quat::IDENTITY.to_array()), (1000, quarter)],
    );
    let seen = degrees(sample(&turning, at(250), &[]).unwrap());
    assert!((seen - 40.0).abs() < 0.01, "a quarter of the way: {seen}");
    // A normalised lerp would have turned it by 31.6 degrees.
    let [x, y, z, w] = Quat::IDENTITY.to_array().mix(quarter, 0.25);
    assert!((degrees([x, y, z, w]) - 40.0).abs() < 0.01);
}

/// A bone of `parent` turning about `pivot` by the rotations `turns` (time, degrees about z).
fn bone(parent: Option<u16>, pivot: [f32; 3], turns: &[(u32, f32)], flags: u32) -> Bone {
    let keys: Vec<(u32, [f32; 4])> = turns
        .iter()
        .map(|(time, angle)| (*time, Quat::from_rotation_z(angle.to_radians()).to_array()))
        .collect();
    Bone {
        key_bone: -1,
        flags,
        parent,
        pivot,
        translation: Track::default(),
        rotation: track(Interpolation::Linear, &keys),
        scale: Track::default(),
    }
}

/// Where `matrix` takes `point`.
fn moved(matrix: &Mat4, point: [f32; 3]) -> Vec3 {
    matrix.transform_point3(Vec3::from(point))
}

fn near(a: Vec3, b: [f32; 3]) -> bool {
    a.distance(Vec3::from(b)) < 1e-4
}

#[test]
fn a_bone_turns_about_its_pivot_and_its_children_with_it() {
    // The child before its parent: computed after it.
    let animation = Animation {
        bones: vec![
            bone(Some(1), [2.0, 0.0, 0.0], &[(0, 0.0), (1000, 90.0)], 0),
            bone(None, [1.0, 0.0, 0.0], &[(0, 0.0), (1000, 90.0)], 0),
        ],
        order: vec![1, 0],
        ..Animation::default()
    };
    let mut out = [Mat4::IDENTITY; 2];
    let posing = Posing {
        moment: at(1000),
        before: None,
        camera: None,
    };
    pose(&animation, &posing, &mut out);
    // The parent turns (2, 0, 0) a quarter about (1, 0, 0): to (1, 1, 0).
    assert!(near(moved(&out[1], [2.0, 0.0, 0.0]), [1.0, 1.0, 0.0]));
    // The child turns (3, 0, 0) about its pivot (2, 0, 0) to (2, 1, 0), which its parent turns to
    // (0, 1, 0).
    assert!(
        near(moved(&out[0], [3.0, 0.0, 0.0]), [0.0, 1.0, 0.0]),
        "{}",
        moved(&out[0], [3.0, 0.0, 0.0])
    );
    // At rest at the first key.
    pose(
        &animation,
        &Posing {
            moment: at(0),
            ..posing
        },
        &mut out,
    );
    assert!(near(moved(&out[0], [3.0, 0.0, 0.0]), [3.0, 0.0, 0.0]));
    assert_eq!(
        rows(&Mat4::IDENTITY),
        [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]
    );
}

#[test]
fn a_sequence_blends_into_the_next_by_its_weight() {
    let animation = Animation {
        bones: vec![bone(None, [0.0; 3], &[(0, 0.0), (1000, 90.0)], 0)],
        order: vec![0],
        ..Animation::default()
    };
    let mut out = [Mat4::IDENTITY];
    // At 90 degrees, a third still at 0: 60 degrees.
    let posing = Posing {
        moment: at(1000),
        before: Some((at(0), 1.0 / 3.0)),
        camera: None,
    };
    pose(&animation, &posing, &mut out);
    let turned = moved(&out[0], [1.0, 0.0, 0.0]);
    assert!((turned.y.atan2(turned.x).to_degrees() - 60.0).abs() < 0.01, "{turned}");
}

#[test]
fn a_billboard_faces_the_camera_about_its_pivot() {
    let animation = Animation {
        bones: vec![bone(None, [0.0, 0.0, 1.0], &[], 0x08)],
        order: vec![0],
        ..Animation::default()
    };
    let mut out = [Mat4::IDENTITY];
    // The camera looking down y: its back is -y, its left x... in the space of the model.
    let posing = Posing {
        moment: at(0),
        before: None,
        camera: Some([Vec3::NEG_Y, Vec3::X, Vec3::Z]),
    };
    pose(&animation, &posing, &mut out);
    // The front of the billboard, +x from its pivot, turned to the camera: -y.
    assert!(near(moved(&out[0], [1.0, 0.0, 1.0]), [0.0, -1.0, 1.0]));
    assert!(near(moved(&out[0], [0.0, 0.0, 2.0]), [0.0, 0.0, 2.0]), "its up kept");
}
