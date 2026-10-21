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

use uniwow_api::glam::{Quat, Vec3};
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

/// A change a drag of a gizmo makes to the selection, from the frame before.
#[derive(Clone, Debug, PartialEq)]
enum Change {
    /// Each one moved by so much in the world.
    Shift(Vec3),
    /// Each one turned about its own origin, by degrees about an axis of the world.
    Turn(Vec3, f32),
    /// Each one scaled by the factor.
    Scale(f32),
    /// The ones turned or scaled about the middle of the selection, where they stand now.
    Place(Vec<(EntityId, Vec3)>),
}

/// What a drag of a gizmo asks of the module, in order: a gesture opened as it is grabbed, the
/// changes made as the pointer moves, the gesture ended once let go; a creature in the world moved
/// live, held or let go.
enum Drag {
    Grabbed(String),
    Moved(Vec<Change>),
    LetGo,
    Live { id: EntityId, position: Vec3, let_go: bool },
}

/// What a gizmo moves: the selection, written by its providers; or a creature in the world, live.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Selection,
    Live(EntityId),
}

/// A gizmo shown: what it moves, where it stands, its parts, whether it is a spawn point's.
struct Shown {
    target: Target,
    frame: gizmo::Frame,
    parts: Vec<gizmo::Part>,
    spawn: bool,
}

/// A handle of a gizmo held: the grab, what it made so far, where the entities of the selection
/// stood, and about what and in which axes it acts.
struct Held {
    grab: gizmo::Grab,
    applied: gizmo::Amount,
    starts: Vec<(EntityId, Vec3)>,
    pivot: gizmo::Pivot,
    space: gizmo::Space,
    /// What it moves, whether it is a spawn point's, and where a creature was last moved live.
    target: Target,
    spawn: bool,
    sent: Option<Vec3>,
}

