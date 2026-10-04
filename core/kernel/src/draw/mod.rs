//! Draws the panels of a module from its interface objects, on the interface thread, and turns
//! what the user does into signals.

use std::collections::HashMap;
use std::sync::Arc;

mod painter;
mod scene;

use painter::replay;
use scene::{SceneView, modifiers};
use uniwow_api::curve::{CurveChange, CurveEditor, CurveOptions, ShownCurve, TimeAxis};
use uniwow_api::ui::{Handle, Kind, Object, SharedUi, Signal, SignalData, Ui, lock};
use uniwow_api::{egui, egui_wgpu};

/// What the interface thread keeps of a module's panels between frames.
#[derive(Default)]
pub struct PanelView {
    scenes: HashMap<Handle, SceneView>,
    /// The size each painting area was last asked to paint.
    painted: HashMap<Handle, [f64; 2]>,
    /// The height, or width, each child of a box layout that does not expand took last frame.
    sizes: HashMap<Handle, f32>,
    /// The curve editor of the module `curves`, which draws the curve views, when it runs.
    curve_editor: Option<Arc<dyn CurveEditor>>,
    /// The time axis of each curve view.
    time_axes: HashMap<Handle, TimeAxis>,
    /// Why the curve editor failed while drawing a curve view, for its provider to be reported.
    editor_failure: Option<String>,
}

/// The text of a panic.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".to_owned())
}

/// Whether an object takes the room left in its layout, as a widget whose size policy expands
/// does in Qt: a graphics view, a painting area, or a layout or group box holding one.
fn expands(store: &Ui, handle: Handle) -> bool {
    store.object(handle).is_some_and(|object| {
        object.visible
            && match object.kind {
                Kind::GraphicsView | Kind::PaintArea | Kind::CurveView => true,
                Kind::VBoxLayout | Kind::HBoxLayout | Kind::GridLayout | Kind::GroupBox => {
                    object.children.iter().any(|child| expands(store, *child))
                }
                _ => false,
            }
    })
}

impl PanelView {
    /// The curve editor to draw the curve views with, from the service of the module `curves`.
    pub fn set_curve_editor(&mut self, editor: Option<Arc<dyn CurveEditor>>) {
        self.curve_editor = editor;
    }

    /// Why the curve editor panicked, once: its provider is the culprit (F5).
    pub fn take_editor_failure(&mut self) -> Option<String> {
        self.editor_failure.take()
    }

    /// Forgets what it kept of the objects that are gone; a view's texture goes with it.
    fn forget_gone(&mut self, store: &Ui) {
        let alive = |handle: &Handle| store.object(*handle).is_some();
        self.scenes.retain(|handle, _| alive(handle));
        self.painted.retain(|handle, _| alive(handle));
        self.sizes.retain(|handle, _| alive(handle));
        self.time_axes.retain(|handle, _| alive(handle));
    }

    /// Draws the panel `panel` of a module.
    pub fn show(&mut self, shared: &SharedUi, panel: &str, ui: &mut egui::Ui, gpu: Option<&egui_wgpu::RenderState>) {
        let mut store = lock(shared);
        store.set_wake(ui.ctx());
        self.forget_gone(&store);
        let layout = store
            .find_panel(panel)
            .and_then(|handle| store.object(handle))
            .and_then(|object| object.children.first().copied());
        let Some(layout) = layout else {
            ui.weak("Waiting for its module to fill this panel.");
            return;
        };
        let mut events = Vec::new();
        self.object(&mut store, shared, layout, ui, gpu, &mut events);
        for event in events {
            store.emit(event);
        }
    }

