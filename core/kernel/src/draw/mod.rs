//! Draws the panels of a module from its interface objects, on the interface thread, and turns
//! what the user does into signals.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

mod grid;
mod painter;
mod scene;

use painter::replay;
use scene::{SceneView, modifiers};
use uniwow_api::curve::{self, CurveChange, CurveEditor, CurveOptions, CurveOutput, ShownCurve, TimeAxis};
use uniwow_api::dopesheet::{self, Dopesheet, DopesheetInput, KeysChange, RowProperty};
use uniwow_api::property_grid::PropertyGrid;
use uniwow_api::sequence::{Sequence, Track, number_colour, number_names, tracks_to_json};
use uniwow_api::ui::data::TreeItem;
use uniwow_api::ui::{Handle, Kind, Object, Property, SharedUi, Signal, SignalData, Ui, lock};
use uniwow_api::{Editor, EditorBackend, PropertyInfo, PropertyKind, egui, egui_wgpu, log};

/// What the interface thread keeps of a module's panels between frames.
#[derive(Default)]
pub struct PanelView {
    scenes: HashMap<Handle, SceneView>,
    /// The size each painting area was last asked to paint.
    painted: HashMap<Handle, [f64; 2]>,
    /// The height, or width, each child of a box layout that does not expand took last frame.
    sizes: HashMap<Handle, f32>,
    /// The curve editor, which draws the curve views, when a running module provides it.
    curve_editor: Option<Arc<dyn CurveEditor>>,
    /// The dopesheet, which draws the dopesheet views, when a running module provides it.
    dopesheet: Option<Arc<dyn Dopesheet>>,
    /// The property grid, which draws the property grids, when a running module provides it.
    property_grid: Option<Arc<dyn PropertyGrid>>,
    /// The module's editor: the labels and values of the properties its sequences animate and its
    /// grids show.
    editor: Option<Editor>,
    /// The kernel's side of the editors, to record a value changed in a grid as an undo entry of
    /// the property's module.
    backend: Option<Arc<dyn EditorBackend>>,
    grids: grid::Grids,
    /// The animatable properties, by path, read before the objects are locked.
    infos: HashMap<String, PropertyInfo>,
    /// The properties of the tracks each view shows, read before the objects are locked.
    rows: HashMap<Handle, HashMap<String, RowProperty>>,
    /// The id each dopesheet view was drawn under, for the dopesheet to forget it once it is gone.
    sheet_ids: HashMap<Handle, egui::Id>,
    /// The ids each curve view was drawn under, for the curve editor to forget them once it is
    /// gone.
    curve_ids: HashMap<Handle, HashSet<egui::Id>>,
    /// The frame `rows` were read for: once a frame, whatever the panels and dialogs drawn.
    prepared: Option<u64>,
    /// The time axis of each curve view and dopesheet view.
    time_axes: HashMap<AxisKey, TimeAxis>,
    /// The change of its sequence each view has under way.
    editing: HashMap<Handle, Editing>,
    /// Why a service failed while drawing, by service, for its provider to be reported.
    failures: Vec<(&'static str, String)>,
    /// The cell each table view has being edited.
    cell_edits: HashMap<Handle, CellEdit>,
    /// What the user did in tree and table views, applied once the copy of the object drawn is
    /// dropped, so that their data is changed in place rather than copied.
    data_actions: Vec<(Handle, DataAction)>,
    /// The rows each tree view shows, made again only when its items or their folding change.
    flat_trees: HashMap<Handle, FlatTree>,
    /// How many times a tree's rows were made.
    #[cfg(test)]
    flattened: usize,
}

/// The rows a tree view shows, for the version of its items they were made from.
#[derive(Default)]
struct FlatTree {
    version: Option<u64>,
    rows: Vec<FlatRow>,
}

/// A row of a tree view: its depth, the row of its parent (`NO_PARENT` at the top), and its place
/// among its parent's children.
#[derive(Clone, Copy)]
struct FlatRow {
    depth: u32,
    parent: u32,
    child: u32,
}

const NO_PARENT: u32 = u32::MAX;

/// What the user did in a tree view or a table view during a frame.
enum DataAction {
    /// An item clicked; an item folded or unfolded, and whether now unfolded.
    Tree {
        clicked: Option<u64>,
        toggled: Option<(u64, bool)>,
    },
    Table(TableActions),
}

/// A cell of a table view being edited in place: its row's id, its column, and the text typed.
struct CellEdit {
    row: u64,
    column: usize,
    text: String,
    /// Whether the field has been given the keyboard.
    focused: bool,
}

/// What the user did in a table view during a frame.
#[derive(Default)]
struct TableActions {
    /// The column whose header was clicked.
    sorted: Option<usize>,
    /// The cell clicked, by row id and column.
    clicked: Option<(u64, usize)>,
    /// The cell double-clicked, to edit.
    edit: Option<(u64, usize)>,
    /// The text of the cell edited, done.
    done: Option<(u64, usize, String)>,
}

/// Whose time axis a view draws on: that of a player and the sequence it shows, which the views of
/// the same player showing the same sequence share, or its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum AxisKey {
    Played { player: Handle, sequence: Handle },
    View(Handle),
}

/// A change of a view's sequence under way, shown at once: the sequence, the tracks before the
/// change began, and those the view set.
struct Editing {
    sequence: Handle,
    before: Vec<Track>,
    /// The tracks the view set last: others mean that the tracks changed elsewhere.
    set: Vec<Track>,
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
                Kind::GraphicsView
                | Kind::PaintArea
                | Kind::CurveView
                | Kind::DopesheetView
                | Kind::TreeView
                | Kind::TableView
                | Kind::PropertyGrid => true,
                Kind::VBoxLayout | Kind::HBoxLayout | Kind::GridLayout | Kind::GroupBox => {
                    object.children.iter().any(|child| expands(store, *child))
                }
                _ => false,
            }
    })
}

impl PanelView {
    /// The services the views are drawn with, the curve editor, the dopesheet and the property
    /// grid, from the modules providing them; the editor of the module whose objects are drawn,
    /// and the kernel's side of the editors.
    pub fn set_services(
        &mut self,
        curve_editor: Option<Arc<dyn CurveEditor>>,
        dopesheet: Option<Arc<dyn Dopesheet>>,
        property_grid: Option<Arc<dyn PropertyGrid>>,
        editor: Editor,
        backend: Arc<dyn EditorBackend>,
    ) {
        self.curve_editor = curve_editor;
        self.dopesheet = dopesheet;
        self.property_grid = property_grid;
        self.editor = Some(editor);
        self.backend = Some(backend);
    }

    /// Why services panicked, once, with the id of each service: its provider is the culprit (F5).
    pub fn take_failures(&mut self) -> Vec<(&'static str, String)> {
        std::mem::take(&mut self.failures)
    }

