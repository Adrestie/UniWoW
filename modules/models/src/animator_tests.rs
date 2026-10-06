//! Tests of the thread of the animations: the sequence each motion plays, how it moves on, and the
//! bones it writes for the instances in sight, posing their vertices on the GPU.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use uniwow_api::formats::{
    Animation, Bone, FileRef, Interpolation, Keys, Model, ModelTextureSource, ModelVertex, Sequence, Track,
};
use uniwow_api::glam::{Mat4, Quat, Vec3};
use uniwow_api::models::{Geosets, Instance, Look, LookId, Models, Motion};
use uniwow_api::{JobOutcome, bytemuck};

use crate::animator::{Animator, Playing, advance, choose, facing, scaled, start, wanted};
use crate::gpu::{Vertex, moving_radius};
use crate::lock;
use crate::pool;
use crate::pool_tests::{LEFT, MIDDLE, Pooled, RIGHT, only, pixel, sized, skin};
use crate::tests::{AIM, FRONT, Fake, instance, plain, read_back, render, settled, square};

/// A sequence of the animation `id`, `duration` milliseconds long, moving at `speed`, as often as
/// `frequency` among its variations, its keys kept when `kept`.
fn sequence(id: u16, duration: u32, speed: f32, frequency: i16, kept: bool) -> Sequence {
    Sequence {
        id,
        variation: 0,
        duration,
        speed,
        flags: 0x20,
        frequency,
        replay: [0, 0],
        blend: [0, 0],
        bounds: [[0.0; 3]; 2],
        radius: 0.0,
        next: None,
        alias: None,
        kept,
    }
}

fn of(sequences: Vec<Sequence>) -> Animation {
    Animation {
        sequences,
        ..Animation::default()
    }
}

#[test]
fn a_motion_asks_for_its_animation_through_its_aliases_its_variations_and_its_fallbacks() {
    let mut walk = sequence(4, 1000, 2.5, 0x7FFF, false);
    walk.alias = Some(3);
    let animation = of(vec![
        sequence(0, 1000, 0.0, 0x7FFF, true),
        walk,
        sequence(5, 1000, 7.0, 0x7FFF, false),
        sequence(13, 1000, 2.5, 0x7FFF, true),
    ]);
    let none = HashMap::new();
    assert_eq!(choose(&animation, 0, 0, &none), Some(0));
    assert_eq!(
        choose(&animation, 4, 0, &none),
        Some(3),
        "the sequence its alias leads to"
    );
    assert_eq!(choose(&animation, 5, 0, &none), None, "its keys not kept, no fallback");
    let fallbacks = HashMap::from([(5, 4), (4, 0), (0, 147), (147, 0)]);
    assert_eq!(choose(&animation, 5, 0, &fallbacks), Some(3), "through Walk");
    assert_eq!(choose(&animation, 69, 0, &fallbacks), None, "none for it");
    // A loop of fallbacks ends.
    assert_eq!(choose(&of(Vec::new()), 0, 0, &fallbacks), None);

    // Variations by their frequency: 3 to 1; one not kept leaves its turn to one kept.
    let animation = of(vec![sequence(0, 1000, 0.0, 3, true), sequence(0, 1000, 0.0, 1, true)]);
    let picked: Vec<_> = (0..8).map(|roll| choose(&animation, 0, roll, &none)).collect();
    assert_eq!(picked, [0, 0, 0, 1, 0, 0, 0, 1].map(Some), "three in four the first");
    let animation = of(vec![sequence(0, 1000, 0.0, 3, false), sequence(0, 1000, 0.0, 1, true)]);
    assert_eq!(choose(&animation, 0, 0, &none), Some(1));
}

