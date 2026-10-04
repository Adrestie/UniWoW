//! Draws the panels of a module from its interface objects, on the interface thread, and turns
//! what the user does into signals.

use std::collections::{BTreeSet, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

mod painter;
mod scene;

use painter::replay;
use scene::{SceneView, modifiers};
use uniwow_api::curve::{self, CurveChange, CurveEditor, CurveOptions, ShownCurve, TimeAxis};
use uniwow_api::dopesheet::{self, Dopesheet, DopesheetInput, KeysChange, RowProperty};
use uniwow_api::sequence::{Sequence, Track, number_colour, number_names, tracks_to_json};
use uniwow_api::ui::{Handle, Kind, Object, Property, SharedUi, Signal, SignalData, Ui, lock};
use uniwow_api::{Editor, PropertyInfo, PropertyKind, egui, egui_wgpu, log};

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
    /// The animatable properties, by path, read before the objects are locked.
    infos: HashMap<String, PropertyInfo>,
    /// The properties of the tracks each view shows, read before the objects are locked.
    rows: HashMap<Handle, HashMap<String, RowProperty>>,
    /// The id each dopesheet view was drawn under, for the dopesheet to forget it once it is gone.
    sheet_ids: HashMap<Handle, egui::Id>,
    /// The time axis of each curve view and dopesheet view.
    time_axes: HashMap<Handle, TimeAxis>,
    /// The change of its sequence each view has under way.
    editing: HashMap<Handle, Editing>,
    /// Why a service failed while drawing, by service, for its provider to be reported.
    failures: Vec<(&'static str, String)>,
}

/// A change of a view's sequence under way, shown at once: the tracks before it began, and the
/// version of the sequence once the view last changed it.
struct Editing {
    before: Vec<Track>,
    generation: u64,
}

/// The curves of a sequence as a curve view shows them, one per number of each track but a
/// boolean's, which holds its value from key to key; with the track and number of each.
fn sequence_curves(
    data: &Sequence,
    infos: &HashMap<String, PropertyInfo>,
    hidden: &BTreeSet<(String, usize)>,
) -> (Vec<ShownCurve>, Vec<(usize, usize)>) {
    let (mut curves, mut origin) = (Vec::new(), Vec::new());
    for (index, track) in data.tracks.iter().enumerate() {
        if track.kind == PropertyKind::Boolean {
            continue;
        }
        let label = infos.get(&track.property).map_or(&track.property, |info| &info.label);
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
                visible: !hidden.contains(&(track.property.clone(), number)),
            });
            origin.push((index, number));
        }
    }
    (curves, origin)
}

/// The tracks of `data` with the curves a curve view changed, from the track and number of each.
fn tracks_of(data: &Sequence, curves: &[ShownCurve], origin: &[(usize, usize)]) -> Vec<Track> {
    let mut tracks = data.tracks.clone();
    for (shown, (index, number)) in curves.iter().zip(origin) {
        if let Some(curve) = tracks.get_mut(*index).and_then(|track| track.curves.get_mut(*number)) {
            curve.clone_from(&shown.curve);
        }
    }
    tracks
}

/// Whether the tracks follow the rules of a file.
fn check_tracks(tracks: &[Track]) -> Result<(), String> {
    Sequence {
        tracks: tracks.to_vec(),
        ..Sequence::default()
    }
    .check()
}

/// Whether nothing the user does could be a change under way: the pointer up, no field typed in.
fn idle(ui: &egui::Ui) -> bool {
    !ui.input(|i| i.pointer.any_down()) && ui.ctx().memory(|memory| memory.focused().is_none())
}

