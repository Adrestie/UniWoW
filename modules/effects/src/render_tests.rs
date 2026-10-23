//! The effects drawn by their layer on the software adapter of the system, as the 3D view draws
//! them: the examples of WoW-mods' tools at moments of their playback, seen from the grazing,
//! top and three-quarter views. With `UNIWOW_EFFECTS_SHOTS` set to a folder, the images are
//! written there as PNG, to be looked at.

use std::future::Future;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::vfx::json;
use uniwow_api::viewport::{Fog, Layer, Phase, Sun, Target, View};
use uniwow_api::{egui, egui_wgpu, miniz_oxide, wgpu};

use crate::layer::EffectLayer;
use crate::{Stage, gpu, load};

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
    color_format: wgpu::TextureFormat::Rgba8Unorm,
    depth_format: wgpu::TextureFormat::Depth32Float,
    sample_count: 1,
    depth_compare: wgpu::CompareFunction::Greater,
};
const SIZE: u32 = 256;

fn view(eye: Vec3, target: Vec3, time: f32) -> View {
    let view_matrix = Mat4::look_at_rh(eye, target, Vec3::Z);
    View {
        view_proj: Mat4::perspective_infinite_reverse_rh(50f32.to_radians(), 1.0, 0.1) * view_matrix,
        view: view_matrix,
        eye,
        size: [SIZE, SIZE],
        time,
        fog: Fog::default(),
        sun: Sun::default(),
        sky: None,
        water: None,
        night: 0.0,
    }
}

