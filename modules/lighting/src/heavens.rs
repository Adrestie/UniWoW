//! The sun, the moon and the stars of the sky, and their glare, by the hour: the tables of the
//! client's day and night (`DayNight`), as the public implementation benilla reads them from the
//! client 1.12.1 (`0x6d3b80` places the bodies, its tables at `0xce8c8c` to `0xce9a98`), read for
//! their facts only; those of 3.3.5a are not read here (no client), but three places of its sun that
//! coa-vfog logged in a client 12340 fall on the table of 1.12.1 within a fifth of a degree. The
//! visible sun is not the sun that lights (`light::sun_direction`): it rises and sets, that one
//! never does. Each body is drawn as a quad facing the eye, 12 yards from it, its side in yards
//! there; colour and alpha in gamma, as the client blends them.

/// A day, in half-minutes.
const DAY: f32 = 2880.0;

/// The visible sun's angle from the top, in radians, by the share of the day (`0xce8d64`): 10°
/// under the horizon from 5 h 30, 5° from the top from 11 h 55 to 12 h 05, under the horizon again
/// at 21 h 30; its bearing a constant 45° (`0xce8d4c`), to the north-west.
const SUN_HEIGHT: [(f32, f32); 5] = [
    (0.229_166_7, 1.745_329_3),
    (0.496_527_8, 0.087_266_5),
    (0.5, 0.087_266_5),
    (0.503_472_2, 0.087_266_5),
    (0.895_833_3, 1.745_329_3),
];
/// The white moon's (`0xce8d24`): 35° from the top from 23 h 55 to 0 h 05, under the horizon from 4
/// h to 22 h; its bearing that of the sun (`0xce8d0c`).
const MOON_HEIGHT: [(f32, f32); 5] = [
    (0.0, 0.610_865_2),
    (0.003_472, 0.610_865_2),
    (0.166_667, 1.745_329_3),
    (0.916_667, 1.745_329_3),
    (0.996_528, 0.610_865_2),
];
/// The bearing of both, from the north towards the west.
const BEARING: f32 = std::f32::consts::FRAC_PI_4;

/// The side of the sun's quad (`0xce8cac`): twice as large at 6 h and 21 h as from 6 h 45 to 20 h 15.
const SUN_SIZE: [(f32, f32); 4] = [(0.25, 2.0), (0.281_25, 1.0), (0.843_75, 1.0), (0.875, 2.0)];
/// The moon's (`0xce8c8c`), times its own size, 1.75: half as large again at 4 h and 22 h as at 1 h.
const MOON_SIZE: [(f32, f32); 4] = [(0.041_667, 1.0), (0.166_667, 1.5), (0.916_667, 1.5), (0.999_306, 1.0)];
const MOON_BASE: f32 = 1.75;

/// How much the stars are drawn (`0xce9a98`): whole from 0 h to 3 h, fading out by 4 h 30, in from
/// 22 h 30.
const STARS: [(f32, f32); 4] = [(0.0, 1.0), (0.125, 1.0), (0.1875, 0.0), (0.9375, 0.0)];

/// The day envelope of the glare of the sun (`0xce9818`): none until 6 h 30, whole from 7 h 30 to 19
/// h 30, none from 21 h.
const SUN_FLARE: [(f32, f32); 4] = [(0.270_833_3, 0.0), (0.3125, 1.0), (0.8125, 1.0), (0.875, 0.0)];
/// That of the moon (`0xce9768`): whole until 2 h, none from 3 h 15 to 22 h 45, whole by midnight.
const MOON_FLARE: [(f32, f32); 4] = [
    (0.083_333_3, 1.0),
    (0.135_416_7, 0.0),
    (0.947_916_7, 0.0),
    (0.999_306, 1.0),
];

/// The envelope of the glow of the body on the clouds (`0xce9ab8`), its keys in the order the client
/// stores them: the two last fall before the fifth and are never reached, the client's scan wrapping
/// back to 1 past 22 h 10.
const CLOUD_GLOW: [(f32, f32); 8] = [
    (0.166_67, 1.0),
    (0.194_44, 0.0),
    (0.201_39, 0.0),
    (0.229_17, 1.0),
    (0.895_83, 1.0),
    (0.923_61, 0.0),
    (0.888_89, 0.0),
    (0.916_67, 1.0),
];
/// The sun glows on the clouds from 4 h 50 to 22 h 10, the moon otherwise (`0x6cfb00`).
const SUN_GLOWS: [f32; 2] = [0.201_388_9, 0.923_611_1];

/// The glare of the sun: the side of its quad from 3 to 20 yards and its alpha from a half to 1 as
/// the view turns onto it; that of the moon twice the moon's size curve, its alpha from a tenth to
/// 1 (`0x6cf490`).
pub const SUN_GLARE_SIDE: [f32; 2] = [3.0, 20.0];
pub const SUN_GLARE_ALPHA: [f32; 2] = [0.5, 1.0];
pub const MOON_GLARE_SIZE: f32 = 2.0;
pub const MOON_GLARE_ALPHA: [f32; 2] = [0.1, 1.0];
/// How fast the envelope of a glare rises, the sun's then the moon's, and falls, a second
/// (`[0xce97d0]`, `[0xce9720]`, `[0xce97d4]`).
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

/// The envelope of the glow of the body on the clouds at `time`, and whether that body is the sun.
pub fn cloud_glow(time: f32) -> (f32, bool) {
    let fraction = share_of_day(time);
    (
        day_table(&CLOUD_GLOW, fraction),
        (SUN_GLOWS[0]..=SUN_GLOWS[1]).contains(&fraction),
    )
}

/// The weight of the params of storm in a weather that brings a storm of `intensity`, from 0 to 1:
/// four times it, whole from a quarter (`0x6d4500`, 1.12.1).
pub fn storm_share(intensity: f32) -> f32 {
    (4.0 * intensity.max(0.0)).min(1.0)
}

/// The alpha of the bodies and of their glare under a storm of weight `storm`: a byte of what it
/// leaves, `floor(255 (1 − storm))`, over 255; 1 without a storm (`0x6d2c74`).
pub fn storm_alpha(storm: f32) -> f32 {
    if storm > 0.0 {
        (255.0 * (1.0 - storm.min(1.0))).floor() / 255.0
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

/// How much of the sky the sun, the moon, the stars and the clouds are drawn over, from 0 to 1,
/// under the models of the sky `skies` (each its alpha and whether it lets them be drawn, the flag
/// 0x2 of `LightSkybox.dbc`): what the strongest of those that hide them leaves, nothing past 0.99,
/// as the client stops drawing them under a model of the sky so strong (`0x6d49fb`, 1.12.1); nothing
/// under a building's own sky, nor in a liquid, where the client draws no sky.
pub fn share(skies: &[(f32, bool)], building: bool, immersed: bool) -> f32 {
    if building || immersed {
        return 0.0;
    }
    let strongest = skies
        .iter()
        .filter(|(_, combined)| !combined)
        .map(|(alpha, _)| *alpha)
        .fold(0.0, f32::max);
    if strongest > 0.99 { 0.0 } else { 1.0 - strongest }
}
