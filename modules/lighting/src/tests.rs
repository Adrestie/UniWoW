//! Tests of the light of a place and an hour on tables the tests write; those of the client are
//! read by the tests of `assets`.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::formats::{
    AnimationRecord, AreaRecord, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair, FileRef,
    Formats, GameObjectDisplay, HairGeoset, LightBand, LightParamsRecord, LightRecord, LiquidTypeRecord, MapRecord,
    Model, Texture, Tile, Wdl, Wdt, Wmo, ZoneLightRecord,
};
use uniwow_api::liquids::{Liquids, Surfaces};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::vfs::{Vfs, VfsState};
use uniwow_api::viewport::{self, MapLight, Sun};
use uniwow_api::{
    CallId, CommandInfo, Context, Editor, EditorBackend, Event, JobContext, JobFn, JobId, JobOutcome, Module,
    PropertyValue, Registrar, egui, egui_wgpu,
};

use crate::light::{Immersion, Tables, colour_at, map_light, number_at, prepared_fog, sun_direction};

/// A band of numbers of `keys`.
fn numbers(id: u32, keys: &[(u32, f32)]) -> LightBand<f32> {
    LightBand {
        id,
        keys: keys.to_vec(),
    }
}

/// A band of colours of `keys`, each a grey.
fn greys(id: u32, keys: &[(u32, u8)]) -> LightBand<[u8; 3]> {
    LightBand {
        id,
        keys: keys.iter().map(|(time, grey)| (*time, [*grey; 3])).collect(),
    }
}

#[test]
fn a_band_is_read_between_its_keys_and_past_midnight() {
    let band = numbers(1, &[(0, 0.0), (1440, 100.0)]);
    assert_eq!(number_at(&band, 720.0), Some(50.0));
    // Past the last key, towards the first of the next day.
    assert_eq!(number_at(&band, 2160.0), Some(50.0));
    assert_eq!(number_at(&band, 2880.0 + 720.0), Some(50.0), "the next day");
    // Before the first key, from the last of the day before.
    let band = numbers(1, &[(720, 10.0), (2160, 30.0)]);
    assert_eq!(number_at(&band, 0.0), Some(20.0));
    assert_eq!(number_at(&band, 720.0), Some(10.0));
    assert_eq!(number_at(&numbers(1, &[(600, 7.0)]), 100.0), Some(7.0), "a single key");
    assert_eq!(number_at(&numbers(1, &[]), 100.0), None, "no key");
    let colour = colour_at(&greys(1, &[(0, 0), (1440, 255)]), 1080.0).unwrap();
    assert!((colour[0] - 0.75).abs() < 1e-6, "{colour:?}");
}

/// A light of the map 0 at `place` in the world, of radii `radii` in yards, its first params
/// `params`, in the units of the file.
fn light(id: u32, place: [f32; 2], radii: [f32; 2], params: [u32; 2]) -> LightRecord {
    let middle = 17_066.666;
    LightRecord {
        id,
        map: 0,
        position: if place == [0.0; 2] {
            [0.0; 3]
        } else {
            [(middle - place[1]) * 36.0, 50.0 * 36.0, (middle - place[0]) * 36.0]
        },
        radii: radii.map(|radius| radius * 36.0),
        params: [params[0], params[1], 0, 0, 0, 0, 0, 0],
    }
}

fn params(id: u32, river: f32) -> LightParamsRecord {
    LightParamsRecord {
        id,
        highlight_sky: false,
        skybox: 0,
        cloud: 0,
        glow: river,
        river_alphas: [river, 1.0],
        ocean_alphas: [0.75, 1.0],
    }
}

/// The tables of `tables`: their lights, params, colours and numbers.
type Parts = (
    Vec<LightRecord>,
    Vec<LightParamsRecord>,
    Vec<LightBand<[u8; 3]>>,
    Vec<LightBand<f32>>,
);

/// Tables of a global light (params 1: diffuse 100, ambient 40, the top of the sky white, fog from
/// 0.25 of 18,000) and of local lights: 2 at 1,000, 0, whole within 100 yards, fading to 300, of
/// params 2 (diffuse 200, no ambient band, no fog), its params under the water 4 (diffuse 60, fog
/// to 100 yards); 3 at 1,100, 0, within 50 to 150 yards, of params 3 (diffuse 0).
fn parts() -> Parts {
    let lights = vec![
        light(1, [0.0; 2], [0.0; 2], [1, 0]),
        light(2, [1_000.0, 0.0], [100.0, 300.0], [2, 4]),
        light(3, [1_100.0, 0.0], [50.0, 150.0], [3, 0]),
    ];
    let records = vec![params(1, 0.1), params(2, 0.5), params(3, 0.9), params(4, 0.3)];
    // The bands of the params `p`: colours from 18 p − 17, numbers from 6 p − 5.
    let colours = vec![
        greys(1, &[(0, 100)]),
        greys(2, &[(0, 40)]),
        greys(3, &[(0, 255)]),
        greys(19, &[(0, 200)]),
        greys(37, &[(0, 0)]),
        greys(55, &[(0, 60)]),
    ];
    let fog = vec![
        numbers(1, &[(0, 18_000.0)]),
        numbers(2, &[(0, 0.25)]),
        numbers(7, &[(0, 0.0)]),
        numbers(8, &[(0, 0.0)]),
        numbers(19, &[(0, 3_600.0)]),
        numbers(3, &[(0, 0.0), (1440, 10.0)]),
    ];
    (lights, records, colours, fog)
}

fn tables() -> Tables {
    let (lights, records, colours, fog) = parts();
    Tables::new(&lights, &records, &colours, &fog, &[])
}

/// A type of liquid `id`, the params of its own light `light`, 0 for none.
fn liquid(id: u32, light: u32) -> LiquidTypeRecord {
    LiquidTypeRecord {
        id,
        name: String::new(),
        kind: 1,
        material: 1,
        vertex_format: Some(0),
        textures: Default::default(),
        animation: [0.0; 2],
        depth_table: 0,
        depth_scale: 1.0,
        light,
    }
}

/// The types of liquid of the tests: a water, 5, and a magma, 3, lit by the params 4.
fn liquids() -> Vec<LiquidTypeRecord> {
    vec![liquid(5, 0), liquid(3, 4)]
}

fn diffuse(mixed: &crate::light::Mixed) -> f32 {
    mixed.values.colours[0].unwrap()[0] * 255.0
}

#[test]
fn the_global_light_is_mixed_with_the_local_ones_holding_the_place_the_farthest_first() {
    let tables = tables();
    // Far from the local lights: the global alone.
    let far = tables.light_at(0, [-5_000.0, 0.0], 0.0, 0, true).unwrap();
    assert_eq!(far.used, [(1, 1.0)]);
    assert!((diffuse(&far) - 100.0).abs() < 1e-3);
    assert!((far.values.colours[1].unwrap()[0] * 255.0 - 40.0).abs() < 1e-3);
    assert_eq!(far.values.numbers[0..2], [Some(18_000.0), Some(0.25)]);
    // Within the inner radius of the light 2, its height ignored: its diffuse, the global's ambient
    // and fog, as it has none.
    let within = tables.light_at(0, [920.0, -50.0], 0.0, 0, true).unwrap();
    assert_eq!(within.used, [(1, 1.0), (2, 1.0)]);
    assert!((diffuse(&within) - 200.0).abs() < 1e-3);
    assert!((within.values.colours[1].unwrap()[0] * 255.0 - 40.0).abs() < 1e-3);
    assert_eq!(within.values.numbers[0..2], [Some(18_000.0), Some(0.25)]);
    assert_eq!(within.values.river_alphas[0], 0.5);
    // Halfway between its radii: half of it.
    let between = tables.light_at(0, [800.0, 0.0], 0.0, 0, true).unwrap();
    assert_eq!(between.used.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1, 2]);
    assert!((between.used[1].1 - 0.5).abs() < 1e-4, "{:?}", between.used);
    assert!((diffuse(&between) - 150.0).abs() < 1e-3);
    // Within both 2 and 3: 2, the farthest, then 3, which weighs last.
    let both = tables.light_at(0, [1_090.0, 0.0], 0.0, 0, true).unwrap();
    assert_eq!(both.used.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(diffuse(&both).abs() < 1e-3, "the nearest last");
    // The local lights not mixed in.
    let alone = tables.light_at(0, [1_000.0, 0.0], 0.0, 0, false).unwrap();
    assert_eq!(alone.used, [(1, 1.0)]);
}

