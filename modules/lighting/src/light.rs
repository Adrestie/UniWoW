//! The light of a map at a place and an hour, from the tables of the client: the values of a set of
//! params at the hour, their bands read between their keys; the global light of the map, then the
//! zones of light near the place, then each local light whose sphere holds the place, the farthest
//! first, mixed in by its weight, as Wow.exe 12340 mixes them (0x7F1360), but by the place on the
//! map whatever its height.

use std::collections::HashMap;

use uniwow_api::formats::{LightBand, LightParamsRecord, LightRecord, TILE, ZoneLightRecord};
use uniwow_api::viewport::{Fog, MapLight, Sun};

/// A day, in half-minutes.
pub const DAY: f32 = 2880.0;
/// The bands of colours and of numbers of a set of params.
pub const COLOURS: usize = 18;
pub const NUMBERS: usize = 6;
/// The middle of the world, 32 tiles from its corner, in yards, and the unit of the lights, 36ths of
/// a yard.
const MIDDLE: f32 = 32.0 * TILE;
const UNIT: f32 = 36.0;
/// Where the fog ends, in 36ths of a yard (about 180 yards), and where it starts, a share of its end,
/// when the global light gives neither (Noggit's defaults).
const FOG_END: f32 = 6_500.0;
const FOG_START: f32 = 0.1;
/// A zone of light weighs from nothing to whole across its edge, from 50 yards outside it to 50
/// inside, by the distance to its edge (0x77EED0, 0x7EE6B0); five at most are mixed in, the first in
/// the order of the client (0x7ED150).
const ZONE_MARGIN: f32 = 50.0;
const ZONE_FADE: f32 = 100.0;
const ZONES_MIXED: usize = 5;
/// A local light whose outer radius is less, in yards, is left out (0x7F1360).
const SMALLEST: f32 = 3.0;
/// Two local lights whose centres are no farther apart, in yards, share them: the one of the wider
/// inner radius is mixed in first (0x7ED0A0).
const SAME_CENTRE: f32 = 1.0 / 3.0;

/// The values of a light at an hour: its colours, red, green and blue from 0 to 1 as stored (in
/// gamma), and its numbers, each none where its params have no such band or no keys in it; the
/// alphas of the river and of the ocean, shallow then deep, and its glow.
#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub colours: [Option<[f32; 3]>; COLOURS],
    pub numbers: [Option<f32>; NUMBERS],
    pub river_alphas: [f32; 2],
    pub ocean_alphas: [f32; 2],
    pub glow: f32,
}

/// The light of a place: its values, each light mixed in with its weight, in their order, how many
/// of them, after the first, are those of zones, and whether the first is the light 1 for a map
/// without a global light of its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Mixed {
    pub values: Values,
    pub used: Vec<(u32, f32)>,
    pub zones: usize,
    pub fallback: bool,
}

/// Where `time`, in half-minutes, falls among the times of `keys`, in increasing order: the key
/// before it, the key after it and how far between; past the last key towards the first of the next
/// day, before the first from the last of the day before. None without keys.
fn between<T>(keys: &[(u32, T)], time: f32) -> Option<(usize, usize, f32)> {
    let count = keys.len();
    if count < 2 {
        return (count == 1).then_some((0, 0, 0.0));
    }
    let time = time.rem_euclid(DAY);
    let at = |key: usize| keys[key].0 as f32;
    let (from, to, start, end, now) = match keys.iter().rposition(|(key, _)| *key as f32 <= time) {
        Some(key) if key + 1 < count => (key, key + 1, at(key), at(key + 1), time),
        Some(key) => (key, 0, at(key), at(0) + DAY, time),
        None => (count - 1, 0, at(count - 1), at(0) + DAY, time + DAY),
    };
    let span = end - start;
    Some((
        from,
        to,
        if span > 0.0 {
            ((now - start) / span).clamp(0.0, 1.0)
        } else {
            0.0
        },
    ))
}

fn mix(a: f32, b: f32, share: f32) -> f32 {
    a + (b - a) * share
}

fn mix_colour(a: [f32; 3], b: [f32; 3], share: f32) -> [f32; 3] {
    std::array::from_fn(|channel| mix(a[channel], b[channel], share))
}

