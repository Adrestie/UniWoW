//! The curve editor, as the Curves view of Unity: a graph of times and values where the keys of
//! curves and the handles of their tangents are edited by hand. Offered to Rust modules as a
//! service; the core also has it draw the `CurveView` objects of every language.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::curve::{
    self, Curve, CurveChange, CurveEditor, CurveOptions, DEFAULT_WEIGHT, ShownCurve, SideMode, TangentMode, TimeAxis,
};
use uniwow_api::egui::{self, Align2, Color32, FontId, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};
use uniwow_api::{Module, Registrar};

/// A key: its curve and its index there.
type KeyRef = (usize, usize);

/// How near the pointer must be to a key or a handle to take it, in pixels.
const REACH: f32 = 7.0;
/// The length of a handle drawn on a key without neighbour on that side, in pixels.
const LONE_HANDLE: f32 = 40.0;
const ZOOM: [f32; 2] = [1e-4, 1e5];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

#[derive(Default)]
enum Gesture {
    #[default]
    None,
    /// The selected keys dragged from `from`, with where they started.
    Move { from: Pos2, start: Vec<(KeyRef, f64, f64)> },
    /// The handle of a tangent dragged.
    Handle { key: KeyRef, side: Side },
    /// A selection box.
    Select { from: Pos2, to: Pos2 },
}

/// What one editor keeps between frames.
#[derive(Default)]
struct State {
    /// The value at the bottom edge, and the pixels a unit of value takes; 0 until fitted.
    bottom: f64,
    pixels_per_value: f32,
    selection: BTreeSet<KeyRef>,
    gesture: Gesture,
    /// The keys the context menu acts on.
    menu: Vec<KeyRef>,
    /// The times and values of the keys as this editor left them, to notice when they change
    /// elsewhere: by an undo, by a module, by a key pressed during a drag.
    left: Vec<Vec<(f64, f64)>>,
}

impl State {
    /// The editor removed or inserted keys itself: a gesture or a menu holding their numbers ends.
    fn keys_renumbered(&mut self) {
        self.gesture = Gesture::None;
        self.menu.clear();
    }
}

fn keys_of(curves: &[ShownCurve]) -> Vec<Vec<(f64, f64)>> {
    curves
        .iter()
        .map(|shown| shown.curve.keys.iter().map(|key| (key.time, key.value)).collect())
        .collect()
}

/// Where times and values are drawn.
#[derive(Clone, Copy)]
struct Graph {
    rect: Rect,
    time: TimeAxis,
    bottom: f64,
    pixels_per_value: f32,
}

impl Graph {
    fn x(&self, time: f64) -> f32 {
        self.rect.left() + ((time - self.time.first) as f32) * self.time.pixels_per_unit
    }

    fn time(&self, x: f32) -> f64 {
        self.time.first + f64::from((x - self.rect.left()) / self.time.pixels_per_unit)
    }

    fn y(&self, value: f64) -> f32 {
        self.rect.bottom() - ((value - self.bottom) as f32) * self.pixels_per_value
    }

    fn value(&self, y: f32) -> f64 {
        self.bottom + f64::from((self.rect.bottom() - y) / self.pixels_per_value)
    }

    fn at(&self, time: f64, value: f64) -> Pos2 {
        egui::pos2(self.x(time), self.y(value))
    }

    /// Shows the times and values of these points, or the time `span` when there are none.
    fn frame(&mut self, points: &[(f64, f64)], span: Option<[f64; 2]>, times_too: bool) {
        let (mut times, mut values) = ([f64::MAX, f64::MIN], [f64::MAX, f64::MIN]);
        for (time, value) in points {
            times = [times[0].min(*time), times[1].max(*time)];
            values = [values[0].min(*value), values[1].max(*value)];
        }
        if points.is_empty() {
            times = span.unwrap_or([0.0, 1.0]);
            values = [0.0, 1.0];
        }
        if times_too {
            let pad = ((times[1] - times[0]) * 0.05).max(1.0);
            self.time.pixels_per_unit =
                (self.rect.width() / (times[1] - times[0] + 2.0 * pad) as f32).clamp(ZOOM[0], ZOOM[1]);
            self.time.first = times[0] - pad;
        }
        let pad = ((values[1] - values[0]) * 0.1).max(0.5);
        self.pixels_per_value =
            (self.rect.height() / (values[1] - values[0] + 2.0 * pad) as f32).clamp(ZOOM[0], ZOOM[1]);
        self.bottom = values[0] - pad;
    }
}