#[test]
fn the_params_of_a_slot_are_taken_and_those_of_the_first_where_a_light_has_none() {
    let tables = tables();
    // Under the water: the light 2 gives its params 4, the global its first, having none there.
    let under = tables.light_at(0, [920.0, -50.0], 0.0, 1, true).unwrap();
    assert!((diffuse(&under) - 60.0).abs() < 1e-3);
    assert_eq!(under.values.river_alphas[0], 0.3);
    // A local light's fog given where it has one.
    assert_eq!(under.values.numbers[0], Some(3_600.0));
    // A map without a global light of its own: the light 1.
    assert_eq!(tables.light_at(9, [0.0; 2], 0.0, 0, true).unwrap().used, [(1, 1.0)]);
    // Params unknown: none.
    assert!(
        Tables::new(&[light(1, [0.0; 2], [0.0; 2], [5, 0])], &[], &[], &[], &[])
            .light_at(0, [0.0; 2], 0.0, 0, true)
            .is_none()
    );
}

#[test]
fn the_fog_of_a_global_light_that_gives_none_is_noggit_s() {
    let lights = [light(1, [0.0; 2], [0.0; 2], [1, 0])];
    let tables = Tables::new(&lights, &[params(1, 0.1)], &[], &[], &[]);
    let light = tables.light_at(0, [0.0; 2], 0.0, 0, true).unwrap();
    assert_eq!(light.values.numbers[0..2], [Some(6_500.0), Some(0.1)]);
    assert_eq!(light.values.colours[0], None, "a band absent");
    // Given, but 0.
    let zero = [numbers(1, &[(0, 0.0)]), numbers(2, &[(0, 0.0)])];
    let tables = Tables::new(&lights, &[params(1, 0.1)], &[], &zero, &[]);
    let light = tables.light_at(0, [0.0; 2], 0.0, 0, true).unwrap();
    assert_eq!(light.values.numbers[0..2], [Some(6_500.0), Some(0.1)]);
}

#[test]
fn a_band_is_read_by_its_keys_in_the_order_of_their_times() {
    let tables = Tables::new(
        &[light(1, [0.0; 2], [0.0; 2], [1, 0])],
        &[params(1, 0.1)],
        &[],
        &[numbers(3, &[(1440, 4.0), (0, 2.0)])],
        &[],
    );
    let light = tables.light_at(0, [0.0; 2], 720.0, 0, true).unwrap();
    assert_eq!(light.values.numbers[2], Some(3.0));
    // An hour before the day or past it, as within it.
    let band = numbers(1, &[(0, 0.0), (1440, 100.0)]);
    assert_eq!(number_at(&band, -720.0), number_at(&band, 2160.0));
    assert_eq!(number_at(&band, 2880.0 * 3.0 + 720.0), Some(50.0));
}

#[test]
fn a_light_of_equal_radii_holds_the_place_within_them_wholly() {
    let lights = [
        light(1, [0.0; 2], [0.0; 2], [1, 0]),
        light(2, [100.0, 0.0], [50.0, 50.0], [2, 0]),
    ];
    let tables = Tables::new(&lights, &[params(1, 0.1), params(2, 0.5)], &[], &[], &[]);
    assert_eq!(
        tables.light_at(0, [140.0, 0.0], 0.0, 0, true).unwrap().used,
        [(1, 1.0), (2, 1.0)]
    );
    assert_eq!(tables.light_at(0, [160.0, 0.0], 0.0, 0, true).unwrap().used, [(1, 1.0)]);
    // Exactly on them: held, as by Noggit. A light over the middle of the world, its centre exact.
    let mut over = light(3, [0.0; 2], [64.0, 64.0], [2, 0]);
    over.position = [0.0, 36.0, 0.0];
    let middle = 32.0 * uniwow_api::formats::TILE;
    let tables = Tables::new(
        &[lights[0].clone(), over],
        &[params(1, 0.1), params(2, 0.5)],
        &[],
        &[],
        &[],
    );
    assert_eq!(
        tables.light_at(0, [middle + 64.0, middle], 0.0, 0, true).unwrap().used,
        [(1, 1.0), (3, 1.0)]
    );
    // A global light of its own, not the light 1 of another map.
    assert!(!tables.light_at(0, [0.0; 2], 0.0, 0, true).unwrap().fallback);
    assert!(tables.light_at(9, [0.0; 2], 0.0, 0, true).unwrap().fallback);
}

/// A zone of light of the map 0 within `points`, giving the light `light`.
fn zone(light: u32, points: &[[f32; 2]]) -> ZoneLightRecord {
    ZoneLightRecord {
        map: 0,
        light,
        points: points.to_vec(),
    }
}

/// The tables of a global light (diffuse 100), the light 2 of `tables` (diffuse 200), the lights
/// 5 and 6 of the zones `zones`, far from the places of the tests and a few yards wide, of params 5
/// (diffuse 0) and 6 (diffuse 250).
fn zoned(zones: &[ZoneLightRecord]) -> Tables {
    let lights = [
        light(1, [0.0; 2], [0.0; 2], [1, 0]),
        light(2, [1_000.0, 0.0], [100.0, 300.0], [2, 4]),
        light(5, [9_000.0, 9_000.0], [1.0, 4.0], [5, 0]),
        light(6, [9_000.0, 9_100.0], [1.0, 4.0], [6, 0]),
    ];
    let records = [params(1, 0.1), params(2, 0.5), params(5, 0.2), params(6, 0.4)];
    let colours = [
        greys(1, &[(0, 100)]),
        greys(19, &[(0, 200)]),
        greys(73, &[(0, 0)]),
        greys(91, &[(0, 250)]),
    ];
    Tables::new(&lights, &records, &colours, &[], zones)
}

/// The ids of the lights mixed in, in their order.
fn ids(mixed: &crate::light::Mixed) -> Vec<u32> {
    mixed.used.iter().map(|(id, _)| *id).collect()
}

