//! The 3D view. Draws a ground grid and the layers that other features add through the
//! "viewport" service, into an offscreen target shown in its panel.

mod camera;
mod grid;

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::time::Instant;

use uniwow_api::viewport::{self, Layer, Target, View};
use uniwow_api::{Context, DockArea, Event, FEATURE_FAILED_TOPIC, Feature, Registrar, egui, egui_wgpu, log, wgpu};

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

type Layers = Rc<RefCell<Vec<(String, Box<dyn Layer>)>>>;

/// Implementation of the service, sharing the layer list with the feature.
struct Service {
    layers: Layers,
}

impl viewport::Viewport for Service {
    fn add_layer(&self, owner: &str, layer: Box<dyn Layer>) {
        self.layers.borrow_mut().push((owner.to_owned(), layer));
    }

    fn remove_layers(&self, owner: &str) {
        self.layers.borrow_mut().retain(|(o, _)| o != owner);
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
            layers: Rc::default(),
            camera: OrbitCamera::default(),
            targets: None,
            grid: None,
            start: Instant::now(),
        }
    }
}

impl Feature for ViewportFeature {
    fn register(&mut self, reg: &mut Registrar) {
        let service: viewport::Handle = Rc::new(Service {
            layers: self.layers.clone(),
        });
        reg.panel("view", "3D View", DockArea::Center)
            .provide(viewport::SERVICE, service)
            .subscribe(FEATURE_FAILED_TOPIC)
            .menu_item("View", "Reset camera", "reset_camera");
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(gpu) = ctx.gpu() else {
            ui.label("No GPU device is available.");
            return;
        };
        let size = ui.available_size().max(egui::vec2(1.0, 1.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        self.camera.handle_input(ui, &response);

        let pixels = size * ui.ctx().pixels_per_point();
        let pixels = [pixels.x.round().max(1.0) as u32, pixels.y.round().max(1.0) as u32];
        self.ensure_targets(gpu, pixels);
        self.render(gpu, pixels);

        let targets = self.targets.as_ref().expect("created above");
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(targets.texture_id, rect, uv, egui::Color32::WHITE);
        let caption = format!(
            "{} layers · drag: orbit · right drag: pan · wheel: zoom",
            self.layers.borrow().len()
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
            self.layers.borrow_mut().retain(|(owner, _)| owner != id);
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

    fn render(&mut self, gpu: &egui_wgpu::RenderState, size: [u32; 2]) {
        let view = View {
            view_proj: self.camera.view_proj(size[0] as f32 / size[1] as f32),
            eye: self.camera.eye(),
            size,
            time: self.start.elapsed().as_secs_f32(),
        };
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

            // Layers may add layers while drawing: take the list out, then put it back in front.
            let mut layers = std::mem::take(&mut *self.layers.borrow_mut());
            layers.retain_mut(|(owner, layer)| {
                let drawn = catch_unwind(AssertUnwindSafe(|| layer.draw(gpu, &TARGET, &view, &mut pass)));
                if drawn.is_err() {
                    log::error!("the layer of '{owner}' failed and was removed");
                }
                drawn.is_ok()
            });
            let mut shared = self.layers.borrow_mut();
            layers.append(&mut shared);
            *shared = layers;
        }
        gpu.queue.submit([encoder.finish()]);
    }
}

uniwow_api::export_feature!(ViewportFeature::default());
