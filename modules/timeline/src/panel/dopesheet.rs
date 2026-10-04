//! The dopesheet: its rows, ruler and keys, the properties on its left, and the playhead.

use super::*;

#[derive(Default)]
pub(super) enum Gesture {
    #[default]
    None,
    /// The selected keys dragged, from and to these x.
    Move { from: f32, to: f32 },
    /// A selection box.
    Select { from: Pos2, to: Pos2 },
}

/// A row of the dopesheet, with the keys it shows.
pub(super) enum Row {
    /// Every key of the sequence.
    Summary,
    /// The keys of one module's tracks.
    Group(String),
    /// A property: the keys of all its numbers.
    Track(usize),
    /// One number of a property, by its index.
    Number(usize, usize),
}

/// The rows of the dopesheet: every key, then each module's tracks under its name.
pub(super) fn rows(sequence: &Sequence, unfolded: &BTreeSet<String>) -> Vec<Row> {
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
pub(super) fn row_keys(sequence: &Sequence, row: &Row) -> BTreeMap<u32, Vec<KeyId>> {
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

pub(super) fn dopesheet(
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

pub(super) fn draw_ruler(
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
pub(super) fn time_label(frame: i64, frame_rate: u32) -> String {
    let rate = i64::from(frame_rate.max(1));
    format!("{}:{:02}", frame / rate, frame % rate)
}

/// Frames between two labels of the ruler, at least 50 pixels apart.
pub(super) fn tick_step(pixels_per_frame: f32, frame_rate: u32) -> u32 {
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
pub(super) fn properties_row(
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
                let response = number_field(ui, value, range, speed);
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

/// The wheel zooms the time around the pointer, the middle button scrolls it.
pub(super) fn navigate(timeline: &mut TimelineModule, ui: &mut egui::Ui, dope: Rect, view: View) {
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
pub(super) fn keys_area(
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

pub(super) fn diamond(painter: &egui::Painter, centre: Pos2, fill: Color32, outline: Color32) {
    let points = vec![
        centre + Vec2::new(0.0, -DIAMOND),
        centre + Vec2::new(DIAMOND, 0.0),
        centre + Vec2::new(0.0, DIAMOND),
        centre + Vec2::new(-DIAMOND, 0.0),
    ];
    painter.add(egui::Shape::convex_polygon(points, fill, Stroke::new(1.0, outline)));
}

/// A number as its field shows it: at most three decimals.
fn field_text(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0".to_owned() } else { text.to_owned() }
}

/// The field of a number at the playhead. Its text, typed or not, is kept once the field loses the
/// keyboard; the text it showed when it took it is no value typed, even rounded or out of range.
pub(super) fn number_field(ui: &mut egui::Ui, value: &mut f64, range: [f64; 2], speed: f64) -> egui::Response {
    let shown = field_text(*value);
    ui.add_sized(
        [62.0, ROW - 4.0],
        egui::DragValue::new(value)
            .speed(speed)
            .range(range[0]..=range[1])
            .clamp_existing_to_range(false)
            .custom_formatter(|number, _| field_text(number))
            .custom_parser(move |text| {
                let text = text.trim();
                if text == shown { None } else { text.parse().ok() }
            })
            .update_while_editing(false),
    )
}

#[cfg(test)]
mod tests {
    use uniwow_api::egui;

    use super::{field_text, number_field};

    /// A frame drawing a field of `value`, with `events`; returns whether it changed and its place.
    fn frame(ctx: &egui::Context, value: &mut f64, events: Vec<egui::Event>) -> (bool, egui::Rect) {
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
        let (_, rect) = frame(&ctx, &mut value, Vec::new());
        let at = rect.center();
        frame(&ctx, &mut value, vec![egui::Event::PointerMoved(at), click(at, true)]);
        frame(&ctx, &mut value, vec![click(at, false)]);
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let (changed, _) = frame(&ctx, &mut value, vec![enter]);
        frame(&ctx, &mut value, Vec::new());
        assert!(!changed);
        assert_eq!(value, 1.0487, "neither rounded nor brought back within the range");
    }

    #[test]
    fn numbers_are_shown_with_three_decimals_at_most() {
        assert_eq!(field_text(1.0487), "1.049");
        assert_eq!(field_text(0.5), "0.5");
        assert_eq!(field_text(10.0), "10");
        assert_eq!(field_text(-0.0001), "0");
    }
}