#[test]
fn a_zone_of_light_weighs_from_50_yards_outside_its_edge_to_50_within() {
    let square = [
        [-3_000.0, -1_000.0],
        [-3_000.0, 1_000.0],
        [-1_000.0, 1_000.0],
        [-1_000.0, -1_000.0],
    ];
    let tables = zoned(&[zone(5, &square)]);
    let at = |place: [f32; 2]| tables.light_at(0, place, 0.0, 0, true).unwrap();
    // Within it by 50 yards or more: whole, its diffuse 0 over the global's 100.
    for place in [[-2_000.0, 0.0], [-1_050.0, 0.0], [-2_000.0, 950.0]] {
        let within = at(place);
        assert_eq!(
            (within.used.clone(), within.zones),
            (vec![(1, 1.0), (5, 1.0)], 1),
            "{place:?}"
        );
        assert!(diffuse(&within).abs() < 1e-3);
    }
    // Across its edge, by the distance to it: three quarters 25 yards within, half on it, a quarter
    // 25 yards outside, also from a corner; nothing 50 yards outside.
    for (place, weight) in [
        ([-1_025.0, 0.0], 0.75),
        ([-1_000.0, 0.0], 0.5),
        ([-975.0, 0.0], 0.25),
        ([-980.0, 1_015.0], 0.25),
        ([-3_020.0, -1_015.0], 0.25),
    ] {
        let near = at(place);
        assert_eq!(ids(&near), [1, 5], "{place:?}");
        assert!((near.used[1].1 - weight).abs() < 1e-4, "{place:?}: {:?}", near.used);
        assert!((diffuse(&near) - 100.0 * (1.0 - weight)).abs() < 1e-2, "{place:?}");
    }
    for place in [[-950.0, 0.0], [-970.0, 1_040.0], [-2_000.0, 1_060.0]] {
        let away = at(place);
        assert_eq!((ids(&away), away.zones), (vec![1], 0), "{place:?}");
    }
    // Switched off with the local lights; of its own map only.
    assert_eq!(
        tables.light_at(0, [-2_000.0, 0.0], 0.0, 0, false).unwrap().used,
        [(1, 1.0)]
    );
    let other = ZoneLightRecord {
        map: 1,
        ..zone(5, &square)
    };
    assert_eq!(
        ids(&zoned(&[other]).light_at(0, [-2_000.0, 0.0], 0.0, 0, true).unwrap()),
        [1]
    );
}

#[test]
fn a_zone_of_light_holds_what_its_outline_holds_however_it_turns() {
    // An L: its notch, from 100 to 300 on both axes, outside it.
    let shape = [
        [0.0, 0.0],
        [0.0, 300.0],
        [100.0, 300.0],
        [100.0, 100.0],
        [300.0, 100.0],
        [300.0, 0.0],
    ];
    let mut reversed = shape;
    reversed.reverse();
    // Its points in either order.
    for shape in [shape, reversed] {
        let tables = zoned(&[zone(5, &shape)]);
        for (place, weight) in [
            ([50.0, 250.0], Some(1.0)),
            ([250.0, 50.0], Some(1.0)),
            ([200.0, 80.0], Some(0.7)),
            ([80.0, 200.0], Some(0.7)),
            ([200.0, 120.0], Some(0.3)),
            ([120.0, 200.0], Some(0.3)),
            ([250.0, 250.0], None),
        ] {
            let mixed = tables.light_at(0, place, 0.0, 0, true).unwrap();
            let found = mixed.used.get(1).map(|(_, weight)| *weight);
            let same = match (found, weight) {
                (Some(found), Some(weight)) => (found - weight).abs() < 1e-4,
                (found, weight) => found.is_none() && weight.is_none(),
            };
            assert!(same, "{shape:?} {place:?}: {:?}", mixed.used);
        }
    }
}

#[test]
fn zones_are_mixed_after_the_global_light_and_before_the_local_ones_five_at_most() {
    // Around the light 2: the zone first, the local light last, its diffuse 200 kept.
    let around = [[700.0, -300.0], [700.0, 300.0], [1_300.0, 300.0], [1_300.0, -300.0]];
    let tables = zoned(&[zone(6, &around)]);
    let both = tables.light_at(0, [1_000.0, 0.0], 0.0, 0, true).unwrap();
    assert_eq!((both.used.clone(), both.zones), (vec![(1, 1.0), (6, 1.0), (2, 1.0)], 1));
    assert!((diffuse(&both) - 200.0).abs() < 1e-3);
    // Seven zones holding the place: the first five, in their order, those far from it not
    // counted; a zone whose light the tables lack, or without outline, left out before them.
    let square = [
        [-3_000.0, -1_000.0],
        [-3_000.0, 1_000.0],
        [-1_000.0, 1_000.0],
        [-1_000.0, -1_000.0],
    ];
    let far = square.map(|[x, y]| [x, y + 5_000.0]);
    let mut zones = vec![zone(99, &square), zone(5, &[])];
    zones.extend([6, 6, 6, 6, 6].map(|light| zone(light, &far)));
    zones.extend([5, 6, 5, 6, 6, 5, 5].map(|light| zone(light, &square)));
    let many = zoned(&zones).light_at(0, [-2_000.0, 0.0], 0.0, 0, true).unwrap();
    assert_eq!((ids(&many), many.zones), (vec![1, 5, 6, 5, 6, 6], 5));
    assert!((diffuse(&many) - 250.0).abs() < 1e-3, "the last mixed weighs last");
}

#[test]
fn a_local_light_whose_outer_radius_is_under_3_yards_is_left_out() {
    let lights = [
        light(1, [0.0; 2], [0.0; 2], [1, 0]),
        light(2, [500.0, 0.0], [1.0, 2.99], [2, 0]),
        light(3, [600.0, 0.0], [1.0, 3.0], [2, 0]),
    ];
    let tables = Tables::new(&lights, &[params(1, 0.1), params(2, 0.5)], &[], &[], &[]);
    assert_eq!(tables.light_at(0, [500.0, 0.0], 0.0, 0, true).unwrap().used, [(1, 1.0)]);
    assert_eq!(
        tables.light_at(0, [600.0, 0.0], 0.0, 0, true).unwrap().used,
        [(1, 1.0), (3, 1.0)]
    );
}

#[test]
fn lights_sharing_a_centre_are_mixed_the_widest_inner_radius_first() {
    let records = [params(1, 0.1), params(2, 0.5), params(3, 0.9)];
    let colours = [greys(1, &[(0, 100)]), greys(19, &[(0, 200)]), greys(37, &[(0, 0)])];
    // The lights 2 (diffuse 200) and 3 (diffuse 0), 3 `apart` yards north of 2 and `up` yards
    // higher, of inner radii `inner`; the place 30 yards west of 2.
    let mixed = |apart: f32, up: f32, inner: [f32; 2]| {
        let mut third = light(3, [1_000.0 + apart, 0.0], [inner[1], 300.0], [3, 0]);
        third.position[1] += up * 36.0;
        let lights = [
            light(1, [0.0; 2], [0.0; 2], [1, 0]),
            light(2, [1_000.0, 0.0], [inner[0], 300.0], [2, 0]),
            third,
        ];
        let tables = Tables::new(&lights, &records, &colours, &[], &[]);
        let mixed = tables.light_at(0, [1_000.0, 30.0], 0.0, 0, true).unwrap();
        (ids(&mixed), diffuse(&mixed).round())
    };
    // Within a third of a yard: the widest inner radius first, the narrowest weighing last.
    assert_eq!(mixed(0.2, 0.0, [100.0, 50.0]), (vec![1, 2, 3], 0.0));
    assert_eq!(mixed(0.2, 0.0, [50.0, 100.0]), (vec![1, 3, 2], 200.0));
    assert_eq!(mixed(0.0, 0.33, [50.0, 100.0]), (vec![1, 3, 2], 200.0));
    // Farther apart, on the map or in height: the farthest first, then by id.
    assert_eq!(mixed(0.5, 0.0, [100.0, 50.0]), (vec![1, 3, 2], 200.0));
    assert_eq!(mixed(0.0, 10.0, [50.0, 100.0]), (vec![1, 2, 3], 0.0));
    // A centre shared through a light near both: the three by their inner radii.
    let lights = [
        light(1, [0.0; 2], [0.0; 2], [1, 0]),
        light(2, [1_000.0, 0.0], [100.0, 300.0], [2, 0]),
        light(3, [1_000.3, 0.0], [60.0, 300.0], [3, 0]),
        light(4, [1_000.6, 0.0], [50.0, 300.0], [2, 0]),
    ];
    let tables = Tables::new(&lights, &records, &colours, &[], &[]);
    assert_eq!(
        ids(&tables.light_at(0, [1_000.0, 30.0], 0.0, 0, true).unwrap()),
        [1, 2, 3, 4]
    );
    // The same, the light near both listed first: 4 led to 2, then 3 to 2 through it, at the
    // centre of 2 although 3's is nearer.
    let lights = [
        light(1, [0.0; 2], [0.0; 2], [1, 0]),
        light(4, [1_000.0, 0.0], [60.0, 300.0], [2, 0]),
        light(2, [999.75, 0.0], [50.0, 300.0], [2, 0]),
        light(3, [1_000.25, 0.0], [100.0, 300.0], [3, 0]),
    ];
    let tables = Tables::new(&lights, &records, &colours, &[], &[]);
    assert_eq!(
        ids(&tables.light_at(0, [1_001.0, 30.0], 0.0, 0, true).unwrap()),
        [1, 3, 4, 2]
    );
}

