//! The tools of the 3D view for the scene (milestone 15): the selection by a click, Ctrl toggling
//! and Shift adding, a click again on the same spot going down from the parent to the child; by a
//! rectangle dragged, of the kinds its filter in the toolbar leaves; the entities like the active one
//! selected from the toolbar; the boxes of the selection drawn by the GPU; F frames the selection.
//! Choosing is not a change of the world: nothing enters the history.

mod boxes;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

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

/// What the tool shares with its module.
#[derive(Default)]
struct Shared {
    /// Whether the pointer is over the view, where F frames the selection.
    hovered: AtomicBool,
}

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
}

impl Tool for Selecting {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
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
    scene: Option<scene::Handle>,
    view: Option<viewport::Handle>,
}

impl SceneTools {
    /// Frames the selection in the view: the bounds of all its entities the scene holds.
    fn frame(&self, ctx: &mut Context) {
        let Some(scene) = &self.scene else {
            return;
        };
        let (selection, snapshot) = (scene.selection(), scene.snapshot());
        let boxes = boxes::boxes(&snapshot, &selection.ids);
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
            }),
        );
        self.scene = Some(scene);
        self.view = Some(view);
    }

    fn windows_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        if self.shared.hovered.load(Ordering::Relaxed) && self.frame.as_ref().is_some_and(|key| key.pressed(egui)) {
            self.frame(ctx);
        }
    }

    fn shutdown(&mut self) {
        if let Some(view) = self.view.take() {
            view.remove_tools(MODULE);
            view.remove_layers(MODULE);
        }
    }
}

uniwow_api::export_module!(SceneTools::default());
