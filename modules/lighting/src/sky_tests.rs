//! Tests of the sun, the moon, the stars and the clouds, by the hour, and of their layers, drawn on
//! the software adapter of the system.

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::viewport::{Drawing, Fog, Layer, Phase, Pyramid, Stage, Sun, Target, View};
use uniwow_api::{bytemuck, egui, egui_wgpu, wgpu};

use crate::clouds::{self, Field, Palette, SIDE};
use crate::heavens::{self, Body};
use crate::sky::{self, Drawn, Glare, GlareLayer, Heavens, HeavensLayer, Images};

/// The height of `towards` over the horizon, in degrees.
fn height(towards: [f32; 3]) -> f32 {
    let length = towards.iter().map(|value| value * value).sum::<f32>().sqrt();
    (towards[2] / length).clamp(-1.0, 1.0).asin().to_degrees()
}

/// The time of `hours` and `minutes`, in half-minutes.
fn at(hours: f32, minutes: f32) -> f32 {
    (hours * 60.0 + minutes) * 2.0
}

#[test]
fn the_tables_of_the_day_are_read_between_their_keys_past_the_last_towards_the_first() {
    let table = [(0.25, 10.0), (0.5, 20.0), (0.75, 10.0)];
    assert_eq!(heavens::day_table(&table, 0.25), 10.0);
    assert_eq!(heavens::day_table(&table, 0.375), 15.0);
    assert_eq!(heavens::day_table(&table, 0.5), 20.0);
    // Past the last key towards the first of the next day, before the first from the last.
    assert_eq!(heavens::day_table(&table, 0.875), 10.0);
    let wrapping = [(0.25, 0.0), (0.75, 100.0)];
    assert!((heavens::day_table(&wrapping, 0.875) - 75.0).abs() < 1e-3);
    assert!((heavens::day_table(&wrapping, 0.125) - 25.0).abs() < 1e-3);
    // Keys stored out of order are read in the order stored, as the client reads them: past 22 h 10
    // the envelope of the glow on the clouds wraps back to 1.
    assert_eq!(heavens::cloud_glow(at(22.0, 30.0)).0, 1.0);
}

#[test]
fn the_visible_sun_rises_at_5_30_stands_near_the_top_at_noon_and_sets_at_21_30_to_the_north_west() {
    assert!((height(heavens::sun_towards(at(12.0, 0.0))) - 85.0).abs() < 0.01);
    assert!((height(heavens::sun_towards(at(5.0, 30.0))) + 10.0).abs() < 0.01);
    assert!((height(heavens::sun_towards(at(21.0, 30.0))) + 10.0).abs() < 0.01);
    assert!(
        (height(heavens::sun_towards(at(2.0, 0.0))) + 10.0).abs() < 0.01,
        "under it all night"
    );
    // Halfway from noon to its setting, halfway down.
    let halfway = (0.503_472_2 + 0.895_833_3) / 2.0 * 2880.0;
    assert!((height(heavens::sun_towards(halfway)) - 37.5).abs() < 0.01);
    // The three places of the sun coa-vfog logged in a client 12340, by the share of the day.
    for (share, logged) in [
        (0.7511f32, [0.641f32, 0.641, 0.423]),
        (0.7802, [0.673, 0.673, 0.307]),
        (0.8337, [0.704, 0.704, 0.086]),
    ] {
        let sun = heavens::sun_towards(share * 2880.0);
        assert!(
            (height(sun) - height(logged)).abs() < 0.2,
            "{share}: {} against {}",
            height(sun),
            height(logged)
        );
    }
    // To the north-west: as far north as west, and so for the moon.
    let sun = heavens::sun_towards(at(17.0, 0.0));
    assert!(sun[0] > 0.1 && (sun[0] - sun[1]).abs() < 1e-6);
    let moon = heavens::moon_towards(at(0.0, 0.0));
    assert!(moon[0] > 0.1 && (moon[0] - moon[1]).abs() < 1e-6);
}

#[test]
fn the_moon_stands_55_degrees_high_at_midnight_and_under_the_horizon_from_4_to_22_h() {
    assert!((height(heavens::moon_towards(at(0.0, 0.0))) - 55.0).abs() < 0.01);
    assert!((height(heavens::moon_towards(at(23.0, 56.0))) - 55.0).abs() < 0.01);
    for hour in [4.0, 12.0, 22.0] {
        assert!(
            (height(heavens::moon_towards(at(hour, 0.0))) + 10.0).abs() < 0.01,
            "{hour}"
        );
    }
    // Rising from 22 h to 23 h 55, halfway between.
    let halfway = (0.916_667 + 0.996_528) / 2.0 * 2880.0;
    assert!((height(heavens::moon_towards(halfway)) - 22.5).abs() < 0.01);
}