#[derive(Default)]
struct Editor {
    states: Mutex<HashMap<egui::Id, State>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The keys of the visible curves, or of the selection when there is one.
fn key_points(curves: &[ShownCurve], selection: &BTreeSet<KeyRef>) -> Vec<(f64, f64)> {
    let mut points: Vec<(f64, f64)> = selection
        .iter()
        .filter_map(|(c, k)| curves.get(*c).and_then(|shown| shown.curve.keys.get(*k)))
        .map(|key| (key.time, key.value))
        .collect();
    if points.is_empty() {
        points = curves
            .iter()
            .filter(|shown| shown.visible)
            .flat_map(|shown| shown.curve.keys.iter().map(|key| (key.time, key.value)))
            .collect();
    }
    points
}

/// The point of a handle of a key, if the user can drag it: none for the Linear and Constant
/// sides of a broken key.
fn handle_point(curves: &[ShownCurve], (c, k): KeyRef, side: Side, graph: &Graph) -> Option<Pos2> {
    let keys = &curves.get(c)?.curve.keys;
    let key = keys.get(k)?;
    let (tangent, neighbour) = match side {
        Side::Left => (&key.left, k.checked_sub(1).and_then(|n| keys.get(n))),
        Side::Right => (&key.right, keys.get(k + 1)),
    };
    if key.mode == TangentMode::Broken && tangent.mode != SideMode::Free {
        return None;
    }
    let reach = match neighbour {
        Some(other) => (other.time - key.time) * tangent.weight.unwrap_or(DEFAULT_WEIGHT),
        None => {
            let sign = if side == Side::Left { -1.0 } else { 1.0 };
            sign * f64::from(LONE_HANDLE / graph.time.pixels_per_unit)
        }
    };
    Some(graph.at(key.time + reach, key.value + tangent.slope * reach))
}

fn nearest_key(curves: &[ShownCurve], graph: &Graph, at: Pos2) -> Option<KeyRef> {
    let mut best: Option<(f32, KeyRef)> = None;
    for (c, shown) in curves.iter().enumerate().filter(|(_, shown)| shown.visible) {
        for (k, key) in shown.curve.keys.iter().enumerate() {
            let distance = graph.at(key.time, key.value).distance(at);
            if distance <= REACH && best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, (c, k)));
            }
        }
    }
    best.map(|(_, key)| key)
}

fn nearest_handle(
    curves: &[ShownCurve],
    selection: &BTreeSet<KeyRef>,
    graph: &Graph,
    at: Pos2,
) -> Option<(KeyRef, Side)> {
    selection
        .iter()
        .flat_map(|key| [(*key, Side::Left), (*key, Side::Right)])
        .filter_map(|(key, side)| handle_point(curves, key, side, graph).map(|p| (p.distance(at), key, side)))
        .filter(|(distance, _, _)| *distance <= REACH)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, key, side)| (key, side))
}

fn snapped(time: f64, options: &CurveOptions) -> f64 {
    options.snap.map_or(time, |step| (time / step).round() * step)
}

/// Sets the slope of a side to point at `target`, as dragging its handle does: a key set
/// automatically becomes Free Smooth, a broken side becomes Free; a weighted side takes the
/// handle's length.
fn drag_handle(shown: &mut ShownCurve, k: usize, side: Side, target: (f64, f64)) {
    let keys = &mut shown.curve.keys;
    if k >= keys.len() {
        return;
    }
    let neighbour = match side {
        Side::Left => k.checked_sub(1).map(|n| keys[n].time),
        Side::Right => keys.get(k + 1).map(|n| n.time),
    };
    let key = &mut keys[k];
    let reach = target.0 - key.time;
    let outward = if side == Side::Left {
        reach < -1e-9
    } else {
        reach > 1e-9
    };
    if !outward {
        return;
    }
    let slope = ((target.1 - key.value) / reach).clamp(-curve::LIMIT, curve::LIMIT);
    if matches!(
        key.mode,
        TangentMode::ClampedAuto | TangentMode::Auto | TangentMode::Flat
    ) {
        key.mode = TangentMode::FreeSmooth;
    }
    let weight_of =
        |weight: Option<f64>| weight.map(|w| neighbour.map_or(w, |n| (reach / (n - key.time)).clamp(0.01, 1.0)));
    if key.mode == TangentMode::FreeSmooth {
        key.left.slope = slope;
        key.right.slope = slope;
    }
    let tangent = if side == Side::Left {
        &mut key.left
    } else {
        &mut key.right
    };
    if key.mode == TangentMode::Broken {
        tangent.mode = SideMode::Free;
        tangent.slope = slope;
    }
    tangent.weight = weight_of(tangent.weight);
    shown.curve.update_tangents();
}