#[test]
fn a_motion_moving_walks_or_runs_by_the_nearer_speed_at_the_scale_of_its_instance() {
    let animation = of(vec![
        sequence(0, 1000, 0.0, 0x7FFF, true),
        sequence(4, 1000, 2.5, 0x7FFF, true),
        sequence(5, 1000, 7.0, 0x7FFF, true),
    ]);
    assert_eq!(wanted(&animation, Motion::Moving(2.5)), (4, Some(2.5)));
    assert_eq!(
        wanted(&animation, Motion::Moving(4.7)),
        (4, Some(4.7)),
        "nearer the walk"
    );
    assert_eq!(wanted(&animation, Motion::Moving(4.8)), (5, Some(4.8)));
    assert_eq!(
        wanted(&animation, Motion::Walking(8.0)),
        (4, Some(8.0)),
        "walking as its flags say"
    );
    assert_eq!(wanted(&animation, Motion::Standing), (0, None));
    // Twice as large, its model walks 2.5 yards a second at 5.
    assert_eq!(scaled(Motion::Moving(5.0), 2.0), Motion::Moving(2.5));
    assert_eq!(scaled(Motion::Walking(5.0), 2.0), Motion::Walking(2.5));
    let walker = of(vec![
        sequence(0, 1000, 0.0, 0x7FFF, true),
        sequence(4, 1000, 2.5, 0x7FFF, true),
    ]);
    assert_eq!(wanted(&walker, Motion::Moving(7.0)).0, 4, "without a run, its walk");
    let still = of(vec![sequence(0, 1000, 0.0, 0x7FFF, true)]);
    assert_eq!(
        wanted(&still, Motion::Moving(2.5)).0,
        5,
        "neither: its run, through the fallbacks"
    );
}

#[test]
fn a_sequence_plays_at_the_speed_of_its_instance_and_picks_a_variation_again_at_each_loop() {
    let animation = of(vec![
        sequence(0, 1000, 0.0, 0x7FFF, true),
        sequence(4, 1000, 2.0, 0x7FFF, true),
        sequence(4, 1000, 2.0, 0x7FFF, true),
    ]);
    let none = HashMap::new();
    let mut playing = start(LookId(0), &animation, Motion::Walking(4.0), 7, &none).unwrap();
    playing.time = 0.0;
    advance(&mut playing, &animation, Motion::Walking(4.0), 100.0, 7, &none);
    assert_eq!(playing.time, 200.0, "twice as fast as its sequence");
    advance(&mut playing, &animation, Motion::Walking(4.0), 450.0, 7, &none);
    assert_eq!((playing.time, playing.loops), (100.0, 1), "looped");
    let mut played = HashSet::new();
    for _ in 0..40 {
        advance(&mut playing, &animation, Motion::Walking(2.0), 1000.0, 7, &none);
        played.insert(playing.sequence);
    }
    assert_eq!(played, HashSet::from([1, 2]), "both variations");
    // Standing, at the pace of its sequence.
    let mut standing = start(LookId(0), &animation, Motion::Standing, 7, &none).unwrap();
    let before = standing.time;
    advance(&mut standing, &animation, Motion::Standing, 100.0, 7, &none);
    assert_eq!(standing.time, (before + 100.0) % 1000.0);
}

#[test]
fn a_new_motion_starts_its_sequence_blending_the_one_before_out_for_its_time_of_blending() {
    let mut walk = sequence(4, 1000, 2.5, 0x7FFF, true);
    walk.blend = [150, 150];
    let animation = of(vec![sequence(0, 1000, 0.0, 0x7FFF, true), walk]);
    let none = HashMap::new();
    let mut playing = start(LookId(0), &animation, Motion::Standing, 3, &none).unwrap();
    let stood = playing.time;
    advance(&mut playing, &animation, Motion::Walking(2.5), 50.0, 3, &none);
    assert_eq!(
        playing,
        Playing {
            look: LookId(0),
            wanted: 4,
            sequence: 1,
            time: 50.0,
            loops: 0,
            before: Some((0, (stood + 50.0) % 1000.0, 100.0, 150.0)),
        },
        "from its start, a third of the blending done"
    );
    advance(&mut playing, &animation, Motion::Walking(2.5), 100.0, 3, &none);
    assert_eq!(playing.before, None, "blended");
    // Without a time of blending, at once.
    advance(&mut playing, &animation, Motion::Standing, 10.0, 3, &none);
    assert_eq!((playing.sequence, playing.before), (0, None));
}

