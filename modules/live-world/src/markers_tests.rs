use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::server_link::protocol::{CATMULL_ROM, CYCLIC, DEAD, Entity, GAME_MASTER, Kind, PathPoint, Spline};
use uniwow_api::viewport::{Layer, Target, View};
use uniwow_api::{bytemuck, egui, egui_wgpu, wgpu};

use crate::markers::{self, Drawing, Instance, LABEL_REACH, LABELS, Markers, SIZE};
use crate::world::{Tracked, World, catmull_rom};

fn entity(guid: u64, kind: Kind, position: [f32; 3]) -> Entity {
    Entity {
        guid,
        kind,
        entry: 1,
        spawn: 1,
        flags: 0,
        pool: 0,
        event: 0,
        phase: 1,
        display: 1,
        position,
        orientation: 0.0,
        scale: 1.0,
        rotation: None,
        state: None,
        spline: None,
        name: format!("entity {guid}"),
    }
}

fn path(points: &[([f32; 2], u32)], flags: u8, elapsed: u32) -> Spline {
    Spline {
        id: 1,
        flags,
        elapsed,
        points: points
            .iter()
            .map(|&([x, y], time)| PathPoint {
                position: [x, y, 0.0],
                time,
            })
            .collect(),
    }
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-3)
}

#[test]
fn a_catmull_rom_spline_is_followed_with_the_weights_and_the_ends_of_azerothcore() {
    // The curve passes through its points; between collinear ones evenly spaced, it is straight.
    let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0]];
    assert!(close(catmull_rom(points, 0.0), points[1]));
    assert!(close(catmull_rom(points, 1.0), points[2]));
    assert!(close(catmull_rom(points, 0.5), [1.5, 0.0, 0.0]));

    let received = Instant::now();
    let at = |spline: Spline, after: u64| {
        let mut moving = entity(1, Kind::Creature, [0.0; 3]);
        moving.spline = Some(spline);
        Tracked {
            entity: moving,
            received,
        }
        .position_at(received + Duration::from_millis(after))
    };
    // Half its first segment: the point before the first a yard back the way it heads, the one
    // after the next point of the spline. By hand: x = 1/16 + 10 × 9/16 − 10/16, y = −20/16.
    let corner = [([0.0, 0.0], 0), ([10.0, 0.0], 1000), ([10.0, 20.0], 3000)];
    assert!(close(at(path(&corner, CATMULL_ROM, 500), 0), [5.0625, -1.25, 0.0]));
    assert!(
        close(at(path(&corner, CATMULL_ROM, 0), 1000), [10.0, 0.0, 0.0]),
        "through its points"
    );
    assert!(
        close(at(path(&corner, CATMULL_ROM, 0), 60_000), [10.0, 20.0, 0.0]),
        "its end"
    );

    // A cyclic one sent whole goes round, its ends taken from the other side of the loop.
    let square = [
        ([0.0, 0.0], 0),
        ([10.0, 0.0], 1000),
        ([10.0, 10.0], 2000),
        ([0.0, 10.0], 3000),
        ([0.0, 0.0], 4000),
    ];
    assert!(close(
        at(path(&square, CATMULL_ROM | CYCLIC, 4500), 0),
        [5.0, -1.25, 0.0]
    ));
    // One sent by a window, which does not start at 0, stops at its end until it is sent again.
    let window = [([10.0, 0.0], 1000), ([10.0, 10.0], 2000)];
    assert!(close(at(path(&window, CYCLIC, 9000), 0), [10.0, 10.0, 0.0]));
}

#[test]
fn the_markers_stand_on_their_entities_coloured_by_kind_and_the_nearest_named() {
    let now = Instant::now();
    let mut world = World::default();
    let mut add = |entity: Entity| {
        world
            .entities
            .insert(entity.guid, Arc::new(Tracked { entity, received: now }));
    };
    add(entity(1, Kind::Creature, [0.0, 0.0, 10.0]));
    let mut dead = entity(2, Kind::Creature, [5.0, 0.0, 10.0]);
    dead.flags = DEAD;
    add(dead);
    let mut master = entity(3, Kind::Player, [10.0, 0.0, 10.0]);
    master.flags = GAME_MASTER;
    add(master);
    let mut big = entity(4, Kind::GameObject, [500.0, 0.0, 10.0]);
    big.scale = 2.0;
    add(big);
    let mut nameless = entity(5, Kind::Player, [1.0, 0.0, 10.0]);
    nameless.name.clear();
    add(nameless);

    let (instances, labels) = markers::build(&world, now, Some(Vec3::new(0.0, 0.0, 15.0)));
    assert_eq!(instances.len(), 5);
    let by_x = |x: f32| instances.iter().find(|instance| instance.centre[0] == x).unwrap();
    assert_eq!(by_x(0.0).centre, [0.0, 0.0, 10.0 + SIZE], "standing on its position");
    assert_eq!(by_x(0.0).colour, [1.0, 0.55, 0.1, 1.0], "a creature");
    assert_eq!(by_x(5.0).colour, [0.5, 0.5, 0.5, 1.0], "dead");
    assert_eq!(by_x(10.0).colour, [0.75, 0.45, 1.0, 1.0], "a game master");
    assert_eq!(by_x(500.0).size, 2.0 * SIZE, "by its scale");
    let names: Vec<&str> = labels.iter().map(|label| label.text.as_str()).collect();
    assert_eq!(
        names,
        vec!["entity 1", "entity 2", "entity 3"],
        "the nearest first, within {LABEL_REACH} yards, a name needed"
    );
    assert_eq!(
        labels[0].position,
        Vec3::new(0.0, 0.0, 10.0 + 2.0 * SIZE),
        "above its marker"
    );
    assert!(markers::build(&world, now, None).1.is_empty(), "no eye, no name");

    let crowd: Vec<Entity> = (0..100)
        .map(|i| entity(100 + i, Kind::Creature, [i as f32, 0.0, 0.0]))
        .collect();
    for entity in crowd {
        world
            .entities
            .insert(entity.guid, Arc::new(Tracked { entity, received: now }));
    }
    assert_eq!(markers::build(&world, now, Some(Vec3::ZERO)).1.len(), LABELS);
}