/// How far in time the keys of `start` may move together, from where they started: never onto or
/// past a key that stays, never before the start of the span.
fn admissible(curves: &[ShownCurve], start: &[(KeyRef, f64, f64)], options: &CurveOptions) -> [f64; 2] {
    let gap = options.snap.unwrap_or(1e-3);
    let moving: BTreeSet<KeyRef> = start.iter().map(|(key, _, _)| *key).collect();
    let (mut low, mut high) = (-2.0 * curve::LIMIT, 2.0 * curve::LIMIT);
    for &((c, _), time, _) in start {
        let Some(shown) = curves.get(c) else {
            continue;
        };
        for (index, other) in shown.curve.keys.iter().enumerate() {
            if moving.contains(&(c, index)) {
                continue;
            }
            if other.time < time {
                low = low.max(other.time - time + gap);
            } else {
                high = high.min(other.time - time - gap);
            }
        }
        if let Some([first, _]) = options.span {
            low = low.max(first - time);
        }
        low = low.max(-curve::LIMIT - time);
        high = high.min(curve::LIMIT - time);
    }
    [low, high]
}

/// Moves the keys of `start` by `dt` in time, as much as they all may, and by `dv` in value, from
/// where they started; the keys of each curve stay in time order, each at a time of its own.
fn move_keys(curves: &mut [ShownCurve], start: &[(KeyRef, f64, f64)], dt: f64, dv: f64, options: &CurveOptions) {
    let [low, high] = admissible(curves, start, options);
    let wanted = options.snap.map_or(dt, |step| (dt / step).round() * step);
    let dt = if low <= high { wanted.clamp(low, high) } else { 0.0 };
    let before: Vec<Curve> = curves.iter().map(|shown| shown.curve.clone()).collect();
    for &((c, k), time, value) in start {
        if let Some(key) = curves.get_mut(c).and_then(|shown| shown.curve.keys.get_mut(k)) {
            key.time = time + dt;
            key.value = (value + dv).clamp(-curve::LIMIT, curve::LIMIT);
        }
    }
    for (shown, before) in curves.iter_mut().zip(before) {
        if shown.curve.check().is_err() {
            shown.curve = before;
        }
        shown.curve.update_tangents();
    }
}

/// Applies a choice of the context menu to `keys`.
fn apply_choice(curves: &mut [ShownCurve], keys: &[KeyRef], choice: Choice) {
    for &(c, k) in keys {
        let Some(key) = curves.get_mut(c).and_then(|shown| shown.curve.keys.get_mut(k)) else {
            continue;
        };
        match choice {
            Choice::Mode(mode) => {
                if mode == TangentMode::FreeSmooth {
                    let slope = (key.left.slope + key.right.slope) / 2.0;
                    key.left.slope = slope;
                    key.right.slope = slope;
                }
                if mode == TangentMode::Broken && key.mode != TangentMode::Broken {
                    key.left.mode = SideMode::Free;
                    key.right.mode = SideMode::Free;
                }
                key.mode = mode;
            }
            Choice::Sides(left, right, side_mode) => {
                if key.mode != TangentMode::Broken {
                    key.mode = TangentMode::Broken;
                    key.left.mode = SideMode::Free;
                    key.right.mode = SideMode::Free;
                }
                if left {
                    key.left.mode = side_mode;
                }
                if right {
                    key.right.mode = side_mode;
                }
            }
            Choice::Weighted(left, right, on) => {
                let weight = on.then_some(DEFAULT_WEIGHT);
                if left {
                    key.left.weight = weight;
                }
                if right {
                    key.right.weight = weight;
                }
            }
        }
    }
    for shown in curves.iter_mut() {
        shown.curve.update_tangents();
    }
}

#[derive(Clone, Copy)]
enum Choice {
    Mode(TangentMode),
    /// The left side, the right side, and their new mode.
    Sides(bool, bool, SideMode),
    /// The left side, the right side, and whether they become weighted.
    Weighted(bool, bool, bool),
}

/// Removes keys, from the last of each curve to the first so that the others keep their index.
fn remove_keys(curves: &mut [ShownCurve], keys: &BTreeSet<KeyRef>) {
    for &(c, k) in keys.iter().rev() {
        if let Some(shown) = curves.get_mut(c)
            && k < shown.curve.keys.len()
        {
            shown.curve.keys.remove(k);
        }
    }
    for shown in curves.iter_mut() {
        shown.curve.update_tangents();
    }
}

