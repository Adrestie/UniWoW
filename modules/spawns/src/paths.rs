//! The paths of the creatures that walk one, read from the world database with their spawns: the
//! points of `waypoint_data` their `creature_addon` (or their template's) names, and those of
//! `waypoints` the SmartAI scripts start; drawn as red lines in the 3D view, for the spawns selected
//! or for all. Read only.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uniwow_api::db::Rows;
use uniwow_api::serde_json::Value;
use uniwow_api::viewport::{Layer, Phase, Target, View};
use uniwow_api::{bytemuck, egui_wgpu, wgpu};

/// The paths of `creature_addon` and `creature_template_addon` of the creatures of a map: the
/// spawn, the path, the point and its place; `entry` the column of a creature's template.
pub fn addon_paths(entry: &str) -> String {
    format!(
        "SELECT c.guid, w.id AS path, w.point, w.position_x, w.position_y, w.position_z \
         FROM creature c LEFT JOIN creature_addon a ON a.guid = c.guid \
         LEFT JOIN creature_template_addon t ON t.entry = c.{entry} \
         JOIN waypoint_data w ON w.id = IF(a.path_id IS NOT NULL AND a.path_id <> 0, a.path_id, t.path_id) \
         WHERE c.map = ? ORDER BY c.guid, w.point"
    )
}

/// The paths the SmartAI scripts of the creatures of a map start (`SMART_ACTION_WP_START`, 53), of
/// their template or of the spawn itself.
pub fn smart_paths(entry: &str) -> String {
    format!(
        "SELECT c.guid, w.entry AS path, w.pointid, w.position_x, w.position_y, w.position_z \
         FROM creature c JOIN smart_scripts s ON s.source_type = 0 AND s.action_type = 53 \
         AND (s.entryorguid = c.{entry} OR s.entryorguid = -CAST(c.guid AS SIGNED)) \
         JOIN waypoints w ON w.entry = s.action_param2 WHERE c.map = ? ORDER BY c.guid, w.entry, w.pointid"
    )
}

/// The paths of each spawn by its guid, each its points in order, in the world.
pub type Paths = BTreeMap<u64, Vec<Vec<[f32; 3]>>>;

/// The points of a path as read, each its number and its place.
type Points = Vec<(u64, [f32; 3])>;

/// The paths of `rows` (spawn, path, point, x, y, z) added to `paths`, a path of a spawn once.
pub fn add_paths(paths: &mut Paths, rows: &Rows) {
    let number = |value: Option<&Value>| match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    };
    let mut by_path: BTreeMap<(u64, u64), Points> = BTreeMap::new();
    for row in &rows.rows {
        let (Some(guid), Some(path), Some(point), Some(x), Some(y), Some(z)) = (
            number(row.first()),
            number(row.get(1)),
            number(row.get(2)),
            number(row.get(3)),
            number(row.get(4)),
            number(row.get(5)),
        ) else {
            continue;
        };
        by_path
            .entry((guid as u64, path as u64))
            .or_default()
            .push((point as u64, [x as f32, y as f32, z as f32]));
    }
    for ((guid, _), mut points) in by_path {
        points.sort_by_key(|(point, _)| *point);
        let points: Vec<[f32; 3]> = points.into_iter().map(|(_, at)| at).collect();
        if points.len() < 2 {
            continue;
        }
        let held = paths.entry(guid).or_default();
        if !held.contains(&points) {
            held.push(points);
        }
    }
}

/// How high above its points a path is drawn, so that the ground hides it less.
const LIFT: f32 = 0.4;

/// A segment drawn: from, to, and its colour, red, green, blue and alpha in gamma.
pub type Segment = [f32; 10];

/// The colour of the paths.
const PATH: [f32; 4] = [1.0, 0.12, 0.08, 1.0];
/// The colour of the places of spawn and of the line to the creature.
const SPAWN: [f32; 4] = [1.0, 0.85, 0.2, 1.0];

fn segment(from: [f32; 3], to: [f32; 3], colour: [f32; 4]) -> Segment {
    [
        from[0], from[1], from[2], to[0], to[1], to[2], colour[0], colour[1], colour[2], colour[3],
    ]
}