/// What the tool shares with its module.
#[derive(Default)]
struct Shared {
    /// Whether the pointer is over the view, where F, W, E and R act.
    hovered: AtomicBool,
    /// The gizmo shown, about what and in which axes, and what its drags ask, for the module to
    /// write.
    mode: std::sync::Mutex<gizmo::Mode>,
    pivot: std::sync::Mutex<gizmo::Pivot>,
    space: std::sync::Mutex<gizmo::Space>,
    writes: std::sync::Mutex<Vec<Drag>>,
    /// The turn of the active entity, read from its provider for the axes of `Space::Local`; and
    /// whether to read it again, once a drag changed it.
    local: std::sync::Mutex<Option<(EntityId, Quat)>>,
    reread: AtomicBool,
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

/// The turn of an entity of `provider` whose Transform reads `rotation`: a spawn's about the
/// vertical alone, facing along its X; the others' as their files place them.
fn local_turn(provider: &str, rotation: [f32; 3]) -> Quat {
    if provider == "spawns" {
        Quat::from_rotation_z(rotation[1].to_radians())
    } else {
        uniwow_api::formats::turning(rotation)
    }
}

/// Whether a turn about `axis` is about the vertical, the only one a spawn takes.
fn vertical(axis: Vec3) -> bool {
    axis.z.abs() > 1.0 - 1e-4
}

/// A write of a provider: the provider, the field, the ids, the axis, the change.
type Write = (String, &'static str, Vec<String>, Option<usize>, String);

/// The writes of `change` to the entities of `by_provider`, as their providers take them.
fn written(
    change: &Change,
    by_provider: &std::collections::BTreeMap<String, Vec<String>>,
    snapshot: &scene::Snapshot,
) -> Vec<Write> {
    let takes = |provider: &str, field: &str| {
        WRITTEN
            .iter()
            .any(|(written, fields)| *written == provider && fields.contains(&field))
    };
    let mut writes = Vec::new();
    match change {
        Change::Shift(by) => {
            for (provider, ids) in by_provider.iter().filter(|(provider, _)| takes(provider, "position")) {
                for axis in (0..3).filter(|axis| by[*axis].abs() > 1e-6) {
                    writes.push((
                        provider.clone(),
                        "position",
                        ids.clone(),
                        Some(axis),
                        format!("+{}", by[axis]),
                    ));
                }
            }
        }
        Change::Turn(axis, degrees) => {
            let turn = scene::turn_change(axis.to_array(), *degrees);
            for (provider, ids) in by_provider.iter().filter(|(provider, _)| takes(provider, "rotation")) {
                if provider == "spawns" && !vertical(*axis) {
                    continue;
                }
                writes.push((provider.clone(), "rotation", ids.clone(), None, turn.clone()));
            }
        }
        Change::Scale(factor) => {
            for (provider, ids) in by_provider.iter().filter(|(provider, _)| takes(provider, "scale")) {
                writes.push((provider.clone(), "scale", ids.clone(), None, format!("*{factor}")));
            }
        }
        Change::Place(placed) => {
            for (id, at) in placed {
                let Some(provider) = snapshot.editable(id).map(|entity| entity.provider().to_owned()) else {
                    continue;
                };
                if !takes(&provider, "position") {
                    continue;
                }
                for axis in 0..3 {
                    writes.push((
                        provider.clone(),
                        "position",
                        vec![id.to_string()],
                        Some(axis),
                        format!("{}", at[axis]),
                    ));
                }
            }
        }
    }
    writes
}

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
    /// The handle of a gizmo being dragged; when a creature dragged was last moved live.
    grabbed: Option<Held>,
    sent_at: Option<std::time::Instant>,
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

    /// The gizmo about the selection as it stands: about the middle of its bounds or the pivot of
    /// the active one, in the axes of the world or of the active one. An entity written by its
    /// provider is taken where its provider has it (a creature at its spawn point), one only seen
    /// where it is now.
    fn frame(&self, seen: &viewport::Seen, ids: &[EntityId]) -> Option<gizmo::Frame> {
        let snapshot = self.scene.snapshot();
        let active = self.scene.selection().active();
        let pivot = *held(&self.shared.pivot);
        let now = |id: &EntityId| {
            if snapshot.editable(id).is_some() {
                None
            } else {
                self.scene.position_now(id)
            }
        };
        let at = |id: &EntityId| {
            snapshot
                .editable(id)
                .map(|entity| entity.origin())
                .or_else(|| self.scene.position_now(id))
                .or_else(|| snapshot.get(id).map(|entity| entity.origin()))
                .map(Vec3::from)
        };
        let centre = match (pivot, active) {
            (gizmo::Pivot::Pivot, Some(active)) => at(&active),
            _ => {
                let (boxes, _) = boxes::boxes(&snapshot, ids, now);
                let all = boxes.iter().copied().reduce(|mut all, b| {
                    for axis in 0..3 {
                        all[axis] = all[axis].min(b[axis]);
                        all[axis + 3] = all[axis + 3].max(b[axis + 3]);
                    }
                    all
                });
                all.map(|all| Vec3::new(all[0] + all[3], all[1] + all[4], all[2] + all[5]) / 2.0)
                    .or_else(|| middle(&snapshot, ids))
            }
        }?;
        let turn = match (*held(&self.shared.space), *held(&self.shared.local)) {
            (gizmo::Space::Local, Some((id, turn))) if Some(id) == active => turn,
            _ => Quat::IDENTITY,
        };
        gizmo::Frame::new(seen, centre, turn)
    }

    /// The gizmos of the mode shown about the selection: its own, which writes it; for a creature
    /// selected alone, its own is that of its spawn point, smaller and of the colour of the circle
    /// drawn there, and with *Move* the creature in the world has one too, which moves it live.
    fn shown(&self, mode: gizmo::Mode, seen: &viewport::Seen, ids: &[EntityId], shift: bool) -> Vec<Shown> {
        let snapshot = self.scene.snapshot();
        let creature = match ids {
            [only] if only.kind == Kind::Creature => Some(*only),
            _ => None,
        };
        let mut out = Vec::new();
        if let Some(mut frame) = self.frame(seen, ids) {
            let spawn = creature.is_some_and(|id| snapshot.editable(&id).is_some());
            if spawn {
                frame.size *= gizmo::SPAWN_SIZE;
            }
            let parts = gizmo::parts(mode, seen, &frame, shift, 1.0);
            out.push(Shown {
                target: Target::Selection,
                frame,
                parts: if spawn { gizmo::as_spawn_point(parts) } else { parts },
                spawn,
            });
        }
        if mode == gizmo::Mode::Move
            && let Some(id) = creature
            && let Some(now) = self.scene.position_now(&id)
            && let Some(frame) = gizmo::Frame::new(seen, Vec3::from(now), Quat::IDENTITY)
        {
            out.push(Shown {
                target: Target::Live(id),
                frame,
                parts: gizmo::parts(mode, seen, &frame, shift, 1.0),
                spawn: false,
            });
        }
        out
    }

    /// The gizmos of the mode shown about the selection, as Unity's: drawn, the handle under the
    /// pointer lit, the nearest of them grabbed, dragged; the changes of the selection given to the
    /// module as the pointer moves, a creature in the world moved live ten times a second at most.
    /// Whether it holds the pointer.
    fn gizmo(&mut self, input: &ToolInput<'_>, painter: &egui::Painter) -> bool {
        let mode = *held(&self.shared.mode);
        let response = input.response;
        if mode == gizmo::Mode::Select {
            self.grabbed = None;
            return false;
        }
        let offset = input.rect.min.to_vec2();
        let (pointer, origin, shift, snap, down) = response.ctx.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.press_origin(),
                i.modifiers.shift,
                i.modifiers.command,
                i.pointer.primary_down(),
            )
        });
        let ids = self.scene.groups().expanded(&self.scene.selection().ids);
        // Grabbed.
        if self.grabbed.is_none() {
            let shown = self.shown(mode, &input.seen, &ids, shift);
            if shown.is_empty() {
                return false;
            }
            // The handle nearest to a point, among the gizmos shown.
            let near = |at: egui::Pos2| {
                shown
                    .iter()
                    .enumerate()
                    .filter_map(|(index, gizmo)| {
                        gizmo::nearest(&gizmo.parts, at - offset).map(|(handle, away)| (index, handle, away))
                    })
                    .min_by(|a, b| a.2.0.cmp(&b.2.0).then(a.2.1.total_cmp(&b.2.1)))
            };
            let hovered = pointer.and_then(near);
            if response.drag_started_by(egui::PointerButton::Primary)
                && let Some((index, handle, _)) = origin.and_then(near)
            {
                let chosen = &shown[index];
                let starts = match chosen.target {
                    Target::Selection => {
                        let snapshot = self.scene.snapshot();
                        ids.iter()
                            .filter_map(|id| Some((*id, Vec3::from(snapshot.editable(id)?.origin()))))
                            .collect()
                    }
                    Target::Live(id) => vec![(id, chosen.frame.centre)],
                };
                let start = origin.unwrap_or_default() - offset;
                self.grabbed = Some(Held {
                    grab: gizmo::Grab::new(handle, chosen.frame, &input.seen, start),
                    applied: gizmo::Amount::none(handle),
                    starts,
                    pivot: *held(&self.shared.pivot),
                    space: *held(&self.shared.space),
                    target: chosen.target,
                    spawn: chosen.spawn,
                    sent: None,
                });
                if chosen.target == Target::Selection {
                    let label = match (mode, chosen.spawn) {
                        (gizmo::Mode::Rotate, true) => "Turn the spawn point",
                        (_, true) => "Move the spawn point",
                        (gizmo::Mode::Rotate, false) => "Rotate",
                        (gizmo::Mode::Scale, false) => "Scale",
                        _ => "Move",
                    };
                    held(&self.shared.writes).push(Drag::Grabbed(label.to_owned()));
                }
            } else {
                for (index, gizmo) in shown.iter().enumerate() {
                    let lit = hovered.filter(|(at, _, _)| *at == index).map(|(_, handle, _)| handle);
                    gizmo::draw(painter, &gizmo.parts, offset, lit, None);
                }
                if let (Some(at), Some((index, _, _))) = (pointer, hovered) {
                    let gizmo = &shown[index];
                    hint(painter, at, gizmo.target, gizmo.spawn);
                }
                // A click on a handle is no choice; inside the rings, it is.
                return hovered.is_some_and(|(_, handle, _)| handle != gizmo::Handle::Free);
            }
        }
        let Some(holding) = self.grabbed.as_mut() else {
            return false;
        };
        // Dragged: what the pointer makes now, less what is written already; one undo entry once
        // let go.
        let now = response
            .interact_pointer_pos()
            .or(pointer)
            .unwrap_or(origin.unwrap_or_default())
            - offset;
        let amount = holding.grab.amount(&input.seen, now, snap);
        let centre = holding.grab.frame.centre;
        let orbit = holding.pivot == gizmo::Pivot::Center && holding.starts.len() > 1;
        let mut changes = Vec::new();
        match (amount, holding.applied) {
            (gizmo::Amount::Shift(to), gizmo::Amount::Shift(from)) if (to - from).length() > 1e-5 => {
                changes.push(Change::Shift(to - from));
            }
            (gizmo::Amount::Turn(to), gizmo::Amount::Turn(from)) => {
                let (axis, angle) = (to * from.inverse()).to_axis_angle();
                if angle.abs() > 1e-5 && angle.is_finite() {
                    changes.push(Change::Turn(axis, angle.to_degrees()));
                    if orbit {
                        let placed = holding
                            .starts
                            .iter()
                            .map(|(id, at)| (*id, centre + to * (*at - centre)));
                        changes.push(Change::Place(placed.collect()));
                    }
                }
            }
            (gizmo::Amount::Scale(to), gizmo::Amount::Scale(from)) if (to / from - 1.0).abs() > 1e-5 => {
                changes.push(Change::Scale(to / from));
                if orbit {
                    let placed = holding
                        .starts
                        .iter()
                        .map(|(id, at)| (*id, centre + (*at - centre) * to));
                    changes.push(Change::Place(placed.collect()));
                }
            }
            _ => {}
        }
        if !changes.is_empty() {
            holding.applied = amount;
            if holding.target == Target::Selection {
                held(&self.shared.writes).push(Drag::Moved(changes));
            }
        }
        // A creature in the world: where it is dragged, ten times a second at most.
        let dragged_to = match (holding.target, holding.applied) {
            (Target::Live(id), gizmo::Amount::Shift(by)) => Some((id, centre + by)),
            _ => None,
        };
        if let Some((id, to)) = dragged_to
            && holding.sent != Some(to)
            && self.sent_at.is_none_or(|at| at.elapsed() >= LIVE_EVERY)
        {
            holding.sent = Some(to);
            self.sent_at = Some(std::time::Instant::now());
            held(&self.shared.writes).push(Drag::Live {
                id,
                position: to,
                let_go: false,
            });
        }
        // Drawn as it follows what it drags: moved with it, its axes turned with the entity in its
        // own, its axes of scale stretched.
        let (frame, stretch) = match holding.applied {
            gizmo::Amount::Shift(by) => (holding.grab.frame.moved(by, Quat::IDENTITY), 1.0),
            gizmo::Amount::Turn(turn) if holding.space == gizmo::Space::Local => {
                (holding.grab.frame.moved(Vec3::ZERO, turn), 1.0)
            }
            gizmo::Amount::Turn(_) => (holding.grab.frame, 1.0),
            gizmo::Amount::Scale(factor) => (holding.grab.frame, factor),
        };
        if let Some(slice) = holding.grab.slice(holding.applied) {
            gizmo::draw_slice(painter, &input.seen, &holding.grab.frame, offset, slice);
        }
        let parts = gizmo::parts(mode, &input.seen, &frame, shift, stretch);
        let parts = if holding.spawn {
            gizmo::as_spawn_point(parts)
        } else {
            parts
        };
        gizmo::draw(painter, &parts, offset, None, Some(holding.grab.handle));
        if response.drag_stopped_by(egui::PointerButton::Primary) || !down {
            let target = holding.target;
            self.grabbed = None;
            match (target, dragged_to) {
                (Target::Live(id), Some((_, to))) => {
                    self.sent_at = None;
                    held(&self.shared.writes).push(Drag::Live {
                        id,
                        position: to,
                        let_go: true,
                    });
                }
                (Target::Live(_), None) => {}
                (Target::Selection, _) => {
                    held(&self.shared.writes).push(Drag::LetGo);
                    self.shared.reread.store(true, Ordering::Relaxed);
                }
            }
        }
        true
    }
}

