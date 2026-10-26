//! The phases of an NPC (step 18.3, as asked after it): named by the designer (Normal, Enraged,
//! Fleeing…), each when it starts and when it ends, and which it gives way to: the next one, or one
//! of several by chance. The skills of SmartAI changing its phase are made by the editor from them
//! (`SMART_ACTION_SET_EVENT_PHASE`, `SMART_ACTION_RANDOM_PHASE`), its phase set back to none when it
//! evades; each skill ticks by their names the phases it acts in. Their course drawn as a frieze.

use uniwow_api::egui::{self, Color32, RichText, Stroke};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::{Context, theme};

use crate::NpcsModule;
use crate::smart::{self, Skill};

/// What makes a phase start or end: an event and its parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct Trigger {
    pub event: i64,
    pub params: [i64; 6],
}

impl Trigger {
    fn to_json(&self) -> Value {
        json!({ "type": self.event, "params": self.params })
    }

    fn of_json(value: &Value) -> Option<Self> {
        let mut params = [0; 6];
        for (at, slot) in params.iter_mut().enumerate() {
            *slot = value["params"].get(at).and_then(Value::as_i64).unwrap_or(0);
        }
        Some(Self {
            event: value["type"].as_i64()?,
            params,
        })
    }
}

/// A phase: its number (1 to 12, the bit of `event_phase_mask`), its name, when it starts, when it
/// ends and which phases follow it (none: no phase; several: one of them by chance).
#[derive(Clone, Debug, PartialEq)]
pub struct Phase {
    pub phase: i64,
    pub name: String,
    pub start: Option<Trigger>,
    pub end: Option<Trigger>,
    pub then: Vec<i64>,
}

impl Phase {
    pub fn to_json(&self) -> Value {
        json!({
            "phase": self.phase,
            "name": self.name,
            "start": self.start.as_ref().map(Trigger::to_json),
            "end": self.end.as_ref().map(Trigger::to_json),
            "then": self.then,
        })
    }

    pub fn of_json(value: &Value) -> Option<Self> {
        Some(Self {
            phase: value["phase"].as_i64().filter(|phase| (1..=12).contains(phase))?,
            name: value["name"].as_str().unwrap_or_default().to_owned(),
            start: Trigger::of_json(&value["start"]),
            end: Trigger::of_json(&value["end"]),
            then: value["then"]
                .as_array()
                .map(|then| then.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default(),
        })
    }

    /// Its name, else "Phase n".
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            format!("Phase {}", self.phase)
        } else {
            self.name.clone()
        }
    }
}

pub fn phases_of(values: &[Value]) -> Vec<Phase> {
    values.iter().filter_map(Phase::of_json).collect()
}

/// The event its phase is set back to none on: its evade.
const EVADE: i64 = 7;
const SET_PHASE: i64 = 22;
const RANDOM_PHASE: i64 = 30;
const SELF: i64 = 1;
/// Once, until it respawns or evades (`SMART_EVENT_FLAG_NOT_REPEATABLE`).
const ONCE: i64 = 0x1;

/// The skills of SmartAI the phases make: each start, out of every phase (a phase entered once, from
/// none); each end, in its phase alone, to the phase that follows or one of them by chance, or none;
/// the phase set back to none at its evade.
pub fn skills(phases: &[Phase]) -> Vec<Skill> {
    let mut out = Vec::new();
    for phase in phases {
        if let Some(start) = &phase.start {
            out.push(Skill {
                event: start.event,
                event_params: start.params,
                phases: 0,
                flags: ONCE,
                action: SET_PHASE,
                action_params: [phase.phase, 0, 0, 0, 0, 0],
                target: SELF,
                comment: format!("{}: starts", phase.label()),
                ..Skill::default()
            });
        }
        if let Some(end) = &phase.end {
            let mut action_params = [0; 6];
            let action = match phase.then.as_slice() {
                [] => SET_PHASE,
                [one] => {
                    action_params[0] = *one;
                    SET_PHASE
                }
                several => {
                    for (slot, next) in action_params.iter_mut().zip(several) {
                        *slot = *next;
                    }
                    RANDOM_PHASE
                }
            };
            out.push(Skill {
                event: end.event,
                event_params: end.params,
                phases: 1 << (phase.phase - 1),
                flags: ONCE,
                action,
                action_params,
                target: SELF,
                comment: format!("{}: ends", phase.label()),
                ..Skill::default()
            });
        }
    }
    if !phases.is_empty() {
        out.push(Skill {
            event: EVADE,
            event_params: [0; 6],
            phases: 0,
            flags: 0,
            action: SET_PHASE,
            action_params: [0; 6],
            target: SELF,
            comment: "Its phase set back to none when it evades".to_owned(),
            ..Skill::default()
        });
    }
    out
}

