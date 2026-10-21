//! The tools of the 3D view for the scene (milestone 15): the selection by a click, Ctrl toggling
//! and Shift adding, a click again on the same spot going down from the parent to the child; by a
//! rectangle dragged, of the kinds its filter in the toolbar leaves; the entities like the active one
//! selected from the toolbar; the boxes of the selection drawn by the GPU; F frames the selection.
//! Choosing is not a change of the world: nothing enters the history.

mod boxes;
mod gizmo;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use uniwow_api::glam::Vec3;
use uniwow_api::hotkey::{Hotkey, HotkeyKind, Keys};
use uniwow_api::scene::{self, Answer, EntityId, Kind, Question, Select};
use uniwow_api::serde_json::json;
use uniwow_api::viewport::{self, Tool, ToolInput};
use uniwow_api::{Context, Editor, Module, Registrar, egui, log};

use boxes::BoxesLayer;

/// The id of the module, the owner of its tool and its layer.
const MODULE: &str = "scene-tools";

/// How near a click must fall to the last one, in points, to go down the chain of what it hit.
const AGAIN: f32 = 4.0;

/// How the selection changes with a choice, by the modifiers held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    Set,
    Toggle,
    Add,
}

impl Way {
    fn of(modifiers: egui::Modifiers) -> Self {
        if modifiers.command {
            Way::Toggle
        } else if modifiers.shift {
            Way::Add
        } else {
            Way::Set
        }
    }

    fn select(self, ids: Vec<EntityId>) -> Select {
        match self {
            Way::Set => Select::Set(ids),
            Way::Toggle => Select::Toggle(ids),
            Way::Add => Select::Add(ids),
        }
    }
}

/// The entity a click chooses in `chain`, the parents of what it hit from the outermost down: the
/// outermost; when it hit the same as the click before (`again`) while one of the chain is the only
/// one selected, the next down from it, past the deepest the outermost again.
pub fn chosen(chain: &[EntityId], selected: &[EntityId], again: bool) -> Option<EntityId> {
    let first = *chain.first()?;
    if again
        && let [only] = selected
        && let Some(at) = chain.iter().position(|id| id == only)
    {
        return Some(chain[(at + 1) % chain.len()]);
    }
    Some(first)
}

/// The kinds a rectangle selects, as its filter in the toolbar offers them.
const FILTERS: [(&str, &[Kind]); 5] = [
    ("Everything", &[]),
    ("Doodads", &[Kind::Doodad]),
    ("Buildings", &[Kind::Building]),
    ("Building doodads", &[Kind::BuildingDoodad]),
    ("Live", &[Kind::Live]),
];

/// A question asked of the scene, its answer to come, with what it was asked for.
enum Asked {
    Click {
        at: egui::Pos2,
        way: Way,
        answer: Receiver<Answer>,
    },
    Rectangle {
        way: Way,
        answer: Receiver<Answer>,
    },
}

