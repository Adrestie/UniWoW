//! The card of an NPC's skills (step 18.3): its SmartAI as a list of sentences, each an event, an
//! action and a target; the one chosen edited below, every parameter by its name, a spell, a
//! creature, an emote or a text chosen by searching its name; skills ready made; one skill made to
//! follow another at once (`link`).

use uniwow_api::egui::{self, RichText};
use uniwow_api::{Context, theme};

use crate::NpcsModule;
use crate::smart::{self, Kind, Names, Param, Skill, Spec};
use crate::texts;

/// What the module names: the client's spells, emotes, factions and models, the database's
/// creatures, the NPC's own texts.
pub(crate) struct Named<'a> {
    module: &'a NpcsModule,
}

pub(crate) fn named(module: &NpcsModule) -> Named<'_> {
    Named { module }
}

impl Names for Named<'_> {
    fn named(&self, kind: Kind, value: i64) -> Option<String> {
        let client = self.module.client.as_ref();
        let id = u32::try_from(value).ok()?;
        match kind {
            Kind::Spell => {
                let locale = client?.locale.clone();
                client?.spell(id).map(|spell| spell.name(&locale))
            }
            Kind::Emote => client?.emote(id),
            Kind::Faction => {
                let locale = client?.locale.clone();
                client?.faction(id).map(|faction| faction.name(&locale))
            }
            Kind::Display => client?.models.get(&id).cloned(),
            Kind::Creature => self.module.creature_name(id),
            Kind::Item => self.module.item_name(id),
            Kind::Text => {
                let doc = self.module.doc.as_ref()?;
                texts::groups_of(&doc.prefab.texts)
                    .into_iter()
                    .find(|group| group.group == value)
                    .map(|group| group.label())
            }
            _ => None,
        }
    }
}

/// Which part of a skill a parameter is of.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    Event,
    Action,
    Target,
}

impl NpcsModule {
    fn skills(&self) -> Vec<Skill> {
        self.doc
            .as_ref()
            .map(|doc| doc.prefab.skills.iter().map(Skill::of_json).collect())
            .unwrap_or_default()
    }

