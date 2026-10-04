//! The panel of the timeline: the sequence and playback bars, the animated properties on the
//! left, and the dopesheet on the right, with its ruler and playhead.

use std::collections::{BTreeMap, BTreeSet};

use uniwow_api::curve::{self, CurveChange, CurveOptions, ShownCurve, TimeAxis};
use uniwow_api::egui::{self, Align, Align2, Color32, FontId, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use uniwow_api::serde_json::json;
use uniwow_api::{Context, DIALOG_COMMAND, PropertyInfo, PropertyKind, PropertyValue};

use crate::sequence::{KeyId, MAX_FRAME_RATE, MAX_LENGTH, Sequence, Track, frame_of};
use crate::{Question, TimelineModule};

/// Height of a row of the dopesheet.
const ROW: f32 = 22.0;
const RULER: f32 = 22.0;
/// Width of the properties, left of the dopesheet.
const LEFT: f32 = 430.0;
/// Half the width of a key's diamond.
const DIAMOND: f32 = 5.0;
const ZOOM: [f32; 2] = [0.2, 200.0];
const PLAYHEAD: Color32 = Color32::from_rgb(225, 65, 55);

/// What the panel keeps between frames.
#[derive(Default)]
pub struct State {
    /// The frame at the left edge of the dopesheet.
    first_frame: f64,
    pixels_per_frame: f32,
    /// Set once the dopesheet fits the sequence shown.
    pub fitted: bool,
    gesture: Gesture,
    /// The sequence and its unsaved mark before a change made over several frames, such as a
    /// value dragged, recorded as one undo entry when it ends.
    editing: Option<(String, Sequence, bool)>,
    new_name: String,
    message: Option<String>,
    /// The properties whose numbers have rows of their own.
    unfolded: BTreeSet<String>,
    curves: bool,
    /// The numbers whose curves the Curves view does not show.
    hidden: BTreeSet<(String, usize)>,
}

#[derive(Default)]
enum Gesture {
    #[default]
    None,
    /// The selected keys dragged, from and to these x.
    Move { from: f32, to: f32 },
    /// A selection box.
    Select { from: Pos2, to: Pos2 },
}

/// A row of the dopesheet, with the keys it shows.
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

pub fn show(timeline: &mut TimelineModule, ui: &mut egui::Ui, ctx: &mut Context) {
    let declared: BTreeMap<String, PropertyInfo> = timeline
        .editor
        .as_ref()
        .map(|editor| editor.properties())
        .unwrap_or_default()
        .into_iter()
        .map(|info| (info.path.clone(), info))
        .collect();
    sequence_bar(timeline, ui, ctx);
    if let Some(message) = &timeline.panel.message {
        ui.colored_label(ui.visuals().warn_fg_color, message);
    }
    let Some(sequence) = timeline.sequence().cloned() else {
        ui.weak("No sequence shown: choose one or create one.");
        return;
    };
    timeline.selection.retain(|key| sequence.has_key(key));
    playback_bar(timeline, &sequence, &declared, ui, ctx);
    ui.separator();
    dopesheet(timeline, &sequence, &declared, ui, ctx);
    keyboard(timeline, &sequence, ui, ctx);
}

/// Chooses, creates and saves sequences.
fn sequence_bar(timeline: &mut TimelineModule, ui: &mut egui::Ui, ctx: &mut Context) {
    let dirty = timeline
        .current
        .as_ref()
        .and_then(|name| timeline.documents.get(name))
        .is_some_and(|document| document.dirty);
    ui.horizontal(|ui| {
        ui.label("Sequence");
        let shown = match &timeline.current {
            Some(name) if dirty => format!("{name} *"),
            Some(name) => name.clone(),
            None => "none".to_owned(),
        };
        let mut chosen = None;
        egui::ComboBox::from_id_salt("timeline-sequence")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for name in &timeline.names {
                    if ui
                        .selectable_label(timeline.current.as_ref() == Some(name), name)
                        .clicked()
                    {
                        chosen = Some(name.clone());
                    }
                }
            });
        if let Some(name) = chosen.filter(|name| timeline.current.as_ref() != Some(name)) {
            switch(timeline, ctx, name, dirty);
        }
        let save = ui.add_enabled(dirty, egui::Button::new("Save"));
        if save.clicked()
            && let Some(name) = timeline.current.clone()
        {
            timeline.panel.message = timeline.save(&name).err();
        }
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut timeline.panel.new_name)
                .hint_text("name")
                .desired_width(120.0),
        );
        if ui.button("New").clicked() {
            let name = timeline.panel.new_name.trim().to_owned();
            match timeline.create(&name) {
                Ok(()) => switch(timeline, ctx, name, dirty),
                Err(error) => timeline.panel.message = Some(error),
            }
            timeline.panel.new_name.clear();
        }
    });
}

