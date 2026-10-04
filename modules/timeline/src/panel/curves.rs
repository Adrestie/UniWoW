//! The Curves view: the properties on the left, the curve editor of the module `curves` on the right.

use super::*;

/// A box showing or hiding the curves of these numbers in the Curves view.
pub(super) fn shown_box(
    timeline: &mut TimelineModule,
    property: &str,
    numbers: std::ops::Range<usize>,
    ui: &mut egui::Ui,
) {
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
pub(super) fn curves_side(
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
            // A drag under way is dropped: the next change starts from the sequence as it was.
            cancel_editing(timeline);
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