#[test]
fn the_loops_of_any_length_are_counted_at_once() {
    // A sequence of no length, held to a millisecond, played for a day and more.
    let animation = of(vec![
        sequence(0, 0, 0.0, 0x7FFF, true),
        sequence(4, 1000, 2.5, 0x7FFF, true),
    ]);
    let none = HashMap::new();
    let mut playing = start(LookId(0), &animation, Motion::Standing, 1, &none).unwrap();
    playing.time = 0.0;
    let started = std::time::Instant::now();
    advance(&mut playing, &animation, Motion::Standing, 1e8, 1, &none);
    assert_eq!((playing.loops, playing.time), (100_000_000, 0.0));
    // Walking at no speed its sequence can count.
    advance(&mut playing, &animation, Motion::Walking(f32::INFINITY), 10.0, 1, &none);
    assert_eq!(playing.time, 0.0, "started again");
    assert!(started.elapsed().as_secs_f32() < 1.0);
}

#[test]
fn instances_start_apart_the_same_at_every_run() {
    let animation = of(vec![sequence(0, 1000, 0.0, 0x7FFF, true)]);
    let none = HashMap::new();
    let times: HashSet<u32> = (0..20)
        .map(|id| start(LookId(0), &animation, Motion::Standing, id, &none).unwrap().time as u32)
        .collect();
    assert!(times.len() > 15, "{times:?}");
    assert_eq!(
        start(LookId(0), &animation, Motion::Standing, 7, &none),
        start(LookId(0), &animation, Motion::Standing, 7, &none)
    );
}

/// A track of one value held through the sequence 0.
fn held<T: Copy>(value: T) -> Track<T> {
    Track {
        interpolation: Interpolation::Linear,
        global: None,
        keys: vec![Keys {
            times: vec![0, 1000],
            values: vec![value, value],
            tangents: Vec::new(),
        }],
    }
}

fn bone(translation: Option<[f32; 3]>) -> Bone {
    Bone {
        key_bone: -1,
        flags: 0,
        parent: None,
        pivot: [0.0; 3],
        translation: translation.map(held).unwrap_or_default(),
        rotation: Track::default(),
        scale: Track::default(),
    }
}

/// The square of `square`, moved by its bone `translation` through its sequence *Stand*, whose
/// radius is 2.5; at rest walking (2.5 yards a second) and running (7).
fn moving(translation: [f32; 3]) -> Model {
    let mut model = square(0, 0);
    let mut stand = sequence(0, 1000, 0.0, 0x7FFF, true);
    stand.radius = 2.5;
    model.animation = Animation {
        sequences: vec![
            stand,
            sequence(4, 1000, 2.5, 0x7FFF, true),
            sequence(5, 1000, 7.0, 0x7FFF, true),
        ],
        bones: vec![bone(Some(translation))],
        order: vec![0],
        ..Animation::default()
    };
    model
}

fn look(file: &str) -> Look {
    Look {
        model: FileRef::Path(file.to_owned()),
        textures: Vec::new(),
        geosets: Geosets::All,
    }
}

/// A bench of the pool with the looks of `files`, by their order, and an animator.
fn bench(files: &[(&str, Model)]) -> Option<(Pooled, Animator)> {
    let fake = Fake {
        files: files
            .iter()
            .map(|(file, model)| ((*file).to_owned(), model.clone()))
            .collect(),
        textures: HashMap::from([("red.blp".to_owned(), plain([255, 0, 0, 255]))]),
        ..Fake::default()
    };
    let pooled = Pooled::new(pool::SLOTS)?;
    for (file, _) in files {
        assert!(pooled.add(&fake, &look(file)));
    }
    Some((pooled, Animator::default()))
}

/// A step of `animator` at `time` seconds over the bench.
fn step(pooled: &Pooled, animator: &mut Animator, time: f32) {
    let bench = &pooled.bench;
    animator.step(
        time,
        &bench.scene,
        &bench.service,
        None,
        &bench.gpu.device,
        &bench.gpu.queue,
    );
}