/// Shows another sequence. The unsaved changes of the one shown are asked about first, in a
/// window of the module `dialogs`, which answers later; without that module, they are lost.
fn switch(timeline: &mut TimelineModule, ctx: &mut Context, target: String, dirty: bool) {
    if !dirty {
        open(timeline, &target);
        return;
    }
    if timeline.question.is_some() {
        return;
    }
    let current = timeline.current.clone().unwrap_or_default();
    let asked = timeline
        .editor
        .as_ref()
        .is_some_and(|editor| editor.commands().iter().any(|c| c.name == DIALOG_COMMAND));
    if !asked {
        timeline.documents.remove(&current);
        open(timeline, &target);
        return;
    }
    let call = ctx.call(
        DIALOG_COMMAND,
        json!({
            "title": "Unsaved changes",
            "text": format!("The sequence '{current}' has unsaved changes. Save them before showing '{target}'?"),
            "buttons": [
                { "id": "save", "label": "Save" },
                { "id": "discard", "label": "Don't save" },
                { "id": "cancel", "label": "Cancel" },
            ],
            "escape": "cancel",
        }),
    );
    timeline.question = Some(Question {
        call,
        dialog: None,
        target,
    });
}

impl State {
    /// A message shown under the sequence bar.
    pub fn say(&mut self, message: &str) {
        self.message = Some(message.to_owned());
    }
}

/// Shows the sequence `name`, or says why it cannot.
pub fn open(timeline: &mut TimelineModule, name: &str) {
    timeline.panel.message = timeline.open(name).err();
    timeline.panel.editing = None;
}

/// Playback, the frame, the frame rate and the length, and the properties to add.
fn playback_bar(
    timeline: &mut TimelineModule,
    sequence: &Sequence,
    declared: &BTreeMap<String, PropertyInfo>,
    ui: &mut egui::Ui,
    ctx: &mut Context,
) {
    let frames = sequence.key_frames();
    let at = timeline.playhead.round() as u32;
    ui.horizontal(|ui| {
        if ui.button("⏮").on_hover_text("First frame").clicked() {
            timeline.playing = None;
            timeline.playhead = 0.0;
        }
        let previous = frames.range(..at).next_back().copied();
        if ui
            .add_enabled(previous.is_some(), egui::Button::new("◀"))
            .on_hover_text("Previous key")
            .clicked()
        {
            timeline.playing = None;
            timeline.playhead = f64::from(previous.unwrap_or(0));
        }
        let playing = timeline.playing.is_some();
        if ui
            .button(if playing { "⏸" } else { "▶" })
            .on_hover_text("Play or pause (Space)")
            .clicked()
        {
            toggle_playback(timeline);
        }
        let next = frames.range(at + 1..).next().copied();
        if ui
            .add_enabled(next.is_some(), egui::Button::new("▶|"))
            .on_hover_text("Next key")
            .clicked()
        {
            timeline.playing = None;
            timeline.playhead = f64::from(next.unwrap_or(0));
        }
        if ui.button("⏭").on_hover_text("Last frame").clicked() {
            timeline.playing = None;
            timeline.playhead = f64::from(sequence.length);
        }
        ui.checkbox(&mut timeline.looping, "Loop");
        ui.separator();

        ui.label("Frame");
        let mut frame = at;
        if ui
            .add(
                egui::DragValue::new(&mut frame)
                    .range(0..=sequence.length)
                    .clamp_existing_to_range(false),
            )
            .changed()
        {
            timeline.playing = None;
            timeline.playhead = f64::from(frame);
        }
        ui.label("Frame rate");
        let mut frame_rate = sequence.frame_rate;
        let response = ui.add(
            egui::DragValue::new(&mut frame_rate)
                .range(1..=MAX_FRAME_RATE)
                .update_while_editing(false),
        );
        if response.changed() {
            change_live(timeline, "frame rate", |s| s.frame_rate = frame_rate);
        }
        let mut finished = ended(&response);
        ui.label("Length");
        let mut length = sequence.length;
        let response = ui.add(
            egui::DragValue::new(&mut length)
                .range(1..=MAX_LENGTH)
                .update_while_editing(false),
        );
        if response.changed() {
            change_live(timeline, "length", |s| s.length = length);
        }
        finished |= ended(&response);
        if finished {
            finish_editing(timeline, ctx);
        }
        ui.separator();
        if ui.selectable_label(!timeline.panel.curves, "Dopesheet").clicked() {
            timeline.panel.curves = false;
        }
        if ui.selectable_label(timeline.panel.curves, "Curves").clicked() {
            timeline.panel.curves = true;
        }
        ui.separator();

        let addable: Vec<&PropertyInfo> = declared
            .values()
            .filter(|info| sequence.track(&info.path).is_none())
            .collect();
        ui.add_enabled_ui(!addable.is_empty(), |ui| {
            ui.menu_button("Add property", |ui| {
                for info in addable {
                    if ui.button(format!("{}: {}", info.owner, info.label)).clicked() {
                        let mut after = sequence.clone();
                        after.tracks.push(Track::new(&info.path, info.kind));
                        timeline.edit(ctx, &format!("add {}", info.label), after);
                        ui.close();
                    }
                }
            });
        });
    });
}

