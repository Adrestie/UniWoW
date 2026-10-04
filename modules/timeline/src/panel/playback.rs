//! The playback bar: play, the frame, the frame rate and the length, the view shown, and the
//! properties to add.

use std::collections::BTreeMap;

use uniwow_api::egui;
use uniwow_api::sequence::{MAX_FRAME_RATE, MAX_LENGTH, Sequence, Track};
use uniwow_api::ui::Property;
use uniwow_api::{Context, PropertyInfo, log};

use super::{change_live, ended, finish_editing};
use crate::TimelineModule;

/// Playback, the frame, the frame rate and the length, the view shown, and the properties to add.
pub(super) fn playback_bar(timeline: &mut TimelineModule, sequence: &Sequence, ui: &mut egui::Ui, ctx: &mut Context) {
    let frames = sequence.key_frames();
    let (time, playing, looping) = timeline.objects.playback();
    let at = time.round() as u32;
    ui.horizontal(|ui| {
        if ui.button("⏮").on_hover_text("First frame").clicked() {
            timeline.objects.seek(0.0);
        }
        let previous = frames.range(..at).next_back().copied();
        if ui
            .add_enabled(previous.is_some(), egui::Button::new("◀"))
            .on_hover_text("Previous key")
            .clicked()
        {
            timeline.objects.seek(f64::from(previous.unwrap_or(0)));
        }
        if ui
            .button(if playing { "⏸" } else { "▶" })
            .on_hover_text("Play or pause (Space)")
            .clicked()
        {
            timeline.objects.toggle_playback();
        }
        let next = frames.range(at + 1..).next().copied();
        if ui
            .add_enabled(next.is_some(), egui::Button::new("▶|"))
            .on_hover_text("Next key")
            .clicked()
        {
            timeline.objects.seek(f64::from(next.unwrap_or(0)));
        }
        if ui.button("⏭").on_hover_text("Last frame").clicked() {
            timeline.objects.seek(f64::from(sequence.length));
        }
        let mut looped = looping;
        if ui.checkbox(&mut looped, "Loop").changed() {
            let player = timeline.objects.player;
            timeline
                .objects
                .set(player, Property::Loop, if looped { 1.0 } else { 0.0 });
        }
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
            timeline.objects.seek(f64::from(frame));
        }
        ui.label("Frame rate");
        let mut frame_rate = sequence.frame_rate;
        let response = ui.add(
            egui::DragValue::new(&mut frame_rate)
                .range(1..=MAX_FRAME_RATE)
                .update_while_editing(false),
        );
        if response.changed() {
            change_live(timeline, "frame rate", Property::FrameRate, f64::from(frame_rate));
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
            change_live(timeline, "length", Property::Length, f64::from(length));
        }
        finished |= ended(&response);
        if finished {
            finish_editing(timeline, ctx);
        }
        ui.separator();
        let mut curves = timeline.panel.curves;
        if ui.selectable_label(!curves, "Dopesheet").clicked() {
            curves = false;
        }
        if ui.selectable_label(curves, "Curves").clicked() {
            curves = true;
        }
        if curves != timeline.panel.curves {
            timeline.panel.curves = curves;
            timeline.objects.show_curves(curves);
        }
        ui.separator();
        add_property(timeline, sequence, ui);
    });
}

/// The menu adding a track for a property of a running module the sequence does not animate yet.
fn add_property(timeline: &mut TimelineModule, sequence: &Sequence, ui: &mut egui::Ui) {
    let declared: BTreeMap<String, PropertyInfo> = timeline
        .editor
        .as_ref()
        .map(|editor| editor.properties())
        .unwrap_or_default()
        .into_iter()
        .map(|info| (info.path.clone(), info))
        .collect();
    let addable: Vec<&PropertyInfo> = declared
        .values()
        .filter(|info| sequence.track(&info.path).is_none())
        .collect();
    let Some(handle) = timeline
        .current
        .as_ref()
        .and_then(|name| timeline.documents.get(name))
        .map(|document| document.sequence)
    else {
        return;
    };
    ui.add_enabled_ui(!addable.is_empty(), |ui| {
        ui.menu_button("Add property", |ui| {
            for info in addable {
                if ui.button(format!("{}: {}", info.owner, info.label)).clicked() {
                    let mut tracks = sequence.tracks.clone();
                    tracks.push(Track::new(&info.path, info.kind));
                    let label = format!("add {}", info.label);
                    if let Err(error) = timeline.objects.lock().change_tracks(handle, tracks, &label) {
                        log::warn!("{error}");
                    }
                    ui.close();
                }
            }
        });
    });
}
