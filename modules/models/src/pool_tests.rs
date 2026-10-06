//! Tests of the pool: its arenas, and the models drawn from it by a few commands, the looks it has
//! no room for drawn on the path of step 9.4c; on the software adapter of the system when it has
//! one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use uniwow_api::bytemuck;
use uniwow_api::formats::{FileRef, Model, ModelTextureSource, Texture, TextureFormat};
use uniwow_api::glam::Vec3;
use uniwow_api::models::{Geosets, Look, Models};
use uniwow_api::viewport::{Drawing, Layer};
use uniwow_api::wgpu;

use crate::arena::{Arena, Holes};
use crate::choice::Tables;
use crate::gpu::Shared;
use crate::layer::{ModelsLayer, Scene};
use crate::loading::{Caches, Ready};
use crate::lock;
use crate::pool;
use crate::service::Service;
use crate::tests::{AIM, Bench, FRONT, Fake, TARGET, device, instance, plain, read_back, render, settled, square};

#[test]
fn a_range_is_taken_from_the_first_hole_holding_it_and_given_back_merged() {
    let mut holes = Holes::default();
    assert_eq!(holes.take(1), None, "nothing before the arena grows");
    holes.grow(10);
    assert_eq!(holes.take(4), Some(0..4));
    assert_eq!(holes.take(3), Some(4..7));
    assert_eq!(holes.take(5), None, "3 left");
    holes.give(0..4);
    assert_eq!(holes.take(5), None, "two holes, of 4 and 3, apart");
    holes.give(4..7);
    assert_eq!(holes.take(10), Some(0..10), "merged with both its neighbours");
    holes.give(0..10);
    holes.grow(16);
    assert_eq!(
        holes.take(16),
        Some(0..16),
        "the units added joined to the hole before them"
    );
}

#[test]
fn an_arena_keeps_its_ranges_in_place_when_it_grows_and_takes_back_those_given() {
    let Some(gpu) = device() else {
        return;
    };
    let arena = Arena::new(&gpu.device, &gpu.queue, "test arena", wgpu::BufferUsages::STORAGE, 4, 4);
    let first = arena.put(bytemuck::cast_slice(&[1u32, 2, 3, 4])).unwrap();
    let second = arena.put(bytemuck::cast_slice(&[5u32, 6])).unwrap();
    assert_eq!((first.clone(), second), (0..4, 4..6));
    let (buffer, generation) = arena.buffer().unwrap();
    assert_eq!(generation, 2, "grown once");
    let read: Vec<u32> = bytemuck::cast_slice(&read_back(&gpu, &buffer, 24)).to_vec();
    assert_eq!(
        read,
        [1, 2, 3, 4, 5, 6],
        "the first range copied into the larger buffer"
    );
    assert_eq!(arena.bytes(), (32, 24));
    arena.give(first);
    assert_eq!(
        arena.put(bytemuck::cast_slice(&[7u32])).unwrap(),
        0..1,
        "a range given back taken again"
    );
}

#[test]
fn released_looks_give_their_ranges_back_and_their_textures_when_purged() {
    let Some(gpu) = device() else {
        return;
    };
    let shared = Shared::new(&gpu, &TARGET, Some(pool::SLOTS));
    let pool = shared.pool.clone().unwrap();
    let fake = Fake {
        model: Some(square(0, 0)),
        textures: HashMap::from([("red.blp".to_owned(), plain([255, 0, 0, 255]))]),
        ..Fake::default()
    };
    let caches = Caches::default();
    let look = |geosets| Look {
        model: FileRef::Path("square.m2".to_owned()),
        textures: Vec::new(),
        geosets,
    };
    let first = crate::load(&shared, &fake, &caches, &look(Geosets::All), &mut Vec::new()).unwrap();
    assert_eq!(
        lock(&pool.pipelines).len(),
        1,
        "the pipeline of its batch made by its load"
    );
    let second = crate::load(&shared, &fake, &caches, &look(Geosets::Default), &mut Vec::new()).unwrap();
    // Four vertices of 48 bytes and six indices, shared; a material each.
    let model = 4 * 48 + 6 * 4;
    assert_eq!(pool.arenas().1, model + 2 * 96);
    drop(first);
    assert_eq!(pool.arenas().1, model + 96, "its material");
    drop(second);
    assert_eq!(pool.arenas().1, 0, "the model with its last look");
    assert_eq!(
        pool.arrays.counts().layers,
        1,
        "the texture until the arrays are purged"
    );
    pool.arrays.purge();
    assert_eq!(pool.arrays.counts().layers, 0);
}