fn toggle_playback(timeline: &mut TimelineModule) {
    if timeline.playing.take().is_none() {
        timeline.play();
    }
}

/// Whether a value being changed over several frames is done with.
fn ended(response: &egui::Response) -> bool {
    response.drag_stopped()
        || response.lost_focus()
        || (response.changed() && !response.dragged() && !response.has_focus())
}

/// Changes the shown sequence at once, for a change made over several frames: `finish_editing`
/// records it as one undo entry.
fn change_live(timeline: &mut TimelineModule, label: &str, change: impl FnOnce(&mut Sequence)) {
    let Some(document) = timeline
        .current
        .as_ref()
        .and_then(|name| timeline.documents.get_mut(name))
    else {
        return;
    };
    if timeline.panel.editing.is_none() {
        timeline.panel.editing = Some((label.to_owned(), document.sequence.clone(), document.dirty));
    }
    change(&mut document.sequence);
    document.dirty = true;
    timeline.keys_changed = true;
}

/// Puts the sequence back as it was before the change, and records the change as one undo entry.
fn finish_editing(timeline: &mut TimelineModule, ctx: &mut Context) {
    let Some((label, before, dirty)) = timeline.panel.editing.take() else {
        return;
    };
    let Some(document) = timeline
        .current
        .as_ref()
        .and_then(|name| timeline.documents.get_mut(name))
    else {
        return;
    };
    let after = std::mem::replace(&mut document.sequence, before);
    if after == document.sequence {
        document.dirty = dirty;
    } else {
        timeline.edit(ctx, &label, after);
    }
}

/// The rows of the dopesheet: every key, then each module's tracks under its name.
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

fn owner(path: &str) -> &str {
    path.split_once('/').map_or(path, |(owner, _)| owner)
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

/// The names of a property's numbers.
pub fn number_names(kind: PropertyKind) -> &'static [&'static str] {
    match kind {
        PropertyKind::Vector => &["x", "y", "z"],
        PropertyKind::Colour => &["red", "green", "blue"],
        PropertyKind::Number | PropertyKind::Boolean => &["value"],
    }
}

/// The colour a number's curve is drawn in.
pub fn number_colour(kind: PropertyKind, number: usize) -> Color32 {
    match (kind.components(), number) {
        (3, 0) => Color32::from_rgb(220, 70, 60),
        (3, 1) => Color32::from_rgb(80, 170, 60),
        (3, _) => Color32::from_rgb(60, 120, 230),
        _ => Color32::from_rgb(230, 160, 40),
    }
}

fn label(track: &Track, declared: &BTreeMap<String, PropertyInfo>) -> String {
    declared.get(&track.property).map_or_else(
        || track.property.rsplit('/').next().unwrap_or(&track.property).to_owned(),
        |info| info.label.clone(),
    )
}

