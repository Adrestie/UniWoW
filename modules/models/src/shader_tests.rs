//! Tests of the shaders of WotLK chosen for the batches, on models the tests make; drawn on the
//! software adapter of the system when it has one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uniwow_api::egui_wgpu;
use uniwow_api::formats::{Batch, FileRef, Material, Model, ModelTexture, ModelTextureSource, Texture, TextureFormat};
use uniwow_api::glam::{Mat4, Vec2, Vec3};
use uniwow_api::models::{Geosets, Instance, Look, Models, Motion};
use uniwow_api::viewport::View;

use crate::gpu::{Shared, camera_values};
use crate::layer::ModelsLayer;
use crate::loading::Caches;
use crate::pool;
use crate::service::Service;
use crate::shaders::{self, Combiner, Coords};
use crate::tests::{AIM, Bench, FRONT, Fake, TARGET, device, middle, plain, render, square};

/// A model of 3.3.5a of three textures and the materials `materials` (flags, blending), its skin
/// of the batches `batches` (material, layer, texture count, texture combo, combo of coordinates,
/// shader), every one of the same weight; its combos of coordinates `uvs` (-1 the environment)
/// and its combiners `combiners`, the flag 0x08 set when it has some.
fn shaded(materials: &[(u16, u16)], batches: &[[u16; 6]], uvs: &[i16], combiners: &[u16]) -> Model {
    let mut model = square(0, 0);
    model.textures = (0..3)
        .map(|_| ModelTexture {
            source: ModelTextureSource::File(FileRef::Path("red.blp".to_owned())),
            flags: 0,
        })
        .collect();
    model.texture_combos = vec![0, 1, 2];
    model.materials = materials
        .iter()
        .map(|(flags, blending)| Material {
            flags: *flags,
            blending: *blending,
        })
        .collect();
    model.uv_combos = uvs.iter().map(|uv| *uv as u16).collect();
    model.combiner_combos = combiners.to_vec();
    if !combiners.is_empty() {
        model.flags |= 0x08;
    }
    let batch = model.skins[0].batches[0];
    model.skins[0].batches = batches
        .iter()
        .map(|[material, layer, count, combo, uv, shader]| Batch {
            material: *material,
            layer: *layer,
            texture_count: *count,
            texture_combo: *combo,
            uv_combo: *uv,
            shader: *shader,
            ..batch
        })
        .collect();
    model
}

type Chosen = Option<(Combiner, [Coords; 2], Vec<usize>)>;

fn selected(model: &Model) -> Vec<Chosen> {
    shaders::select(model, &model.skins[0])
        .into_iter()
        .map(|shader| shader.map(|shader| (shader.combiner, shader.coords, shader.textures)))
        .collect()
}

#[test]
fn a_batch_alone_takes_its_shader_from_its_blending_and_its_coordinates() {
    use Combiner::*;
    use Coords::*;
    let one = |blending, uv| selected(&shaded(&[(0, blending)], &[[0, 0, 1, 0, 0, 0]], &[uv], &[]))[0].clone();
    assert_eq!(one(0, 0), Some((Opaque, [T1, T1], vec![0])));
    assert_eq!(
        one(2, -1),
        Some((Mod, [Env, T1], vec![0])),
        "blended, on the environment"
    );
    assert_eq!(one(4, 1), Some((Mod, [T2, T1], vec![0])), "its second set");
    // Opaque, its environment is not kept: a coordinate other than the first is the second.
    assert_eq!(one(0, -1), Some((Opaque, [T2, T1], vec![0])));
}

