//! Draws the panels of a module from its interface objects, on the interface thread, and turns
//! what the user does into signals.

use std::collections::HashMap;

use super::painter::replay;
use super::scene::{SceneView, modifiers};
use super::{Handle, Kind, Object, SharedUi, Signal, SignalData, Ui, lock};
use crate::{egui, egui_wgpu};

/// What the interface thread keeps of a module's panels between frames.
#[derive(Default)]
pub struct PanelView {
    scenes: HashMap<Handle, SceneView>,
    /// The size each painting area was last asked to paint.
    painted: HashMap<Handle, [f64; 2]>,
}

impl PanelView {
    /// Draws the panel `panel` of a module.
    pub fn show(&mut self, shared: &SharedUi, panel: &str, ui: &mut egui::Ui, gpu: Option<&egui_wgpu::RenderState>) {
        let mut store = lock(shared);
        store.set_wake(ui.ctx());
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
                ui.vertical(|ui| {
                    for child in &object.children {
                        self.object(store, shared, *child, ui, gpu, events);
                    }
                });
                None
            }
            Kind::HBoxLayout => {
                ui.horizontal(|ui| {
                    for child in &object.children {
                        self.object(store, shared, *child, ui, gpu, events);
                    }
                });
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
            Kind::Panel
            | Kind::GraphicsScene
            | Kind::RectItem
            | Kind::LineItem
            | Kind::EllipseItem
            | Kind::TextItem
            | Kind::ItemGroup => None,
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