    /// Draws the dialogs of a module that are shown, each in a modal window over the editor, and
    /// returns the layers of those windows. The user closes one with Escape or its close button:
    /// it is then hidden and sends `Rejected`.
    pub fn dialogs(
        &mut self,
        shared: &SharedUi,
        ctx: &egui::Context,
        gpu: Option<&egui_wgpu::RenderState>,
    ) -> Vec<egui::LayerId> {
        let mut store = lock(shared);
        store.set_wake(ctx);
        self.forget_gone(&store);
        let mut events = Vec::new();
        let mut layers = Vec::new();
        for handle in store.dialogs() {
            let Some(object) = store.object(handle).cloned() else {
                continue;
            };
            // Each module numbers its objects from 1: its store tells its dialogs apart.
            let id = egui::Id::new(("uniwow-dialog", std::sync::Arc::as_ptr(shared) as usize, handle));
            let modal = egui::Modal::new(id).show(ctx, |ui| {
                ui.set_min_width(320.0);
                let closed = ui
                    .horizontal(|ui| {
                        ui.strong(&object.title);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), close_button)
                            .inner
                    })
                    .inner;
                ui.separator();
                if let Some(layout) = object.children.first() {
                    self.object(&mut store, shared, *layout, ui, gpu, &mut events);
                }
                closed
            });
            layers.push(modal.response.layer_id);
            let escape = modal.is_top_modal && ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if modal.inner || escape {
                if let Some(target) = store.object_mut(handle) {
                    target.visible = false;
                }
                events.push(SignalData {
                    sender: handle,
                    signal: Signal::Rejected as u32,
                    ..Default::default()
                });
            }
        }
        for event in events {
            store.emit(event);
        }
        layers
    }

    fn object(
        &mut self,
        store: &mut Ui,
        shared: &SharedUi,
        handle: Handle,
        ui: &mut egui::Ui,
        gpu: Option<&egui_wgpu::RenderState>,
        events: &mut Vec<SignalData>,
    ) {
        let Some(object) = store.object(handle).cloned() else {
            return;
        };
        if !object.visible {
            return;
        }
        ui.push_id(handle, |ui| {
            ui.add_enabled_ui(object.enabled, |ui| {
                let response = self.widget(store, shared, handle, &object, ui, gpu, events);
                if let Some(response) = response
                    && !object.tooltip.is_empty()
                {
                    response.on_hover_text(&object.tooltip);
                }
            });
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn widget(
        &mut self,
        store: &mut Ui,
        shared: &SharedUi,
        handle: Handle,
        object: &Object,
        ui: &mut egui::Ui,
        gpu: Option<&egui_wgpu::RenderState>,
        events: &mut Vec<SignalData>,
    ) -> Option<egui::Response> {
        let signal = |signal: Signal| SignalData {
            sender: handle,
            signal: signal as u32,
            ..Default::default()
        };
        match object.kind {
            Kind::Label => Some(ui.label(&object.text)),
            Kind::PushButton => {
                let response = ui.button(&object.text);
                if response.clicked() {
                    events.push(signal(Signal::Clicked));
                }
                Some(response)
            }
            Kind::CheckBox => {
                let mut checked = object.checked;
                let response = ui.checkbox(&mut checked, &object.text);
                if response.changed() {
                    if let Some(target) = store.object_mut(handle) {
                        target.checked = checked;
                    }
                    events.push(SignalData {
                        boolean: checked,
                        ..signal(Signal::Toggled)
                    });
                }
                Some(response)
            }
            Kind::Slider => {
                let mut value = object.value;
                let mut slider = egui::Slider::new(&mut value, object.minimum..=object.maximum)
                    .show_value(false)
                    .fixed_decimals(object.decimals as usize);
                if object.step > 0.0 {
                    slider = slider.step_by(object.step);
                }
                let response = ui.add(slider);
                if response.drag_started() {
                    events.push(SignalData {
                        number: value,
                        ..signal(Signal::SliderPressed)
                    });
                }
                if response.changed() {
                    if let Some(target) = store.object_mut(handle) {
                        target.value = value;
                    }
                    events.push(SignalData {
                        number: value,
                        ..signal(Signal::ValueChanged)
                    });
                }
                // Also after a click or a key, so that the module always learns the final value.
                if response.drag_stopped() || (response.changed() && !response.dragged()) {
                    events.push(SignalData {
                        number: value,
                        ..signal(Signal::SliderReleased)
                    });
                }
                Some(response)
            }
            Kind::SpinBox => {
                let mut value = object.value;
                let response = ui.add(
                    egui::DragValue::new(&mut value)
                        .range(object.minimum..=object.maximum)
                        .speed(object.step.max(0.001))
                        .fixed_decimals(object.decimals as usize),
                );
                if response.changed() {
                    if let Some(target) = store.object_mut(handle) {
                        target.value = value;
                    }
                    events.push(SignalData {
                        number: value,
                        ..signal(Signal::ValueChanged)
                    });
                }
                if response.drag_stopped() || response.lost_focus() {
                    events.push(SignalData {
                        number: value,
                        ..signal(Signal::EditingFinished)
                    });
                }
                Some(response)
            }
            Kind::LineEdit => {
                let mut text = object.text.clone();
                let response = ui.add(egui::TextEdit::singleline(&mut text).hint_text(&object.placeholder));
                if response.changed() {
                    if let Some(target) = store.object_mut(handle) {
                        target.text.clone_from(&text);
                    }
                    events.push(SignalData {
                        text: text.clone(),
                        ..signal(Signal::TextChanged)
                    });
                }
                if response.lost_focus() {
                    events.push(SignalData {
                        text,
                        ..signal(Signal::EditingFinished)
                    });
                }
                Some(response)
            }
            Kind::ComboBox => {
                let current = usize::try_from(object.current_index).ok();
                let selected = current.and_then(|i| object.items.get(i)).cloned().unwrap_or_default();
                let mut chosen = None;
                let inner = egui::ComboBox::from_id_salt(("uniwow-combo", handle))
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (index, item) in object.items.iter().enumerate() {
                            if ui.selectable_label(current == Some(index), item).clicked() {
                                chosen = Some(index);
                            }
                        }
                    });
                if let Some(index) = chosen.filter(|index| Some(*index) != current) {
                    if let Some(target) = store.object_mut(handle) {
                        target.current_index = index as i64;
                    }
                    events.push(SignalData {
                        integer: index as i64,
                        ..signal(Signal::CurrentIndexChanged)
                    });
                }
                Some(inner.response)
            }
            Kind::Separator => Some(ui.separator()),
            Kind::GroupBox => Some(
                ui.group(|ui| {
                    if !object.title.is_empty() {
                        ui.strong(&object.title);
                    }
                    if let Some(layout) = object.children.first() {
                        self.object(store, shared, *layout, ui, gpu, events);
                    }
                })
                .response,
            ),
            Kind::VBoxLayout => {
                ui.vertical(|ui| self.boxed(store, shared, &object.children, true, ui, gpu, events));
                None
            }
            Kind::HBoxLayout => {
                ui.horizontal(|ui| self.boxed(store, shared, &object.children, false, ui, gpu, events));
                None
            }
            Kind::GridLayout => {
                let mut cells: Vec<(u32, u32, Handle)> = object
                    .children
                    .iter()
                    .filter_map(|child| store.object(*child).map(|o| (o.cell[0], o.cell[1], *child)))
                    .collect();
                cells.sort();
                egui::Grid::new(("uniwow-grid", handle)).show(ui, |ui| {
                    let (mut row, mut column) = (cells.first().map_or(0, |c| c.0), 0);
                    for (cell_row, cell_column, child) in cells {
                        while row < cell_row {
                            ui.end_row();
                            row += 1;
                            column = 0;
                        }
                        while column < cell_column {
                            ui.label("");
                            column += 1;
                        }
                        self.object(store, shared, child, ui, gpu, events);
                        column += 1;
                    }
                });
                None
            }
            Kind::GraphicsView => {
                self.scenes
                    .entry(handle)
                    .or_default()
                    .show(store, handle, ui, gpu, events);
                None
            }
            Kind::PaintArea => Some(self.paint_area(store, shared, handle, object, ui, events)),
            Kind::CurveView => {
                let size = egui::vec2(
                    ui.available_width(),
                    ui.available_height().max(object.minimum_height as f32),
                );
                let Some(editor) = self.curve_editor.clone() else {
                    return Some(
                        ui.allocate_ui(size, |ui| ui.weak("No curve editor: the module curves is not running."))
                            .response,
                    );
                };
                let mut curves = object.curves.clone();
                let time = self.time_axes.entry(handle).or_default();
                let inner = ui.allocate_ui(size, |ui| {
                    let id = ui.id().with(("uniwow-curves", handle));
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        editor.show(ui, id, &mut curves, time, &CurveOptions::default())
                    }))
                });
                let change = match inner.inner {
                    Ok(change) => change,
                    Err(panic) => {
                        self.editor_failure = Some(format!("the curve editor panicked: {}", panic_text(&*panic)));
                        CurveChange::None
                    }
                };
                if change != CurveChange::None {
                    events.push(SignalData {
                        text: ShownCurve::list_to_json(&curves).to_string(),
                        boolean: change == CurveChange::Finished,
                        ..signal(Signal::CurvesChanged)
                    });
                    if let Some(target) = store.object_mut(handle) {
                        target.curves = curves;
                    }
                }
                Some(inner.response)
            }
            Kind::Panel
            | Kind::Dialog
            | Kind::GraphicsScene
            | Kind::RectItem
            | Kind::LineItem
            | Kind::EllipseItem
            | Kind::TextItem
            | Kind::ItemGroup
            | Kind::Sequence
            | Kind::Player => None,
        }
    }

    /// The children of a box layout one after the other, as in Qt: those that expand share the
    /// room the others leave, as the others measured at the previous frame.
    #[allow(clippy::too_many_arguments)]
    fn boxed(
        &mut self,
        store: &mut Ui,
        shared: &SharedUi,
        children: &[Handle],
        vertical: bool,
        ui: &mut egui::Ui,
        gpu: Option<&egui_wgpu::RenderState>,
        events: &mut Vec<SignalData>,
    ) {
        let expanding: Vec<bool> = children.iter().map(|child| expands(store, *child)).collect();
        let along = |size: egui::Vec2| if vertical { size.y } else { size.x };
        let fixed: f32 = children
            .iter()
            .zip(&expanding)
            .filter(|(_, expands)| !**expands)
            .map(|(child, _)| self.sizes.get(child).copied().unwrap_or(0.0))
            .sum::<f32>()
            + along(ui.spacing().item_spacing) * children.len().saturating_sub(1) as f32;
        let count = expanding.iter().filter(|expands| **expands).count().max(1);
        let share = ((along(ui.available_size()) - fixed) / count as f32).max(0.0);
        for (child, expands) in children.iter().zip(expanding) {
            if expands {
                let size = if vertical {
                    egui::vec2(ui.available_width(), share)
                } else {
                    egui::vec2(share, ui.available_height())
                };
                ui.allocate_ui(size, |ui| self.object(store, shared, *child, ui, gpu, events));
            } else {
                let rect = ui
                    .scope(|ui| self.object(store, shared, *child, ui, gpu, events))
                    .response
                    .rect;
                self.sizes.insert(*child, along(rect.size()));
            }
        }
    }

    /// A painting area: asks its module to paint when its size changes or after `update`, shows
    /// the last picture, and sends the mouse as signals.
    fn paint_area(
        &mut self,
        store: &mut Ui,
        shared: &SharedUi,
        handle: Handle,
        object: &Object,
        ui: &mut egui::Ui,
        events: &mut Vec<SignalData>,
    ) -> egui::Response {
        let height = ui.available_height().max(object.minimum_height as f32);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
        let size = [f64::from(rect.width()), f64::from(rect.height())];
        if object.repaint || self.painted.get(&handle) != Some(&size) {
            if let Some(target) = store.object_mut(handle) {
                target.repaint = false;
            }
            self.painted.insert(handle, size);
            store.request_paint(shared, handle, size);
        }
        replay(&ui.painter().with_clip_rect(rect), rect, &object.picture);

        let local = |p: egui::Pos2| (f64::from(p.x - rect.min.x), f64::from(p.y - rect.min.y));
        let mouse = |signal: Signal, at: egui::Pos2, button: u32| {
            let (x, y) = local(at);
            SignalData {
                sender: handle,
                signal: signal as u32,
                x,
                y,
                button,
                modifiers: modifiers(ui),
                ..Default::default()
            }
        };
        let pointer = response.hover_pos().or_else(|| response.interact_pointer_pos());
        let pressed = ui.input(|i| {
            [
                (egui::PointerButton::Primary, 1),
                (egui::PointerButton::Secondary, 2),
                (egui::PointerButton::Middle, 3),
            ]
            .into_iter()
            .find(|(button, _)| i.pointer.button_pressed(*button))
            .map(|(_, number)| number)
        });
        if let (Some(button), Some(at), true) = (pressed, pointer, response.hovered()) {
            events.push(mouse(Signal::MousePress, at, button));
        }
        if response.dragged()
            && response.drag_delta() != egui::Vec2::ZERO
            && let Some(at) = pointer
        {
            events.push(mouse(Signal::MouseMove, at, 0));
        }
        if (response.drag_stopped() || response.clicked())
            && let Some(at) = pointer
        {
            events.push(mouse(Signal::MouseRelease, at, 0));
        }
        let wheel = if response.hovered() {
            ui.input(|i| i.smooth_scroll_delta)
        } else {
            egui::Vec2::ZERO
        };
        if wheel != egui::Vec2::ZERO
            && let Some(at) = pointer
        {
            let mut data = mouse(Signal::Wheel, at, 0);
            data.dx = f64::from(wheel.x);
            data.dy = f64::from(wheel.y);
            events.push(data);
        }
        response
    }
}

