//! The dopesheet, as the Dopesheet of Unity: the rows of a sequence, one per track unfolding into
//! one per number, with their keys as diamonds, a ruler and a playhead. Keys are selected, moved
//! and deleted, the playhead moved; on the left, each track's label and values at the playhead,
//! which set keys, a key added at the playhead and the track removed. Offered to Rust modules as a
//! service; the core also has it draw the `DopesheetView` objects of every language, and the left
//! of the `CurveView` objects showing a sequence.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::curve::{self, TimeAxis};
use uniwow_api::dopesheet::{self, CurveProperties, Dopesheet, DopesheetInput, DopesheetOutput, KeysChange};
use uniwow_api::egui::{self, Align, Align2, Color32, FontId, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use uniwow_api::hotkey::{Hotkey, HotkeyKind, Keys};
use uniwow_api::sequence::{self, KeyId, Sequence, Track, frame_of, number_colour, number_names};
use uniwow_api::{Module, PropertyKind, PropertyValue, Registrar};

/// Height of a row.
const ROW: f32 = 22.0;
const RULER: f32 = 22.0;
/// The widest the properties on the left go, and their share of a narrow dopesheet.
const LEFT: f32 = 430.0;
const LEFT_SHARE: f32 = 0.5;
/// Half the width of a key's diamond.
const DIAMOND: f32 = 5.0;
const ZOOM: [f32; 2] = [0.2, 200.0];
const PLAYHEAD: Color32 = Color32::from_rgb(225, 65, 55);
/// The most labels the ruler draws.
const MAX_TICKS: usize = 1000;

#[derive(Default)]
enum Gesture {
    #[default]
    None,
    /// The selected keys dragged, from and to these x, the sequence as it was when the drag began.
    Move { from: f32, to: f32, start: Sequence },
    /// A selection box.
    Select { from: Pos2, to: Pos2 },
}

/// What one dopesheet keeps between frames.
#[derive(Default)]
struct State {
    selection: BTreeSet<KeyId>,
    gesture: Gesture,
    /// The properties whose numbers have rows of their own.
    unfolded: BTreeSet<String>,
    /// The numbers whose curves a curve view does not show.
    hidden: BTreeSet<(String, usize)>,
    /// The tracks as this dopesheet expects to see them next, to notice when they change
    /// elsewhere: by an undo, by a module.
    seen: Vec<Track>,
    /// The frames the keys being dragged were last shown moved by.
    shown_offset: i64,
    /// The undo label of a value being set in a field, until the change is done.
    field: Option<String>,
}

impl State {
    /// Ends the gesture under way when the tracks changed elsewhere: it would act on keys that are
    /// no more.
    fn notice(&mut self, sequence: &Sequence) {
        if sequence.tracks != self.seen {
            self.gesture = Gesture::None;
            self.shown_offset = 0;
        }
        // During a drag, the keys selected are where the drag began.
        if matches!(self.gesture, Gesture::None) {
            self.selection.retain(|key| sequence.has_key(key));
        }
    }

    /// What the next frame should show: the tracks given to the caller, or those it showed.
    fn expect(&mut self, keys: &KeysChange, sequence: &Sequence) {
        self.seen = match keys {
            KeysChange::Changing(tracks) | KeysChange::Finished { tracks, .. } => tracks.clone(),
            KeysChange::None => sequence.tracks.clone(),
        };
    }
}

/// A row, with the keys it shows.
enum Row {
    /// Every key of the sequence.
    Summary,
    /// The keys of one module's tracks.
    Group(String),
    /// A property: the keys of all its numbers.
    Track(usize),
    /// One number of a property, by its index.
    Number(usize, usize),
}

/// Where frames are drawn.
#[derive(Clone, Copy)]
struct View {
    left: f32,
    first: f64,
    pixels_per_frame: f32,
}

impl View {
    fn x(self, frame: f64) -> f32 {
        self.left + ((frame - self.first) as f32) * self.pixels_per_frame
    }

    fn frame(self, x: f32) -> f64 {
        self.first + f64::from((x - self.left) / self.pixels_per_frame)
    }
}

/// What the left of the rows did during a frame.
struct Edited {
    keys: KeysChange,
    /// The frame a value was set at, where the playhead pauses.
    playhead: Option<f64>,
}

impl Edited {
    fn none() -> Self {
        Self {
            keys: KeysChange::None,
            playhead: None,
        }
    }
}

struct Sheet {
    states: Mutex<HashMap<egui::Id, State>>,
    /// The hotkey deleting the selected keys.
    delete: Hotkey,
}

/// The keys deleting the selected keys, by default.
const DELETE: Keys = Keys::key(egui::Key::Delete);

impl Default for Sheet {
    fn default() -> Self {
        Self {
            states: Mutex::default(),
            delete: Hotkey::new(HotkeyKind::Press, DELETE),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn owner(path: &str) -> &str {
    path.split_once('/').map_or(path, |(owner, _)| owner)
}

/// The rows: every key, then each module's tracks under its name.
fn rows(sequence: &Sequence, unfolded: &BTreeSet<String>) -> Vec<Row> {
    let mut rows = vec![Row::Summary];
    let mut owners: Vec<&str> = Vec::new();
    for track in &sequence.tracks {
        let owner = owner(&track.property);
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }
    for owner_name in owners {
        rows.push(Row::Group(owner_name.to_owned()));
        for (index, track) in sequence.tracks.iter().enumerate() {
            if owner(&track.property) == owner_name {
                rows.push(Row::Track(index));
                if unfolded.contains(&track.property) {
                    rows.extend((0..track.curves.len()).map(|number| Row::Number(index, number)));
                }
            }
        }
    }
    rows
}

/// The keys a row shows, by frame.
fn row_keys(sequence: &Sequence, row: &Row) -> BTreeMap<u32, Vec<KeyId>> {
    let mut keys: BTreeMap<u32, Vec<KeyId>> = BTreeMap::new();
    let (tracks, only): (Vec<&Track>, Option<usize>) = match row {
        Row::Summary => (sequence.tracks.iter().collect(), None),
        Row::Group(name) => (
            sequence
                .tracks
                .iter()
                .filter(|track| owner(&track.property) == name)
                .collect(),
            None,
        ),
        Row::Track(index) => (vec![&sequence.tracks[*index]], None),
        Row::Number(index, number) => (vec![&sequence.tracks[*index]], Some(*number)),
    };
    for track in tracks {
        for (number, curve) in track.curves.iter().enumerate() {
            if only.is_some_and(|only| only != number) {
                continue;
            }
            for key in &curve.keys {
                let frame = frame_of(key);
                keys.entry(frame)
                    .or_default()
                    .push((track.property.clone(), number, frame));
            }
        }
    }
    keys
}

/// Why a value cannot be a key, if it cannot.
const BEYOND: &str = "a value beyond 1e9 cannot be keyed";

/// Whether the numbers of `value` may be keys: within the limit of the curves.
fn keyable(value: &PropertyValue) -> bool {
    value.components().iter().all(|number| sequence::admissible(*number))
}

/// The tracks with a key of each value given, on its number of `property`, at `frame`.
fn with_keys(tracks: &[Track], property: &str, frame: f64, values: &[(usize, f64)]) -> Vec<Track> {
    let mut tracks = tracks.to_vec();
    if let Some(track) = tracks.iter_mut().find(|track| track.property == property) {
        for (number, value) in values {
            if let Some(curve) = track.curves.get_mut(*number) {
                curve.set_key(frame, *value);
            }
        }
    }
    tracks
}

/// The tracks with values set by hand on numbers of `property`, each a key at `frame`. A number
/// without a key yet, set away from frame 0, keeps there as a key the value it had `before`.
fn with_values(tracks: &[Track], property: &str, frame: f64, values: &[(usize, f64)], before: &[f64]) -> Vec<Track> {
    let mut tracks = tracks.to_vec();
    if let Some(track) = tracks.iter_mut().find(|track| track.property == property) {
        for (number, value) in values {
            if let Some(curve) = track.curves.get_mut(*number) {
                if curve.keys.is_empty()
                    && frame > 0.0
                    && let Some(old) = before.get(*number)
                {
                    curve.set_key(0.0, *old);
                }
                curve.set_key(frame, *value);
            }
        }
    }
    tracks
}

/// The tracks without that of `property`.
fn without_track(tracks: &[Track], property: &str) -> Vec<Track> {
    tracks
        .iter()
        .filter(|track| track.property != property)
        .cloned()
        .collect()
}

impl Dopesheet for Sheet {
    fn show(&self, ui: &mut egui::Ui, id: egui::Id, input: &DopesheetInput, time: &mut TimeAxis) -> DopesheetOutput {
        let mut states = lock(&self.states);
        let state = states.entry(id).or_default();
        let sequence = input.sequence;
        state.notice(sequence);
        let mut output = DopesheetOutput {
            keys: KeysChange::None,
            playhead: None,
        };

        let width = ui.available_width().max(200.0);
        let left_width = (width * LEFT_SHARE).min(LEFT);
        let (ruler, _) = ui.allocate_exact_size(egui::vec2(width, RULER), Sense::hover());
        let left = ruler.left() + left_width;
        let usable = time.pixels_per_unit.is_finite() && time.pixels_per_unit > 0.0 && time.first.is_finite();
        if !usable {
            let room = (ruler.right() - left - 24.0).max(50.0);
            time.pixels_per_unit = (room / sequence.length as f32).clamp(ZOOM[0], ZOOM[1]);
            time.first = -f64::from(12.0 / time.pixels_per_unit);
        }
        let view = View {
            left,
            first: time.first,
            pixels_per_frame: time.pixels_per_unit,
        };
        let ruler_right = Rect::from_min_max(egui::pos2(left, ruler.top()), ruler.max);
        output.playhead = draw_ruler(ui, id, sequence, input.playhead, ruler, ruler_right, view);
        let playhead = output.playhead.or(input.playhead);

        let rows = rows(sequence, &state.unfolded);
        egui::ScrollArea::vertical()
            .id_salt(id.with("rows"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let height = (rows.len() as f32 * ROW).max(ui.available_height());
                let (area, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
                let edited = left_column(state, input, playhead, &rows, area, left_width, false, ui);
                let keys = Rect::from_min_max(egui::pos2(left, area.top()), area.max);
                navigate(ui, keys.union(ruler_right), view, time);
                let moved = keys_area(state, input, &rows, keys, view, playhead, ui, id);
                output.keys = if edited.keys != KeysChange::None {
                    output.playhead = edited.playhead.or(output.playhead);
                    edited.keys
                } else if moved != KeysChange::None {
                    moved
                } else {
                    delete(state, sequence, keys, &self.delete, ui)
                };
            });
        state.expect(&output.keys, sequence);
        output
    }

    fn curve_properties(&self, ui: &mut egui::Ui, id: egui::Id, input: &DopesheetInput) -> CurveProperties {
        let mut states = lock(&self.states);
        let state = states.entry(id).or_default();
        state.notice(input.sequence);
        let rows = rows(input.sequence, &state.unfolded);
        let mut edited = Edited::none();
        egui::ScrollArea::vertical()
            .id_salt(id.with("curve-rows"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let width = ui.available_width();
                let (area, _) = ui.allocate_exact_size(egui::vec2(width, rows.len() as f32 * ROW), Sense::hover());
                edited = left_column(state, input, input.playhead, &rows, area, width, true, ui);
            });
        state.expect(&edited.keys, input.sequence);
        CurveProperties {
            keys: edited.keys,
            playhead: edited.playhead,
            hidden: state.hidden.clone(),
        }
    }

    fn forget(&self, id: egui::Id) {
        lock(&self.states).remove(&id);
    }
}

/// The ruler, with the frame at the playhead on its left; pressing on it moves the playhead, which
/// it returns.
fn draw_ruler(
    ui: &mut egui::Ui,
    id: egui::Id,
    sequence: &Sequence,
    playhead: Option<f64>,
    ruler: Rect,
    right: Rect,
    view: View,
) -> Option<f64> {
    let response = ui.interact(right, id.with("ruler"), Sense::click_and_drag());
    let mut moved = None;
    if playhead.is_some()
        && (response.is_pointer_button_down_on() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        moved = Some(view.frame(pointer.x).round().clamp(0.0, f64::from(sequence.length)));
    }
    let painter = ui.painter_at(ruler);
    let visuals = ui.visuals();
    painter.rect_filled(right, 0.0, visuals.faint_bg_color);
    let shown = moved.or(playhead);
    if let Some(frame) = shown {
        painter.text(
            ruler.left_center() + egui::vec2(6.0, 0.0),
            Align2::LEFT_CENTER,
            format!(
                "Frame {} ({})",
                frame.round(),
                time_label(frame.round() as i64, sequence.frame_rate)
            ),
            FontId::proportional(12.0),
            visuals.text_color(),
        );
    }
    let painter = ui.painter_at(right);
    let step = i64::from(tick_step(view.pixels_per_frame, sequence.frame_rate));
    let first = (view.frame(right.left()).floor() as i64).max(0);
    let last = view.frame(right.right()).ceil() as i64;
    let mut frame = first - first % step;
    for _ in 0..MAX_TICKS {
        if frame > last {
            break;
        }
        let x = view.x(frame as f64);
        painter.line_segment(
            [egui::pos2(x, right.bottom() - 6.0), egui::pos2(x, right.bottom())],
            Stroke::new(1.0, visuals.weak_text_color()),
        );
        painter.text(
            egui::pos2(x + 3.0, right.top() + 2.0),
            Align2::LEFT_TOP,
            time_label(frame, sequence.frame_rate),
            FontId::proportional(11.0),
            visuals.weak_text_color(),
        );
        frame += step;
    }
    if let Some(frame) = shown {
        let x = view.x(frame);
        let marker = Rect::from_center_size(egui::pos2(x, right.center().y), egui::vec2(28.0, RULER - 4.0));
        painter.rect_filled(marker, 3.0, PLAYHEAD);
        painter.text(
            marker.center(),
            Align2::CENTER_CENTER,
            format!("{}", frame.round()),
            FontId::proportional(11.0),
            Color32::WHITE,
        );
    }
    moved
}

/// Seconds and frames, as `1:15`.
fn time_label(frame: i64, frame_rate: u32) -> String {
    let rate = i64::from(frame_rate.max(1));
    format!("{}:{:02}", frame / rate, frame % rate)
}

/// Frames between two labels of the ruler, at least 50 pixels apart.
fn tick_step(pixels_per_frame: f32, frame_rate: u32) -> u32 {
    let mut steps: Vec<u32> = vec![1, 2, 5, 10];
    steps.extend([1, 2, 5, 10, 30, 60, 120, 300, 600, 1800, 3600].map(|seconds| seconds * frame_rate));
    steps.sort_unstable();
    steps
        .into_iter()
        .find(|step| *step as f32 * pixels_per_frame >= 50.0)
        .unwrap_or(3600 * frame_rate)
}

/// The left of every row; the first change one of them made.
#[allow(clippy::too_many_arguments)]
fn left_column(
    state: &mut State,
    input: &DopesheetInput,
    playhead: Option<f64>,
    rows: &[Row],
    area: Rect,
    width: f32,
    curves: bool,
    ui: &mut egui::Ui,
) -> Edited {
    let mut edited = Edited::none();
    for (index, row) in rows.iter().enumerate() {
        let rect = Rect::from_min_size(area.min + egui::vec2(0.0, index as f32 * ROW), egui::vec2(width, ROW));
        let row_edited = properties_row(state, input, playhead, row, rect, curves, ui);
        if edited.keys == KeysChange::None {
            edited = row_edited;
        }
    }
    edited
}

/// The left of a row: the name of the sequence or of a module, a property with the values of its
/// numbers at the playhead, or one of those numbers. A value set is a key at the playhead.
#[allow(clippy::too_many_arguments)]
fn properties_row(
    state: &mut State,
    input: &DopesheetInput,
    playhead: Option<f64>,
    row: &Row,
    rect: Rect,
    curves: bool,
    ui: &mut egui::Ui,
) -> Edited {
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(6.0, 1.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.set_clip_rect(rect);
    let ui = &mut child;
    let (index, only) = match row {
        Row::Summary => {
            ui.weak(if input.title.is_empty() {
                "All keys"
            } else {
                input.title
            });
            return Edited::none();
        }
        Row::Group(name) => {
            ui.strong(name);
            return Edited::none();
        }
        Row::Track(index) => (*index, None),
        Row::Number(index, number) => (*index, Some(*number)),
    };
    let track = &input.sequence.tracks[index];
    let property = input.properties.get(&track.property);
    let declared = property.is_some_and(|property| property.declared);
    let colour = if declared {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let label = property.map_or_else(
        || track.property.rsplit('/').next().unwrap_or(&track.property).to_owned(),
        |property| property.label.clone(),
    );
    let names = number_names(track.kind);
    let shown_name = match only {
        Some(number) => format!("{label}.{}", names[number]),
        None => label.clone(),
    };
    match only {
        None => {
            if track.curves.len() > 1 {
                let unfolded = state.unfolded.contains(&track.property);
                if icon_button(ui, if unfolded { Icon::Unfolded } else { Icon::Folded })
                    .on_hover_text("Show or hide a row for each number")
                    .clicked()
                    && !state.unfolded.remove(&track.property)
                {
                    state.unfolded.insert(track.property.clone());
                }
            } else {
                ui.add_space(ROW - 4.0 + ui.spacing().item_spacing.x);
            }
            if curves {
                shown_box(state, &track.property, 0..track.curves.len(), ui);
            }
            ui.add_sized(
                [66.0, ROW - 4.0],
                egui::Label::new(egui::RichText::new(&label).color(colour)).truncate(),
            )
            .on_hover_text(if declared {
                track.property.clone()
            } else {
                format!("{}: no running module declares it", track.property)
            });
        }
        Some(number) => {
            ui.add_space(ROW + 8.0);
            if curves {
                shown_box(state, &track.property, number..number + 1, ui);
            }
            let (swatch, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
            let [r, g, b] = number_colour(track.kind, number);
            ui.painter().rect_filled(swatch, 2.0, Color32::from_rgb(r, g, b));
            ui.add_sized(
                [52.0, ROW - 4.0],
                egui::Label::new(egui::RichText::new(names[number]).color(colour)).truncate(),
            );
        }
    }
    let mut edited = Edited::none();
    if let Some(at) = playhead {
        edited = values(state, input, track, property, only, &shown_name, at.round(), ui);
    }
    if only.is_none()
        && icon_button(ui, Icon::Remove)
            .on_hover_text("Remove this track")
            .clicked()
    {
        edited = Edited {
            keys: KeysChange::Finished {
                label: format!("remove {label}"),
                tracks: without_track(&input.sequence.tracks, &track.property),
            },
            playhead: None,
        };
    }
    edited
}

/// The fields of a row's numbers at the playhead, each setting a key at `frame`, and the button
/// adding a key there with the property's value.
#[allow(clippy::too_many_arguments)]
fn values(
    state: &mut State,
    input: &DopesheetInput,
    track: &Track,
    property: Option<&dopesheet::RowProperty>,
    only: Option<usize>,
    shown_name: &str,
    frame: f64,
    ui: &mut egui::Ui,
) -> Edited {
    let declared = property.is_some_and(|property| property.declared);
    let current = property.and_then(|property| property.current);
    let shown = property
        .and_then(|property| property.value)
        .or(current)
        .unwrap_or_else(|| PropertyValue::from_components(track.kind, &[]));
    let mut numbers = shown.components();
    let before = numbers.clone();
    // Only what the user did sets keys: a field that brings a value back within its range on its
    // own must not.
    let mut changed: Vec<usize> = Vec::new();
    let range = property.map_or([f64::MIN, f64::MAX], |property| property.range);
    let speed = ((range[1] - range[0]) / 2000.0).clamp(0.005, 1.0);
    let mut finished = false;
    let mut added = None;
    ui.add_enabled_ui(declared, |ui| {
        if track.kind == PropertyKind::Boolean {
            let mut on = numbers[0] >= 0.5;
            if ui.checkbox(&mut on, "").changed() {
                numbers[0] = if on { 1.0 } else { 0.0 };
                changed.push(0);
                finished = true;
            }
        } else {
            for (number, value) in numbers.iter_mut().enumerate() {
                if only.is_some_and(|only| only != number) {
                    continue;
                }
                // A value beyond the limit is shown, greyed: it cannot be a key. One typed beyond
                // is brought back to the limit.
                let within = sequence::admissible(*value);
                let response = ui
                    .add_enabled_ui(within, |ui| number_field(ui, value, range, speed))
                    .inner;
                let response = if within {
                    response
                } else {
                    response.on_disabled_hover_text(BEYOND)
                };
                if response.changed() {
                    changed.push(number);
                }
                finished |= ended(&response);
            }
            if track.kind == PropertyKind::Colour && only.is_none() {
                let (swatch, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                let channel = |n: f64| (n.clamp(0.0, 1.0) * 255.0) as u8;
                ui.painter().rect_filled(
                    swatch,
                    2.0,
                    Color32::from_rgb(channel(numbers[0]), channel(numbers[1]), channel(numbers[2])),
                );
            }
        }
        let admissible = current.as_ref().is_none_or(keyable);
        let button = ui.add_enabled_ui(admissible, |ui| icon_button(ui, Icon::Key)).inner;
        let button = if admissible {
            button.on_hover_text("Add a key at the playhead with the current value")
        } else {
            button.on_disabled_hover_text(BEYOND)
        };
        if button.clicked()
            && let Some(value) = current
        {
            let keys: Vec<(usize, f64)> = value
                .components()
                .into_iter()
                .enumerate()
                .filter(|(number, _)| only.is_none_or(|only| only == *number))
                .collect();
            added = Some(with_keys(&input.sequence.tracks, &track.property, frame, &keys));
        }
    });
    if let Some(tracks) = added {
        return Edited {
            keys: KeysChange::Finished {
                label: format!("add a key to {shown_name}"),
                tracks,
            },
            playhead: None,
        };
    }
    if !changed.is_empty() {
        let keys: Vec<(usize, f64)> = changed.into_iter().map(|number| (number, numbers[number])).collect();
        let tracks = with_values(&input.sequence.tracks, &track.property, frame, &keys, &before);
        let label = format!("set a key of {shown_name}");
        let keys = if finished {
            state.field = None;
            KeysChange::Finished { label, tracks }
        } else {
            state.field = Some(label);
            KeysChange::Changing(tracks)
        };
        return Edited {
            keys,
            playhead: Some(frame),
        };
    }
    // A value changed over several frames is done with: one undo entry.
    if finished && let Some(label) = state.field.take() {
        return Edited {
            keys: KeysChange::Finished {
                label,
                tracks: input.sequence.tracks.clone(),
            },
            playhead: None,
        };
    }
    Edited::none()
}

/// A box showing or hiding the curves of these numbers in a curve view.
fn shown_box(state: &mut State, property: &str, numbers: std::ops::Range<usize>, ui: &mut egui::Ui) {
    let hidden = &mut state.hidden;
    let mut shown = numbers.clone().any(|n| !hidden.contains(&(property.to_owned(), n)));
    if ui
        .checkbox(&mut shown, "")
        .on_hover_text("Show the curve in the Curves view")
        .changed()
    {
        for number in numbers {
            let key = (property.to_owned(), number);
            if shown {
                hidden.remove(&key);
            } else {
                hidden.insert(key);
            }
        }
    }
}

/// Whether a value being changed over several frames is done with.
fn ended(response: &egui::Response) -> bool {
    response.drag_stopped()
        || response.lost_focus()
        || (response.changed() && !response.dragged() && !response.has_focus())
}

/// A number typed in a field: none for the text it showed (see `number_field`), nor for what is no
/// finite number.
fn typed(text: &str, shown: &str) -> Option<f64> {
    let text = text.trim();
    if text == shown {
        return None;
    }
    text.parse().ok().filter(|number: &f64| number.is_finite())
}

/// A number as its field shows it: at most three decimals.
fn field_text(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0".to_owned() } else { text.to_owned() }
}

/// What a field lets through: its property's range, within the limit of the curves.
fn field_range(range: [f64; 2]) -> [f64; 2] {
    [range[0].max(-curve::LIMIT), range[1].min(curve::LIMIT)]
}

/// The field of a number at the playhead. Its text, typed or not, is kept once the field loses the
/// keyboard; the text it showed when it took it is no value typed, even rounded or out of range.
fn number_field(ui: &mut egui::Ui, value: &mut f64, range: [f64; 2], speed: f64) -> egui::Response {
    let shown = field_text(*value);
    let range = field_range(range);
    ui.add_sized(
        [62.0, ROW - 4.0],
        egui::DragValue::new(value)
            .speed(speed)
            .range(range[0]..=range[1])
            .clamp_existing_to_range(false)
            .custom_formatter(|number, _| field_text(number))
            .custom_parser(move |text| typed(text, &shown))
            .update_while_editing(false),
    )
}

enum Icon {
    /// A diamond: add a key.
    Key,
    /// A cross: remove.
    Remove,
    /// A triangle pointing right: rows to show.
    Folded,
    /// A triangle pointing down: rows shown.
    Unfolded,
}

/// A small button with a drawn icon, the fonts having no such characters.
fn icon_button(ui: &mut egui::Ui, icon: Icon) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ROW - 4.0, ROW - 4.0), Sense::click());
    let style = ui.style().interact(&response);
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, style.weak_bg_fill);
    let (centre, size) = (rect.center(), 4.0);
    match icon {
        Icon::Key => {
            let points = vec![
                centre + Vec2::new(0.0, -size),
                centre + Vec2::new(size, 0.0),
                centre + Vec2::new(0.0, size),
                centre + Vec2::new(-size, 0.0),
            ];
            painter.add(egui::Shape::convex_polygon(points, style.fg_stroke.color, Stroke::NONE));
        }
        Icon::Folded | Icon::Unfolded => {
            let points = if matches!(icon, Icon::Folded) {
                vec![
                    centre + Vec2::new(-size * 0.6, -size),
                    centre + Vec2::new(size, 0.0),
                    centre + Vec2::new(-size * 0.6, size),
                ]
            } else {
                vec![
                    centre + Vec2::new(-size, -size * 0.6),
                    centre + Vec2::new(size, -size * 0.6),
                    centre + Vec2::new(0.0, size),
                ]
            };
            painter.add(egui::Shape::convex_polygon(points, style.fg_stroke.color, Stroke::NONE));
        }
        Icon::Remove => {
            let stroke = Stroke::new(1.5, style.fg_stroke.color);
            painter.line_segment([centre + Vec2::splat(-size), centre + Vec2::splat(size)], stroke);
            painter.line_segment(
                [centre + Vec2::new(-size, size), centre + Vec2::new(size, -size)],
                stroke,
            );
        }
    }
    response
}

/// The wheel zooms the time around the pointer, the middle button scrolls it.
fn navigate(ui: &mut egui::Ui, area: Rect, view: View, time: &mut TimeAxis) {
    if !ui.rect_contains_pointer(area) {
        return;
    }
    let (scroll, middle, delta, pointer) = ui.input_mut(|i| {
        let scroll = std::mem::take(&mut i.smooth_scroll_delta);
        (
            scroll,
            i.pointer.middle_down(),
            i.pointer.delta(),
            i.pointer.hover_pos(),
        )
    });
    if scroll.y != 0.0
        && let Some(pointer) = pointer
    {
        let anchored = view.frame(pointer.x);
        time.pixels_per_unit = (time.pixels_per_unit * (scroll.y * 0.004).exp()).clamp(ZOOM[0], ZOOM[1]);
        time.first = anchored - f64::from((pointer.x - view.left) / time.pixels_per_unit);
    }
    if middle && delta.x != 0.0 {
        time.first -= f64::from(delta.x / time.pixels_per_unit);
    }
}

/// The keys as diamonds, their selection, moves and box, and the playhead across the rows. Keys
/// dragged are given moved at each frame, from where the drag began.
#[allow(clippy::too_many_arguments)]
fn keys_area(
    state: &mut State,
    input: &DopesheetInput,
    rows: &[Row],
    area: Rect,
    view: View,
    playhead: Option<f64>,
    ui: &mut egui::Ui,
    id: egui::Id,
) -> KeysChange {
    let sequence = input.sequence;
    let response = ui.interact(area, id.with("keys"), Sense::click_and_drag());
    let command = ui.input(|i| i.modifiers.command);
    let centre_y = |index: usize| area.top() + (index as f32 + 0.5) * ROW;
    let diamonds: Vec<(usize, Pos2, Vec<KeyId>)> = rows
        .iter()
        .enumerate()
        .flat_map(|(index, row)| {
            row_keys(sequence, row)
                .into_iter()
                .map(move |(frame, keys)| (index, egui::pos2(view.x(f64::from(frame)), centre_y(index)), keys))
        })
        .collect();
    let hit = |at: Pos2| -> Option<Vec<KeyId>> {
        diamonds
            .iter()
            .filter(|(_, centre, _)| (centre.x - at.x).abs() <= DIAMOND + 3.0 && (centre.y - at.y).abs() <= ROW / 2.0)
            .min_by(|a, b| (a.1.x - at.x).abs().total_cmp(&(b.1.x - at.x).abs()))
            .map(|(_, _, keys)| keys.clone())
    };
    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        state.gesture = match hit(origin) {
            Some(keys) => {
                if !keys.iter().all(|key| state.selection.contains(key)) {
                    if !command {
                        state.selection.clear();
                    }
                    state.selection.extend(keys);
                }
                Gesture::Move {
                    from: origin.x,
                    to: origin.x,
                    start: sequence.clone(),
                }
            }
            None => {
                if !command {
                    state.selection.clear();
                }
                Gesture::Select {
                    from: origin,
                    to: origin,
                }
            }
        };
    }
    if response.clicked()
        && let Some(at) = response.interact_pointer_pos()
    {
        let selection = &mut state.selection;
        match hit(at) {
            Some(keys) if command && keys.iter().all(|key| selection.contains(key)) => {
                for key in &keys {
                    selection.remove(key);
                }
            }
            Some(keys) if command => selection.extend(keys),
            Some(keys) => *selection = keys.into_iter().collect(),
            None if !command => selection.clear(),
            None => {}
        }
    }
    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        match &mut state.gesture {
            Gesture::Move { to, .. } => *to = pointer.x,
            Gesture::Select { to, .. } => *to = pointer,
            Gesture::None => {}
        }
    }
    let selecting = match state.gesture {
        Gesture::Select { from, to } => Some(Rect::from_two_pos(from, to)),
        _ => None,
    };
    // During a drag, the keys as they began, moved.
    let (shown, selected, offset) = match &state.gesture {
        Gesture::Move { from, to, start } => {
            let offset = ((to - from) / view.pixels_per_frame).round() as i64;
            let mut moved = start.clone();
            let selected = moved.move_keys(&state.selection, offset);
            (moved, selected, offset)
        }
        _ => (sequence.clone(), state.selection.clone(), 0),
    };
    let mut change = KeysChange::None;
    if offset != state.shown_offset {
        change = KeysChange::Changing(shown.tracks.clone());
        state.shown_offset = offset;
    }
    if response.drag_stopped() {
        if offset != 0 {
            change = KeysChange::Finished {
                label: "move keys".to_owned(),
                tracks: shown.tracks.clone(),
            };
            state.selection = selected.clone();
        }
        if let Some(rect) = selecting {
            for (index, centre, keys) in &diamonds {
                if matches!(rows[*index], Row::Track(_) | Row::Number(..)) && rect.contains(*centre) {
                    state.selection.extend(keys.iter().cloned());
                }
            }
        }
        state.gesture = Gesture::None;
        state.shown_offset = 0;
    }

    let painter = ui.painter_at(area);
    let visuals = ui.visuals();
    for (index, row) in rows.iter().enumerate() {
        let band = Rect::from_min_size(
            egui::pos2(area.left(), area.top() + index as f32 * ROW),
            egui::vec2(area.width(), ROW),
        );
        if matches!(row, Row::Summary | Row::Group(_)) {
            painter.rect_filled(band, 0.0, visuals.faint_bg_color);
        }
        painter.line_segment(
            [band.left_bottom(), band.right_bottom()],
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.5)),
        );
    }
    let outside = Color32::from_black_alpha(28);
    let start = view.x(0.0);
    let end = view.x(f64::from(sequence.length));
    if start > area.left() {
        painter.rect_filled(Rect::from_x_y_ranges(area.left()..=start, area.y_range()), 0.0, outside);
    }
    if end < area.right() {
        painter.rect_filled(Rect::from_x_y_ranges(end..=area.right(), area.y_range()), 0.0, outside);
    }
    for (index, row) in rows.iter().enumerate() {
        let missing = matches!(row, Row::Track(i) | Row::Number(i, _)
            if !input.properties.get(&sequence.tracks[*i].property).is_some_and(|p| p.declared));
        for (frame, keys) in row_keys(&shown, row) {
            let centre = egui::pos2(view.x(f64::from(frame)), centre_y(index));
            let chosen = keys.iter().all(|key| selected.contains(key));
            let fill = if chosen {
                visuals.selection.bg_fill
            } else if missing {
                visuals.weak_text_color()
            } else {
                visuals.text_color()
            };
            diamond(&painter, centre, fill, visuals.extreme_bg_color);
        }
    }
    if let Some(rect) = selecting {
        painter.rect(
            rect,
            0.0,
            visuals.selection.bg_fill.gamma_multiply(0.15),
            Stroke::new(1.0, visuals.selection.bg_fill),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(frame) = playhead {
        let x = view.x(frame);
        painter.line_segment(
            [egui::pos2(x, area.top()), egui::pos2(x, area.bottom())],
            Stroke::new(1.5, PLAYHEAD),
        );
    }
    change
}

fn diamond(painter: &egui::Painter, centre: Pos2, fill: Color32, outline: Color32) {
    let points = vec![
        centre + Vec2::new(0.0, -DIAMOND),
        centre + Vec2::new(DIAMOND, 0.0),
        centre + Vec2::new(0.0, DIAMOND),
        centre + Vec2::new(-DIAMOND, 0.0),
    ];
    painter.add(egui::Shape::convex_polygon(points, fill, Stroke::new(1.0, outline)));
}

/// Its hotkey, Delete by default, removes the selected keys, while the pointer is over the keys and
/// no field has the keyboard, but not under a gesture.
fn delete(state: &mut State, sequence: &Sequence, area: Rect, hotkey: &Hotkey, ui: &mut egui::Ui) -> KeysChange {
    let typing = ui.ctx().memory(|memory| memory.focused().is_some());
    let idle = matches!(state.gesture, Gesture::None);
    if typing || !idle || state.selection.is_empty() || !ui.rect_contains_pointer(area) || !hotkey.pressed(ui.ctx()) {
        return KeysChange::None;
    }
    let mut after = sequence.clone();
    after.remove_keys(&state.selection);
    state.selection.clear();
    KeysChange::Finished {
        label: "delete keys".to_owned(),
        tracks: after.tracks,
    }
}

#[derive(Default)]
struct DopesheetModule;

impl Module for DopesheetModule {
    fn register(&mut self, reg: &mut Registrar) {
        let sheet: Arc<dyn Dopesheet> = Arc::new(Sheet {
            states: Mutex::default(),
            delete: reg.hotkey("delete", "Delete the selected keys", HotkeyKind::Press, DELETE),
        });
        reg.provide(dopesheet::SERVICE, sheet);
    }
}

uniwow_api::export_module!(DopesheetModule);

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use uniwow_api::curve::TimeAxis;
    use uniwow_api::dopesheet::{Dopesheet, DopesheetInput, DopesheetOutput, KeysChange};
    use uniwow_api::egui;
    use uniwow_api::sequence::{Sequence, Track};
    use uniwow_api::{PropertyKind, PropertyValue};

    use super::{
        Gesture, Row, Sheet, State, field_range, field_text, keyable, lock, number_field, row_keys, rows, tick_step,
        typed, with_keys, with_values, without_track,
    };

    /// `cube/position` with keys at frames 10 and 20 on x, and `cube/opacity` with one at 10.
    fn sequence() -> Sequence {
        let mut position = Track::new("cube/position", PropertyKind::Vector);
        position.curves[0].set_key(10.0, 1.0);
        position.curves[0].set_key(20.0, 2.0);
        let mut opacity = Track::new("cube/opacity", PropertyKind::Number);
        opacity.set_key(10, PropertyValue::Number(0.5));
        Sequence {
            tracks: vec![position, opacity],
            ..Sequence::default()
        }
    }

    /// Frames of a dopesheet of 800 by 600 points, 10 points a frame from frame 0, starting with
    /// `state`, each with its events; returns what the last one gave and the state then.
    fn frames(state: State, sequence: &Sequence, frames: Vec<Vec<egui::Event>>) -> (DopesheetOutput, State) {
        let sheet = Sheet::default();
        let id = egui::Id::new("dopesheet");
        lock(&sheet.states).insert(id, state);
        let ctx = egui::Context::default();
        let properties = HashMap::new();
        let mut output = None;
        for events in frames {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
                events,
                ..egui::RawInput::default()
            };
            let mut rendered = ctx.run_ui(input, |ui| {
                let mut time = TimeAxis {
                    first: 0.0,
                    pixels_per_unit: 10.0,
                };
                let input = DopesheetInput {
                    sequence,
                    properties: &properties,
                    playhead: Some(0.0),
                    title: "",
                };
                output = Some(sheet.show(ui, id, &input, &mut time));
            });
            rendered.textures_delta.clear();
        }
        let state = lock(&sheet.states).remove(&id).expect("kept");
        (output.expect("shown"), state)
    }

    fn frame(state: State, sequence: &Sequence, events: Vec<egui::Event>) -> (DopesheetOutput, State) {
        frames(state, sequence, vec![events])
    }

    #[test]
    fn each_module_has_a_row_and_each_track_unfolds_into_its_numbers() {
        let sequence = sequence();
        let folded = rows(&sequence, &BTreeSet::new());
        assert_eq!(folded.len(), 4, "every key, the cube, its two tracks");
        let unfolded = rows(&sequence, &BTreeSet::from(["cube/position".to_owned()]));
        assert_eq!(unfolded.len(), 7, "the three numbers of the position");
        assert_eq!(row_keys(&sequence, &Row::Summary)[&10].len(), 2);
        assert_eq!(row_keys(&sequence, &Row::Number(0, 1)).len(), 0);
    }

    #[test]
    fn keys_dragged_are_given_moved_from_where_the_drag_began_until_the_tracks_change_elsewhere() {
        let sequence = sequence();
        let selected = ("cube/position".to_owned(), 0, 10);
        let dragging = |seen: &Sequence, to: f32| State {
            selection: BTreeSet::from([selected.clone()]),
            gesture: Gesture::Move {
                from: 400.0,
                to,
                start: sequence.clone(),
            },
            seen: seen.tracks.clone(),
            ..State::default()
        };
        let (output, state) = frame(dragging(&sequence, 450.0), &sequence, Vec::new());
        let KeysChange::Changing(moved) = output.keys else {
            panic!("{:?}", output.keys);
        };
        assert_eq!(moved[0].curves[0].keys[0].time, 15.0, "moved by 5 frames");
        assert!(matches!(state.gesture, Gesture::Move { .. }));
        // Shown moved, as the caller does: the drag goes on from where it began.
        let shown = Sequence {
            tracks: moved,
            ..sequence.clone()
        };
        let (output, state) = frame(dragging(&shown, 470.0), &shown, Vec::new());
        assert!(matches!(state.gesture, Gesture::Move { .. }));
        assert!(
            state.selection.contains(&selected),
            "the key selected where the drag began"
        );
        let KeysChange::Changing(further) = output.keys else {
            panic!("{:?}", output.keys);
        };
        assert_eq!(further[0].curves[0].keys[0].time, 17.0, "7 frames from where it began");
        // A module removes the other key of the curve while the user drags.
        let mut changed = shown.clone();
        changed.tracks[0].curves[0].keys.remove(1);
        let (output, state) = frame(dragging(&shown, 470.0), &changed, Vec::new());
        assert_eq!(output.keys, KeysChange::None, "no key changes");
        assert!(matches!(state.gesture, Gesture::None), "the drag ends");
    }

    #[test]
    fn delete_removes_the_selected_keys_as_one_change() {
        let sequence = sequence();
        let state = State {
            selection: BTreeSet::from([("cube/opacity".to_owned(), 0, 10)]),
            seen: sequence.tracks.clone(),
            ..State::default()
        };
        let delete = egui::Event::Key {
            key: egui::Key::Delete,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let events = vec![egui::Event::PointerMoved(egui::pos2(600.0, 200.0)), delete];
        let (output, state) = frame(state, &sequence, events);
        let KeysChange::Finished { label, tracks } = output.keys else {
            panic!("{:?}", output.keys);
        };
        assert_eq!(label, "delete keys");
        assert!(tracks[1].curves[0].keys.is_empty());
        assert_eq!(tracks[0], sequence.tracks[0], "the other track stays");
        assert!(state.selection.is_empty());
    }

    #[test]
    fn pressing_on_the_ruler_moves_the_playhead_to_a_whole_frame() {
        let sequence = sequence();
        let press = egui::Event::PointerButton {
            pos: egui::pos2(504.0, 11.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        // The first frame lays the ruler out, where the second presses.
        let hover = vec![egui::Event::PointerMoved(egui::pos2(504.0, 11.0))];
        let (output, _) = frames(State::default(), &sequence, vec![hover, vec![press]]);
        assert_eq!(output.playhead, Some(10.0));
    }

    #[test]
    fn a_value_sets_a_key_on_its_number_and_a_track_removed_takes_its_keys() {
        let sequence = sequence();
        let tracks = with_keys(&sequence.tracks, "cube/position", 30.0, &[(1, 4.0)]);
        assert_eq!(tracks[0].curves[1].keys.len(), 1);
        assert_eq!(
            tracks[0].curves[0], sequence.tracks[0].curves[0],
            "the other numbers stay"
        );
        assert_eq!(
            with_keys(&sequence.tracks, "cube/size", 0.0, &[(0, 1.0)]),
            sequence.tracks
        );
        let left = without_track(&sequence.tracks, "cube/position");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].property, "cube/opacity");
    }

    #[test]
    fn a_number_set_for_the_first_time_away_from_the_start_keeps_its_value_there() {
        let mut colour = Track::new("cube/colour", PropertyKind::Colour);
        colour.curves[1].set_key(10.0, 0.5);
        let tracks = vec![colour];
        let before = [1.0, 0.5, 1.0];
        // Red has no key: set at frame 30, it keeps 1 at frame 0.
        let set = with_values(&tracks, "cube/colour", 30.0, &[(0, 0.2)], &before);
        let red: Vec<(f64, f64)> = set[0].curves[0].keys.iter().map(|key| (key.time, key.value)).collect();
        assert_eq!(red, vec![(0.0, 1.0), (30.0, 0.2)]);
        // Green has a key: only the one set.
        let set = with_values(&tracks, "cube/colour", 30.0, &[(1, 0.8)], &before);
        assert_eq!(set[0].curves[1].keys.len(), 2);
        assert!(set[0].curves[1].keys.iter().all(|key| key.time != 0.0));
        // At frame 0, the value set is the key there.
        let set = with_values(&tracks, "cube/colour", 0.0, &[(2, 0.4)], &before);
        let blue: Vec<(f64, f64)> = set[0].curves[2].keys.iter().map(|key| (key.time, key.value)).collect();
        assert_eq!(blue, vec![(0.0, 0.4)]);
    }

    #[test]
    fn a_value_beyond_the_limit_of_the_curves_is_no_key() {
        assert!(keyable(&PropertyValue::Vector([1.0, -1e9, 1e9])));
        assert!(!keyable(&PropertyValue::Vector([1.0, 2e9, 0.0])));
        assert!(!keyable(&PropertyValue::Number(f64::INFINITY)));
    }

    #[test]
    fn a_dopesheet_forgotten_keeps_nothing() {
        let sheet = Sheet::default();
        let id = egui::Id::new("gone");
        lock(&sheet.states).insert(id, State::default());
        sheet.forget(id);
        assert!(lock(&sheet.states).is_empty());
    }

    #[test]
    fn the_ruler_keeps_its_labels_apart() {
        assert_eq!(tick_step(10.0, 30), 5);
        assert_eq!(tick_step(0.2, 30), 300);
    }

    /// A frame drawing a field of `value`, with `events`; returns whether it changed and its place.
    fn field(ctx: &egui::Context, value: &mut f64, events: Vec<egui::Event>) -> (bool, egui::Rect) {
        let mut result = (false, egui::Rect::NOTHING);
        let input = egui::RawInput {
            events,
            ..egui::RawInput::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let response = number_field(ui, value, [0.0, 1.0], 0.005);
            result = (response.changed(), response.rect);
        });
        output.textures_delta.clear();
        result
    }

    fn click(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn a_field_taken_then_left_without_typing_changes_nothing() {
        let ctx = egui::Context::default();
        let mut value = 1.0487;
        let (_, rect) = field(&ctx, &mut value, Vec::new());
        let at = rect.center();
        field(&ctx, &mut value, vec![egui::Event::PointerMoved(at), click(at, true)]);
        field(&ctx, &mut value, vec![click(at, false)]);
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let (changed, _) = field(&ctx, &mut value, vec![enter]);
        field(&ctx, &mut value, Vec::new());
        assert!(!changed);
        assert_eq!(value, 1.0487, "neither rounded nor brought back within the range");
    }

    #[test]
    fn a_field_lets_through_only_finite_numbers_within_the_limit_of_the_curves() {
        assert_eq!(typed("nan", "1"), None);
        assert_eq!(typed("inf", "1"), None);
        assert_eq!(typed(" 2.5 ", "1"), Some(2.5));
        assert_eq!(field_range([f64::MIN, 1e12]), [-1e9, 1e9]);
        assert_eq!(field_range([0.0, 1.0]), [0.0, 1.0]);
    }

    #[test]
    fn numbers_are_shown_with_three_decimals_at_most() {
        assert_eq!(field_text(1.0487), "1.049");
        assert_eq!(field_text(0.5), "0.5");
        assert_eq!(field_text(10.0), "10");
        assert_eq!(field_text(-0.0001), "0");
    }
}
