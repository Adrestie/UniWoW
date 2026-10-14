//! Tests of the models of the sky (step 9.7e), drawn on the software adapter of the system when it
//! has one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uniwow_api::formats::{FileRef, Model, ModelTextureSource};
use uniwow_api::glam::Vec3;
use uniwow_api::models::{Geosets, Look, Models};
use uniwow_api::viewport::{Fog, Layer, Stage, Sun};

use crate::gpu::Shared;
use crate::layer::ModelsLayer;
use crate::loading::{Caches, Ready};
use crate::pool;
use crate::service::Service;
use crate::sky::SkyLayer;
use crate::tests::{
    AIM, Bench, FRONT, Fake, device, flat, instance, middle, plain, render, render_with, scene, square,
};

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];

/// A model of the sky: the square of the tests, unlit, 40 yards wide and high, 20 yards towards −X
/// from its origin, facing it, textured `blue.blp`.
fn sky_model() -> Model {
    let mut model = square(0x1, 0);
    for vertex in &mut model.vertices {
        let [_, y, z] = vertex.position;
        vertex.position = [-20.0, 20.0 * y, 20.0 * (z - 1.0)];
    }
    model.textures[0].source = ModelTextureSource::File(FileRef::Path("blue.blp".to_owned()));
    model.radius = 30.0;
    model
}

/// The bench of the tests with three looks: the square, red and lit (look 0), the model of the
/// sky (look 1) and the same drawn as the stars (look 2); with the pool when `pooled` says; and the
/// layer of the other models of the sky.
fn bench(pooled: bool) -> Option<(Bench, SkyLayer)> {
    let gpu = device()?;
    let shared = Arc::new(Shared::new(&gpu, &crate::tests::TARGET, pooled.then_some(pool::SLOTS)));
    let service = Arc::new(Service::default());
    let _ = service.gpu.set((gpu.device.clone(), gpu.queue.clone()));
    let fake = Fake {
        model: Some(square(0, 0)),
        files: HashMap::from([("sky.m2".to_owned(), sky_model())]),
        textures: HashMap::from([("red.blp".to_owned(), plain(RED)), ("blue.blp".to_owned(), plain(BLUE))]),
        ..Fake::default()
    };
    let looks = [
        ("square.m2", Geosets::All),
        ("sky.m2", Geosets::Sky),
        ("sky.m2", Geosets::Stars),
    ]
    .map(|(model, geosets)| Look {
        model: FileRef::Path(model.to_owned()),
        textures: Vec::new(),
        geosets,
    });
    let mut ready = HashMap::new();
    for look in &looks {
        let id = service.look(look);
        let mut refused = Vec::new();
        let made = crate::load(&shared, &fake, &Caches::default(), look, &mut refused).unwrap();
        assert!(refused.is_empty(), "{refused:?}");
        ready.insert(id, Arc::new(made));
    }
    for (look, stars) in [(&looks[1], false), (&looks[2], true)] {
        let sky = &ready[&service.look(look)];
        assert!(sky.sky(), "of its own, whether the pool is there or not");
        assert_eq!(sky.stars(), stars);
        let Ready::Own(own) = &**sky else {
            unreachable!("a model of the sky");
        };
        assert!(
            own.skins[0]
                .iter()
                .all(|batch| batch.state.sky && !batch.state.depth_write)
        );
    }
    assert!(!ready[&service.look(&looks[0])].sky());
    let scene = Arc::new(Mutex::new(scene(&gpu.device, ready)));
    let layer = ModelsLayer::new(service.clone(), scene.clone(), Arc::new(Mutex::new(Some(shared))));
    let sky = layer.sky_layer(false);
    Some((
        Bench {
            gpu,
            service,
            layer,
            scene,
            wall: flat(0.0),
            sun: Sun::default(),
            fog: Fog::default(),
        },
        sky,
    ))
}

/// The pixel at the top left corner of an image of `render`.
fn corner(pixels: &[u8]) -> [u8; 4] {
    [pixels[0], pixels[1], pixels[2], pixels[3]]
}