/// The segments of the paths of the spawns `guids`: the path closed, a creature walking it again
/// from its start.
pub fn segments<'a>(paths: &Paths, guids: impl IntoIterator<Item = &'a u64>) -> Vec<Segment> {
    let mut out = Vec::new();
    for guid in guids {
        for path in paths.get(guid).into_iter().flatten() {
            let closing = (path.len() > 2).then(|| (path[path.len() - 1], path[0]));
            for (from, to) in path.windows(2).map(|pair| (pair[0], pair[1])).chain(closing) {
                let lifted = |at: [f32; 3]| [at[0], at[1], at[2] + LIFT];
                out.push(segment(lifted(from), lifted(to), PATH));
            }
        }
    }
    out
}

/// The radius of the circle on a place of spawn, in yards, and the length of a dash and of a gap
/// of the line from it to its creature.
const RING: f32 = 1.0;
const DASH: f32 = 0.8;

/// The place of spawn `spawn` of a creature standing at `now`: a circle on it, and a dotted line
/// from it to the creature, unless it stands there.
pub fn spawn_marks(spawn: [f32; 3], now: [f32; 3]) -> Vec<Segment> {
    let at = |angle: f32| {
        [
            spawn[0] + RING * angle.cos(),
            spawn[1] + RING * angle.sin(),
            spawn[2] + LIFT,
        ]
    };
    let sides = 24;
    let step = std::f32::consts::TAU / sides as f32;
    let mut out: Vec<Segment> = (0..sides)
        .map(|side| segment(at(side as f32 * step), at((side + 1) as f32 * step), SPAWN))
        .collect();
    let (from, to) = ([spawn[0], spawn[1], spawn[2] + LIFT], [now[0], now[1], now[2] + LIFT]);
    let length = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2) + (to[2] - from[2]).powi(2)).sqrt();
    if length <= RING {
        return out;
    }
    let along = |distance: f32| {
        let share = distance / length;
        [
            from[0] + (to[0] - from[0]) * share,
            from[1] + (to[1] - from[1]) * share,
            from[2] + (to[2] - from[2]) * share,
        ]
    };
    // Dashes from the edge of the circle, a gap of a dash between two.
    let mut start = RING;
    while start < length {
        let end = (start + DASH).min(length);
        out.push(segment(along(start), along(end), SPAWN));
        start += 2.0 * DASH;
    }
    out
}

/// The segments to draw, shared by the module and its layer, with a number changed with them.
#[derive(Default)]
pub struct Drawn {
    pub segments: Vec<Segment>,
    pub version: u64,
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

const SHADER: &str = r#"
struct Globals { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) colour: vec4<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vertex: u32,
    @location(0) seg_start: vec3<f32>,
    @location(1) seg_end: vec3<f32>,
    @location(2) colour: vec4<f32>,
) -> Out {
    var out: Out;
    out.position = globals.view_proj * vec4<f32>(select(seg_start, seg_end, vertex == 1u), 1.0);
    out.colour = colour;
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    return in.colour;
}
"#;

struct Gpu {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
}

/// The layer drawing the paths: one draw of instanced lines, a segment 40 bytes with its colour.
pub struct PathsLayer {
    drawn: Arc<Mutex<Drawn>>,
    target: Target,
    gpu: Option<Gpu>,
    seen: Option<u64>,
    count: u32,
    version: u64,
}

impl PathsLayer {
    pub fn new(drawn: Arc<Mutex<Drawn>>, target: Target) -> Self {
        Self {
            drawn,
            target,
            gpu: None,
            seen: None,
            count: 0,
            version: 0,
        }
    }

