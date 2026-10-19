//! Tests of the choice of a click and of the boxes of the selection.

use std::sync::Arc;

use uniwow_api::formats::TileId;
use uniwow_api::scene::{EntityId, Index, Listing, PieceBuilder, PieceKey, Placed, Snapshot, box_around, flags};

use crate::boxes::boxes;
use crate::chosen;

#[test]
fn a_click_chooses_the_parent_first_then_goes_down_on_the_same_spot() {
    let [inn, room, chair] = [
        EntityId::building(0, 9),
        EntityId::room(0, 9, 1),
        EntityId::building_doodad(0, 9, 4),
    ];
    let chain = [inn, room, chair];
    assert_eq!(chosen(&chain, &[], false), Some(inn));
    assert_eq!(chosen(&chain, &[inn], true), Some(room));
    assert_eq!(chosen(&chain, &[room], true), Some(chair));
    assert_eq!(
        chosen(&chain, &[chair], true),
        Some(inn),
        "past the deepest, the outermost again"
    );
    assert_eq!(chosen(&chain, &[chair], false), Some(inn), "elsewhere: the outermost");
    assert_eq!(chosen(&chain, &[inn, room], true), Some(inn), "more than one selected");
    assert_eq!(chosen(&[], &[], false), None);
}

#[test]
fn the_boxes_are_those_of_the_entities_selected_the_registry_holds() {
    let tile = TileId { x: 1, y: 1 };
    let mut builder = PieceBuilder::new(vec![tile]);
    builder.push(uniwow_api::scene::Entity {
        id: EntityId::doodad(0, 7),
        flags: flags::CLIENT,
        parent: None,
        label: "Tree",
        extra: 0,
        origin: [1.0, 2.0, 3.0],
        bounds: box_around([1.0, 2.0, 3.0], 1.0),
    });
    let piece = Arc::new(builder.build());
    let key = PieceKey::tile("doodads", 0, tile);
    let placed = Arc::new(Placed {
        key: key.clone(),
        generation: 1,
        bounds: piece.bounds[0],
        piece,
        areas: Box::new([0]),
    });
    let index: Index = [(EntityId::doodad(0, 7), vec![Listing { slot: 0, row: 0 }])]
        .into_iter()
        .collect();
    let snapshot = Snapshot {
        generation: 1,
        slots: Arc::new(vec![Some(placed)]),
        keys: Arc::new([(key, 0)].into_iter().collect()),
        index: Arc::new(index),
        entities: 1,
        ..Snapshot::default()
    };
    let found = boxes(&snapshot, &[EntityId::doodad(0, 8), EntityId::doodad(0, 7)]);
    assert_eq!(
        found,
        [[0.0, 1.0, 2.0, 2.0, 3.0, 4.0]],
        "an entity unloaded drawn without a box"
    );
}

mod gpu {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

    use uniwow_api::glam::{Mat4, Vec3};
    use uniwow_api::viewport::{Fog, Layer, Phase, Sun, Target, View};
    use uniwow_api::{egui, egui_wgpu, wgpu};

