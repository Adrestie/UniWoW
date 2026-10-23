//! The panel of the timeline: the sequence and playback bars, then the dopesheet or the Curves
//! view, objects of the core drawn by the kernel.

mod playback;

use uniwow_api::egui;
use uniwow_api::serde_json::json;
use uniwow_api::ui::Property;
use uniwow_api::{Context, DIALOG_COMMAND};

use crate::{NumberEdit, Question, TimelineModule, VIEWS};
use playback::playback_bar;

/// The frame rate or the length being dragged, recorded as one undo entry when the drag ends.
pub struct Editing {
    name: String,
    label: String,
    property: Property,
    /// The value before the drag.
    before: f64,
}

/// What the panel keeps between frames.
#[derive(Default)]
pub struct State {
    pub editing: Option<Editing>,
    new_name: String,
    message: Option<String>,
    /// The Curves view shown rather than the dopesheet.
    curves: bool,
}

impl State {
    /// A message shown under the sequence bar.
    pub fn say(&mut self, message: &str) {
        self.message = Some(message.to_owned());
    }

    /// The message shown, for the tests.
    #[cfg(test)]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

pub fn show(timeline: &mut TimelineModule, ui: &mut egui::Ui, ctx: &mut Context) {
    sequence_bar(timeline, ui, ctx);
    if let Some(message) = &timeline.panel.message {
        ui.colored_label(ui.visuals().warn_fg_color, message);
    }
    let Some(sequence) = timeline.sequence() else {
        ui.weak("No sequence shown: choose one or create one.");
        return;
    };
    playback_bar(timeline, &sequence, ui, ctx);
    segment_bar(timeline, ui);
    ui.separator();
    ctx.draw_objects(&timeline.objects.store, VIEWS, ui);
    keyboard(timeline, ui);
}

/// Chooses, creates and saves sequences.
fn sequence_bar(timeline: &mut TimelineModule, ui: &mut egui::Ui, ctx: &mut Context) {
    let dirty = timeline.current.as_ref().is_some_and(|name| timeline.dirty(name));
    ui.horizontal(|ui| {
        ui.label("Sequence");
        let externals: Vec<(String, String)> = crate::external::lock(&timeline.externals)
            .shown
            .iter()
            .map(|(owner, shown)| (owner.clone(), shown.presented.title.clone()))
            .collect();
        let shown = match (&timeline.external, &timeline.current) {
            (Some(owner), _) => externals
                .iter()
                .find(|(o, _)| o == owner)
                .map_or_else(|| owner.clone(), |(_, title)| title.clone()),
            (None, Some(name)) if dirty => format!("{name} *"),
            (None, Some(name)) => name.clone(),
            (None, None) => "none".to_owned(),
        };
        let mut chosen = None;
        let mut chosen_external = None;
        let list = egui::ComboBox::from_id_salt("timeline-sequence")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                // what the modules present first: an effect's keys...
                for (owner, title) in &externals {
                    if ui
                        .selectable_label(timeline.external.as_ref() == Some(owner), format!("⚡ {title}"))
                        .on_hover_text(format!("Presented by the module '{owner}': its changes go back to it"))
                        .clicked()
                    {
                        chosen_external = Some(owner.clone());
                    }
                }
                for name in &timeline.names {
                    if ui
                        .selectable_label(timeline.current.as_ref() == Some(name), name)
                        .clicked()
                    {
                        chosen = Some(name.clone());
                    }
                }
            });
        // The files of the folder read again when the list opens: one added meanwhile shows.
        if list.response.clicked() {
            timeline.refresh_names();
        }
        if let Some(owner) = chosen_external {
            timeline.show_external(&owner);
        }
        if let Some(name) = chosen.filter(|name| timeline.external.is_some() || timeline.current.as_ref() != Some(name))
        {
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

/// Shows the sequence `name`, or says why it cannot.
pub fn open(timeline: &mut TimelineModule, name: &str) {
    timeline.panel.message = timeline.open(name).err();
}

/// Whether a value being changed over several frames is done with.
fn ended(response: &egui::Response) -> bool {
    response.drag_stopped()
        || response.lost_focus()
        || (response.changed() && !response.dragged() && !response.has_focus())
}

/// Sets the frame rate or the length of the sequence shown at once, while its field is dragged:
/// `finish_editing` records the change as one undo entry.
pub(crate) fn change_live(timeline: &mut TimelineModule, label: &str, property: Property, value: f64) {
    let Some(name) = timeline.current.clone() else {
        return;
    };
    let Some(before) = timeline.set_number(&name, property, value) else {
        return;
    };
    if timeline.panel.editing.is_none() {
        timeline.panel.editing = Some(Editing {
            name,
            label: label.to_owned(),
            property,
            before,
        });
    }
}

/// Puts the sequence back as it was before a change under way, which is dropped.
pub(crate) fn cancel_editing(timeline: &mut TimelineModule) {
    if let Some(editing) = timeline.panel.editing.take() {
        timeline.set_number(&editing.name, editing.property, editing.before);
    }
}

/// Puts the sequence back as it was before the change, and records the change as one undo entry.
fn finish_editing(timeline: &mut TimelineModule, ctx: &mut Context) {
    let Some(editing) = timeline.panel.editing.take() else {
        return;
    };
    let Some(after) = timeline.set_number(&editing.name, editing.property, editing.before) else {
        return;
    };
    if after != editing.before {
        ctx.execute(NumberEdit {
            name: editing.name,
            label: editing.label,
            property: editing.property,
            after,
            before: None,
        });
    }
}

/// The segments of what a module presents (an effect's phases) across the panel, the playhead on
/// them; a click puts the playhead at its point.
fn segment_bar(timeline: &mut TimelineModule, ui: &mut egui::Ui) {
    let Some((segments, length)) = timeline.segments() else {
        return;
    };
    if segments.is_empty() || length == 0 {
        return;
    }
    let (time, _, _) = timeline.objects.playback();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::click_and_drag());
    let response = response.on_hover_text(
        "The phases of what is shown (an effect: born once, held while its aura lasts, ended once): click to go there",
    );
    let painter = ui.painter_at(rect);
    let x = |frame: f64| rect.left() + rect.width() * (frame / f64::from(length)) as f32;
    let colours = [
        egui::Color32::from_rgb(70, 110, 160),
        egui::Color32::from_rgb(80, 140, 90),
        egui::Color32::from_rgb(150, 100, 70),
    ];
    for (index, segment) in segments.iter().enumerate() {
        let span = egui::Rect::from_x_y_ranges(x(f64::from(segment.start))..=x(f64::from(segment.end)), rect.y_range());
        painter.rect_filled(span.shrink(1.0), 3.0, colours[index % colours.len()]);
        painter.text(
            span.center(),
            egui::Align2::CENTER_CENTER,
            &segment.label,
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
    }
    painter.vline(
        x(time),
        rect.y_range(),
        egui::Stroke::new(2.0, egui::Color32::from_rgb(240, 200, 60)),
    );
    if (response.clicked() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let share = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        timeline.objects.seek((f64::from(share) * f64::from(length)).round());
    }
}

/// Its hotkey, Space by default, plays or pauses, while the pointer is over the panel and no field
/// has the keyboard.
fn keyboard(timeline: &mut TimelineModule, ui: &mut egui::Ui) {
    let typing = ui.ctx().memory(|memory| memory.focused().is_some());
    if !typing && ui.ui_contains_pointer() && timeline.play.pressed(ui.ctx()) {
        timeline.objects.toggle_playback();
    }
}