/// The changes of a drag of a gizmo, as the providers take them: the field, the axis, the change.
type Writes = Vec<(&'static str, Option<usize>, String)>;

/// What a drag of a gizmo asks of the module, in order: a gesture opened as it is grabbed, the
/// changes made as the pointer moves, the gesture ended once let go.
enum Drag {
    Grabbed(String),
    Moved(Writes),
    LetGo,
}

/// What the tool shares with its module.
#[derive(Default)]
struct Shared {
    /// Whether the pointer is over the view, where F, W, E and R act.
    hovered: AtomicBool,
    /// The gizmo shown, and what its drags ask, for the module to write.
    mode: std::sync::Mutex<gizmo::Mode>,
    writes: std::sync::Mutex<Vec<Drag>>,
    /// Whether the window creating a doodad is asked for.
    create: AtomicBool,
}

/// The window creating a doodad: the models of the archives, found by a job, and the text searched.
#[derive(Default)]
struct Creating {
    open: bool,
    text: String,
    models: Option<Arc<Vec<String>>>,
    listing: Option<uniwow_api::JobId>,
    found: Vec<String>,
    searched: Option<String>,
}

fn held<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The providers whose entities the gizmos write, and the fields of each.
const WRITTEN: [(&str, &[&str]); 3] = [
    ("doodads", &["position", "rotation", "scale"]),
    ("buildings", &["position", "rotation"]),
    ("spawns", &["position", "rotation"]),
];

/// The providers whose entities are duplicated and deleted from the view.
const MADE: [&str; 2] = ["doodads", "spawns"];

/// The tool of the selection.
struct Selecting {
    scene: scene::Handle,
    editor: Editor,
    shared: Arc<Shared>,
    filter: usize,
    asked: Option<Asked>,
    /// Where the rectangle being dragged started.
    dragging: Option<egui::Pos2>,
    /// The last click: where it fell, and the deepest entity it hit.
    last: Option<(egui::Pos2, EntityId)>,
    /// The last refusal of a smart selection, shown in the toolbar.
    said: Option<String>,
    /// The handle of a gizmo being dragged, where the drag started, the middle of the selection
    /// then, and how much of the drag is written already.
    grabbed: Option<(gizmo::Handle, egui::Pos2, Vec3, f32)>,
}

/// The middle of the origins of the entities `ids` the snapshot holds.
fn middle(snapshot: &scene::Snapshot, ids: &[EntityId]) -> Option<Vec3> {
    let origins: Vec<Vec3> = ids
        .iter()
        .filter_map(|id| snapshot.get(id))
        .map(|entity| Vec3::from(entity.origin()))
        .collect();
    (!origins.is_empty()).then(|| origins.iter().sum::<Vec3>() / origins.len() as f32)
}

impl Selecting {
    /// Takes the answer to the question asked, once it came.
    fn answered(&mut self) {
        let Some(asked) = &self.asked else {
            return;
        };
        let answer = match asked {
            Asked::Click { answer, .. } | Asked::Rectangle { answer, .. } => match answer.try_recv() {
                Ok(answer) => answer,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.asked = None;
                    return;
                }
            },
        };
        match (self.asked.take(), answer) {
            (Some(Asked::Click { at, way, .. }), Answer::Picked(hit)) => {
                let Some(hit) = hit else {
                    if way == Way::Set {
                        self.scene.select(Select::Clear);
                    }
                    self.last = None;
                    return;
                };
                let again = self
                    .last
                    .is_some_and(|(before, deepest)| before.distance(at) <= AGAIN && deepest == hit.id);
                let selected = self.scene.selection().ids.clone();
                if let Some(id) = chosen(&hit.chain, &selected, again && way == Way::Set) {
                    self.scene.select(way.select(vec![id]));
                }
                self.last = Some((at, hit.id));
            }
            (Some(Asked::Rectangle { way, .. }), Answer::Within(ids)) => {
                self.scene.select(way.select(ids));
            }
            _ => {}
        }
    }

    fn like(&mut self, by: &str) {
        self.said = self.editor.call("scene.select_like", json!({ "by": by })).err();
    }

    /// The gizmo of the mode shown about the selection: drawn, grabbed, dragged; its change given
    /// to the module once let go. Whether it holds the pointer.
    fn gizmo(&mut self, input: &ToolInput<'_>, painter: &egui::Painter) -> bool {
        let mode = *held(&self.shared.mode);
        let response = input.response;
        let selection = self.scene.selection();
        let snapshot = self.scene.snapshot();
        let centre = match self.grabbed {
            Some((_, _, centre, _)) => Some(centre),
            None => middle(&snapshot, &selection.ids),
        };
        let Some(centre) = centre.filter(|_| mode != gizmo::Mode::Select) else {
            self.grabbed = None;
            return false;
        };
        let local = |at: egui::Pos2| egui::pos2(at.x - input.rect.left(), at.y - input.rect.top());
        let screen = |at: egui::Pos2| egui::pos2(at.x + input.rect.left(), at.y + input.rect.top());
        // Drawn where the selection stands now, moving with it as it is dragged; the drag measured
        // from where it was grabbed.
        let drawn = middle(&snapshot, &selection.ids).unwrap_or(centre);
        let Some(origin) = input.project(drawn) else {
            return false;
        };
        // Drawn.
        match mode {
            gizmo::Mode::Move => {
                for axis in 0..3 {
                    if let Some((_, direction)) = gizmo::along(&input.seen, drawn, axis) {
                        let tip = origin + direction * gizmo::LENGTH;
                        painter.arrow(origin, tip - origin, egui::Stroke::new(3.0, gizmo::colour(axis)));
                    }
                }
            }
            gizmo::Mode::Rotate => {
                let ring: Vec<egui::Pos2> = gizmo::ring(&input.seen, drawn).into_iter().map(screen).collect();
                painter.add(egui::Shape::line(ring, egui::Stroke::new(2.5, gizmo::colour(2))));
            }
            gizmo::Mode::Scale => {
                let corner = origin + egui::vec2(gizmo::LENGTH, -gizmo::LENGTH) * std::f32::consts::FRAC_1_SQRT_2;
                painter.line_segment([origin, corner], egui::Stroke::new(2.0, egui::Color32::WHITE));
                painter.rect_filled(
                    egui::Rect::from_center_size(corner, egui::vec2(12.0, 12.0)),
                    2.0,
                    egui::Color32::WHITE,
                );
            }
            gizmo::Mode::Select => {}
        }
        // Grabbed.
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(at) = response.interact_pointer_pos()
            && let Some(handle) = gizmo::grab(mode, &input.seen, centre, local(at))
        {
            let still = match handle {
                gizmo::Handle::Scale => 1.0,
                _ => 0.0,
            };
            self.grabbed = Some((handle, at, centre, still));
            let label = match handle {
                gizmo::Handle::Axis(_) => "Move",
                gizmo::Handle::Ring => "Rotate",
                gizmo::Handle::Scale => "Scale",
            };
            held(&self.shared.writes).push(Drag::Grabbed(label.to_owned()));
        }
        let Some((handle, start, centre, applied)) = self.grabbed else {
            return false;
        };
        // Dragged: the change made as the pointer moves, what it adds to what is written already;
        // one undo entry once let go.
        let now = response.interact_pointer_pos().unwrap_or(start);
        let snap = response.ctx.input(|i| i.modifiers.command);
        let amount = gizmo::dragged(handle, &input.seen, centre, local(start), local(now), snap);
        let text = match handle {
            gizmo::Handle::Axis(axis) => format!("{} {amount:+.2} yd", ["X", "Y", "Z"][axis]),
            gizmo::Handle::Ring => format!("{amount:+.1}°"),
            gizmo::Handle::Scale => format!("×{amount:.2}"),
        };
        painter.line_segment([start, now], egui::Stroke::new(1.0, egui::Color32::WHITE));
        painter.text(
            now + egui::vec2(12.0, -12.0),
            egui::Align2::LEFT_BOTTOM,
            text,
            egui::FontId::proportional(13.0),
            egui::Color32::WHITE,
        );
        let step = match handle {
            gizmo::Handle::Scale => amount / applied,
            _ => amount - applied,
        };
        let moves = match handle {
            gizmo::Handle::Scale => (step - 1.0).abs() > 1e-5,
            _ => step.abs() > 1e-5,
        };
        if moves {
            held(&self.shared.writes).push(Drag::Moved(gizmo::writes(handle, step)));
            if let Some(grabbed) = self.grabbed.as_mut() {
                grabbed.3 = amount;
            }
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) || !response.ctx.input(|i| i.pointer.primary_down()) {
            self.grabbed = None;
            held(&self.shared.writes).push(Drag::LetGo);
        }
        true
    }
}