    fn build(device: &wgpu::Device, target: &Target) -> Gpu {
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("patrol paths globals"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("patrol paths"),
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
            label: Some("patrol paths"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("patrol paths"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("patrol paths"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("patrol paths"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 40,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: target.depth_format,
                depth_write_enabled: Some(false),
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
            globals,
            bind_group,
            instances: Self::instances(device, 256),
            capacity: 256,
        }
    }

    fn instances(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("patrol paths"),
            size: (capacity * 40) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }
}

impl Layer for PathsLayer {
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        let target = self.target;
        let state = self.gpu.get_or_insert_with(|| Self::build(&gpu.device, &target));
        let globals = view.view_proj.to_cols_array();
        gpu.queue
            .write_buffer(&state.globals, 0, bytemuck::cast_slice(&globals));
        let drawn = lock(&self.drawn);
        if self.seen == Some(drawn.version) {
            return;
        }
        self.seen = Some(drawn.version);
        if drawn.segments.len() > state.capacity {
            state.capacity = drawn.segments.len().next_power_of_two();
            state.instances = Self::instances(&gpu.device, state.capacity);
        }
        if !drawn.segments.is_empty() {
            gpu.queue
                .write_buffer(&state.instances, 0, bytemuck::cast_slice(&drawn.segments));
        }
        self.count = drawn.segments.len() as u32;
        self.version += 1;
    }

    fn version(&self) -> Option<u64> {
        Some(self.version)
    }

    fn draw<'a>(
        &'a mut self,
        _gpu: &egui_wgpu::RenderState,
        _target: &Target,
        _view: &View,
        phase: Phase,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    ) {
        let Some(state) = &self.gpu else {
            return;
        };
        if phase != Phase::Near || self.count == 0 {
            return;
        }
        bundle.set_pipeline(&state.pipeline);
        bundle.set_bind_group(0, &state.bind_group, &[]);
        bundle.set_vertex_buffer(0, state.instances.slice(..));
        bundle.draw(0..2, 0..self.count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::serde_json::json;

    fn rows(rows: Vec<Vec<Value>>) -> Rows {
        Rows {
            columns: ["guid", "path", "point", "position_x", "position_y", "position_z"]
                .map(str::to_owned)
                .to_vec(),
            rows,
        }
    }

    #[test]
    fn the_points_of_a_path_are_read_in_order_once_and_drawn_closed() {
        let mut paths = Paths::new();
        // A patrol of three points, given out of order, a point written as text.
        add_paths(
            &mut paths,
            &rows(vec![
                vec![
                    json!(79753),
                    json!(797530),
                    json!(2),
                    json!(10.0),
                    json!(0.0),
                    json!(1.0),
                ],
                vec![
                    json!(79753),
                    json!(797530),
                    json!(1),
                    json!(0.0),
                    json!(0.0),
                    json!(1.0),
                ],
                vec![
                    json!(79753),
                    json!(797530),
                    json!(3),
                    json!("10.0"),
                    json!(10.0),
                    json!(1.0),
                ],
                vec![json!(5), json!(50), json!(1), json!(0.0), json!(0.0), json!(0.0)],
            ]),
        );
        // The same path read again by the SmartAI query: kept once.
        add_paths(
            &mut paths,
            &rows(vec![
                vec![json!(79753), json!(7), json!(1), json!(0.0), json!(0.0), json!(1.0)],
                vec![json!(79753), json!(7), json!(2), json!(10.0), json!(0.0), json!(1.0)],
                vec![json!(79753), json!(7), json!(3), json!(10.0), json!(10.0), json!(1.0)],
            ]),
        );
        assert_eq!(
            paths[&79753],
            [vec![[0.0, 0.0, 1.0], [10.0, 0.0, 1.0], [10.0, 10.0, 1.0]]]
        );
        assert!(!paths.contains_key(&5), "one point is no path");
        let drawn = segments(&paths, &[79753, 1]);
        assert_eq!(drawn.len(), 3, "two segments and the one back to its start");
        assert_eq!(drawn[0][..6], [0.0, 0.0, 1.4, 10.0, 0.0, 1.4]);
        assert_eq!(drawn[2][..6], [10.0, 10.0, 1.4, 0.0, 0.0, 1.4]);
        assert_eq!(drawn[0][6..], PATH);
        assert!(segments(&paths, &[]).is_empty());
    }
}

#[cfg(test)]
mod wgsl_tests {
    use super::*;

    #[test]
    fn its_shaders_parse_and_validate() {
        uniwow_api::viewport::check_wgsl(SHADER).unwrap_or_else(|error| panic!("patrol paths: {error}"));
    }

    #[test]
    fn a_place_of_spawn_is_a_circle_and_a_dotted_line_to_its_creature() {
        let marks = spawn_marks([0.0, 0.0, 0.0], [10.0, 0.0, 0.0]);
        let circle: Vec<&Segment> = marks.iter().take(24).collect();
        for side in &circle {
            let radius = (side[0].powi(2) + side[1].powi(2)).sqrt();
            assert!((radius - RING).abs() < 1e-4, "on the circle");
        }
        let dashes = &marks[24..];
        assert_eq!(dashes.len(), 6, "from 1 to 10 yards, 0.8 drawn then 0.8 left");
        assert!((dashes[0][0] - RING).abs() < 1e-4 && (dashes[0][3] - (RING + DASH)).abs() < 1e-4);
        assert!(dashes.iter().all(|dash| dash[3] <= 10.0 + 1e-4));
        assert_eq!(marks[0][6..], SPAWN);
        assert_eq!(
            spawn_marks([0.0; 3], [0.5, 0.0, 0.0]).len(),
            24,
            "standing at its place: the circle alone"
        );
    }
}