#[test]
fn a_model_of_the_sky_is_drawn_around_the_eye_behind_everything_unfogged() {
    for pooled in [true, false] {
        let Some((mut bench, mut sky)) = bench(pooled) else {
            return;
        };
        let render_sky = |bench: &mut Bench, sky: &mut SkyLayer| {
            render_with(bench, FRONT, AIM, Some(sky as &mut dyn Layer));
            render_with(bench, FRONT, AIM, Some(sky as &mut dyn Layer))
        };
        // Its owner places it at the eye: drawn all over, by the layer of the sky alone.
        bench.service.place("lighting", &[instance(1, 1, FRONT, 1.0)]);
        let alone = render_sky(&mut bench, &mut sky);
        assert_eq!((middle(&alone), corner(&alone)), (BLUE, BLUE), "pooled {pooled}");
        assert_eq!(
            middle(&render(&mut bench, FRONT, AIM)),
            BLACK,
            "not by the layer of the models"
        );
        let stats = sky.stats();
        assert_eq!((stats.draws, stats.triangles), (1, 2));
        // Around the eye wherever its instance stands: the same image from a little apart.
        bench
            .service
            .place("lighting", &[instance(1, 1, FRONT + Vec3::new(3.0, -2.0, 1.0), 1.0)]);
        assert_eq!(render_sky(&mut bench, &mut sky), alone);
        // Behind the square before it, which hides it in the middle, around it still.
        bench.service.place("test", &[instance(2, 0, Vec3::ZERO, 1.0)]);
        let behind = render_sky(&mut bench, &mut sky);
        let seen = middle(&behind);
        assert!(seen[0] > 100 && seen[2] < 30, "the square: {seen:?}");
        assert_eq!(corner(&behind), BLUE);
        // Never fogged, nor cut by the end of the fog of the game, even nearer than all it shows;
        // the square beyond that end not drawn, the sky seen in its place.
        bench.fog = Fog {
            colour: [0.0, 1.0, 0.0],
            start: 0.01,
            middle: 0.02,
            end: 0.05,
            rate: 2.0,
        };
        let fogged = render_sky(&mut bench, &mut sky);
        assert_eq!((middle(&fogged), corner(&fogged)), (BLUE, BLUE));
        // Taken away: nothing drawn.
        bench.fog = Fog::default();
        bench.service.place("lighting", &[]);
        bench.service.place("test", &[]);
        assert_eq!(corner(&render_sky(&mut bench, &mut sky)), BLACK);
    }
}

#[test]
fn the_stars_are_drawn_by_a_layer_of_their_own_before_the_bodies_the_other_models_after() {
    for pooled in [true, false] {
        let Some((mut bench, mut sky)) = bench(pooled) else {
            return;
        };
        let mut stars = bench.layer.sky_layer(true);
        assert_eq!((stars.stage(), sky.stage()), (Stage::Stars, Stage::Sky));
        let by = |bench: &mut Bench, layer: &mut SkyLayer| {
            render_with(bench, FRONT, AIM, Some(layer as &mut dyn Layer));
            render_with(bench, FRONT, AIM, Some(layer as &mut dyn Layer))
        };
        // The model of the stars, by the layer of the stars alone.
        bench.service.place("lighting", &[instance(1, 2, FRONT, 1.0)]);
        assert_eq!(corner(&by(&mut bench, &mut stars)), BLUE, "pooled {pooled}");
        assert_eq!(stars.stats().items, "1 models of the stars");
        assert_eq!(corner(&by(&mut bench, &mut sky)), BLACK);
        // A model of the sky, by the other alone.
        bench.service.place("lighting", &[instance(1, 1, FRONT, 1.0)]);
        assert_eq!(corner(&by(&mut bench, &mut sky)), BLUE);
        assert_eq!(corner(&by(&mut bench, &mut stars)), BLACK);
    }
}