/// The menu of a right click on keys; returns the choice made.
fn menu(ui: &mut egui::Ui, curves: &[ShownCurve], keys: &[KeyRef]) -> Option<MenuAction> {
    let Some(&(c, k)) = keys.first() else {
        ui.weak("Right click a key to set its tangents.");
        return None;
    };
    let key = curves.get(c).and_then(|shown| shown.curve.keys.get(k)).copied()?;
    let mut chosen = None;
    for (mode, name) in [
        (TangentMode::ClampedAuto, "Clamped Auto"),
        (TangentMode::Auto, "Auto"),
        (TangentMode::FreeSmooth, "Free Smooth"),
        (TangentMode::Flat, "Flat"),
        (TangentMode::Broken, "Broken"),
    ] {
        if ui.selectable_label(key.mode == mode, name).clicked() {
            chosen = Some(MenuAction::Choice(Choice::Mode(mode)));
        }
    }
    ui.separator();
    for (name, left, right) in [
        ("Left Tangent", true, false),
        ("Right Tangent", false, true),
        ("Both Tangents", true, true),
    ] {
        ui.menu_button(name, |ui| {
            let sides = [left.then_some(key.left), right.then_some(key.right)];
            let all = |test: &dyn Fn(&curve::Side) -> bool| sides.iter().flatten().all(test);
            let broken = key.mode == TangentMode::Broken;
            for (mode, label) in [
                (SideMode::Free, "Free"),
                (SideMode::Linear, "Linear"),
                (SideMode::Constant, "Constant"),
            ] {
                if ui
                    .selectable_label(broken && all(&|side| side.mode == mode), label)
                    .clicked()
                {
                    chosen = Some(MenuAction::Choice(Choice::Sides(left, right, mode)));
                }
            }
            let weighted = all(&|side| side.weight.is_some());
            if ui.selectable_label(weighted, "Weighted").clicked() {
                chosen = Some(MenuAction::Choice(Choice::Weighted(left, right, !weighted)));
            }
        });
    }
    ui.separator();
    if ui.button("Delete Key").clicked() {
        chosen = Some(MenuAction::Delete);
    }
    chosen
}

enum MenuAction {
    Choice(Choice),
    Delete,
}

/// A step of the grid, at least `pixels` apart: 1, 2 or 5 times a power of ten, or a multiple of
/// `snap` when there is one. None for a zoom that is not a positive number.
fn grid_step(pixels_per_unit: f32, pixels: f32, snap: Option<f64>) -> Option<f64> {
    let wanted = f64::from(pixels / pixels_per_unit);
    if !(wanted.is_finite() && wanted > 0.0) {
        return None;
    }
    let power = 10f64.powf(wanted.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * power)
        .find(|step| *step >= wanted)?;
    let step = snap.map_or(step, |snap| (step / snap).ceil().max(1.0) * snap);
    (step.is_finite() && step > 0.0).then_some(step)
}

/// The most lines a grid draws.
const MAX_LINES: usize = 1000;

