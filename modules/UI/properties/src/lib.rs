//! The property grid, as the Inspector of Unity: animatable properties, one row each, with its
//! label and a field for its kind: numbers dragged or typed, a colour picked, a box ticked. Only the
//! rows in sight are drawn. Offered as a service; the core has it draw the `PropertyGrid` objects
//! of every language, and writes and records the values it changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::egui::{self, Align, Layout};
use uniwow_api::property_grid::{self, GridChange, GridInput, GridOutput, GridRow, PropertyGrid};
use uniwow_api::{Module, PropertyKind, PropertyValue, Registrar};

/// Height of a row.
const ROW: f32 = 22.0;
/// The share of the width the labels take, and its bounds.
const LABEL_SHARE: f32 = 0.4;
const LABEL_WIDTH: [f32; 2] = [80.0, 220.0];
/// Width of the field of a number.
const FIELD: f32 = 62.0;
/// The least difference a colour picked makes to a number: the picker gives each colour back
/// through another form, a little changed even when only looked at.
const PICKED: f32 = 1e-4;

/// What one grid keeps between frames.
#[derive(Default)]
struct State {
    /// The value being changed, by path: shown instead of the value read until it is done.
    under_way: Option<(String, PropertyValue)>,
}

#[derive(Default)]
struct Grid {
    states: Mutex<HashMap<egui::Id, State>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl PropertyGrid for Grid {
    fn show(&self, ui: &mut egui::Ui, id: egui::Id, input: &GridInput) -> GridOutput {
        let mut states = lock(&self.states);
        let state = states.entry(id).or_default();
        let label_width = (ui.available_width() * LABEL_SHARE).clamp(LABEL_WIDTH[0], LABEL_WIDTH[1]);
        let mut change = GridChange::None;
        let mut shown = 0..0;
        egui::ScrollArea::vertical()
            .id_salt(id)
            .auto_shrink([false, false])
            .show_rows(ui, ROW, input.count, |ui, positions| {
                shown = positions.clone();
                for position in positions {
                    let row = position.checked_sub(input.first).and_then(|at| input.rows.get(at));
                    match row {
                        Some(row) => {
                            if let Some(changed) = row_ui(ui, row, label_width, state) {
                                change = changed;
                            }
                        }
                        None => {
                            ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), egui::Sense::hover());
                        }
                    }
                }
            });
        // Nothing done by the user any more, though no field said the change was done, as when
        // its row left the sight during a drag: it is done.
        if change == GridChange::None
            && idle(ui)
            && let Some((path, value)) = state.under_way.take()
        {
            change = GridChange::Finished { path, value };
        }
        GridOutput { change, shown }
    }

    fn forget(&self, id: egui::Id) {
        lock(&self.states).remove(&id);
    }
}

/// Whether the user does nothing: no button held, no field taking the keyboard.
fn idle(ui: &egui::Ui) -> bool {
    !ui.input(|i| i.pointer.any_down()) && ui.ctx().memory(|memory| memory.focused().is_none())
}

/// A row: its label, then the field of its value; returns what the user did to it.
fn row_ui(ui: &mut egui::Ui, row: &GridRow, label_width: f32, state: &mut State) -> Option<GridChange> {
    let shown = match &state.under_way {
        Some((path, value)) if *path == row.path => Some(*value),
        _ => row.value,
    };
    ui.horizontal(|ui| {
        ui.set_height(ROW);
        ui.allocate_ui_with_layout(
            egui::vec2(label_width, ROW),
            Layout::left_to_right(Align::Center),
            |ui| {
                // The fields of every row line up.
                ui.set_width(label_width);
                ui.add_enabled(row.kind.is_some(), egui::Label::new(&row.label).truncate())
                    .on_hover_text(&row.path);
            },
        );
        let (Some(kind), Some(value)) = (row.kind, shown) else {
            ui.weak(if row.kind.is_none() { "not running" } else { "" });
            return None;
        };
        let (value, changed, done) = field(ui, kind, value, row.range);
        if changed && !done {
            state.under_way = Some((row.path.clone(), value));
            return Some(GridChange::Changing {
                path: row.path.clone(),
                value,
            });
        }
        let under_way = state.under_way.as_ref().is_some_and(|(path, _)| *path == row.path);
        if done && (changed || under_way) {
            state.under_way = None;
            return Some(GridChange::Finished {
                path: row.path.clone(),
                value,
            });
        }
        None
    })
    .inner
}

