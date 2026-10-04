//! Draws the panels of a module from its interface objects, on the interface thread, and turns
//! what the user does into signals.

use std::collections::HashMap;
use std::sync::Arc;

mod painter;
mod scene;

use painter::replay;
use scene::{SceneView, modifiers};
use uniwow_api::curve::{self, Curve, CurveChange, CurveEditor, CurveOptions, ShownCurve, TimeAxis};
use uniwow_api::dopesheet::{self, Dopesheet, DopesheetInput, KeysChange, RowProperty};
use uniwow_api::sequence::{Sequence, Track, number_colour, number_names, tracks_to_json};
use uniwow_api::ui::{Handle, Kind, Object, Property, SharedUi, Signal, SignalData, Ui, lock};
use uniwow_api::{Editor, egui, egui_wgpu, log};

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
    /// The dopesheet of the module `dopesheet`, which draws the dopesheet views, when it runs.
    dopesheet: Option<Arc<dyn Dopesheet>>,
    /// The module's editor: the labels and values of the properties its sequences animate.
    editor: Option<Editor>,
    /// The time axis of each curve view and dopesheet view.
    time_axes: HashMap<Handle, TimeAxis>,
    /// The curves of a curve view showing a sequence while a change of them goes on, with the
    /// version of the sequence they started from.
    working: HashMap<Handle, (u64, Vec<ShownCurve>)>,
    /// Why a service failed while drawing, by service, for its provider to be reported.
    failures: Vec<(&'static str, String)>,
}

/// The curves of a sequence as a curve view shows them: one per number of each track.
fn sequence_curves(data: &Sequence, labels: &HashMap<String, String>) -> Vec<ShownCurve> {
    let mut curves = Vec::new();
    for track in &data.tracks {
        let label = labels.get(&track.property).unwrap_or(&track.property);
        let names = number_names(track.kind);
        for (number, curve) in track.curves.iter().enumerate() {
            curves.push(ShownCurve {
                label: if track.curves.len() > 1 {
                    format!("{label}.{}", names[number])
                } else {
                    label.clone()
                },
                colour: number_colour(track.kind, number),
                curve: curve.clone(),
                visible: true,
            });
        }
    }
    curves
}

/// The tracks of `data` with the curves a curve view changed, in the order `sequence_curves` gave.
fn tracks_of(data: &Sequence, curves: &[ShownCurve]) -> Vec<Track> {
    let mut changed = curves.iter();
    data.tracks
        .iter()
        .map(|track| Track {
            curves: track
                .curves
                .iter()
                .map(|_| changed.next().map_or_else(Curve::default, |shown| shown.curve.clone()))
                .collect(),
            ..track.clone()
        })
        .collect()
}

/// The time of the player a view shows as its playhead.
fn playhead(store: &Ui, view: &Object) -> Option<f64> {
    view.player
        .and_then(|player| store.object(player))
        .filter(|player| player.kind == Kind::Player)
        .map(|player| player.time)
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
                Kind::GraphicsView | Kind::PaintArea | Kind::CurveView | Kind::DopesheetView => true,
                Kind::VBoxLayout | Kind::HBoxLayout | Kind::GridLayout | Kind::GroupBox => {
                    object.children.iter().any(|child| expands(store, *child))
                }
                _ => false,
            }
    })
}

impl PanelView {
    /// The services the views are drawn with, from the modules `curves` and `dopesheet`, and the
    /// editor of the module whose objects are drawn.
    pub fn set_services(
        &mut self,
        curve_editor: Option<Arc<dyn CurveEditor>>,
        dopesheet: Option<Arc<dyn Dopesheet>>,
        editor: Editor,
    ) {
        self.curve_editor = curve_editor;
        self.dopesheet = dopesheet;
        self.editor = Some(editor);
    }

    /// Why services panicked, once, with the id of each service: its provider is the culprit (F5).
    pub fn take_failures(&mut self) -> Vec<(&'static str, String)> {
        std::mem::take(&mut self.failures)
    }

    /// The labels of the animatable properties of running modules, by path.
    fn labels(&self) -> HashMap<String, String> {
        self.editor
            .as_ref()
            .map(Editor::properties)
            .unwrap_or_default()
            .into_iter()
            .map(|info| (info.path, info.label))
            .collect()
    }