#[test]
fn a_model_with_combiners_takes_them_for_its_textures() {
    use Combiner::*;
    use Coords::*;
    // Two textures: the first opaque, the second mod2x on the environment.
    let model = shaded(&[(0, 0)], &[[0, 0, 2, 0, 0, 0]], &[0, -1], &[0, 4]);
    assert_eq!(selected(&model), [Some((OpaqueMod2x, [T1, Env], vec![0, 1]))]);
    // Modulated then added.
    let model = shaded(&[(0, 2)], &[[0, 0, 2, 0, 0, 0]], &[0, 1], &[1, 3]);
    assert_eq!(selected(&model), [Some((ModAdd, [T1, T2], vec![0, 1]))]);
    // A pair WotLK has no shader for: that of 0x11.
    let model = shaded(&[(0, 2)], &[[0, 0, 2, 0, 0, 0]], &[0, 0], &[2, 2]);
    assert_eq!(selected(&model), [Some((ModMod, [T1, T2], vec![0, 1]))]);
    // Opaque, its first texture is opaque whatever its combiner.
    let model = shaded(&[(0, 0)], &[[0, 0, 2, 0, 0, 0]], &[0, -1], &[4, 4]);
    assert_eq!(selected(&model), [Some((OpaqueMod2x, [T1, Env], vec![0, 1]))]);
}

#[test]
fn an_added_layer_on_the_environment_is_merged_into_its_first() {
    use Combiner::*;
    use Coords::*;
    // An opaque layer, then an added one on the environment, of the same weight.
    let batches = [[0, 0, 1, 0, 0, 0], [1, 1, 1, 1, 1, 0]];
    let model = shaded(&[(0, 0), (0, 4)], &batches, &[0, -1], &[]);
    assert_eq!(selected(&model), [Some((OpaqueAddAlpha, [T1, Env], vec![0, 1])), None]);
    // A mod2x one: the shader 0xE.
    let model = shaded(&[(0, 0), (0, 6)], &batches, &[0, -1], &[]);
    assert_eq!(selected(&model), [Some((OpaqueMod2xNa, [T1, Env], vec![0, 1])), None]);
    // Then an alpha layer of its first texture: merged too, the shader with its alpha.
    let three = [[0, 0, 1, 0, 0, 0], [1, 1, 1, 1, 1, 0], [2, 2, 1, 0, 0, 0]];
    let model = shaded(&[(0, 0), (0, 4), (0, 2)], &three, &[0, -1], &[]);
    assert_eq!(
        selected(&model),
        [Some((OpaqueAddAlphaAlpha, [T1, Env], vec![0, 1])), None, None]
    );
    // Of a weight of its own, a layer stays apart.
    let mut model = shaded(&[(0, 0), (0, 4)], &batches, &[0, -1], &[]);
    model.weight_combos = vec![0, 1];
    model.weights = vec![1.0, 1.0];
    model.skins[0].batches[1].weight_combo = 1;
    assert_eq!(selected(&model).iter().filter(|shader| shader.is_some()).count(), 2);
    // On the first set, it stays apart too.
    let model = shaded(
        &[(0, 0), (0, 4)],
        &[[0, 0, 1, 0, 0, 0], [1, 1, 1, 1, 0, 0]],
        &[0, -1],
        &[],
    );
    assert_eq!(selected(&model).iter().filter(|shader| shader.is_some()).count(), 2);
}

#[test]
fn an_alpha_layer_of_the_first_texture_of_two_is_merged() {
    use Combiner::*;
    use Coords::*;
    // An opaque layer of two textures, the second mod2x on the environment, then an alpha layer of
    // its first texture.
    let batches = [[0, 0, 2, 0, 0, 0], [1, 1, 1, 0, 0, 0]];
    let model = shaded(&[(0, 0), (0, 2)], &batches, &[0, -1], &[0, 4]);
    assert_eq!(
        selected(&model),
        [Some((OpaqueMod2xNaAlpha, [T1, Env], vec![0, 1])), None]
    );
}