#[test]
fn the_sun_turns_once_a_day_by_noggit_s_table_to_the_north_west() {
    let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-3);
    // 37° high at midnight and noon, 20° at 6 h and 18 h, between them linearly by the angle.
    for (hour, towards) in [
        (0.0, [0.565, 0.565, 0.602]),
        (6.0, [0.664, 0.664, 0.342]),
        (12.0, [0.565, 0.565, 0.602]),
        (18.0, [0.664, 0.664, 0.342]),
        (24.0, [0.565, 0.565, 0.602]),
    ] {
        assert!(
            close(sun_direction(hour * 120.0), towards),
            "{hour} h: {:?}",
            sun_direction(hour * 120.0)
        );
    }
    let three = sun_direction(360.0);
    assert!((three[2] - (-(118.5f32.to_radians().cos()))).abs() < 1e-4, "{three:?}");
}

#[test]
fn the_light_of_the_water_is_that_of_its_bands_with_the_alphas_of_its_params() {
    let lights = [light(1, [0.0; 2], [0.0; 2], [1, 0])];
    // The bands 9, 14, 15 and 17 of the params 1; the band 16 lacking.
    let bands = [
        greys(10, &[(0, 255)]),
        greys(15, &[(0, 51)]),
        greys(16, &[(0, 102)]),
        greys(18, &[(0, 204)]),
    ];
    let tables = Tables::new(&lights, &[params(1, 0.1)], &bands, &[], &[]);
    let values = tables.light_at(0, [0.0; 2], 0.0, 0, true).unwrap().values;
    let water = map_light(&values, 0.0, None, false).water;
    assert_eq!(water.sun, [1.0; 3]);
    assert_eq!(water.ocean, [[0.2, 0.2, 0.2, 0.75], [0.4, 0.4, 0.4, 1.0]]);
    assert_eq!(
        water.river,
        [[0.0, 0.0, 0.0, 0.1], [0.8, 0.8, 0.8, 1.0]],
        "black where a band lacks"
    );
}

#[test]
fn the_light_given_to_the_view_is_the_fixed_one_where_the_tables_give_none() {
    let tables = tables();
    let values = tables.light_at(0, [920.0, -50.0], 0.0, 0, true).unwrap().values;
    let given = map_light(&values, 0.0, Some([1.0, 2.0, 3.0]), false);
    assert_eq!(given.sun.colour, [200.0 / 255.0; 3], "the diffuse of the light 2");
    assert_eq!(given.sun.ambient, [40.0 / 255.0; 3], "the ambient of the global light");
    assert_eq!(
        given.fog_colour,
        viewport::Fog::default().colour,
        "no fog band: the fixed colour"
    );
    // The fog of the game given, none for the editor's.
    assert_eq!(given.fog, Some([1.0, 2.0, 3.0]));
    assert_eq!(map_light(&values, 0.0, None, false).fog, None, "the editor's");
    // No band but the colour of the fog: the fixed sun.
    let lights = [light(1, [0.0; 2], [0.0; 2], [1, 0])];
    let empty = Tables::new(&lights, &[params(1, 0.1)], &[greys(8, &[(0, 128)])], &[], &[]);
    let values = empty.light_at(0, [0.0; 2], 0.0, 0, true).unwrap().values;
    let given = map_light(&values, 0.0, None, false);
    assert_eq!(
        (given.sun.colour, given.sun.ambient),
        (Sun::default().colour, Sun::default().ambient)
    );
    assert!(
        (given.fog_colour[0] - 0.2158).abs() < 1e-3,
        "a grey fog, made linear: {:?}",
        given.fog_colour
    );
    // Its sky the bands 2 to 7 in gamma, the colour of its fog where it lacks one.
    let grey = 128.0 / 255.0;
    assert_eq!(given.sky, [[grey; 3]; 6]);
    let topped = Tables::new(
        &lights,
        &[params(1, 0.1)],
        &[greys(3, &[(0, 255)]), greys(7, &[(0, 51)]), greys(8, &[(0, 128)])],
        &[],
        &[],
    );
    let values = topped.light_at(0, [0.0; 2], 0.0, 0, true).unwrap().values;
    let sky = map_light(&values, 0.0, None, false).sky;
    assert_eq!(sky, [[1.0; 3], [grey; 3], [grey; 3], [grey; 3], [0.2; 3], [grey; 3]]);
    // Without a colour of fog either, that of the fixed fog, in gamma.
    let values = tables.light_at(0, [-5_000.0, 0.0], 0.0, 0, true).unwrap().values;
    let fixed = viewport::Fog::default().colour;
    let sky = map_light(&values, 0.0, None, false).sky;
    for (channel, linear) in sky[3].iter().zip(fixed) {
        let back = ((channel + 0.055) / 1.055).powf(2.4);
        assert!((back - linear).abs() < 1e-4, "{sky:?}");
    }
    assert!(
        sky[0] == [1.0; 3] && sky[1..].iter().all(|colour| *colour == sky[1]),
        "its top its own, the bands it lacks of the fixed fog: {sky:?}"
    );
}

#[test]
fn the_fog_of_a_light_is_prepared_straight_before_outland_and_curved_to_the_far_clip_from_it() {
    let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-3);
    // Before the map 530: straight, its end 10 yards at least, its share within -1 and 1.
    assert_eq!(prepared_fog(500.0, 0.25, 0, 1_277.0), [500.0, 0.25, 1.0]);
    assert_eq!(prepared_fog(2_000.0, -0.2, 529, 1_277.0), [2_000.0, -0.2, 1.0]);
    assert_eq!(prepared_fog(5.0, 1.5, 1, 1_277.0), [10.0, 1.0, 1.0]);
    assert_eq!(prepared_fog(0.0, -2.0, 0, 1_277.0), [10.0, -1.0, 1.0]);
    // From it: ending at the far clip, its share no less than 0, its rate by its own fog against
    // the far clip less 200 yards, 700 at most, 1.5 past it, up to 7.
    let deadwind = prepared_fog(361.11, -0.2, 530, 1_277.0);
    assert!(
        close(deadwind, [1_277.0, 0.0, 1.5 + 5.5 * (1.0 - 433.332 / 500.0)]),
        "{deadwind:?}"
    );
    let northrend = prepared_fog(888.89, 0.5, 571, 1_277.0);
    assert!(
        close(northrend, [1_277.0, 0.5, 1.5 + 5.5 * (1.0 - 444.445 / 500.0)]),
        "{northrend:?}"
    );
    assert_eq!(prepared_fog(1_000.0, 0.5, 571, 600.0), [600.0, 0.5, 1.5]);
    let near = prepared_fog(500.0, 0.25, 571, 600.0);
    assert!(
        close(near, [600.0, 0.25, 1.5 + 5.5 * (1.0 - 375.0 / 400.0)]),
        "{near:?}"
    );
    assert!(close(prepared_fog(400.0, 1.0, 571, 1_277.0), [1_277.0, 1.0, 7.0]));
    assert_eq!(prepared_fog(500.0, 0.0, 571, 1_277.0), [1_277.0, 0.0, 1.5]);
    // Shorter than 1000/36 yards: straight, where it ends; its share no less than 0 still.
    assert_eq!(prepared_fog(20.0, -0.5, 571, 1_277.0), [20.0, 0.0, 1.0]);
    assert_eq!(prepared_fog(27.7, 0.5, 571, 1_277.0), [27.7, 0.5, 1.0]);
    assert_eq!(prepared_fog(27.8, 0.5, 571, 1_277.0)[0], 1_277.0);
}

