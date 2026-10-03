//! Sample feature: a viewport layer drawing a red triangle, which records invalid GPU commands on
//! request. The viewport must then disable this feature only, keeping the grid and the others.

use std::cell::Cell;
use std::rc::Rc;

use uniwow_api::viewport::{self, Layer, Target, View};
use uniwow_api::{Context, DockArea, Feature, Registrar, bytemuck, egui, egui_wgpu, log, wgpu};

const SHADER: &str = r#"
struct Globals { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corners = array(vec2<f32>(3.0, -1.0), vec2<f32>(5.0, -1.0), vec2<f32>(4.0, 1.0));
    let corner = corners[index];
    return globals.view_proj * vec4<f32>(corner.x, corner.y, 0.05, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.9, 0.15, 0.1, 1.0);
}
"#;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Valid,
    /// Draws without setting the bind group the pipeline needs.
    NoBindGroup,
    /// Uses a pipeline built for another colour format than the viewport's.
    WrongFormat,
}

#[derive(Default)]
struct FaultyFeature {
    mode: Rc<Cell<Mode>>,
    drawn: bool,
}

impl Feature for FaultyFeature {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("faulty", "Faulty layer", DockArea::Right);
    }

    fn init(&mut self, ctx: &mut Context) {
        match ctx.service(viewport::SERVICE) {
            Some(view) => {
                view.add_layer(ctx.feature_id(), Box::new(FaultyLayer::new(self.mode.clone())));
                self.drawn = true;
            }
            None => log::info!("no viewport service: the triangle is not drawn"),
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, _ctx: &mut Context) {
        if !self.drawn {
            ui.colored_label(ui.visuals().warn_fg_color, "No 3D view: nothing is drawn.");
            return;
        }
        ui.label("Draws a red triangle next to the cube. Each button makes the next frame record invalid commands:");
        if ui.button("Draw without the bind group").clicked() {
            self.mode.set(Mode::NoBindGroup);
        }
        if ui.button("Use a pipeline with the wrong colour format").clicked() {
            self.mode.set(Mode::WrongFormat);
        }
        ui.weak("Expected: the grid and the cube stay, only this feature is marked as failed.");
    }
}

struct Gpu {
    valid: wgpu::RenderPipeline,
    wrong_format: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

struct FaultyLayer {
    mode: Rc<Cell<Mode>>,
    gpu: Option<Gpu>,
}

impl FaultyLayer {
    fn new(mode: Rc<Cell<Mode>>) -> Self {
        Self { mode, gpu: None }
    }
}

impl Layer for FaultyLayer {
    fn draw<'a>(
        &'a mut self,
        gpu: &egui_wgpu::RenderState,
        target: &Target,
        view: &View,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    ) {
        let resources = &*self.gpu.get_or_insert_with(|| create(&gpu.device, target));
        gpu.queue.write_buffer(
            &resources.globals,
            0,
            bytemuck::cast_slice(&view.view_proj.to_cols_array()),
        );
        match self.mode.get() {
            Mode::Valid => {
                bundle.set_pipeline(&resources.valid);
                bundle.set_bind_group(0, &resources.bind_group, &[]);
            }
            Mode::NoBindGroup => bundle.set_pipeline(&resources.valid),
            Mode::WrongFormat => {
                bundle.set_pipeline(&resources.wrong_format);
                bundle.set_bind_group(0, &resources.bind_group, &[]);
            }
        }
        bundle.draw(0..3, 0..1);
    }
}

fn create(device: &wgpu::Device, target: &Target) -> Gpu {
    let globals = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("faulty globals"),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("faulty"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("faulty"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: globals.as_entire_binding(),
        }],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("faulty"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("faulty"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = |color_format: wgpu::TextureFormat| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("faulty"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: target.depth_format,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: target.sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(color_format.into())],
            }),
            multiview_mask: None,
            cache: None,
        })
    };
    Gpu {
        valid: pipeline(target.color_format),
        wrong_format: pipeline(wgpu::TextureFormat::Rgba8Unorm),
        globals,
        bind_group,
    }
}

uniwow_api::export_feature!(FaultyFeature::default());