fn dopesheet(
    timeline: &mut TimelineModule,
    sequence: &Sequence,
    declared: &BTreeMap<String, PropertyInfo>,
    ui: &mut egui::Ui,
    ctx: &mut Context,
) {
    let (ruler, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), RULER), Sense::hover());
    let left = ruler.left() + LEFT;
    let state = &mut timeline.panel;
    if !state.fitted || state.pixels_per_frame <= 0.0 {
        let width = (ruler.right() - left - 24.0).max(50.0);
        state.pixels_per_frame = (width / sequence.length as f32).clamp(ZOOM[0], ZOOM[1]);
        state.first_frame = -f64::from(12.0 / state.pixels_per_frame);
        state.fitted = true;
    }
    let view = View {
        left,
        first: state.first_frame,
        pixels_per_frame: state.pixels_per_frame,
    };
    let ruler_right = Rect::from_min_max(egui::pos2(left, ruler.top()), ruler.max);
    draw_ruler(timeline, sequence, ui, ruler, ruler_right, view);

    let rows = rows(sequence, &timeline.panel.unfolded);
    if timeline.panel.curves {
        curves_side(timeline, sequence, declared, &rows, ui, ctx, view);
        navigate(timeline, ui, ruler_right, view);
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("timeline-rows")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let height = (rows.len() as f32 * ROW).max(ui.available_height());
            let (area, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
            for (index, row) in rows.iter().enumerate() {
                let rect = Rect::from_min_size(area.min + egui::vec2(0.0, index as f32 * ROW), egui::vec2(LEFT, ROW));
                properties_row(timeline, sequence, declared, row, rect, ui, ctx);
            }
            let keys = Rect::from_min_max(egui::pos2(left, area.top()), area.max);
            let dope = keys.union(ruler_right);
            navigate(timeline, ui, dope, view);
            keys_area(timeline, sequence, declared, &rows, keys, view, ui, ctx);
        });
}