#[test]
fn the_sun_is_twice_as_large_at_its_rising_and_the_moon_half_as_large_again() {
    assert_eq!(heavens::sun_side(at(6.0, 0.0)), 2.0);
    assert_eq!(heavens::sun_side(at(12.0, 0.0)), 1.0);
    assert!((heavens::sun_side(at(6.0, 22.5)) - 1.5).abs() < 1e-3);
    assert_eq!(heavens::sun_side(at(21.0, 0.0)), 2.0);
    assert_eq!(heavens::moon_side(at(1.0, 0.0)), 1.75);
    assert!((heavens::moon_side(at(4.0, 0.0)) - 2.625).abs() < 1e-3);
    assert!((heavens::moon_side(at(12.0, 0.0)) - 2.625).abs() < 1e-3);
}

#[test]
fn the_stars_are_whole_until_3_h_gone_by_4_h_30_and_back_from_22_h_30_by_bytes() {
    assert_eq!(heavens::stars(at(0.0, 0.0)), 1.0);
    assert_eq!(heavens::stars(at(3.0, 0.0)), 1.0);
    assert_eq!(heavens::stars(at(12.0, 0.0)), 0.0);
    assert_eq!(heavens::stars(at(22.0, 30.0)), 0.0);
    // Halfway out, a byte: trunc(254 × 0.5 + 1).
    assert_eq!(heavens::stars(at(3.0, 45.0)), 128.0 / 255.0);
    // Under the byte 2, none.
    assert_eq!(heavens::stars((0.1875 - 0.0625 / 200.0) * 2880.0), 2.0 / 255.0);
    assert_eq!(heavens::stars((0.1875 - 0.0625 / 400.0) * 2880.0), 0.0);
}

#[test]
fn the_glare_of_the_sun_is_of_the_day_that_of_the_moon_of_the_night() {
    let flares =
        |hours: f32, minutes: f32| heavens::flares(at(hours, minutes)).map(|flare| (flare * 1e4).round() / 1e4);
    assert_eq!(flares(6.0, 30.0), [0.0, 0.0]);
    assert_eq!(flares(7.0, 0.0), [0.5, 0.0]);
    assert_eq!(flares(12.0, 0.0), [1.0, 0.0]);
    assert_eq!(flares(21.0, 0.0), [0.0, 0.0]);
    assert_eq!(flares(0.0, 0.0), [0.0, 1.0]);
    assert_eq!(flares(1.0, 30.0), [0.0, 1.0]);
    assert_eq!(flares(2.0, 37.5), [0.0, 0.5]);
    // The glow on the clouds: the sun's from 4 h 50 to 22 h 10, its envelope out at the notch.
    assert_eq!(heavens::cloud_glow(at(12.0, 0.0)), (1.0, true));
    assert_eq!(heavens::cloud_glow(at(4.0, 45.0)), (0.0, false));
    assert!(heavens::cloud_glow(at(4.0, 51.0)).1);
    assert!(!heavens::cloud_glow(at(2.0, 0.0)).1);
}

#[test]
fn a_storm_weighs_four_times_its_intensity_and_fades_the_bodies_by_bytes() {
    assert_eq!(heavens::storm_share(0.1), 0.4);
    assert_eq!(heavens::storm_share(0.25), 1.0);
    assert_eq!(heavens::storm_share(0.8), 1.0);
    assert_eq!(heavens::storm_share(0.0), 0.0);
    assert_eq!(heavens::storm_alpha(0.0), 1.0);
    assert_eq!(heavens::storm_alpha(0.5), 127.0 / 255.0);
    assert_eq!(heavens::storm_alpha(1.0), 0.0);
    let [sun, moon] = heavens::bodies(at(12.0, 0.0), [1.0, 0.5, 0.25], 0.5);
    assert_eq!(
        (sun.colour, sun.alpha, moon.alpha),
        ([1.0, 0.5, 0.25], 127.0 / 255.0, 127.0 / 255.0)
    );
    assert_eq!(sun.side, 1.0);
    assert_eq!(moon.side, heavens::moon_side(at(12.0, 0.0)));
    assert_eq!(sun.towards, heavens::sun_towards(at(12.0, 0.0)));
}

#[test]
fn the_view_turns_onto_a_body_from_45_degrees_away() {
    let up = [0.0, 0.0, 1.0];
    assert_eq!(heavens::view_onto(up, up), 1.0);
    let away = |degrees: f32| {
        let radians = degrees.to_radians();
        [radians.sin(), 0.0, radians.cos()]
    };
    assert_eq!(heavens::view_onto(up, away(50.0)), 0.0);
    let cosine = 30f32.to_radians().cos();
    assert!((heavens::view_onto(up, away(30.0)) - (cosine - 0.7) / 0.3).abs() < 1e-5);
}