/// The colour of `band` at `time`, from 0 to 1.
pub fn colour_at(band: &LightBand<[u8; 3]>, time: f32) -> Option<[f32; 3]> {
    let (from, to, share) = between(&band.keys, time)?;
    let unit = |colour: [u8; 3]| colour.map(|channel| f32::from(channel) / 255.0);
    Some(mix_colour(unit(band.keys[from].1), unit(band.keys[to].1), share))
}

/// The number of `band` at `time`.
pub fn number_at(band: &LightBand<f32>, time: f32) -> Option<f32> {
    let (from, to, share) = between(&band.keys, time)?;
    Some(mix(band.keys[from].1, band.keys[to].1, share))
}

/// A zone of light: the light it gives and its outline in the world, of one point at least.
struct Zone {
    light: LightRecord,
    points: Vec<[f32; 2]>,
}

impl Zone {
    /// Its weight at `place`, by how deep within it the place lies, less than nothing outside it;
    /// none where it is 0.
    fn weight(&self, place: [f32; 2]) -> Option<f32> {
        let (inside, distance) = outline(&self.points, place);
        let depth = if inside { distance } else { -distance };
        let weight = ((ZONE_MARGIN + depth) / ZONE_FADE).min(1.0);
        (weight > 0.0).then_some(weight)
    }
}

/// Whether `place` lies within the outline `points`, by the edges west of it counted as the client
/// counts them (0x7F9C90), and its distance to the nearest edge.
fn outline(points: &[[f32; 2]], place: [f32; 2]) -> (bool, f32) {
    let [x, y] = place;
    let mut inside = false;
    let mut nearest = f32::MAX;
    let mut before = points[points.len() - 1];
    for &point in points {
        nearest = nearest.min(to_edge(place, point, before));
        if (before[0] <= x && x < point[0]) || (point[0] <= x && x < before[0]) {
            let crossed = point[1] + (x - point[0]) / (before[0] - point[0]) * (before[1] - point[1]);
            if crossed > y {
                inside = !inside;
            }
        }
        before = point;
    }
    (inside, nearest.sqrt())
}