#[test]
fn the_frame_reads_an_arena_while_a_job_holds_its_holes() {
    let Some(gpu) = device() else {
        return;
    };
    let arena = Arena::new(&gpu.device, &gpu.queue, "test arena", wgpu::BufferUsages::STORAGE, 4, 4);
    arena.put(bytemuck::cast_slice(&[1u32, 2, 3, 4])).unwrap();
    let (held, holding) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (read, reading) = mpsc::channel();
    std::thread::scope(|scope| {
        // A job taking a range or growing the buffer, which can take milliseconds.
        let arena = &arena;
        scope.spawn(move || {
            let _holes = lock(&arena.holes);
            held.send(()).unwrap();
            let _ = released.recv();
        });
        holding.recv().unwrap();
        scope.spawn(move || read.send((arena.bytes(), arena.buffer().map(|(_, generation)| generation))));
        let seen = reading.recv_timeout(Duration::from_secs(5)).ok();
        release.send(()).unwrap();
        assert_eq!(seen, Some(((16, 16), Some(1))), "read without waiting for the job");
    });
}

/// A texture of `side` × `side` texels of `colour`.
pub(crate) fn sized(side: u32, colour: [u8; 4]) -> Texture {
    Texture {
        width: side,
        height: side,
        format: TextureFormat::Rgba8,
        levels: vec![colour.repeat((side * side) as usize)],
    }
}

/// A bench of a layer drawing with the pool, and what its module would make the looks with.
pub(crate) struct Pooled {
    pub(crate) bench: Bench,
    pub(crate) shared: Arc<Shared>,
    caches: Caches,
}

impl Pooled {
    /// The bench of a pool of `slots` arrays, its scene empty.
    pub(crate) fn new(slots: usize) -> Option<Self> {
        let gpu = device()?;
        let shared = Arc::new(Shared::new(&gpu, &TARGET, Some(slots)));
        let service = Arc::new(Service::default());
        let _ = service.gpu.set((gpu.device.clone(), gpu.queue.clone()));
        let scene = Arc::new(Mutex::new(Scene {
            reach: 100.0,
            ..Scene::default()
        }));
        let layer = ModelsLayer::new(
            service.clone(),
            scene.clone(),
            Arc::new(Mutex::new(Some(shared.clone()))),
        );
        Some(Self {
            bench: Bench {
                gpu,
                service,
                layer,
                scene,
            },
            shared,
            caches: Caches::default(),
        })
    }

    /// `look` of `fake` made ready as the module makes it, added to the scene; whether from the
    /// pool.
    pub(crate) fn add(&self, fake: &Fake, look: &Look) -> bool {
        let id = self.bench.service.look(look);
        let mut refused = Vec::new();
        let made = crate::load(&self.shared, fake, &self.caches, look, &mut refused).unwrap();
        assert!(refused.is_empty(), "{refused:?}");
        let pooled = matches!(made, Ready::Pooled(_));
        let mut scene = self.bench.scene.lock().unwrap();
        let mut looks = (*scene.looks).clone();
        looks.insert(id, Arc::new(made));
        scene.looks = Arc::new(looks);
        scene.generation += 1;
        scene.tables = Some(Arc::new(Tables::new(
            &self.bench.gpu.device,
            scene.generation,
            &scene.looks,
        )));
        pooled
    }
}

