//! Tests of the choice by the GPU: each instance by itself, its level kept from a frame to the next,
//! the blended instances drawn the farthest first, the templates kept at the level chosen, the
//! draws packed; on the software adapter of the system when it has one.

use std::collections::HashMap;
use std::sync::Arc;

use uniwow_api::formats::{FileRef, Model, ModelTexture, ModelTextureSource};
use uniwow_api::glam::Vec3;
use uniwow_api::liquids::{Liquids, Surfaces};
use uniwow_api::models::{Geosets, Look, Models};
use uniwow_api::viewport::Layer;
use uniwow_api::wgpu;

use crate::lock;
use crate::pool;
use crate::pool_tests::{LEFT, Pooled, RIGHT, colours, only, pixel, skin};
use crate::tests::{AIM, FRONT, Fake, depth_at, instance, middle, plain, render, settled, square};

/// Unlit and unfogged: a pixel is the colour of its texture.
const PLAIN: u16 = 0x03;

/// The look of the model `file` of `fake`, its textures its own.
/// Draws looking away from the instances, so that none is seen at the frame before, then towards
/// them: every one is found in sight by the second phase, whose draws `Choice::written` reads.
fn revealed(bench: &mut crate::tests::Bench) {
    render(bench, FRONT, FRONT * 2.0);
    render(bench, FRONT, AIM);
}

fn look(file: &str) -> Look {
    Look {
        model: FileRef::Path(file.to_owned()),
        textures: Vec::new(),
        geosets: Geosets::All,
    }
}

/// The square of `square` ten times larger, in two levels of skin, the first textured by
/// `first.blp`, the second by `second.blp`, of `material` (flags, blending); of radius 0.5, which
/// the levels go by: seen whole at the distances of its levels.
fn levelled(material: (u16, u16)) -> Model {
    let mut model = square(material.0, material.1);
    for vertex in &mut model.vertices {
        vertex.position = vertex.position.map(|axis| axis * 10.0);
    }
    model.textures = ["first.blp", "second.blp"]
        .map(|name| ModelTexture {
            source: ModelTextureSource::File(FileRef::Path(name.to_owned())),
            flags: 0,
        })
        .to_vec();
    model.texture_combos = vec![0, 1];
    let mut second = model.skins[0].clone();
    second.batches[0].texture_combo = 1;
    model.skins.push(second);
    model.radius = 0.5;
    model
}

/// The red first level and the blue second.
fn levels_textures() -> HashMap<String, uniwow_api::formats::Texture> {
    HashMap::from([
        ("first.blp".to_owned(), plain([255, 0, 0, 255])),
        ("second.blp".to_owned(), plain([0, 0, 255, 255])),
    ])
}

/// Where an instance of `levelled` stands so that `FRONT` is `ratio` radii from the nearest point
/// of its box, along the axis of the view.
fn at_ratio(ratio: f32) -> Vec3 {
    let along = ((0.5 * ratio).powi(2) - 0.25).sqrt() + 0.5;
    Vec3::new(FRONT.x - along, 0.0, 0.0)
}

/// Whether `seen` is red, else blue: the level drawn.
fn level_seen(seen: [u8; 4]) -> usize {
    if only(seen, 0) {
        0
    } else {
        assert!(only(seen, 2), "{seen:?}");
        1
    }
}