/// The names of the phases of `phases`, by number, for the skills: "Phase n" for those not named.
pub fn names(phases: &[Phase]) -> [String; 12] {
    std::array::from_fn(|at| {
        phases
            .iter()
            .find(|phase| phase.phase == at as i64 + 1)
            .map_or_else(|| format!("Phase {}", at + 1), Phase::label)
    })
}

impl NpcsModule {
    pub(crate) fn phases(&self) -> Vec<Phase> {
        self.doc
            .as_ref()
            .map(|doc| phases_of(&doc.prefab.phases))
            .unwrap_or_default()
    }

    fn write_phases(&mut self, phases: &[Phase], label: &str, ctx: &mut Context) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let mut after = doc.prefab.clone();
        after.phases = phases.iter().map(Phase::to_json).collect();
        self.change(label.to_owned(), after, ctx);
    }

    /// A trigger edited: its event and its parameters, by their names.
    fn trigger_ui(&mut self, ui: &mut egui::Ui, salt: usize, trigger: &mut Trigger) {
        if let Some(event) = self.event_combo(ui, ("npcs phase trigger", salt), trigger.event) {
            trigger.event = event;
            trigger.params = [0; 6];
        }
        if let Some(spec) = smart::event(trigger.event) {
            let mut values = trigger.params;
            self.event_params_ui(ui, salt, spec, &mut values);
            trigger.params = values;
        }
    }

    /// The card of its phases: their course as a frieze, each named, when it starts and ends, what
    /// follows it, its skills.
    pub(crate) fn phases_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let phases = self.phases();
        let count = phases.len().to_string();
        theme::section(
            ui,
            egui::Id::new("npcs phases"),
            theme::icon::HISTORY,
            "Phases",
            Some(&count),
            true,
            |ui| {
                ui.label(
                RichText::new("The stages of its fight: each named, when it starts and when it ends, what follows it. The skills that change its phase are made by the editor; each skill ticks the phases it acts in. Its phase is set back to none when it evades.")
                    .weak()
                    .small(),
            );
                frieze(ui, &phases);
                let mut changed: Option<(Vec<Phase>, String)> = None;
                let skills = self.skills_list();
                let names = names(&phases);
                for (at, phase) in phases.iter().enumerate() {
                    let mut edited = phase.clone();
                    egui::CollapsingHeader::new(RichText::new(phase.label()).strong())
                    .id_salt(("npcs phase", phase.phase))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Name").weak()).on_hover_text("Its name, for you: kept in the prefab, shown in the skills");
                            let key = format!("phase:{}:name", phase.phase);
                            if let Some(typed) = self.typed(ui, &key, &phase.name, 200.0, "Its name: Normal, Enraged, Fleeing…") {
                                edited.name = typed;
                            }
                            if ui.small_button("✖").on_hover_text("Takes the phase away; its skills act in every phase again").clicked() {
                                let mut list = phases.clone();
                                list.remove(at);
                                changed = Some((list, "Take a phase away".to_owned()));
                            }
                        });
                        // when it starts
                        let mut starts = edited.start.is_some();
                        if ui
                            .checkbox(&mut starts, "Starts by itself when…")
                            .on_hover_text("An event makes it enter this phase (from no phase); without it, it is entered only when another phase ends into it")
                            .changed()
                        {
                            edited.start = starts.then_some(Trigger {
                                event: 4,
                                params: [0; 6],
                            });
                        }
                        if let Some(mut start) = edited.start.clone() {
                            egui::Frame::group(ui.style()).show(ui, |ui| self.trigger_ui(ui, phase.phase as usize * 2, &mut start));
                            edited.start = Some(start);
                        }
                        // when it ends
                        let mut ends = edited.end.is_some();
                        if ui
                            .checkbox(&mut ends, "Ends when…")
                            .on_hover_text("An event, while it is in this phase, makes it leave it")
                            .changed()
                        {
                            edited.end = ends.then_some(Trigger {
                                event: 2,
                                params: [0, 50, 0, 0, 0, 0],
                            });
                        }
                        if let Some(mut end) = edited.end.clone() {
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                self.trigger_ui(ui, phase.phase as usize * 2 + 1, &mut end);
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new("then").weak())
                                        .on_hover_text("The phase it goes to; several ticked: one of them by chance; none: no phase");
                                    for other in &phases {
                                        if other.phase == phase.phase {
                                            continue;
                                        }
                                        let mut on = edited.then.contains(&other.phase);
                                        if ui
                                            .checkbox(&mut on, other.label())
                                            .on_hover_text("It goes to this phase; several ticked: one of them by chance (at most six)")
                                            .changed()
                                        {
                                            if on && edited.then.len() < 6 {
                                                edited.then.push(other.phase);
                                            } else {
                                                edited.then.retain(|held| *held != other.phase);
                                            }
                                        }
                                    }
                                    if edited.then.is_empty() {
                                        ui.label(RichText::new("no phase").italics().weak());
                                    }
                                });
                            });
                            edited.end = Some(end);
                        }
                        // its skills
                        ui.label(RichText::new("Its skills").weak())
                            .on_hover_text("The skills acting in this phase; a skill with no phase ticked acts in every phase");
                        if skills.is_empty() {
                            ui.label(RichText::new("No skill yet: add them in the card Skills.").italics().weak());
                        }
                        let bit = 1i64 << (phase.phase - 1);
                        let mut list = skills.clone();
                        let mut touched = false;
                        for (k, skill) in list.iter_mut().enumerate() {
                            let mut on = skill.phases & bit != 0;
                            let sentence = smart::sentence(skill, &crate::skills::named(self));
                            if ui
                                .checkbox(&mut on, RichText::new(format!("{}. {sentence}", k + 1)).small())
                                .on_hover_text("Acts in this phase")
                                .changed()
                            {
                                skill.phases = if on { skill.phases | bit } else { skill.phases & !bit };
                                touched = true;
                            }
                        }
                        if touched {
                            self.write_skills_pub(&list, &format!("Give skills to {}", phase.label()), ctx);
                        }
                    });
                    if edited != *phase && changed.is_none() {
                        let mut list = phases.clone();
                        list[at] = edited;
                        changed = Some((list, "Change a phase".to_owned()));
                    }
                }
                let free = (1..=12).find(|number| !phases.iter().any(|phase| phase.phase == *number));
                if ui
                    .add_enabled(free.is_some(), egui::Button::new("✚ Add a phase"))
                    .on_hover_text("A new stage of its fight, named, with when it starts and ends")
                    .on_disabled_hover_text("Twelve phases at most")
                    .clicked()
                    && let Some(number) = free
                {
                    let mut list = phases.clone();
                    let first = list.is_empty();
                    list.push(Phase {
                        phase: number,
                        name: if first { "Normal".to_owned() } else { String::new() },
                        start: first.then_some(Trigger {
                            event: 4,
                            params: [0; 6],
                        }),
                        end: None,
                        then: Vec::new(),
                    });
                    changed = Some((list, "Add a phase".to_owned()));
                }
                let _ = names;
                if let Some((list, label)) = changed {
                    self.write_phases(&list, &label, ctx);
                }
            },
        );
    }
}