#[test]
fn the_thread_writes_the_bones_of_the_animated_instances_in_sight_and_where_they_begin() {
    let Some((mut pooled, mut animator)) = bench(&[("moving.m2", moving([0.0, 3.0, 0.0])), ("still.m2", square(0, 0))])
    else {
        return;
    };
    let bench = &mut pooled.bench;
    // The owner 0: an animated instance and a still one; the owner 1: two behind the camera, one
    // moving at 2.5 yards a second at a scale of 0.5, 5 yards of its model: nearer its run.
    let mover = Instance {
        motion: Motion::Moving(2.5),
        ..instance(4, 0, Vec3::new(61.0, 0.0, 0.0), 0.5)
    };
    bench.service.place(
        "a",
        &[
            instance(1, 0, Vec3::ZERO, 0.5),
            instance(2, 1, Vec3::new(0.0, -1.5, 0.0), 0.5),
        ],
    );
    bench
        .service
        .place("b", &[instance(3, 0, Vec3::new(60.0, 0.0, 0.0), 0.5), mover]);
    // The owner 2: one past the side of the view by a fifth, in the view widened by a quarter.
    bench
        .service
        .place("c", &[instance(5, 0, Vec3::new(-75.0, 67.0, 0.0), 0.5)]);
    render(bench, FRONT, AIM);
    step(&pooled, &mut animator, 0.0);
    let (animated, stats, radius) = {
        let scene = lock(&pooled.bench.scene);
        let radius = scene.looks.values().map(|look| look.radius()).fold(0.0, f32::max);
        (scene.animated.clone().unwrap(), scene.animation.clone(), radius)
    };
    assert_eq!(radius, 2.5, "that of its sequence");
    let owners: Vec<(u32, u32, u32)> = animated
        .owners
        .iter()
        .map(|(number, _, at, count)| (*number, *at, *count))
        .collect();
    assert_eq!(owners, [(0, 0, 2), (2, 2, 1)], "the owner 1 has none in sight");
    assert_eq!(animated.bones_at, 256);
    assert_eq!((stats.instances, stats.bones), (2, 2));
    let words: Vec<u32> = bytemuck::cast_slice(&read_back(&pooled.bench.gpu, &animated.buffer, 256 + 96)).to_vec();
    assert_eq!(
        words[..3],
        [1, 0, 2],
        "the first bone of each instance, none for the still one"
    );
    let rows: &[f32] = bytemuck::cast_slice(&words[64..]);
    let moved = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 3.0, 0.0, 0.0, 1.0, 0.0];
    assert_eq!(rows, [moved, moved].concat());
    // Out of sight, its time goes on.
    let before = animator.playing(1, 3).unwrap().time;
    step(&pooled, &mut animator, 0.1);
    assert_eq!(animator.playing(1, 3).unwrap().time, (before + 100.0) % 1000.0);
    assert!(animator.playing(0, 2).is_none(), "the still one plays nothing");
    assert_eq!(animator.playing(1, 4).unwrap().wanted, 5);
    // The fourth step writes again the buffer of the first, through the queue: the owner 2 gone.
    pooled.bench.service.place("c", &[]);
    step(&pooled, &mut animator, 0.2);
    step(&pooled, &mut animator, 0.3);
    let again = lock(&pooled.bench.scene).animated.clone().unwrap();
    assert!(Arc::ptr_eq(&again.buffer, &animated.buffer));
    let words: Vec<u32> = bytemuck::cast_slice(&read_back(&pooled.bench.gpu, &again.buffer, 256 + 48)).to_vec();
    assert_eq!(words[..3], [1, 0, 0], "the table of the owner 2 gone");
    let rows: &[f32] = bytemuck::cast_slice(&words[64..]);
    assert_eq!(rows, moved, "one bone left");
}