#[test]
fn under_a_liquid_the_lights_take_their_params_under_the_water_or_the_liquid_its_own_alone() {
    let tables = tables().with_liquids(&liquids());
    assert_eq!(
        [5, 3, 99].map(|liquid| tables.immersion(Some(liquid))),
        [Immersion::Under, Immersion::Lit(4), Immersion::Under],
        "a water, a magma lit by its own params, a type unknown under the water"
    );
    assert_eq!(tables.immersion(None), Immersion::Dry);
    // Within the light 2: its clear params, diffuse 200; under the water, its params 4, diffuse 60,
    // the global light without params under the water keeping its own.
    let place = [920.0, -50.0];
    let dry = tables.light_in(0, place, 0.0, true, Immersion::Dry).unwrap();
    let under = tables.light_in(0, place, 0.0, true, Immersion::Under).unwrap();
    assert_eq!((ids(&dry), ids(&under)), (vec![1, 2], vec![1, 2]));
    assert!((diffuse(&dry) - 200.0).abs() < 1e-3 && (diffuse(&under) - 60.0).abs() < 1e-3);
    // Its fog under the water, to 100 yards from 0.
    let fog = |mixed, map, immersion| tables.fog_of_the_game(mixed, map, immersion, 0.0, 1_277.0).unwrap();
    assert_eq!(fog(&under, 0, Immersion::Under), [0.0, 100.0, 1.0]);
    // In the magma, anywhere: the params 4 alone, no light mixed in, their fog too.
    let lit = tables
        .light_in(0, [-5_000.0, 0.0], 0.0, true, Immersion::Lit(4))
        .unwrap();
    assert!(lit.used.is_empty());
    assert!((diffuse(&lit) - 60.0).abs() < 1e-3);
    assert_eq!(fog(&lit, 0, Immersion::Lit(4)), [0.0, 100.0, 1.0]);
    assert!(
        tables.light_in(0, place, 0.0, true, Immersion::Lit(99)).is_none(),
        "params unknown"
    );
    // On Northrend, of the light 1 from a quarter of 500 yards, curved: twice as steep under a
    // liquid, its distances kept.
    let northrend = tables.light_in(571, place, 0.0, true, Immersion::Under).unwrap();
    let [start, end, rate] = fog(&northrend, 571, Immersion::Dry);
    assert_eq!([start, end], [319.25, 1_277.0]);
    assert!((rate - (1.5 + 5.5 * (1.0 - 375.0 / 500.0))).abs() < 1e-4, "{rate}");
    assert_eq!(fog(&northrend, 571, Immersion::Under), [start, end, rate * 2.0]);
    let curved_lit = fog(&lit, 571, Immersion::Lit(4))[2];
    assert!(
        (curved_lit - 2.0 * (1.5 + 5.5 * (1.0 - 100.0 / 500.0))).abs() < 1e-4,
        "{curved_lit}"
    );
    // Under a liquid, no sky: the colour of the fog all over.
    let grey = 128.0 / 255.0;
    let lights = [light(1, [0.0; 2], [0.0; 2], [1, 0])];
    let skied = Tables::new(
        &lights,
        &[params(1, 0.1)],
        &[greys(3, &[(0, 255)]), greys(8, &[(0, 128)])],
        &[],
        &[],
    );
    let values = skied.light_at(0, [0.0; 2], 0.0, 0, true).unwrap().values;
    assert_eq!(map_light(&values, 0.0, None, false).sky[0], [1.0; 3]);
    assert_eq!(map_light(&values, 0.0, None, true).sky, [[grey; 3]; 6]);
}

#[test]
fn the_fog_of_the_game_is_that_of_the_lights_mixed_within_the_far_clip() {
    let tables = tables();
    let fog = |place: [f32; 2], map: u32, far: f32| {
        let mixed = tables.light_at(map, place, 0.0, 0, true).unwrap();
        tables.fog_of_the_game(&mixed, map, Immersion::Dry, 0.0, far).unwrap()
    };
    // The global light alone: from a quarter of 500 yards, straight; within the far clip.
    assert_eq!(fog([-5_000.0, 0.0], 0, 1_277.0), [125.0, 500.0, 1.0]);
    assert_eq!(fog([-5_000.0, 0.0], 0, 300.0), [75.0, 300.0, 1.0]);
    // Within the light 2, whose fog ends at 0: 10 yards, as the client reads it; halfway, half of
    // it mixed in.
    assert_eq!(fog([920.0, -50.0], 0, 1_277.0), [0.0, 10.0, 1.0]);
    // Within the light 3 too, which has no band of fog: its end read as 0 as well.
    assert_eq!(fog([1_100.0, 0.0], 0, 1_277.0), [0.0, 10.0, 1.0]);
    let halfway = fog([800.0, 0.0], 0, 1_277.0);
    assert!(
        halfway
            .iter()
            .zip([31.875, 255.0, 1.0])
            .all(|(a, b)| (a - b).abs() < 1e-2),
        "{halfway:?}"
    );
    // From Outland on, each light curved before they are mixed: a global light ending at 300
    // yards from half of it, and a local one ending at 2,000 from a fifth, by half.
    let mut local = light(2, [1_000.0, 0.0], [100.0, 300.0], [2, 0]);
    local.map = 571;
    let mut global = light(1, [0.0; 2], [0.0; 2], [1, 0]);
    global.map = 571;
    let numbers = [
        numbers(1, &[(0, 300.0 * 36.0)]),
        numbers(2, &[(0, 0.5)]),
        numbers(7, &[(0, 2_000.0 * 36.0)]),
        numbers(8, &[(0, 0.2)]),
    ];
    let curved = Tables::new(&[global, local], &[params(1, 0.1), params(2, 0.5)], &[], &numbers, &[]);
    let mixed = curved.light_at(571, [800.0, 0.0], 0.0, 0, true).unwrap();
    assert_eq!(ids(&mixed), [1, 2]);
    assert!((mixed.used[1].1 - 0.5).abs() < 1e-4, "{:?}", mixed.used);
    let fog = curved
        .fog_of_the_game(&mixed, 571, Immersion::Dry, 0.0, 1_277.0)
        .unwrap();
    let rate = (1.5 + 5.5 * (1.0 - 150.0 / 500.0) + 1.5) / 2.0;
    assert!(
        fog.iter()
            .zip([0.35 * 1_277.0, 1_277.0, rate])
            .all(|(a, b)| (a - b).abs() < 1e-2),
        "{fog:?}"
    );
}

#[test]
fn the_hour_set_turns_at_its_speed_past_midnight() {
    assert_eq!(crate::half_minutes(720.0, 0, 100.0), 1440.0, "still");
    assert_eq!(crate::half_minutes(720.0, 60, 2.0), 1680.0, "two hours in two seconds");
    assert_eq!(crate::half_minutes(1439.0, 1, 2.0), 2.0, "past midnight");
    // The speed alone changed: on from the hour reached, not from the hour set.
    let mut module = crate::LightingModule {
        set: Some((720, 60, 720.0, Instant::now() - Duration::from_secs(10))),
        ..Default::default()
    };
    let time = module.time(720, 0);
    assert!((time - 2640.0).abs() < 2.0, "{time}");
    assert!((module.time(720, 0) - time).abs() < 0.1, "then still");
    // The hour set again: from it.
    assert_eq!(module.time(600, 0), 1200.0);
}