fn draw_ruler(
    timeline: &mut TimelineModule,
    sequence: &Sequence,
    ui: &mut egui::Ui,
    ruler: Rect,
    right: Rect,
    view: View,
) {
    let response = ui.interact(right, ui.id().with("timeline-ruler"), Sense::click_and_drag());
    if (response.is_pointer_button_down_on() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        timeline.playing = None;
        timeline.playhead = view.frame(pointer.x).round().clamp(0.0, f64::from(sequence.length));
    }
    let painter = ui.painter_at(ruler);
    let visuals = ui.visuals();
    painter.rect_filled(right, 0.0, visuals.faint_bg_color);
    painter.text(
        ruler.left_center() + egui::vec2(6.0, 0.0),
        Align2::LEFT_CENTER,
        format!(
            "Frame {} ({})",
            timeline.playhead.round(),
            time_label(timeline.playhead.round() as i64, sequence.frame_rate)
        ),
        FontId::proportional(12.0),
        visuals.text_color(),
    );
    let painter = ui.painter_at(right);
    let step = tick_step(view.pixels_per_frame, sequence.frame_rate);
    let first = (view.frame(right.left()).floor() as i64).max(0);
    let last = view.frame(right.right()).ceil() as i64;
    let mut frame = first - first % i64::from(step);
    while frame <= last {
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
        frame += i64::from(step);
    }
    let x = view.x(timeline.playhead);
    let marker = Rect::from_center_size(egui::pos2(x, right.center().y), egui::vec2(28.0, RULER - 4.0));
    painter.rect_filled(marker, 3.0, PLAYHEAD);
    painter.text(
        marker.center(),
        Align2::CENTER_CENTER,
        format!("{}", timeline.playhead.round()),
        FontId::proportional(11.0),
        Color32::WHITE,
    );
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

/// The left side of a row: a module's name, a property with the values of its numbers at the
/// playhead, or one of those numbers.
#[allow(clippy::too_many_arguments)]
fn properties_row(
    timeline: &mut TimelineModule,
    sequence: &Sequence,
    declared: &BTreeMap<String, PropertyInfo>,
    row: &Row,
    rect: Rect,
    ui: &mut egui::Ui,
    ctx: &mut Context,
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
            ui.weak(timeline.current.clone().unwrap_or_default());
            return;
        }
        Row::Group(name) => {
            ui.strong(name);
            return;
        }
        Row::Track(index) => (*index, None),
        Row::Number(index, number) => (*index, Some(*number)),
    };
    let track = &sequence.tracks[index];
    let info = declared.get(&track.property);
    let name = label(track, declared);
    let names = number_names(track.kind);
    let shown_name = match only {
        Some(number) => format!("{name}.{}", names[number]),
        None => name.clone(),
    };
    let text = |ui: &egui::Ui, text: &str| {
        egui::RichText::new(text).color(if info.is_some() {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        })
    };
    match only {
        None => {
            if track.curves.len() > 1 {
                let unfolded = timeline.panel.unfolded.contains(&track.property);
                if icon_button(ui, if unfolded { Icon::Unfolded } else { Icon::Folded })
                    .on_hover_text("Show or hide a row for each number")
                    .clicked()
                    && !timeline.panel.unfolded.remove(&track.property)
                {
                    timeline.panel.unfolded.insert(track.property.clone());
                }
            } else {
                ui.add_space(ROW - 4.0 + ui.spacing().item_spacing.x);
            }
            if timeline.panel.curves {
                shown_box(timeline, &track.property, 0..track.curves.len(), ui);
            }
            ui.add_sized([66.0, ROW - 4.0], egui::Label::new(text(ui, &name)).truncate())
                .on_hover_text(if info.is_some() {
                    track.property.clone()
                } else {
                    format!("{}: no running module declares it", track.property)
                });
        }
        Some(number) => {
            ui.add_space(ROW + 8.0);
            if timeline.panel.curves {
                shown_box(timeline, &track.property, number..number + 1, ui);
            }
            let (swatch, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(swatch, 2.0, number_colour(track.kind, number));
            ui.add_sized([52.0, ROW - 4.0], egui::Label::new(text(ui, names[number])).truncate());
        }
    }
    let editor = timeline.editor.clone();
    let current = editor.as_ref().and_then(|e| e.read_property(&track.property).ok());
    let shown = track
        .evaluate(timeline.playhead, current)
        .or(current)
        .unwrap_or_else(|| PropertyValue::from_components(track.kind, &[]));
    let mut numbers = shown.components();
    // Only what the user did sets keys: a field that brings a value back within its range on its
    // own must not.
    let mut edited: Vec<usize> = Vec::new();
    let range = info.map_or([f64::MIN, f64::MAX], |info| info.range);
    let speed = ((range[1] - range[0]) / 2000.0).clamp(0.005, 1.0);
    let mut finished = false;
    ui.add_enabled_ui(info.is_some(), |ui| {
        if track.kind == PropertyKind::Boolean {
            let mut on = numbers[0] >= 0.5;
            if ui.checkbox(&mut on, "").changed() {
                numbers[0] = if on { 1.0 } else { 0.0 };
                edited.push(0);
                finished = true;
            }
        } else {
            for (number, value) in numbers.iter_mut().enumerate() {
                if only.is_some_and(|only| only != number) {
                    continue;
                }
                let response = ui.add_sized(
                    [62.0, ROW - 4.0],
                    egui::DragValue::new(value)
                        .speed(speed)
                        .range(range[0]..=range[1])
                        .clamp_existing_to_range(false)
                        .max_decimals(3)
                        .update_while_editing(false),
                );
                if response.changed() {
                    edited.push(number);
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
        if icon_button(ui, Icon::Key)
            .on_hover_text("Add a key at the playhead with the current value")
            .clicked()
            && let Some(value) = current
        {
            let frame = f64::from(timeline.playhead.round() as u32);
            let mut after = sequence.clone();
            if let Some(edited) = after.track_mut(&track.property) {
                for (number, (curve, value)) in edited.curves.iter_mut().zip(value.components()).enumerate() {
                    if only.is_none_or(|only| only == number) {
                        curve.set_key(frame, value);
                    }
                }
            }
            timeline.edit(ctx, &format!("add a key to {shown_name}"), after);
        }
    });
    // Each number changed gets a key on its own curve, at the frame shown, which the playhead
    // then stays on.
    let changed: Vec<(usize, f64)> = edited.into_iter().map(|number| (number, numbers[number])).collect();
    if !changed.is_empty() {
        let frame = timeline.playhead.round();
        timeline.playing = None;
        timeline.playhead = frame;
        let property = track.property.clone();
        change_live(timeline, &format!("set a key of {shown_name}"), |s| {
            if let Some(edited) = s.track_mut(&property) {
                for (number, value) in changed {
                    edited.curves[number].set_key(frame, value);
                }
            }
        });
    }
    if finished {
        finish_editing(timeline, ctx);
    }
    if only.is_none()
        && icon_button(ui, Icon::Remove)
            .on_hover_text("Remove this track")
            .clicked()
    {
        let mut after = sequence.clone();
        after.tracks.retain(|t| t.property != track.property);
        timeline.edit(ctx, &format!("remove {name}"), after);
    }
}

/// A box showing or hiding the curves of these numbers in the Curves view.
fn shown_box(timeline: &mut TimelineModule, property: &str, numbers: std::ops::Range<usize>, ui: &mut egui::Ui) {
    let hidden = &mut timeline.panel.hidden;
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

/// The Curves view: the properties on the left, the curve editor of the module `curves` on the
/// right, its time following the ruler's.
#[allow(clippy::too_many_arguments)]
fn curves_side(
    timeline: &mut TimelineModule,
    sequence: &Sequence,
    declared: &BTreeMap<String, PropertyInfo>,
    rows: &[Row],
    ui: &mut egui::Ui,
    ctx: &mut Context,
    view: View,
) {
    let body = ui.available_rect_before_wrap();
    ui.allocate_rect(body, Sense::hover());
    let mut left =
        ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(body.min, egui::pos2(view.left, body.bottom()))));
    egui::ScrollArea::vertical()
        .id_salt("timeline-curve-rows")
        .auto_shrink([false, false])
        .show(&mut left, |ui| {
            let (area, _) = ui.allocate_exact_size(egui::vec2(LEFT, rows.len() as f32 * ROW), Sense::hover());
            for (index, row) in rows.iter().enumerate() {
                let rect = Rect::from_min_size(area.min + egui::vec2(0.0, index as f32 * ROW), egui::vec2(LEFT, ROW));
                properties_row(timeline, sequence, declared, row, rect, ui, ctx);
            }
        });
    let mut right =
        ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(egui::pos2(view.left, body.top()), body.max)));
    let Some(editor) = ctx.service(curve::SERVICE) else {
        right.centered_and_justified(|ui| {
            ui.weak("No curve editor: the module curves is not running.");
        });
        return;
    };
    // The curves of the numbers, a boolean holding its value from key to key without a curve.
    let mut shown = Vec::new();
    let mut origin = Vec::new();
    for (index, track) in sequence.tracks.iter().enumerate() {
        if track.kind == PropertyKind::Boolean {
            continue;
        }
        let name = label(track, declared);
        for (number, curve) in track.curves.iter().enumerate() {
            let colour = number_colour(track.kind, number);
            shown.push(ShownCurve {
                label: format!("{name}.{}", number_names(track.kind)[number]),
                colour: [colour.r(), colour.g(), colour.b()],
                curve: curve.clone(),
                visible: !timeline.panel.hidden.contains(&(track.property.clone(), number)),
            });
            origin.push((index, number));
        }
    }
    let mut time = TimeAxis {
        first: view.first,
        pixels_per_unit: view.pixels_per_frame,
    };
    let options = CurveOptions {
        snap: Some(1.0),
        playhead: Some(timeline.playhead),
        span: Some([0.0, f64::from(sequence.length)]),
    };
    let id = right.id().with("timeline-curves");
    let shown_before = shown.clone();
    let change = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        editor.show(&mut right, id, &mut shown, &mut time, &options)
    })) {
        Ok(change) => change,
        Err(_) => {
            // The curve editor's fault, not the Timeline's (F5): its sequences stay.
            if let Some(provider) = ctx.service_provider(curve::SERVICE) {
                ctx.report_failure(&provider, "the curve editor panicked in the Timeline");
            }
            shown = shown_before;
            CurveChange::None
        }
    };
    timeline.panel.first_frame = time.first;
    timeline.panel.pixels_per_frame = time.pixels_per_unit;
    if change != CurveChange::None {
        change_live(timeline, "edit curves", |s| {
            for ((index, number), edited) in origin.iter().zip(shown) {
                s.tracks[*index].curves[*number] = edited.curve;
            }
        });
        if change == CurveChange::Finished {
            finish_editing(timeline, ctx);
        }
    }
}

