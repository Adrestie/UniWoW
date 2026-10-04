//! A graphics scene shown in a view, as `QGraphicsScene` and `QGraphicsView`. The items are kept on
//! the GPU and drawn through the view's transform: scrolling and zooming cost nothing per item,
//! and a changed item is written again in place. Texts are drawn by egui above the shapes, for the
//! visible items only.

use std::collections::HashMap;
use std::sync::Arc;

use uniwow_api::ui::{Handle, Kind, Object, Signal, SignalData, Ui};
use uniwow_api::{bytemuck, egui, egui_wgpu, wgpu};

use super::painter::{color, text_pixels};

const SAMPLES: u32 = 4;
/// Not sRGB: egui shows a texture's values as they are, so the scene keeps egui's gamma-encoded
/// colours, blended as egui blends its own.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Wheel distance that zooms by a factor e.
const ZOOM_DISTANCE: f32 = 400.0;

const SHADER: &str = r"
struct View { scale: vec2<f32>, offset: vec2<f32>, size: vec2<f32>, padding: vec2<f32> };
@group(0) @binding(0) var<uniform> view: View;

struct Out { @builtin(position) position: vec4<f32>, @location(0) color: vec4<f32> };

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> Out {
    let screen = pos * view.scale + view.offset;
    var out: Out;
    out.position = vec4<f32>(screen.x / view.size.x * 2.0 - 1.0, 1.0 - screen.y / view.size.y * 2.0, 0.0, 1.0);
    // egui colours, gamma-encoded and premultiplied, kept as they are.
    out.color = color;
    return out;
}

@fragment
fn fs(input: Out) -> @location(0) vec4<f32> {
    return input.color;
}
";

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[bytemuck(crate = "uniwow_api::bytemuck")]
struct ViewUniform {
    scale: [f32; 2],
    offset: [f32; 2],
    size: [f32; 2],
    padding: [f32; 2],
}

struct Targets {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    resolved: wgpu::TextureView,
    texture: ViewTexture,
}

/// The texture egui shows a view through, freed with the view.
struct ViewTexture {
    id: egui::TextureId,
    renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
}

impl Drop for ViewTexture {
    fn drop(&mut self) {
        self.renderer.write().free_texture(&self.id);
    }
}

/// Where an item's mesh sits in the buffers.
struct Cached {
    first_vertex: u32,
    vertices: u32,
    indices: u32,
    generation: u64,
}

struct Drag {
    item: Handle,
    start: [f64; 2],
    pointer: egui::Pos2,
    moved: bool,
}

struct Gpu {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    vertices: Option<wgpu::Buffer>,
    indices: Option<wgpu::Buffer>,
    index_count: u32,
    targets: Option<Targets>,
}

/// What the interface thread keeps of one view.
#[derive(Default)]
pub struct SceneView {
    gpu: Option<Gpu>,
    scene: Option<Handle>,
    structure: u64,
    /// The leaf items in drawing order.
    order: Vec<Handle>,
    cached: HashMap<Handle, Cached>,
    drag: Option<Drag>,
}

/// The position of an item in the scene: its own, plus its groups'.
fn absolute(ui: &Ui, handle: Handle) -> [f64; 2] {
    let mut position = [0.0, 0.0];
    let mut next = Some(handle);
    while let Some(current) = next {
        let Some(object) = ui.object(current) else { break };
        if object.kind == Kind::GraphicsScene {
            break;
        }
        position[0] += object.pos[0];
        position[1] += object.pos[1];
        next = object.parent;
    }
    position
}