fn resolved<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
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

/// The bytes of `buffer`, from `offset`, copied back from the GPU.
fn read_back(gpu: &egui_wgpu::RenderState, buffer: &wgpu::Buffer, size: u64) -> Vec<u8> {
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read back"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    gpu.queue.submit([encoder.finish()]);
    staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    staging.slice(..).get_mapped_range().expect("mapped").to_vec()
}

/// What `layer` draws seen from `eye` towards `look`, 64 × 64 pixels of RGBA.
fn render(gpu: &egui_wgpu::RenderState, layer: &mut Markers, eye: Vec3, look: Vec3) -> Vec<u8> {
    let view = View {
        view_proj: Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1)
            * Mat4::look_at_rh(eye, look, Vec3::Z),
        eye,
        size: [64, 64],
        time: 0.0,
    };
    // Twice: the first frame makes the resources, which the second writes its camera to.
    let mut bundle = None;
    for _ in 0..2 {
        layer.prepare(gpu, &view);
        let mut encoder = gpu
            .device
            .create_render_bundle_encoder(&wgpu::RenderBundleEncoderDescriptor {
                label: None,
                color_formats: &[Some(TARGET.color_format)],
                depth_stencil: Some(wgpu::RenderBundleDepthStencil {
                    format: TARGET.depth_format,
                    depth_read_only: false,
                    stencil_read_only: true,
                }),
                sample_count: 1,
                multiview: None,
            });
        layer.draw(gpu, &TARGET, &view, &mut encoder);
        bundle = Some(encoder.finish(&wgpu::RenderBundleDescriptor { label: None }));
    }
    let texture = |format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
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
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256 * 64,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &colour.create_view(&Default::default()),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.create_view(&Default::default()),
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.execute_bundles(bundle.iter());
    }
    encoder.copy_texture_to_buffer(
        colour.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &pixels,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(64),
            },
        },
        wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read_back(gpu, &pixels, 256 * 64)
}

#[test]
fn the_markers_are_written_by_their_thread_to_a_buffer_that_grows_and_drawn_in_one_draw() {
    let Some(gpu) = device() else {
        eprintln!("skipped: no software adapter for a device");
        return;
    };
    let drawing = Arc::new(Drawing::default());
    let mut layer = Markers::new(drawing.clone());
    let first = layer.version();

    // Nothing written yet: nothing drawn.
    let pixels = render(&gpu, &mut layer, Vec3::new(-10.0, 0.0, 2.0), Vec3::new(0.0, 0.0, 1.2));
    assert!(pixels.iter().all(|byte| *byte == 0 || *byte == 255));
    assert_ne!(layer.version(), first, "recorded again once its resources exist");

    let marker = Instance {
        centre: [0.0, 0.0, 1.2],
        size: 1.2,
        colour: [1.0, 0.55, 0.1, 1.0],
    };
    drawing.write(&gpu.device, &gpu.queue, &[marker], Vec::new());
    let version = layer.version();
    let pixels = render(&gpu, &mut layer, Vec3::new(-10.0, 0.0, 2.0), Vec3::new(0.0, 0.0, 1.2));
    let centre = &pixels[32 * 256 + 32 * 4..32 * 256 + 32 * 4 + 4];
    assert!(
        centre[0] > 150 && centre[1] > 40 && centre[2] < 60,
        "orange at the centre: {centre:?}"
    );
    assert_eq!(layer.stats().draws, 1);
    assert_eq!(layer.stats().triangles, 8);

    // 300 markers do not fit 256: a buffer twice as large, recorded again.
    let many = vec![marker; 300];
    drawing.write(&gpu.device, &gpu.queue, &many, Vec::new());
    assert_ne!(layer.version(), version);
    assert_eq!(layer.stats().bytes, 512 * size_of::<Instance>() as u64);

    // Fewer again: those drawn before and not now have no size.
    drawing.write(&gpu.device, &gpu.queue, &[marker], Vec::new());
    render(&gpu, &mut layer, Vec3::new(-10.0, 0.0, 2.0), Vec3::new(0.0, 0.0, 1.2));
    let buffer = layer.buffer().expect("written");
    let instances: Vec<Instance> =
        bytemuck::cast_slice(&read_back(&gpu, &buffer, 300 * size_of::<Instance>() as u64)).to_vec();
    assert_eq!(instances[0], marker);
    assert!(instances[1..].iter().all(|instance| instance.size == 0.0));
}