impl Tool for Selecting {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut mode = *held(&self.shared.mode);
        for (shown, label, hint) in [
            (gizmo::Mode::Select, "Select", "Select only"),
            (
                gizmo::Mode::Move,
                "Move",
                "Move along the axes of the world (W); Ctrl: by the yard",
            ),
            (
                gizmo::Mode::Rotate,
                "Rotate",
                "Turn about the vertical (E); Ctrl: by 15 degrees",
            ),
            (gizmo::Mode::Scale, "Scale", "Scale each one (R); Ctrl: by tenths"),
        ] {
            ui.selectable_value(&mut mode, shown, label).on_hover_text(hint);
        }
        *held(&self.shared.mode) = mode;
        if ui
            .button("Create…")
            .on_hover_text("A doodad of a model of the archives, on the ground in the middle of the view")
            .clicked()
        {
            self.shared.create.store(true, Ordering::Relaxed);
        }
        ui.separator();
        ui.label("Rectangle:");
        egui::ComboBox::from_id_salt("scene-tools rectangle")
            .selected_text(FILTERS[self.filter].0)
            .show_ui(ui, |ui| {
                for (index, (name, _)) in FILTERS.iter().enumerate() {
                    ui.selectable_value(&mut self.filter, index, *name);
                }
            });
        let any = self.scene.selection().active().is_some();
        if ui
            .add_enabled(any, egui::Button::new("Same model"))
            .on_hover_text("The entities within the scope of the model of the active one")
            .clicked()
        {
            self.like("model");
        }
        if ui
            .add_enabled(any, egui::Button::new("Same kind in its zone"))
            .on_hover_text("The entities within the scope of the kind of the active one, in its zone")
            .clicked()
        {
            self.like("kind-in-zone");
        }
        if let Some(said) = &self.said {
            ui.colored_label(ui.visuals().warn_fg_color, said);
        }
    }

    fn pointer(&mut self, input: &ToolInput<'_>, painter: &egui::Painter) {
        let response = input.response;
        self.shared.hovered.store(response.hovered(), Ordering::Relaxed);
        self.answered();
        if self.gizmo(input, painter) {
            return;
        }
        let modifiers = response.ctx.input(|i| i.modifiers);
        if response.clicked_by(egui::PointerButton::Primary)
            && let Some(at) = response.interact_pointer_pos()
            && let Some(ray) = input.ray_at(at)
        {
            self.asked = Some(Asked::Click {
                at,
                way: Way::of(modifiers),
                answer: self.scene.ask(Question::Pick(ray)),
            });
        }
        if response.drag_started_by(egui::PointerButton::Primary) {
            self.dragging = response.interact_pointer_pos();
        }
        let Some(start) = self.dragging else {
            return;
        };
        let now = response.interact_pointer_pos().unwrap_or(start);
        let rect = egui::Rect::from_two_pos(start, now);
        let [r, g, b, _] = boxes::COLOUR.map(|v| (v * 255.0) as u8);
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_unmultiplied(r, g, b, 32));
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, egui::Color32::from_rgb(r, g, b)),
            egui::StrokeKind::Inside,
        );
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            self.dragging = None;
            let corner = |at: egui::Pos2| [at.x - input.rect.left(), at.y - input.rect.top()];
            let question = Question::Within {
                seen: input.seen,
                rect: [corner(rect.min), corner(rect.max)],
                kinds: FILTERS[self.filter].1.to_vec(),
            };
            self.asked = Some(Asked::Rectangle {
                way: Way::of(modifiers),
                answer: self.scene.ask(question),
            });
        }
    }
}