/// The playhead a view moved: its player paused at that frame, and `playheadMoved` sent.
fn move_playhead(store: &mut Ui, view: Handle, object: &Object, frame: f64, events: &mut Vec<SignalData>) {
    let Some(player) = object.player else {
        return;
    };
    let moved = store
        .set_numbers(player, Property::Playing, &[0.0])
        .and_then(|()| store.set_numbers(player, Property::Time, &[frame]));
    if moved.is_ok() {
        events.push(SignalData {
            sender: view,
            signal: Signal::PlayheadMoved as u32,
            number: frame,
            ..Default::default()
        });
    }
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

    /// Reads, before the objects are locked, the labels and values of the properties the sequences
    /// shown animate: reading a property runs its module's code, which may lock objects.
    fn prepare(&mut self, shared: &SharedUi) {
        let shown = lock(shared).shown_sequences();
        self.rows.clear();
        if shown.is_empty() {
            return;
        }
        self.infos = self
            .editor
            .as_ref()
            .map(Editor::properties)
            .unwrap_or_default()
            .into_iter()
            .map(|info| (info.path.clone(), info))
            .collect();
        for (view, data, playhead) in shown {
            let rows = self.row_properties(&data, playhead);
            self.rows.insert(view, rows);
        }
    }

    /// The property of each track of `data`, with its value at `playhead`.
    fn row_properties(&self, data: &Sequence, playhead: Option<f64>) -> HashMap<String, RowProperty> {
        data.tracks
            .iter()
            .map(|track| {
                let info = self.infos.get(&track.property);
                let current = info
                    .and(self.editor.as_ref())
                    .and_then(|editor| editor.read_property(&track.property).ok());
                let value = playhead.and_then(|frame| track.evaluate(frame, current)).or(current);
                let property = RowProperty {
                    label: info.map_or_else(
                        || track.property.rsplit('/').next().unwrap_or(&track.property).to_owned(),
                        |info| info.label.clone(),
                    ),
                    declared: info.is_some(),
                    value,
                    current,
                    range: info.map_or([f64::MIN, f64::MAX], |info| info.range),
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
        self.editing.retain(|handle, _| alive(handle));
        let sheet = self.dopesheet.clone();
        self.sheet_ids.retain(|handle, id| {
            let kept = alive(handle);
            if !kept && let Some(sheet) = &sheet {
                sheet.forget(*id);
            }
            kept
        });
    }

    /// Draws the panel `panel` of a module.
    pub fn show(&mut self, shared: &SharedUi, panel: &str, ui: &mut egui::Ui, gpu: Option<&egui_wgpu::RenderState>) {
        self.prepare(shared);
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
        self.prepare(shared);
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
                if let Some((sequence, data)) = object
                    .plays
                    .and_then(|sequence| Some((sequence, store.sequence(sequence).ok()?)))
                {
                    let response =
                        self.sequence_curve_view(store, handle, object, sequence, &data, editor, size, ui, events);
                    return Some(response);
                }
                let mut curves = object.curves.clone();
                let time = self.time_axes.entry(handle).or_default();
                let inner = ui.allocate_ui(size, |ui| {
                    let id = ui.id().with(("uniwow-curves", handle));
                    catch_unwind(AssertUnwindSafe(|| {
                        editor.show(ui, id, &mut curves, time, &CurveOptions::default())
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
                let properties = self.rows.remove(&handle).unwrap_or_default();
                let id = ui.id().with(("uniwow-dopesheet", handle));
                self.sheet_ids.insert(handle, id);
                let time = self.time_axes.entry(handle).or_default();
                let inner = ui.allocate_ui(size, |ui| {
                    let input = DopesheetInput {
                        sequence: &data,
                        properties: &properties,
                        playhead,
                        title: &object.title,
                    };
                    catch_unwind(AssertUnwindSafe(|| sheet.show(ui, id, &input, time)))
                });
                let output = match inner.inner {
                    Ok(output) => output,
                    Err(panic) => {
                        let message = format!("the dopesheet panicked: {}", panic_text(&*panic));
                        self.failures.push((dopesheet::SERVICE.id(), message));
                        return Some(inner.response);
                    }
                };
                if let Some(frame) = output.playhead {
                    move_playhead(store, handle, object, frame, events);
                }
                let idle = idle(ui);
                self.apply_keys(store, handle, sequence, output.keys, idle, events);
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

    /// A curve view showing a sequence: on its left the rows of the dopesheet, when it runs, with a
    /// box showing or hiding each curve; on its right the curves shown. What either does goes to
    /// the sequence as a dopesheet view's does.
    #[allow(clippy::too_many_arguments)]
    fn sequence_curve_view(
        &mut self,
        store: &mut Ui,
        view: Handle,
        object: &Object,
        sequence: Handle,
        data: &Sequence,
        editor: Arc<dyn CurveEditor>,
        size: egui::Vec2,
        ui: &mut egui::Ui,
        events: &mut Vec<SignalData>,
    ) -> egui::Response {
        let playhead = playhead(store, object);
        let properties = self.rows.remove(&view).unwrap_or_default();
        let id = ui.id().with(("uniwow-curves", view));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
        let mut left = (KeysChange::None, None, BTreeSet::new());
        let mut left_width = 0.0;
        if let Some(sheet) = self.dopesheet.clone() {
            self.sheet_ids.insert(view, id);
            left_width = (rect.width() * 0.5).min(430.0);
            let area = egui::Rect::from_min_size(rect.min, egui::vec2(left_width, rect.height()));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
            let input = DopesheetInput {
                sequence: data,
                properties: &properties,
                playhead,
                title: &object.title,
            };
            match catch_unwind(AssertUnwindSafe(|| sheet.curve_properties(&mut child, id, &input))) {
                Ok(output) => left = (output.keys, output.playhead, output.hidden),
                Err(panic) => {
                    let message = format!("the dopesheet panicked: {}", panic_text(&*panic));
                    self.failures.push((dopesheet::SERVICE.id(), message));
                }
            }
        }
        let (left_keys, moved, hidden) = left;
        let (mut curves, origin) = sequence_curves(data, &self.infos, &hidden);
        let options = CurveOptions {
            snap: Some(1.0),
            playhead,
            span: Some([0.0, f64::from(data.length)]),
        };
        let area = egui::Rect::from_min_max(egui::pos2(rect.left() + left_width, rect.top()), rect.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
        let time = self.time_axes.entry(view).or_default();
        let shown = catch_unwind(AssertUnwindSafe(|| {
            editor.show(&mut child, id.with("curves"), &mut curves, time, &options)
        }));
        let change = shown.unwrap_or_else(|panic| {
            let message = format!("the curve editor panicked: {}", panic_text(&*panic));
            self.failures.push((curve::SERVICE.id(), message));
            CurveChange::None
        });
        let keys = match (left_keys, change) {
            (KeysChange::None, CurveChange::None) => KeysChange::None,
            (KeysChange::None, CurveChange::Changing) => KeysChange::Changing(tracks_of(data, &curves, &origin)),
            (KeysChange::None, CurveChange::Finished) => KeysChange::Finished {
                label: "edit curves".to_owned(),
                tracks: tracks_of(data, &curves, &origin),
            },
            (keys, _) => keys,
        };
        if let Some(frame) = moved {
            move_playhead(store, view, object, frame, events);
        }
        let idle = idle(ui);
        self.apply_keys(store, view, sequence, keys, idle, events);
        response
    }

    /// Makes what a view did to its sequence's keys: a change under way at once, recording
    /// nothing; a change done as one undo entry from the tracks before it began; a change dropped,
    /// nothing being done by the user any more, undone.
    fn apply_keys(
        &mut self,
        store: &mut Ui,
        view: Handle,
        sequence: Handle,
        keys: KeysChange,
        idle: bool,
        events: &mut Vec<SignalData>,
    ) {
        let generation = store.object(sequence).map(|object| object.generation);
        // Changed elsewhere since the view changed it: what the change began from no longer stands.
        if self
            .editing
            .get(&view)
            .is_some_and(|editing| Some(editing.generation) != generation)
        {
            self.editing.remove(&view);
        }
        match keys {
            KeysChange::None => {
                if idle && let Some(editing) = self.editing.remove(&view) {
                    let _ = store.set_tracks_under_way(sequence, editing.before);
                }
            }
            KeysChange::Changing(tracks) => {
                if let Err(error) = check_tracks(&tracks) {
                    log::warn!("the keys changed in a view were not kept: {error}");
                    return;
                }
                let before = match self.editing.remove(&view) {
                    Some(editing) => editing.before,
                    None => store
                        .sequence(sequence)
                        .map(|data| data.tracks.clone())
                        .unwrap_or_default(),
                };
                let text = store
                    .has_slots(view, Signal::KeysChanged)
                    .then(|| tracks_to_json(&tracks).to_string());
                if store.set_tracks_under_way(sequence, tracks).is_ok() {
                    let generation = store.object(sequence).map_or(0, |object| object.generation);
                    self.editing.insert(view, Editing { before, generation });
                    if let Some(text) = text {
                        events.push(SignalData {
                            sender: view,
                            signal: Signal::KeysChanged as u32,
                            text,
                            ..Default::default()
                        });
                    }
                }
            }
            KeysChange::Finished { label, tracks } => {
                let before = self.editing.remove(&view).map(|editing| editing.before);
                match change_keys(store, view, sequence, before.clone(), tracks, &label) {
                    Ok(event) => events.extend(event),
                    Err(error) => {
                        log::warn!("the keys changed in a view were not kept: {error}");
                        if let Some(before) = before {
                            let _ = store.set_tracks_under_way(sequence, before);
                        }
                    }
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

/// Makes a change of keys done in a view to its sequence, as one undo entry named `label` from the
/// tracks `before` it began, and returns the signal telling it, when a slot receives it. Tracks
/// that break the rules of a file, or a change the kernel refuses to record, are an error.
fn change_keys(
    store: &mut Ui,
    view: Handle,
    sequence: Handle,
    before: Option<Vec<Track>>,
    tracks: Vec<Track>,
    label: &str,
) -> Result<Option<SignalData>, String> {
    check_tracks(&tracks)?;
    let text = store
        .has_slots(view, Signal::KeysChanged)
        .then(|| tracks_to_json(&tracks).to_string());
    store.finish_tracks(sequence, before, tracks, label)?;
    Ok(text.map(|text| SignalData {
        sender: view,
        signal: Signal::KeysChanged as u32,
        text,
        boolean: true,
        ..Default::default()
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use uniwow_api::curve::{CurveChange, CurveEditor, CurveOptions, ShownCurve, TimeAxis};
    use uniwow_api::dopesheet::{CurveProperties, Dopesheet, DopesheetInput, DopesheetOutput, KeysChange};
    use uniwow_api::sequence::Track;
    use uniwow_api::serde_json::Value;
    use uniwow_api::ui::{Handle, Kind, Property, SharedUi, Signal, SignalData, Ui, lock};
    use uniwow_api::{
        AppliedChange, CommandInfo, Editor, EditorBackend, Event, PropertyInfo, PropertyKind, PropertyValue, egui,
    };

    use super::PanelView;

    type Jobs = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;
    /// The changes the module would record.
    type Changes = Arc<Mutex<Vec<Box<dyn AppliedChange>>>>;

    /// The left of a curve view hiding nothing and changing nothing.
    fn nothing_hidden() -> CurveProperties {
        CurveProperties {
            keys: KeysChange::None,
            playhead: None,
            hidden: Default::default(),
        }
    }

    /// A dopesheet whose left of a curve view hides the curve of `cube/opacity`.
    struct Hiding;

    impl Dopesheet for Hiding {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            _input: &DopesheetInput,
            _time: &mut TimeAxis,
        ) -> DopesheetOutput {
            DopesheetOutput {
                keys: KeysChange::None,
                playhead: None,
            }
        }

        fn curve_properties(&self, _ui: &mut egui::Ui, _id: egui::Id, _input: &DopesheetInput) -> CurveProperties {
            CurveProperties {
                hidden: [("cube/opacity".to_owned(), 0)].into(),
                ..nothing_hidden()
            }
        }

        fn forget(&self, _id: egui::Id) {}
    }

    /// A curve editor noting which curves it was given shown.
    #[derive(Default)]
    struct Seeing(Mutex<Vec<bool>>);

    impl CurveEditor for Seeing {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            curves: &mut [ShownCurve],
            _time: &mut TimeAxis,
            _options: &CurveOptions,
        ) -> CurveChange {
            *self.0.lock().unwrap() = curves.iter().map(|shown| shown.visible).collect();
            CurveChange::None
        }
    }
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

        fn curve_properties(&self, _ui: &mut egui::Ui, _id: egui::Id, _input: &DopesheetInput) -> CurveProperties {
            nothing_hidden()
        }

        fn forget(&self, _id: egui::Id) {}
    }

    /// A dopesheet changing nothing, which notes the dopesheets it is told to forget.
    #[derive(Default)]
    struct Forgetting(Mutex<Vec<egui::Id>>);

    impl Dopesheet for Forgetting {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            _input: &DopesheetInput,
            _time: &mut TimeAxis,
        ) -> DopesheetOutput {
            DopesheetOutput {
                keys: KeysChange::None,
                playhead: None,
            }
        }

        fn curve_properties(&self, _ui: &mut egui::Ui, _id: egui::Id, _input: &DopesheetInput) -> CurveProperties {
            nothing_hidden()
        }

        fn forget(&self, id: egui::Id) {
            self.0.lock().unwrap().push(id);
        }
    }

    /// An editor whose one property, `cube/opacity`, notes whether the objects were free when it
    /// was read.
    struct Catalogue {
        objects: SharedUi,
        free: Mutex<Vec<bool>>,
    }

    impl EditorBackend for Catalogue {
        fn commands(&self) -> Vec<CommandInfo> {
            Vec::new()
        }

        fn call(&self, _caller: &str, _name: &str, _arguments: Value) -> Result<Value, String> {
            Err("no command".to_owned())
        }

        fn publish(&self, _source: &str, _topic: &str, _payload: Value) -> Result<(), String> {
            Ok(())
        }

        fn subscribe(&self, _caller: &str, _topic: &str) -> Result<u64, String> {
            Err("no event".to_owned())
        }

        fn next_event(
            &self,
            _caller: &str,
            _subscription: u64,
            _timeout: std::time::Duration,
        ) -> Result<Option<Event>, String> {
            Ok(None)
        }

        fn unsubscribe(&self, _subscription: u64) {}

        fn setting(&self, _caller: &str, _space: &str, _key: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }

        fn set_setting(&self, _caller: &str, _space: &str, _key: &str, _value: Value) -> Result<(), String> {
            Ok(())
        }

        fn begin_group(&self, _caller: &str, _label: &str) -> Result<(), String> {
            Ok(())
        }

        fn end_group(&self, _caller: &str) -> Result<(), String> {
            Ok(())
        }

        fn record_change(&self, _caller: &str, _label: &str, _change: Box<dyn AppliedChange>) -> Result<(), String> {
            Ok(())
        }

        fn properties(&self) -> Vec<PropertyInfo> {
            vec![PropertyInfo {
                path: "cube/opacity".to_owned(),
                owner: "cube".to_owned(),
                label: "Opacity".to_owned(),
                kind: PropertyKind::Number,
                range: [0.0, 1.0],
            }]
        }

        fn read_property(&self, _caller: &str, _path: &str) -> Result<PropertyValue, String> {
            self.free.lock().unwrap().push(self.objects.try_lock().is_ok());
            Ok(PropertyValue::Number(0.25))
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

        fn curve_properties(&self, _ui: &mut egui::Ui, _id: egui::Id, _input: &DopesheetInput) -> CurveProperties {
            nothing_hidden()
        }

        fn forget(&self, _id: egui::Id) {}
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
        changes: Changes,
        signals: Sent,
    }

    const TRACKS: &str =
        r#"[{"property":"cube/opacity","kind":"number","curves":[{"keys":[{"time":0,"value":0.5}]}]}]"#;

    fn fixture(kind: Kind) -> Fixture {
        let jobs: Jobs = Arc::default();
        let queue = jobs.clone();
        let shared = Ui::new(Arc::new(move |job| queue.lock().unwrap().push(job)));
        let recorded: Arc<Mutex<Vec<String>>> = Arc::default();
        let changes: Changes = Arc::default();
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
            let (labels, kept) = (recorded.clone(), changes.clone());
            store.set_recorder(Arc::new(move |label, change| {
                labels.lock().unwrap().push(label.to_owned());
                kept.lock().unwrap().push(change);
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
            changes,
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

        fn tracks(&self) -> Vec<Track> {
            lock(&self.shared).sequence(self.sequence).unwrap().tracks.clone()
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

    /// A dopesheet giving `keys`.
    fn giving(keys: KeysChange) -> Option<Arc<dyn Dopesheet>> {
        Some(Arc::new(Giving(DopesheetOutput { keys, playhead: None })))
    }

    #[test]
    fn a_change_of_keys_under_way_shows_at_once_and_is_undone_when_dropped() {
        let fixture = fixture(Kind::DopesheetView);
        let mut panels = PanelView {
            dopesheet: giving(KeysChange::Changing(moved_to(3.0))),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        assert_eq!(fixture.tracks(), moved_to(3.0), "shown at once");
        assert!(
            fixture.recorded.lock().unwrap().is_empty(),
            "nothing recorded while it goes on"
        );
        assert!(
            fixture
                .signals
                .lock()
                .unwrap()
                .iter()
                .any(|(signal, _, done, _)| *signal == Signal::KeysChanged && !done)
        );
        // Nothing done any more, the pointer up and no field typed in: the change was dropped.
        panels.dopesheet = giving(KeysChange::None);
        fixture.frame(&mut panels);
        assert_eq!(fixture.tracks(), moved_to(0.0));
        assert!(fixture.recorded.lock().unwrap().is_empty());
    }

    #[test]
    fn a_change_done_is_one_entry_from_where_it_began_and_one_breaking_the_rules_is_refused() {
        let fixture = fixture(Kind::DopesheetView);
        let mut panels = PanelView {
            dopesheet: giving(KeysChange::Changing(moved_to(3.0))),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        panels.dopesheet = giving(KeysChange::Changing(moved_to(4.0)));
        fixture.frame(&mut panels);
        panels.dopesheet = giving(KeysChange::Finished {
            label: "move keys".to_owned(),
            tracks: moved_to(4.0),
        });
        fixture.frame(&mut panels);
        assert_eq!(fixture.tracks(), moved_to(4.0));
        assert_eq!(*fixture.recorded.lock().unwrap(), vec!["move keys".to_owned()]);
        let mut change = fixture.changes.lock().unwrap().pop().unwrap();
        change.undo();
        assert_eq!(fixture.tracks(), moved_to(0.0), "undone to where it began");
        change.redo();
        panels.dopesheet = giving(KeysChange::Finished {
            label: "move keys".to_owned(),
            tracks: moved_to(2.5),
        });
        fixture.frame(&mut panels);
        assert_eq!(fixture.tracks(), moved_to(4.0), "between frames: refused");
        assert_eq!(fixture.recorded.lock().unwrap().len(), 1);
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
    fn a_curve_view_showing_a_sequence_changes_it_at_once_and_records_it_when_done() {
        let fixture = fixture(Kind::CurveView);
        let mut panels = PanelView {
            curve_editor: Some(Arc::new(Editing(CurveChange::Changing))),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        assert_eq!(fixture.value(), 0.75, "shown at once");
        assert!(fixture.recorded.lock().unwrap().is_empty());
        panels.curve_editor = Some(Arc::new(Editing(CurveChange::Finished)));
        fixture.frame(&mut panels);
        assert_eq!(fixture.value(), 0.75);
        assert!(panels.editing.is_empty());
        assert_eq!(*fixture.recorded.lock().unwrap(), vec!["edit curves".to_owned()]);
        fixture.changes.lock().unwrap().pop().unwrap().undo();
        assert_eq!(fixture.value(), 0.5, "undone to where the change began");
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

    #[test]
    fn the_properties_of_a_sequence_shown_are_read_while_the_objects_are_free() {
        let fixture = fixture(Kind::DopesheetView);
        let catalogue = Arc::new(Catalogue {
            objects: fixture.shared.clone(),
            free: Mutex::default(),
        });
        let mut panels = PanelView {
            dopesheet: Some(Arc::new(Forgetting::default())),
            editor: Some(Editor::new(catalogue.clone(), "test")),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        assert_eq!(
            *catalogue.free.lock().unwrap(),
            vec![true],
            "read once, the objects unlocked"
        );
    }

    #[test]
    fn the_dopesheet_forgets_a_view_that_is_gone() {
        let fixture = fixture(Kind::DopesheetView);
        let sheet = Arc::new(Forgetting::default());
        let mut panels = PanelView {
            dopesheet: Some(sheet.clone()),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        assert!(sheet.0.lock().unwrap().is_empty());
        lock(&fixture.shared).destroy(fixture.view).unwrap();
        fixture.frame(&mut panels);
        assert_eq!(sheet.0.lock().unwrap().len(), 1);
        assert!(panels.sheet_ids.is_empty());
    }

    #[test]
    fn the_left_of_a_curve_view_hides_the_curves_it_unticks() {
        let fixture = fixture(Kind::CurveView);
        let seeing = Arc::new(Seeing::default());
        let mut panels = PanelView {
            curve_editor: Some(seeing.clone()),
            dopesheet: Some(Arc::new(Hiding)),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        assert_eq!(*seeing.0.lock().unwrap(), vec![false]);
    }
}