    /// The property of each track of `data`, with its value at `playhead`.
    fn row_properties(&self, data: &Sequence, playhead: Option<f64>) -> HashMap<String, RowProperty> {
        let labels = self.labels();
        data.tracks
            .iter()
            .map(|track| {
                let label = labels.get(&track.property);
                let current = label
                    .and(self.editor.as_ref())
                    .and_then(|editor| editor.read_property(&track.property).ok());
                let value = playhead.and_then(|frame| track.evaluate(frame, current)).or(current);
                let property = RowProperty {
                    label: label
                        .cloned()
                        .unwrap_or_else(|| track.property.rsplit('/').next().unwrap_or(&track.property).to_owned()),
                    declared: label.is_some(),
                    value,
                };
                (track.property.clone(), property)
            })
            .collect()
    }

    /// Forgets what it kept of the objects that are gone; a view's texture goes with it.
    fn forget_gone(&mut self, store: &Ui) {
        let alive = |handle: &Handle| store.object(*handle).is_some();
        self.scenes.retain(|handle, _| alive(handle));
        self.painted.retain(|handle, _| alive(handle));
        self.sizes.retain(|handle, _| alive(handle));
        self.time_axes.retain(|handle, _| alive(handle));
        self.working.retain(|handle, _| alive(handle));
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
                // A sequence shown: its curves, or those of the change going on.
                let shown = object.plays.and_then(|sequence| {
                    let generation = store.object(sequence)?.generation;
                    Some((sequence, store.sequence(sequence).ok()?, generation))
                });
                let (mut curves, options) = match &shown {
                    Some((_, data, generation)) => {
                        let curves = match self.working.get(&handle) {
                            Some((started, working)) if started == generation => working.clone(),
                            _ => sequence_curves(data, &self.labels()),
                        };
                        let options = CurveOptions {
                            snap: Some(1.0),
                            playhead: playhead(store, object),
                            span: Some([0.0, f64::from(data.length)]),
                        };
                        (curves, options)
                    }
                    None => (object.curves.clone(), CurveOptions::default()),
                };
                let time = self.time_axes.entry(handle).or_default();
                let inner = ui.allocate_ui(size, |ui| {
                    let id = ui.id().with(("uniwow-curves", handle));
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        editor.show(ui, id, &mut curves, time, &options)
                    }))
                });
                let change = match inner.inner {
                    Ok(change) => change,
                    Err(panic) => {
                        let message = format!("the curve editor panicked: {}", panic_text(&*panic));
                        self.failures.push((curve::SERVICE.id(), message));
                        CurveChange::None
                    }
                };
                match shown {
                    Some((sequence, data, generation)) => {
                        let idle = !ui.input(|i| i.pointer.any_down());
                        self.sequence_curves_changed(
                            store, handle, sequence, &data, generation, curves, change, idle, events,
                        );
                    }
                    None if change != CurveChange::None => {
                        events.push(SignalData {
                            text: ShownCurve::list_to_json(&curves).to_string(),
                            boolean: change == CurveChange::Finished,
                            ..signal(Signal::CurvesChanged)
                        });
                        if let Some(target) = store.object_mut(handle) {
                            target.curves = curves;
                        }
                    }
                    None => {}
                }
                Some(inner.response)
            }
            Kind::DopesheetView => {
                let size = egui::vec2(
                    ui.available_width(),
                    ui.available_height().max(object.minimum_height as f32),
                );
                let Some(sheet) = self.dopesheet.clone() else {
                    return Some(
                        ui.allocate_ui(size, |ui| ui.weak("No dopesheet: the module dopesheet is not running."))
                            .response,
                    );
                };
                let Some((sequence, data)) = object
                    .plays
                    .and_then(|sequence| Some((sequence, store.sequence(sequence).ok()?)))
                else {
                    return Some(ui.allocate_ui(size, |ui| ui.weak("No sequence shown.")).response);
                };
                let playhead = playhead(store, object);
                let properties = self.row_properties(&data, playhead);
                let time = self.time_axes.entry(handle).or_default();
                let inner = ui.allocate_ui(size, |ui| {
                    let id = ui.id().with(("uniwow-dopesheet", handle));
                    let input = DopesheetInput {
                        sequence: &data,
                        properties: &properties,
                        playhead,
                        title: &object.title,
                    };
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sheet.show(ui, id, &input, time)))
                });
                let output = match inner.inner {
                    Ok(output) => output,
                    Err(panic) => {
                        let message = format!("the dopesheet panicked: {}", panic_text(&*panic));
                        self.failures.push((dopesheet::SERVICE.id(), message));
                        return Some(inner.response);
                    }
                };
                if let (Some(frame), Some(player)) = (output.playhead, object.player) {
                    let moved = store
                        .set_numbers(player, Property::Playing, &[0.0])
                        .and_then(|()| store.set_numbers(player, Property::Time, &[frame]));
                    if moved.is_ok() {
                        events.push(SignalData {
                            number: frame,
                            ..signal(Signal::PlayheadMoved)
                        });
                    }
                }
                match output.keys {
                    KeysChange::None => {}
                    KeysChange::Changing(tracks) => events.push(SignalData {
                        text: tracks_to_json(&tracks).to_string(),
                        ..signal(Signal::KeysChanged)
                    }),
                    KeysChange::Finished { label, tracks } => {
                        if let Some(event) = change_keys(store, handle, sequence, tracks, &label) {
                            events.push(event);
                        }
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

    /// What a curve view showing a sequence did: a change going on is kept and drawn until it is
    /// done, then made to the sequence as one undo entry; one dropped is forgotten once the pointer
    /// is up.
    #[allow(clippy::too_many_arguments)]
    fn sequence_curves_changed(
        &mut self,
        store: &mut Ui,
        view: Handle,
        sequence: Handle,
        data: &Sequence,
        generation: u64,
        curves: Vec<ShownCurve>,
        change: CurveChange,
        idle: bool,
        events: &mut Vec<SignalData>,
    ) {
        match change {
            CurveChange::None if idle => {
                self.working.remove(&view);
            }
            CurveChange::None => {}
            CurveChange::Changing => {
                events.push(SignalData {
                    sender: view,
                    signal: Signal::KeysChanged as u32,
                    text: tracks_to_json(&tracks_of(data, &curves)).to_string(),
                    ..Default::default()
                });
                self.working.insert(view, (generation, curves));
            }
            CurveChange::Finished => {
                self.working.remove(&view);
                if let Some(event) = change_keys(store, view, sequence, tracks_of(data, &curves), "edit curves") {
                    events.push(event);
                }
            }
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

/// Makes a change of keys done in a view to its sequence, as one undo entry named `label`, and
/// returns the signal telling it; nothing for tracks that break the rules of a file, or a change
/// the kernel refuses to record.
fn change_keys(store: &mut Ui, view: Handle, sequence: Handle, tracks: Vec<Track>, label: &str) -> Option<SignalData> {
    let text = tracks_to_json(&tracks).to_string();
    let checked = Sequence {
        tracks,
        ..Sequence::default()
    };
    let made = checked
        .check()
        .and_then(|()| store.change_tracks(sequence, checked.tracks, label));
    match made {
        Ok(()) => Some(SignalData {
            sender: view,
            signal: Signal::KeysChanged as u32,
            text,
            boolean: true,
            ..Default::default()
        }),
        Err(error) => {
            log::warn!("the keys changed in a view were not kept: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use uniwow_api::curve::{CurveChange, CurveEditor, CurveOptions, ShownCurve, TimeAxis};
    use uniwow_api::dopesheet::{Dopesheet, DopesheetInput, DopesheetOutput, KeysChange};
    use uniwow_api::egui;
    use uniwow_api::sequence::Track;
    use uniwow_api::ui::{Handle, Kind, Property, SharedUi, Signal, SignalData, Ui, lock};

    use super::PanelView;

    type Jobs = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;
    /// The signals a view sent: which, its text, its boolean and its number.
    type Sent = Arc<Mutex<Vec<(Signal, String, bool, f64)>>>;

    /// A dopesheet giving the same at every frame.
    struct Giving(DopesheetOutput);

    impl Dopesheet for Giving {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            _input: &DopesheetInput,
            _time: &mut TimeAxis,
        ) -> DopesheetOutput {
            self.0.clone()
        }
    }

    struct BrokenSheet;

    impl Dopesheet for BrokenSheet {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            _input: &DopesheetInput,
            _time: &mut TimeAxis,
        ) -> DopesheetOutput {
            panic!("broken dopesheet")
        }
    }

    /// A curve editor setting the first key of the first curve to 0.75, which says `change`.
    struct Editing(CurveChange);

    impl CurveEditor for Editing {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            curves: &mut [ShownCurve],
            _time: &mut TimeAxis,
            _options: &CurveOptions,
        ) -> CurveChange {
            curves[0].curve.keys[0].value = 0.75;
            self.0
        }
    }

    /// A panel holding a view of `kind` on a sequence with a key of 0.5 at frame 0 on
    /// `cube/opacity`, played by a player at frame 5; what the module would record, and the
    /// signals the view sends.
    struct Fixture {
        shared: SharedUi,
        jobs: Jobs,
        sequence: Handle,
        player: Handle,
        view: Handle,
        recorded: Arc<Mutex<Vec<String>>>,
        signals: Sent,
    }

    const TRACKS: &str =
        r#"[{"property":"cube/opacity","kind":"number","curves":[{"keys":[{"time":0,"value":0.5}]}]}]"#;

    fn fixture(kind: Kind) -> Fixture {
        let jobs: Jobs = Arc::default();
        let queue = jobs.clone();
        let shared = Ui::new(Arc::new(move |job| queue.lock().unwrap().push(job)));
        let recorded: Arc<Mutex<Vec<String>>> = Arc::default();
        let signals: Sent = Arc::default();
        let (sequence, player, view) = {
            let mut store = lock(&shared);
            let panel = store.panel("p");
            let layout = store.create(Kind::VBoxLayout, None).unwrap();
            store.add_to(panel, layout, [0, 0, 1, 1]).unwrap();
            let sequence = store.create(Kind::Sequence, None).unwrap();
            store.set_text(sequence, Property::Tracks, TRACKS).unwrap();
            let player = store.create(Kind::Player, None).unwrap();
            store
                .set_numbers(player, Property::Sequence, &[sequence as f64])
                .unwrap();
            store.set_numbers(player, Property::Time, &[5.0]).unwrap();
            store.set_numbers(player, Property::Playing, &[1.0]).unwrap();
            let view = store.create(kind, None).unwrap();
            store.set_numbers(view, Property::Sequence, &[sequence as f64]).unwrap();
            store.set_numbers(view, Property::Player, &[player as f64]).unwrap();
            store.add_to(layout, view, [0, 0, 1, 1]).unwrap();
            for signal in [Signal::KeysChanged, Signal::PlayheadMoved, Signal::CurvesChanged] {
                let seen = signals.clone();
                store
                    .connect(
                        view,
                        signal,
                        Arc::new(move |data: &SignalData| {
                            let signal = Signal::from_u32(data.signal).unwrap();
                            seen.lock()
                                .unwrap()
                                .push((signal, data.text.clone(), data.boolean, data.number));
                        }),
                    )
                    .unwrap();
            }
            let labels = recorded.clone();
            store.set_recorder(Arc::new(move |label, _change| {
                labels.lock().unwrap().push(label.to_owned());
                Ok(())
            }));
            (sequence, player, view)
        };
        Fixture {
            shared,
            jobs,
            sequence,
            player,
            view,
            recorded,
            signals,
        }
    }

    impl Fixture {
        /// Draws the panel once, then runs the slots.
        fn frame(&self, panels: &mut PanelView) {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| panels.show(&self.shared, "p", ui, None));
            output.textures_delta.clear();
            let jobs = std::mem::take(&mut *self.jobs.lock().unwrap());
            for job in jobs {
                job();
            }
        }

        fn value(&self) -> f64 {
            lock(&self.shared).sequence(self.sequence).unwrap().tracks[0].curves[0].keys[0].value
        }
    }

    /// The tracks of the fixture with the key at `frame`.
    fn moved_to(frame: f64) -> Vec<Track> {
        let mut tracks = uniwow_api::ui::read_tracks(TRACKS).unwrap();
        tracks[0].curves[0].keys[0].time = frame;
        tracks
    }

    #[test]
    fn a_change_of_keys_done_in_a_dopesheet_view_is_made_and_recorded_and_the_playhead_moves() {
        let fixture = fixture(Kind::DopesheetView);
        let mut panels = PanelView {
            dopesheet: Some(Arc::new(Giving(DopesheetOutput {
                keys: KeysChange::Finished {
                    label: "move keys".to_owned(),
                    tracks: moved_to(3.0),
                },
                playhead: Some(12.0),
            }))),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        let store = lock(&fixture.shared);
        assert_eq!(store.sequence(fixture.sequence).unwrap().tracks, moved_to(3.0));
        assert_eq!(store.numbers(fixture.player, Property::Time).unwrap(), vec![12.0]);
        assert_eq!(
            store.numbers(fixture.player, Property::Playing).unwrap(),
            vec![0.0],
            "paused there"
        );
        drop(store);
        assert_eq!(*fixture.recorded.lock().unwrap(), vec!["move keys".to_owned()]);
        let signals = fixture.signals.lock().unwrap();
        assert!(signals.contains(&(Signal::PlayheadMoved, String::new(), false, 12.0)));
        assert!(
            signals
                .iter()
                .any(|(signal, text, done, _)| *signal == Signal::KeysChanged && *done && text.contains("3.0"))
        );
    }

    #[test]
    fn a_change_of_keys_under_way_or_breaking_the_rules_leaves_the_sequence() {
        for (keys, sent) in [
            (KeysChange::Changing(moved_to(3.0)), true),
            (
                KeysChange::Finished {
                    label: "move keys".to_owned(),
                    tracks: moved_to(2.5),
                },
                false,
            ),
        ] {
            let fixture = fixture(Kind::DopesheetView);
            let mut panels = PanelView {
                dopesheet: Some(Arc::new(Giving(DopesheetOutput { keys, playhead: None }))),
                ..PanelView::default()
            };
            fixture.frame(&mut panels);
            assert_eq!(
                lock(&fixture.shared).sequence(fixture.sequence).unwrap().tracks,
                moved_to(0.0)
            );
            assert!(fixture.recorded.lock().unwrap().is_empty());
            let signals = fixture.signals.lock().unwrap();
            assert_eq!(
                signals
                    .iter()
                    .any(|(signal, _, done, _)| *signal == Signal::KeysChanged && !done),
                sent
            );
        }
    }

    #[test]
    fn a_panic_of_the_dopesheet_is_kept_for_its_provider_to_be_reported() {
        let fixture = fixture(Kind::DopesheetView);
        let mut panels = PanelView {
            dopesheet: Some(Arc::new(BrokenSheet)),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        let failures = panels.take_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "dopesheet");
        assert!(failures[0].1.contains("broken dopesheet"));
    }

    #[test]
    fn a_curve_view_showing_a_sequence_changes_it_once_the_change_is_done() {
        let fixture = fixture(Kind::CurveView);
        let mut panels = PanelView {
            curve_editor: Some(Arc::new(Editing(CurveChange::Changing))),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        assert_eq!(fixture.value(), 0.5, "not while it goes on");
        assert!(panels.working.contains_key(&fixture.view), "kept and drawn until done");
        panels.curve_editor = Some(Arc::new(Editing(CurveChange::Finished)));
        fixture.frame(&mut panels);
        assert_eq!(fixture.value(), 0.75);
        assert!(panels.working.is_empty());
        assert_eq!(*fixture.recorded.lock().unwrap(), vec!["edit curves".to_owned()]);
        let signals = fixture.signals.lock().unwrap();
        assert!(
            signals
                .iter()
                .any(|(signal, _, done, _)| *signal == Signal::KeysChanged && *done)
        );
        assert!(
            signals.iter().all(|(signal, ..)| *signal != Signal::CurvesChanged),
            "its own curves are not what it shows"
        );
    }

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
        let mut panels = PanelView {
            curve_editor: Some(Arc::new(Broken)),
            ..PanelView::default()
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| panels.show(&shared, "p", ui, None));
        output.textures_delta.clear();
        let failures = panels.take_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "curve-editor");
        assert!(failures[0].1.contains("broken editor"));
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