#[derive(Default)]
struct SceneTools {
    shared: Arc<Shared>,
    frame: Option<Hotkey>,
    /// The hotkeys of the gizmos: Move, Rotate, Scale; of Delete and Duplicate.
    gizmos: Vec<(Hotkey, gizmo::Mode)>,
    delete: Option<Hotkey>,
    duplicate: Option<Hotkey>,
    /// The calls creating doodads, whose answers are selected.
    creating_calls: Vec<uniwow_api::CallId>,
    creating: Creating,
    scene: Option<scene::Handle>,
    view: Option<viewport::Handle>,
    /// The gesture of the drag of a gizmo under way, taken up at each frame until let go.
    dragging: Option<uniwow_api::Gesture>,
}

impl SceneTools {
    /// Calls `command` (`doodads.delete`, `doodads.duplicate`) for the doodads selected, one gesture;
    /// the doodads made selected once answered.
    fn on_selection(&mut self, verb: &str, label: &str, ctx: &mut Context) {
        let Some(scene) = &self.scene else {
            return;
        };
        let (selection, snapshot) = (scene.selection(), scene.snapshot());
        let ids = scene.groups().expanded(&selection.ids);
        let mut by_provider: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for id in &ids {
            if let Some(entity) = snapshot.editable(id).filter(|entity| MADE.contains(&entity.provider())) {
                by_provider
                    .entry(entity.provider().to_owned())
                    .or_default()
                    .push(id.to_string());
            }
        }
        if by_provider.is_empty() {
            return;
        }
        let gesture = ctx.begin_gesture(label);
        for (provider, ids) in by_provider {
            let call = ctx.call(&format!("{provider}.{verb}"), json!({ "ids": ids }));
            if verb == "duplicate" {
                self.creating_calls.push(call);
            }
        }
        ctx.end_gesture(gesture);
        if verb == "delete" {
            scene.select(Select::Clear);
        }
    }

