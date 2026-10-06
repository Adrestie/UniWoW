//! Tests of the materials that move: which do, their values at a moment, and the slots their
//! batches point to.

use uniwow_api::formats::{Interpolation, Keys, Model, TextureTransform, Track};
use uniwow_api::glam::Quat;
use uniwow_api::models::{Geosets, Look};

use crate::dress::{Moving, dressed, moving};
use crate::loading::plan;
use crate::pose::Moment;
use crate::tests::square;

/// A track of `keys` (time, value) in the sequence 0.
fn keyed<T: Copy>(keys: &[(u32, T)]) -> Track<T> {
    Track {
        interpolation: Interpolation::Linear,
        global: None,
        keys: vec![Keys {
            times: keys.iter().map(|(time, _)| *time).collect(),
            values: keys.iter().map(|(_, value)| *value).collect(),
            tangents: Vec::new(),
        }],
    }
}

/// The square of `square`, its batch of the colour 0 (red at rest), the weight 0 (1) and the
/// transform 0, each track holding one value.
fn coloured() -> Model {
    let mut model = square(0, 0);
    model.colours = vec![[1.0, 0.0, 0.0, 1.0]];
    model.skins[0].batches[0].colour = Some(0);
    model.animation.colours = vec![(keyed(&[(0, [1.0, 0.0, 0.0])]), keyed(&[(0, 1.0)]))];
    model.animation.weights = vec![keyed(&[(0, 1.0)])];
    model
}

fn at(time: u32) -> Moment {
    Moment {
        sequence: 0,
        time,
        clock: 0,
    }
}

#[test]
fn a_material_moves_by_its_colour_its_weight_or_the_transforms_of_its_textures() {
    let still = coloured();
    let batch = still.skins[0].batches[0];
    assert_eq!(
        moving(&still, &batch, 1),
        None,
        "each track holding one value, no transform"
    );
    let mut weighted = coloured();
    weighted.animation.weights[0] = keyed(&[(0, 1.0), (500, 0.5)]);
    let all = Moving {
        colour: Some(0),
        weight: Some(0),
        transforms: [None; 2],
    };
    assert_eq!(moving(&weighted, &batch, 1), Some(all));
    let mut pulsing = coloured();
    pulsing.animation.colours[0].1 = keyed(&[(0, 1.0), (500, 0.0)]);
    assert_eq!(moving(&pulsing, &batch, 1), Some(all));
    // A transform, even holding one value; for the textures the batch reads only.
    let mut turning = coloured();
    turning.transform_combos = vec![0, 0];
    turning.animation.transforms = vec![TextureTransform::default()];
    let one = Moving {
        transforms: [Some(0), None],
        ..all
    };
    assert_eq!(moving(&turning, &batch, 1), Some(one));
    assert_eq!(
        moving(&turning, &batch, 2),
        Some(Moving {
            transforms: [Some(0), Some(0)],
            ..all
        })
    );
}

#[test]
fn a_slot_takes_the_colour_alpha_and_weight_of_its_moment_and_the_rows_of_its_transforms() {
    let mut model = coloured();
    model.animation.colours[0] = (
        keyed(&[(0, [1.0, 0.0, 0.0]), (1000, [0.0, 1.0, 0.0])]),
        keyed(&[(0, 1.0), (1000, 0.0)]),
    );
    model.animation.weights[0] = keyed(&[(0, 0.5)]);
    let mut shifting = TextureTransform {
        translation: keyed(&[(0, [0.25, 0.0, 0.0])]),
        ..TextureTransform::default()
    };
    let turning = TextureTransform {
        rotation: keyed(&[(0, Quat::from_rotation_z(90f32.to_radians()).to_array())]),
        ..TextureTransform::default()
    };
    model.animation.transforms = vec![shifting.clone(), turning];
    let moving = Moving {
        colour: Some(0),
        weight: Some(0),
        transforms: [Some(0), Some(1)],
    };
    let slot = dressed(&model, &moving, at(500));
    // Halfway from red to green, its alpha a half, times its weight.
    assert_eq!(slot[..4], [0.5, 0.5, 0.0, 0.25]);
    let moved =
        |rows: &[f32], u: f32, v: f32| [rows[0] * u + rows[1] * v + rows[2], rows[3] * u + rows[4] * v + rows[5]];
    assert_eq!(moved(&slot[4..10], 0.5, 0.5), [0.75, 0.5], "a quarter across");
    let turned = moved(&slot[10..], 1.0, 0.5);
    assert!(
        (turned[0] - 0.5).abs() < 1e-5 && (turned[1] - 1.0).abs() < 1e-5,
        "a quarter turn about the middle: {turned:?}"
    );
    // Scaled about the middle too.
    shifting.scale = keyed(&[(0, [2.0, 2.0, 1.0])]);
    shifting.translation = Track::default();
    model.animation.transforms[0] = shifting;
    let scaled = dressed(&model, &moving, at(0));
    assert_eq!(moved(&scaled[4..10], 0.75, 0.5), [1.0, 0.5]);
    // Without keys in its sequence, at rest; on a global sequence, by the clock.
    let elsewhere = Moment { sequence: 3, ..at(500) };
    assert_eq!(
        dressed(&model, &moving, elsewhere)[..4],
        [1.0, 0.0, 0.0, 1.0],
        "red, its alpha and weight at rest"
    );
    model.animation.colours[0].1.global = Some(0);
    model.animation.globals = vec![1000];
    let pulse = Moment {
        sequence: 3,
        time: 0,
        clock: 2250,
    };
    assert_eq!(
        dressed(&model, &moving, pulse)[3],
        0.75,
        "a quarter of its loop, its weight at rest"
    );
}

#[test]
fn the_batches_point_to_their_slot_and_one_unseen_at_rest_whose_alpha_moves_is_kept() {
    let mut model = coloured();
    model.animation.weights[0] = keyed(&[(0, 0.0), (500, 1.0)]);
    model.weights = vec![0.0];
    // A second level of the same batch, and a batch that does not move, of the weight 1.
    model.skins.push(model.skins[0].clone());
    let mut still = model.skins[0].batches[0];
    still.colour = None;
    still.weight_combo = 1;
    model.weight_combos = vec![0, 1];
    model.weights.push(1.0);
    model.animation.weights.push(keyed(&[(0, 1.0)]));
    model.skins[0].batches.push(still);
    let look = Look {
        model: uniwow_api::formats::FileRef::Path("square.m2".to_owned()),
        textures: Vec::new(),
        geosets: Geosets::All,
    };
    let (skins, slots) = plan(&model, &look);
    assert_eq!(slots.len(), 1, "one slot for the batch at both levels");
    let pointed: Vec<Vec<u32>> = skins
        .iter()
        .map(|batches| batches.iter().map(|batch| batch.params.combine[3]).collect())
        .collect();
    assert_eq!(pointed, [vec![1, 0], vec![1]], "unseen at rest, kept: its weight moves");
    // Its weight held at 0, it is left out.
    model.animation.weights[0] = keyed(&[(0, 0.0)]);
    let (skins, slots) = plan(&model, &look);
    assert!(slots.is_empty());
    assert_eq!(skins.iter().map(Vec::len).collect::<Vec<_>>(), [1, 0]);
}