/// What the layer draws seen by `view`, on a grey ground-coloured clear.
fn render(gpu: &egui_wgpu::RenderState, layer: &mut dyn Layer, view: &View) -> Vec<u8> {
    layer.prepare(gpu, view);
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
    let depth = texture(TARGET.depth_format, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let (colour_view, depth_view) = (
        colour.create_view(&Default::default()),
        depth.create_view(&Default::default()),
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
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.08,
                        g: 0.08,
                        b: 0.1,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
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

/// An RGBA image as a PNG file.
fn png(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    fn crc(bytes: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for b in bytes {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    let chunk = |out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc(&body).to_be_bytes());
    };
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    for row in rgba.chunks((width * 4) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Each example drawn at moments of its playback from three views, far from the world's origin as
/// in the game; every image shows the effect (pixels brighter than the clear).
#[test]
#[ignore = "needs WoW-mods' tools, and a while on the software adapter"]
fn the_examples_are_drawn() {
    let Some(gpu) = device() else {
        eprintln!("no software adapter: nothing drawn");
        return;
    };
    let dir = PathBuf::from(r"E:\WoW-mods\tools\vfx\examples");
    let shots = std::env::var_os("UNIWOW_EFFECTS_SHOTS").map(PathBuf::from);
    let resources = Arc::new(gpu::create(&gpu.device, &gpu.queue, &TARGET));
    let anchor = Vec3::new(228.0, 2633.0, 87.0);
    for name in ["holy_pillar", "lotus", "arcane_nova", "tree"] {
        let path = dir.join(format!("{name}.json"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let spec = json::parse(&text).unwrap();
        let mut loaded = load::forged(&spec, &dir, &path.display().to_string(), None).unwrap();
        loaded.version = 1;
        let groups = loaded
            .images
            .iter()
            .zip(&loaded.model.textures)
            .map(|(image, texture)| {
                image.as_ref().map(|image| {
                    gpu::texture_group(
                        &gpu.device,
                        &gpu.queue,
                        &resources.texture_layout,
                        &resources.samplers,
                        image,
                        texture.flags,
                    )
                })
            })
            .collect::<Vec<_>>();
        let stage = Arc::new(Mutex::new(Stage {
            effect: Some(Arc::new(loaded)),
            textures: Some(Arc::new(groups)),
            anchor,
            playing: false,
            ..Stage::default()
        }));
        let mut layer = EffectLayer::new(stage.clone(), Arc::new(Mutex::new(Some(resources.clone()))));
        for (time, label) in [(0.4, "a"), (1.5, "b"), (3.0, "c")] {
            lock_time(&stage, time);
            for (view_name, eye, target) in [
                ("grazing", Vec3::new(7.0, 0.0, 0.6), Vec3::new(0.0, 0.0, 1.0)),
                ("top", Vec3::new(0.01, 0.0, 9.0), Vec3::ZERO),
                ("three", Vec3::new(5.0, 5.0, 4.0), Vec3::new(0.0, 0.0, 1.0)),
            ] {
                let image = render(&gpu, &mut layer, &view(anchor + eye, anchor + target, time as f32));
                let lit = image.chunks(4).filter(|p| p[0] > 40 || p[1] > 40 || p[2] > 40).count();
                if let Some(dir) = &shots {
                    std::fs::create_dir_all(dir).unwrap();
                    std::fs::write(
                        dir.join(format!("{name}_{view_name}_{label}.png")),
                        png(&image, SIZE, SIZE),
                    )
                    .unwrap();
                }
                assert!(lit > 20, "{name} {view_name} at {time} s: {lit} pixels lit");
            }
        }
    }
}

/// The Effect view of each example, on its sloped grid, at a moment of its playback.
#[test]
#[ignore = "needs WoW-mods' tools, and a while on the software adapter"]
fn the_effect_view_draws_the_examples_on_their_ground() {
    let Some(gpu) = device() else {
        return;
    };
    const VIEW: Target = Target {
        color_format: wgpu::TextureFormat::Rgba8Unorm,
        depth_format: wgpu::TextureFormat::Depth32Float,
        sample_count: 4,
        depth_compare: wgpu::CompareFunction::Greater,
    };
    let dir = PathBuf::from(r"E:\WoW-mods\tools\vfx\examples");
    let shots = std::env::var_os("UNIWOW_EFFECTS_SHOTS").map(PathBuf::from);
    let resources = Arc::new(gpu::create(&gpu.device, &gpu.queue, &VIEW));
    for (name, slope) in [
        ("holy_pillar", 0.0),
        ("lotus", 15.0),
        ("tree", 0.0),
        ("arcane_nova", 20.0),
    ] {
        let path = dir.join(format!("{name}.json"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let spec = json::parse(&text).unwrap();
        let mut loaded = load::forged(&spec, &dir, &path.display().to_string(), None).unwrap();
        loaded.version = 1;
        let groups = loaded
            .images
            .iter()
            .zip(&loaded.model.textures)
            .map(|(image, texture)| {
                image.as_ref().map(|image| {
                    gpu::texture_group(
                        &gpu.device,
                        &gpu.queue,
                        &resources.texture_layout,
                        &resources.samplers,
                        image,
                        texture.flags,
                    )
                })
            })
            .collect::<Vec<_>>();
        let radius = loaded
            .model
            .boxes
            .get(24..28)
            .map_or(3.0, |b| f32::from_le_bytes(b.try_into().unwrap()));
        let stage = Stage {
            effect: Some(Arc::new(loaded)),
            textures: Some(Arc::new(groups)),
            playing: false,
            time: 2.0,
            ..Stage::default()
        };
        let mut view = crate::view::EffectView::default();
        view.slope = slope;
        view.angle(crate::view::Angle::Three);
        view.distance = (radius * 2.6).clamp(4.0, 60.0);
        view.target = uniwow_api::glam::Vec3::new(0.0, 0.0, radius * 0.6);
        let image = view.snapshot(&gpu, &resources, &VIEW, &stage, [320, 240]);
        if let Some(dir) = &shots {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(format!("view_{name}.png")), png(&image, 320, 240)).unwrap();
        }
        // the background is (41, 43, 48): the ground and the effect cover a good share of the view
        let drawn = image
            .chunks(4)
            .filter(|p| p[0].abs_diff(41) > 6 || p[2].abs_diff(48) > 6)
            .count();
        assert!(drawn > 320 * 240 / 10, "{name}: {drawn} pixels drawn");
    }
}

fn lock_time(stage: &Mutex<Stage>, time: f64) {
    let mut stage = stage.lock().unwrap();
    stage.time = time;
}