/// The leaf items of a scene in drawing order: siblings by stacking order, then creation.
fn drawing_order(ui: &Ui, scene: Handle) -> Vec<Handle> {
    let mut order = Vec::new();
    let mut stack = vec![scene];
    let visit = |handle: Handle, order: &mut Vec<Handle>, stack: &mut Vec<Handle>| {
        let Some(object) = ui.object(handle) else { return };
        if !object.visible {
            return;
        }
        if matches!(object.kind, Kind::GraphicsScene | Kind::ItemGroup) {
            let mut children = object.children.clone();
            children.sort_by(|a, b| {
                let z = |h: &Handle| ui.object(*h).map_or(0.0, |o| o.z);
                z(a).total_cmp(&z(b))
            });
            // Pushed in reverse, so that the lowest is drawn first.
            stack.extend(children.into_iter().rev());
        } else {
            order.push(handle);
        }
    };
    let root = stack.pop().expect("scene");
    visit(root, &mut order, &mut stack);
    while let Some(next) = stack.pop() {
        visit(next, &mut order, &mut stack);
    }
    order
}

/// The shape of an item in scene coordinates; none for a text, which egui draws.
fn shape(object: &Object, at: [f64; 2]) -> Option<egui::Shape> {
    let pen = egui::Stroke::new(object.pen_width as f32, color(object.pen_color));
    let [x, y, w, h] = object.rect.map(|v| v as f32);
    let origin = egui::pos2(at[0] as f32, at[1] as f32);
    match object.kind {
        Kind::RectItem => Some(egui::Shape::Rect(egui::epaint::RectShape::new(
            egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(w, h)),
            object.radius as f32,
            color(object.brush_color),
            pen,
            egui::StrokeKind::Inside,
        ))),
        Kind::EllipseItem => Some(egui::Shape::Ellipse(egui::epaint::EllipseShape {
            center: origin + egui::vec2(x + w / 2.0, y + h / 2.0),
            radius: egui::vec2(w / 2.0, h / 2.0),
            fill: color(object.brush_color),
            stroke: pen,
            angle: 0.0,
        })),
        Kind::LineItem => {
            let [x1, y1, x2, y2] = object.line.map(|v| v as f32);
            Some(egui::Shape::line_segment(
                [origin + egui::vec2(x1, y1), origin + egui::vec2(x2, y2)],
                pen,
            ))
        }
        _ => None,
    }
}