#[test]
fn a_shader_already_chosen_is_kept_a_later_model_and_a_shared_material_too() {
    use Combiner::*;
    use Coords::*;
    let batches = [[0, 0, 2, 0, 0, 0x8003], [1, 1, 1, 0, 0, 0x8000]];
    let model = shaded(&[(0, 2), (0, 2)], &batches, &[0, -1], &[]);
    assert_eq!(selected(&model)[0], Some((OpaqueAddAlphaAlpha, [T1, Env], vec![0, 1])));
    assert_eq!(selected(&model)[1], None);
    // A later version: its first texture alone, opaque or modulated by its blending.
    let mut later = shaded(
        &[(0, 0), (0, 2)],
        &[[0, 0, 2, 0, 0, 0x8003], [1, 0, 1, 0, 0, 5]],
        &[],
        &[],
    );
    later.version = 274;
    assert_eq!(
        selected(&later),
        [Some((Opaque, [T1, T1], vec![0])), Some((Mod, [T1, T1], vec![0]))]
    );
    // Nor are its layers merged.
    let mut later = shaded(
        &[(0, 0), (0, 4)],
        &[[0, 0, 1, 0, 0, 0], [1, 1, 1, 1, 1, 0]],
        &[0, -1],
        &[],
    );
    later.version = 274;
    assert_eq!(
        selected(&later),
        [Some((Opaque, [T1, T1], vec![0])), Some((Mod, [T1, T1], vec![1]))]
    );
    // A batch of the material of the one before takes what it became: merged as well.
    let batches = [[0, 0, 1, 0, 0, 0], [1, 1, 1, 1, 1, 0], [1, 1, 1, 1, 1, 0]];
    let model = shaded(&[(0, 0), (0, 4)], &batches, &[0, -1], &[]);
    assert_eq!(selected(&model)[2], None);
    // On a skin of one layer nothing is merged nor copied: each batch keeps its texture.
    let model = shaded(&[(0, 0)], &[[0, 0, 1, 0, 0, 0], [0, 0, 1, 1, 0, 0]], &[0], &[]);
    assert_eq!(
        selected(&model),
        [Some((Opaque, [T1, T1], vec![0])), Some((Opaque, [T1, T1], vec![1]))]
    );
}

/// Unlit and unfogged: what a batch of these flags draws is its colour as combined.
const PLAIN: u16 = 0x03;

/// `model` with the files `names` for its textures, in their order.
fn named(mut model: Model, names: &[&str]) -> Model {
    for (texture, name) in model.textures.iter_mut().zip(names) {
        texture.source = ModelTextureSource::File(FileRef::Path((*name).to_owned()));
    }
    model
}

/// A grey of `value`, opaque but where `alpha` says.
fn grey(value: u8, alpha: u8) -> Texture {
    plain([value, value, value, alpha])
}

/// The pixel at the middle of what `model` draws with `textures`, its instance turned `turn`
/// radians about the vertical, seen from `FRONT` towards `AIM`: the same, within 2, from the pool
/// and on the path of step 9.4c.
fn drawn(gpu: &egui_wgpu::RenderState, model: &Model, textures: &[(&str, Texture)], turn: f32) -> [u8; 4] {
    let pooled = drawn_on(gpu, model, textures, turn, Some(pool::SLOTS));
    let own = drawn_on(gpu, model, textures, turn, None);
    assert!(
        pooled.iter().zip(own).all(|(a, b)| a.abs_diff(b) <= 2),
        "from the pool {pooled:?}, on the path of 9.4c {own:?}"
    );
    pooled
}

/// The pixel of `drawn`, with the pool of `slots` arrays when given.
fn drawn_on(
    gpu: &egui_wgpu::RenderState,
    model: &Model,
    textures: &[(&str, Texture)],
    turn: f32,
    slots: Option<usize>,
) -> [u8; 4] {
    let shared = Arc::new(Shared::new(gpu, &TARGET, slots));
    assert_eq!(shared.pool.is_some(), slots.is_some());
    let service = Arc::new(Service::default());
    let _ = service.gpu.set((gpu.device.clone(), gpu.queue.clone()));
    let fake = Fake {
        model: Some(model.clone()),
        textures: textures
            .iter()
            .map(|(name, texture)| ((*name).to_owned(), texture.clone()))
            .collect(),
        ..Fake::default()
    };
    let look = Look {
        model: FileRef::Path("square.m2".to_owned()),
        textures: Vec::new(),
        geosets: Geosets::All,
    };
    let id = service.look(&look);
    let mut refused = Vec::new();
    let ready = crate::load(&shared, &fake, &Caches::default(), &look, &mut refused).unwrap();
    assert!(refused.is_empty(), "{refused:?}");
    let scene = Arc::new(Mutex::new(crate::tests::scene(
        &gpu.device,
        HashMap::from([(id, Arc::new(ready))]),
    )));
    let layer = ModelsLayer::new(service.clone(), scene.clone(), Arc::new(Mutex::new(Some(shared))));
    let mut bench = Bench {
        gpu: gpu.clone(),
        service,
        layer,
        scene,
        wall: crate::tests::flat(0.0),
    };
    let placed = Instance {
        id: 1,
        look: id,
        transform: Mat4::from_rotation_z(turn),
        alpha: 1.0,
        motion: Motion::Standing,
    };
    bench.service.place("test", &[placed]);
    middle(&render(&mut bench, FRONT, AIM))
}