    /// Creates a doodad of the model `file` on the ground in the middle of the view, or 30 yards in
    /// front of the eye.
    fn create(&mut self, file: &str, ctx: &mut Context) {
        let (Some(scene), Some(view)) = (&self.scene, &self.view) else {
            return;
        };
        let Some(seen) = view.last_view() else {
            return;
        };
        let Some(ray) = seen.ray([seen.size[0] / 2.0, seen.size[1] / 2.0]) else {
            return;
        };
        let distance = scene.ground(&ray).unwrap_or(30.0);
        let at = ray.at(distance).to_array();
        let gesture = ctx.begin_gesture("Create a doodad");
        let call = ctx.call("doodads.create", json!({ "file": file, "position": at }));
        ctx.end_gesture(gesture);
        self.creating_calls.push(call);
    }

    /// The window creating a doodad: a search over the models of the archives, listed by a job.
    fn creating_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        if self.shared.create.swap(false, Ordering::Relaxed) {
            self.creating.open = true;
        }
        if !self.creating.open {
            return;
        }
        if self.creating.models.is_none()
            && self.creating.listing.is_none()
            && let Some(vfs) = ctx.service(uniwow_api::vfs::SERVICE)
        {
            self.creating.listing = Some(ctx.spawn("List the models of the archives", move |_| {
                let mut models: Vec<String> = vfs
                    .files_under("")
                    .into_iter()
                    .filter(|path| {
                        let lower = path.to_ascii_lowercase();
                        lower.ends_with(".m2") || lower.ends_with(".mdx")
                    })
                    .collect();
                models.sort_by_key(|path| path.to_ascii_lowercase());
                Arc::new(models)
            }));
        }
        let mut open = self.creating.open;
        let mut chosen = None;
        egui::Window::new("Create a doodad")
            .open(&mut open)
            .default_size([420.0, 360.0])
            .show(egui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.creating.text)
                        .hint_text("Search a model: ElwynnTree, Barrel…"),
                );
                let Some(models) = self.creating.models.clone() else {
                    ui.label("Listing the models of the archives…");
                    return;
                };
                let text = self.creating.text.trim().to_lowercase();
                if self.creating.searched.as_ref() != Some(&text) {
                    self.creating.found = models
                        .iter()
                        .filter(|path| text.is_empty() || path.to_lowercase().contains(&text))
                        .take(500)
                        .cloned()
                        .collect();
                    self.creating.searched = Some(text);
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{} shown, of {} models",
                        self.creating.found.len(),
                        models.len()
                    ))
                    .weak(),
                );
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                    ui,
                    18.0,
                    self.creating.found.len(),
                    |ui, range| {
                        for path in &self.creating.found[range] {
                            if ui
                                .selectable_label(false, path)
                                .on_hover_text("Created on the ground in the middle of the view")
                                .clicked()
                            {
                                chosen = Some(path.clone());
                            }
                        }
                    },
                );
            });
        self.creating.open = open;
        if let Some(path) = chosen {
            self.create(&path, ctx);
        }
    }

    /// Makes what the drags of the gizmos ask: a gesture opened when one is grabbed and taken up
    /// at each frame it is held, the changes written into the selection as the pointer moves, over
    /// the providers of its entities; the gesture ended once let go, one undo entry.
    fn write(&mut self, ctx: &mut Context) {
        let Some(scene) = self.scene.clone() else {
            return;
        };
        if let Some(gesture) = self.dragging
            && !ctx.resume_gesture(gesture)
        {
            self.dragging = None;
        }
        let drags = std::mem::take(&mut *held(&self.shared.writes));
        if drags.is_empty() {
            return;
        }
        let (selection, snapshot) = (scene.selection(), scene.snapshot());
        let ids = scene.groups().expanded(&selection.ids);
        let mut by_provider: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for id in &ids {
            if let Some(entity) = snapshot.editable(id) {
                by_provider
                    .entry(entity.provider().to_owned())
                    .or_default()
                    .push(id.to_string());
            }
        }
        for drag in drags {
            match drag {
                Drag::Grabbed(label) => {
                    if let Some(gesture) = self.dragging.take() {
                        ctx.end_gesture(gesture);
                    }
                    let count = ids.len();
                    self.dragging = Some(ctx.begin_gesture(&if count == 1 {
                        label
                    } else {
                        format!("{label} {count} objects")
                    }));
                }
                Drag::Moved(writes) => {
                    for (provider, ids) in &by_provider {
                        let Some((_, fields)) = WRITTEN.iter().find(|(written, _)| written == provider) else {
                            continue;
                        };
                        for (field, axis, change) in &writes {
                            if fields.contains(field) {
                                ctx.call(
                                    &format!("{provider}.write"),
                                    json!({ "ids": ids, "field": field, "axis": axis, "change": change }),
                                );
                            }
                        }
                    }
                }
                Drag::LetGo => {
                    if let Some(gesture) = self.dragging.take() {
                        ctx.end_gesture(gesture);
                    }
                }
            }
        }
    }

    /// Frames the selection in the view: the bounds of all its entities the scene holds.
    fn frame(&self, ctx: &mut Context) {
        let Some(scene) = &self.scene else {
            return;
        };
        let (selection, snapshot) = (scene.selection(), scene.snapshot());
        let (boxes, _) = boxes::boxes(&snapshot, &selection.ids, |id| scene.position_now(id));
        let Some(first) = boxes.first() else {
            return;
        };
        let all = boxes.iter().fold(*first, |mut all, b| {
            for axis in 0..3 {
                all[axis] = all[axis].min(b[axis]);
                all[axis + 3] = all[axis + 3].max(b[axis + 3]);
            }
            all
        });
        let framed = ctx.editor().call(
            "viewport.frame",
            json!({ "min": [all[0], all[1], all[2]], "max": [all[3], all[4], all[5]] }),
        );
        if let Err(reason) = framed {
            log::warn!("the selection is not framed: {reason}");
        }
    }
}