/// Whether a scene point falls on an item.
fn hits(object: &Object, at: [f64; 2], point: [f64; 2], scale: f64) -> bool {
    let local = [point[0] - at[0], point[1] - at[1]];
    let [x, y, w, h] = object.rect;
    match object.kind {
        Kind::RectItem => local[0] >= x && local[0] <= x + w && local[1] >= y && local[1] <= y + h,
        Kind::EllipseItem => {
            let (cx, cy, rx, ry) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
            rx > 0.0 && ry > 0.0 && ((local[0] - cx) / rx).powi(2) + ((local[1] - cy) / ry).powi(2) <= 1.0
        }
        Kind::LineItem => {
            let [x1, y1, x2, y2] = object.line;
            let (dx, dy) = (x2 - x1, y2 - y1);
            let length = dx * dx + dy * dy;
            let t = if length > 0.0 {
                (((local[0] - x1) * dx + (local[1] - y1) * dy) / length).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (px, py) = (x1 + t * dx - local[0], y1 + t * dy - local[1]);
            (px * px + py * py).sqrt() <= (object.pen_width / 2.0).max(4.0 / scale)
        }
        Kind::TextItem => {
            let width = object.text.chars().count() as f64 * object.font_size * 0.55;
            local[0] >= 0.0 && local[0] <= width && local[1] >= 0.0 && local[1] <= object.font_size * 1.25
        }
        _ => false,
    }
}

/// The bounds of an item in scene coordinates, for its selection outline.
fn bounds(object: &Object, at: [f64; 2]) -> [f64; 4] {
    match object.kind {
        Kind::LineItem => {
            let [x1, y1, x2, y2] = object.line;
            [at[0] + x1.min(x2), at[1] + y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs()]
        }
        Kind::TextItem => {
            let width = object.text.chars().count() as f64 * object.font_size * 0.55;
            [at[0], at[1], width, object.font_size * 1.25]
        }
        _ => [
            at[0] + object.rect[0],
            at[1] + object.rect[1],
            object.rect[2],
            object.rect[3],
        ],
    }
}

/// The modifier keys held, as `uniwow.h` numbers them.
pub(crate) fn modifiers(ui: &egui::Ui) -> u32 {
    ui.input(|i| {
        u32::from(i.modifiers.command) | (u32::from(i.modifiers.shift) << 1) | (u32::from(i.modifiers.alt) << 2)
    })
}

impl SceneView {
    /// Draws the view `handle` and handles the mouse over it. Signals are added to `events`.
    pub fn show(
        &mut self,
        store: &mut Ui,
        handle: Handle,
        ui: &mut egui::Ui,
        gpu: Option<&egui_wgpu::RenderState>,
        events: &mut Vec<SignalData>,
    ) {
        let Some(view) = store.object(handle).cloned() else {
            return;
        };
        let height = ui.available_height().max(view.minimum_height as f32);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
        let Some(scene) = view.scene.filter(|s| store.object(*s).is_some()) else {
            ui.painter().rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            return;
        };
        if self.scene != Some(scene) || self.structure != store.structure(scene) {
            self.order = drawing_order(store, scene);
        }

        // The transform: the view's centre at the middle of the rectangle.
        let local_center = egui::vec2(rect.width(), rect.height()) / 2.0;
        let to_scene = |p: egui::Pos2, scale: f64, center: [f64; 2]| {
            let local = p - rect.min - local_center;
            [
                center[0] + f64::from(local.x) / scale,
                center[1] + f64::from(local.y) / scale,
            ]
        };
        self.interact(store, handle, scene, &view, &response, ui, events, to_scene);
        let Some(view) = store.object(handle).cloned() else {
            return;
        };
        let (scale, center) = (view.view_scale, view.view_center);
        let offset = [
            local_center.x - (center[0] * scale) as f32,
            local_center.y - (center[1] * scale) as f32,
        ];

        let background = ui.visuals().extreme_bg_color;
        match gpu {
            Some(gpu) => {
                self.update_buffers(store, scene, gpu);
                self.render(gpu, rect, ui.ctx().pixels_per_point(), scale as f32, offset, background);
                if let Some(targets) = &self.gpu.as_ref().and_then(|g| g.targets.as_ref()) {
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    ui.painter().image(targets.texture.id, rect, uv, egui::Color32::WHITE);
                }
            }
            None => {
                ui.painter().rect_filled(rect, 0.0, background);
            }
        }

        // Texts and selection outlines, above the shapes.
        let painter = ui.painter().with_clip_rect(rect);
        let to_screen = |p: [f64; 2]| {
            rect.min + egui::vec2(offset[0], offset[1]) + egui::vec2((p[0] * scale) as f32, (p[1] * scale) as f32)
        };
        for item in &self.order {
            let Some(object) = store.object(*item) else { continue };
            let at = absolute(store, *item);
            if object.kind == Kind::TextItem {
                let position = to_screen(at);
                if let Some(size) = text_pixels((object.font_size * scale) as f32)
                    && size >= 4.0
                    && rect.expand(size * 20.0).contains(position)
                {
                    painter.text(
                        position,
                        egui::Align2::LEFT_TOP,
                        &object.text,
                        egui::FontId::proportional(size),
                        color(object.pen_color),
                    );
                }
            }
            if object.selected {
                let [x, y, w, h] = bounds(object, at);
                let outline = egui::Rect::from_min_max(to_screen([x, y]), to_screen([x + w, y + h])).expand(2.0);
                painter.rect_stroke(outline, 2.0, ui.visuals().selection.stroke, egui::StrokeKind::Outside);
            }
        }
        self.scene = Some(scene);
        self.structure = store.structure(scene);
    }

    /// Zoom, pan, selection, presses and the dragging of movable items.
    #[allow(clippy::too_many_arguments)]
    fn interact(
        &mut self,
        store: &mut Ui,
        view: Handle,
        scene: Handle,
        object: &Object,
        response: &egui::Response,
        ui: &egui::Ui,
        events: &mut Vec<SignalData>,
        to_scene: impl Fn(egui::Pos2, f64, [f64; 2]) -> [f64; 2],
    ) {
        let mut scale = object.view_scale;
        let mut center = object.view_center;
        let pointer = response.hover_pos().or_else(|| response.interact_pointer_pos());

        // The wheel zooms around the pointer.
        let wheel = if response.hovered() {
            ui.input(|i| i.smooth_scroll_delta.y)
        } else {
            0.0
        };
        if wheel != 0.0
            && let Some(pointer) = pointer
        {
            let before = to_scene(pointer, scale, center);
            scale = (scale * f64::from((wheel / ZOOM_DISTANCE).exp())).clamp(0.01, 100.0);
            let after = to_scene(pointer, scale, center);
            center = [center[0] + before[0] - after[0], center[1] + before[1] - after[1]];
        }
        // The middle or right button scrolls.
        if response.dragged_by(egui::PointerButton::Middle) || response.dragged_by(egui::PointerButton::Secondary) {
            let delta = response.drag_delta();
            center = [
                center[0] - f64::from(delta.x) / scale,
                center[1] - f64::from(delta.y) / scale,
            ];
        }
        if let Some(target) = store.object_mut(view) {
            target.view_scale = scale;
            target.view_center = center;
        }

        let pressed = response.hovered() && ui.input(|i| i.pointer.primary_pressed());
        if pressed && let Some(pointer) = pointer {
            let point = to_scene(pointer, scale, center);
            let hit = self.hit(store, point, scale);
            let mut data = SignalData {
                sender: scene,
                signal: Signal::ItemPressed as u32,
                x: point[0],
                y: point[1],
                button: 1,
                modifiers: modifiers(ui),
                ..Default::default()
            };
            if let Some(item) = hit {
                data.item = item;
                events.push(data);
            }
            self.select(store, scene, hit, ui.input(|i| i.modifiers.command), events);
            self.drag = hit.and_then(|item| self.movable(store, item)).map(|item| Drag {
                item,
                start: store.object(item).map_or([0.0; 2], |o| o.pos),
                pointer,
                moved: false,
            });
        }
        if let Some(drag) = &mut self.drag {
            if let Some(current) = ui
                .input(|i| i.pointer.interact_pos())
                .filter(|_| response.dragged_by(egui::PointerButton::Primary))
            {
                let delta = current - drag.pointer;
                if let Some(item) = store.object(drag.item) {
                    let mut pos = drag.start;
                    if item.movable & 1 != 0 {
                        pos[0] += f64::from(delta.x) / scale;
                    }
                    if item.movable & 2 != 0 {
                        pos[1] += f64::from(delta.y) / scale;
                    }
                    if let Some([bx, by, bw, bh]) = item.bounds {
                        pos = [pos[0].clamp(bx, bx + bw.max(0.0)), pos[1].clamp(by, by + bh.max(0.0))];
                    }
                    if pos != item.pos {
                        drag.moved = true;
                        if let Some(target) = store.object_mut(drag.item) {
                            target.pos = pos;
                        }
                        store.moved(drag.item);
                    }
                }
            }
            if !ui.input(|i| i.pointer.primary_down()) {
                let drag = self.drag.take().expect("dragging");
                if drag.moved
                    && let Some(item) = store.object(drag.item)
                {
                    events.push(SignalData {
                        sender: scene,
                        signal: Signal::ItemMoved as u32,
                        item: drag.item,
                        x: item.pos[0],
                        y: item.pos[1],
                        dx: item.pos[0] - drag.start[0],
                        dy: item.pos[1] - drag.start[1],
                        ..Default::default()
                    });
                }
            }
        }
        if response.double_clicked()
            && let Some(pointer) = pointer
        {
            let point = to_scene(pointer, scale, center);
            if let Some(item) = self.hit(store, point, scale) {
                events.push(SignalData {
                    sender: scene,
                    signal: Signal::ItemDoubleClicked as u32,
                    item,
                    x: point[0],
                    y: point[1],
                    ..Default::default()
                });
            }
        }
        if self.drag.is_some() || wheel != 0.0 {
            ui.ctx().request_repaint();
        }
    }

    /// The topmost item under a scene point.
    fn hit(&self, store: &Ui, point: [f64; 2], scale: f64) -> Option<Handle> {
        self.order.iter().rev().copied().find(|item| {
            store
                .object(*item)
                .is_some_and(|object| hits(object, absolute(store, *item), point, scale))
        })
    }

    /// The item a press on `item` moves: itself, or its nearest movable group.
    fn movable(&self, store: &Ui, item: Handle) -> Option<Handle> {
        let mut next = Some(item);
        while let Some(current) = next {
            let object = store.object(current)?;
            if object.kind == Kind::GraphicsScene {
                return None;
            }
            if object.movable != 0 {
                return Some(current);
            }
            next = object.parent;
        }
        None
    }

    /// Selects the item pressed (Ctrl adds or removes it), or nothing on the background.
    fn select(&self, store: &mut Ui, scene: Handle, hit: Option<Handle>, add: bool, events: &mut Vec<SignalData>) {
        let selectable = hit.filter(|item| store.object(*item).is_some_and(|o| o.selectable));
        let mut changed = false;
        for item in &self.order {
            let Some(object) = store.object(*item) else { continue };
            let selected = match selectable {
                Some(target) if target == *item => {
                    if add {
                        !object.selected
                    } else {
                        true
                    }
                }
                _ if add => object.selected,
                _ => false,
            };
            if selected != object.selected {
                changed = true;
                if let Some(target) = store.object_mut(*item) {
                    target.selected = selected;
                }
            }
        }
        if changed {
            events.push(SignalData {
                sender: scene,
                signal: Signal::SelectionChanged as u32,
                ..Default::default()
            });
        }
    }

    fn gpu<'a>(gpu: &'a mut Option<Gpu>, render: &egui_wgpu::RenderState) -> &'a mut Gpu {
        gpu.get_or_insert_with(|| {
            let device = &render.device;
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("graphics scene"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("graphics view"),
                size: std::mem::size_of::<ViewUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("graphics view"),
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
                label: Some("graphics view"),
                layout: &layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                }],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("graphics scene"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let blend = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            };
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("graphics scene"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<egui::epaint::Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Unorm8x4],
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: Some(wgpu::BlendState {
                            color: blend,
                            alpha: blend,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: SAMPLES,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            });
            Gpu {
                pipeline,
                uniform,
                bind_group,
                vertices: None,
                indices: None,
                index_count: 0,
                targets: None,
            }
        })
    }

    /// Writes again the items that changed, in place; builds the buffers again when the scene's
    /// structure changed or an item's mesh changed size.
    fn update_buffers(&mut self, store: &Ui, scene: Handle, render: &egui_wgpu::RenderState) {
        let rebuild = self.scene != Some(scene)
            || self.structure != store.structure(scene)
            || self.gpu.as_ref().is_none_or(|g| g.vertices.is_none());
        let mut tessellator = egui::epaint::Tessellator::new(
            1.0,
            egui::epaint::TessellationOptions {
                feathering: false,
                ..Default::default()
            },
            [1, 1],
            Vec::new(),
        );
        let mut mesh_of = |object: &Object, at: [f64; 2]| {
            let mut mesh = egui::Mesh::default();
            if let Some(shape) = shape(object, at) {
                tessellator.tessellate_shape(shape, &mut mesh);
            }
            mesh
        };
        let gpu = Self::gpu(&mut self.gpu, render);
        let mut resized = false;
        if !rebuild {
            for item in &self.order {
                let (Some(object), Some(cached)) = (store.object(*item), self.cached.get_mut(item)) else {
                    continue;
                };
                if object.generation == cached.generation {
                    continue;
                }
                let mesh = mesh_of(object, absolute(store, *item));
                if mesh.vertices.len() as u32 != cached.vertices || mesh.indices.len() as u32 != cached.indices {
                    resized = true;
                    break;
                }
                if let Some(buffer) = &gpu.vertices {
                    let offset = u64::from(cached.first_vertex) * std::mem::size_of::<egui::epaint::Vertex>() as u64;
                    render
                        .queue
                        .write_buffer(buffer, offset, bytemuck::cast_slice(&mesh.vertices));
                }
                cached.generation = object.generation;
            }
        }
        if rebuild || resized {
            self.rebuild(store, render);
        }
    }

    fn rebuild(&mut self, store: &Ui, render: &egui_wgpu::RenderState) {
        let mut tessellator = egui::epaint::Tessellator::new(
            1.0,
            egui::epaint::TessellationOptions {
                feathering: false,
                ..Default::default()
            },
            [1, 1],
            Vec::new(),
        );
        let mut vertices: Vec<egui::epaint::Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        self.cached.clear();
        for item in &self.order {
            let Some(object) = store.object(*item) else { continue };
            let mut mesh = egui::Mesh::default();
            if let Some(shape) = shape(object, absolute(store, *item)) {
                tessellator.tessellate_shape(shape, &mut mesh);
            }
            let first = vertices.len() as u32;
            self.cached.insert(
                *item,
                Cached {
                    first_vertex: first,
                    vertices: mesh.vertices.len() as u32,
                    indices: mesh.indices.len() as u32,
                    generation: object.generation,
                },
            );
            indices.extend(mesh.indices.iter().map(|i| i + first));
            vertices.extend(mesh.vertices);
        }
        let device = &render.device;
        let gpu = Self::gpu(&mut self.gpu, render);
        let buffer = |label, contents: &[u8], usage| {
            use wgpu::util::DeviceExt;
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: usage | wgpu::BufferUsages::COPY_DST,
            })
        };
        // A buffer is never empty: wgpu refuses empty bindings.
        let empty_vertex = [egui::epaint::Vertex::default()];
        let vertex_bytes: &[u8] = if vertices.is_empty() {
            bytemuck::cast_slice(&empty_vertex)
        } else {
            bytemuck::cast_slice(&vertices)
        };
        let index_bytes: &[u8] = if indices.is_empty() {
            bytemuck::cast_slice(&[0u32])
        } else {
            bytemuck::cast_slice(&indices)
        };
        gpu.vertices = Some(buffer(
            "graphics scene vertices",
            vertex_bytes,
            wgpu::BufferUsages::VERTEX,
        ));
        gpu.indices = Some(buffer("graphics scene indices", index_bytes, wgpu::BufferUsages::INDEX));
        gpu.index_count = indices.len() as u32;
    }

    fn render(
        &mut self,
        render: &egui_wgpu::RenderState,
        rect: egui::Rect,
        pixels_per_point: f32,
        scale: f32,
        offset: [f32; 2],
        background: egui::Color32,
    ) {
        let size = [
            (rect.width() * pixels_per_point).round().max(1.0) as u32,
            (rect.height() * pixels_per_point).round().max(1.0) as u32,
        ];
        let device = &render.device;
        let gpu = Self::gpu(&mut self.gpu, render);
        if gpu.targets.as_ref().is_none_or(|t| t.size != size) {
            let extent = wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            };
            let texture = |label, samples, usage| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: extent,
                        mip_level_count: 1,
                        sample_count: samples,
                        dimension: wgpu::TextureDimension::D2,
                        format: FORMAT,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default())
            };
            let msaa = texture("graphics view msaa", SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
            let resolved = texture(
                "graphics view",
                1,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let texture = match gpu.targets.take() {
                Some(old) => {
                    render.renderer.write().update_egui_texture_from_wgpu_texture(
                        device,
                        &resolved,
                        wgpu::FilterMode::Linear,
                        old.texture.id,
                    );
                    old.texture
                }
                None => ViewTexture {
                    id: render
                        .renderer
                        .write()
                        .register_native_texture(device, &resolved, wgpu::FilterMode::Linear),
                    renderer: render.renderer.clone(),
                },
            };
            gpu.targets = Some(Targets {
                size,
                msaa,
                resolved,
                texture,
            });
        }
        let uniform = ViewUniform {
            scale: [scale, scale],
            offset,
            size: [rect.width(), rect.height()],
            padding: [0.0; 2],
        };
        render.queue.write_buffer(&gpu.uniform, 0, bytemuck::bytes_of(&uniform));
        let targets = gpu.targets.as_ref().expect("created above");
        let clear = background.to_normalized_gamma_f32();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("graphics view"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("graphics view"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa,
                    depth_slice: None,
                    resolve_target: Some(&targets.resolved),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(clear[0]),
                            g: f64::from(clear[1]),
                            b: f64::from(clear[2]),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let (Some(vertices), Some(indices)) = (&gpu.vertices, &gpu.indices)
                && gpu.index_count > 0
            {
                pass.set_pipeline(&gpu.pipeline);
                pass.set_bind_group(0, &gpu.bind_group, &[]);
                pass.set_vertex_buffer(0, vertices.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..gpu.index_count, 0, 0..1);
            }
        }
        render.queue.submit([encoder.finish()]);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{absolute, drawing_order, hits};
    use uniwow_api::ui::{Kind, Property, Ui, lock};

    #[test]
    fn items_are_drawn_by_stacking_order_and_hit_from_the_top() {
        let shared = Ui::new(Arc::new(|job| job()));
        let mut ui = lock(&shared);
        let scene = ui.create(Kind::GraphicsScene, None).unwrap();
        let low = ui.create(Kind::RectItem, Some(scene)).unwrap();
        let group = ui.create(Kind::ItemGroup, Some(scene)).unwrap();
        let high = ui.create(Kind::RectItem, Some(group)).unwrap();
        ui.set_numbers(low, Property::ZValue, &[5.0]).unwrap();
        ui.set_numbers(low, Property::Rect, &[0.0, 0.0, 10.0, 10.0]).unwrap();
        ui.set_numbers(group, Property::Pos, &[100.0, 0.0]).unwrap();
        ui.set_numbers(high, Property::Rect, &[0.0, 0.0, 10.0, 10.0]).unwrap();
        assert_eq!(drawing_order(&ui, scene), vec![high, low], "the higher z is drawn last");
        assert_eq!(absolute(&ui, high), [100.0, 0.0], "a group moves its items");
        let object = ui.object(high).unwrap();
        assert!(hits(object, absolute(&ui, high), [105.0, 5.0], 1.0));
        assert!(!hits(object, absolute(&ui, high), [5.0, 5.0], 1.0));
    }

    #[test]
    fn a_line_is_hit_near_it_whatever_its_width() {
        let shared = Ui::new(Arc::new(|job| job()));
        let mut ui = lock(&shared);
        let scene = ui.create(Kind::GraphicsScene, None).unwrap();
        let line = ui.create(Kind::LineItem, Some(scene)).unwrap();
        ui.set_numbers(line, Property::Line, &[0.0, 0.0, 100.0, 0.0]).unwrap();
        let object = ui.object(line).unwrap();
        assert!(hits(object, [0.0, 0.0], [50.0, 3.0], 1.0));
        assert!(!hits(object, [0.0, 0.0], [50.0, 10.0], 1.0));
        assert!(
            hits(object, [0.0, 0.0], [50.0, 10.0], 0.25),
            "4 pixels on screen at a quarter scale"
        );
    }
}