/// The field of a value of `kind`: returns the value, whether the user changed it this frame, and
/// whether the change is done.
fn field(ui: &mut egui::Ui, kind: PropertyKind, value: PropertyValue, range: [f64; 2]) -> (PropertyValue, bool, bool) {
    match kind {
        PropertyKind::Boolean => {
            let mut on = matches!(value, PropertyValue::Boolean(true));
            let changed = ui.checkbox(&mut on, "").changed();
            (PropertyValue::Boolean(on), changed, changed)
        }
        PropertyKind::Colour => {
            let numbers = value.components();
            let shown = [0, 1, 2].map(|n| numbers.get(n).copied().unwrap_or(0.0).clamp(0.0, 1.0) as f32);
            let mut rgb = shown;
            let response = egui::color_picker::color_edit_button_rgb(ui, &mut rgb).on_hover_text(format!(
                "{}, {}, {}",
                field_text(numbers[0]),
                field_text(numbers[1]),
                field_text(numbers[2])
            ));
            match colour_picked(&numbers, shown, rgb).filter(|_| response.changed()) {
                // Picked by dragging in its window: done once the button is let go.
                Some(picked) => (picked, true, !ui.input(|i| i.pointer.any_down())),
                None => (value, false, false),
            }
        }
        PropertyKind::Number | PropertyKind::Vector => {
            let mut numbers = value.components();
            let speed = ((range[1] - range[0]) / 2000.0).clamp(0.005, 1.0);
            let (mut changed, mut done) = (false, false);
            for number in &mut numbers {
                let response = number_field(ui, number, range, speed);
                changed |= response.changed();
                done |= ended(&response);
            }
            (PropertyValue::from_components(kind, &numbers), changed, done)
        }
    }
}

