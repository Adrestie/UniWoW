//! The 3D view. Draws a ground grid and the layers that other features add through the
//! "viewport" service, into an offscreen target shown in its panel.

mod camera;
mod grid;

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Poll, Waker};
use std::time::Instant;

use uniwow_api::viewport::{self, Layer, Target, View};
use uniwow_api::{Context, DockArea, Event, FEATURE_FAILED_TOPIC, Feature, Registrar, egui, egui_wgpu, wgpu};

use camera::OrbitCamera;
use grid::Grid;

const TARGET: Target = Target {
    color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
    depth_format: wgpu::TextureFormat::Depth32Float,
    sample_count: 4,
};

const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.012,
    g: 0.014,
    b: 0.02,
    a: 1.0,
};

/// The layers, and the owners whose layers were removed while the list was out being drawn.
#[derive(Default)]
struct LayerList {
    layers: Vec<(String, Box<dyn Layer>)>,
    /// Set while `record_layers` has the layers out.
    drawing: bool,
    /// Removed again from the drawn layers when they come back.
    removed: Vec<String>,
}

type Layers = Arc<Mutex<LayerList>>;

/// The layer list, even if a panic left its lock poisoned: layers are taken out while drawn.
fn lock(layers: &Layers) -> MutexGuard<'_, LayerList> {
    layers.lock().unwrap_or_else(|e| e.into_inner())
}

/// Removes the layers of `owner`, including those out being drawn at this moment.
fn remove(layers: &Layers, owner: &str) {
    let mut list = lock(layers);
    list.layers.retain(|(o, _)| o != owner);
    if list.drawing {
        list.removed.push(owner.to_owned());
    }
}

/// Implementation of the service, sharing the layer list with the feature.
struct Service {
    layers: Layers,
}

impl viewport::Viewport for Service {
    fn add_layer(&self, owner: &str, layer: Box<dyn Layer>) {
        lock(&self.layers).layers.push((owner.to_owned(), layer));
    }

    fn remove_layers(&self, owner: &str) {
        remove(&self.layers, owner);
    }

    fn target(&self) -> Target {
        TARGET
    }
}

/// Offscreen textures, recreated when the panel changes size.
struct Targets {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    resolved: wgpu::TextureView,
    depth: wgpu::TextureView,
    texture_id: egui::TextureId,
}

struct ViewportFeature {
    layers: Layers,
    camera: OrbitCamera,
    targets: Option<Targets>,
    grid: Option<Grid>,
    start: Instant,
}

impl Default for ViewportFeature {
    fn default() -> Self {
        Self {
            layers: Arc::default(),
            camera: OrbitCamera::default(),
            targets: None,
            grid: None,
            start: Instant::now(),
        }
    }
}

impl Feature for ViewportFeature {
    fn register(&mut self, reg: &mut Registrar) {
        let service: viewport::Handle = Arc::new(Service {
            layers: self.layers.clone(),
        });
        reg.panel("view", "3D View", DockArea::Center)
            .provide(viewport::SERVICE, service)
            .subscribe(FEATURE_FAILED_TOPIC)
            .menu_item("View", "Reset camera", "reset_camera");
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(gpu) = ctx.gpu().cloned() else {
            ui.label("No GPU device is available.");
            return;
        };
        let size = ui.available_size().max(egui::vec2(1.0, 1.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        self.camera.handle_input(ui, &response);

        let pixels = size * ui.ctx().pixels_per_point();
        let pixels = [pixels.x.round().max(1.0) as u32, pixels.y.round().max(1.0) as u32];
        self.ensure_targets(&gpu, pixels);
        self.render(&gpu, pixels, ctx);

        let targets = self.targets.as_ref().expect("created above");
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(targets.texture_id, rect, uv, egui::Color32::WHITE);
        let caption = format!(
            "{} layers · drag: orbit · right drag: pan · wheel: zoom",
            lock(&self.layers).layers.len()
        );
        ui.painter().text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            caption,
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(150),
        );
        ui.ctx().request_repaint();
    }

    fn on_event(&mut self, event: &Event, _ctx: &mut Context) {
        if event.topic == FEATURE_FAILED_TOPIC
            && let Some(id) = event.payload.get("id").and_then(|v| v.as_str())
        {
            remove(&self.layers, id);
        }
    }

    fn on_menu(&mut self, action: &str, _ctx: &mut Context) {
        if action == "reset_camera" {
            self.camera = OrbitCamera::default();
        }
    }
}

impl ViewportFeature {
    fn ensure_targets(&mut self, gpu: &egui_wgpu::RenderState, size: [u32; 2]) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let device = &gpu.device;
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let texture = |label, format, samples, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let msaa = texture(
            "viewport msaa",
            TARGET.color_format,
            TARGET.sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let resolved = texture(
            "viewport colour",
            TARGET.color_format,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = texture(
            "viewport depth",
            TARGET.depth_format,
            TARGET.sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );

        let mut renderer = gpu.renderer.write();
        let texture_id = match &self.targets {
            Some(old) => {
                renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    &resolved,
                    wgpu::FilterMode::Linear,
                    old.texture_id,
                );
                old.texture_id
            }
            None => renderer.register_native_texture(device, &resolved, wgpu::FilterMode::Linear),
        };
        self.targets = Some(Targets {
            size,
            msaa,
            resolved,
            depth,
            texture_id,
        });
    }