#[test]
fn the_vertices_of_an_animated_instance_are_posed_by_its_bones_on_the_gpu() {
    let Some((mut pooled, mut animator)) = bench(&[("moving.m2", moving([0.0, 3.0, 0.0])), ("still.m2", square(0, 0))])
    else {
        return;
    };
    let bench = &mut pooled.bench;
    // The second owner's instance after the first's in the frame; the still one above the middle.
    bench.service.place(
        "a",
        &[
            instance(1, 0, Vec3::ZERO, 0.5),
            instance(2, 1, Vec3::new(0.0, 0.0, 2.0), 0.5),
        ],
    );
    bench
        .service
        .place("b", &[instance(3, 0, Vec3::new(0.0, -3.0, 0.0), 0.5)]);
    let image = settled(bench, FRONT, AIM);
    let seen = |image: &[u8]| [LEFT, MIDDLE, RIGHT, ABOVE].map(|(row, column)| only(pixel(image, row, column), 0));
    assert_eq!(
        seen(&image),
        [false, true, false, true],
        "at rest before the thread writes"
    );
    step(&pooled, &mut animator, 0.0);
    let image = render(&mut pooled.bench, FRONT, AIM);
    assert_eq!(
        seen(&image),
        [true, false, true, true],
        "each moved 3 yards by its bone, at a scale of 0.5; the still one where it stands"
    );
    // The thread looking away from farther: none in its sight, drawn at rest when seen again.
    render(&mut pooled.bench, FRONT * 4.0, FRONT * 8.0);
    step(&pooled, &mut animator, 0.1);
    let image = render(&mut pooled.bench, FRONT, AIM);
    assert_eq!(seen(&image), [false, true, false, true]);
}

/// Where a square of scale 0.5 stands at y = 0, z = 2 in an image of `render`.
const ABOVE: (usize, usize) = (8, 16);

#[test]
fn an_owner_animated_is_drawn_as_the_thread_saw_it_until_its_next_step() {
    let Some((mut pooled, mut animator)) = bench(&[("moving.m2", moving([0.0, 3.0, 0.0]))]) else {
        return;
    };
    let bench = &mut pooled.bench;
    bench.service.place("a", &[instance(1, 0, Vec3::ZERO, 0.5)]);
    settled(bench, FRONT, AIM);
    step(&pooled, &mut animator, 0.0);
    // Another instance, in a tile before it: its layout changes, its first instance is the new one.
    let added = [
        instance(1, 0, Vec3::ZERO, 0.5),
        instance(4, 0, Vec3::new(0.0, -3.0, 0.0), 0.5),
    ];
    pooled.bench.service.place("a", &added);
    let seen = |image: &[u8]| [LEFT, MIDDLE, RIGHT].map(|(row, column)| only(pixel(image, row, column), 0));
    let image = render(&mut pooled.bench, FRONT, AIM);
    assert_eq!(
        seen(&image),
        [false, false, true],
        "the set its bones were computed for"
    );
    step(&pooled, &mut animator, 0.1);
    let image = render(&mut pooled.bench, FRONT, AIM);
    assert_eq!(seen(&image), [true, false, true], "the new set, both posed");
}

#[test]
fn a_look_of_its_own_is_posed_from_the_instances_of_the_frame() {
    let mut model = moving([0.0, 3.0, 0.0]);
    model.textures[0].source = ModelTextureSource::Filled(11);
    let fake = Fake {
        model: Some(model),
        textures: HashMap::from([
            ("small.blp".to_owned(), sized(4, [255, 0, 0, 255])),
            ("large.blp".to_owned(), sized(8, [255, 0, 0, 255])),
        ]),
        ..Fake::default()
    };
    // One slot: the second class finds no room, its look drawn on the path of 9.4c.
    let Some(mut pooled) = Pooled::new(1) else {
        return;
    };
    assert!(pooled.add(&fake, &skin("small.blp")));
    assert!(!pooled.add(&fake, &skin("large.blp")));
    let mut animator = Animator::default();
    // The look of its own in a second owner: after the first's instances in the frame.
    pooled
        .bench
        .service
        .place("a", &[instance(1, 0, Vec3::new(0.0, -3.0, 0.0), 0.5)]);
    pooled.bench.service.place("b", &[instance(2, 1, Vec3::ZERO, 0.5)]);
    let radii: Vec<f32> = lock(&pooled.bench.scene)
        .looks
        .values()
        .map(|look| look.radius())
        .collect();
    assert_eq!(radii, [2.5, 2.5], "that of their sequence, on both paths");
    let seen = |image: &[u8]| [LEFT, MIDDLE, RIGHT].map(|(row, column)| only(pixel(image, row, column), 0));
    let image = settled(&mut pooled.bench, FRONT, AIM);
    assert_eq!(seen(&image), [false, true, false], "at rest");
    step(&pooled, &mut animator, 0.0);
    let image = render(&mut pooled.bench, FRONT, AIM);
    assert_eq!(seen(&image), [true, false, true], "both posed");
}