/// The multiples of `step` from `first` to `last`, counted rather than summed, so that a step lost
/// in the size of the numbers cannot loop for ever.
fn grid_lines(first: f64, last: f64, step: f64) -> Vec<f64> {
    let start = (first / step).floor();
    if !(start.is_finite() && last.is_finite()) {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for index in 0..MAX_LINES {
        let line = (start + index as f64) * step;
        if line > last {
            break;
        }
        lines.push(line);
    }
    lines
}

fn number_label(value: f64, step: f64) -> String {
    let decimals = if step >= 1.0 {
        0
    } else {
        (-step.log10()).ceil() as usize
    };
    format!("{value:.decimals$}")
}

impl CurveEditor for Editor {
    fn show(
        &self,
        ui: &mut egui::Ui,
        id: egui::Id,
        curves: &mut [ShownCurve],
        time: &mut TimeAxis,
        options: &CurveOptions,
    ) -> CurveChange {
        let mut states = lock(&self.states);
        let state = states.entry(id).or_default();
        let size = ui.available_size().max(egui::vec2(120.0, 80.0));
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let mut graph = Graph {
            rect,
            time: *time,
            bottom: state.bottom,
            pixels_per_value: state.pixels_per_value,
        };
        // Keys changed elsewhere: the gesture and the menu would act on keys that are no more.
        if keys_of(curves) != state.left {
            state.gesture = Gesture::None;
            state.menu.clear();
        }
        state.selection.retain(|(c, k)| {
            curves
                .get(*c)
                .is_some_and(|shown| shown.visible && *k < shown.curve.keys.len())
        });
        let usable = |zoom: f32, at: f64| zoom.is_finite() && zoom > 0.0 && at.is_finite();
        let time_usable = usable(graph.time.pixels_per_unit, graph.time.first);
        if !time_usable || !usable(graph.pixels_per_value, graph.bottom) {
            let points = key_points(curves, &BTreeSet::new());
            graph.frame(&points, options.span, !time_usable);
        }
        let mut change = CurveChange::None;
        let command = ui.input(|i| i.modifiers.command);

        // Zooming and scrolling.
        if ui.rect_contains_pointer(rect) {
            let (scroll, zoom, shift, pointer) = ui.input_mut(|i| {
                (
                    std::mem::take(&mut i.smooth_scroll_delta),
                    i.zoom_delta(),
                    i.modifiers.shift,
                    i.pointer.hover_pos(),
                )
            });
            if let Some(pointer) = pointer {
                let (factor, times, values) = if zoom != 1.0 {
                    (zoom, true, false)
                } else {
                    ((f64::from(scroll.x + scroll.y) * 0.003).exp() as f32, !shift, true)
                };
                if factor != 1.0 {
                    let (anchor_time, anchor_value) = (graph.time(pointer.x), graph.value(pointer.y));
                    if times {
                        graph.time.pixels_per_unit = (graph.time.pixels_per_unit * factor).clamp(ZOOM[0], ZOOM[1]);
                        graph.time.first =
                            anchor_time - f64::from((pointer.x - rect.left()) / graph.time.pixels_per_unit);
                    }
                    if values {
                        graph.pixels_per_value = (graph.pixels_per_value * factor).clamp(ZOOM[0], ZOOM[1]);
                        graph.bottom = anchor_value - f64::from((rect.bottom() - pointer.y) / graph.pixels_per_value);
                    }
                }
            }
        }
        if response.dragged_by(PointerButton::Middle) {
            let delta = response.drag_delta();
            graph.time.first -= f64::from(delta.x / graph.time.pixels_per_unit);
            graph.bottom += f64::from(delta.y / graph.pixels_per_value);
        }

        // Pressing on a handle, a key or nothing.
        if response.drag_started_by(PointerButton::Primary)
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            state.gesture = if let Some((key, side)) = nearest_handle(curves, &state.selection, &graph, origin) {
                Gesture::Handle { key, side }
            } else if let Some(key) = nearest_key(curves, &graph, origin) {
                if !state.selection.contains(&key) {
                    if !command {
                        state.selection.clear();
                    }
                    state.selection.insert(key);
                }
                let start = state
                    .selection
                    .iter()
                    .filter_map(|&(c, k)| {
                        let key = curves.get(c)?.curve.keys.get(k)?;
                        Some(((c, k), key.time, key.value))
                    })
                    .collect();
                Gesture::Move { from: origin, start }
            } else {
                if !command {
                    state.selection.clear();
                }
                Gesture::Select {
                    from: origin,
                    to: origin,
                }
            };
        }
        if response.clicked()
            && let Some(at) = response.interact_pointer_pos()
            && nearest_handle(curves, &state.selection, &graph, at).is_none()
        {
            match nearest_key(curves, &graph, at) {
                Some(key) if command => {
                    if !state.selection.remove(&key) {
                        state.selection.insert(key);
                    }
                }
                Some(key) => state.selection = BTreeSet::from([key]),
                None if !command => state.selection.clear(),
                None => {}
            }
        }
        if response.dragged_by(PointerButton::Primary)
            && let Some(pointer) = response.interact_pointer_pos()
        {
            match &mut state.gesture {
                Gesture::Move { from, start } => {
                    let delta = pointer - *from;
                    let (dt, dv) = (
                        f64::from(delta.x / graph.time.pixels_per_unit),
                        -f64::from(delta.y / graph.pixels_per_value),
                    );
                    move_keys(curves, start, dt, dv, options);
                    change = CurveChange::Changing;
                }
                Gesture::Handle { key: (c, k), side } => {
                    let target = (graph.time(pointer.x), graph.value(pointer.y));
                    if let Some(shown) = curves.get_mut(*c) {
                        drag_handle(shown, *k, *side, target);
                        change = CurveChange::Changing;
                    }
                }
                Gesture::Select { to, .. } => *to = pointer,
                Gesture::None => {}
            }
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            match std::mem::take(&mut state.gesture) {
                Gesture::Move { .. } | Gesture::Handle { .. } => change = CurveChange::Finished,
                Gesture::Select { from, to } => {
                    let area = Rect::from_two_pos(from, to);
                    for (c, shown) in curves.iter().enumerate().filter(|(_, shown)| shown.visible) {
                        for (k, key) in shown.curve.keys.iter().enumerate() {
                            if area.contains(graph.at(key.time, key.value)) {
                                state.selection.insert((c, k));
                            }
                        }
                    }
                }
                Gesture::None => {}
            }
        }

        // A double click on a curve adds a key there.
        if response.double_clicked()
            && let Some(at) = response.interact_pointer_pos()
            && nearest_key(curves, &graph, at).is_none()
        {
            let time = snapped(graph.time(at.x), options);
            let target = curves
                .iter()
                .enumerate()
                .filter(|(_, shown)| shown.visible)
                .map(|(c, shown)| (c, (graph.y(shown.curve.evaluate(graph.time(at.x))) - at.y).abs()))
                .filter(|(_, distance)| *distance <= REACH)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((c, _)) = target {
                let value = curves[c].curve.evaluate(time);
                let k = curves[c].curve.set_key(time, value);
                state.selection = BTreeSet::from([(c, k)]);
                state.keys_renumbered();
                change = CurveChange::Finished;
            }
        }

        // The menu of a right click.
        if response.secondary_clicked()
            && let Some(at) = response.interact_pointer_pos()
        {
            if let Some(key) = nearest_key(curves, &graph, at)
                && !state.selection.contains(&key)
            {
                state.selection = BTreeSet::from([key]);
            }
            state.menu = state.selection.iter().copied().collect();
        }
        let keys = state.menu.clone();
        let mut action = None;
        response.context_menu(|ui| {
            action = menu(ui, curves, &keys);
            if action.is_some() {
                ui.close();
            }
        });
        match action {
            Some(MenuAction::Choice(choice)) => {
                apply_choice(curves, &keys, choice);
                change = CurveChange::Finished;
            }
            Some(MenuAction::Delete) => {
                remove_keys(curves, &keys.iter().copied().collect());
                state.selection.clear();
                state.keys_renumbered();
                change = CurveChange::Finished;
            }
            None => {}
        }

        // Delete removes the selected keys, but not under a gesture, which holds their numbers;
        // F frames them, or all the keys.
        let typing = ui.ctx().memory(|memory| memory.focused().is_some());
        if ui.rect_contains_pointer(rect) && !typing {
            let idle = matches!(state.gesture, Gesture::None);
            if idle
                && !state.selection.is_empty()
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
            {
                remove_keys(curves, &state.selection);
                state.selection.clear();
                state.keys_renumbered();
                change = CurveChange::Finished;
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F)) {
                let points = key_points(curves, &state.selection);
                graph.frame(&points, options.span, true);
            }
        }

        draw(ui, curves, state, &graph, options);
        state.left = keys_of(curves);
        *time = graph.time;
        state.bottom = graph.bottom;
        state.pixels_per_value = graph.pixels_per_value;
        change
    }
}