    /// Reads, before the objects are locked, the labels and values of the properties the sequences
    /// shown animate and the grids show: reading a property runs its module's code, which may lock
    /// objects.
    fn prepare(&mut self, shared: &SharedUi, ctx: &egui::Context) {
        let pass = ctx.cumulative_pass_nr();
        if self.prepared == Some(pass) {
            return;
        }
        self.prepared = Some(pass);
        let (shown, grids) = {
            let store = lock(shared);
            (store.shown_sequences(), store.property_grids())
        };
        self.rows.clear();
        // The axis of a sequence no view shows with its player any more is forgotten: shown again,
        // it fits again.
        let played: HashSet<AxisKey> = shown
            .iter()
            .filter_map(|shown| {
                Some(AxisKey::Played {
                    player: shown.player?,
                    sequence: shown.sequence,
                })
            })
            .collect();
        self.time_axes
            .retain(|key, _| matches!(key, AxisKey::View(_)) || played.contains(key));
        if shown.is_empty() && grids.is_empty() {
            self.grids.read(&[], &self.infos, None);
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
        for shown in shown {
            let rows = self.row_properties(&shown.data, shown.time);
            self.rows.insert(shown.view, rows);
        }
        self.grids.read(&grids, &self.infos, self.editor.as_ref());
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
    fn forget_gone(&mut self, store: &mut Ui) {
        let gone: Vec<Handle> = self
            .editing
            .keys()
            .filter(|view| store.object(**view).is_none())
            .copied()
            .collect();
        for view in gone {
            self.drop_editing(store, view);
        }
        let alive = |handle: &Handle| store.object(*handle).is_some();
        self.scenes.retain(|handle, _| alive(handle));
        self.painted.retain(|handle, _| alive(handle));
        self.sizes.retain(|handle, _| alive(handle));
        self.cell_edits.retain(|handle, _| alive(handle));
        self.flat_trees.retain(|handle, _| alive(handle));
        self.time_axes.retain(|key, _| match key {
            AxisKey::Played { player, sequence } => alive(player) && alive(sequence),
            AxisKey::View(view) => alive(view),
        });
        self.grids.forget_gone(alive, self.property_grid.as_ref());
        let sheet = self.dopesheet.clone();
        self.sheet_ids.retain(|handle, id| {
            let kept = alive(handle);
            if !kept && let Some(sheet) = &sheet {
                sheet.forget(*id);
            }
            kept
        });
        let editor = self.curve_editor.clone();
        self.curve_ids.retain(|handle, ids| {
            let kept = alive(handle);
            if !kept && let Some(editor) = &editor {
                for id in ids.iter() {
                    editor.forget(*id);
                }
            }
            kept
        });
    }

    /// Draws the panel `panel` of a module.
    pub fn show(&mut self, shared: &SharedUi, panel: &str, ui: &mut egui::Ui, gpu: Option<&egui_wgpu::RenderState>) {
        self.prepare(shared, ui.ctx());
        let mut store = lock(shared);
        store.set_wake(ui.ctx());
        self.forget_gone(&mut store);
        let layout = store
            .find_panel(panel)
            .and_then(|handle| store.object(handle))
            .and_then(|object| object.children.first().copied());
        let Some(layout) = layout else {
            ui.weak("Waiting for its module to fill this panel.");
            drop(store);
            self.grids.write(self.editor.as_ref(), self.backend.as_ref());
            return;
        };
        let mut events = Vec::new();
        self.object(&mut store, shared, layout, ui, gpu, &mut events);
        for event in events {
            store.emit(event);
        }
        drop(store);
        self.grids.write(self.editor.as_ref(), self.backend.as_ref());
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
        self.prepare(shared, ctx);
        let mut store = lock(shared);
        store.set_wake(ctx);
        self.forget_gone(&mut store);
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
        drop(store);
        self.grids.write(self.editor.as_ref(), self.backend.as_ref());
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
        let kind = object.kind;
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
        drop(object);
        for (handle, action) in std::mem::take(&mut self.data_actions) {
            match action {
                DataAction::Tree { clicked, toggled } => apply_tree(store, handle, clicked, toggled, events),
                DataAction::Table(actions) => {
                    if let Some(edit) = apply_table(store, handle, actions, events) {
                        self.cell_edits.insert(handle, edit);
                    }
                }
            }
        }
        // Once a frame at most, whatever the module changed since.
        if kind == Kind::TableView {
            store.start_sort(handle);
        }
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
                    self.drop_editing(store, handle);
                    return Some(
                        ui.allocate_ui(size, |ui| ui.weak("No curve editor: no running module provides one."))
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
                self.drop_editing(store, handle);
                let mut curves = object.curves.to_vec();
                let time = self.time_axis(handle, object, None);
                let inner = ui.allocate_ui(size, |ui| {
                    let id = ui.id().with(("uniwow-curves", handle));
                    let shown = catch_unwind(AssertUnwindSafe(|| {
                        editor.show(ui, id, &mut curves, time, &CurveOptions::default())
                    }));
                    (id, shown)
                });
                let (id, shown) = inner.inner;
                self.curve_ids.entry(handle).or_default().insert(id);
                let change = match shown {
                    Ok(output) => output.change,
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
                        target.curves = std::sync::Arc::new(curves);
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
                    self.drop_editing(store, handle);
                    return Some(
                        ui.allocate_ui(size, |ui| ui.weak("No dopesheet: no running module provides one."))
                            .response,
                    );
                };
                let Some((sequence, data)) = object
                    .plays
                    .and_then(|sequence| Some((sequence, store.sequence(sequence).ok()?)))
                else {
                    self.drop_editing(store, handle);
                    return Some(ui.allocate_ui(size, |ui| ui.weak("No sequence shown.")).response);
                };
                let playhead = playhead(store, object);
                let properties = self.rows.remove(&handle).unwrap_or_default();
                let id = ui.id().with(("uniwow-dopesheet", handle));
                self.sheet_ids.insert(handle, id);
                let time = self.time_axis(handle, object, Some(sequence));
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
                        self.drop_editing(store, handle);
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
            Kind::TreeView => Some(self.tree_view(handle, object, ui)),
            Kind::TableView => Some(self.table_view(handle, object, ui)),
            Kind::PropertyGrid => {
                let service = self.property_grid.clone();
                Some(self.grids.show(service, handle, object, ui, &mut self.failures))
            }
        }
    }

    /// A tree view: each item a row, indented under its parent, a triangle folding or unfolding its
    /// children; a click makes it current. Only the rows in sight are drawn.
    fn tree_view(&mut self, handle: Handle, object: &Object, ui: &mut egui::Ui) -> egui::Response {
        let size = egui::vec2(
            ui.available_width(),
            ui.available_height().max(object.minimum_height as f32),
        );
        let items = object.tree.clone().unwrap_or_default();
        let flat = self.flat_trees.entry(handle).or_default();
        if flat.version != Some(object.tree_version) {
            flat.rows = flatten(&items);
            flat.version = Some(object.tree_version);
            #[cfg(test)]
            {
                self.flattened += 1;
            }
        }
        let rows = &self.flat_trees[&handle].rows;
        let (mut clicked, mut toggled) = (None, None);
        let inner = ui.allocate_ui(size, |ui| {
            let height = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical()
                .id_salt(("uniwow-tree", handle))
                .auto_shrink([false, false])
                .show_rows(ui, height, rows.len(), |ui, positions| {
                    for position in positions {
                        if let Some(item) = flat_item(&items, rows, position) {
                            let depth = rows[position].depth as usize;
                            tree_row(ui, depth, item, object.current_item, height, &mut clicked, &mut toggled);
                        }
                    }
                });
        });
        if clicked.is_some() || toggled.is_some() {
            self.data_actions.push((handle, DataAction::Tree { clicked, toggled }));
        }
        inner.response
    }

    /// A table view: its headers, a click sorting by a column, then again from the highest; its rows
    /// in the order shown, only those in sight drawn; a click makes a cell current, a double click
    /// edits it in place, Enter or leaving it keeps the text, Escape drops it.
    fn table_view(&mut self, handle: Handle, object: &Object, ui: &mut egui::Ui) -> egui::Response {
        let size = egui::vec2(
            ui.available_width(),
            ui.available_height().max(object.minimum_height as f32),
        );
        let table = object.table.clone().unwrap_or_default();
        // A row removed meanwhile ends the edit of its cell.
        let mut edit = self
            .cell_edits
            .remove(&handle)
            .filter(|edit| table.row(edit.row).is_some());
        let mut actions = TableActions::default();
        let mut edit_drawn = false;
        let inner = ui.allocate_ui(size, |ui| {
            let columns = table.columns().len().max(1);
            let width = ((ui.available_width() - 16.0) / columns as f32).max(60.0);
            let height = ui.spacing().interact_size.y;
            let spacing = ui.spacing().item_spacing;
            let (column_step, row_step) = (width + spacing.x, height + spacing.y);
            // The columns whose room crosses `from..to`, along the rows.
            let in_sight = |from: f32, to: f32| {
                let first = ((from / column_step).floor().max(0.0) as usize).min(columns);
                let last = ((to / column_step).ceil().max(0.0) as usize).min(columns);
                first..last.max(first)
            };
            // The header's room, filled once the rows tell how far they are scrolled sideways.
            let (header, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
            ui.separator();
            let rows = egui::ScrollArea::both()
                .id_salt(("uniwow-table", handle))
                .auto_shrink([false, false])
                .show_viewport(ui, |ui, viewport| {
                    ui.set_width(columns as f32 * column_step);
                    ui.set_height((row_step * table.len() as f32 - spacing.y).max(0.0));
                    let first_row = (viewport.min.y / row_step).floor().max(0.0) as usize;
                    let last_row = ((viewport.max.y / row_step).ceil().max(0.0) as usize + 1).min(table.len());
                    let shown = in_sight(viewport.min.x, viewport.max.x);
                    let area = egui::Rect::from_min_max(
                        ui.max_rect().min + egui::vec2(shown.start as f32 * column_step, first_row as f32 * row_step),
                        egui::pos2(ui.max_rect().max.x, ui.max_rect().top() + last_row as f32 * row_step),
                    );
                    ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
                        for position in first_row..last_row.max(first_row) {
                            let Some(row) = table.shown(position) else {
                                continue;
                            };
                            ui.horizontal(|ui| {
                                for column in shown.clone() {
                                    let text = row.cells.get(column).map_or("", String::as_str);
                                    let current = row.id == object.current_item;
                                    edit_drawn |= edit.as_ref().is_some_and(|e| e.row == row.id && e.column == column);
                                    table_cell(
                                        ui,
                                        row.id,
                                        column,
                                        text,
                                        current,
                                        [width, height],
                                        &mut edit,
                                        &mut actions,
                                    );
                                }
                            });
                        }
                    });
                });
            let scrolled = rows.state.offset.x;
            let mut ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(header.translate(egui::vec2(-scrolled, 0.0)).with_max_x(f32::INFINITY))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            ui.set_clip_rect(header.intersect(ui.clip_rect()));
            let sorting = table
                .is_sorting()
                .then(|| table.sort().map(|(column, _)| column))
                .flatten();
            let shown = in_sight(scrolled, scrolled + header.width());
            ui.add_space(shown.start as f32 * column_step);
            for column in shown {
                let state = if sorting == Some(column) {
                    Header::Sorting
                } else {
                    match table.shown_sort() {
                        Some((sorted, descending)) if sorted == column => Header::Sorted { descending },
                        _ => Header::Plain,
                    }
                };
                let label = table.columns().get(column).map_or("", String::as_str);
                if column_header(&mut ui, label, state, [width, height]).clicked() {
                    actions.sorted = Some(column);
                }
            }
        });
        // The cell edited scrolled out of sight: its text is kept, as in Qt, the field being gone.
        if let Some(gone) = edit.take_if(|edit| edit.focused && !edit_drawn) {
            actions.done = Some((gone.row, gone.column, gone.text));
        }
        if let Some(edit) = edit {
            self.cell_edits.insert(handle, edit);
        }
        if actions.sorted.is_some() || actions.clicked.is_some() || actions.edit.is_some() || actions.done.is_some() {
            self.data_actions.push((handle, DataAction::Table(actions)));
        }
        inner.response
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
        self.curve_ids.entry(view).or_default().insert(id.with("curves"));
        let time = self.time_axis(view, object, Some(sequence));
        let shown = catch_unwind(AssertUnwindSafe(|| {
            editor.show(&mut child, id.with("curves"), &mut curves, time, &options)
        }));
        let CurveOutput {
            change,
            playhead: on_ruler,
        } = match shown {
            Ok(output) => output,
            Err(panic) => {
                let message = format!("the curve editor panicked: {}", panic_text(&*panic));
                self.failures.push((curve::SERVICE.id(), message));
                self.drop_editing(store, view);
                return response;
            }
        };
        let keys = match (left_keys, change) {
            (KeysChange::None, CurveChange::None) => KeysChange::None,
            (KeysChange::None, CurveChange::Changing) => KeysChange::Changing(tracks_of(data, &curves, &origin)),
            (KeysChange::None, CurveChange::Finished) => KeysChange::Finished {
                label: "edit curves".to_owned(),
                tracks: tracks_of(data, &curves, &origin),
            },
            (keys, _) => keys,
        };
        if let Some(frame) = moved.or(on_ruler) {
            move_playhead(store, view, object, frame, events);
        }
        let idle = idle(ui);
        self.apply_keys(store, view, sequence, keys, idle, events);
        response
    }

    /// The time axis of `view`: when it shows `sequence` with a player, that of the player and the
    /// sequence, shared with the views showing them, new when first shown so that the view fits it;
    /// its own otherwise.
    fn time_axis(&mut self, view: Handle, object: &Object, sequence: Option<Handle>) -> &mut TimeAxis {
        let key = match (sequence, object.player) {
            (Some(sequence), Some(player)) => AxisKey::Played { player, sequence },
            _ => AxisKey::View(view),
        };
        self.time_axes.entry(key).or_default()
    }

    /// Puts back the sequence a view was changing, when the view can no longer end the change: gone,
    /// or drawn without its service. Not when the tracks changed elsewhere meanwhile.
    fn drop_editing(&mut self, store: &mut Ui, view: Handle) {
        if let Some(editing) = self.editing.remove(&view)
            && store
                .sequence(editing.sequence)
                .is_ok_and(|data| data.tracks == editing.set)
        {
            let _ = store.set_tracks_under_way(editing.sequence, editing.before);
        }
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
        // Changed elsewhere since the view changed them: what the change began from no longer
        // stands. A change of the frame rate or the length leaves the tracks.
        let stale = self.editing.get(&view).is_some_and(|editing| {
            editing.sequence != sequence || store.sequence(sequence).map_or(true, |data| data.tracks != editing.set)
        });
        if stale {
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
                if store.set_tracks_under_way(sequence, tracks.clone()).is_ok() {
                    let editing = Editing {
                        sequence,
                        before,
                        set: tracks,
                    };
                    self.editing.insert(view, editing);
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

/// What the user did in a tree view: an item folded or unfolded, an item clicked, current.
fn apply_tree(
    store: &mut Ui,
    handle: Handle,
    clicked: Option<u64>,
    toggled: Option<(u64, bool)>,
    events: &mut Vec<SignalData>,
) {
    let signal = |signal: Signal, item: u64| SignalData {
        sender: handle,
        signal: signal as u32,
        item,
        ..Default::default()
    };
    if let Some((item, expanded)) = toggled
        && store.set_item_expanded(handle, item, expanded) == Ok(true)
    {
        events.push(SignalData {
            boolean: expanded,
            ..signal(Signal::ItemExpanded, item)
        });
    }
    let Some(target) = store.object_mut(handle) else {
        return;
    };
    if let Some(item) = clicked {
        events.push(signal(Signal::ItemClicked, item));
        if item != target.current_item {
            target.current_item = item;
            events.push(signal(Signal::CurrentItemChanged, item));
        }
    }
}

/// What the user did in a table view: a column sorted by, from the highest when sorted by it
/// already; a cell made current; a cell's text kept. Returns the edit of a cell double-clicked.
fn apply_table(
    store: &mut Ui,
    handle: Handle,
    actions: TableActions,
    events: &mut Vec<SignalData>,
) -> Option<CellEdit> {
    let object = store.object(handle)?;
    let table = object.table.as_ref()?;
    let (sort, current) = (table.sort(), (object.current_item, object.current_column));
    // A cell given back with the text it had changes nothing, and tells nothing, as in Qt.
    let unchanged = actions
        .done
        .as_ref()
        .is_some_and(|(row, column, text)| table.row(*row).and_then(|found| found.cells.get(*column)) == Some(text));
    let edit = actions.edit.map(|(row, column)| CellEdit {
        row,
        column,
        text: table
            .row(row)
            .and_then(|found| found.cells.get(column).cloned())
            .unwrap_or_default(),
        focused: false,
    });
    let signal = |signal: Signal, item: u64, column: usize| SignalData {
        sender: handle,
        signal: signal as u32,
        item,
        integer: column as i64,
        ..Default::default()
    };
    if let Some(column) = actions.sorted {
        let descending = sort == Some((column, false));
        if store.set_sort(handle, Some((column, descending))).is_ok() {
            events.push(SignalData {
                boolean: descending,
                ..signal(Signal::SortChanged, 0, column)
            });
        }
    }
    if let Some((row, column)) = actions.clicked
        && (row, column) != current
        && let Some(target) = store.object_mut(handle)
    {
        target.current_item = row;
        target.current_column = column;
        events.push(signal(Signal::CurrentCellChanged, row, column));
    }
    if let Some((row, column, text)) = actions.done.filter(|_| !unchanged)
        && store.set_cell(handle, row, column, &text).is_ok()
    {
        events.push(SignalData {
            text,
            ..signal(Signal::CellChanged, row, column)
        });
    }
    edit
}

/// The rows of a tree: each item, and under it the children of one unfolded.
fn flatten(items: &[TreeItem]) -> Vec<FlatRow> {
    let mut rows = Vec::new();
    // Each level down: the row of its parent, its items, and the next one.
    let mut levels: Vec<(u32, &[TreeItem], usize)> = vec![(NO_PARENT, items, 0)];
    while let Some(&(parent, children, next)) = levels.last() {
        let depth = levels.len() - 1;
        let Some(item) = children.get(next) else {
            levels.pop();
            continue;
        };
        levels[depth].2 += 1;
        rows.push(FlatRow {
            depth: depth as u32,
            parent,
            child: next as u32,
        });
        if item.expanded && !item.children.is_empty() {
            levels.push(((rows.len() - 1) as u32, &item.children, 0));
        }
    }
    rows
}

/// The item of the row at `position`, found from the top by the places of its parents.
fn flat_item<'a>(items: &'a [TreeItem], rows: &[FlatRow], position: usize) -> Option<&'a TreeItem> {
    let mut places = [0u32; uniwow_api::ui::data::MAX_DEPTH];
    let mut depth = 0;
    let mut at = position;
    loop {
        let row = rows.get(at)?;
        *places.get_mut(depth)? = row.child;
        depth += 1;
        if row.parent == NO_PARENT {
            break;
        }
        at = row.parent as usize;
    }
    let mut level = items;
    let mut found = None;
    for &place in places[..depth].iter().rev() {
        let item = level.get(place as usize)?;
        level = &item.children;
        found = Some(item);
    }
    found
}

/// The row of an item of a tree view at `depth`.
fn tree_row(
    ui: &mut egui::Ui,
    depth: usize,
    item: &TreeItem,
    current: u64,
    height: f32,
    clicked: &mut Option<u64>,
    toggled: &mut Option<(u64, bool)>,
) {
    ui.horizontal(|ui| {
        ui.set_height(height);
        ui.add_space(depth as f32 * 16.0);
        if item.children.is_empty() {
            // The place of the triangle, for the texts to line up.
            ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
        } else if fold_button(ui, item.expanded).clicked() {
            *toggled = Some((item.id, !item.expanded));
        }
        if ui.selectable_label(item.id == current, &item.text).clicked() {
            *clicked = Some(item.id);
        }
    });
}

/// What the header of a column shows besides its text.
#[derive(Clone, Copy)]
enum Header {
    Plain,
    /// The rows are shown sorted by the column: a triangle pointing up, or down from the highest,
    /// the fonts having no arrow.
    Sorted {
        descending: bool,
    },
    /// The rows are being sorted by the column, off the interface's thread.
    Sorting,
}

/// The header of a column of a table view.
fn column_header(ui: &mut egui::Ui, header: &str, state: Header, size: [f32; 2]) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size[0], size[1]), egui::Sense::click());
    let visuals = ui.style().interact(&response);
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, visuals.weak_bg_fill);
    }
    let colour = ui.visuals().strong_text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(header.to_owned(), egui::TextStyle::Body.resolve(ui.style()), colour);
    let at = rect.left_center() + egui::vec2(4.0, -galley.size().y / 2.0);
    let end = at.x + galley.size().x;
    ui.painter_at(rect).galley(at, galley, colour);
    match state {
        Header::Plain => {}
        Header::Sorted { descending } => {
            let centre = egui::pos2(end + 9.0, rect.center().y);
            let (tip, base) = if descending { (4.0, -3.0) } else { (-4.0, 3.0) };
            ui.painter_at(rect).add(egui::Shape::convex_polygon(
                vec![
                    centre + egui::vec2(0.0, tip),
                    centre + egui::vec2(4.0, base),
                    centre + egui::vec2(-4.0, base),
                ],
                colour,
                egui::Stroke::NONE,
            ));
        }
        Header::Sorting => {
            ui.painter_at(rect).text(
                egui::pos2(end + 6.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "sorting…",
                egui::TextStyle::Small.resolve(ui.style()),
                ui.visuals().weak_text_color(),
            );
        }
    }
    response
}