    fn render(&mut self, gpu: &egui_wgpu::RenderState, size: [u32; 2], ctx: &mut Context) {
        let view = View {
            view_proj: self.camera.view_proj(size[0] as f32 / size[1] as f32),
            eye: self.camera.eye(),
            size,
            time: self.start.elapsed().as_secs_f32(),
        };
        let bundles = self.record_layers(gpu, &view, ctx);
        let targets = self.targets.as_ref().expect("created before rendering");
        let grid = self.grid.get_or_insert_with(|| Grid::new(&gpu.device, &TARGET));
        grid.update(&gpu.queue, &view);

        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewport"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa,
                    depth_slice: None,
                    resolve_target: Some(&targets.resolved),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(BACKGROUND),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            grid.draw(&mut pass);
            pass.execute_bundles(bundles.iter());
        }
        gpu.queue.submit([encoder.finish()]);
    }

    /// Records each layer into its own render bundle, inside a validation error scope. A layer
    /// that panics or records invalid commands is removed and its feature reported; the bundles of
    /// the others are returned.
    fn record_layers(
        &mut self,
        gpu: &egui_wgpu::RenderState,
        view: &View,
        ctx: &mut Context,
    ) -> Vec<wgpu::RenderBundle> {
        // Layers may be added or removed meanwhile, by a layer or by another thread: take the
        // list out, then put it back in front, without the layers removed in between.
        let mut layers = {
            let mut list = lock(&self.layers);
            list.drawing = true;
            std::mem::take(&mut list.layers)
        };
        let mut bundles = Vec::new();
        layers.retain_mut(|(owner, layer)| {
            let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut encoder = gpu
                .device
                .create_render_bundle_encoder(&wgpu::RenderBundleEncoderDescriptor {
                    label: Some(owner.as_str()),
                    color_formats: &[Some(TARGET.color_format)],
                    depth_stencil: Some(wgpu::RenderBundleDepthStencil {
                        format: TARGET.depth_format,
                        depth_read_only: false,
                        stencil_read_only: true,
                    }),
                    sample_count: TARGET.sample_count,
                    multiview: None,
                });
            let layer: &mut dyn Layer = layer.as_mut();
            let recording = &mut encoder;
            // Moved into the closure: the bundle borrows the layer's resources for its whole life.
            let drawn = catch_unwind(AssertUnwindSafe(move || {
                let layer = layer;
                layer.draw(gpu, &TARGET, view, recording)
            }));
            // wgpu 30 validates the recorded commands here and panics on an invalid one instead of
            // reporting it to the error scope, so the panic is caught too.
            let label = owner.as_str();
            let finished = catch_unwind(AssertUnwindSafe(move || {
                encoder.finish(&wgpu::RenderBundleDescriptor { label: Some(label) })
            }));
            let error = resolved(scope.pop()).flatten();
            let failure = match (drawn, finished, error) {
                (Err(payload), _, _) => Err(format!("its viewport layer panicked: {}", panic_text(payload))),
                (Ok(()), Err(payload), _) => Err(format!(
                    "its viewport layer recorded invalid GPU commands: {}",
                    panic_text(payload)
                )),
                (Ok(()), Ok(_), Some(error)) => Err(format!("its viewport layer caused a GPU error: {error}")),
                (Ok(()), Ok(bundle), None) => Ok(bundle),
            };
            match failure {
                Ok(bundle) => {
                    bundles.push(bundle);
                    true
                }
                Err(message) => {
                    ctx.report_failure(owner, &message);
                    false
                }
            }
        });
        let mut list = lock(&self.layers);
        list.drawing = false;
        let removed = std::mem::take(&mut list.removed);
        layers.retain(|(owner, _)| !removed.contains(owner));
        layers.append(&mut list.layers);
        list.layers = layers;
        bundles
    }
}

/// The value of a future that is already complete, as error scopes are on native backends.
fn resolved<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut std::task::Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

fn panic_text(payload: Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic without message".to_owned())
}

uniwow_api::export_feature!(ViewportFeature::default());