impl Module for SceneTools {
    fn register(&mut self, reg: &mut Registrar) {
        self.frame = Some(reg.hotkey(
            "frame",
            "Frame the selection",
            HotkeyKind::Press,
            Keys::key(egui::Key::F),
        ));
        self.delete = Some(reg.hotkey(
            "delete",
            "Delete the doodads selected",
            HotkeyKind::Press,
            Keys::key(egui::Key::Delete),
        ));
        self.duplicate = Some(reg.hotkey(
            "duplicate",
            "Duplicate the doodads selected",
            HotkeyKind::Press,
            Keys::ctrl(egui::Key::D),
        ));
        self.gizmos = vec![
            (
                reg.hotkey("move", "Move gizmo", HotkeyKind::Press, Keys::key(egui::Key::W)),
                gizmo::Mode::Move,
            ),
            (
                reg.hotkey("rotate", "Rotate gizmo", HotkeyKind::Press, Keys::key(egui::Key::E)),
                gizmo::Mode::Rotate,
            ),
            (
                reg.hotkey("scale", "Scale gizmo", HotkeyKind::Press, Keys::key(egui::Key::R)),
                gizmo::Mode::Scale,
            ),
        ];
    }

    fn on_job(&mut self, job: uniwow_api::JobId, outcome: uniwow_api::JobOutcome, _ctx: &mut Context) {
        if self.creating.listing == Some(job) {
            self.creating.listing = None;
            self.creating.models = Some(outcome.take::<Arc<Vec<String>>>().unwrap_or_default());
        }
    }

