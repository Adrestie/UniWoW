//! The sun, the moon and the stars of the sky, and their glare, by the hour: the tables of the
//! client's day and night (`DayNight`) as Wow.exe 12340 has them (`0x7eecc0` places the bodies from
//! its tables at `0xd39108` to `0xd391e0`, the stars at `0xaf4c20`, the glares from `0x7ee150` and
//! `0x7ee230`), the same as those of 1.12.1 the public implementation benilla reads but where said.
//! The visible sun is not the sun that lights (`light::sun_direction`): it rises and sets, that one
//! never does. Each body is drawn as a quad facing the eye, 12 yards from it, its side in yards
//! there; colour and alpha in gamma, as the client blends them.

/// A day, in half-minutes.
const DAY: f32 = 2880.0;

/// The visible sun's angle from the top, in radians, by the share of the day (`0xd391e0`): 10°
/// under the horizon from 5 h 30, 5° from the top from 11 h 55 to 12 h 05, under the horizon again
/// at 21 h 30; its bearing a constant 45° (`0xd391c8`), to the north-west.
const SUN_HEIGHT: [(f32, f32); 5] = [
    (0.229_166_7, 1.745_329_3),
    (0.496_527_8, 0.087_266_5),
    (0.5, 0.087_266_5),
    (0.503_472_2, 0.087_266_5),
    (0.895_833_3, 1.745_329_3),
];
/// The white moon's (`0xd391a0`): 35° from the top from 23 h 55 to 0 h 05, under the horizon from 4
/// h to 22 h; its bearing that of the sun (`0xd39188`).
const MOON_HEIGHT: [(f32, f32); 5] = [
    (0.0, 0.610_865_2),
    (0.003_472, 0.610_865_2),
    (0.166_667, 1.745_329_3),
    (0.916_667, 1.745_329_3),
    (0.996_528, 0.610_865_2),
];
/// The bearing of both, from the north towards the west.
const BEARING: f32 = std::f32::consts::FRAC_PI_4;

/// The side of the sun's quad (`0xd39128`): twice as large at 6 h and 21 h as from 6 h 45 to 20 h 15.
const SUN_SIZE: [(f32, f32); 4] = [(0.25, 2.0), (0.281_25, 1.0), (0.843_75, 1.0), (0.875, 2.0)];
/// The moon's (`0xd39108`), times its own size, 1.75: half as large again at 4 h and 22 h as at 1 h.
const MOON_SIZE: [(f32, f32); 4] = [(0.041_667, 1.0), (0.166_667, 1.5), (0.916_667, 1.5), (0.999_306, 1.0)];
const MOON_BASE: f32 = 1.75;

/// How much the stars are drawn (`0xaf4c20`): whole from 0 h to 3 h, fading out by 4 h 30, in from
/// 22 h 30.
const STARS: [(f32, f32); 4] = [(0.0, 1.0), (0.125, 1.0), (0.1875, 0.0), (0.9375, 0.0)];

/// The day envelope of the glare of the sun (`0x7ee150`): none until 6 h 30, whole from 7 h 30 to 19
/// h 30, none from 21 h.
const SUN_FLARE: [(f32, f32); 4] = [(0.270_833_3, 0.0), (0.3125, 1.0), (0.8125, 1.0), (0.875, 0.0)];
/// That of the moon (`0x7ee230`): whole until 2 h, none from 3 h 15 to 22 h 45, whole by midnight.
const MOON_FLARE: [(f32, f32); 4] = [
    (0.083_333_3, 1.0),
    (0.135_416_7, 0.0),
    (0.947_916_7, 0.0),
    (0.999_306, 1.0),
];

/// The sun glows on the clouds from 4 h 50 to 22 h 10, the moon otherwise (`0x7efae0`), whole at
/// every hour: the table of its envelope holds only ones in 12340 (`0xaf4be0`).
const SUN_GLOWS: [f32; 2] = [0.201_388_9, 0.923_611_1];