#[test]
fn each_instance_of_a_group_is_chosen_by_itself() {
    let fake = Fake {
        model: Some(square(0, 0)),
        textures: colours(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    assert!(pooled.add(&fake, &look("square.m2")));
    // One group: an instance in sight, one beside the view, one beyond the reach of its size.
    pooled.bench.service.place(
        "test",
        &[
            instance(1, 0, Vec3::new(-2.0, 0.0, 0.0), 1.0),
            instance(2, 0, Vec3::new(-2.0, 50.0, 0.0), 1.0),
            instance(3, 0, Vec3::new(-200.0, 0.0, 0.0), 1.0),
        ],
    );
    let image = settled(&mut pooled.bench, FRONT, AIM);
    assert!(only(middle(&image), 0), "{:?}", middle(&image));
    let stats = pooled.bench.layer.stats();
    assert!(
        stats.items.contains("3 instances in 1 groups in sight")
            && stats.items.contains("1 pairs, levels [1, 0, 0, 0]"),
        "{}",
        stats.items
    );
    assert_eq!((stats.draws, stats.triangles), (1, 2));
}

#[test]
fn the_level_of_an_instance_is_kept_until_past_its_limit_by_the_margin() {
    let fake = Fake {
        model: Some(levelled((PLAIN, 0))),
        textures: levels_textures(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    assert!(pooled.add(&fake, &look("square.m2")));
    // The limit of the second level at 40 radii, kept by a tenth of it either way.
    for (ratio, level) in [(42.0, 1), (38.0, 1), (30.0, 0), (42.0, 0), (46.0, 1)] {
        pooled
            .bench
            .service
            .place("test", &[instance(1, 0, at_ratio(ratio), 1.0)]);
        let seen = middle(&render(&mut pooled.bench, FRONT, AIM));
        assert_eq!(level_seen(seen), level, "at {ratio} radii");
    }
    settled(&mut pooled.bench, FRONT, AIM);
    let stats = pooled.bench.layer.stats();
    assert!(stats.items.contains("levels [0, 1, 0, 0]"), "{}", stats.items);
}

#[test]
fn a_level_is_kept_where_its_owner_moves_in_the_frame_and_lost_when_it_regroups() {
    let fake = Fake {
        model: Some(levelled((PLAIN, 0))),
        textures: levels_textures(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    assert!(pooled.add(&fake, &look("square.m2")));
    let aside = |id| instance(id, 0, Vec3::new(-2.0, 50.0, 0.0), 1.0);
    let bench = &mut pooled.bench;
    bench.service.place("before", &[aside(1)]);
    for ratio in [42.0, 38.0] {
        bench.service.place("test", &[instance(1, 0, at_ratio(ratio), 1.0)]);
        render(bench, FRONT, AIM);
    }
    // The owner before grows: the instances of this one move in the buffer of the frame.
    bench.service.place("before", &[aside(1), aside(2), aside(3)]);
    let seen = middle(&render(bench, FRONT, AIM));
    assert_eq!(level_seen(seen), 1, "kept where it moved");
    // Its own set regrouped: the level chosen again, the second no more kept at 38 radii.
    bench.service.place(
        "test",
        &[
            instance(1, 0, at_ratio(38.0), 1.0),
            instance(2, 0, Vec3::new(-2000.0, 0.0, 0.0), 1.0),
        ],
    );
    let seen = middle(&render(bench, FRONT, AIM));
    assert_eq!(level_seen(seen), 0, "chosen again");
}

/// A bench of a blended square, half seen through, unlit, of the looks red (0) and blue (1).
fn blended() -> Option<Pooled> {
    blended_on(Some(pool::SLOTS))
}

/// The bench of `blended`, with a pool of `slots` arrays or on the path of step 9.4c.
fn blended_on(slots: Option<usize>) -> Option<Pooled> {
    let mut model = square(PLAIN, 2);
    model.textures[0].source = ModelTextureSource::Filled(11);
    let fake = Fake {
        model: Some(model),
        textures: colours(),
        ..Fake::default()
    };
    let pooled = Pooled::on(slots)?;
    assert_eq!(pooled.add(&fake, &skin("red.blp")), slots.is_some());
    assert_eq!(pooled.add(&fake, &skin("blue.blp")), slots.is_some());
    Some(pooled)
}

/// An instance of `look` of `blended`, half seen through, at `x` on the axis of the view.
fn half(id: u64, look: u32, x: f32) -> uniwow_api::models::Instance {
    let mut placed = instance(id, look, Vec3::new(x, 0.0, 0.0), 1.0);
    placed.alpha = 0.5;
    placed
}

/// Whether the red and blue of `seen` are, once stored, `red` and `blue` in light, within 12.
fn stored(seen: [u8; 4], red: f32, blue: f32) -> bool {
    let srgb = |light: f32| {
        let value = if light <= 0.0031308 {
            light * 12.92
        } else {
            1.055 * light.powf(1.0 / 2.4) - 0.055
        };
        (value * 255.0).round() as u8
    };
    seen[0].abs_diff(srgb(red)) <= 12 && seen[2].abs_diff(srgb(blue)) <= 12 && seen[1] < 10
}

#[test]
fn blended_instances_are_drawn_one_by_one_the_farthest_first() {
    let Some(mut pooled) = blended() else {
        return;
    };
    // One behind another, from the farthest: red, blue, red, blue; the reds of one owner, the
    // blues of another.
    pooled
        .bench
        .service
        .place("reds", &[half(1, 0, 0.25), half(2, 0, 1.25)]);
    pooled
        .bench
        .service
        .place("blues", &[half(1, 1, 0.75), half(2, 1, 1.75)]);
    let seen = middle(&render(&mut pooled.bench, FRONT, AIM));
    // Each over the one before, half: 0.3125 red and 0.625 blue. The nearest first would give
    // 0.625 and 0.3125, the reds before the blues 0.1875 and 0.75.
    assert!(stored(seen, 0.3125, 0.625), "{seen:?}");
}

#[test]
fn the_blended_instances_of_each_owner_are_drawn_where_they_stand() {
    for slots in [Some(pool::SLOTS), None] {
        let Some(pooled) = blended_on(slots) else {
            return;
        };
        where_they_stand(pooled);
    }
}

/// The red of an owner on the left and the blue of another on the right, as `pooled` draws them.
fn where_they_stand(mut pooled: Pooled) {
    let side = |id, look, y| {
        let mut placed = instance(id, look, Vec3::new(0.0, y, 0.0), 0.5);
        placed.alpha = 0.5;
        placed
    };
    pooled.bench.service.place("reds", &[side(1, 0, -1.5)]);
    pooled.bench.service.place("blues", &[side(1, 1, 1.5)]);
    let image = render(&mut pooled.bench, FRONT, AIM);
    assert!(
        stored(pixel(&image, LEFT.0, LEFT.1), 0.5, 0.0),
        "{:?}",
        pixel(&image, LEFT.0, LEFT.1)
    );
    assert!(
        stored(pixel(&image, RIGHT.0, RIGHT.1), 0.0, 0.5),
        "{:?}",
        pixel(&image, RIGHT.0, RIGHT.1)
    );
}

/// Water at 0 around the origin.
struct Pond(Arc<Surfaces>);

impl Liquids for Pond {
    fn surfaces(&self) -> Arc<Surfaces> {
        self.0.clone()
    }
}

#[test]
fn a_blended_instance_beyond_the_surface_of_the_water_is_drawn_before_those_on_the_eye_s_side() {
    for slots in [Some(pool::SLOTS), None] {
        let Some(pooled) = blended_on(slots) else {
            return;
        };
        beyond_the_water_first(pooled);
    }
}

/// The red under the water and the blue over it, of two owners, drawn as `pooled` draws them.
fn beyond_the_water_first(mut pooled: Pooled) {
    let surfaces =
        Surfaces::from_cells((-8..=8).flat_map(|x| (-8..=8).map(move |y| (Surfaces::cell(x as f32, y as f32), 0.0))));
    // Red under the surface, blue over it, both in the middle of the view; seen from over the water
    // and from under it, the nearer drawn first where the farthest first would draw it last.
    let at = |id, look, x: f32, z: f32| {
        let mut placed = instance(id, look, Vec3::new(x, 0.0, z), 1.0);
        placed.alpha = 0.5;
        placed
    };
    let (above, below) = (FRONT, Vec3::new(5.0, 0.0, -1.0));
    for (eye, red, blue, seen_without, seen_with) in [
        (above, 1.0, 0.0, (0.5, 0.25), (0.25, 0.5)),
        (below, 0.0, 1.0, (0.25, 0.5), (0.5, 0.25)),
    ] {
        let bench = &mut pooled.bench;
        // Owners new to the layer: an order of their own, not one kept.
        bench.service.clear("reds");
        bench.service.clear("blues");
        bench.service.place("reds", &[at(1, 0, red, -0.5)]);
        bench.service.place("blues", &[at(1, 1, blue, 0.5)]);
        lock(&bench.scene).liquids = None;
        let seen = middle(&render(bench, eye, AIM));
        assert!(
            stored(seen, seen_without.0, seen_without.1),
            "the farthest first: {seen:?}"
        );
        lock(&bench.scene).liquids = Some(Arc::new(Pond(Arc::new(surfaces.clone()))));
        let seen = middle(&render(bench, eye, AIM));
        assert!(
            stored(seen, seen_with.0, seen_with.1),
            "beyond the water first: {seen:?}"
        );
    }
}

#[test]
fn the_order_of_blended_instances_is_kept_until_two_cross_by_the_margin() {
    let Some(mut pooled) = blended() else {
        return;
    };
    let bench = &mut pooled.bench;
    bench.service.place("test", &[half(1, 0, 1.0), half(2, 1, 1.5)]);
    let seen = middle(&render(bench, FRONT, AIM));
    assert!(stored(seen, 0.25, 0.5), "the red, then the blue: {seen:?}");
    // The blue now behind the red by less than the margin of 2 yards: still drawn after it.
    bench.service.place("test", &[half(1, 0, 1.0), half(2, 1, 0.8)]);
    let seen = middle(&render(bench, FRONT, AIM));
    assert!(stored(seen, 0.25, 0.5), "kept: {seen:?}");
    // Behind it by more: sorted again.
    bench.service.place("test", &[half(1, 0, 1.0), half(2, 1, -2.0)]);
    let seen = middle(&render(bench, FRONT, AIM));
    assert!(stored(seen, 0.5, 0.25), "sorted: {seen:?}");
}

#[test]
fn an_owner_out_of_sight_is_not_given_to_the_frame() {
    let fake = Fake {
        model: Some(square(0, 0)),
        textures: colours(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    assert!(pooled.add(&fake, &look("square.m2")));
    // An owner beside the view, another in sight: the second alone in the frame, from its start.
    pooled
        .bench
        .service
        .place("aside", &[instance(1, 0, Vec3::new(-2.0, 50.0, 0.0), 1.0)]);
    pooled
        .bench
        .service
        .place("seen", &[instance(2, 0, Vec3::new(-2.0, 0.0, 0.0), 1.0)]);
    let image = settled(&mut pooled.bench, FRONT, AIM);
    assert!(only(middle(&image), 0), "{:?}", middle(&image));
    let given = |pooled: &mut Pooled| {
        let choice = pooled.bench.layer.choice().expect("with the pool");
        choice
            .sections()
            .iter()
            .map(|section| (section.2, section.3))
            .collect::<Vec<_>>()
    };
    assert_eq!(given(&mut pooled), [(0, 1)]);
    // Both in sight, both given.
    pooled
        .bench
        .service
        .place("aside", &[instance(1, 0, Vec3::new(-2.0, 0.5, 0.0), 1.0)]);
    render(&mut pooled.bench, FRONT, AIM);
    assert_eq!(given(&mut pooled), [(0, 1), (1, 1)]);
}

#[test]
fn a_level_is_forgotten_while_its_instance_is_out_of_sight() {
    let fake = Fake {
        model: Some(levelled((PLAIN, 0))),
        textures: levels_textures(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    assert!(pooled.add(&fake, &look("square.m2")));
    let bench = &mut pooled.bench;
    for _ in 0..2 {
        bench.service.place("test", &[instance(1, 0, at_ratio(42.0), 1.0)]);
        assert_eq!(level_seen(middle(&render(bench, FRONT, AIM))), 1);
    }
    // Beside the view a frame, in the same tile: its group left out by the CPU, its level
    // forgotten.
    let aside = at_ratio(42.0) + Vec3::new(0.0, 200.0, 0.0);
    bench.service.place("test", &[instance(1, 0, aside, 1.0)]);
    render(bench, FRONT, AIM);
    bench.service.place("test", &[instance(1, 0, at_ratio(38.0), 1.0)]);
    assert_eq!(level_seen(middle(&render(bench, FRONT, AIM))), 0, "chosen afresh");
}

#[test]
fn a_template_is_kept_only_at_the_level_the_gpu_chose() {
    let fake = Fake {
        model: Some(levelled((PLAIN, 2))),
        textures: levels_textures(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    assert!(pooled.add(&fake, &look("square.m2")));
    for (ratio, level) in [(60.0, 1), (25.0, 0)] {
        pooled
            .bench
            .service
            .place("test", &[instance(1, 0, at_ratio(ratio), 1.0)]);
        let seen = middle(&render(&mut pooled.bench, FRONT, AIM));
        assert_eq!(level_seen(seen), level, "at {ratio} radii");
    }
}

#[test]
fn what_the_first_phase_drew_is_not_drawn_again_and_what_the_second_found_is_drawn_in_its_place() {
    let fake = Fake {
        model: Some({
            let mut model = square(0, 0);
            model.textures[0].source = ModelTextureSource::Filled(11);
            model
        }),
        textures: colours(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    for file in ["red.blp", "green.blp"] {
        assert!(pooled.add(&fake, &skin(file)));
    }
    let bench = &mut pooled.bench;
    bench
        .service
        .place("left", &[instance(1, 0, Vec3::new(0.0, -1.5, 0.0), 0.5)]);
    render(bench, FRONT, AIM);
    // The red seen at the frame before, drawn by the first phase; the green new, by the second.
    bench
        .service
        .place("right", &[instance(1, 1, Vec3::new(0.0, 1.5, 0.0), 0.5)]);
    let image = render(bench, FRONT, AIM);
    for ((row, column), channel) in [(LEFT, 0), (RIGHT, 1)] {
        let seen = pixel(&image, row, column);
        assert!(only(seen, channel), "{seen:?} at column {column}");
    }
    // Both seen since: drawn by the first phase, once each.
    for _ in 0..4 {
        render(bench, FRONT, AIM);
    }
    let items = bench.layer.stats().items;
    assert!(items.contains("chosen by the GPU: 2 pairs"), "{items}");
}

#[test]
fn an_instance_is_tested_against_the_depth_where_it_falls_in_the_view() {
    let fake = Fake {
        model: Some({
            let mut model = square(0, 0);
            model.textures[0].source = ModelTextureSource::Filled(11);
            model
        }),
        textures: colours(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    for file in ["red.blp", "green.blp"] {
        assert!(pooled.add(&fake, &skin(file)));
    }
    let bench = &mut pooled.bench;
    let far = depth_at(3.0);
    // Left and right: the wall over the left half only.
    bench.service.place(
        "test",
        &[
            instance(1, 0, Vec3::new(0.0, -1.5, 0.0), 0.5),
            instance(2, 1, Vec3::new(0.0, 1.5, 0.0), 0.5),
        ],
    );
    bench.wall = [[far, 0.0], [far, 0.0]];
    let image = render(bench, FRONT, AIM);
    assert_eq!(pixel(&image, LEFT.0, LEFT.1), [0, 0, 0, 255], "hidden on the left");
    assert!(only(pixel(&image, RIGHT.0, RIGHT.1), 1), "seen on the right");
    // Above and below: the wall over the upper half only.
    bench.service.place(
        "test",
        &[
            instance(3, 0, Vec3::new(0.0, 0.0, 2.0), 0.5),
            instance(4, 1, Vec3::new(0.0, 0.0, -1.0), 0.5),
        ],
    );
    bench.wall = [[far, far], [0.0, 0.0]];
    let image = render(bench, FRONT, AIM);
    let coloured = |rows: std::ops::Range<usize>, channel: usize| {
        rows.flat_map(|row| (0..32).map(move |column| (row, column)))
            .any(|(row, column)| only(pixel(&image, row, column), channel))
    };
    assert!(!coloured(0..16, 0), "hidden above");
    assert!(coloured(16..32, 1), "seen below");
}

#[test]
fn the_draws_of_a_state_are_packed_in_the_order_of_their_records_and_counted() {
    let fake = Fake {
        model: Some({
            let mut model = square(0, 0);
            model.textures[0].source = ModelTextureSource::Filled(11);
            model
        }),
        textures: colours(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    let features = pooled.bench.gpu.device.features();
    if !features.contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT) {
        return;
    }
    for file in ["red.blp", "green.blp", "blue.blp"] {
        assert!(pooled.add(&fake, &skin(file)));
    }
    // An instance of the first look, none of the second, two of the third.
    pooled.bench.service.place(
        "test",
        &[
            instance(1, 0, Vec3::ZERO, 1.0),
            instance(2, 2, Vec3::new(0.0, 1.0, 0.0), 1.0),
            instance(3, 2, Vec3::new(0.0, -1.0, 0.0), 1.0),
        ],
    );
    for packed in [false, true] {
        render(&mut pooled.bench, FRONT, AIM);
        pooled.bench.layer.choice().expect("with the pool").packed = packed;
        revealed(&mut pooled.bench);
        let gpu = pooled.bench.gpu.clone();
        let (args, counts) = pooled.bench.layer.choice().expect("with the pool").written(&gpu);
        let drawn: Vec<u32> = args.chunks(5).map(|draw| draw[1]).collect();
        if packed {
            assert_eq!(
                (&drawn[..2], counts[0]),
                (&[1, 2][..], 2),
                "the draws with instances, in order"
            );
        } else {
            assert_eq!(&drawn[..3], [1, 0, 2], "every record");
        }
    }
}

#[test]
fn the_draws_are_packed_across_the_blocks_of_records() {
    // A square drawn by 120 batches, a triangle each: three looks of it hold 360 records, two
    // blocks of the prefix sum.
    let mut model = square(0, 0);
    model.textures[0].source = ModelTextureSource::Filled(11);
    let skin = &mut model.skins[0];
    let (batch, submesh) = (skin.batches[0], skin.submeshes[0]);
    skin.triangles = [0, 1, 2].repeat(120);
    skin.submeshes = (0..120u16)
        .map(|at| uniwow_api::formats::Submesh {
            start: 3 * u32::from(at),
            count: 3,
            ..submesh
        })
        .collect();
    skin.batches = (0..120u16)
        .map(|at| uniwow_api::formats::Batch { submesh: at, ..batch })
        .collect();
    let fake = Fake {
        model: Some(model),
        textures: colours(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    if !pooled
        .bench
        .gpu
        .device
        .features()
        .contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT)
    {
        return;
    }
    for file in ["red.blp", "green.blp", "blue.blp"] {
        assert!(pooled.add(&fake, &crate::pool_tests::skin(file)));
    }
    pooled.bench.service.place(
        "test",
        &[
            instance(1, 0, Vec3::ZERO, 1.0),
            instance(2, 2, Vec3::new(0.0, 1.0, 0.0), 1.0),
            instance(3, 2, Vec3::new(0.0, -1.0, 0.0), 1.0),
        ],
    );
    render(&mut pooled.bench, FRONT, AIM);
    pooled.bench.layer.choice().expect("with the pool").packed = true;
    revealed(&mut pooled.bench);
    let gpu = pooled.bench.gpu.clone();
    let (args, counts) = pooled.bench.layer.choice().expect("with the pool").written(&gpu);
    let draws: Vec<&[u32]> = args.chunks(5).take(240).collect();
    assert_eq!(counts[0], 240, "the draws of the first and the third look");
    assert!(draws[..120].iter().all(|draw| draw[1] == 1), "the first look's");
    assert!(
        draws[120..].iter().all(|draw| draw[1] == 2),
        "the third look's, after it"
    );
    // Their instances placed one after another, across the blocks.
    let firsts: Vec<u32> = draws.iter().map(|draw| draw[4]).collect();
    let wanted: Vec<u32> = (0..120).chain((0..120).map(|at| 120 + 2 * at)).collect();
    assert_eq!(firsts, wanted);
}