/// The close button of a dialog: a drawn cross, the fonts having no such character.
fn close_button(ui: &mut egui::Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    let stroke = egui::Stroke::new(1.5, ui.style().interact(&response).fg_stroke.color);
    let (centre, size) = (rect.center(), 4.0);
    ui.painter().line_segment(
        [centre + egui::Vec2::splat(-size), centre + egui::Vec2::splat(size)],
        stroke,
    );
    ui.painter().line_segment(
        [centre + egui::vec2(-size, size), centre + egui::vec2(size, -size)],
        stroke,
    );
    response.on_hover_text("Close").clicked()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uniwow_api::curve::{CurveChange, CurveEditor, CurveOptions, ShownCurve, TimeAxis};
    use uniwow_api::egui;
    use uniwow_api::ui::{Kind, Property, Ui, lock};

    use super::PanelView;

    struct Broken;

    impl CurveEditor for Broken {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            _curves: &mut [ShownCurve],
            _time: &mut TimeAxis,
            _options: &CurveOptions,
        ) -> CurveChange {
            panic!("broken editor")
        }
    }

    #[test]
    fn a_panic_of_the_curve_editor_is_kept_for_its_provider_to_be_reported() {
        let shared = Ui::new(Arc::new(|_job| {}));
        {
            let mut store = lock(&shared);
            let panel = store.panel("p");
            let layout = store.create(Kind::VBoxLayout, None).unwrap();
            store.add_to(panel, layout, [0, 0, 1, 1]).unwrap();
            let view = store.create(Kind::CurveView, None).unwrap();
            store.add_to(layout, view, [0, 0, 1, 1]).unwrap();
        }
        let mut panels = PanelView::default();
        panels.set_curve_editor(Some(Arc::new(Broken)));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| panels.show(&shared, "p", ui, None));
        output.textures_delta.clear();
        assert!(
            panels
                .take_editor_failure()
                .is_some_and(|m| m.contains("broken editor"))
        );
    }

    #[test]
    fn what_was_kept_of_a_destroyed_view_is_forgotten() {
        let shared = Ui::new(Arc::new(|_job| {}));
        let (layout, view, area, label) = {
            let mut store = lock(&shared);
            let panel = store.panel("p");
            let layout = store.create(Kind::VBoxLayout, None).unwrap();
            store.add_to(panel, layout, [0, 0, 1, 1]).unwrap();
            let view = store.create(Kind::GraphicsView, None).unwrap();
            store.add_to(layout, view, [0, 0, 1, 1]).unwrap();
            let area = store.create(Kind::PaintArea, None).unwrap();
            store.set_numbers(area, Property::MinimumHeight, &[50.0]).unwrap();
            store.add_to(layout, area, [0, 0, 1, 1]).unwrap();
            let label = store.create(Kind::Label, None).unwrap();
            store.add_to(layout, label, [0, 0, 1, 1]).unwrap();
            (layout, view, area, label)
        };
        let mut panels = PanelView::default();
        let ctx = egui::Context::default();
        let frame = |panels: &mut PanelView| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| panels.show(&shared, "p", ui, None));
            output.textures_delta.clear();
        };
        frame(&mut panels);
        assert!(panels.scenes.contains_key(&view) && panels.painted.contains_key(&area));
        assert!(panels.sizes.contains_key(&label));
        lock(&shared).destroy(layout).unwrap();
        frame(&mut panels);
        assert!(panels.scenes.is_empty() && panels.sizes.is_empty() && !panels.painted.contains_key(&area));
    }
}