    use crate::boxes::BoxesLayer;

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
        color_format: wgpu::TextureFormat::Rgba8Unorm,
        depth_format: wgpu::TextureFormat::Depth32Float,
        sample_count: 1,
        depth_compare: wgpu::CompareFunction::Greater,
    };

    /// A scene holding one entity, selected, of the bounds `bounds`.
    fn selected(bounds: [[f32; 3]; 2]) -> uniwow_api::scene::Handle {
        use uniwow_api::scene::*;
        struct One(Arc<Snapshot>, Arc<Selection>);
        impl Scene for One {
            fn publish(&self, _: PieceKey, _: Arc<Piece>) {}
            fn withdraw(&self, _: PieceKey) {}
            fn withdraw_provider(&self, _: &str, _: Option<u32>) {}
            fn snapshot(&self) -> Arc<Snapshot> {
                self.0.clone()
            }
            fn selection(&self) -> Arc<Selection> {
                self.1.clone()
            }
            fn select(&self, _: Select) {}
            fn add_picker(&self, _: &str, _: Arc<dyn Picker>) {}
            fn locked_kinds(&self) -> Vec<Kind> {
                Vec::new()
            }
            fn set_kind_locked(&self, _: Kind, _: bool) {}
            fn pick(&self, _: &uniwow_api::viewport::Ray) -> Option<Hit> {
                None
            }
            fn within(&self, _: &uniwow_api::viewport::Seen, _: [[f32; 2]; 2], _: &[Kind]) -> Vec<EntityId> {
                Vec::new()
            }
            fn ask(&self, _: Question) -> std::sync::mpsc::Receiver<Answer> {
                std::sync::mpsc::channel().1
            }
        }
        let id = EntityId::doodad(0, 1);
        let mut builder = PieceBuilder::new(Vec::new());
        builder.push(Entity {
            id,
            flags: flags::CLIENT,
            parent: None,
            label: "",
            extra: 0,
            origin: [0.0; 3],
            bounds,
        });
        let piece = Arc::new(builder.build());
        let key = PieceKey::part("doodads", 0, 0);
        let placed = Arc::new(Placed {
            key: key.clone(),
            generation: 1,
            bounds,
            piece,
            areas: Box::new([0]),
        });
        let snapshot = Snapshot {
            generation: 1,
            slots: Arc::new(vec![Some(placed)]),
            keys: Arc::new([(key, 0)].into_iter().collect()),
            index: Arc::new([(id, vec![Listing { slot: 0, row: 0 }])].into_iter().collect()),
            entities: 1,
            ..Snapshot::default()
        };
        let selection = Selection {
            generation: 1,
            ids: vec![id],
        };
        Arc::new(One(Arc::new(snapshot), Arc::new(selection)))
    }

    /// The boxes of the selection drawn by the software adapter in the colour of the selection,
    /// along the edges of the box seen from above, nowhere in its middle.
    #[test]
    fn the_box_of_an_entity_selected_is_drawn_along_its_edges() {
        let Some(gpu) = device() else {
            eprintln!("skipped: no software adapter");
            return;
        };
        let size = 64u32;
        let eye = Vec3::new(0.0, 0.0, 10.0);
        let view = View {
            view_proj: Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.1)
                * Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y),
            view: Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y),
            eye,
            size: [size; 2],
            time: 0.0,
            fog: Fog::default(),
            sun: Sun::default(),
            sky: None,
            water: None,
            night: 0.0,
        };
        let mut layer = BoxesLayer::new(selected([[-2.0, -2.0, -1.0], [2.0, 2.0, 1.0]]), TARGET);
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        layer.prepare(&gpu, &view);
        let texture = |format, usage| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
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
        let mut bundle = gpu
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
        layer.draw(&gpu, &TARGET, &view, Phase::Near, &mut bundle);
        let bundle = bundle.finish(&wgpu::RenderBundleDescriptor::default());
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
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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
            pass.execute_bundles(std::iter::once(&bundle));
        }
        let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(size * size * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            colour.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size * 4),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);
        assert!(resolved(scope.pop()).flatten().is_none(), "no validation error");
        read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        let bytes = read.slice(..).get_mapped_range().expect("mapped").to_vec();
        let pixel = |x: u32, y: u32| {
            let at = ((y * size + x) * 4) as usize;
            [bytes[at], bytes[at + 1], bytes[at + 2]]
        };
        let orange = |p: [u8; 3]| p[0] > 200 && p[1] > 100 && p[1] < 180 && p[2] < 80;
        assert!(!orange(pixel(32, 32)), "nothing in the middle of the box");
        let edges = (0..size)
            .flat_map(|y| (0..size).map(move |x| (x, y)))
            .filter(|(x, y)| orange(pixel(*x, *y)))
            .count();
        assert!(edges > 40, "{edges} pixels of the edges");
    }
}
