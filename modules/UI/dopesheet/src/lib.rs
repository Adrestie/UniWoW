//! The dopesheet, as the Dopesheet of Unity: the rows of a sequence, one per track unfolding into
//! one per number, with their keys as diamonds, a ruler and a playhead. Keys are selected, moved
//! and deleted, the playhead moved; on the left, each track's label and value at the playhead.
//! Offered to Rust modules as a service; the core also has it draw the `DopesheetView` objects of
//! every language.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::curve::TimeAxis;
use uniwow_api::dopesheet::{self, Dopesheet, DopesheetInput, DopesheetOutput, KeysChange};
use uniwow_api::egui::{self, Align, Align2, Color32, FontId, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use uniwow_api::sequence::{KeyId, Sequence, Track, frame_of, number_colour, number_names};
use uniwow_api::{Module, Registrar};

/// Height of a row.
const ROW: f32 = 22.0;
const RULER: f32 = 22.0;
/// The widest the properties on the left go, and their share of a narrow dopesheet.
const LEFT: f32 = 300.0;
const LEFT_SHARE: f32 = 0.4;
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
    /// The selected keys dragged, from and to these x.
    Move { from: f32, to: f32 },
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
    /// The tracks as this dopesheet left them, to notice when they change elsewhere: by an undo,
    /// by a module.
    seen: Vec<Track>,
    /// The frames the keys being dragged were last shown moved by.
    shown_offset: i64,
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

#[derive(Default)]
struct Sheet {
    states: Mutex<HashMap<egui::Id, State>>,
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

impl Dopesheet for Sheet {
    fn show(&self, ui: &mut egui::Ui, id: egui::Id, input: &DopesheetInput, time: &mut TimeAxis) -> DopesheetOutput {
        let mut states = lock(&self.states);
        let state = states.entry(id).or_default();
        let sequence = input.sequence;
        // Tracks changed elsewhere: a gesture would act on keys that are no more.
        if sequence.tracks != state.seen {
            state.gesture = Gesture::None;
            state.shown_offset = 0;
        }
        state.selection.retain(|key| sequence.has_key(key));
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
                for (index, row) in rows.iter().enumerate() {
                    let rect = Rect::from_min_size(
                        area.min + egui::vec2(0.0, index as f32 * ROW),
                        egui::vec2(left_width, ROW),
                    );
                    properties_row(state, input, playhead, row, rect, ui);
                }
                let keys = Rect::from_min_max(egui::pos2(left, area.top()), area.max);
                navigate(ui, keys.union(ruler_right), view, time);
                output.keys = keys_area(state, input, &rows, keys, view, playhead, ui, id);
                if output.keys == KeysChange::None {
                    output.keys = delete(state, sequence, keys, ui);
                }
            });
        state.seen = match &output.keys {
            KeysChange::Finished { tracks, .. } => tracks.clone(),
            _ => sequence.tracks.clone(),
        };
        output
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

/// A number as the left of a row shows it: at most three decimals.
fn number_text(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0".to_owned() } else { text.to_owned() }
}

/// The left of a row: the name of the sequence or of a module, a property with the values of its
/// numbers at the playhead, or one of those numbers.
fn properties_row(
    state: &mut State,
    input: &DopesheetInput,
    playhead: Option<f64>,
    row: &Row,
    rect: Rect,
    ui: &mut egui::Ui,
) {
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
            return;
        }
        Row::Group(name) => {
            ui.strong(name);
            return;
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
    match only {
        None => {
            if track.curves.len() > 1 {
                let unfolded = state.unfolded.contains(&track.property);
                if fold_button(ui, unfolded)
                    .on_hover_text("Show or hide a row for each number")
                    .clicked()
                    && !state.unfolded.remove(&track.property)
                {
                    state.unfolded.insert(track.property.clone());
                }
            } else {
                ui.add_space(ROW - 4.0 + ui.spacing().item_spacing.x);
            }
            ui.add_sized(
                [96.0, ROW - 4.0],
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
            let (swatch, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
            let [r, g, b] = number_colour(track.kind, number);
            ui.painter().rect_filled(swatch, 2.0, Color32::from_rgb(r, g, b));
            ui.add_sized(
                [52.0, ROW - 4.0],
                egui::Label::new(egui::RichText::new(names[number]).color(colour)).truncate(),
            );
        }
    }
    if playhead.is_none() {
        return;
    }
    let Some(value) = property.and_then(|property| property.value) else {
        return;
    };
    for (number, value) in value.components().into_iter().enumerate() {
        if only.is_none_or(|only| only == number) {
            ui.add_sized(
                [52.0, ROW - 4.0],
                egui::Label::new(egui::RichText::new(number_text(value)).color(colour)).truncate(),
            );
        }
    }
}

/// A small button with a triangle drawn, the fonts having no such character.
fn fold_button(ui: &mut egui::Ui, unfolded: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ROW - 4.0, ROW - 4.0), Sense::click());
    let style = ui.style().interact(&response);
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, style.weak_bg_fill);
    let (centre, size) = (rect.center(), 4.0);
    let points = if unfolded {
        vec![
            centre + Vec2::new(-size, -size * 0.6),
            centre + Vec2::new(size, -size * 0.6),
            centre + Vec2::new(0.0, size),
        ]
    } else {
        vec![
            centre + Vec2::new(-size * 0.6, -size),
            centre + Vec2::new(size, 0.0),
            centre + Vec2::new(-size * 0.6, size),
        ]
    };
    painter.add(egui::Shape::convex_polygon(points, style.fg_stroke.color, Stroke::NONE));
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

/// The keys as diamonds, their selection, moves and box, and the playhead across the rows.
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
    let offset = match state.gesture {
        Gesture::Move { from, to } => ((to - from) / view.pixels_per_frame).round() as i64,
        _ => 0,
    };
    let selecting = match state.gesture {
        Gesture::Select { from, to } => Some(Rect::from_two_pos(from, to)),
        _ => None,
    };
    // Drawn as they will be once a move ends.
    let (shown, selected) = if offset == 0 {
        (sequence.clone(), state.selection.clone())
    } else {
        let mut preview = sequence.clone();
        let moved = preview.move_keys(&state.selection, offset);
        (preview, moved)
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

/// Delete removes the selected keys, while the pointer is over the keys and no field has the
/// keyboard, but not under a gesture.
fn delete(state: &mut State, sequence: &Sequence, area: Rect, ui: &mut egui::Ui) -> KeysChange {
    let typing = ui.ctx().memory(|memory| memory.focused().is_some());
    let idle = matches!(state.gesture, Gesture::None);
    if typing
        || !idle
        || state.selection.is_empty()
        || !ui.rect_contains_pointer(area)
        || !ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
    {
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
        let sheet: Arc<dyn Dopesheet> = Arc::new(Sheet::default());
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

    use super::{Gesture, Row, Sheet, State, lock, row_keys, rows, tick_step};

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
    fn a_drag_goes_on_while_the_tracks_stay_and_ends_when_they_change_elsewhere() {
        let sequence = sequence();
        let dragging = || State {
            selection: BTreeSet::from([("cube/position".to_owned(), 0, 10)]),
            gesture: Gesture::Move { from: 400.0, to: 450.0 },
            seen: sequence.tracks.clone(),
            ..State::default()
        };
        let (output, state) = frame(dragging(), &sequence, Vec::new());
        assert!(
            matches!(output.keys, KeysChange::Changing(_)),
            "shown moved by 5 frames"
        );
        assert!(matches!(state.gesture, Gesture::Move { .. }));
        // A module removes the other key of the curve while the user drags.
        let mut changed = sequence.clone();
        changed.tracks[0].curves[0].keys.remove(1);
        let (output, state) = frame(dragging(), &changed, Vec::new());
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
        let events = vec![egui::Event::PointerMoved(egui::pos2(500.0, 200.0)), delete];
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
            pos: egui::pos2(404.0, 11.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        // The first frame lays the ruler out, where the second presses.
        let hover = vec![egui::Event::PointerMoved(egui::pos2(404.0, 11.0))];
        let (output, _) = frames(State::default(), &sequence, vec![hover, vec![press]]);
        assert_eq!(output.playhead, Some(10.0));
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
}