#[test]
fn the_bodies_and_the_clouds_are_drawn_but_under_a_model_of_the_sky_that_hides_them() {
    assert_eq!(heavens::share(&[], false, false), 1.0);
    assert!((heavens::share(&[(0.4, false)], false, false) - 0.6).abs() < 1e-6);
    assert!(
        (heavens::share(&[(0.4, false), (0.7, false)], false, false) - 0.3).abs() < 1e-6,
        "the strongest"
    );
    assert_eq!(
        heavens::share(&[(1.0, true)], false, false),
        1.0,
        "one that lets them be drawn"
    );
    assert_eq!(heavens::share(&[(0.995, false)], false, false), 0.0, "past 0.99");
    assert_eq!(heavens::share(&[], true, false), 0.0, "a building's own sky");
    assert_eq!(heavens::share(&[], false, true), 0.0, "in a liquid");
}

/// A direction from the eye meeting the dome of the clouds at the co-latitude `turns`, in turns of
/// half a circle, towards the north.
fn on_dome_at(turns: f32) -> [f32; 3] {
    let angle = turns * std::f32::consts::PI;
    let point = Vec3::new(angle.sin(), 0.0, angle.cos() - std::f32::consts::FRAC_PI_4.cos());
    point.normalize().to_array()
}

#[test]
fn the_dome_puts_the_top_of_the_sky_in_the_middle_of_the_tile_and_its_edge_on_the_horizon() {
    let ([u, v], alpha) = clouds::on_dome([0.0, 0.0, 1.0]);
    assert!(
        (u - 64.0).abs() < 1e-3 && (v - 64.0).abs() < 1e-3 && alpha == 1.0,
        "{u} {v} {alpha}"
    );
    // On the horizon, the edge: the ring 11, of no alpha.
    let ([u, v], alpha) = clouds::on_dome([1.0, 0.0, 0.0]);
    assert!(
        (u - (0.5 + 11.0 / 24.0) * 128.0).abs() < 1e-2 && (v - 64.0).abs() < 1e-3,
        "{u} {v}"
    );
    assert_eq!(alpha, 0.0);
    // The ninth ring whole, the tenth half, and between them halfway.
    let ([u, _], alpha) = clouds::on_dome(on_dome_at(0.205));
    assert!(
        (u - (0.5 + 8.0 / 24.0) * 128.0).abs() < 0.05 && (alpha - 1.0).abs() < 1e-3,
        "{u} {alpha}"
    );
    let (_, alpha) = clouds::on_dome(on_dome_at(0.23));
    assert!((alpha - 128.0 / 255.0).abs() < 1e-3, "{alpha}");
    let (_, alpha) = clouds::on_dome(on_dome_at(0.2175));
    assert!((alpha - (1.0 + 128.0 / 255.0) / 2.0).abs() < 1e-3, "{alpha}");
    // Towards the west, along the rows of the tile.
    let [x, y, z] = on_dome_at(0.1);
    let ([u, v], _) = clouds::on_dome([y, x, z]);
    assert!(
        (u - 64.0).abs() < 1e-3 && (v - (0.5 + 4.0 / 24.0) * 128.0).abs() < 0.05,
        "{u} {v}"
    );
    // Under the horizon, the edge still, of no alpha.
    let (_, alpha) = clouds::on_dome([1.0, 0.0, -0.2]);
    assert_eq!(alpha, 0.0);
}

fn palette() -> Palette {
    Palette {
        glow: [0.0, 1.0, 0.0],
        thin: [1.0, 0.0, 0.0],
        body: [0.0, 0.0, 0.2],
        storm: 0.0,
        towards: [0.0, 0.0, 1.0],
        envelope: 1.0,
    }
}

#[test]
fn the_clouds_cover_nothing_without_density_all_at_full_density_and_more_the_denser() {
    let covered = |density: f32| {
        let mut field = Field::default();
        assert!(field.tick(0.0, density, &palette()), "made whole the first time");
        field.cover().iter().filter(|cover| **cover > 0).count()
    };
    assert_eq!(covered(0.0), 0);
    assert_eq!(covered(1.0), SIDE * SIDE);
    let (thin, thick) = (covered(0.4), covered(0.6));
    assert!(0 < thin && thin < thick && thick < SIDE * SIDE, "{thin} {thick}");
    // At full density the least of the tile is the curve some way along it.
    let mut field = Field::default();
    field.tick(0.0, 1.0, &palette());
    let least = *field.cover().iter().min().unwrap();
    assert!(least > 0 && least < 200, "{least}");
    // The curve: 255 − 255 × 0.96^(0.6015625 i), whole numbers down.
    assert_eq!([0, 1, 16, 64, 255].map(|at| field.curve(at)), [0, 6, 82, 202, 254]);
}