/// The square of the distance from `place` to the edge from `a` to `b`.
fn to_edge(place: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let along = [b[0] - a[0], b[1] - a[1]];
    let from = [place[0] - a[0], place[1] - a[1]];
    let length = along[0] * along[0] + along[1] * along[1];
    let share = if length > 0.0 {
        ((from[0] * along[0] + from[1] * along[1]) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let off = [from[0] - share * along[0], from[1] - share * along[1]];
    off[0] * off[0] + off[1] * off[1]
}

/// The tables of the lights, by map and by id, and the zones of light by map, in their order.
#[derive(Default)]
pub struct Tables {
    lights: HashMap<u32, Vec<LightRecord>>,
    /// The light of a map without a global one of its own: the light 1.
    fallback: Option<LightRecord>,
    params: HashMap<u32, LightParamsRecord>,
    colours: HashMap<u32, LightBand<[u8; 3]>>,
    numbers: HashMap<u32, LightBand<f32>>,
    zones: HashMap<u32, Vec<Zone>>,
    /// The local lights by id: the least id of those sharing their centre, its own where none does,
    /// and that light's centre on the map.
    centres: HashMap<u32, (u32, [f32; 2])>,
    /// Every light, by id.
    by_id: HashMap<u32, LightRecord>,
}

/// Whether `light` is the global light of its map, at 0, 0, 0.
fn global(light: &LightRecord) -> bool {
    light.position == [0.0; 3]
}

/// The centre of `light` in the world, in yards: north, west and up.
fn centre(light: &LightRecord) -> [f32; 3] {
    let [x, y, z] = light.position;
    [MIDDLE - z / UNIT, MIDDLE - x / UNIT, y / UNIT]
}

/// The local lights of the maps by id: the least id of those sharing their centre, through those
/// that share one with them, and that light's centre on the map.
fn centres(lights: &HashMap<u32, Vec<LightRecord>>) -> HashMap<u32, (u32, [f32; 2])> {
    let mut centres = HashMap::new();
    for lights in lights.values() {
        let local: Vec<(u32, [f32; 3])> = lights
            .iter()
            .filter(|light| !global(light))
            .map(|light| (light.id, centre(light)))
            .collect();
        // Each light led to the least id it shares a centre with.
        let mut lead: Vec<usize> = (0..local.len()).collect();
        let find = |lead: &[usize], mut at: usize| {
            while lead[at] != at {
                at = lead[at];
            }
            at
        };
        for a in 0..local.len() {
            for b in a + 1..local.len() {
                let apart = (0..3)
                    .map(|axis| (local[a].1[axis] - local[b].1[axis]).powi(2))
                    .sum::<f32>();
                if apart.sqrt() <= SAME_CENTRE {
                    let (first, second) = (find(&lead, a), find(&lead, b));
                    let (least, other) = if local[first].0 <= local[second].0 {
                        (first, second)
                    } else {
                        (second, first)
                    };
                    lead[other] = least;
                }
            }
        }
        for (at, (id, _)) in local.iter().enumerate() {
            let (least, [x, y, _]) = local[find(&lead, at)];
            centres.insert(*id, (least, [x, y]));
        }
    }
    centres
}

impl Tables {
    pub fn new(
        lights: &[LightRecord],
        params: &[LightParamsRecord],
        colours: &[LightBand<[u8; 3]>],
        numbers: &[LightBand<f32>],
        zones: &[ZoneLightRecord],
    ) -> Self {
        let mut by_map: HashMap<u32, Vec<LightRecord>> = HashMap::new();
        for light in lights {
            by_map.entry(light.map).or_default().push(light.clone());
        }
        // The keys of a band by their times, as they are read.
        fn sorted<T: Clone>(band: &LightBand<T>) -> (u32, LightBand<T>) {
            let mut band = band.clone();
            band.keys.sort_by_key(|(time, _)| *time);
            (band.id, band)
        }
        // A zone whose light `Light.dbc` lacks left out, as the client leaves it, whatever its map.
        let mut by_zone: HashMap<u32, Vec<Zone>> = HashMap::new();
        for zone in zones.iter().filter(|zone| !zone.points.is_empty()) {
            let Some(light) = lights.iter().find(|light| light.id == zone.light) else {
                continue;
            };
            by_zone.entry(zone.map).or_default().push(Zone {
                light: light.clone(),
                points: zone.points.clone(),
            });
        }
        Self {
            centres: centres(&by_map),
            by_id: lights.iter().map(|light| (light.id, light.clone())).collect(),
            lights: by_map,
            fallback: lights.iter().find(|light| light.id == 1).cloned(),
            params: params.iter().map(|record| (record.id, record.clone())).collect(),
            colours: colours.iter().map(sorted).collect(),
            numbers: numbers.iter().map(sorted).collect(),
            zones: by_zone,
        }
    }

    /// The values of the params `params` at `time`, in half-minutes; none for params unknown.
    pub fn values(&self, params: u32, time: f32) -> Option<Values> {
        let record = self.params.get(&params).filter(|_| params > 0)?;
        // The id of the band `band` of `count` a set of params.
        let id = |count: usize, band: usize| {
            (params.checked_mul(count as u32)? - (count as u32 - 1)).checked_add(band as u32)
        };
        Some(Values {
            colours: std::array::from_fn(|band| {
                let band = self.colours.get(&id(COLOURS, band)?)?;
                colour_at(band, time)
            }),
            numbers: std::array::from_fn(|band| {
                let band = self.numbers.get(&id(NUMBERS, band)?)?;
                number_at(band, time)
            }),
            river_alphas: record.river_alphas,
            ocean_alphas: record.ocean_alphas,
            glow: record.glow,
        })
    }

    /// The light of the map `map` at the place `place` on it, in yards, at `time`, in half-minutes,
    /// for the params of the slot `slot` of each light (those of its first slot where it has none),
    /// its zones and local lights mixed in when `local`. None when the map has no global light and
    /// there is no light 1, or when its params are unknown.
    pub fn light_at(&self, map: u32, place: [f32; 2], time: f32, slot: usize, local: bool) -> Option<Mixed> {
        let lights = self.lights.get(&map).map(Vec::as_slice).unwrap_or_default();
        let own = lights.iter().find(|light| global(light));
        let base = own.or(self.fallback.as_ref())?;
        let mut values = self.values(params_of(base, slot), time)?;
        for (number, default) in [(0, FOG_END), (1, FOG_START)] {
            if values.numbers[number].is_none_or(|value| value == 0.0) {
                values.numbers[number] = Some(default);
            }
        }
        let mut used = vec![(base.id, 1.0)];
        let zones: Vec<(&LightRecord, f32)> = match local {
            true => self
                .zones
                .get(&map)
                .into_iter()
                .flatten()
                .filter_map(|zone| Some((&zone.light, zone.weight(place)?)))
                .take(ZONES_MIXED)
                .collect(),
            false => Vec::new(),
        };
        for (light, weight) in zones {
            if self.mix_in(&mut values, light, slot, time, weight) {
                used.push((light.id, weight));
            }
        }
        let zones = used.len() - 1;
        // The distance to the centre each light shares, that centre's least id, its inner radius.
        let mut held: Vec<(f32, u32, f32, &LightRecord, f32)> = Vec::new();
        for light in lights.iter().filter(|light| local && !global(light)) {
            let [x, y, _] = centre(light);
            let distance = ((place[0] - x).powi(2) + (place[1] - y).powi(2)).sqrt();
            let [inner, outer] = light.radii.map(|radius| radius / UNIT);
            if outer < SMALLEST {
                continue;
            }
            let weight = if distance <= inner {
                1.0
            } else if distance < outer {
                (outer - distance) / (outer - inner)
            } else {
                0.0
            };
            if weight > 0.0 {
                let (shared, at) = self.centres[&light.id];
                let apart = ((place[0] - at[0]).powi(2) + (place[1] - at[1]).powi(2)).sqrt();
                held.push((apart, shared, inner, light, weight));
            }
        }
        // The farthest first, so that the nearest weighs last; of a centre, the widest inner radius.
        held.sort_by(|a, b| {
            (b.0.total_cmp(&a.0))
                .then(a.1.cmp(&b.1))
                .then(b.2.total_cmp(&a.2))
                .then(a.3.id.cmp(&b.3.id))
        });
        for (_, _, _, light, weight) in held {
            if self.mix_in(&mut values, light, slot, time, weight) {
                used.push((light.id, weight));
            }
        }
        Some(Mixed {
            values,
            used,
            zones,
            fallback: own.is_none(),
        })
    }

    /// The fog of the game where `mixed` was taken, on the map `map`, for the slot `slot` (the
    /// client's for the slot 0, the eye out of the water) at `time`, as Wow.exe 12340 draws it with
    /// the far clip `far`: the fog of each light mixed in, prepared (`prepared_fog`) from its
    /// bands, one it lacks read as 0 as the client reads it, mixed by its weight in the order of
    /// `mixed` (0x7ED4C0); then its end within the far clip, its start the share of it (0x7F16F0).
    /// Where it starts and ends, in yards, and its rate; none without a light.
    pub fn fog_of_the_game(&self, mixed: &Mixed, map: u32, slot: usize, time: f32, far: f32) -> Option<[f32; 3]> {
        let mut lights = mixed.used.iter().filter_map(|(id, weight)| {
            let values = self.values(params_of(self.by_id.get(id)?, slot), time)?;
            let [end, share] = [
                values.numbers[0].unwrap_or(0.0) / UNIT,
                values.numbers[1].unwrap_or(0.0),
            ];
            Some((prepared_fog(end, share, map, far), *weight))
        });
        let (first, _) = lights.next()?;
        let [end, share, rate] = lights.fold(first, |kept, (given, weight)| {
            std::array::from_fn(|at| mix(kept[at], given[at], weight))
        });
        let end = end.min(far);
        Some([share * end, end, rate])
    }

    /// `values` with those of `light` for the slot `slot` at `time` mixed in by `weight`; false,
    /// unchanged, where its params are unknown.
    fn mix_in(&self, values: &mut Values, light: &LightRecord, slot: usize, time: f32, weight: f32) -> bool {
        let Some(other) = self.values(params_of(light, slot), time) else {
            return false;
        };
        for (kept, given) in values.colours.iter_mut().zip(other.colours) {
            if let Some(given) = given {
                *kept = Some(kept.map_or(given, |kept| mix_colour(kept, given, weight)));
            }
        }
        for (number, (kept, given)) in values.numbers.iter_mut().zip(other.numbers).enumerate() {
            // A light without fog gives none.
            let given = given.filter(|value| number > 1 || *value != 0.0);
            if let Some(given) = given {
                *kept = Some(kept.map_or(given, |kept| mix(kept, given, weight)));
            }
        }
        for (kept, given) in values
            .river_alphas
            .iter_mut()
            .chain(&mut values.ocean_alphas)
            .zip(other.river_alphas.iter().chain(&other.ocean_alphas))
        {
            *kept = mix(*kept, *given, weight);
        }
        values.glow = mix(values.glow, other.glow, weight);
        true
    }
}

/// The angle of the light of the sun from the top, in degrees, at 0, 6, 12 and 18 h (Noggit's table).
const SUN_ANGLES: [f32; 4] = [127.0, 110.0, 127.0, 110.0];

/// The direction towards the sun at `time`, in half-minutes, in the axes of the world (north, west,
/// up): Noggit's table read as it reads it for the models, between its hours, once a day; always to
/// the north-west, from 20° to 37° high.
pub fn sun_direction(time: f32) -> [f32; 3] {
    let quarter = time.rem_euclid(DAY) / (DAY / 4.0);
    let at = (quarter.floor() as usize).min(3);
    let share = quarter - at as f32;
    let angle = mix(SUN_ANGLES[at], SUN_ANGLES[(at + 1) % 4], share).to_radians();
    let across = angle.sin() * std::f32::consts::FRAC_1_SQRT_2;
    [across, across, -angle.cos()]
}

/// The linear value of a value in gamma, as an sRGB target encodes it back.
fn linear(gamma: f32) -> f32 {
    if gamma <= 0.04045 {
        gamma / 12.92
    } else {
        ((gamma + 0.055) / 1.055).powf(2.4)
    }
}

/// The light of `values` at `time` the view draws with: its sun, in the direction of the hour, its
/// diffuse and ambient light (the fixed light's where it has none); the colour of its fog, made
/// linear (the fixed one where it has none); and the fog of the game `game_fog`, where it starts
/// and ends and its rate, when it is drawn (`Tables::fog_of_the_game`).
pub fn map_light(values: &Values, time: f32, game_fog: Option<[f32; 3]>) -> MapLight {
    let fixed = Sun::default();
    MapLight {
        sun: Sun {
            direction: sun_direction(time),
            colour: values.colours[0].unwrap_or(fixed.colour),
            ambient: values.colours[1].unwrap_or(fixed.ambient),
        },
        fog_colour: values.colours[7].map_or(Fog::default().colour, |colour| colour.map(linear)),
        fog: game_fog,
    }
}

/// The first map whose fog is curved, Outland's, on hardware with shaders of the second version or
/// later, as any card of today (0x781739).
const CURVED_FROM: u32 = 530;
/// The shortest end of the fog of a light, and the shortest it is curved from, in yards
/// (0x7ECD80).
const SHORTEST_FOG: f32 = 10.0;
const SHORTEST_CURVED: f32 = 1_000.0 / 36.0;
/// The far clip a curved fog is measured against, at most, and what is taken from it, in yards
/// (0x7ECD00).
const FAR_FOR_RATE: f32 = 700.0;
const NEAR_FOR_RATE: f32 = 200.0;

/// The fog of a light of the map `map`, from the end `end` of its fog, in yards, and the share
/// `share` of it where it starts, as the client prepares it before the lights are mixed (0x7EBFF0,
/// 0x7ECD80): its end 10 yards at least, its share within −1 and 1, its rate 1; from Outland on,
/// of an end of 1000/36 yards or more, its rate by its own fog against the far clip `far`, then its
/// end the far clip; its share no less than 0 there. Its end, its share and its rate.
pub fn prepared_fog(end: f32, share: f32, map: u32, far: f32) -> [f32; 3] {
    let end = end.max(SHORTEST_FOG);
    let share = share.clamp(-1.0, 1.0);
    if map < CURVED_FROM {
        return [end, share, 1.0];
    }
    let [end, rate] = match end >= SHORTEST_CURVED {
        true => [far, curve(share * end, end, far)],
        false => [end, 1.0],
    };
    [end, share.max(0.0), rate]
}

/// How steep a fog from `start` to `end` is curved against the far clip `far` (0x7ECD00): 1.5 plus
/// 5.5 times what its span leaves of the far clip less 200 yards, 700 at most; 1.5 past it.
fn curve(start: f32, end: f32, far: f32) -> f32 {
    let span = far.min(FAR_FOR_RATE) - NEAR_FOR_RATE;
    if end - start <= span {
        1.5 + 5.5 * (1.0 - (end - start) / span)
    } else {
        1.5
    }
}

/// The params of the slot `slot` of `light`, or those of its first slot where it has none there (a
/// choice of the editor: Noggit gives no light then).
fn params_of(light: &LightRecord, slot: usize) -> u32 {
    match light.params.get(slot).copied().unwrap_or(0) {
        0 => light.params[0],
        params => params,
    }
}