/// How often a creature dragged in the world is moved at most.
const LIVE_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// What the gizmo under the pointer does, beside it: a creature moved in the game, live; a spawn
/// point written to the server once let go.
fn hint(painter: &egui::Painter, at: egui::Pos2, target: Target, spawn: bool) {
    let text = match (target, spawn) {
        (Target::Live(_), _) => {
            "The creature in the game: moved live as it is dragged, nothing written to the database; it moves again once let go"
        }
        (Target::Selection, true) => {
            "Its spawn point: written to the server once let go (one undo entry, Ctrl+Z puts it back); the creature keeps its place and goes home there"
        }
        (Target::Selection, false) => return,
    };
    let font = egui::FontId::proportional(12.0);
    let galley = painter.layout(text.to_owned(), font, egui::Color32::WHITE, 320.0);
    let rect = egui::Rect::from_min_size(at + egui::vec2(16.0, 16.0), galley.size()).expand(4.0);
    painter.rect_filled(rect, 3.0, egui::Color32::from_black_alpha(200));
    painter.galley(rect.min + egui::vec2(4.0, 4.0), galley, egui::Color32::WHITE);
}

impl Tool for Selecting {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut mode = *held(&self.shared.mode);
        for (shown, label, hint) in [
            (gizmo::Mode::Select, "Select", "Select only"),
            (
                gizmo::Mode::Move,
                "Move",
                "Move (W): along an arrow, in the plane of a square, in the view's with Shift; Ctrl: by the yard",
            ),
            (
                gizmo::Mode::Rotate,
                "Rotate",
                "Rotate (E): about a ring, the view's outside, freely inside; Ctrl: by 15 degrees",
            ),
            (
                gizmo::Mode::Scale,
                "Scale",
                "Scale (R): by an axis or the cube of the middle, alike on every axis as the files keep one scale; Ctrl: by tenths",
            ),
        ] {
            ui.selectable_value(&mut mode, shown, label).on_hover_text(hint);
        }
        *held(&self.shared.mode) = mode;
        ui.separator();
        let mut pivot = held(&self.shared.pivot);
        let (text, hint) = match *pivot {
            gizmo::Pivot::Center => (
                "Center",
                "About the middle of the selection, each one turned and scaled about it",
            ),
            gizmo::Pivot::Pivot => (
                "Pivot",
                "About the pivot of the active one, each one turned and scaled about its own",
            ),
        };
        if ui.button(text).on_hover_text(hint).clicked() {
            *pivot = match *pivot {
                gizmo::Pivot::Center => gizmo::Pivot::Pivot,
                gizmo::Pivot::Pivot => gizmo::Pivot::Center,
            };
        }
        drop(pivot);
        let mut space = held(&self.shared.space);
        let (text, hint) = match *space {
            gizmo::Space::Global => ("Global", "In the axes of the world (X)"),
            gizmo::Space::Local => ("Local", "In the axes of the active one (X)"),
        };
        if ui.button(text).on_hover_text(hint).clicked() {
            *space = match *space {
                gizmo::Space::Global => gizmo::Space::Local,
                gizmo::Space::Local => gizmo::Space::Global,
            };
        }
        drop(space);
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
    /// The gesture of the drag of a gizmo under way, taken up at each frame until let go, and its
    /// label.
    dragging: Option<uniwow_api::Gesture>,
    dragging_label: String,
    /// The hotkey of Global and Local.
    space: Option<Hotkey>,
    /// The reading of the turn of the active entity for the axes of Local: the call, the entity,
    /// its provider; and when it was read last.
    reading: Option<(uniwow_api::CallId, EntityId, String)>,
    read_at: Option<std::time::Instant>,
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
                    let label = if count == 1 {
                        label
                    } else {
                        format!("{label} {count} objects")
                    };
                    self.dragging = Some(ctx.begin_gesture(&label));
                    self.dragging_label = label;
                }
                Drag::Moved(changes) => {
                    for change in changes {
                        for (provider, field, ids, axis, change) in written(&change, &by_provider, &snapshot) {
                            // Held while dragged: what is applied to the server at once is, once let go.
                            ctx.call(
                                &format!("{provider}.write"),
                                json!({ "ids": ids, "field": field, "axis": axis, "change": change, "held": true }),
                            );
                        }
                    }
                }
                Drag::LetGo => {
                    // The spawn points and patrols dragged applied to the server, in the gesture.
                    if by_provider.contains_key("spawns") {
                        ctx.call("spawns.commit", json!({ "label": self.dragging_label }));
                    }
                    if let Some(gesture) = self.dragging.take() {
                        ctx.end_gesture(gesture);
                    }
                }
                Drag::Live { id, position, let_go } => {
                    ctx.call(
                        "live-world.move",
                        json!({ "id": id.to_string(), "position": position.to_array(), "let_go": let_go }),
                    );
                }
            }
        }
    }

    /// Reads the turn of the active entity from its provider, for the axes of Local: once it is
    /// chosen, once a drag changed it, and every second, its Inspector changing it too.
    fn read_local(&mut self, ctx: &mut Context) {
        let Some(scene) = &self.scene else {
            return;
        };
        if *held(&self.shared.space) != gizmo::Space::Local || self.reading.is_some() {
            return;
        }
        let Some(active) = scene.selection().active() else {
            return;
        };
        let known = held(&self.shared.local).is_some_and(|(id, _)| id == active);
        let again = self.shared.reread.swap(false, Ordering::Relaxed)
            || self
                .read_at
                .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1));
        if known && !again {
            return;
        }
        let snapshot = scene.snapshot();
        let Some(provider) = snapshot
            .editable(&active)
            .map(|entity| entity.provider().to_owned())
            .filter(|provider| WRITTEN.iter().any(|(written, _)| written == provider))
        else {
            return;
        };
        let call = ctx.call(&format!("{provider}.read"), json!({ "ids": [active.to_string()] }));
        self.reading = Some((call, active, provider));
        self.read_at = Some(std::time::Instant::now());
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
        self.space = Some(reg.hotkey(
            "space",
            "Gizmos in the axes of the world or of the active one",
            HotkeyKind::Press,
            Keys::key(egui::Key::X),
        ));
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
        if let Some((_, id, provider)) = self.reading.take_if(|(reading, _, _)| *reading == call) {
            let rotation = result.ok().and_then(|read| {
                let values = read["entities"][0]["components"]["transform"]["rotation"]
                    .as_array()?
                    .clone();
                Some([0, 1, 2].map(|at| values.get(at).and_then(|value| value.as_f64()).unwrap_or(0.0) as f32))
            });
            *held(&self.shared.local) = rotation.map(|rotation| (id, local_turn(&provider, rotation)));
            return;
        }
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
                sent_at: None,
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
            if self.space.as_ref().is_some_and(|key| key.pressed(egui)) {
                let mut space = held(&self.shared.space);
                *space = match *space {
                    gizmo::Space::Global => gizmo::Space::Local,
                    gizmo::Space::Local => gizmo::Space::Global,
                };
            }
            if self.delete.as_ref().is_some_and(|key| key.pressed(egui)) {
                self.on_selection("delete", "Delete", ctx);
            }
            if self.duplicate.as_ref().is_some_and(|key| key.pressed(egui)) {
                self.on_selection("duplicate", "Duplicate", ctx);
            }
        }
        self.write(ctx);
        self.read_local(ctx);
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
