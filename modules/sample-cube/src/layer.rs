use std::sync::{Arc, Mutex};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::viewport::{Layer, Phase, Target, View};
use uniwow_api::wgpu::util::DeviceExt;
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

use crate::{Params, lock};

const SHADER: &str = r#"
struct Globals {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    // The inverse transpose of `model`, which keeps normals right under any scale.
    normal: mat4x4<f32>,
    color: vec4<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = globals.view_proj * globals.model * vec4<f32>(position, 1.0);
    out.normal = (globals.normal * vec4<f32>(normal, 0.0)).xyz;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let light = normalize(vec3<f32>(0.4, 0.3, 0.85));
    let diffuse = max(dot(normalize(in.normal), light), 0.0);
    return vec4<f32>(globals.color.rgb * (0.25 + 0.75 * diffuse), 1.0);
}
"#;

/// Floats of the shader's `Globals`.
const GLOBALS: usize = 52;

/// Pipeline and buffers of the cube, built in a job (rule T5) then handed to the layer.
pub struct Gpu {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

pub struct CubeLayer {
    params: Arc<Mutex<Params>>,
    gpu: Option<Gpu>,
    /// Where the job building the GPU resources leaves them.
    incoming: Arc<Mutex<Option<Gpu>>>,
}

impl CubeLayer {
    pub fn new(params: Arc<Mutex<Params>>, incoming: Arc<Mutex<Option<Gpu>>>) -> Self {
        Self {
            params,
            gpu: None,
            incoming,
        }
    }
}

impl Layer for CubeLayer {
    fn draw<'a>(
        &'a mut self,
        gpu: &egui_wgpu::RenderState,
        _target: &Target,
        view: &View,
        phase: Phase,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    ) {
        // Nothing blended.
        if phase != Phase::Opaque {
            return;
        }
        if self.gpu.is_none() {
            self.gpu = lock(&self.incoming).take();
        }
        // Nothing to draw until the job has built the resources.
        let Some(resources) = self.gpu.as_ref() else {
            return;
        };
        let (model, color) = {
            let params = lock(&self.params);
            let [x, y, z] = params.rotation.map(f32::to_radians);
            let model = Mat4::from_translation(Vec3::from(params.position))
                * Mat4::from_rotation_z(z)
                * Mat4::from_rotation_y(y)
                * Mat4::from_rotation_x(x)
                * Mat4::from_scale(Vec3::from(params.scale));
            (model, params.color)
        };
        let mut globals = [0f32; GLOBALS];
        globals[0..16].copy_from_slice(&view.view_proj.to_cols_array());
        globals[16..32].copy_from_slice(&model.to_cols_array());
        globals[32..48].copy_from_slice(&model.inverse().transpose().to_cols_array());
        globals[48..51].copy_from_slice(&color);
        globals[51] = 1.0;
        gpu.queue
            .write_buffer(&resources.globals, 0, bytemuck::cast_slice(&globals));

        bundle.set_pipeline(&resources.pipeline);
        bundle.set_bind_group(0, &resources.bind_group, &[]);
        bundle.set_vertex_buffer(0, resources.vertices.slice(..));
        bundle.draw(0..36, 0..1);
    }
}

/// Six faces of a cube of side 2 centred on the origin: position then normal.
fn vertices() -> Vec<[f32; 6]> {
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ];
    let mut out = Vec::new();
    for (n, u, v) in faces {
        let (n, u, v) = (Vec3::from(n), Vec3::from(u), Vec3::from(v));
        let corner = |a: f32, b: f32| n + u * a + v * b;
        for (a, b) in [
            (-1.0, -1.0),
            (1.0, -1.0),
            (1.0, 1.0),
            (-1.0, -1.0),
            (1.0, 1.0),
            (-1.0, 1.0),
        ] {
            let p = corner(a, b);
            out.push([p.x, p.y, p.z, n.x, n.y, n.z]);
        }
    }
    out
}

pub fn create(device: &wgpu::Device, target: &Target) -> Gpu {
    let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cube vertices"),
        contents: bytemuck::cast_slice(&vertices()),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let globals = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cube globals"),
        size: (GLOBALS * 4) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("cube"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cube"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: globals.as_entire_binding(),
        }],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cube"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("cube"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cube"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 24,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
            })],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: target.depth_format,
            depth_write_enabled: Some(true),
            depth_compare: Some(target.depth_compare),
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
    Gpu {
        pipeline,
        vertices,
        globals,
        bind_group,
    }
}