/// The colour picked in place of `numbers`, shown as `shown`, when the user changed it: only the
/// numbers it changed by more than `PICKED` are taken, the others kept whole.
fn colour_picked(numbers: &[f64], shown: [f32; 3], rgb: [f32; 3]) -> Option<PropertyValue> {
    let changed = [0, 1, 2].map(|n| (rgb[n] - shown[n]).abs() > PICKED);
    if !changed.contains(&true) {
        return None;
    }
    let picked = [0, 1, 2].map(|n| {
        if changed[n] {
            f64::from(rgb[n])
        } else {
            numbers.get(n).copied().unwrap_or(0.0)
        }
    });
    Some(PropertyValue::Colour(picked))
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

/// The field of a number, kept within `range`. Its text, typed or not, is kept once the field loses
/// the keyboard; the text it showed when it took it is no value typed, even rounded.
fn number_field(ui: &mut egui::Ui, value: &mut f64, range: [f64; 2], speed: f64) -> egui::Response {
    let shown = field_text(*value);
    ui.add_sized(
        [FIELD, ROW - 4.0],
        egui::DragValue::new(value)
            .speed(speed)
            .range(range[0]..=range[1])
            .clamp_existing_to_range(false)
            .custom_formatter(|number, _| field_text(number))
            .custom_parser(move |text| typed(text, &shown))
            .update_while_editing(false),
    )
}

#[derive(Default)]
struct PropertiesModule;

impl Module for PropertiesModule {
    fn register(&mut self, reg: &mut Registrar) {
        let grid: Arc<dyn PropertyGrid> = Arc::new(Grid::default());
        reg.provide(property_grid::SERVICE, grid);
    }
}

uniwow_api::export_module!(PropertiesModule);

#[cfg(test)]
mod tests {
    use uniwow_api::egui;
    use uniwow_api::property_grid::{GridChange, GridInput, GridOutput, GridRow, PropertyGrid};
    use uniwow_api::{PropertyKind, PropertyValue};

    use super::{FIELD, Grid, LABEL_WIDTH, ROW, State, colour_picked, field_text, lock, typed};

    fn row(kind: Option<PropertyKind>, value: Option<PropertyValue>) -> GridRow {
        GridRow {
            path: "cube/opacity".to_owned(),
            label: "Opacity".to_owned(),
            kind,
            range: [0.0, 1.0],
            value,
        }
    }

    /// Frames of a grid of 800 by 600 points showing `rows` from the first, each with its events;
    /// returns what each gave, and the state then.
    fn frames(state: State, rows: &[GridRow], count: usize, frames: Vec<Vec<egui::Event>>) -> (Vec<GridOutput>, State) {
        let grid = Grid::default();
        let id = egui::Id::new("grid");
        lock(&grid.states).insert(id, state);
        let ctx = egui::Context::default();
        let mut outputs = Vec::new();
        for events in frames {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
                events,
                ..egui::RawInput::default()
            };
            let mut rendered = ctx.run_ui(input, |ui| {
                let input = GridInput { count, first: 0, rows };
                outputs.push(grid.show(ui, id, &input));
            });
            rendered.textures_delta.clear();
        }
        let state = lock(&grid.states).remove(&id).expect("kept");
        (outputs, state)
    }

    /// The middle of the field of the first row, after its label.
    fn field_at() -> egui::Pos2 {
        egui::pos2(LABEL_WIDTH[1] + 8.0 + FIELD / 2.0, ROW / 2.0)
    }

    fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn a_number_dragged_is_under_way_then_done_and_shown_meanwhile() {
        let rows = [row(Some(PropertyKind::Number), Some(PropertyValue::Number(0.5)))];
        let at = field_at();
        let to = at + egui::vec2(40.0, 0.0);
        let (outputs, state) = frames(
            State::default(),
            &rows,
            1,
            vec![
                Vec::new(),
                vec![egui::Event::PointerMoved(at), button(at, true)],
                vec![egui::Event::PointerMoved(at + egui::vec2(20.0, 0.0))],
                vec![egui::Event::PointerMoved(to)],
                vec![button(to, false)],
            ],
        );
        let changing: Vec<f64> = outputs
            .iter()
            .filter_map(|output| match &output.change {
                GridChange::Changing {
                    value: PropertyValue::Number(value),
                    ..
                } => Some(*value),
                _ => None,
            })
            .collect();
        assert!(changing.len() >= 2 && changing.is_sorted(), "{changing:?}");
        assert!(
            changing[1] > changing[0],
            "the value dragged is shown, not the one read: {changing:?}"
        );
        let GridChange::Finished {
            path,
            value: PropertyValue::Number(done),
        } = &outputs[4].change
        else {
            panic!("{:?}", outputs[4].change)
        };
        assert_eq!((path.as_str(), *done), ("cube/opacity", *changing.last().unwrap()));
        assert!(state.under_way.is_none());
        assert_eq!(outputs[0].shown, 0..1);
    }

    #[test]
    fn a_box_ticked_is_done_at_once() {
        let rows = [row(Some(PropertyKind::Boolean), Some(PropertyValue::Boolean(false)))];
        let at = egui::pos2(LABEL_WIDTH[1] + 8.0 + 9.0, ROW / 2.0);
        let (outputs, _) = frames(
            State::default(),
            &rows,
            1,
            vec![
                Vec::new(),
                vec![egui::Event::PointerMoved(at), button(at, true)],
                vec![button(at, false)],
            ],
        );
        assert_eq!(
            outputs[2].change,
            GridChange::Finished {
                path: "cube/opacity".to_owned(),
                value: PropertyValue::Boolean(true),
            }
        );
    }

    #[test]
    fn a_property_no_module_declares_is_greyed_and_cannot_change() {
        let rows = [row(None, None)];
        let at = field_at();
        let (outputs, _) = frames(
            State::default(),
            &rows,
            1,
            vec![
                Vec::new(),
                vec![egui::Event::PointerMoved(at), button(at, true)],
                vec![button(at, false)],
            ],
        );
        assert!(outputs.iter().all(|output| output.change == GridChange::None));
    }

    #[test]
    fn a_change_left_under_way_is_done_once_the_user_does_nothing() {
        let state = State {
            under_way: Some(("cube/opacity".to_owned(), PropertyValue::Number(0.75))),
        };
        // Its row out of sight, as after a drag that scrolled the grid.
        let (outputs, state) = frames(state, &[], 1000, vec![Vec::new()]);
        assert_eq!(
            outputs[0].change,
            GridChange::Finished {
                path: "cube/opacity".to_owned(),
                value: PropertyValue::Number(0.75),
            }
        );
        assert!(state.under_way.is_none());
    }

    #[test]
    fn rows_not_made_are_drawn_empty_and_the_rows_in_sight_told() {
        let (outputs, _) = frames(State::default(), &[], 100_000, vec![Vec::new()]);
        assert_eq!(outputs[0].change, GridChange::None);
        assert_eq!(outputs[0].shown.start, 0);
        assert!((20..40).contains(&outputs[0].shown.len()), "{:?}", outputs[0].shown);
    }

    #[test]
    fn a_colour_only_looked_at_is_no_change_and_one_picked_keeps_the_numbers_left_alone() {
        let numbers = [0.15, 0.3, 1.5];
        let shown = [0.15_f32, 0.3, 1.0];
        assert_eq!(colour_picked(&numbers, shown, shown), None);
        assert_eq!(
            colour_picked(&numbers, shown, [0.149_999_99, 0.300_000_1, 1.0]),
            None,
            "given back a little changed, as the picker does when it opens"
        );
        assert_eq!(
            colour_picked(&numbers, shown, [0.5, 0.3, 1.0]),
            Some(PropertyValue::Colour([0.5, 0.3, 1.5])),
            "the numbers it did not change kept whole, even beyond 1"
        );
    }

    #[test]
    fn a_field_lets_through_only_finite_numbers_and_shows_three_decimals_at_most() {
        assert_eq!(typed("nan", "1"), None);
        assert_eq!(typed("1", "1"), None, "the text it showed");
        assert_eq!(typed(" 2.5 ", "1"), Some(2.5));
        assert_eq!(field_text(1.0487), "1.049");
        assert_eq!(field_text(-0.0001), "0");
    }
}