#[test]
fn a_vertex_is_moved_by_each_of_its_bones_by_its_share_of_their_weights() {
    let mut model = moving([0.0, 3.0, 0.0]);
    model.animation.bones = vec![bone(None), bone(Some([0.0, 3.0, 0.0]))];
    model.animation.order = vec![0, 1];
    for vertex in &mut model.vertices {
        vertex.bone_indices = [0, 1, 0, 0];
        vertex.bone_weights = [50, 50, 0, 0];
    }
    let Some((mut pooled, mut animator)) = bench(&[("half.m2", model)]) else {
        return;
    };
    let bench = &mut pooled.bench;
    bench.service.place("a", &[instance(1, 0, Vec3::ZERO, 0.5)]);
    render(bench, FRONT, AIM);
    step(&pooled, &mut animator, 0.0);
    let image = render(&mut pooled.bench, FRONT, AIM);
    // Moved by half of 3 yards, its weights shared equally whatever their sum, at a scale of 0.5:
    // from y = 0.25 to 1.25.
    let row = MIDDLE.0;
    let red = [16, 20, 24].map(|column| only(pixel(&image, row, column), 0));
    assert_eq!(red, [false, true, false]);
}

#[test]
fn a_bone_past_those_of_its_model_weighs_nothing_and_a_sequence_kept_widens_the_radius() {
    let vertex = ModelVertex {
        position: [0.0; 3],
        normal: [0.0; 3],
        uv: [[0.0; 2]; 2],
        bone_weights: [128, 127, 0, 0],
        bone_indices: [1, 2, 0, 0],
    };
    let made = Vertex::of(&vertex, 2);
    assert_eq!((made.bones, made.weights), ([1, 2, 0, 0], [128, 0, 0, 0]));
    let mut model = square(0, 0);
    let mut wide = sequence(0, 1000, 0.0, 0x7FFF, true);
    wide.radius = 4.0;
    let mut wider = sequence(4, 1000, 0.0, 0x7FFF, false);
    wider.radius = 9.0;
    model.animation.sequences = vec![wide, wider];
    assert_eq!(moving_radius(&model), 4.0, "the sequence not kept is not played");
    assert_eq!(size_of::<Vertex>(), 48);
}

#[test]
fn a_billboard_faces_the_camera_whatever_the_turn_of_its_instance() {
    // An instance turned a quarter left and doubled, seen from +x: the camera behind it on its -y,
    // its left on its -x.
    let transform =
        Mat4::from_scale_rotation_translation(Vec3::splat(2.0), Quat::from_rotation_z(90f32.to_radians()), Vec3::ONE);
    let view = Mat4::look_at_rh(Vec3::new(5.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 1.0), Vec3::Z);
    let [back, left, up] = facing(transform, view);
    for (seen, wanted) in [(back, Vec3::NEG_Y), (left, Vec3::NEG_X), (up, Vec3::Z)] {
        assert!(seen.distance(wanted) < 1e-5, "{seen} for {wanted}");
    }
}