    fn write_skills(&mut self, skills: &[Skill], label: &str, ctx: &mut Context) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let mut after = doc.prefab.clone();
        after.skills = skills.iter().map(Skill::to_json).collect();
        self.change(label.to_owned(), after, ctx);
    }

    /// The card of its skills: their sentences, the one chosen edited, the skills ready made.
    pub(crate) fn skills_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let skills = self.skills();
        let count = skills.len().to_string();
        theme::section(
            ui,
            egui::Id::new("npcs skills"),
            theme::icon::SLIDERS,
            "Skills (SmartAI)",
            Some(&count),
            true,
            |ui| {
                ui.label(
                RichText::new("What it does by itself: each skill is an event, an action and a target, as AzerothCore's SmartAI runs them (smart_scripts). Its AI is SmartAI once it has one.")
                    .weak()
                    .small(),
            );
                let mut after: Option<(Vec<Skill>, String)> = None;
                let names = Named { module: self };
                let sentences: Vec<String> = skills.iter().map(|skill| smart::sentence(skill, &names)).collect();
                let mut chosen = self.skill_chosen;
                for (at, sentence) in sentences.iter().enumerate() {
                    let follows = at > 0 && skills[at - 1].linked;
                    ui.horizontal(|ui| {
                        let text = if follows {
                            format!(
                                "{}. ↳ then at once: {}",
                                at + 1,
                                sentence.split(" → ").skip(1).collect::<Vec<_>>().join(" → ")
                            )
                        } else {
                            format!("{}. {sentence}", at + 1)
                        };
                        if ui
                            .selectable_label(chosen == Some(at), RichText::new(text).small())
                            .on_hover_text("Edits this skill below")
                            .clicked()
                        {
                            chosen = if chosen == Some(at) { None } else { Some(at) };
                        }
                    });
                }
                if skills.is_empty() {
                    ui.label(
                        RichText::new("No skill: it fights with its blows alone.")
                            .italics()
                            .weak(),
                    );
                }
                ui.horizontal(|ui| {
                    ui.menu_button("✚ Add a skill", |ui| {
                        for (name, hint, preset) in smart::presets() {
                            if ui.button(name).on_hover_text(hint).clicked() {
                                let mut list = skills.clone();
                                list.push(preset);
                                chosen = Some(list.len() - 1);
                                after = Some((list, format!("Add a skill: {}", name.to_lowercase())));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui
                            .button("Blank")
                            .on_hover_text("An event, an action and a target to choose")
                            .clicked()
                        {
                            let mut list = skills.clone();
                            list.push(Skill::default());
                            chosen = Some(list.len() - 1);
                            after = Some((list, "Add a skill".to_owned()));
                            ui.close();
                        }
                    })
                    .response
                    .on_hover_text("A skill ready made, or a blank one");
                });
                self.skill_chosen = chosen.filter(|at| *at < skills.len());
                if let Some(at) = self.skill_chosen {
                    ui.separator();
                    if let Some(edited) = self.skill_ui(ui, &skills, at) {
                        after = Some(edited);
                    }
                }
                if let Some((list, label)) = after {
                    if self.skill_chosen.is_some_and(|at| at >= list.len()) {
                        self.skill_chosen = None;
                    }
                    self.write_skills(&list, &label, ctx);
                }
            },
        );
    }

    /// The skill `at` of `skills` edited; the skills changed, if they are.
    fn skill_ui(&mut self, ui: &mut egui::Ui, skills: &[Skill], at: usize) -> Option<(Vec<Skill>, String)> {
        let skill = skills[at].clone();
        let mut edited = skill.clone();
        let mut out = None;
        let follows = at > 0 && skills[at - 1].linked;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Skill {}", at + 1)).strong());
            if ui
                .add_enabled(at > 0, egui::Button::new("↑"))
                .on_hover_text("Moves it up")
                .clicked()
            {
                let mut list = skills.to_vec();
                list.swap(at, at - 1);
                self.skill_chosen = Some(at - 1);
                out = Some((list, "Move a skill".to_owned()));
            }
            if ui
                .add_enabled(at + 1 < skills.len(), egui::Button::new("↓"))
                .on_hover_text("Moves it down")
                .clicked()
            {
                let mut list = skills.to_vec();
                list.swap(at, at + 1);
                self.skill_chosen = Some(at + 1);
                out = Some((list, "Move a skill".to_owned()));
            }
            if ui.button("Duplicate").on_hover_text("A copy after it").clicked() {
                let mut list = skills.to_vec();
                list.insert(at + 1, skill.clone());
                self.skill_chosen = Some(at + 1);
                out = Some((list, "Duplicate a skill".to_owned()));
            }
            if ui.button("✖ Delete").on_hover_text("Takes the skill away").clicked() {
                let mut list = skills.to_vec();
                list.remove(at);
                if at > 0 && at == list.len() {
                    list[at - 1].linked = false;
                }
                self.skill_chosen = None;
                out = Some((list, "Delete a skill".to_owned()));
            }
        });
        if out.is_some() {
            return out;
        }
        // the event: when
        egui::Frame::group(ui.style()).show(ui, |ui| {
            if follows {
                ui.label(RichText::new(format!("When: right after skill {} (linked)", at)).strong())
                    .on_hover_text(
                        "The skill above follows into this one at once (smart_scripts.link, event SMART_EVENT_LINK)",
                    );
                return;
            }
            ui.label(RichText::new("When").strong());
            if let Some(kind) = self.spec_combo(
                ui,
                ("npcs event", at),
                smart::EVENTS,
                skill.event,
                "The event: what makes the NPC act (smart_scripts.event_type)",
            ) {
                edited.event = kind;
                edited.event_params = [0; 6];
            }
            if let Some(spec) = smart::event(edited.event) {
                let mut values = edited.event_params;
                self.params_ui(ui, (at, Part::Event), spec, &mut values);
                edited.event_params = values;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("chance").weak())
                    .on_hover_text("Its chance to happen each time, in percent (smart_scripts.event_chance)");
                ui.add(egui::DragValue::new(&mut edited.chance).range(1..=100).suffix(" %"))
                    .on_hover_text("Its chance to happen each time (smart_scripts.event_chance)");
                for (bit, name, hint) in smart::EVENT_FLAGS {
                    let mut on = edited.flags & bit != 0;
                    if ui.checkbox(&mut on, *name).on_hover_text(*hint).changed() {
                        edited.flags = if on { edited.flags | bit } else { edited.flags & !bit };
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("in").weak()).on_hover_text(
                    "The phases of its script it happens in; none ticked: every phase (smart_scripts.event_phase_mask)",
                );
                for phase in 1..=12 {
                    let bit = 1i64 << (phase - 1);
                    let mut on = edited.phases & bit != 0;
                    if ui
                        .checkbox(&mut on, phase.to_string())
                        .on_hover_text(format!("In phase {phase} of its script (Set the phase)"))
                        .changed()
                    {
                        edited.phases = if on { edited.phases | bit } else { edited.phases & !bit };
                    }
                }
            });
        });
        // the action: what
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(RichText::new("Does").strong());
            if let Some(kind) = self.spec_combo(
                ui,
                ("npcs action", at),
                smart::ACTIONS,
                skill.action,
                "The action: what the NPC does (smart_scripts.action_type)",
            ) {
                edited.action = kind;
                edited.action_params = [0; 6];
            }
            if let Some(spec) = smart::action(edited.action) {
                let mut values = edited.action_params;
                self.params_ui(ui, (at, Part::Action), spec, &mut values);
                edited.action_params = values;
            }
        });
        // the target: on whom
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(RichText::new("On").strong());
            if let Some(kind) = self.spec_combo(
                ui,
                ("npcs target", at),
                smart::TARGETS,
                skill.target,
                "The target: on whom or where it acts (smart_scripts.target_type)",
            ) {
                edited.target = kind;
                edited.target_params = [0; 4];
            }
            if let Some(spec) = smart::target(edited.target) {
                let mut values = [0i64; 6];
                values[..4].copy_from_slice(&edited.target_params);
                self.params_ui(ui, (at, Part::Target), spec, &mut values);
                edited.target_params.copy_from_slice(&values[..4]);
            }
            if matches!(edited.target, 8 | 202) {
                ui.horizontal(|ui| {
                    for (k, axis) in ["x", "y", "z", "o"].iter().enumerate() {
                        ui.label(RichText::new(*axis).weak())
                            .on_hover_text("The position's coordinate (smart_scripts.target_x/y/z/o)");
                        ui.add(egui::DragValue::new(&mut edited.position[k]).speed(0.1))
                            .on_hover_text("The position's coordinate, in the world (smart_scripts.target_x/y/z/o)");
                    }
                });
            }
        });
        ui.horizontal(|ui| {
            let mut linked = edited.linked;
            if ui
                .add_enabled(
                    at + 1 < skills.len(),
                    egui::Checkbox::new(&mut linked, "The next skill follows at once"),
                )
                .on_hover_text(
                    "The skill below happens right after this one, whatever its own event (smart_scripts.link)",
                )
                .on_disabled_hover_text("Add a skill after this one first")
                .changed()
            {
                edited.linked = linked;
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Note").weak())
                .on_hover_text("A note of yours, written to smart_scripts.comment");
            let key = format!("skill:{at}:comment");
            if let Some(typed) = self.typed(
                ui,
                &key,
                &skill.comment,
                320.0,
                "A note of yours (smart_scripts.comment); the sentence is written when empty",
            ) {
                edited.comment = typed;
            }
        });
        if edited != skill {
            let mut list = skills.to_vec();
            list[at] = edited;
            return Some((list, "Change a skill".to_owned()));
        }
        None
    }

    /// A list of `specs`, each named and said; the one chosen when another is.
    fn spec_combo(
        &mut self,
        ui: &mut egui::Ui,
        salt: impl egui::AsIdSalt,
        specs: &[Spec],
        value: i64,
        hint: &str,
    ) -> Option<i64> {
        let id = ui.make_persistent_id(salt);
        let shown = specs
            .iter()
            .find(|spec| spec.id == value)
            .map_or(format!("#{value}"), |spec| spec.name.to_owned());
        let mut chosen = None;
        egui::ComboBox::from_id_salt(id)
            .selected_text(shown)
            .width(260.0)
            .height(400.0)
            .show_ui(ui, |ui| {
                let search = self.searches.entry(format!("{id:?}")).or_default();
                ui.add(
                    egui::TextEdit::singleline(search)
                        .hint_text("Search")
                        .desired_width(240.0),
                )
                .on_hover_text("The kinds whose name or explanation holds the text");
                let search = search.trim().to_lowercase();
                for spec in specs {
                    if !search.is_empty()
                        && !spec.name.to_lowercase().contains(&search)
                        && !spec.hint.to_lowercase().contains(&search)
                    {
                        continue;
                    }
                    if ui
                        .selectable_label(spec.id == value, spec.name)
                        .on_hover_text(spec.hint)
                        .clicked()
                    {
                        chosen = Some(spec.id);
                    }
                }
            })
            .response
            .on_hover_text(hint);
        chosen.filter(|chosen| *chosen != value)
    }

    /// The parameters of `spec` in a grid, each by its name; `values` changed in place.
    fn params_ui(&mut self, ui: &mut egui::Ui, (at, part): (usize, Part), spec: &Spec, values: &mut [i64; 6]) {
        if spec.params.is_empty() {
            ui.label(RichText::new(spec.hint).weak().small());
            return;
        }
        let column = match part {
            Part::Event => "event_param",
            Part::Action => "action_param",
            Part::Target => "target_param",
        };
        egui::Grid::new(("npcs params", at, part as u8))
            .num_columns(2)
            .spacing([8.0, 3.0])
            .show(ui, |ui| {
                for (k, param) in spec.params.iter().enumerate() {
                    ui.label(RichText::new(param.name).weak()).on_hover_text(format!(
                        "{} (smart_scripts.{column}{})",
                        param.hint,
                        k + 1
                    ));
                    let salt = (at, part as u8, k);
                    if let Some(value) = self.param_ui(ui, salt, param, values[k]) {
                        values[k] = value;
                    }
                    ui.end_row();
                }
            });
    }

    /// One parameter, chosen as its kind wants; its new value.
    fn param_ui(&mut self, ui: &mut egui::Ui, salt: (usize, u8, usize), param: &Param, value: i64) -> Option<i64> {
        let mut out = value;
        match param.kind {
            Kind::Number(unit) => {
                let suffix = if unit.is_empty() {
                    String::new()
                } else {
                    format!(" {unit}")
                };
                ui.add(
                    egui::DragValue::new(&mut out)
                        .range(0..=i64::from(u32::MAX))
                        .suffix(suffix),
                )
                .on_hover_text(param.hint);
            }
            Kind::Bool => {
                let mut on = value != 0;
                if ui.checkbox(&mut on, "").on_hover_text(param.hint).changed() {
                    out = i64::from(on);
                }
            }
            Kind::Choice(values) => {
                let shown = values
                    .iter()
                    .find(|(held, _)| *held == value)
                    .map_or(value.to_string(), |(_, name)| (*name).to_owned());
                egui::ComboBox::from_id_salt(("npcs param", salt))
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for (held, name) in values {
                            ui.selectable_value(&mut out, *held, *name).on_hover_text(param.hint);
                        }
                    })
                    .response
                    .on_hover_text(param.hint);
            }
            Kind::Mask(bits) => {
                ui.horizontal_wrapped(|ui| {
                    for (bit, name) in bits {
                        let mut on = value & bit != 0;
                        if ui.checkbox(&mut on, *name).on_hover_text(param.hint).changed() {
                            out = if on { out | bit } else { out & !bit };
                        }
                    }
                });
            }
            Kind::Phase => {
                egui::ComboBox::from_id_salt(("npcs param", salt))
                    .selected_text(if value == 0 {
                        "none".to_owned()
                    } else {
                        format!("phase {value}")
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut out, 0, "none").on_hover_text("No phase");
                        for phase in 1..=12 {
                            ui.selectable_value(&mut out, phase, format!("phase {phase}"))
                                .on_hover_text(param.hint);
                        }
                    })
                    .response
                    .on_hover_text(param.hint);
            }
            Kind::Phases => {
                ui.horizontal_wrapped(|ui| {
                    for phase in 1..=12 {
                        let bit = 1i64 << (phase - 1);
                        let mut on = value & bit != 0;
                        if ui
                            .checkbox(&mut on, phase.to_string())
                            .on_hover_text(param.hint)
                            .changed()
                        {
                            out = if on { out | bit } else { out & !bit };
                        }
                    }
                });
            }
            Kind::Emote => {
                let shown = self.client.as_ref().and_then(|client| client.emote(value as u32));
                if let Some(chosen) = self.emote_combo(ui, ("npcs param", salt), value, shown) {
                    out = chosen;
                }
            }
            Kind::Text => {
                let groups = self
                    .doc
                    .as_ref()
                    .map(|doc| texts::groups_of(&doc.prefab.texts))
                    .unwrap_or_default();
                let shown = groups
                    .iter()
                    .find(|group| group.group == value)
                    .map_or(format!("group {value}"), |group| group.label());
                egui::ComboBox::from_id_salt(("npcs param", salt))
                    .selected_text(shown)
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        if groups.is_empty() {
                            ui.weak("No text yet: add a group in the card Texts.");
                        }
                        for group in &groups {
                            ui.selectable_value(&mut out, group.group, group.label())
                                .on_hover_text("A group of its texts, one line said by chance");
                        }
                    })
                    .response
                    .on_hover_text(param.hint);
            }
            Kind::Spell => {
                if let Some(chosen) = self.spell_combo(ui, ("npcs param", salt), value) {
                    out = chosen;
                }
            }
            Kind::Creature => {
                if let Some(chosen) = self.creature_combo(ui, ("npcs param", salt), value) {
                    out = chosen;
                }
            }
            Kind::Faction | Kind::Display | Kind::Item | Kind::GameObject | Kind::Quest => {
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut out).range(0..=i64::from(u32::MAX)))
                        .on_hover_text(param.hint);
                    let names = Named { module: self };
                    if let Some(name) = names.named(param.kind, value) {
                        ui.label(RichText::new(name).weak());
                    }
                });
            }
        }
        (out != value).then_some(out)
    }

    /// The spells of the client searched by a name of any language or by id, with their icons; the
    /// one chosen.
    fn spell_combo(&mut self, ui: &mut egui::Ui, salt: impl egui::AsIdSalt, value: i64) -> Option<i64> {
        let client = self.client.clone()?;
        let locale = client.locale.clone();
        let id = ui.make_persistent_id(salt);
        let held = client.spell(value as u32);
        let shown = match (value, held) {
            (0, _) => "any / none".to_owned(),
            (_, Some(spell)) => format!("{}  {}", spell.name(&locale), spell.id),
            _ => value.to_string(),
        };
        let mut chosen = None;
        ui.horizontal(|ui| {
            if let Some(icon) = held.and_then(|spell| self.spell_icon(spell.icon)) {
                ui.image((icon.id(), egui::vec2(18.0, 18.0)));
            }
            egui::ComboBox::from_id_salt(id)
                .selected_text(shown)
                .width(260.0)
                .height(400.0)
                .show_ui(ui, |ui| {
                    let search = self.searches.entry(format!("{id:?}")).or_default();
                    ui.add(egui::TextEdit::singleline(search).hint_text("Search a spell: name, any language, or id").desired_width(240.0))
                        .on_hover_text("The spells of the client whose name, in any language, holds the text, or whose id begins with it");
                    let search = search.trim().to_lowercase();
                    if ui.selectable_label(value == 0, "any / none").on_hover_text("No spell").clicked() {
                        chosen = Some(0);
                    }
                    if search.chars().count() < 2 {
                        ui.weak("Type two letters or more.");
                        return;
                    }
                    let found: Vec<&crate::client::Spell> = client
                        .spells
                        .iter()
                        .filter(|spell| {
                            spell.id.to_string().starts_with(&search)
                                || spell.names.iter().any(|(_, name)| name.to_lowercase().contains(&search))
                        })
                        .take(300)
                        .collect();
                    egui::ScrollArea::vertical().max_height(320.0).show_rows(ui, 22.0, found.len(), |ui, rows| {
                        for spell in &found[rows] {
                            ui.horizontal(|ui| {
                                match self.spell_icon(spell.icon) {
                                    Some(icon) => {
                                        ui.image((icon.id(), egui::vec2(18.0, 18.0)));
                                    }
                                    None => {
                                        ui.add_space(22.0);
                                    }
                                }
                                let rank = if spell.rank.is_empty() { String::new() } else { format!(" ({})", spell.rank) };
                                let names: Vec<String> = spell.names.iter().filter(|(_, n)| !n.is_empty()).map(|(l, n)| format!("{l}: {n}")).collect();
                                if ui
                                    .selectable_label(i64::from(spell.id) == value, format!("{}{rank}  {}", spell.name(&locale), spell.id))
                                    .on_hover_text(names.join("\n"))
                                    .clicked()
                                {
                                    chosen = Some(i64::from(spell.id));
                                }
                            });
                        }
                    });
                })
                .response
                .on_hover_text("A spell of the client, searched by name in any language or by id");
        });
        chosen
    }

    /// The creatures of the database searched by a name of any language or by entry; the one chosen.
    fn creature_combo(&mut self, ui: &mut egui::Ui, salt: impl egui::AsIdSalt, value: i64) -> Option<i64> {
        let id = ui.make_persistent_id(salt);
        let shown = match (value, self.creature_name(value as u32)) {
            (0, _) => "any / none".to_owned(),
            (_, Some(name)) => format!("{name}  {value}"),
            _ => value.to_string(),
        };
        let world = self.world();
        let mut chosen = None;
        egui::ComboBox::from_id_salt(id)
            .selected_text(shown)
            .width(260.0)
            .height(400.0)
            .show_ui(ui, |ui| {
                let Some(world) = world else {
                    ui.weak("The creatures come with the world database.");
                    return;
                };
                let search = self.searches.entry(format!("{id:?}")).or_default();
                ui.add(egui::TextEdit::singleline(search).hint_text("Search a creature: name, any language, or entry").desired_width(240.0))
                    .on_hover_text("The creatures of the database whose name, in any language, holds the text, or whose entry begins with it");
                let search = search.trim().to_lowercase();
                if ui.selectable_label(value == 0, "any / none").on_hover_text("No creature").clicked() {
                    chosen = Some(0);
                }
                if search.chars().count() < 2 {
                    ui.weak("Type two letters or more.");
                    return;
                }
                let found: Vec<&crate::world::Creature> = world
                    .creatures
                    .iter()
                    .filter(|creature| {
                        creature.entry.to_string().starts_with(&search)
                            || creature.names.iter().any(|name| name.to_lowercase().contains(&search))
                    })
                    .take(300)
                    .collect();
                egui::ScrollArea::vertical().max_height(320.0).show_rows(ui, 18.0, found.len(), |ui, rows| {
                    for creature in &found[rows] {
                        if ui
                            .selectable_label(i64::from(creature.entry) == value, format!("{}  {}", creature.name(), creature.entry))
                            .on_hover_text(creature.names.join("\n"))
                            .clicked()
                        {
                            chosen = Some(i64::from(creature.entry));
                        }
                    }
                });
            })
            .response
            .on_hover_text("A creature of the database, searched by name in any language or by entry");
        chosen
    }
}