#[test]
fn the_light_is_a_category_of_the_settings_noon_by_default_still() {
    let mut module = crate::LightingModule::default();
    let mut reg = Registrar::default();
    module.register(&mut reg);
    let category = reg.settings.expect("declared");
    assert_eq!(category.title, "Light");
    let settings: Vec<_> = category
        .settings
        .iter()
        .map(|spec| (spec.key.as_str(), spec.range, spec.default))
        .collect();
    assert_eq!(
        settings,
        [
            ("hour", [0, 1439], 720),
            ("speed", [0, 1440], 0),
            ("local_lights", [0, 1], 1),
            ("game_fog", [0, 1], 1),
            ("far_clip", [184, 2000], 1277)
        ]
    );
}

/// Formats that read the tables of `tables` and the zones of light `zones`, or why they cannot,
/// and nothing else.
struct TestFormats {
    zones: Result<Vec<ZoneLightRecord>, String>,
    liquids: Result<Vec<LiquidTypeRecord>, String>,
}

impl Formats for TestFormats {
    fn lights(&self) -> Result<Arc<Vec<LightRecord>>, String> {
        Ok(Arc::new(parts().0))
    }
    fn light_params(&self) -> Result<Arc<Vec<LightParamsRecord>>, String> {
        Ok(Arc::new(parts().1))
    }
    fn light_colours(&self) -> Result<Arc<Vec<LightBand<[u8; 3]>>>, String> {
        Ok(Arc::new(parts().2))
    }
    fn light_numbers(&self) -> Result<Arc<Vec<LightBand<f32>>>, String> {
        Ok(Arc::new(parts().3))
    }
    fn zone_lights(&self) -> Result<Arc<Vec<ZoneLightRecord>>, String> {
        self.zones.clone().map(Arc::new)
    }
    fn liquid_types(&self) -> Result<Arc<Vec<LiquidTypeRecord>>, String> {
        self.liquids.clone().map(Arc::new)
    }
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
    fn texture(&self, _file: &FileRef) -> Result<Texture, String> {
        Err("none".to_owned())
    }
    fn texture_rgba(&self, _file: &FileRef) -> Result<Texture, String> {
        Err("none".to_owned())
    }
}

/// The client's files, in the state the test sets.
struct Files(Mutex<VfsState>);