/// The wheel zooms the time around the pointer, the middle button scrolls it.
fn navigate(timeline: &mut TimelineModule, ui: &mut egui::Ui, dope: Rect, view: View) {
    if !ui.rect_contains_pointer(dope) {
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
    let state = &mut timeline.panel;
    if scroll.y != 0.0
        && let Some(pointer) = pointer
    {
        let anchored = view.frame(pointer.x);
        state.pixels_per_frame = (state.pixels_per_frame * (scroll.y * 0.004).exp()).clamp(ZOOM[0], ZOOM[1]);
        state.first_frame = anchored - f64::from((pointer.x - view.left) / state.pixels_per_frame);
    }
    if middle && delta.x != 0.0 {
        state.first_frame -= f64::from(delta.x / state.pixels_per_frame);
    }
}

/// The keys as diamonds, their selection, moves and box, and the playhead across the rows.
#[allow(clippy::too_many_arguments)]
fn keys_area(
    timeline: &mut TimelineModule,
    sequence: &Sequence,
    declared: &BTreeMap<String, PropertyInfo>,
    rows: &[Row],
    area: Rect,
    view: View,
    ui: &mut egui::Ui,
    ctx: &mut Context,
) {
    let response = ui.interact(area, ui.id().with("timeline-keys"), Sense::click_and_drag());
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
    let selection = &mut timeline.selection;
    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        timeline.panel.gesture = match hit(origin) {
            Some(keys) => {
                if !keys.iter().all(|key| selection.contains(key)) {
                    if !command {
                        selection.clear();
                    }
                    selection.extend(keys);
                }
                Gesture::Move {
                    from: origin.x,
                    to: origin.x,
                }
            }
            None => {
                if !command {
                    selection.clear();
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
        match &mut timeline.panel.gesture {
            Gesture::Move { to, .. } => *to = pointer.x,
            Gesture::Select { to, .. } => *to = pointer,
            Gesture::None => {}
        }
    }
    let offset = match timeline.panel.gesture {
        Gesture::Move { from, to } => ((to - from) / view.pixels_per_frame).round() as i64,
        _ => 0,
    };
    let selecting = match timeline.panel.gesture {
        Gesture::Select { from, to } => Some(Rect::from_two_pos(from, to)),
        _ => None,
    };
    if response.drag_stopped() {
        if offset != 0 {
            let mut after = sequence.clone();
            let moved = after.move_keys(&timeline.selection, offset);
            timeline.edit(ctx, "move keys", after);
            timeline.selection = moved;
        }
        if let Some(rect) = selecting {
            for (index, centre, keys) in &diamonds {
                if matches!(rows[*index], Row::Track(_) | Row::Number(..)) && rect.contains(*centre) {
                    timeline.selection.extend(keys.iter().cloned());
                }
            }
        }
        timeline.panel.gesture = Gesture::None;
    }

    // Drawn as they will be once a move ends.
    let (shown, selected) = if offset == 0 {
        (sequence.clone(), timeline.selection.clone())
    } else {
        let mut preview = sequence.clone();
        let moved = preview.move_keys(&timeline.selection, offset);
        (preview, moved)
    };
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
        let missing =
            matches!(row, Row::Track(i) | Row::Number(i, _) if !declared.contains_key(&sequence.tracks[*i].property));
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
    let x = view.x(timeline.playhead);
    painter.line_segment(
        [egui::pos2(x, area.top()), egui::pos2(x, area.bottom())],
        Stroke::new(1.5, PLAYHEAD),
    );
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

/// Space plays or pauses and Delete removes the selected keys, while the pointer is over the
/// panel and no field has the keyboard.
fn keyboard(timeline: &mut TimelineModule, sequence: &Sequence, ui: &mut egui::Ui, ctx: &mut Context) {
    let typing = ui.ctx().memory(|memory| memory.focused().is_some());
    if typing || !ui.ui_contains_pointer() {
        return;
    }
    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space)) {
        toggle_playback(timeline);
    }
    // In the Curves view, the curve editor deletes its own keys.
    if !timeline.panel.curves
        && !timeline.selection.is_empty()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
    {
        let mut after = sequence.clone();
        after.remove_keys(&timeline.selection);
        timeline.edit(ctx, "delete keys", after);
        timeline.selection = BTreeSet::new();
    }
}