#[test]
fn the_tile_is_made_again_a_band_each_tenth_of_a_second_moving_on_in_time_after_four() {
    let mut field = Field::default();
    assert!(field.tick(0.0, 0.5, &palette()));
    assert_eq!(field.step(), 1, "the whole tile made: a step on");
    let first: Vec<u8> = field.cover().to_vec();
    assert!(!field.tick(0.05, 0.5, &palette()), "not before a tenth of a second");
    assert!(field.tick(0.06, 0.5, &palette()));
    for _ in 0..2 {
        assert!(field.tick(0.1, 0.5, &palette()));
    }
    assert_eq!(field.step(), 1);
    assert!(field.tick(0.1, 0.5, &palette()));
    assert_eq!(field.step(), 2, "a step on once the whole tile was made again");
    // A step moves the clouds a little, not wholly.
    let same = first.iter().zip(field.cover()).filter(|(a, b)| a == b).count();
    assert!(same > SIDE * SIDE / 2 && same < SIDE * SIDE, "{same}");
}

#[test]
fn a_covered_texel_is_coloured_by_its_thinness_and_glows_towards_the_body() {
    let mut field = Field::default();
    field.tick(0.0, 0.6, &palette());
    let texels = field.texels();
    for (texel, cover) in texels.iter().zip(field.cover()) {
        if *cover == 0 {
            assert_eq!(texel[3], 0, "a texel not covered is of no alpha");
            continue;
        }
        assert_eq!(texel[3], *cover);
        let thinness = f32::from(((255 - cover) >> 1) + 64) / 255.0;
        assert_eq!(texel[0], (thinness * 255.0) as u8, "the colour added by the thinness");
        assert_eq!(texel[2], (0.2f32 * 255.0) as u8, "the colour of the clouds");
    }
    // The body over the middle of the tile: the slopes near it glow more than those far from it.
    let glow = |rows: std::ops::Range<usize>| {
        let mut sum = 0.0;
        let mut count = 0;
        for row in rows {
            for column in 48..80 {
                let texel = texels[row * SIDE + column];
                if texel[3] > 0 {
                    sum += f32::from(texel[1]);
                    count += 1;
                }
            }
        }
        sum / count.max(1) as f32
    };
    let (near, far) = (glow(56..72), glow(0..16));
    assert!(near > far + 20.0, "{near} {far}");
    // None without the envelope of the glow.
    let mut dark = Field::default();
    dark.tick(
        0.0,
        0.6,
        &Palette {
            envelope: 0.0,
            ..palette()
        },
    );
    assert!(
        dark.texels()
            .iter()
            .filter(|texel| texel[3] > 0)
            .all(|texel| texel[1] == 0)
    );
}

#[test]
fn the_cover_towards_a_direction_is_that_of_the_texel_the_dome_puts_there() {
    let mut field = Field::default();
    field.tick(0.0, 0.5, &palette());
    let cover = field.cover();
    assert_eq!(
        field.cover_towards([0.0, 0.0, 1.0]),
        f32::from(cover[64 * SIDE + 64]) / 255.0
    );
    let towards = on_dome_at(0.1);
    let ([u, v], _) = clouds::on_dome(towards);
    assert_eq!(
        field.cover_towards(towards),
        f32::from(cover[v as usize * SIDE + u as usize]) / 255.0
    );
}

fn resolved<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut std::task::Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// A device of the software adapter of the system, or none.
fn device() -> Option<egui_wgpu::RenderState> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: true,
        ..Default::default()
    }))?
    .ok()?;
    let (device, queue) = resolved(adapter.request_device(&wgpu::DeviceDescriptor::default()))?.ok()?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
    Some(egui_wgpu::RenderState {
        adapter,
        available_adapters: Vec::new(),
        instance,
        device,
        queue,
        target_format: format,
        renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
        surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
    })
}

const TARGET: Target = Target {
    color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
    depth_format: wgpu::TextureFormat::Depth32Float,
    sample_count: 1,
    depth_compare: wgpu::CompareFunction::Greater,
};
const SIZE: u32 = 64;

/// The view from the eye at the origin towards `towards`, 60° wide, at `time` seconds.
fn view_towards(towards: Vec3, time: f32) -> View {
    let up = if towards.normalize().z.abs() > 0.99 {
        Vec3::X
    } else {
        Vec3::Z
    };
    let view_matrix = Mat4::look_at_rh(Vec3::ZERO, towards, up);
    View {
        view_proj: Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1) * view_matrix,
        view: view_matrix,
        eye: Vec3::ZERO,
        size: [SIZE, SIZE],
        time,
        fog: Fog::default(),
        sun: Sun::default(),
        sky: None,
        water: None,
        night: 0.0,
    }
}