/// Whether each colour of `pixel` is within `margin` of `wanted`.
fn near(pixel: [u8; 4], wanted: [f32; 3], margin: f32) -> bool {
    pixel
        .iter()
        .zip(wanted)
        .all(|(got, want)| (f32::from(*got) - want).abs() <= margin)
}

#[test]
fn two_textures_are_combined_in_gamma_as_their_shader_says() {
    let Some(gpu) = device() else {
        return;
    };
    let textures = [("one.blp", grey(128, 255)), ("two.blp", grey(64, 255))];
    // Opaque then mod2x: 128 times 64 doubled in gamma, 64; combined in linear it would be 137.
    let model = named(
        shaded(&[(PLAIN, 0)], &[[0, 0, 2, 0, 0, 0]], &[0, 1], &[0, 4]),
        &["one.blp", "two.blp"],
    );
    assert_eq!(
        selected(&model)[0].as_ref().map(|shader| shader.0),
        Some(Combiner::OpaqueMod2x)
    );
    let seen = drawn(&gpu, &model, &textures, 0.0);
    assert!(near(seen, [64.3; 3], 3.0), "{seen:?}");
    // Opaque then added: 128 and 64, 192.
    let model = named(
        shaded(&[(PLAIN, 0)], &[[0, 0, 2, 0, 0, 0]], &[0, 1], &[0, 3]),
        &["one.blp", "two.blp"],
    );
    let seen = drawn(&gpu, &model, &textures, 0.0);
    assert!(near(seen, [192.0; 3], 3.0), "{seen:?}");
}

#[test]
fn a_mod2x_layer_doubles_what_is_under_it_in_gamma() {
    let Some(gpu) = device() else {
        return;
    };
    // An opaque grey of 128, then on the same triangles a layer of mod2x blending, of 64 on the
    // first set, which stays apart: 128 times 64 doubled, 64 in gamma (62 as the curve of the
    // target is not quite a power of 2.2); 128 were the layer not drawn on its first.
    let batches = [[0, 0, 1, 0, 0, 0], [1, 1, 1, 1, 0, 0]];
    let model = named(
        shaded(&[(PLAIN, 0), (PLAIN, 6)], &batches, &[0], &[]),
        &["one.blp", "two.blp"],
    );
    assert!(selected(&model).iter().all(Option::is_some));
    let seen = drawn(
        &gpu,
        &model,
        &[("one.blp", grey(128, 255)), ("two.blp", grey(64, 255))],
        0.0,
    );
    assert!(near(seen, [62.0; 3], 4.0), "{seen:?}");
}

#[test]
fn an_alpha_layer_then_a_mod_layer_are_drawn_in_that_order() {
    let Some(gpu) = device() else {
        return;
    };
    // An opaque grey of 128, then a red alpha layer of half its alpha, then a mod layer of a grey
    // of 128, each apart: the pool gathers the draws by state, alpha before mod as their batches
    // come. Mixed then darkened, a red of about 100; darkened then mixed, about 190.
    let batches = [[0, 0, 1, 0, 0, 0], [1, 1, 1, 1, 0, 0], [2, 2, 1, 2, 0, 0]];
    let model = named(
        shaded(&[(PLAIN, 0), (PLAIN, 2), (PLAIN, 5)], &batches, &[0], &[]),
        &["one.blp", "two.blp", "three.blp"],
    );
    assert!(selected(&model).iter().all(Option::is_some));
    let textures = [
        ("one.blp", grey(128, 255)),
        ("two.blp", plain([255, 0, 0, 128])),
        ("three.blp", grey(128, 255)),
    ];
    let seen = drawn(&gpu, &model, &textures, 0.0);
    assert!(seen[0] > 70 && seen[0] < 130 && seen[1] < 60, "{seen:?}");
}