    fn on_reply(
        &mut self,
        call: uniwow_api::CallId,
        result: Result<uniwow_api::serde_json::Value, String>,
        _ctx: &mut Context,
    ) {
        if let Some(at) = self.creating_calls.iter().position(|waiting| *waiting == call) {
            self.creating_calls.remove(at);
            if let (Ok(answer), Some(scene)) = (&result, &self.scene) {
                let ids: Vec<EntityId> = answer["ids"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|id| id.as_str()?.parse().ok())
                    .collect();
                if !ids.is_empty() {
                    scene.select(Select::Set(ids));
                }
            }
        }
        match result {
            Ok(answer) => {
                for refused in answer["refused"].as_array().cloned().unwrap_or_default() {
                    log::warn!("the gizmo's change refused: {refused}");
                }
            }
            Err(reason) => log::warn!("the gizmo's change refused: {reason}"),
        }
    }

    fn init(&mut self, ctx: &mut Context) {
        let (Some(view), Some(scene)) = (ctx.service(viewport::SERVICE), ctx.service(scene::SERVICE)) else {
            log::info!("no 3D view or no scene: nothing is selected in the view");
            return;
        };
        view.add_layer(ctx.module_id(), Box::new(BoxesLayer::new(scene.clone(), view.target())));
        view.add_tool(
            ctx.module_id(),
            Box::new(Selecting {
                scene: scene.clone(),
                editor: ctx.editor(),
                shared: self.shared.clone(),
                filter: 0,
                asked: None,
                dragging: None,
                last: None,
                said: None,
                grabbed: None,
            }),
        );
        self.scene = Some(scene);
        self.view = Some(view);
    }

    fn windows_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        if self.shared.hovered.load(Ordering::Relaxed) {
            if self.frame.as_ref().is_some_and(|key| key.pressed(egui)) {
                self.frame(ctx);
            }
            for (key, mode) in &self.gizmos {
                if key.pressed(egui) {
                    let mut shown = held(&self.shared.mode);
                    // Pressed again: back to selecting only.
                    *shown = if *shown == *mode { gizmo::Mode::Select } else { *mode };
                }
            }
            if self.delete.as_ref().is_some_and(|key| key.pressed(egui)) {
                self.on_selection("delete", "Delete", ctx);
            }
            if self.duplicate.as_ref().is_some_and(|key| key.pressed(egui)) {
                self.on_selection("duplicate", "Duplicate", ctx);
            }
        }
        self.write(ctx);
        self.creating_ui(egui, ctx);
    }

    fn shutdown(&mut self) {
        if let Some(view) = self.view.take() {
            view.remove_tools(MODULE);
            view.remove_layers(MODULE);
        }
    }
}

uniwow_api::export_module!(SceneTools::default());