/// The pixel at `row` and `column` of an image of `render`.
pub(crate) fn pixel(pixels: &[u8], row: usize, column: usize) -> [u8; 4] {
    let at = row * 256 + column * 4;
    [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
}

/// Whether `pixel` is lit of its colour `channel` alone: 0 red, 1 green, 2 blue.
pub(crate) fn only(pixel: [u8; 4], channel: usize) -> bool {
    (0..3).all(|other| {
        if other == channel {
            pixel[other] > 100
        } else {
            pixel[other] < 30
        }
    })
}

/// Where a square of scale 0.5 stands at y = -1.5, 0 and 1.5 in an image of `render`, on its
/// middle row.
pub(crate) const LEFT: (usize, usize) = (18, 8);
pub(crate) const MIDDLE: (usize, usize) = (18, 16);
pub(crate) const RIGHT: (usize, usize) = (18, 24);

/// The square of `square`, its texture given by each look, as the skin of a creature.
fn skinned() -> Model {
    let mut model = square(0, 0);
    model.textures[0].source = ModelTextureSource::Filled(11);
    model
}

/// A look of `skinned` with `file` for its skin.
pub(crate) fn skin(file: &str) -> Look {
    Look {
        model: FileRef::Path("square.m2".to_owned()),
        textures: vec![(11, FileRef::Path(file.to_owned()))],
        geosets: Geosets::All,
    }
}

/// Textures of 4 × 4 texels, red, green and blue.
pub(crate) fn colours() -> HashMap<String, Texture> {
    [
        ("red.blp", [255, 0, 0, 255]),
        ("green.blp", [0, 255, 0, 255]),
        ("blue.blp", [0, 0, 255, 255]),
    ]
    .into_iter()
    .map(|(name, colour)| (name.to_owned(), plain(colour)))
    .collect()
}

#[test]
fn the_looks_of_one_model_are_drawn_by_one_command_their_instances_from_every_owner() {
    let fake = Fake {
        model: Some(skinned()),
        textures: colours(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    // Three looks of one model, as the NPCs of one body differ: here by their skins, layers of
    // one array, each read by its material.
    for file in ["red.blp", "green.blp", "blue.blp"] {
        assert!(pooled.add(&fake, &skin(file)));
    }
    let bench = &mut pooled.bench;
    // Two owners: their instances copied into the buffer of the frame, each at its base.
    bench
        .service
        .place("left", &[instance(1, 0, Vec3::new(0.0, -1.5, 0.0), 0.5)]);
    bench.service.place(
        "right",
        &[
            instance(1, 1, Vec3::new(0.0, 1.5, 0.0), 0.5),
            instance(2, 2, Vec3::ZERO, 0.5),
        ],
    );
    let image = settled(bench, FRONT, AIM);
    assert_eq!(bench.layer.drawing(), Drawing::Pass);
    for ((row, column), channel) in [(LEFT, 0), (RIGHT, 1), (MIDDLE, 2)] {
        let seen = pixel(&image, row, column);
        assert!(only(seen, channel), "{seen:?} at column {column}");
    }
    let stats = bench.layer.stats();
    assert_eq!(stats.draws, 3, "a draw a batch and a group");
    assert!(stats.items.contains("1 commands in the pass"), "{}", stats.items);
}

/// A square beside that of `square`, from y = 1 to 3, of its upper left triangle only, green.
fn beside() -> Model {
    let mut model = square(0, 0);
    for vertex in &mut model.vertices {
        vertex.position[1] += 2.0;
    }
    model.textures[0].source = ModelTextureSource::File(FileRef::Path("green.blp".to_owned()));
    model.skins[0].triangles = vec![0, 2, 3];
    model.skins[0].submeshes[0].count = 3;
    model.bounds = [[0.0, 1.0, 0.0], [0.0, 3.0, 2.0]];
    model
}

#[test]
fn two_models_share_the_arenas_each_drawn_from_its_own_ranges() {
    let fake = Fake {
        files: HashMap::from([
            ("square.m2".to_owned(), square(0, 0)),
            ("beside.m2".to_owned(), beside()),
        ]),
        textures: colours(),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    for file in ["square.m2", "beside.m2"] {
        let look = Look {
            model: FileRef::Path(file.to_owned()),
            textures: Vec::new(),
            geosets: Geosets::All,
        };
        assert!(pooled.add(&fake, &look));
    }
    let bench = &mut pooled.bench;
    bench.service.place(
        "test",
        &[instance(1, 0, Vec3::ZERO, 0.5), instance(2, 1, Vec3::ZERO, 0.5)],
    );
    let image = render(bench, FRONT, AIM);
    // The second after the first in both arenas: its vertices and its triangle, not the first's.
    assert!(only(pixel(&image, 18, 16), 0), "{:?}", pixel(&image, 18, 16));
    assert!(
        only(pixel(&image, 17, 20), 1),
        "its triangle: {:?}",
        pixel(&image, 17, 20)
    );
    assert_eq!(pixel(&image, 20, 23), [0, 0, 0, 255], "the other half of its square");
    assert!(bench.layer.stats().items.contains("1 commands in the pass"));
}

#[test]
fn a_look_made_after_the_first_frame_is_drawn_from_its_new_array_and_larger_buffers() {
    let fake = Fake {
        model: Some(skinned()),
        textures: HashMap::from([
            ("small.blp".to_owned(), sized(4, [255, 0, 0, 255])),
            ("large.blp".to_owned(), sized(8, [0, 255, 0, 255])),
        ]),
        ..Fake::default()
    };
    let Some(mut pooled) = Pooled::new(pool::SLOTS) else {
        return;
    };
    assert!(pooled.add(&fake, &skin("small.blp")));
    let red = instance(1, 0, Vec3::new(0.0, -1.5, 0.0), 0.5);
    pooled.bench.service.place("test", &[red]);
    let image = render(&mut pooled.bench, FRONT, AIM);
    assert!(only(pixel(&image, LEFT.0, LEFT.1), 0));
    // A look of another class: an array the bind group of the frame must bind.
    assert!(pooled.add(&fake, &skin("large.blp")));
    let green = |id, y| instance(id, 1, Vec3::new(0.0, y, 0.0), 0.5);
    pooled.bench.service.place("test", &[red, green(2, 1.5)]);
    let image = render(&mut pooled.bench, FRONT, AIM);
    assert!(
        only(pixel(&image, RIGHT.0, RIGHT.1), 1),
        "{:?}",
        pixel(&image, RIGHT.0, RIGHT.1)
    );
    // Instances enough to need a larger buffer of the frame, the two looks swapped.
    let mut placed = vec![instance(1, 0, Vec3::new(0.0, 1.5, 0.0), 0.5)];
    placed.extend((2..10).map(|id| green(id, -1.5)));
    pooled.bench.service.place("test", &placed);
    let image = render(&mut pooled.bench, FRONT, AIM);
    assert!(
        only(pixel(&image, LEFT.0, LEFT.1), 1),
        "{:?}",
        pixel(&image, LEFT.0, LEFT.1)
    );
    assert!(
        only(pixel(&image, RIGHT.0, RIGHT.1), 0),
        "{:?}",
        pixel(&image, RIGHT.0, RIGHT.1)
    );
}

#[test]
fn a_look_the_arrays_have_no_room_for_is_drawn_on_the_path_of_9_4c() {
    let fake = Fake {
        model: Some(skinned()),
        textures: HashMap::from([
            ("small.blp".to_owned(), sized(4, [255, 0, 0, 255])),
            ("large.blp".to_owned(), sized(8, [255, 0, 0, 255])),
        ]),
        ..Fake::default()
    };
    // One slot: the second class finds no room.
    let Some(mut pooled) = Pooled::new(1) else {
        return;
    };
    assert!(pooled.add(&fake, &skin("small.blp")));
    assert!(!pooled.add(&fake, &skin("large.blp")));
    let bench = &mut pooled.bench;
    bench.service.place(
        "test",
        &[
            instance(1, 0, Vec3::new(0.0, -1.5, 0.0), 0.5),
            instance(2, 1, Vec3::new(0.0, 1.5, 0.0), 0.5),
        ],
    );
    let image = render(bench, FRONT, AIM);
    assert!(only(pixel(&image, LEFT.0, LEFT.1), 0), "from the pool");
    assert!(
        only(pixel(&image, RIGHT.0, RIGHT.1), 0),
        "on the path of 9.4c, in the pass"
    );
    let stats = bench.layer.stats();
    assert!(
        stats.items.contains("2 commands in the pass"),
        "one multi-draw, one draw: {}",
        stats.items
    );
}