#[test]
fn an_instance_whose_look_changes_while_it_blends_starts_again_in_its_new_model() {
    // Its first model runs, then walks blending its run out; its second has no run.
    let mut first = moving([0.0, 3.0, 0.0]);
    first.animation.sequences[1].blend = [150, 150];
    first.animation.sequences[2].blend = [150, 150];
    let mut second = moving([0.0, 3.0, 0.0]);
    second.animation.sequences.truncate(2);
    let Some((mut pooled, mut animator)) = bench(&[("first.m2", first), ("second.m2", second)]) else {
        return;
    };
    let at = |look, motion| Instance {
        motion,
        ..instance(1, look, Vec3::ZERO, 0.5)
    };
    pooled.bench.service.place("a", &[at(0, Motion::Moving(3.5))]);
    render(&mut pooled.bench, FRONT, AIM);
    step(&pooled, &mut animator, 0.0);
    assert_eq!(animator.playing(0, 1).unwrap().sequence, 2, "running");
    pooled.bench.service.place("a", &[at(0, Motion::Moving(0.5))]);
    step(&pooled, &mut animator, 0.05);
    let blending = animator.playing(0, 1).unwrap();
    assert_eq!(
        (blending.sequence, blending.before.map(|before| before.0)),
        (1, Some(2))
    );
    // A morph: the same id, the second look.
    pooled.bench.service.place("a", &[at(1, Motion::Moving(0.5))]);
    step(&pooled, &mut animator, 0.1);
    let playing = animator.playing(0, 1).unwrap();
    assert_eq!(
        (playing.look, playing.sequence, playing.before),
        (LookId(1), 1, None),
        "walking in its new model"
    );
    let seen = |image: &[u8]| [LEFT, MIDDLE, RIGHT].map(|(row, column)| only(pixel(image, row, column), 0));
    assert_eq!(
        seen(&render(&mut pooled.bench, FRONT, AIM)),
        [false, true, false],
        "its walk at rest"
    );
}

#[test]
fn once_the_thread_ends_each_owner_is_drawn_from_its_last_publication() {
    let Some((mut pooled, mut animator)) = bench(&[("moving.m2", moving([0.0, 3.0, 0.0]))]) else {
        return;
    };
    pooled.bench.service.place("a", &[instance(1, 0, Vec3::ZERO, 0.5)]);
    settled(&mut pooled.bench, FRONT, AIM);
    step(&pooled, &mut animator, 0.0);
    // Regrouped after the last step of the thread.
    let added = [
        instance(1, 0, Vec3::ZERO, 0.5),
        instance(4, 0, Vec3::new(0.0, 1.5, 0.0), 0.5),
    ];
    pooled.bench.service.place("a", &added);
    let seen = |image: &[u8]| [LEFT, MIDDLE, RIGHT].map(|(row, column)| only(pixel(image, row, column), 0));
    assert_eq!(
        seen(&render(&mut pooled.bench, FRONT, AIM)),
        [false, false, true],
        "as the thread saw it, posed"
    );
    crate::animations_ended(&pooled.bench.scene, JobOutcome::Panicked("a test".to_owned()));
    assert_eq!(
        seen(&render(&mut pooled.bench, FRONT, AIM)),
        [false, true, true],
        "its last set, at rest"
    );
}

#[test]
fn an_instance_back_to_its_look_after_one_playing_nothing_starts_afresh() {
    // Its second look has no sequence for its motion, nor a fallback.
    let mut silent = moving([0.0, 3.0, 0.0]);
    silent.animation.sequences = vec![sequence(69, 1000, 0.0, 0x7FFF, true)];
    let Some((mut pooled, mut animator)) = bench(&[("moving.m2", moving([0.0, 3.0, 0.0])), ("silent.m2", silent)])
    else {
        return;
    };
    pooled.bench.service.place("a", &[instance(1, 0, Vec3::ZERO, 0.5)]);
    render(&mut pooled.bench, FRONT, AIM);
    for frame in 0..3 {
        step(&pooled, &mut animator, frame as f32 * 0.4);
    }
    pooled.bench.service.place("a", &[instance(1, 1, Vec3::ZERO, 0.5)]);
    step(&pooled, &mut animator, 1.2);
    assert_eq!(animator.playing(0, 1), None, "nothing played");
    pooled.bench.service.place("a", &[instance(1, 0, Vec3::ZERO, 0.5)]);
    step(&pooled, &mut animator, 1.6);
    let fresh = start(
        LookId(0),
        &moving([0.0; 3]).animation,
        Motion::Standing,
        1,
        &HashMap::new(),
    );
    assert_eq!(animator.playing(0, 1), fresh);
}
