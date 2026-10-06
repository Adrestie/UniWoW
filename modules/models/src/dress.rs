//! The materials of a model that move (step 9.5c): the colour, the transparency and the transforms
//! of the coordinates of the textures of a batch, at a moment of its instance's sequence or of a
//! global one. Each combination that moves is a slot of its look, which its materials point to;
//! the thread of the animations writes the values of each slot of an instance before its bones.

use uniwow_api::formats::{Batch, Model, TextureTransform, Track};
use uniwow_api::glam::{Mat4, Quat, Vec3};

use crate::pose::{Moment, sample};

/// The floats of a slot: its colour, then the two rows of the transform of each texture.
pub const SLOT: usize = 16;

/// What the material of a batch takes from the animation of its model, by their index there: its
/// colour, its weight, and the transform of the coordinates of each of its two textures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Moving {
    pub colour: Option<u16>,
    pub weight: Option<u16>,
    pub transforms: [Option<u16>; 2],
}

/// Whether `track` takes more than one value.
fn varies<T: Copy + PartialEq>(track: &Track<T>) -> bool {
    let mut values = track.keys.iter().flat_map(|keys| &keys.values);
    values.next().is_some_and(|first| values.any(|value| value != first))
}

/// What the material of `batch` of `model`, reading `textures` textures, takes from its animation;
/// none when its colour and its weight hold one value and its textures have no transform.
pub fn moving(model: &Model, batch: &Batch, textures: usize) -> Option<Moving> {
    let animation = &model.animation;
    let colour = batch
        .colour
        .filter(|at| animation.colours.get(usize::from(*at)).is_some());
    let weight = model
        .weight_combos
        .get(usize::from(batch.weight_combo))
        .copied()
        .filter(|at| animation.weights.get(usize::from(*at)).is_some());
    let transforms = std::array::from_fn(|slot| {
        let at = *model
            .transform_combos
            .get(usize::from(batch.transform_combo) + slot)
            .filter(|_| slot < textures)?;
        animation.transforms.get(usize::from(at)).map(|_| at)
    });
    let colour_varies = colour.is_some_and(|at| {
        let (rgb, alpha) = &animation.colours[usize::from(at)];
        varies(rgb) || varies(alpha)
    });
    let weight_varies = weight.is_some_and(|at| varies(&animation.weights[usize::from(at)]));
    (colour_varies || weight_varies || transforms.iter().any(Option::is_some)).then_some(Moving {
        colour,
        weight,
        transforms,
    })
}

/// The two rows of the transform of coordinates `transform` at `moment`, of the global sequences
/// `globals`: translated, rotated and scaled about the middle of the texture, as WMV reads them.
fn rows(transform: &TextureTransform, moment: Moment, globals: &[u32]) -> [f32; 6] {
    let translation = sample(&transform.translation, moment, globals).map_or(Vec3::ZERO, Vec3::from);
    let rotation = sample(&transform.rotation, moment, globals)
        .map_or(Quat::IDENTITY, |[x, y, z, w]| Quat::from_xyzw(x, y, z, w).normalize());
    let scale = sample(&transform.scale, moment, globals).map_or(Vec3::ONE, Vec3::from);
    let middle = Vec3::splat(0.5);
    let matrix = Mat4::from_translation(translation)
        * Mat4::from_translation(middle)
        * Mat4::from_quat(rotation)
        * Mat4::from_scale(scale)
        * Mat4::from_translation(-middle);
    [
        matrix.x_axis.x,
        matrix.y_axis.x,
        matrix.w_axis.x,
        matrix.x_axis.y,
        matrix.y_axis.y,
        matrix.w_axis.y,
    ]
}

/// The slot of `moving` of `model` at `moment`: its colour, red, green, blue and alpha, the alpha
/// times the weight; then the rows of the transforms of its two textures, none the identity. A
/// track without keys in the sequence gives the value at rest.
pub fn dressed(model: &Model, moving: &Moving, moment: Moment) -> [f32; SLOT] {
    let animation = &model.animation;
    let globals = &animation.globals;
    let [r, g, b, a] = moving
        .colour
        .and_then(|at| model.colours.get(usize::from(at)))
        .copied()
        .unwrap_or([1.0; 4]);
    let (rgb, alpha) = moving.colour.map_or(([r, g, b], a), |at| {
        let (rgb, alpha) = &animation.colours[usize::from(at)];
        (
            sample(rgb, moment, globals).unwrap_or([r, g, b]),
            sample(alpha, moment, globals).unwrap_or(a),
        )
    });
    let weight = moving.weight.map_or(1.0, |at| {
        let rest = model.weights.get(usize::from(at)).copied().unwrap_or(1.0);
        sample(&animation.weights[usize::from(at)], moment, globals).unwrap_or(rest)
    });
    let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let [one, two] = moving.transforms.map(|at| {
        at.map_or(identity, |at| {
            rows(&animation.transforms[usize::from(at)], moment, globals)
        })
    });
    let mut slot = [0.0; SLOT];
    slot[..3].copy_from_slice(&rgb);
    slot[3] = (alpha * weight).clamp(0.0, 1.0);
    slot[4..10].copy_from_slice(&one);
    slot[10..].copy_from_slice(&two);
    slot
}