/// The glare of the sun: the side of its quad from 3 to 20 yards and its alpha from a half to 1 as
/// the view turns onto it; that of the moon twice the side of the moon, its alpha from a tenth to 1
/// (`0x7ef6e0`, `0x7ef389`).
pub const SUN_GLARE_SIDE: [f32; 2] = [3.0, 20.0];
pub const SUN_GLARE_ALPHA: [f32; 2] = [0.5, 1.0];
pub const MOON_GLARE_SIZE: f32 = 2.0;
pub const MOON_GLARE_ALPHA: [f32; 2] = [0.1, 1.0];
/// How fast the envelope of a glare rises, the sun's then the moon's, and falls, a second (`0x7ee150`,
/// `0x7ee230`).
pub const FLARE_RISE: [f32; 2] = [4.0, 100.0 / 33.0];
pub const FLARE_FALL: f32 = 50.0 / 33.0;

/// The share of the day of `time`, in half-minutes, from 0 to 1.
pub fn share_of_day(time: f32) -> f32 {
    (time.rem_euclid(DAY) / DAY).min(0.999_999_9)
}

/// The value of a table of the client's day at `fraction` of the day, as `DayNight::InterpTable`
/// reads it: from the first key whose time is not before it, in the order stored, back to the key
/// before, past the last towards the first of the next day, before the first from the last.
pub fn day_table(table: &[(f32, f32)], fraction: f32) -> f32 {
    let count = table.len();
    let mut after = 0;
    while after < count && fraction > table[after].0 {
        after += 1;
    }
    let (to, from) = match after {
        0 => (0, count - 1),
        at if at == count => (0, count - 1),
        at => (at, at - 1),
    };
    let mut span = table[to].0 - table[from].0;
    if span < 0.0 {
        span += 1.0;
    }
    let mut into = fraction - table[from].0;
    if into < 0.0 {
        into += 1.0;
    }
    let share = if span > 0.0 { into / span } else { 0.0 };
    table[from].1 + share * (table[to].1 - table[from].1)
}

/// Towards a body whose angle from the top is `polar`, on the bearing of the bodies.
fn towards(polar: f32) -> [f32; 3] {
    let across = polar.sin();
    [across * BEARING.cos(), across * BEARING.sin(), polar.cos()]
}

/// Towards the visible sun at `time`, in half-minutes, in the axes of the world (north, west, up).
pub fn sun_towards(time: f32) -> [f32; 3] {
    towards(day_table(&SUN_HEIGHT, share_of_day(time)))
}

/// Towards the moon at `time`.
pub fn moon_towards(time: f32) -> [f32; 3] {
    towards(day_table(&MOON_HEIGHT, share_of_day(time)))
}

/// The side of the sun's quad at `time`, in yards 12 yards from the eye.
pub fn sun_side(time: f32) -> f32 {
    day_table(&SUN_SIZE, share_of_day(time))
}

/// The moon's size curve at `time`, and the side of its quad.
pub fn moon_size(time: f32) -> f32 {
    day_table(&MOON_SIZE, share_of_day(time))
}

pub fn moon_side(time: f32) -> f32 {
    MOON_BASE * moon_size(time)
}

/// How much the stars are drawn at `time`: their curve made a byte as the client gives it to their
/// model, `trunc(254 × curve + 1)`, over 255, none under 2 (`0x6d1b50`).
pub fn stars(time: f32) -> f32 {
    let byte = (day_table(&STARS, share_of_day(time)) * 254.0 + 1.0).trunc();
    if byte < 2.0 { 0.0 } else { byte / 255.0 }
}

/// The day envelopes of the glare of the sun and of the moon at `time`.
pub fn flares(time: f32) -> [f32; 2] {
    let fraction = share_of_day(time);
    [day_table(&SUN_FLARE, fraction), day_table(&MOON_FLARE, fraction)]
}