/// The course of the phases as a frieze: each a box, an arrow to what follows it, said by the event
/// that ends it; those starting by themselves marked by the event that starts them.
fn frieze(ui: &mut egui::Ui, phases: &[Phase]) {
    if phases.is_empty() {
        return;
    }
    let event_name = |trigger: &Option<Trigger>| -> String {
        trigger
            .as_ref()
            .and_then(|trigger| smart::event(trigger.event))
            .map_or_else(String::new, |spec| spec.name.to_lowercase())
    };
    let colours = [
        Color32::from_rgb(70, 110, 160),
        Color32::from_rgb(80, 140, 90),
        Color32::from_rgb(150, 100, 70),
        Color32::from_rgb(130, 80, 150),
    ];
    let width = 150.0;
    let gap = 70.0;
    let height = 44.0;
    let total = phases.len() as f32 * (width + gap);
    egui::ScrollArea::horizontal()
        .id_salt("npcs phases frieze")
        .show(ui, |ui| {
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(total.max(ui.available_width()), height + 34.0),
                egui::Sense::hover(),
            );
            let painter = ui.painter_at(rect);
            let place = |at: usize| rect.left() + 8.0 + at as f32 * (width + gap);
            for (at, phase) in phases.iter().enumerate() {
                let left = place(at);
                let body = egui::Rect::from_min_size(egui::pos2(left, rect.top() + 16.0), egui::vec2(width, height));
                painter.rect_filled(body, 6.0, colours[at % colours.len()]);
                painter.text(
                    body.center(),
                    egui::Align2::CENTER_CENTER,
                    phase.label(),
                    egui::FontId::proportional(14.0),
                    Color32::WHITE,
                );
                if phase.start.is_some() {
                    painter.text(
                        egui::pos2(left, rect.top() + 6.0),
                        egui::Align2::LEFT_CENTER,
                        format!("▶ {}", event_name(&phase.start)),
                        egui::FontId::proportional(11.0),
                        ui.visuals().weak_text_color(),
                    );
                }
                if phase.end.is_some() {
                    let said = event_name(&phase.end);
                    for next in &phase.then {
                        if let Some(to) = phases.iter().position(|other| other.phase == *next) {
                            let from = egui::pos2(body.right(), body.center().y);
                            let target = egui::pos2(place(to), body.center().y);
                            let (a, b) = if to > at {
                                (from, target)
                            } else {
                                (
                                    egui::pos2(body.left(), body.bottom()),
                                    egui::pos2(place(to) + width, body.bottom()),
                                )
                            };
                            painter.arrow(a, b - a, Stroke::new(2.0, ui.visuals().text_color()));
                        }
                    }
                    let label = if phase.then.len() > 1 {
                        format!("{said} (by chance)")
                    } else {
                        said
                    };
                    painter.text(
                        egui::pos2(body.right() + 4.0, body.bottom() + 10.0),
                        egui::Align2::LEFT_CENTER,
                        format!("■ {label}"),
                        egui::FontId::proportional(11.0),
                        ui.visuals().weak_text_color(),
                    );
                }
            }
            response.on_hover_text(
                "The course of its phases: ▶ what starts one, ■ what ends one, the arrows where it goes",
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phases_make_the_skills_that_change_them_and_set_them_back_at_evade() {
        let normal = Phase {
            phase: 1,
            name: "Normal".to_owned(),
            start: Some(Trigger {
                event: 4,
                params: [0; 6],
            }),
            end: Some(Trigger {
                event: 2,
                params: [0, 50, 0, 0, 0, 0],
            }),
            then: vec![2],
        };
        let enraged = Phase {
            phase: 2,
            name: String::new(),
            start: None,
            end: Some(Trigger {
                event: 2,
                params: [0, 20, 0, 0, 0, 0],
            }),
            then: vec![1, 3],
        };
        assert_eq!(Phase::of_json(&normal.to_json()), Some(normal.clone()));
        let made = skills(&[normal, enraged.clone()]);
        assert_eq!(made.len(), 4);
        assert_eq!(
            (made[0].event, made[0].action, made[0].action_params[0]),
            (4, SET_PHASE, 1)
        );
        assert_eq!(
            (made[1].phases, made[1].action_params[0]),
            (1, 2),
            "in phase 1 alone, to 2"
        );
        assert_eq!(
            (made[2].action, made[2].action_params[..2].to_vec()),
            (RANDOM_PHASE, vec![1, 3])
        );
        assert_eq!((made[3].event, made[3].action_params[0]), (EVADE, 0));
        assert_eq!(names(&[enraged])[1], "Phase 2");
    }
}