/// The images of the bodies on `gpu`, 4 × 4 texels each: the sun white, its glare grey, the moon
/// red, its glare green.
fn images(gpu: &egui_wgpu::RenderState) -> Arc<Images> {
    let views = sky::IMAGES.map(|file| {
        let rgba: [u8; 4] = match file {
            "Textures\\SunCenter.blp" => [255, 255, 255, 255],
            "Textures\\SunGlare.blp" => [128, 128, 128, 255],
            "Textures\\Moon.blp" => [255, 0, 0, 255],
            _ => [0, 255, 0, 255],
        };
        let texture = uniwow_api::formats::Texture {
            width: 4,
            height: 4,
            format: uniwow_api::formats::TextureFormat::Rgba8,
            levels: vec![rgba.repeat(16)],
        };
        Some(sky::texture_view(&gpu.device, &gpu.queue, file, &texture).expect("an image"))
    });
    Arc::new(Images(views))
}

/// What the layer `layer` draws seen by `view`, `SIZE` × `SIZE` pixels cleared to black, its depth
/// cleared to `depth` (0 at infinity, nothing drawn), every phase in its order.
fn render(gpu: &egui_wgpu::RenderState, layer: &mut dyn Layer, view: &View, depth: f32) -> Vec<u8> {
    layer.prepare(gpu, view);
    assert_eq!(layer.drawing(), Drawing::Pass);
    let texture = |format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let colour = texture(
        TARGET.color_format,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let depth_texture = texture(TARGET.depth_format, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let (colour_view, depth_view) = (
        colour.create_view(&Default::default()),
        depth_texture.create_view(&Default::default()),
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &colour_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(depth),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for phase in Phase::ALL {
            layer.draw_pass(gpu, &TARGET, view, phase, &mut pass);
        }
    }
    let row = SIZE * 4;
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &colour,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &pixels,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    pixels.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    pixels.slice(..).get_mapped_range().expect("mapped").to_vec()
}

/// The pixel at `x`, `y` of an image of `render`.
fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * SIZE + x) * 4) as usize;
    [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
}

/// The heavens of a test: the sun towards `sun`, the moon towards `moon`, each 4 yards wide at 12,
/// the sun orange, the moon white, drawn over the whole sky; no glare.
fn heavens(sun: [f32; 3], moon: [f32; 3]) -> Heavens {
    Heavens {
        sun: Body {
            towards: Vec3::from(sun).normalize().to_array(),
            side: 4.0,
            colour: [1.0, 0.5, 0.25],
            alpha: 1.0,
        },
        moon: Body {
            towards: Vec3::from(moon).normalize().to_array(),
            side: 4.0,
            colour: [1.0, 1.0, 1.0],
            alpha: 1.0,
        },
        share: 1.0,
        glares: [Glare::default(); 2],
        flares: [0.0; 2],
    }
}

/// The layers of the sky on `gpu`, drawing `drawn`, their resources built at once.
fn layers(gpu: &egui_wgpu::RenderState, drawn: &Arc<Mutex<Drawn>>) -> (HeavensLayer, GlareLayer, Arc<sky::Gpu>) {
    let resources = Arc::new(sky::Gpu::new(gpu, &TARGET));
    let incoming = Arc::new(Mutex::new(Some(resources.clone())));
    (
        HeavensLayer::new(drawn.clone(), incoming.clone()),
        GlareLayer::new(drawn.clone(), incoming),
        resources,
    )
}

/// Whether `value` is within `margin` of `wanted`.
fn near(value: u8, wanted: i32, margin: i32) -> bool {
    (i32::from(value) - wanted).abs() <= margin
}

