use uniwow_api::viewport::{Target, View};
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, wgpu};

const HALF_EXTENT: i32 = 50;

const SHADER: &str = r#"
struct Globals { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) color: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = globals.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

/// Lines on the ground every unit, brighter every ten, X axis red, Y axis green.
pub struct Grid {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    vertex_count: u32,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl Grid {
    pub fn new(device: &wgpu::Device, target: &Target) -> Self {
        let mut data: Vec<[f32; 7]> = Vec::new();
        let extent = HALF_EXTENT as f32;
        for i in -HALF_EXTENT..=HALF_EXTENT {
            let t = i as f32;
            let shade = if i % 10 == 0 { 0.16 } else { 0.05 };
            let x_axis = if i == 0 {
                [0.6, 0.06, 0.06, 1.0]
            } else {
                [shade, shade, shade, 1.0]
            };
            let y_axis = if i == 0 {
                [0.06, 0.5, 0.06, 1.0]
            } else {
                [shade, shade, shade, 1.0]
            };
            // Line parallel to X (at y = t), then parallel to Y (at x = t).
            data.push([-extent, t, 0.0, x_axis[0], x_axis[1], x_axis[2], x_axis[3]]);
            data.push([extent, t, 0.0, x_axis[0], x_axis[1], x_axis[2], x_axis[3]]);
            data.push([t, -extent, 0.0, y_axis[0], y_axis[1], y_axis[2], y_axis[3]]);
            data.push([t, extent, 0.0, y_axis[0], y_axis[1], y_axis[2], y_axis[3]]);
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grid vertices"),
            contents: bytemuck::cast_slice(&data),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grid globals"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grid"),
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
            label: Some("grid"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grid"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grid"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grid"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 28,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
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
                targets: &[Some(target.color_format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            vertices,
            vertex_count: data.len() as u32,
            globals,
            bind_group,
        }
    }

    pub fn update(&self, queue: &wgpu::Queue, view: &View) {
        queue.write_buffer(&self.globals, 0, bytemuck::cast_slice(&view.view_proj.to_cols_array()));
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..self.vertex_count, 0..1);
    }
}
