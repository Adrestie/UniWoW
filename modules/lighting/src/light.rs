//! The light of a map at a place and an hour, from the tables of the client: the values of a set of
//! params at the hour, their bands read between their keys; the global light of the map, then each
//! local light whose sphere holds the place, the farthest first, mixed in by its weight, as Noggit
//! mixes them, but by the place on the map whatever its height.

use std::collections::HashMap;

use uniwow_api::formats::{LightBand, LightParamsRecord, LightRecord, TILE};

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

/// The light of a place: its values, each light mixed in with its weight, in their order, and
/// whether the first is the light 1 for a map without a global light of its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Mixed {
    pub values: Values,
    pub used: Vec<(u32, f32)>,
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

/// The tables of the lights, by map and by id.
#[derive(Default)]
pub struct Tables {
    lights: HashMap<u32, Vec<LightRecord>>,
    /// The light of a map without a global one of its own: the light 1.
    fallback: Option<LightRecord>,
    params: HashMap<u32, LightParamsRecord>,
    colours: HashMap<u32, LightBand<[u8; 3]>>,
    numbers: HashMap<u32, LightBand<f32>>,
}

/// Whether `light` is the global light of its map, at 0, 0, 0.
fn global(light: &LightRecord) -> bool {
    light.position == [0.0; 3]
}

impl Tables {
    pub fn new(
        lights: &[LightRecord],
        params: &[LightParamsRecord],
        colours: &[LightBand<[u8; 3]>],
        numbers: &[LightBand<f32>],
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
        Self {
            lights: by_map,
            fallback: lights.iter().find(|light| light.id == 1).cloned(),
            params: params.iter().map(|record| (record.id, record.clone())).collect(),
            colours: colours.iter().map(sorted).collect(),
            numbers: numbers.iter().map(sorted).collect(),
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
    /// its local lights mixed in when `local`. None when the map has no global light and there is no
    /// light 1, or when its params are unknown.
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
        let mut held: Vec<(f32, &LightRecord, f32)> = Vec::new();
        for light in lights.iter().filter(|light| local && !global(light)) {
            let [x, _, z] = light.position;
            let centre = [MIDDLE - z / UNIT, MIDDLE - x / UNIT];
            let distance = ((place[0] - centre[0]).powi(2) + (place[1] - centre[1]).powi(2)).sqrt();
            let [inner, outer] = light.radii.map(|radius| radius / UNIT);
            let weight = if distance <= inner {
                1.0
            } else if distance < outer {
                (outer - distance) / (outer - inner)
            } else {
                0.0
            };
            if weight > 0.0 {
                held.push((distance, light, weight));
            }
        }
        // The farthest first, so that the nearest weighs last.
        held.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
        for (_, light, weight) in held {
            let Some(other) = self.values(params_of(light, slot), time) else {
                continue;
            };
            for (kept, given) in values.colours.iter_mut().zip(other.colours) {
                if let Some(given) = given {
                    *kept = Some(kept.map_or(given, |kept| mix_colour(kept, given, weight)));
                }
            }
            for (number, (kept, given)) in values.numbers.iter_mut().zip(other.numbers).enumerate() {
                // A local light without fog gives none.
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
            used.push((light.id, weight));
        }
        Some(Mixed {
            values,
            used,
            fallback: own.is_none(),
        })
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