#[test]
fn the_sun_is_drawn_where_it_stands_tinted_by_its_colour_over_the_sky_only() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter");
        return;
    };
    let drawn = Arc::new(Mutex::new(Drawn::default()));
    let (mut layer, _, _) = layers(&gpu, &drawn);
    assert_eq!(layer.stage(), Stage::Celestial);
    let towards = Vec3::new(1.0, 0.0, 0.5);
    {
        let mut drawn = sky::lock(&drawn);
        drawn.heavens = Some(heavens(towards.to_array(), [-1.0, 0.0, 0.5]));
        drawn.images = Some((1, images(&gpu)));
    }
    let view = view_towards(towards, 0.0);
    let pixels = render(&gpu, &mut layer, &view, 0.0);
    // In the middle, the white disc tinted orange, its bytes in gamma those of the colour.
    let middle = pixel(&pixels, SIZE / 2, SIZE / 2);
    assert!(
        near(middle[0], 255, 2) && near(middle[1], 128, 2) && near(middle[2], 64, 2),
        "{middle:?}"
    );
    // The quad 4 yards wide 12 yards away spans 18.9° of the 60° of the view: a corner is out of it.
    assert_eq!(pixel(&pixels, 2, 2), [0, 0, 0, 255]);
    // Where something is drawn, nothing.
    let covered = render(&gpu, &mut layer, &view, 0.5);
    assert_eq!(pixel(&covered, SIZE / 2, SIZE / 2), [0, 0, 0, 255]);
    // Without its image, nothing.
    sky::lock(&drawn).images = None;
    let unread = render(&gpu, &mut layer, &view, 0.0);
    assert_eq!(pixel(&unread, SIZE / 2, SIZE / 2), [0, 0, 0, 255]);
    sky::lock(&drawn).images = Some((2, images(&gpu)));
    // Half drawn over half of the sky; nothing over none of it, nor without heavens.
    sky::lock(&drawn).heavens.as_mut().unwrap().share = 0.5;
    let half = render(&gpu, &mut layer, &view, 0.0);
    let middle = pixel(&half, SIZE / 2, SIZE / 2);
    assert!(near(middle[0], 188, 3), "{middle:?}");
    sky::lock(&drawn).heavens.as_mut().unwrap().share = 0.0;
    let none = render(&gpu, &mut layer, &view, 0.0);
    assert_eq!(pixel(&none, SIZE / 2, SIZE / 2), [0, 0, 0, 255]);
    sky::lock(&drawn).heavens = None;
    let none = render(&gpu, &mut layer, &view, 0.0);
    assert_eq!(pixel(&none, SIZE / 2, SIZE / 2), [0, 0, 0, 255]);
}

#[test]
fn a_body_is_clipped_under_the_horizon_and_faded_from_it_as_the_corners_of_its_quad() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter");
        return;
    };
    let drawn = Arc::new(Mutex::new(Drawn::default()));
    let (mut layer, _, _) = layers(&gpu, &drawn);
    sky::lock(&drawn).images = Some((1, images(&gpu)));
    let gamma = |linear: f32| {
        if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        }
    };
    // The alpha of a corner of the quad `height` yards over the eye: 2.5 times its height within
    // the 0.4 yard over the horizon, that of the body above.
    let corner = |height: f32, alpha: f32| {
        if 2.5 * height < 1.0 {
            (2.5 * height).clamp(0.0, 1.0)
        } else {
            alpha
        }
    };
    // The sun on the horizon, its lower half under it, cut there, its corners 2 yards up of its own
    // alpha, whole then left 0.4 by a storm: from none at the horizon to its own at the top. Then
    // its quad 0.2 yard over the horizon, its lower corners 0.5 by the ramp, its upper ones 0.4 under
    // the storm: from the one to the other, no step where the ramp ends, 0.4 yard up. Then the same
    // seen 35° up, the quad facing the camera, its corners 1.64 yards from its middle in height: all
    // over the ramp. Then the sun on the horizon seen 25° up, its top corners 1.81 yards up.
    let ahead = Vec3::X;
    let raised = Vec3::new(1.0, 0.0, 35f32.to_radians().tan());
    let less = Vec3::new(1.0, 0.0, 25f32.to_radians().tan());
    for (looking, height, alpha) in [
        (ahead, 0.0, 1.0),
        (ahead, 0.0, 0.4),
        (ahead, 2.2, 0.4),
        (raised, 2.2, 0.4),
        (less, 0.0, 1.0),
    ] {
        let view = view_towards(looking, 0.0);
        let rise = height / (144.0f32 - height * height).sqrt();
        let mut given = heavens([1.0, 0.0, rise], [-1.0, 0.0, 0.0]);
        given.sun.alpha = alpha;
        sky::lock(&drawn).heavens = Some(given);
        let pixels = render(&gpu, &mut layer, &view, 0.0);
        if looking == ahead {
            assert_eq!(
                pixel(&pixels, SIZE / 2, SIZE / 2 + 4),
                [0, 0, 0, 255],
                "under the horizon"
            );
        }
        let centre = Vec3::new(1.0, 0.0, rise).normalize() * 12.0;
        let to_world = view.view.inverse();
        let forward = to_world.transform_vector3(-Vec3::Z);
        let up = to_world.transform_vector3(Vec3::Y);
        let (lowest, highest) = ((centre.z - 2.0 * up.z).max(0.0), centre.z + 2.0 * up.z);
        let mut tested = 0;
        for row in 0..SIZE {
            // Where the middle of the row meets the plane of the quad.
            let across = 1.0 / SIZE as f32;
            let down = 1.0 - (row as f32 + 0.5) * 2.0 / SIZE as f32;
            let towards = to_world.transform_vector3(Vec3::new(across, down, -30f32.to_radians().tan().recip()));
            let point = towards * (centre.dot(forward) / towards.dot(forward));
            if (point - centre).dot(up).abs() > 1.9 || point.z < lowest + 0.05 || point.z > highest - 0.1 {
                continue;
            }
            let share = (point.z - lowest) / (highest - lowest);
            let seen = corner(lowest, alpha) + (corner(highest, alpha) - corner(lowest, alpha)) * share;
            let wanted = (gamma(seen) * 255.0).round() as i32;
            let red = pixel(&pixels, SIZE / 2, row)[0];
            assert!(
                near(red, wanted, 2),
                "{height} yards up, alpha {alpha}, row {row}: {red} against {wanted}"
            );
            tested += 1;
        }
        assert!(tested >= 8, "{tested}");
    }
}

