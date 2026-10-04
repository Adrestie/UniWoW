//! The playback bar: play, the frame, the frame rate and the length, and the properties to add.

use super::*;

/// Playback, the frame, the frame rate and the length, and the properties to add.
pub(super) fn playback_bar(
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

pub(super) fn toggle_playback(timeline: &mut TimelineModule) {
    if timeline.playing.take().is_none() {
        timeline.play();
    }
}

pub(super) enum Icon {
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
pub(super) fn icon_button(ui: &mut egui::Ui, icon: Icon) -> egui::Response {
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