fn draw(ui: &egui::Ui, curves: &[ShownCurve], state: &State, graph: &Graph, options: &CurveOptions) {
    let rect = graph.rect;
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
    if let Some([start, end]) = options.span {
        let outside = Color32::from_black_alpha(28);
        let (left, right) = (graph.x(start), graph.x(end));
        if left > rect.left() {
            painter.rect_filled(Rect::from_x_y_ranges(rect.left()..=left, rect.y_range()), 0.0, outside);
        }
        if right < rect.right() {
            painter.rect_filled(
                Rect::from_x_y_ranges(right..=rect.right(), rect.y_range()),
                0.0,
                outside,
            );
        }
    }

    // The grid, its times along the bottom and its values along the left.
    let line = visuals.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6);
    let label = visuals.weak_text_color();
    let step = grid_step(graph.time.pixels_per_unit, 60.0, options.snap).unwrap_or(f64::NAN);
    for time in grid_lines(graph.time(rect.left()), graph.time(rect.right()), step) {
        let x = graph.x(time);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.0, line),
        );
        painter.text(
            egui::pos2(x + 2.0, rect.bottom() - 2.0),
            Align2::LEFT_BOTTOM,
            number_label(time, step),
            FontId::proportional(10.0),
            label,
        );
    }
    let step = grid_step(graph.pixels_per_value, 28.0, None).unwrap_or(f64::NAN);
    for value in grid_lines(graph.value(rect.bottom()), graph.value(rect.top()), step) {
        let y = graph.y(value);
        let width = if value.abs() < step / 2.0 { 1.5 } else { 1.0 };
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(width, line),
        );
        painter.text(
            egui::pos2(rect.left() + 3.0, y - 1.0),
            Align2::LEFT_BOTTOM,
            number_label(value, step),
            FontId::proportional(10.0),
            label,
        );
    }

    // The curves, sampled every two pixels, then their keys and the handles of the selected ones.
    for shown in curves
        .iter()
        .filter(|shown| shown.visible && !shown.curve.keys.is_empty())
    {
        let colour = Color32::from_rgb(shown.colour[0], shown.colour[1], shown.colour[2]);
        let points: Vec<Pos2> = (0..=(rect.width() / 2.0) as usize)
            .map(|step| {
                let x = rect.left() + step as f32 * 2.0;
                egui::pos2(x, graph.y(shown.curve.evaluate(graph.time(x))))
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(1.6, colour)));
    }
    for (c, shown) in curves.iter().enumerate().filter(|(_, shown)| shown.visible) {
        let colour = Color32::from_rgb(shown.colour[0], shown.colour[1], shown.colour[2]);
        for (k, key) in shown.curve.keys.iter().enumerate() {
            let centre = graph.at(key.time, key.value);
            if state.selection.contains(&(c, k)) {
                for side in [Side::Left, Side::Right] {
                    if let Some(handle) = handle_point(curves, (c, k), side, graph) {
                        painter.line_segment([centre, handle], Stroke::new(1.0, visuals.text_color()));
                        painter.circle_filled(handle, 3.5, visuals.text_color());
                    }
                }
                painter.rect_filled(Rect::from_center_size(centre, Vec2::splat(8.0)), 1.0, Color32::WHITE);
                painter.rect_stroke(
                    Rect::from_center_size(centre, Vec2::splat(8.0)),
                    1.0,
                    Stroke::new(1.5, colour),
                    egui::StrokeKind::Inside,
                );
            } else {
                painter.rect_filled(Rect::from_center_size(centre, Vec2::splat(6.0)), 1.0, colour);
            }
        }
    }
    if let Gesture::Select { from, to } = state.gesture {
        painter.rect(
            Rect::from_two_pos(from, to),
            0.0,
            visuals.selection.bg_fill.gamma_multiply(0.15),
            Stroke::new(1.0, visuals.selection.bg_fill),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(playhead) = options.playhead {
        let x = graph.x(playhead);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.5, Color32::from_rgb(225, 65, 55)),
        );
    }
}