#[test]
fn a_texture_is_held_to_its_edge_on_the_axes_its_flags_do_not_wrap() {
    let Some(gpu) = device() else {
        return;
    };
    // The coordinates of the square doubled across: 1.09 at its middle. A texture of two columns
    // of red, then two of blue.
    let mut model = square(0, 0);
    for vertex in &mut model.vertices {
        vertex.uv[0][0] *= 2.0;
    }
    let halves = Texture {
        width: 4,
        height: 4,
        format: TextureFormat::Rgba8,
        levels: vec![
            [[255, 0, 0, 255], [255, 0, 0, 255], [0, 0, 255, 255], [0, 0, 255, 255]]
                .concat()
                .repeat(4),
        ],
    };
    let textures = [("red.blp", halves)];
    // Held, its last column; wrapped, mostly its first.
    let held = drawn(&gpu, &model, &textures, 0.0);
    assert!(held[2] > 100 && held[0] < 30, "{held:?}");
    model.textures[0].flags = 0x01;
    let wrapped = drawn(&gpu, &model, &textures, 0.0);
    assert!(wrapped[0] > 100 && wrapped[2] < 60, "{wrapped:?}");
}

#[test]
fn a_texture_held_to_its_edge_never_reads_the_other_at_a_coarse_level() {
    let Some(gpu) = device() else {
        return;
    };
    // As the hide of the Orc Tent: alpha keyed, held to its edges, opaque at the top and clear at
    // the bottom, its levels made down to one texel; the coordinates moved up so that the middle
    // reads just past the top edge, at a level of about 6 texels a pixel.
    let mut model = shaded(&[(PLAIN, 1)], &[[0, 0, 1, 0, 0, 0]], &[0], &[]);
    for vertex in &mut model.vertices {
        vertex.uv[0][1] -= 0.57;
    }
    let side = 64usize;
    let levels = (0..7)
        .map(|level| {
            let side = side >> level;
            let opaque = [255u8, 0, 0, 255].repeat(side * side.div_ceil(2));
            let clear = [0u8; 4].repeat(side * (side / 2));
            [opaque, clear].concat()
        })
        .collect();
    let hide = Texture {
        width: side as u32,
        height: side as u32,
        format: TextureFormat::Rgba8,
        levels,
    };
    let seen = drawn(&gpu, &model, &[("red.blp", hide)], 0.0);
    assert!(seen[0] > 100, "the top edge, not mixed with the bottom one: {seen:?}");
}