/// Whether the body that glows on the clouds at `time` is the sun, the moon otherwise.
pub fn sun_glows(time: f32) -> bool {
    (SUN_GLOWS[0]..=SUN_GLOWS[1]).contains(&share_of_day(time))
}

/// The weight of the params of storm under a weather of `intensity`, from 0 to 1, whose row of
/// `Weather.dbc` gives `transition` (its column 3, 1 for a weather it does not name): four times
/// their product, whole from a quarter (`0x784a01`, `0x7f3978`).
pub fn storm_share(intensity: f32, transition: f32) -> f32 {
    (4.0 * intensity.max(0.0) * transition.max(0.0)).min(1.0)
}

/// The alpha of the bodies and of their glare under a storm of weight `storm`: a byte of what it
/// leaves, `255 (1 − storm)` rounded as an x87 store rounds, over 255; 1 without a storm
/// (`0x7f37d6`).
pub fn storm_alpha(storm: f32) -> f32 {
    if storm > 0.0 {
        (255.0 * (1.0 - storm.min(1.0))).round() / 255.0
    } else {
        1.0
    }
}

/// How much the view looks onto a body towards `towards`, from 0 to 1: nothing while it turns 45°
/// away from it, whole straight at it (`0x6cf490`, the cosine from 0.7 to 1).
pub fn view_onto(forward: [f32; 3], towards: [f32; 3]) -> f32 {
    let cosine: f32 = (0..3).map(|axis| forward[axis] * towards[axis]).sum();
    ((cosine - 0.7) / 0.3).clamp(0.0, 1.0)
}

/// A body as the layers draw it: towards it from the eye, the side of its quad in yards 12 yards
/// from the eye, its colour in gamma and its alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Body {
    pub towards: [f32; 3],
    pub side: f32,
    pub colour: [f32; 3],
    pub alpha: f32,
}

/// The sun and the moon at `time`, in the colour `colour` (the band 9 of the light, in gamma, as the
/// client colours both and their glare, `0x7f36f6` in 12340), under a storm of weight `storm`.
pub fn bodies(time: f32, colour: [f32; 3], storm: f32) -> [Body; 2] {
    let alpha = storm_alpha(storm);
    [
        Body {
            towards: sun_towards(time),
            side: sun_side(time),
            colour,
            alpha,
        },
        Body {
            towards: moon_towards(time),
            side: moon_side(time),
            colour,
            alpha,
        },
    ]
}

/// How much of the sky the sun, the moon, the stars and the clouds are drawn over, under the models
/// of the sky `skies` (each its alpha and whether it lets them be drawn, the flag 0x2 of
/// `LightSkybox.dbc`): all of it or none, none under one of them past 0.99 that hides them, as 12340
/// leaves them out (`0x7f0abe`), the models drawn over them by their alpha otherwise; none under a
/// building's own sky, nor in a liquid, where the client draws no sky.
pub fn share(skies: &[(f32, bool)], building: bool, immersed: bool) -> f32 {
    let hidden = skies.iter().any(|(alpha, combined)| !combined && *alpha > 0.99);
    if building || immersed || hidden { 0.0 } else { 1.0 }
}

/// How much the glares are drawn under the models of the sky `skies` (each its alpha and its flag
/// 0x2, which the glares do not heed) and a building's own sky, `depth` yards under a liquid: what
/// the strongest of them leaves, none under the building's, then less by a tenth a yard under the
/// surface, none from 10 yards (`0x7ef6e0`, `0x7eda30`).
pub fn glare_share(skies: &[(f32, bool)], building: bool, depth: f32) -> f32 {
    let strongest = if building {
        1.0
    } else {
        skies.iter().map(|(alpha, _)| *alpha).fold(0.0, f32::max)
    };
    (1.0 - strongest.min(1.0)) * (1.0 - (0.1 * depth).clamp(0.0, 1.0))
}