#[derive(Default)]
struct CurvesModule;

impl Module for CurvesModule {
    fn register(&mut self, reg: &mut Registrar) {
        let editor: Arc<dyn CurveEditor> = Arc::new(Editor::default());
        reg.provide(curve::SERVICE, editor);
    }
}

uniwow_api::export_module!(CurvesModule);

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use uniwow_api::curve::{Curve, CurveEditor, CurveOptions, ShownCurve, SideMode, TangentMode, TimeAxis};
    use uniwow_api::egui;

    use super::{
        Choice, Editor, Gesture, MAX_LINES, Side, State, apply_choice, drag_handle, grid_lines, grid_step, keys_of,
        lock, move_keys,
    };

    fn shown(points: &[(f64, f64)]) -> ShownCurve {
        let mut curve = Curve::default();
        for (time, value) in points {
            curve.set_key(*time, *value);
        }
        ShownCurve {
            label: "x".to_owned(),
            colour: [220, 70, 60],
            curve,
            visible: true,
        }
    }

    #[test]
    fn dragging_a_handle_frees_an_automatic_key_and_moves_both_sides() {
        let mut curve = shown(&[(0.0, 0.0), (10.0, 5.0), (20.0, 0.0)]);
        drag_handle(&mut curve, 1, Side::Right, (13.0, 8.0));
        let key = curve.curve.keys[1];
        assert_eq!(key.mode, TangentMode::FreeSmooth);
        assert!((key.right.slope - 1.0).abs() < 1e-9 && (key.left.slope - 1.0).abs() < 1e-9);
        drag_handle(&mut curve, 1, Side::Left, (11.0, 0.0));
        assert!(
            (curve.curve.keys[1].left.slope - 1.0).abs() < 1e-9,
            "a handle pulled across its key is ignored"
        );
    }

    #[test]
    fn a_broken_side_moves_alone_and_a_weighted_one_takes_the_handle_length() {
        let mut curve = shown(&[(0.0, 0.0), (10.0, 5.0), (20.0, 0.0)]);
        apply_choice(
            std::slice::from_mut(&mut curve),
            &[(0, 1)],
            Choice::Weighted(false, true, true),
        );
        apply_choice(
            std::slice::from_mut(&mut curve),
            &[(0, 1)],
            Choice::Sides(false, true, SideMode::Free),
        );
        drag_handle(&mut curve, 1, Side::Right, (15.0, 0.0));
        let key = curve.curve.keys[1];
        assert_eq!(key.mode, TangentMode::Broken);
        assert!((key.right.slope + 1.0).abs() < 1e-9);
        assert_eq!(key.left.slope, 0.0, "the other side stays");
        assert!((key.right.weight.unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn keys_removed_elsewhere_end_the_gesture_and_the_menu_without_panicking() {
        let editor = Editor::default();
        let id = egui::Id::new("curves");
        lock(&editor.states).insert(
            id,
            State {
                selection: BTreeSet::from([(0, 3)]),
                gesture: Gesture::Move {
                    from: egui::Pos2::ZERO,
                    start: vec![((0, 3), 30.0, 3.0)],
                },
                menu: vec![(0, 3)],
                left: vec![vec![(0.0, 0.0), (10.0, 1.0), (20.0, 2.0), (30.0, 3.0)]],
                ..State::default()
            },
        );
        let mut curves = vec![shown(&[(0.0, 0.0), (10.0, 1.0)])];
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.show(ui, id, &mut curves, &mut TimeAxis::default(), &CurveOptions::default());
        });
        output.textures_delta.clear();
        let states = lock(&editor.states);
        let state = &states[&id];
        assert!(matches!(state.gesture, Gesture::None));
        assert!(state.menu.is_empty() && state.selection.is_empty());
    }

    /// Delete pressed with the pointer over the editor, which shows `curves` from `state`.
    fn press_delete(state: State, curves: &mut [ShownCurve]) -> State {
        let editor = Editor::default();
        let id = egui::Id::new("curves");
        lock(&editor.states).insert(id, state);
        let input = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(egui::pos2(100.0, 100.0)),
                egui::Event::Key {
                    key: egui::Key::Delete,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(input, |ui| {
            editor.show(ui, id, curves, &mut TimeAxis::default(), &CurveOptions::default());
        });
        output.textures_delta.clear();
        lock(&editor.states).remove(&id).expect("kept")
    }

    #[test]
    fn delete_does_nothing_while_a_key_is_dragged() {
        let points = [(0.0, 0.0), (10.0, 1.0), (20.0, 5.0), (30.0, 3.0)];
        let mut curves = vec![shown(&points)];
        let state = State {
            selection: BTreeSet::from([(0, 1)]),
            gesture: Gesture::Move {
                from: egui::pos2(100.0, 100.0),
                start: vec![((0, 1), 10.0, 1.0)],
            },
            left: keys_of(&curves),
            ..State::default()
        };
        let state = press_delete(state, &mut curves);
        assert_eq!(curves[0].curve.keys.len(), 4, "the dragged key stays");
        assert!(matches!(state.gesture, Gesture::Move { .. }), "the drag goes on");
    }

    #[test]
    fn keys_the_editor_removes_close_the_menu_holding_their_numbers() {
        let points = [(0.0, 0.0), (10.0, 1.0), (20.0, 5.0), (30.0, 3.0)];
        let mut curves = vec![shown(&points)];
        let state = State {
            selection: BTreeSet::from([(0, 1)]),
            menu: vec![(0, 2)],
            left: keys_of(&curves),
            ..State::default()
        };
        let state = press_delete(state, &mut curves);
        assert_eq!(curves[0].curve.keys.len(), 3);
        assert!(state.menu.is_empty(), "the menu's key 2 is now another key");
    }

    #[test]
    fn keys_moved_together_never_cross_nor_land_on_the_keys_that_stay() {
        let mut curves = vec![shown(&[(0.0, 0.0), (10.0, 1.0), (20.0, 2.0), (25.0, 3.0)])];
        let start = vec![((0, 1), 10.0, 1.0), ((0, 2), 20.0, 2.0)];
        let options = CurveOptions {
            snap: Some(1.0),
            span: Some([0.0, 100.0]),
            ..CurveOptions::default()
        };
        for dt in [14.0, 20.0] {
            move_keys(&mut curves, &start, dt, 0.0, &options);
            let times: Vec<f64> = curves[0].curve.keys.iter().map(|key| key.time).collect();
            assert_eq!(times, vec![0.0, 14.0, 24.0, 25.0], "moved by {dt}");
            assert!(curves[0].curve.evaluate(19.0).is_finite());
        }
        move_keys(&mut curves, &start, -40.0, 0.0, &options);
        let times: Vec<f64> = curves[0].curve.keys.iter().map(|key| key.time).collect();
        assert_eq!(
            times,
            vec![0.0, 1.0, 11.0, 25.0],
            "never onto the first key, nor before the span"
        );
        let mut first = vec![shown(&[(5.0, 0.0), (10.0, 1.0)])];
        move_keys(&mut first, &[((0, 0), 5.0, 0.0)], -9.0, 0.0, &options);
        assert_eq!(first[0].curve.keys[0].time, 0.0, "not before the start of the sequence");
    }

    #[test]
    fn grid_steps_are_round_and_follow_the_snap() {
        assert_eq!(grid_step(10.0, 60.0, None), Some(10.0));
        assert_eq!(grid_step(100.0, 60.0, None), Some(1.0));
        assert_eq!(grid_step(100.0, 28.0, None), Some(0.5));
        assert_eq!(grid_step(300.0, 60.0, Some(1.0)), Some(1.0), "never below a frame");
        assert_eq!(grid_step(f32::NAN, 60.0, None), None);
        assert_eq!(grid_step(0.0, 60.0, None), None);
    }

    #[test]
    fn a_grid_ends_even_when_its_step_is_lost_in_the_size_of_the_numbers() {
        assert_eq!(grid_lines(0.0, 2.0, 0.5), vec![0.0, 0.5, 1.0, 1.5, 2.0]);
        assert!(grid_lines(1e16, 1e16 + 1e6, 1.0).len() <= MAX_LINES);
        assert!(grid_lines(0.0, 1.0, f64::NAN).is_empty());
        assert!(grid_lines(f64::NAN, 1.0, 1.0).is_empty());
    }
}