#[test]
fn the_second_set_and_the_environment_give_their_coordinates() {
    let Some(gpu) = device() else {
        return;
    };
    // The second texture on the second set, which points every vertex at the first texel of a
    // texture green there and blue elsewhere; the first set would read blue.
    let mut model = named(
        shaded(&[(PLAIN, 0)], &[[0, 0, 2, 0, 0, 0]], &[0, 1], &[0, 1]),
        &["white.blp", "patch.blp"],
    );
    for vertex in &mut model.vertices {
        vertex.uv[1] = [0.125, 0.125];
    }
    let mut texels = [0u8, 0, 255, 255].repeat(16);
    texels[..4].copy_from_slice(&[0, 255, 0, 255]);
    let patch = Texture {
        width: 4,
        height: 4,
        format: TextureFormat::Rgba8,
        levels: vec![texels],
    };
    let seen = drawn(
        &gpu,
        &model,
        &[("white.blp", grey(255, 255)), ("patch.blp", patch)],
        0.0,
    );
    assert!(near(seen, [0.0, 255.0, 0.0], 3.0), "{seen:?}");
    // A blended texture on the environment, turned 20 degrees: the texture gives its coordinates
    // as its colour, which are those of WotLK's sphere map at the vertices.
    let model = named(shaded(&[(PLAIN, 2)], &[[0, 0, 1, 0, 0, 0]], &[-1], &[]), &["map.blp"]);
    let size = 64;
    let at = |index: usize| ((index as f32 + 0.5) / size as f32 * 255.0).round() as u8;
    let mut texels = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            texels.extend_from_slice(&[at(x), at(y), 0, 255]);
        }
    }
    let map = Texture {
        width: size as u32,
        height: size as u32,
        format: TextureFormat::Rgba8,
        levels: vec![texels],
    };
    let turn = 20f32.to_radians();
    let seen = drawn(&gpu, &model, &[("map.blp", map)], turn);
    // In the space of the camera as the client's: across, up, and away from the eye.
    let forward = (AIM - FRONT).normalize();
    let across = forward.cross(Vec3::Z).normalize();
    let up = across.cross(forward);
    let turned = Mat4::from_rotation_z(turn);
    let normal = turned.transform_vector3(Vec3::X);
    let sphere = |position: Vec3| {
        let towards = position - FRONT;
        let vertex = Vec3::new(towards.dot(across), towards.dot(up), towards.dot(forward));
        let normal = Vec3::new(normal.dot(across), normal.dot(up), normal.dot(forward));
        let from_eye = -vertex.normalize();
        let reflected = from_eye - normal * (2.0 * from_eye.dot(normal)) + Vec3::Z;
        reflected.normalize().truncate() * 0.5 + Vec2::splat(0.5)
    };
    // The middle of the image lies halfway between the first and third vertices, where their
    // coordinates meet halfway.
    let wanted = (sphere(turned.transform_point3(Vec3::new(0.0, -1.0, 0.0)))
        + sphere(turned.transform_point3(Vec3::new(0.0, 1.0, 2.0))))
        * 0.5
        * 255.0;
    assert!(near(seen, [wanted.x, wanted.y, 0.0], 6.0), "{seen:?} for {wanted:?}");
}

#[test]
fn a_merged_layer_shines_by_the_environment_where_its_texture_lets_it() {
    let Some(gpu) = device() else {
        return;
    };
    // The iron dwarf: an opaque layer of its skin and a reflection mod2x on the environment, then
    // an alpha layer of its skin, merged into one.
    let batches = [[0, 0, 2, 0, 0, 0], [1, 1, 1, 0, 0, 0]];
    let model = named(
        shaded(&[(PLAIN, 0), (PLAIN | 0x10, 2)], &batches, &[0, -1], &[1, 4]),
        &["skin.blp", "shine.blp"],
    );
    assert_eq!(
        selected(&model)[0].as_ref().map(|shader| shader.0),
        Some(Combiner::OpaqueMod2xNaAlpha)
    );
    let seen = |alpha: u8| {
        let textures = [("skin.blp", grey(128, alpha)), ("shine.blp", grey(64, 255))];
        drawn(&gpu, &model, &textures, 0.0)
    };
    // Where the skin is opaque, its own grey; where not, times the reflection doubled.
    let opaque = seen(255);
    assert!(near(opaque, [128.0; 3], 3.0), "{opaque:?}");
    let shining = seen(0);
    assert!(near(shining, [64.3; 3], 3.0), "{shining:?}");
}

#[test]
fn the_axes_of_the_camera_are_those_of_its_view_whatever_its_projection() {
    let view = Mat4::look_at_rh(FRONT, AIM, Vec3::Z);
    let axes = |projection: Mat4| {
        let values = camera_values(
            &View {
                view_proj: projection * view,
                view,
                eye: FRONT,
                size: [32, 32],
                time: 0.0,
                fog: Default::default(),
                sun: Default::default(),
            },
            100.0,
        );
        [40, 44, 48].map(|at| Vec3::from_slice(&values[at..at + 3]))
    };
    // Looking along -X: across +Y, up +Z, back +X, in perspective and orthographic alike.
    let wanted = [Vec3::Y, Vec3::Z, Vec3::X];
    for projection in [
        Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1),
        Mat4::orthographic_rh(-10.0, 10.0, -10.0, 10.0, 0.1, 100.0),
    ] {
        let seen = axes(projection);
        assert!(
            seen.iter().zip(wanted).all(|(axis, want)| axis.abs_diff_eq(want, 1e-6)),
            "{seen:?}"
        );
    }
}
