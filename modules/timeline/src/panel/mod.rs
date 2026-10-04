//! The panel of the timeline: the sequence and playback bars, the animated properties on the
//! left, and the dopesheet on the right, with its ruler and playhead.

mod curves;
mod dopesheet;
mod playback;

use std::collections::{BTreeMap, BTreeSet};

use uniwow_api::curve::{self, CurveChange, CurveOptions, ShownCurve, TimeAxis};
use uniwow_api::egui::{self, Align, Align2, Color32, FontId, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use uniwow_api::serde_json::json;
use uniwow_api::{Context, DIALOG_COMMAND, PropertyInfo, PropertyKind, PropertyValue};

use crate::{Question, TimelineModule};
use uniwow_api::sequence::{KeyId, MAX_FRAME_RATE, MAX_LENGTH, Sequence, Track, frame_of};

use curves::{curves_side, shown_box};
use dopesheet::{Gesture, Row, dopesheet, properties_row};
use playback::{Icon, icon_button, playback_bar, toggle_playback};

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
pub(crate) fn switch(timeline: &mut TimelineModule, ctx: &mut Context, target: String, dirty: bool) {
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
        // Without a window to ask in, the changes are lost, as when closing the editor.
        timeline.discard(&current, ctx);
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

/// Whether a value being changed over several frames is done with.
fn ended(response: &egui::Response) -> bool {
    response.drag_stopped()
        || response.lost_focus()
        || (response.changed() && !response.dragged() && !response.has_focus())
}

/// Changes the shown sequence at once, for a change made over several frames: `finish_editing`
/// records it as one undo entry.
pub(crate) fn change_live(timeline: &mut TimelineModule, label: &str, change: impl FnOnce(&mut Sequence)) {
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

/// Puts the sequence back as it was before a change under way, which is dropped.
pub(crate) fn cancel_editing(timeline: &mut TimelineModule) {
    let Some((_, before, dirty)) = timeline.panel.editing.take() else {
        return;
    };
    if let Some(document) = timeline
        .current
        .as_ref()
        .and_then(|name| timeline.documents.get_mut(name))
    {
        document.sequence = before;
        document.dirty = dirty;
        timeline.keys_changed = true;
    }
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
    } else if !timeline.edit(ctx, &label, after)
        && let Some(document) = timeline
            .current
            .as_ref()
            .and_then(|name| timeline.documents.get_mut(name))
    {
        document.dirty = dirty;
    }
}

fn owner(path: &str) -> &str {
    path.split_once('/').map_or(path, |(owner, _)| owner)
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