#[test]
fn the_moon_is_drawn_over_the_sun_and_the_clouds_over_both() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter");
        return;
    };
    let drawn = Arc::new(Mutex::new(Drawn::default()));
    let (mut layer, _, _) = layers(&gpu, &drawn);
    let up = [0.0, 0.0, 1.0];
    {
        let mut drawn = sky::lock(&drawn);
        drawn.images = Some((1, images(&gpu)));
        drawn.heavens = Some(heavens(up, up));
    }
    let view = view_towards(Vec3::Z, 0.0);
    let pixels = render(&gpu, &mut layer, &view, 0.0);
    // The red moon, tinted white, over the sun.
    let middle = pixel(&pixels, SIZE / 2, SIZE / 2);
    assert!(middle[0] > 250 && middle[1] < 3 && middle[2] < 3, "{middle:?}");
    // Clouds over both: a tile of blue half covering the sky, mixed over them in gamma.
    sky::lock(&drawn).clouds = Some((1, Arc::new(vec![[0, 0, 255, 128]; SIDE * SIDE])));
    let pixels = render(&gpu, &mut layer, &view, 0.0);
    let middle = pixel(&pixels, SIZE / 2, SIZE / 2);
    assert!(near(middle[0], 127, 3) && near(middle[2], 128, 3), "{middle:?}");
    // Where neither body is, the clouds alone over the black: a half of blue, blended in linear.
    let corner = pixel(&pixels, 2, 2);
    assert!(corner[0] < 3 && near(corner[2], 188, 4), "{corner:?}");
    // The same clouds again: not written again.
    sky::lock(&drawn).clouds = Some((1, Arc::new(vec![[0, 0, 0, 0]; SIDE * SIDE])));
    let pixels = render(&gpu, &mut layer, &view, 0.0);
    assert!(near(pixel(&pixels, 2, 2)[2], 188, 4));
}

/// A pyramid of one level of `SIZE` × `SIZE` texels on `gpu`, at infinity (0) but where `covered`
/// says, there 1.
fn pyramid(gpu: &egui_wgpu::RenderState, covered: impl Fn(u32, u32) -> bool) -> wgpu::TextureView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let texels: Vec<f32> = (0..SIZE * SIZE)
        .map(|at| if covered(at % SIZE, at / SIZE) { 1.0 } else { 0.0 })
        .collect();
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&texels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * 4),
            rows_per_image: Some(SIZE),
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&Default::default())
}

/// The layer `layer` prepared with `view`, its sight tested against `pyramid`, submitted.
fn see(
    gpu: &egui_wgpu::RenderState,
    layer: &mut GlareLayer,
    view: &View,
    pyramid: &wgpu::TextureView,
    generation: u64,
) {
    layer.prepare(gpu, view);
    assert!(layer.occludes());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    layer.occlude(
        gpu,
        view,
        &Pyramid {
            view: pyramid,
            size: [SIZE; 2],
            levels: 1,
            generation,
        },
        &mut encoder,
    );
    gpu.queue.submit([encoder.finish()]);
}