/// A triangle folding or unfolding the children of an item, the fonts having no such character.
fn fold_button(ui: &mut egui::Ui, unfolded: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
    let colour = ui.style().interact(&response).fg_stroke.color;
    let (centre, size) = (rect.center(), 4.0);
    let points = if unfolded {
        vec![
            centre + egui::vec2(-size, -size * 0.6),
            centre + egui::vec2(size, -size * 0.6),
            centre + egui::vec2(0.0, size),
        ]
    } else {
        vec![
            centre + egui::vec2(-size * 0.6, -size),
            centre + egui::vec2(size, 0.0),
            centre + egui::vec2(-size * 0.6, size),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(points, colour, egui::Stroke::NONE));
    response
}

/// A cell of a table view: its text, or the field editing it.
#[allow(clippy::too_many_arguments)]
fn table_cell(
    ui: &mut egui::Ui,
    row: u64,
    column: usize,
    text: &str,
    current: bool,
    size: [f32; 2],
    edit: &mut Option<CellEdit>,
    actions: &mut TableActions,
) {
    if let Some(editing) = edit.as_mut().filter(|edit| edit.row == row && edit.column == column) {
        let response = ui.add_sized(size, egui::TextEdit::singleline(&mut editing.text));
        if !editing.focused {
            response.request_focus();
            editing.focused = true;
        } else if response.lost_focus() {
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                actions.done = Some((row, column, editing.text.clone()));
            }
            *edit = None;
        }
        return;
    }
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size[0], size[1]), egui::Sense::click());
    let visuals = ui.visuals();
    if current {
        ui.painter()
            .rect_filled(rect, 0.0, visuals.selection.bg_fill.gamma_multiply(0.5));
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, visuals.widgets.hovered.weak_bg_fill);
    }
    ui.painter_at(rect.shrink2(egui::vec2(4.0, 0.0))).text(
        rect.left_center() + egui::vec2(4.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        egui::TextStyle::Body.resolve(ui.style()),
        visuals.text_color(),
    );
    if response.double_clicked() {
        actions.edit = Some((row, column));
    } else if response.clicked() {
        actions.clicked = Some((row, column));
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

    use uniwow_api::curve::{CurveChange, CurveEditor, CurveOptions, CurveOutput, ShownCurve, TimeAxis};
    use uniwow_api::dopesheet::{CurveProperties, Dopesheet, DopesheetInput, DopesheetOutput, KeysChange};
    use uniwow_api::sequence::Track;
    use uniwow_api::serde_json::Value;
    use uniwow_api::ui::{Handle, Kind, Property, SharedUi, Signal, SignalData, Ui, lock};
    use uniwow_api::{
        AppliedChange, CommandInfo, Editor, EditorBackend, Event, PropertyInfo, PropertyKind, PropertyValue, egui,
    };

    use super::{PanelView, TableActions, apply_table, apply_tree, flat_item, flatten};
    use uniwow_api::ui::data::{Row, Rows, TreeItem};

    type Jobs = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;
    /// The changes the module would record.
    type Changes = Arc<Mutex<Vec<Box<dyn AppliedChange>>>>;

    /// What a curve editor gives when the user did nothing.
    fn unchanged() -> CurveOutput {
        CurveOutput {
            change: CurveChange::None,
            playhead: None,
        }
    }

    /// A dopesheet noting the time axis it is given, which it zooms to 7 points a frame from frame 3
    /// when it has none yet.
    #[derive(Default)]
    struct Zooming(Mutex<Vec<TimeAxis>>);

    impl Dopesheet for Zooming {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            _input: &DopesheetInput,
            time: &mut TimeAxis,
        ) -> DopesheetOutput {
            self.0.lock().unwrap().push(*time);
            if time.pixels_per_unit == 0.0 {
                *time = TimeAxis {
                    first: 3.0,
                    pixels_per_unit: 7.0,
                };
            }
            DopesheetOutput {
                keys: KeysChange::None,
                playhead: None,
            }
        }

        fn curve_properties(&self, _ui: &mut egui::Ui, _id: egui::Id, _input: &DopesheetInput) -> CurveProperties {
            nothing_hidden()
        }

        fn forget(&self, _id: egui::Id) {}
    }

    /// A curve editor noting the time axis it is given, and moving the playhead to `.1` on its ruler.
    #[derive(Default)]
    struct Ruling(Mutex<Vec<TimeAxis>>, Option<f64>);

    impl CurveEditor for Ruling {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            _curves: &mut [ShownCurve],
            time: &mut TimeAxis,
            _options: &CurveOptions,
        ) -> CurveOutput {
            self.0.lock().unwrap().push(*time);
            CurveOutput {
                playhead: self.1,
                ..unchanged()
            }
        }

        fn forget(&self, _id: egui::Id) {}
    }

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
        ) -> CurveOutput {
            *self.0.lock().unwrap() = curves.iter().map(|shown| shown.visible).collect();
            unchanged()
        }

        fn forget(&self, _id: egui::Id) {}
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
        ) -> CurveOutput {
            curves[0].curve.keys[0].value = 0.75;
            CurveOutput {
                change: self.0,
                playhead: None,
            }
        }

        fn forget(&self, _id: egui::Id) {}
    }

    /// A panel holding a view of `kind` on a sequence with a key of 0.5 at frame 0 on
    /// `cube/opacity`, played by a player at frame 5; what the module would record, and the
    /// signals the view sends.
    struct Fixture {
        ctx: egui::Context,
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
            ctx: egui::Context::default(),
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
            let mut output = self
                .ctx
                .run_ui(egui::RawInput::default(), |ui| panels.show(&self.shared, "p", ui, None));
            output.textures_delta.clear();
            self.run_slots();
        }

        /// A frame drawing the panel and the dialogs, as the kernel does.
        fn frame_with_dialogs(&self, panels: &mut PanelView) {
            let mut output = self.ctx.run_ui(egui::RawInput::default(), |ui| {
                panels.show(&self.shared, "p", ui, None);
                panels.dialogs(&self.shared, ui.ctx(), None);
            });
            output.textures_delta.clear();
            self.run_slots();
        }

        fn run_slots(&self) {
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
        ) -> CurveOutput {
            panic!("broken editor")
        }

        fn forget(&self, _id: egui::Id) {}
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

    /// A panel holding one view of `kind`.
    fn data_view(kind: Kind) -> (SharedUi, Handle) {
        let shared = Ui::new(Arc::new(|_job| {}));
        let view = {
            let mut store = lock(&shared);
            let panel = store.panel("p");
            let layout = store.create(Kind::VBoxLayout, None).unwrap();
            store.add_to(panel, layout, [0, 0, 1, 1]).unwrap();
            let view = store.create(kind, None).unwrap();
            store.add_to(layout, view, [0, 0, 1, 1]).unwrap();
            view
        };
        (shared, view)
    }

    /// The signals told, each by its number, item, integer, boolean and text.
    fn told(events: &[SignalData]) -> Vec<(Signal, u64, i64, bool, &str)> {
        events
            .iter()
            .map(|data| {
                let signal = Signal::from_u32(data.signal).unwrap();
                (signal, data.item, data.integer, data.boolean, data.text.as_str())
            })
            .collect()
    }

    #[test]
    fn a_table_view_sorts_makes_current_and_keeps_an_edit_telling_each() {
        let (shared, table) = data_view(Kind::TableView);
        let mut store = lock(&shared);
        store.set_text(table, Property::Columns, r#"["Id","Name"]"#).unwrap();
        store
            .set_text(
                table,
                Property::Rows,
                r#"[{"id":1,"cells":["1","b"]},{"id":2,"cells":["2","a"]}]"#,
            )
            .unwrap();
        let mut events = Vec::new();
        for _ in 0..3 {
            let sorted = TableActions {
                sorted: Some(1),
                ..TableActions::default()
            };
            assert!(apply_table(&mut store, table, sorted, &mut events).is_none());
        }
        assert_eq!(store.table(table).unwrap().sort(), Some((1, false)));
        assert_eq!(
            told(&events),
            vec![
                (Signal::SortChanged, 0, 1, false, ""),
                (Signal::SortChanged, 0, 1, true, ""),
                (Signal::SortChanged, 0, 1, false, ""),
            ],
            "from the lowest, then the highest, then the lowest again"
        );
        events.clear();

        for _ in 0..2 {
            let clicked = TableActions {
                clicked: Some((2, 1)),
                ..TableActions::default()
            };
            apply_table(&mut store, table, clicked, &mut events);
        }
        assert_eq!(store.numbers(table, Property::CurrentItem).unwrap(), vec![2.0]);
        assert_eq!(
            told(&events),
            vec![(Signal::CurrentCellChanged, 2, 1, false, "")],
            "a cell already current is not told again"
        );
        events.clear();

        let edit = TableActions {
            edit: Some((1, 1)),
            ..TableActions::default()
        };
        let edit = apply_table(&mut store, table, edit, &mut events).unwrap();
        assert_eq!(
            (edit.row, edit.column, edit.text.as_str()),
            (1, 1, "b"),
            "from the cell's text"
        );
        let done = TableActions {
            done: Some((1, 1, "c".to_owned())),
            ..TableActions::default()
        };
        apply_table(&mut store, table, done, &mut events);
        assert_eq!(store.table(table).unwrap().row(1).unwrap().cells[1], "c");
        let gone = TableActions {
            done: Some((9, 1, "x".to_owned())),
            ..TableActions::default()
        };
        apply_table(&mut store, table, gone, &mut events);
        assert_eq!(
            told(&events),
            vec![(Signal::CellChanged, 1, 1, false, "c")],
            "the edit of a row removed meanwhile is dropped"
        );
    }

    #[test]
    fn a_tree_view_folds_and_makes_current_what_is_clicked_telling_each() {
        let (shared, tree) = data_view(Kind::TreeView);
        let mut store = lock(&shared);
        store
            .set_text(
                tree,
                Property::Items,
                r#"[{"id":1,"text":"a","children":[{"id":2,"text":"b"}]}]"#,
            )
            .unwrap();
        let mut events = Vec::new();
        apply_tree(&mut store, tree, Some(2), Some((1, true)), &mut events);
        assert!(store.items(tree).unwrap()[0].expanded);
        apply_tree(&mut store, tree, Some(2), None, &mut events);
        apply_tree(&mut store, tree, None, Some((9, true)), &mut events);
        assert_eq!(
            told(&events),
            vec![
                (Signal::ItemExpanded, 1, 0, true, ""),
                (Signal::ItemClicked, 2, 0, false, ""),
                (Signal::CurrentItemChanged, 2, 0, false, ""),
                (Signal::ItemClicked, 2, 0, false, ""),
            ],
            "the current item clicked again tells the click only; an item it does not hold, nothing"
        );
        assert_eq!(store.numbers(tree, Property::CurrentItem).unwrap(), vec![2.0]);
    }

    #[test]
    fn a_header_clicked_sorts_the_table_in_place_once_drawn() {
        let (shared, table) = data_view(Kind::TableView);
        {
            let mut store = lock(&shared);
            store.set_text(table, Property::Columns, r#"["Id"]"#).unwrap();
            store
                .set_text(
                    table,
                    Property::Rows,
                    r#"[{"id":1,"cells":["2"]},{"id":2,"cells":["1"]}]"#,
                )
                .unwrap();
        }
        let address = Arc::as_ptr(&lock(&shared).table(table).unwrap());
        let mut panels = PanelView::default();
        let ctx = egui::Context::default();
        let header = egui::pos2(20.0, 8.0);
        let frame = |panels: &mut PanelView, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
                events,
                ..egui::RawInput::default()
            };
            let mut output = ctx.run_ui(input, |ui| panels.show(&shared, "p", ui, None));
            output.textures_delta.clear();
        };
        let button = |pressed| egui::Event::PointerButton {
            pos: header,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut panels, vec![egui::Event::PointerMoved(header)]);
        frame(&mut panels, vec![button(true)]);
        frame(&mut panels, vec![button(false)]);
        let sorted = lock(&shared).table(table).unwrap();
        assert_eq!(sorted.sort(), Some((0, false)), "the header was clicked");
        assert!(
            std::ptr::eq(Arc::as_ptr(&sorted), address),
            "the table is changed in place, not copied"
        );
    }

    /// A frame of `shared`'s panel on a screen of 800 by 600 with `events`: each text drawn within
    /// its clip, where, and how many texts were drawn, clipped or not.
    fn texts_and_count(
        ctx: &egui::Context,
        panels: &mut PanelView,
        shared: &SharedUi,
        events: Vec<egui::Event>,
    ) -> (Vec<(String, egui::Pos2)>, usize) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            events,
            ..egui::RawInput::default()
        };
        let mut output = ctx.run_ui(input, |ui| panels.show(shared, "p", ui, None));
        output.textures_delta.clear();
        let count = output
            .shapes
            .iter()
            .filter(|clipped| matches!(clipped.shape, egui::Shape::Text(_)))
            .count();
        let texts = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if clipped.clip_rect.contains(text.pos) => {
                    Some((text.galley.text().to_owned(), text.pos))
                }
                _ => None,
            })
            .collect();
        (texts, count)
    }

    /// The texts a frame draws within their clip, where (see `texts_and_count`).
    fn texts_drawn(
        ctx: &egui::Context,
        panels: &mut PanelView,
        shared: &SharedUi,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Pos2)> {
        texts_and_count(ctx, panels, shared, events).0
    }

    #[test]
    fn the_columns_beyond_the_width_of_a_table_are_reached_by_scrolling_and_only_those_in_sight_drawn() {
        let (shared, table) = data_view(Kind::TableView);
        {
            let columns: Vec<String> = (0..30).map(|column| format!("\"C{column}\"")).collect();
            let rows: Vec<String> = (1..=3)
                .map(|row| {
                    let cells: Vec<String> = (0..30).map(|column| format!("\"r{row}c{column}\"")).collect();
                    format!(r#"{{"id":{row},"cells":[{}]}}"#, cells.join(","))
                })
                .collect();
            let mut store = lock(&shared);
            store
                .set_text(table, Property::Columns, &format!("[{}]", columns.join(",")))
                .unwrap();
            store
                .set_text(table, Property::Rows, &format!("[{}]", rows.join(",")))
                .unwrap();
        }
        let ctx = egui::Context::default();
        let mut panels = PanelView::default();
        let has = |texts: &[(String, egui::Pos2)], text: &str| texts.iter().any(|(drawn, _)| drawn == text);
        let (first, count) = texts_and_count(
            &ctx,
            &mut panels,
            &shared,
            vec![egui::Event::PointerMoved(egui::pos2(400.0, 100.0))],
        );
        assert!(has(&first, "C0") && has(&first, "r1c0"));
        assert!(!has(&first, "C29") && !has(&first, "r1c29"), "out of sight");
        assert!(count < 30 * 4 / 2, "only the columns in sight drawn: {count} texts");
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(-5000.0, 0.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        };
        let mut last = texts_drawn(&ctx, &mut panels, &shared, vec![wheel]);
        for _ in 0..60 {
            last = texts_drawn(&ctx, &mut panels, &shared, Vec::new());
        }
        assert!(has(&last, "C29") && has(&last, "r3c29"), "scrolled to the last column");
        assert!(!has(&last, "C0"), "the first column out of sight now");
        // Its header, scrolled with the rows, sorts its column.
        let at = last.iter().find(|(text, _)| text == "C29").expect("drawn").1 + egui::vec2(4.0, 4.0);
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        texts_drawn(&ctx, &mut panels, &shared, vec![egui::Event::PointerMoved(at)]);
        texts_drawn(&ctx, &mut panels, &shared, vec![button(true)]);
        texts_drawn(&ctx, &mut panels, &shared, vec![button(false)]);
        assert_eq!(lock(&shared).table(table).unwrap().sort(), Some((29, false)));
    }

    #[test]
    fn a_cell_edited_then_scrolled_out_of_sight_keeps_its_text() {
        let (shared, table) = data_view(Kind::TableView);
        {
            let rows: Vec<Row> = (1..=300)
                .map(|id| Row {
                    id,
                    cells: vec![id.to_string()],
                })
                .collect();
            let mut store = lock(&shared);
            store.set_text(table, Property::Columns, r#"["Id"]"#).unwrap();
            store.set_rows(table, Rows::new(rows).unwrap()).unwrap();
        }
        let ctx = egui::Context::default();
        let mut panels = PanelView::default();
        let shown = texts_drawn(&ctx, &mut panels, &shared, Vec::new());
        let at = shown.iter().find(|(text, _)| text == "1").expect("row 1 drawn").1 + egui::vec2(4.0, 4.0);
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        texts_drawn(&ctx, &mut panels, &shared, vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false, true, false] {
            texts_drawn(&ctx, &mut panels, &shared, vec![button(pressed)]);
        }
        texts_drawn(&ctx, &mut panels, &shared, Vec::new());
        texts_drawn(&ctx, &mut panels, &shared, vec![egui::Event::Text("zz".to_owned())]);
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -3000.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        };
        texts_drawn(&ctx, &mut panels, &shared, vec![wheel]);
        for _ in 0..40 {
            texts_drawn(&ctx, &mut panels, &shared, Vec::new());
        }
        let store = lock(&shared);
        assert_eq!(store.table(table).unwrap().row(1).unwrap().cells[0], "1zz");
        assert!(panels.cell_edits.is_empty(), "the edit is done");
    }

    #[test]
    fn a_cell_given_back_with_its_text_tells_nothing() {
        let (shared, table) = data_view(Kind::TableView);
        let mut store = lock(&shared);
        store.set_text(table, Property::Columns, r#"["Id"]"#).unwrap();
        store
            .set_text(table, Property::Rows, r#"[{"id":1,"cells":["a"]}]"#)
            .unwrap();
        let mut events = Vec::new();
        let same = TableActions {
            done: Some((1, 0, "a".to_owned())),
            ..TableActions::default()
        };
        apply_table(&mut store, table, same, &mut events);
        assert!(events.is_empty());
        let other = TableActions {
            done: Some((1, 0, "b".to_owned())),
            ..TableActions::default()
        };
        apply_table(&mut store, table, other, &mut events);
        assert_eq!(told(&events), vec![(Signal::CellChanged, 1, 0, false, "b")]);
    }

    /// Draws `shared`'s panel with a screen of 800 by 600 and `events`; returns how long it took.
    fn timed_frame(
        ctx: &egui::Context,
        panels: &mut PanelView,
        shared: &SharedUi,
        events: Vec<egui::Event>,
    ) -> std::time::Duration {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            events,
            ..egui::RawInput::default()
        };
        let started = std::time::Instant::now();
        let mut output = ctx.run_ui(input, |ui| panels.show(shared, "p", ui, None));
        output.textures_delta.clear();
        started.elapsed()
    }

    #[test]
    fn a_header_clicked_in_a_million_rows_leaves_every_frame_short_while_they_sort() {
        let (shared, table) = data_view(Kind::TableView);
        let count = 1_000_000u64;
        {
            // Values in no order, for the sort to take long.
            let many: Vec<Row> = (1..=count)
                .map(|id| Row {
                    id,
                    cells: vec![(id * 2_654_435_761 % 1_000_000_007).to_string()],
                })
                .collect();
            let mut store = lock(&shared);
            store.set_text(table, Property::Columns, r#"["Value"]"#).unwrap();
            store.set_rows(table, Rows::new(many).unwrap()).unwrap();
        }
        let mut panels = PanelView::default();
        let ctx = egui::Context::default();
        let header = egui::pos2(20.0, 8.0);
        let button = |pressed| egui::Event::PointerButton {
            pos: header,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        timed_frame(&ctx, &mut panels, &shared, vec![egui::Event::PointerMoved(header)]);
        let mut frames = vec![
            timed_frame(&ctx, &mut panels, &shared, vec![button(true)]),
            timed_frame(&ctx, &mut panels, &shared, vec![button(false)]),
        ];
        assert_eq!(
            lock(&shared).table(table).unwrap().sort(),
            Some((0, false)),
            "the header was clicked"
        );
        let started = std::time::Instant::now();
        while lock(&shared).table(table).unwrap().is_sorting() {
            assert!(started.elapsed() < std::time::Duration::from_secs(30), "never sorted");
            frames.push(timed_frame(&ctx, &mut panels, &shared, Vec::new()));
        }
        frames.push(timed_frame(&ctx, &mut panels, &shared, Vec::new()));
        let longest = frames.iter().max().unwrap();
        assert!(
            *longest <= std::time::Duration::from_millis(33),
            "a frame took {longest:?} of {} frames",
            frames.len()
        );
        let sorted = lock(&shared).table(table).unwrap();
        let first: Vec<u64> = (0..1000)
            .map(|position| sorted.shown(position).unwrap().cells[0].parse().unwrap())
            .collect();
        assert!(sorted.shown_sort() == Some((0, false)) && first.is_sorted(), "sorted");
    }

    #[test]
    fn the_header_of_the_column_being_sorted_says_so() {
        let (shared, table) = data_view(Kind::TableView);
        {
            let many: Vec<Row> = (1..=uniwow_api::ui::data::BACKGROUND_SORT_ROWS as u64)
                .map(|id| Row {
                    id,
                    cells: vec![id.to_string()],
                })
                .collect();
            let mut store = lock(&shared);
            store.set_text(table, Property::Columns, r#"["Value"]"#).unwrap();
            store.set_rows(table, Rows::new(many).unwrap()).unwrap();
            store.set_sort(table, Some((0, true))).unwrap();
        }
        let ctx = egui::Context::default();
        let mut panels = PanelView::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| panels.show(&shared, "p", ui, None));
        output.textures_delta.clear();
        let texts: Vec<&str> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"sorting…"), "{texts:?}");
    }

    /// A panel filled with one widget, drawn on a screen of 800 by 600 with time going on: the
    /// signals of `senders` its slots receive, and the labels the module would record.
    struct Interactive {
        ctx: egui::Context,
        shared: SharedUi,
        jobs: Jobs,
        signals: Arc<Mutex<Vec<SignalData>>>,
        recorded: Arc<Mutex<Vec<String>>>,
        time: std::cell::Cell<f64>,
    }

    impl Interactive {
        /// A panel holding, one under the other, the widgets `fill` makes in the store, connected
        /// for `signals` on the objects `fill` returns besides them.
        fn new(fill: impl FnOnce(&mut Ui) -> (Vec<Handle>, Vec<Handle>), signals: &[Signal]) -> Self {
            let jobs: Jobs = Arc::default();
            let queue = jobs.clone();
            let shared = Ui::new(Arc::new(move |job| queue.lock().unwrap().push(job)));
            let seen: Arc<Mutex<Vec<SignalData>>> = Arc::default();
            let recorded: Arc<Mutex<Vec<String>>> = Arc::default();
            {
                let mut store = lock(&shared);
                let panel = store.panel("p");
                let layout = store.create(Kind::VBoxLayout, None).unwrap();
                store.add_to(panel, layout, [0, 0, 1, 1]).unwrap();
                let (widgets, senders) = fill(&mut store);
                for (row, widget) in (0..).zip(widgets) {
                    store.add_to(layout, widget, [row, 0, 1, 1]).unwrap();
                }
                for sender in senders {
                    for signal in signals {
                        let seen = seen.clone();
                        store
                            .connect(
                                sender,
                                *signal,
                                Arc::new(move |data: &SignalData| seen.lock().unwrap().push(data.clone())),
                            )
                            .unwrap();
                    }
                }
                let labels = recorded.clone();
                store.set_recorder(Arc::new(move |label, _change| {
                    labels.lock().unwrap().push(label.to_owned());
                    Ok(())
                }));
            }
            Self {
                ctx: egui::Context::default(),
                shared,
                jobs,
                signals: seen,
                recorded,
                time: std::cell::Cell::new(0.0),
            }
        }

        /// A frame `after` seconds after the last, with `events`; then the slots run. Returns
        /// the rectangle of the widget's background and the texts drawn.
        fn frame(
            &self,
            panels: &mut PanelView,
            after: f64,
            events: Vec<egui::Event>,
        ) -> (Option<egui::Rect>, Vec<String>) {
            self.time.set(self.time.get() + after);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
                time: Some(self.time.get()),
                events,
                ..egui::RawInput::default()
            };
            let mut output = self.ctx.run_ui(input, |ui| panels.show(&self.shared, "p", ui, None));
            output.textures_delta.clear();
            for job in std::mem::take(&mut *self.jobs.lock().unwrap()) {
                job();
            }
            let background = self.ctx.global_style().visuals.extreme_bg_color;
            let mut rect = None;
            let mut texts = Vec::new();
            for clipped in &output.shapes {
                match &clipped.shape {
                    egui::Shape::Rect(shape) if shape.fill == background => rect = Some(shape.rect),
                    egui::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                    _ => {}
                }
            }
            (rect, texts)
        }

        /// The signals received, emptied.
        fn signals(&self) -> Vec<SignalData> {
            std::mem::take(&mut *self.signals.lock().unwrap())
        }
    }

    fn pointer(at: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// A view of a scene: a group, movable, with a tooltip, holding a selectable card from -100 to
    /// 100 on both axes; the view, the scene, the group and the card.
    fn card_scene(store: &mut Ui) -> (Handle, Handle, Handle, Handle) {
        let view = store.create(Kind::GraphicsView, None).unwrap();
        let scene = store.create(Kind::GraphicsScene, None).unwrap();
        store.set_scene(view, scene).unwrap();
        let group = store.create(Kind::ItemGroup, Some(scene)).unwrap();
        store.set_numbers(group, Property::Movable, &[3.0]).unwrap();
        store.set_text(group, Property::ToolTip, "the card").unwrap();
        let card = store.create(Kind::RectItem, Some(group)).unwrap();
        store
            .set_numbers(card, Property::Rect, &[-100.0, -100.0, 200.0, 200.0])
            .unwrap();
        store.set_numbers(card, Property::Selectable, &[1.0]).unwrap();
        (view, scene, group, card)
    }

    const SCENE_SIGNALS: [Signal; 4] = [
        Signal::ItemPressed,
        Signal::ItemMoved,
        Signal::ItemDoubleClicked,
        Signal::SelectionChanged,
    ];

    fn scene_fixture() -> (Interactive, Handle, Handle, Handle) {
        let handles = std::cell::Cell::new((0, 0, 0));
        let fixture = Interactive::new(
            |store| {
                let (view, scene, group, card) = card_scene(store);
                handles.set((view, group, card));
                (vec![view], vec![scene])
            },
            &SCENE_SIGNALS,
        );
        let (view, group, card) = handles.get();
        (fixture, view, group, card)
    }

    /// Drags with the primary button from `from` by `by`, in frames a sixtieth of a second apart.
    fn drag(fixture: &Interactive, panels: &mut PanelView, from: egui::Pos2, by: egui::Vec2) {
        let step = 1.0 / 60.0;
        fixture.frame(panels, step, vec![egui::Event::PointerMoved(from)]);
        fixture.frame(panels, step, vec![pointer(from, egui::PointerButton::Primary, true)]);
        fixture.frame(panels, step, vec![egui::Event::PointerMoved(from + by / 2.0)]);
        fixture.frame(panels, step, vec![egui::Event::PointerMoved(from + by)]);
        fixture.frame(
            panels,
            step,
            vec![pointer(from + by, egui::PointerButton::Primary, false)],
        );
    }

    #[test]
    fn an_item_of_a_scene_pressed_and_dragged_moves_its_group_and_tells_the_module_only() {
        let (fixture, _view, group, card) = scene_fixture();
        let mut panels = PanelView::default();
        let (rect, _) = fixture.frame(&mut panels, 0.0, Vec::new());
        let middle = rect.expect("the view is drawn").center();
        drag(&fixture, &mut panels, middle, egui::vec2(50.0, 20.0));
        assert_eq!(
            lock(&fixture.shared).numbers(group, Property::Pos).unwrap(),
            vec![50.0, 20.0]
        );
        let told: Vec<(Signal, Handle, f64, f64)> = fixture
            .signals()
            .iter()
            .map(|data| (Signal::from_u32(data.signal).unwrap(), data.item, data.dx, data.dy))
            .collect();
        assert_eq!(
            told,
            vec![
                (Signal::ItemPressed, card, 0.0, 0.0),
                (Signal::SelectionChanged, 0, 0.0, 0.0),
                (Signal::ItemMoved, group, 50.0, 20.0),
            ]
        );
        assert!(lock(&fixture.shared).object(card).unwrap().selected);
        assert!(
            fixture.recorded.lock().unwrap().is_empty(),
            "the module records the move itself"
        );
    }

    #[test]
    fn the_bounds_of_an_item_hold_it_while_it_is_dragged() {
        let (fixture, _view, group, _card) = scene_fixture();
        lock(&fixture.shared)
            .set_numbers(group, Property::MoveBounds, &[0.0, 0.0, 10.0, 10.0])
            .unwrap();
        let mut panels = PanelView::default();
        let (rect, _) = fixture.frame(&mut panels, 0.0, Vec::new());
        drag(
            &fixture,
            &mut panels,
            rect.expect("drawn").center(),
            egui::vec2(50.0, 20.0),
        );
        assert_eq!(
            lock(&fixture.shared).numbers(group, Property::Pos).unwrap(),
            vec![10.0, 10.0]
        );
        let moved = fixture
            .signals()
            .into_iter()
            .find(|data| data.signal == Signal::ItemMoved as u32);
        assert_eq!(moved.map(|data| (data.dx, data.dy)), Some((10.0, 10.0)));
    }

    #[test]
    fn the_wheel_zooms_around_the_pointer_and_the_middle_button_scrolls_outside_the_history() {
        let (fixture, view, _group, _card) = scene_fixture();
        let mut panels = PanelView::default();
        let (rect, _) = fixture.frame(&mut panels, 0.0, Vec::new());
        let rect = rect.expect("drawn");
        let at = rect.center() + egui::vec2(100.0, 50.0);
        let shown = || {
            let store = lock(&fixture.shared);
            let scale = store.numbers(view, Property::ViewScale).unwrap()[0];
            let center = store.numbers(view, Property::ViewCenter).unwrap();
            (scale, [center[0], center[1]])
        };
        // The scene point under the screen point `at`, as the view places it.
        let under = |scale: f64, center: [f64; 2]| {
            let local = at - rect.center();
            [
                center[0] + f64::from(local.x) / scale,
                center[1] + f64::from(local.y) / scale,
            ]
        };
        let before = under(1.0, [0.0, 0.0]);
        fixture.frame(&mut panels, 1.0 / 60.0, vec![egui::Event::PointerMoved(at)]);
        fixture.frame(
            &mut panels,
            1.0 / 60.0,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 60.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        for _ in 0..60 {
            fixture.frame(&mut panels, 1.0 / 60.0, Vec::new());
        }
        let (scale, center) = shown();
        assert!(scale > 1.0, "zoomed in: {scale}");
        let after = under(scale, center);
        assert!(
            (after[0] - before[0]).abs() < 1e-6 && (after[1] - before[1]).abs() < 1e-6,
            "the point under the pointer stays: {before:?} then {after:?}"
        );
        let step = 1.0 / 60.0;
        fixture.frame(&mut panels, step, vec![pointer(at, egui::PointerButton::Middle, true)]);
        fixture.frame(
            &mut panels,
            step,
            vec![egui::Event::PointerMoved(at + egui::vec2(20.0, 0.0))],
        );
        fixture.frame(
            &mut panels,
            step,
            vec![egui::Event::PointerMoved(at + egui::vec2(40.0, 0.0))],
        );
        fixture.frame(
            &mut panels,
            step,
            vec![pointer(at + egui::vec2(40.0, 0.0), egui::PointerButton::Middle, false)],
        );
        let (_, scrolled) = shown();
        assert!(
            (scrolled[0] - (center[0] - 40.0 / scale)).abs() < 1e-6,
            "{center:?} then {scrolled:?}"
        );
        assert_eq!(scrolled[1], center[1]);
        assert!(fixture.signals().is_empty(), "nothing told");
        assert!(
            fixture.recorded.lock().unwrap().is_empty(),
            "nothing enters the history"
        );
    }

    #[test]
    fn a_double_click_on_an_item_tells_the_module() {
        let (fixture, _view, _group, card) = scene_fixture();
        let mut panels = PanelView::default();
        let (rect, _) = fixture.frame(&mut panels, 0.0, Vec::new());
        let middle = rect.expect("drawn").center();
        let step = 1.0 / 60.0;
        fixture.frame(&mut panels, step, vec![egui::Event::PointerMoved(middle)]);
        for pressed in [true, false, true, false] {
            fixture.frame(
                &mut panels,
                step,
                vec![pointer(middle, egui::PointerButton::Primary, pressed)],
            );
        }
        let double: Vec<Handle> = fixture
            .signals()
            .iter()
            .filter(|data| data.signal == Signal::ItemDoubleClicked as u32)
            .map(|data| data.item)
            .collect();
        assert_eq!(double, vec![card]);
    }

    #[test]
    fn the_tooltip_of_an_item_or_of_its_group_shows_once_the_pointer_rests_on_it() {
        let (fixture, _view, _group, _card) = scene_fixture();
        let mut panels = PanelView::default();
        let (rect, _) = fixture.frame(&mut panels, 0.0, Vec::new());
        let rect = rect.expect("drawn");
        fixture.frame(&mut panels, 0.1, vec![egui::Event::PointerMoved(rect.center())]);
        let mut shown = Vec::new();
        for _ in 0..20 {
            shown = fixture.frame(&mut panels, 0.1, Vec::new()).1;
        }
        assert!(shown.iter().any(|text| text == "the card"), "{shown:?}");
        // Off the card, on the background: none.
        fixture.frame(
            &mut panels,
            0.1,
            vec![egui::Event::PointerMoved(rect.min + egui::vec2(5.0, 5.0))],
        );
        for _ in 0..20 {
            shown = fixture.frame(&mut panels, 0.1, Vec::new()).1;
        }
        assert!(!shown.iter().any(|text| text == "the card"), "{shown:?}");
    }

    #[test]
    fn the_mouse_over_a_painting_area_reaches_its_module_where_it_is_in_the_area() {
        let area = std::cell::Cell::new(0);
        let fixture = Interactive::new(
            |store| {
                // A label above, for the area not to start where the screen does.
                let label = store.create(Kind::Label, None).unwrap();
                store.set_text(label, Property::Text, "above").unwrap();
                let made = store.create(Kind::PaintArea, None).unwrap();
                area.set(made);
                (vec![label, made], vec![made])
            },
            &[
                Signal::MousePress,
                Signal::MouseMove,
                Signal::MouseRelease,
                Signal::Wheel,
            ],
        );
        let mut panels = PanelView::default();
        let step = 1.0 / 60.0;
        let at = egui::pos2(100.0, 100.0);
        fixture.frame(&mut panels, step, Vec::new());
        fixture.frame(&mut panels, step, vec![egui::Event::PointerMoved(at)]);
        fixture.frame(
            &mut panels,
            step,
            vec![pointer(at, egui::PointerButton::Secondary, true)],
        );
        fixture.frame(
            &mut panels,
            step,
            vec![egui::Event::PointerMoved(at + egui::vec2(15.0, 20.0))],
        );
        fixture.frame(
            &mut panels,
            step,
            vec![egui::Event::PointerMoved(at + egui::vec2(30.0, 40.0))],
        );
        fixture.frame(
            &mut panels,
            step,
            vec![pointer(
                at + egui::vec2(30.0, 40.0),
                egui::PointerButton::Secondary,
                false,
            )],
        );
        fixture.frame(
            &mut panels,
            step,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 10.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        for _ in 0..30 {
            fixture.frame(&mut panels, step, Vec::new());
        }
        let told = fixture.signals();
        assert!(told.iter().all(|data| data.sender == area.get()));
        let press = told
            .iter()
            .find(|data| data.signal == Signal::MousePress as u32)
            .expect("pressed");
        assert_eq!(press.button, 2, "the right button");
        // Where the area is on screen, under the label: the press tells it.
        let (x, y) = (100.0 - press.x, 100.0 - press.y);
        assert!(
            (0.0..20.0).contains(&x) && (10.0..40.0).contains(&y),
            "the area starts at ({x}, {y})"
        );
        let moved = told
            .iter()
            .rfind(|data| data.signal == Signal::MouseMove as u32)
            .expect("moved");
        assert_eq!((moved.x, moved.y), (press.x + 30.0, press.y + 40.0));
        let released = told
            .iter()
            .find(|data| data.signal == Signal::MouseRelease as u32)
            .expect("released");
        assert_eq!((released.x, released.y), (moved.x, moved.y));
        let wheeled: f64 = told
            .iter()
            .filter(|data| data.signal == Signal::Wheel as u32)
            .map(|data| data.dy)
            .sum();
        assert!(wheeled > 0.0, "the wheel told");
    }

    /// A tree of `count` items at the top, each unfolded with `children` children.
    fn wide_tree(count: u64, children: u64) -> Vec<TreeItem> {
        (0..count)
            .map(|top| TreeItem {
                id: top * (children + 1) + 1,
                text: format!("top {top}"),
                expanded: true,
                children: (0..children)
                    .map(|child| TreeItem {
                        id: top * (children + 1) + child + 2,
                        text: format!("child {child}"),
                        expanded: false,
                        children: Vec::new(),
                    })
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn the_rows_of_a_tree_are_made_again_only_when_its_items_or_their_folding_change() {
        let (shared, tree) = data_view(Kind::TreeView);
        lock(&shared).set_items(tree, wide_tree(1000, 299)).unwrap();
        let mut panels = PanelView::default();
        let ctx = egui::Context::default();
        for _ in 0..3 {
            timed_frame(&ctx, &mut panels, &shared, Vec::new());
        }
        assert_eq!(panels.flattened, 1, "300,000 rows made once for three frames");
        assert_eq!(panels.flat_trees[&tree].rows.len(), 300_000);
        lock(&shared).set_item_expanded(tree, 1, false).unwrap();
        timed_frame(&ctx, &mut panels, &shared, Vec::new());
        timed_frame(&ctx, &mut panels, &shared, Vec::new());
        assert_eq!(panels.flattened, 2, "made again once folded");
        assert_eq!(panels.flat_trees[&tree].rows.len(), 300_000 - 299);
    }

    #[test]
    fn each_row_of_a_tree_finds_its_item_and_depth() {
        let items: Vec<TreeItem> = uniwow_api::ui::read_items(
            r#"[{"id":1,"text":"a","expanded":true,"children":[
                {"id":2,"text":"b","expanded":true,"children":[{"id":3,"text":"c"}]},
                {"id":4,"text":"d","children":[{"id":5,"text":"hidden"}]}]},
               {"id":6,"text":"e"}]"#,
        )
        .unwrap();
        let rows = flatten(&items);
        let shown: Vec<(u64, u32)> = (0..rows.len())
            .map(|position| (flat_item(&items, &rows, position).unwrap().id, rows[position].depth))
            .collect();
        assert_eq!(
            shown,
            vec![(1, 0), (2, 1), (3, 2), (4, 1), (6, 0)],
            "the folded child hidden"
        );
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

    /// A curve editor noting the editors it is told to forget.
    #[derive(Default)]
    struct ForgettingCurves(Mutex<Vec<egui::Id>>);

    impl CurveEditor for ForgettingCurves {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            _curves: &mut [ShownCurve],
            _time: &mut TimeAxis,
            _options: &CurveOptions,
        ) -> CurveOutput {
            unchanged()
        }

        fn forget(&self, id: egui::Id) {
            self.0.lock().unwrap().push(id);
        }
    }

    #[test]
    fn the_curve_editor_forgets_a_view_that_is_gone() {
        let fixture = fixture(Kind::CurveView);
        let editor = Arc::new(ForgettingCurves::default());
        let mut panels = PanelView {
            curve_editor: Some(editor.clone()),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        assert!(editor.0.lock().unwrap().is_empty());
        lock(&fixture.shared).destroy(fixture.view).unwrap();
        fixture.frame(&mut panels);
        assert_eq!(editor.0.lock().unwrap().len(), 1);
        assert!(panels.curve_ids.is_empty());
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

    #[test]
    fn a_change_under_way_is_put_back_when_its_view_goes_or_is_drawn_without_its_service() {
        for gone in [true, false] {
            let fixture = fixture(Kind::DopesheetView);
            let mut panels = PanelView {
                dopesheet: giving(KeysChange::Changing(moved_to(3.0))),
                ..PanelView::default()
            };
            fixture.frame(&mut panels);
            assert_eq!(fixture.tracks(), moved_to(3.0));
            if gone {
                lock(&fixture.shared).destroy(fixture.view).unwrap();
            } else {
                panels.dopesheet = None;
            }
            fixture.frame(&mut panels);
            assert_eq!(fixture.tracks(), moved_to(0.0), "view gone: {gone}");
            assert!(fixture.recorded.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn a_change_of_length_during_a_drag_leaves_where_the_drag_began() {
        let fixture = fixture(Kind::DopesheetView);
        let mut panels = PanelView {
            dopesheet: giving(KeysChange::Changing(moved_to(3.0))),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        lock(&fixture.shared)
            .set_numbers(fixture.sequence, Property::Length, &[200.0])
            .unwrap();
        panels.dopesheet = giving(KeysChange::Finished {
            label: "move keys".to_owned(),
            tracks: moved_to(4.0),
        });
        fixture.frame(&mut panels);
        fixture.changes.lock().unwrap().pop().unwrap().undo();
        assert_eq!(fixture.tracks(), moved_to(0.0), "undone to where the drag began");
    }

    #[test]
    fn the_properties_are_read_once_a_frame_whatever_is_drawn() {
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
        fixture.frame_with_dialogs(&mut panels);
        fixture.frame_with_dialogs(&mut panels);
        assert_eq!(catalogue.free.lock().unwrap().len(), 2, "once in each frame");
    }

    #[test]
    fn views_of_the_same_player_share_their_time_axis_which_fits_again_for_another_sequence() {
        let fixture = fixture(Kind::DopesheetView);
        let other = {
            let mut store = lock(&fixture.shared);
            let panel = store.find_panel("p").unwrap();
            let layout = store.object(panel).unwrap().children[0];
            let curves = store.create(Kind::CurveView, None).unwrap();
            store
                .set_numbers(curves, Property::Sequence, &[fixture.sequence as f64])
                .unwrap();
            store
                .set_numbers(curves, Property::Player, &[fixture.player as f64])
                .unwrap();
            store.add_to(layout, curves, [0, 0, 1, 1]).unwrap();
            let other = store.create(Kind::Sequence, None).unwrap();
            (curves, other)
        };
        let (curves, other) = other;
        let sheet = Arc::new(Zooming::default());
        let ruling = Arc::new(Ruling::default());
        let mut panels = PanelView {
            dopesheet: Some(sheet.clone()),
            curve_editor: Some(ruling.clone()),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        let zoomed = TimeAxis {
            first: 3.0,
            pixels_per_unit: 7.0,
        };
        assert_eq!(*ruling.0.lock().unwrap(), vec![zoomed], "the dopesheet's zoom");
        // Both views show another sequence: the axis is made again, for them to fit it.
        {
            let mut store = lock(&fixture.shared);
            for view in [fixture.view, curves] {
                store.set_numbers(view, Property::Sequence, &[other as f64]).unwrap();
            }
        }
        fixture.frame(&mut panels);
        let given = sheet.0.lock().unwrap();
        assert_eq!(given.len(), 2);
        assert_eq!(given[1], TimeAxis::default(), "fitting again");
    }

    #[test]
    fn the_ruler_of_a_curve_view_moves_its_player_s_playhead() {
        let fixture = fixture(Kind::CurveView);
        let mut panels = PanelView {
            curve_editor: Some(Arc::new(Ruling(Mutex::default(), Some(12.0)))),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        let store = lock(&fixture.shared);
        assert_eq!(store.numbers(fixture.player, Property::Time).unwrap(), vec![12.0]);
        assert_eq!(
            store.numbers(fixture.player, Property::Playing).unwrap(),
            vec![0.0],
            "paused there"
        );
        drop(store);
        assert!(
            fixture
                .signals
                .lock()
                .unwrap()
                .contains(&(Signal::PlayheadMoved, String::new(), false, 12.0))
        );
    }

    #[test]
    fn views_of_one_player_showing_two_sequences_keep_their_own_axes_and_one_shown_again_fits() {
        let fixture = fixture(Kind::DopesheetView);
        let other = {
            let mut store = lock(&fixture.shared);
            let panel = store.find_panel("p").unwrap();
            let layout = store.object(panel).unwrap().children[0];
            let other = store.create(Kind::Sequence, None).unwrap();
            let second = store.create(Kind::DopesheetView, None).unwrap();
            store.set_numbers(second, Property::Sequence, &[other as f64]).unwrap();
            store
                .set_numbers(second, Property::Player, &[fixture.player as f64])
                .unwrap();
            store.add_to(layout, second, [0, 0, 1, 1]).unwrap();
            other
        };
        let sheet = Arc::new(Zooming::default());
        let mut panels = PanelView {
            dopesheet: Some(sheet.clone()),
            ..PanelView::default()
        };
        fixture.frame(&mut panels);
        fixture.frame(&mut panels);
        let zoomed = TimeAxis {
            first: 3.0,
            pixels_per_unit: 7.0,
        };
        assert_eq!(
            sheet.0.lock().unwrap()[2..],
            [zoomed, zoomed],
            "each keeps its axis, made once"
        );
        // The first view shows the other sequence, then its own again: it fits again.
        let show = |sequence: Handle| {
            lock(&fixture.shared)
                .set_numbers(fixture.view, Property::Sequence, &[sequence as f64])
                .unwrap();
        };
        show(other);
        fixture.frame(&mut panels);
        show(fixture.sequence);
        fixture.frame(&mut panels);
        let given = sheet.0.lock().unwrap();
        assert_eq!(
            given[given.len() - 2],
            TimeAxis::default(),
            "shown again: fitting again"
        );
    }
}