impl Vfs for Files {
    fn read(&self, _path: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
    fn exists(&self, _path: &str) -> bool {
        false
    }
    fn files_under(&self, _folder: &str) -> Vec<String> {
        Vec::new()
    }
    fn path_of(&self, _file_data_id: u32) -> Option<String> {
        None
    }
    fn state(&self) -> VfsState {
        self.0.lock().unwrap().clone()
    }
}

/// A view that keeps the light it is given.
#[derive(Default)]
struct View(Mutex<Option<MapLight>>);

impl viewport::Viewport for View {
    fn add_layer(&self, _owner: &str, _layer: Box<dyn viewport::Layer>) {}
    fn remove_layers(&self, _owner: &str) {}
    fn target(&self) -> viewport::Target {
        unimplemented!("the light draws nothing")
    }
    fn wait_frame(&self, _after: u64, _timeout: Duration) -> Option<viewport::Frame> {
        None
    }
    fn tell_budget(&self, _owner: &str, _demand: viewport::Demand) -> viewport::Allowance {
        unimplemented!("the light keeps nothing on the GPU")
    }
    fn allowance(&self) -> viewport::Allowance {
        unimplemented!("the light keeps nothing on the GPU")
    }
    fn set_budget(&self, _bytes: u64) {}
    fn set_fog(&self, _fog: viewport::Fog) {}
    fn set_light(&self, owner: &str, light: Option<MapLight>) {
        assert_eq!(owner, "lighting", "given as the module");
        *self.0.lock().unwrap() = light;
    }
}

/// An editor whose terrain shows the map `map` (none for `null`) and whose camera stands at
/// `camera`, unread when none.
struct Shown {
    map: Mutex<Value>,
    camera: Mutex<Option<[f64; 3]>>,
}

impl EditorBackend for Shown {
    fn commands(&self) -> Vec<CommandInfo> {
        Vec::new()
    }
    fn call(&self, _caller: &str, name: &str, _arguments: Value) -> Result<Value, String> {
        match name {
            "terrain.map" => Ok(self.map.lock().unwrap().clone()),
            _ => Err("unknown".to_owned()),
        }
    }
    fn publish(&self, _source: &str, _topic: &str, _payload: Value) -> Result<(), String> {
        Ok(())
    }
    fn subscribe(&self, _caller: &str, _topic: &str) -> Result<u64, String> {
        Err("none".to_owned())
    }
    fn next_event(&self, _caller: &str, _subscription: u64, _timeout: Duration) -> Result<Option<Event>, String> {
        Ok(None)
    }
    fn unsubscribe(&self, _subscription: u64) {}
    fn setting(&self, _caller: &str, _space: &str, _key: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }
    fn set_setting(&self, _caller: &str, _space: &str, _key: &str, _value: Value) -> Result<(), String> {
        Ok(())
    }
    fn begin_group(&self, _caller: &str, _label: &str) -> Result<(), String> {
        Ok(())
    }
    fn end_group(&self, _caller: &str) -> Result<(), String> {
        Ok(())
    }
    fn read_property(&self, _caller: &str, path: &str) -> Result<PropertyValue, String> {
        match (path, *self.camera.lock().unwrap()) {
            ("viewport/camera_position", Some(at)) => Ok(PropertyValue::Vector(at)),
            _ => Err("unread".to_owned()),
        }
    }
}

/// Liquids where the eye is in the liquid `liquid`, keeping the point last asked.
#[derive(Default)]
struct Pond {
    liquid: Mutex<Option<u16>>,
    asked: Mutex<Option<[f32; 3]>>,
}

impl Liquids for Pond {
    fn surfaces(&self) -> Arc<Surfaces> {
        Arc::default()
    }
    fn liquid_at(&self, at: [f32; 3]) -> Option<u16> {
        *self.asked.lock().unwrap() = Some(at);
        *self.liquid.lock().unwrap()
    }
}

/// A host offering `formats`, `vfs`, `viewport` and `liquids`, counting the jobs started, kept
/// unrun for the test to run them, and those cancelled, its editor `editor`.
struct Host {
    formats: Arc<dyn Formats>,
    files: Arc<dyn Vfs>,
    pond: Arc<Pond>,
    liquids: uniwow_api::liquids::Handle,
    view: viewport::Handle,
    seen: Arc<View>,
    started: Vec<String>,
    jobs: Vec<JobFn>,
    cancelled: Vec<JobId>,
    settings: HashMap<String, Value>,
    editor: Arc<Shown>,
}

fn host(files: Arc<Files>) -> Host {
    let seen = Arc::new(View::default());
    let pond = Arc::new(Pond::default());
    Host {
        liquids: pond.clone(),
        pond,
        formats: Arc::new(TestFormats {
            zones: Err("none".to_owned()),
            liquids: Ok(liquids()),
        }),
        files,
        view: seen.clone(),
        seen,
        started: Vec::new(),
        jobs: Vec::new(),
        cancelled: Vec::new(),
        settings: HashMap::new(),
        editor: Arc::new(Shown {
            map: Mutex::new(Value::Null),
            camera: Mutex::new(None),
        }),
    }
}

impl uniwow_api::Host for Host {
    fn publish(&mut self, _source: &str, _topic: &str, _payload: uniwow_api::serde_json::Value) {}
    fn execute(&mut self, _owner: &str, _command: Box<dyn uniwow_api::Command>) {}
    fn forget_document(&mut self, _owner: &str, _document: &str) {}
    fn service(&self, id: &str) -> Option<&(dyn Any + Send + Sync)> {
        match id {
            "formats" => Some(&self.formats),
            "vfs" => Some(&self.files),
            "viewport" => Some(&self.view),
            "liquids" => Some(&self.liquids),
            _ => None,
        }
    }
    fn service_provider(&self, _id: &str) -> Option<String> {
        None
    }
    fn gpu(&self) -> Option<&egui_wgpu::RenderState> {
        None
    }
    fn gpu_memory(&self) -> Option<u64> {
        None
    }
    fn draw_panel(&mut self, _owner: &str, _objects: &uniwow_api::ui::SharedUi, _panel: &str, _ui: &mut egui::Ui) {}
    fn draw_dialogs(&mut self, _owner: &str, _objects: &uniwow_api::ui::SharedUi, _egui: &egui::Context) {}
    fn adopt_objects(&mut self, _owner: &str, _objects: &uniwow_api::ui::SharedUi) {}
    fn setting(&self, _module: &str, key: &str) -> Option<uniwow_api::serde_json::Value> {
        self.settings.get(key).cloned()
    }
    fn set_setting(&mut self, _module: &str, key: &str, value: uniwow_api::serde_json::Value) {
        self.settings.insert(key.to_owned(), value);
    }
    fn report_failure(&mut self, _reporter: &str, _culprit: &str, _message: &str) {}
    fn spawn(&mut self, _owner: &str, label: &str, job: JobFn) -> JobId {
        self.started.push(label.to_owned());
        self.jobs.push(job);
        JobId(self.started.len() as u64)
    }
    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId {
        self.spawn(owner, label, job)
    }
    fn cancel(&mut self, _owner: &str, job: JobId) {
        self.cancelled.push(job);
    }
    fn call(&mut self, _caller: &str, _name: &str, _arguments: uniwow_api::serde_json::Value) -> CallId {
        CallId(1)
    }
    fn editor(&self, caller: &str) -> Editor {
        Editor::new(self.editor.clone(), caller)
    }
}

#[test]
fn the_tables_are_read_once_the_client_is_open_and_again_once_it_changes() {
    let files = Arc::new(Files(Mutex::new(VfsState::Opening)));
    let mut host = host(files.clone());
    let egui = egui::Context::default();
    let mut module = crate::LightingModule::default();
    // While the archives are opened: nothing read.
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert!(host.started.is_empty());
    // Open: read once.
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 3,
        files: 100,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(host.started.len(), 1);
    let refused: Result<crate::Read, String> = Err("refused".to_owned());
    module.on_job(
        JobId(1),
        JobOutcome::Done(Box::new(refused)),
        &mut Context::new(&mut host, "lighting"),
    );
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(host.started.len(), 1, "refused, not read again by the same archives");
    // Other archives: read again.
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 4,
        files: 120,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(host.started.len(), 2);
    // The client closed while they are read: the read cancelled.
    *files.0.lock().unwrap() = VfsState::NoClient("none".to_owned());
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(host.cancelled, [JobId(2)]);
    assert_eq!(host.started.len(), 2);
    // A job of before coming back is not taken.
    let read: Result<crate::Read, String> = Ok((Arc::new(Tables::default()), None, None));
    module.on_job(
        JobId(2),
        JobOutcome::Done(Box::new(read)),
        &mut Context::new(&mut host, "lighting"),
    );
    assert!(module.tables.is_none());
    // Cancelled from the jobs, or panicked: not read again by the same archives.
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 5,
        files: 130,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    module.on_job(
        JobId(3),
        JobOutcome::Cancelled,
        &mut Context::new(&mut host, "lighting"),
    );
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert!(matches!(module.tables, Some(Err(_))));
    assert_eq!(host.started.len(), 3);
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 6,
        files: 140,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    module.on_job(
        JobId(4),
        JobOutcome::Panicked("boom".to_owned()),
        &mut Context::new(&mut host, "lighting"),
    );
    assert_eq!(
        module.tables.as_ref().map(|read| read.clone().err()),
        Some(Some("boom".to_owned()))
    );
    // Read without the zones of light: the tables taken, why said; forgotten once the client changes.
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 7,
        files: 150,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    let read: Result<crate::Read, String> = Ok((Arc::new(Tables::default()), Some("no Wow.exe".to_owned()), None));
    module.on_job(
        JobId(5),
        JobOutcome::Done(Box::new(read)),
        &mut Context::new(&mut host, "lighting"),
    );
    assert!(matches!(module.tables, Some(Ok(_))));
    assert_eq!(module.zones_unread.as_deref(), Some("no Wow.exe"));
    *files.0.lock().unwrap() = VfsState::Ready {
        archives: 8,
        files: 160,
    };
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(module.zones_unread, None);
    assert_eq!(host.started.len(), 6);
}

#[test]
fn the_light_is_that_of_the_map_shown_at_the_camera_s_place_on_it() {
    let files = Arc::new(Files(Mutex::new(VfsState::Ready { archives: 1, files: 1 })));
    let mut host = host(files);
    let mut module = crate::LightingModule {
        client: Some((1, 1)),
        tables: Some(Ok(Arc::new(tables()))),
        ..Default::default()
    };
    let egui = egui::Context::default();
    *host.editor.map.lock().unwrap() = json!({ "id": 0, "name": "Azeroth" });
    // Within the light 2 at 1,000, 0 on the map, high above it: its clear params, not those under
    // the water.
    *host.editor.camera.lock().unwrap() = Some([920.0, -50.0, 600.0]);
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    let shown = module.shown.as_ref().expect("a map shown");
    assert_eq!(
        (shown.map, shown.name.as_str(), shown.place),
        (0, "Azeroth", [920.0, -50.0])
    );
    let light = shown.light.as_ref().unwrap();
    assert_eq!(light.used, [(1, 1.0), (2, 1.0)]);
    // Given to the view, the fog of the game by default.
    let given = host.seen.0.lock().unwrap().expect("given");
    assert_eq!(Some(given), shown.given);
    // Within the light 2, whose fog ends at 0: 10 yards, as the client reads it.
    assert_eq!(given.fog, Some([0.0, 10.0, 1.0]));
    assert_eq!(given.sun.direction, sun_direction(1440.0));
    // At noon by default, in half-minutes, the hour read by the bands.
    assert_eq!(shown.time, 1440.0);
    assert_eq!(light.values.numbers[2], Some(10.0));
    assert!((diffuse(light) - 200.0).abs() < 1e-3);
    assert_eq!(
        *host.pond.asked.lock().unwrap(),
        Some([920.0, -50.0, 600.0]),
        "asked at the eye"
    );
    // The eye under a water: the params under the water of the lights, their fog, no sky; in the
    // magma, the light of its params alone, said in the panel.
    module.tables = Some(Ok(Arc::new(tables().with_liquids(&liquids()))));
    *host.pond.liquid.lock().unwrap() = Some(5);
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    let shown = module.shown.as_ref().unwrap();
    assert_eq!((shown.liquid, shown.immersion), (Some(5), Immersion::Under));
    assert!((diffuse(shown.light.as_ref().unwrap()) - 60.0).abs() < 1e-3);
    let given = host.seen.0.lock().unwrap().unwrap();
    assert_eq!(given.fog, Some([0.0, 100.0, 1.0]));
    assert!(
        given.sky.iter().all(|colour| *colour == given.sky[0]) && given.sky[0] != [1.0; 3],
        "the colour of the fog all over, not the white top of the sky"
    );
    assert!(
        panel(&mut module, &mut host)
            .iter()
            .any(|text| text.contains("the params under the water"))
    );
    *host.pond.liquid.lock().unwrap() = Some(3);
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    let shown = module.shown.as_ref().unwrap();
    assert_eq!(shown.immersion, Immersion::Lit(4));
    assert!(shown.light.as_ref().unwrap().used.is_empty());
    assert!(
        panel(&mut module, &mut host)
            .iter()
            .any(|text| text.contains("The eye in the liquid 3: the light of its params 4"))
    );
    *host.pond.liquid.lock().unwrap() = None;
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(module.shown.as_ref().unwrap().immersion, Immersion::Dry);
    assert_eq!(
        host.seen.0.lock().unwrap().unwrap().sky[0],
        [1.0; 3],
        "out of it, the sky again"
    );
    // The local lights switched off.
    host.settings.insert("local_lights".to_owned(), json!(0));
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(module.shown.as_ref().unwrap().light.as_ref().unwrap().used, [(1, 1.0)]);
    // On Northrend, of the light 1, the fog curved to the far clip set.
    host.settings.insert("far_clip".to_owned(), json!(600));
    *host.editor.map.lock().unwrap() = json!({ "id": 571, "name": "Northrend" });
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    let fog = host.seen.0.lock().unwrap().unwrap().fog.unwrap();
    assert_eq!(fog, [150.0, 600.0, 1.5 + 5.5 * (1.0 - 375.0 / 400.0)]);
    *host.editor.map.lock().unwrap() = json!({ "id": 0, "name": "Azeroth" });
    // The camera unread: the light before kept; no map shown: none.
    *host.editor.camera.lock().unwrap() = None;
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(module.shown.as_ref().map(|shown| shown.place), Some([920.0, -50.0]));
    assert!(host.seen.0.lock().unwrap().is_some(), "the light before kept");
    // The fog of the editor chosen.
    *host.editor.camera.lock().unwrap() = Some([920.0, -50.0, 600.0]);
    host.settings.insert("game_fog".to_owned(), json!(0));
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert_eq!(host.seen.0.lock().unwrap().unwrap().fog, None);
    *host.editor.map.lock().unwrap() = Value::Null;
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert!(module.shown.is_none());
    assert!(host.seen.0.lock().unwrap().is_none(), "no map: the fixed light");
    // Stopped: taken back from the view.
    *host.editor.map.lock().unwrap() = json!({ "id": 0, "name": "Azeroth" });
    module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
    assert!(host.seen.0.lock().unwrap().is_some());
    module.shutdown();
    assert!(host.seen.0.lock().unwrap().is_none());
}

/// The texts the panel of `module` draws.
fn panel(module: &mut crate::LightingModule, host: &mut Host) -> Vec<String> {
    let egui = egui::Context::default();
    let mut output = egui.run_ui(egui::RawInput::default(), |ui| {
        module.panel_ui("lighting", ui, &mut Context::new(host, "lighting"));
    });
    output.textures_delta.clear();
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_lights_of_the_liquids_are_read_with_the_tables_the_light_read_without_them_if_need_be() {
    let files = Arc::new(Files(Mutex::new(VfsState::Ready { archives: 1, files: 1 })));
    let refused = "LiquidType.dbc refused".to_owned();
    for liquids in [Ok(liquids()), Err(refused.clone())] {
        let mut host = host(files.clone());
        host.formats = Arc::new(TestFormats {
            zones: Ok(Vec::new()),
            liquids: liquids.clone(),
        });
        *host.editor.map.lock().unwrap() = json!({ "id": 0, "name": "Azeroth" });
        *host.editor.camera.lock().unwrap() = Some([920.0, -50.0, 600.0]);
        *host.pond.liquid.lock().unwrap() = Some(3);
        let egui = egui::Context::default();
        let mut module = crate::LightingModule::default();
        module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
        let job = host.jobs.remove(0);
        let editor = Editor::new(host.editor.clone(), "lighting");
        let read = job(&JobContext::new(Arc::default(), Arc::default(), editor));
        module.on_job(
            JobId(1),
            JobOutcome::Done(read),
            &mut Context::new(&mut host, "lighting"),
        );
        module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
        let immersion = module.shown.as_ref().unwrap().immersion;
        let texts = panel(&mut module, &mut host);
        if liquids.is_ok() {
            assert_eq!(immersion, Immersion::Lit(4), "in the magma, its light");
            assert_eq!(module.liquids_unread, None);
        } else {
            // Without them, the light of the tables, the eye under the water of the magma, why said.
            assert_eq!(immersion, Immersion::Under);
            assert!(host.seen.0.lock().unwrap().is_some(), "a light still");
            assert!(
                texts.contains(&format!("No lights of the liquids: {refused}")),
                "{texts:?}"
            );
        }
    }
    // In a liquid whose params are unknown: said.
    let mut host = host(files);
    let mut module = crate::LightingModule {
        client: Some((1, 1)),
        tables: Some(Ok(Arc::new(tables().with_liquids(&[liquid(3, 99)])))),
        ..Default::default()
    };
    *host.editor.map.lock().unwrap() = json!({ "id": 0, "name": "Azeroth" });
    *host.editor.camera.lock().unwrap() = Some([920.0, -50.0, 600.0]);
    *host.pond.liquid.lock().unwrap() = Some(3);
    module.windows_ui(&egui::Context::default(), &mut Context::new(&mut host, "lighting"));
    let texts = panel(&mut module, &mut host);
    assert!(
        texts.contains(&"The eye in the liquid 3: the params 99 of its light unknown.".to_owned()),
        "{texts:?}"
    );
}

#[test]
fn the_zones_of_light_are_read_with_the_tables_and_said_in_the_panel() {
    let files = Arc::new(Files(Mutex::new(VfsState::Ready { archives: 1, files: 1 })));
    // Around the light 2: a zone of the light 3, whole at the place of the camera.
    let around = [[700.0, -300.0], [700.0, 300.0], [1_300.0, 300.0], [1_300.0, -300.0]];
    let refused = "no Wow.exe in the client's folder".to_owned();
    for zones in [Ok(vec![zone(3, &around)]), Err(refused.clone())] {
        let mut host = host(files.clone());
        host.formats = Arc::new(TestFormats {
            zones: zones.clone(),
            liquids: Ok(liquids()),
        });
        *host.editor.map.lock().unwrap() = json!({ "id": 0, "name": "Azeroth" });
        *host.editor.camera.lock().unwrap() = Some([920.0, -50.0, 600.0]);
        let egui = egui::Context::default();
        let mut module = crate::LightingModule::default();
        module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
        let job = host.jobs.remove(0);
        let editor = Editor::new(host.editor.clone(), "lighting");
        let read = job(&JobContext::new(Arc::default(), Arc::default(), editor));
        module.on_job(
            JobId(1),
            JobOutcome::Done(read),
            &mut Context::new(&mut host, "lighting"),
        );
        module.windows_ui(&egui, &mut Context::new(&mut host, "lighting"));
        let light = module.shown.as_ref().and_then(|shown| shown.light.clone()).unwrap();
        let texts = panel(&mut module, &mut host);
        let mixed = texts.iter().find(|text| text.starts_with("Lights mixed")).unwrap();
        if zones.is_ok() {
            assert_eq!((light.used, light.zones), (vec![(1, 1.0), (3, 1.0), (2, 1.0)], 1));
            assert_eq!(
                mixed,
                "Lights mixed, by their weights: 1 (global) 1.00, 3 (zone) 1.00, 2 1.00"
            );
            assert!(
                !texts.iter().any(|text| text.starts_with("No zones of light")),
                "{texts:?}"
            );
        } else {
            // Without them, the light of the tables, why said.
            assert_eq!(light.used, [(1, 1.0), (2, 1.0)]);
            assert_eq!(mixed, "Lights mixed, by their weights: 1 (global) 1.00, 2 1.00");
            assert!(texts.contains(&format!("No zones of light: {refused}")), "{texts:?}");
        }
    }
}