#[test]
fn the_glare_follows_the_share_of_its_body_in_sight_rising_and_falling_at_their_paces() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter");
        return;
    };
    let drawn = Arc::new(Mutex::new(Drawn::default()));
    let (_, mut layer, resources) = layers(&gpu, &drawn);
    assert_eq!(layer.stage(), Stage::Glare);
    let towards = Vec3::new(1.0, 0.0, 0.5);
    let mut given = heavens(towards.to_array(), [-1.0, 0.0, -0.5]);
    given.flares = [1.0, 1.0];
    given.glares = [
        Glare {
            sides: [3.0, 20.0],
            alphas: [0.5, 1.0],
        },
        Glare {
            sides: [2.0, 2.0],
            alphas: [0.1, 1.0],
        },
    ];
    {
        let mut drawn = sky::lock(&drawn);
        drawn.heavens = Some(given);
        drawn.images = Some((1, images(&gpu)));
    }
    let clear = pyramid(&gpu, |_, _| false);
    // The left half of the view covered: half of the sun in sight.
    let half = pyramid(&gpu, |x, _| x < SIZE / 2);
    let all = pyramid(&gpu, |_, _| true);
    // A first frame measures no time: the envelopes stay at nothing.
    see(&gpu, &mut layer, &view_towards(towards, 0.0), &clear, 1);
    assert_eq!(sky::envelopes(&gpu, &resources), [0.0, 0.0]);
    // A tenth of a second in clear sight: the sun's rises by 0.4, the moon's, under the horizon,
    // not at all.
    see(&gpu, &mut layer, &view_towards(towards, 0.1), &clear, 1);
    let [sun, moon] = sky::envelopes(&gpu, &resources);
    assert!((sun - 0.4).abs() < 1e-4 && moon == 0.0, "{sun} {moon}");
    // Half in sight: up to a half, rising by 0.4 at most; its glare drawn half as strong: a half of
    // the colour of the sun times the grey of its image, made linear.
    see(&gpu, &mut layer, &view_towards(towards, 0.2), &half, 2);
    assert!((sky::envelopes(&gpu, &resources)[0] - 0.5).abs() < 1e-4);
    let pixels = render(&gpu, &mut layer, &view_towards(towards, 0.2), 0.0);
    let middle = pixel(&pixels, SIZE / 2, SIZE / 2);
    assert!(near(middle[0], 93, 3) && near(middle[1], 44, 3), "{middle:?}");
    // Hidden: falling by 50/33 a second, a quarter of a second at most.
    see(&gpu, &mut layer, &view_towards(towards, 0.3), &all, 3);
    let fallen = 0.5 - 50.0 / 33.0 * 0.1;
    assert!((sky::envelopes(&gpu, &resources)[0] - fallen).abs() < 1e-4);
    see(&gpu, &mut layer, &view_towards(towards, 5.0), &all, 3);
    assert_eq!(sky::envelopes(&gpu, &resources)[0], 0.0);
    // Off the screen nothing is in sight: looking away.
    see(&gpu, &mut layer, &view_towards(-towards, 5.25), &clear, 1);
    assert_eq!(sky::envelopes(&gpu, &resources)[0], 0.0);
    // Back in clear sight a quarter of a second: whole; added over the sky, the colour of the sun
    // times the grey of its glare, straight at it whole.
    let view = view_towards(towards, 5.5);
    see(&gpu, &mut layer, &view, &clear, 1);
    assert_eq!(sky::envelopes(&gpu, &resources)[0], 1.0);
    let pixels = render(&gpu, &mut layer, &view, 0.0);
    let middle = pixel(&pixels, SIZE / 2, SIZE / 2);
    assert!(
        near(middle[0], 128, 3) && near(middle[1], 64, 3) && near(middle[2], 32, 3),
        "{middle:?}"
    );
    // Not over what is drawn.
    let covered = render(&gpu, &mut layer, &view, 0.5);
    assert_eq!(pixel(&covered, SIZE / 2, SIZE / 2), [0, 0, 0, 255]);
    // Without heavens, the envelopes fall.
    sky::lock(&drawn).heavens = None;
    see(&gpu, &mut layer, &view_towards(towards, 5.6), &clear, 1);
    let fallen = 1.0 - 50.0 / 33.0 * 0.1;
    assert!((sky::envelopes(&gpu, &resources)[0] - fallen).abs() < 1e-4);
    // A body ahead but under the horizon is not in sight, whatever the depth says.
    let mut under = given;
    under.moon.towards = Vec3::new(1.0, 0.0, -0.27).normalize().to_array();
    sky::lock(&drawn).heavens = Some(under);
    see(&gpu, &mut layer, &view_towards(Vec3::X, 5.7), &clear, 1);
    assert_eq!(sky::envelopes(&gpu, &resources)[1], 0.0);
}

#[test]
fn the_glare_is_as_wide_and_as_strong_as_the_view_turns_onto_its_body() {
    let view = view_towards(Vec3::X, 0.0);
    let mut given = heavens([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    given.glares = [
        Glare {
            sides: [3.0, 20.0],
            alphas: [0.5, 1.0],
        },
        Glare {
            sides: [2.0, 2.0],
            alphas: [0.1, 1.0],
        },
    ];
    given.moon.alpha = 0.5;
    given.share = 0.75;
    given.flares = [0.25, 0.5];
    let values = sky::values(&view, &given, 0.1);
    assert_eq!(&values[60..62], &[20.0, 1.0], "straight at the sun");
    assert_eq!(&values[64..66], &[2.0, 0.05], "the moon 90° away, faded by its alpha");
    assert_eq!(values[35], 0.75);
    assert_eq!(&values[44..48], &[1.0, 0.0, 0.0, 4.0]);
    assert_eq!(&values[68..72], &[0.25, 0.5, 4.0, 100.0 / 33.0]);
    assert_eq!(&values[72..74], &[50.0 / 33.0, 0.1]);
}
