//! The sheet of the NPC opened (step 18.1): its file, its name and title in the language chosen,
//! then its cards: level and kind, faction, strength, movement, models, loot, roles and flags. Each
//! field written once left, one undo entry; each mask a box of named flags, each choice a named
//! list, what each does under the pointer.

use uniwow_api::egui::{self, RichText};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::{Context, theme};

use crate::fields::{self, Choice, Flag, Number};
use crate::prefab::{self, Prefab, number};
use crate::{NpcsModule, client};

/// A value as a field shows it.
fn shown(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// How a template of faction stands to the sides, said.
fn stances(faction: &client::Faction) -> String {
    let said = |stance| match stance {
        client::Stance::Friendly => "friendly",
        client::Stance::Neutral => "neutral",
        client::Stance::Hostile => "hostile",
    };
    format!(
        "{} to the Alliance, {} to the Horde",
        said(faction.alliance()),
        said(faction.horde())
    )
}

/// Everything a template of faction is: its names, its stance, its groups.
fn faction_said(faction: &client::Faction, locale: &str) -> String {
    let mut out = format!(
        "{} (template {}, faction {})
",
        faction.name(locale),
        faction.id,
        faction.faction
    );
    out.push_str(&format!(
        "{}
Its groups: {}",
        stances(faction),
        faction.groups()
    ));
    for (held, name) in &faction.names {
        if !name.is_empty() {
            out.push_str(&format!(
                "
{held}: {name}"
            ));
        }
    }
    out
}

/// Copper as gold, silver and copper.
fn coins(copper: i64) -> String {
    let (gold, silver, copper) = (copper / 10000, copper / 100 % 100, copper % 100);
    match (gold, silver) {
        (0, 0) => format!("{copper}c"),
        (0, _) => format!("{silver}s {copper}c"),
        _ => format!("{gold}g {silver}s {copper}c"),
    }
}

impl NpcsModule {
    fn prefab(&self) -> Prefab {
        self.doc.as_ref().map(|doc| doc.prefab.clone()).unwrap_or_default()
    }

    /// The field `key` showing `current`, `hint` saying what it is: typed, written once left; the
    /// text written, if changed. Every text field of the sheet passes here.
    pub(crate) fn typed(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        current: &str,
        width: f32,
        hint: &str,
    ) -> Option<String> {
        let mut text = match &self.typing {
            Some((typing, text)) if typing == key => text.clone(),
            _ => current.to_owned(),
        };
        let response = ui
            .add(egui::TextEdit::singleline(&mut text).desired_width(width))
            .on_hover_text(format!(
                "{hint}\nWritten once you leave the field, one undo entry; saved with the file"
            ));
        if response.changed() {
            self.typing = Some((key.to_owned(), text.clone()));
        }
        if response.lost_focus()
            && let Some((_, typed)) = self.typing.take_if(|(typing, _)| typing == key)
            && typed != current
        {
            return Some(typed);
        }
        None
    }

    /// `typed` written into `column` of the template, a number (whole unless `real`); refused, said.
    fn write_number(&mut self, column: &str, label: &str, typed: &str, real: bool, ctx: &mut Context) {
        let typed = typed.trim().replace(',', ".");
        let value = if typed.is_empty() {
            Some(0.0)
        } else {
            typed.parse::<f64>().ok().filter(|value| value.is_finite())
        };
        let Some(value) = value.filter(|value| real || value.fract() == 0.0) else {
            self.said = Some(format!(
                "{label}: '{typed}' is no {}",
                if real { "number" } else { "whole number" }
            ));
            return;
        };
        let mut after = self.prefab();
        after.template.insert(column.to_owned(), number(value));
        self.change(format!("Set the {} of the NPC", label.to_lowercase()), after, ctx);
    }

    fn write_whole(&mut self, column: &str, value: i64, label: &str, ctx: &mut Context) {
        let mut after = self.prefab();
        after.template.insert(column.to_owned(), json!(value));
        self.change(format!("Set the {} of the NPC", label.to_lowercase()), after, ctx);
    }

    /// The numbers `numbers` in a grid, each its label, its field and what it does.
    fn numbers_ui(&mut self, ui: &mut egui::Ui, salt: &str, numbers: &[Number], ctx: &mut Context) {
        egui::Grid::new(("npcs numbers", salt))
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                for field in numbers {
                    ui.label(RichText::new(field.label).weak()).on_hover_text(field.hint);
                    let current = shown(self.prefab().value(field.column).as_ref());
                    let hint = format!("{} (creature_template.{})", field.hint, field.column);
                    if let Some(typed) = self.typed(ui, field.column, &current, 90.0, &hint) {
                        self.write_number(field.column, field.label, &typed, field.real, ctx);
                    }
                    ui.end_row();
                }
            });
    }

    /// A list of the values of `column`, each named, what it does under the pointer.
    fn choice_ui(
        &mut self,
        ui: &mut egui::Ui,
        column: &str,
        choices: &[Choice],
        (label, hint): (&str, &str),
        ctx: &mut Context,
    ) {
        let value = self.prefab().whole(column);
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).weak())
                .on_hover_text(format!("{hint} (creature_template.{column})"));
            let shown = choices
                .iter()
                .find(|(held, _, _)| *held == value)
                .map_or(format!("{value}"), |(_, name, _)| (*name).to_owned());
            egui::ComboBox::from_id_salt(("npcs choice", column))
                .selected_text(shown)
                .height(320.0)
                .show_ui(ui, |ui| {
                    for (held, name, element) in choices {
                        let said = if element.is_empty() {
                            format!("{column} = {held}")
                        } else {
                            format!("{element} ({column} = {held})")
                        };
                        if ui.selectable_label(*held == value, *name).on_hover_text(said).clicked() {
                            chosen = Some(*held);
                        }
                    }
                })
                .response
                .on_hover_text(format!("{hint} (creature_template.{column})"));
        });
        if let Some(chosen) = chosen.filter(|chosen| *chosen != value) {
            self.write_whole(column, chosen, label, ctx);
        }
    }

    /// A box of the bits of `column`: each ticked or not, its name and what it does; the raw value
    /// beside, read only.
    fn mask_ui(
        &mut self,
        ui: &mut egui::Ui,
        column: &str,
        flags: &[Flag],
        (label, none): (&str, &str),
        ctx: &mut Context,
    ) {
        let value = self.prefab().whole(column);
        let mut changed = None;
        ui.label(RichText::new(label).strong())
            .on_hover_text(format!("{none} (creature_template.{column} = {value})"));
        if flags.iter().all(|flag| value & i64::from(flag.bit) == 0) {
            ui.label(RichText::new(none).italics().weak());
        }
        egui::Grid::new(("npcs mask", column))
            .num_columns(3)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                for (at, flag) in flags.iter().enumerate() {
                    let bit = i64::from(flag.bit);
                    let mut ticked = value & bit != 0;
                    if ui
                        .checkbox(&mut ticked, flag.name)
                        .on_hover_text(format!("{} (bit {:#x} of {column})", flag.hint, flag.bit))
                        .changed()
                    {
                        changed = Some(if ticked { value | bit } else { value & !bit });
                    }
                    if at % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
        if let Some(value) = changed {
            self.write_whole(column, value, label, ctx);
        }
    }

    /// The sheet of the NPC opened.
    pub(crate) fn sheet_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let file = doc
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(|| "not saved".to_owned(), |name| name.to_string_lossy().into_owned());
        let unsaved = doc.unsaved();
        let entry = doc.prefab.entry;
        ui.horizontal_wrapped(|ui| {
            ui.heading(doc.prefab.name("enUS").to_owned());
            ui.label(RichText::new(if unsaved { format!("{file} *") } else { file }).weak())
                .on_hover_text(match &doc.path {
                    Some(path) => format!(
                        "{}{}",
                        path.display(),
                        if unsaved { "; changed since saved" } else { "" }
                    ),
                    None => "Not saved yet: Save gives it a file".to_owned(),
                });
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(unsaved, egui::Button::new("Save"))
                .on_hover_text("Writes the NPC to its file (Save as… the first time)")
                .on_disabled_hover_text("Saved: nothing changed since")
                .clicked()
            {
                self.save_said(false);
            }
            if ui
                .button("Save as…")
                .on_hover_text("Writes the NPC to another file, which it is then")
                .clicked()
            {
                self.save_said(true);
            }
            ui.separator();
            let connected = ctx
                .service(uniwow_api::world_db::SERVICE)
                .is_some_and(|database| matches!(database.state(), uniwow_api::world_db::State::Connected(_)));
            let sent = entry.is_some_and(|entry| self.sent.prefabs.contains_key(&entry));
            if ui
                .add_enabled(connected, egui::Button::new("Place in the world"))
                .on_hover_text("A creature of it on the ground in the middle of the 3D view, moved and copied as any spawn; also: drag it from the panel Prefabs into the view. Its template is sent to the server (written by Apply)")
                .on_disabled_hover_text("Connect the world database first (panel World database)")
                .clicked()
                && let Some(position) = self.middle_of_view(ctx)
            {
                self.place(position, ctx);
            }
            if ui
                .add_enabled(connected && !sent, egui::Button::new("Send to the server"))
                .on_hover_text("Its template, names, models, weapons, skills and texts wait for Apply in Changes for the server; kept as you change it")
                .on_disabled_hover_text(if sent { "Sent: it waits for Apply, kept as you change it" } else { "Connect the world database first (panel World database)" })
                .clicked()
            {
                self.send(ctx);
            }
            ui.label(RichText::new(match entry {
                Some(entry) => format!("Entry {entry}"),
                None => "No entry yet".to_owned(),
            }))
            .on_hover_text(format!(
                "The entry of its template (creature_template.entry), of the editor's block {}-{}, given when it is first sent to the server",
                prefab::RANGE.start(),
                prefab::RANGE.end()
            ));
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            self.names_ui(ui, ctx);
            self.kind_ui(ui, ctx);
            self.faction_ui(ui, ctx);
            theme::section(
                ui,
                egui::Id::new("npcs strength"),
                theme::icon::SLIDERS,
                "Strength",
                None,
                true,
                |ui| {
                    self.numbers_ui(ui, "strength", &fields::STRENGTH, ctx);
                },
            );
            theme::section(
                ui,
                egui::Id::new("npcs movement"),
                theme::icon::SLIDERS,
                "Movement",
                None,
                false,
                |ui| {
                    self.choice_ui(
                        ui,
                        "MovementType",
                        &fields::MOVEMENTS,
                        ("Moves", "How it moves where it is spawned; a spawn can say otherwise"),
                        ctx,
                    );
                    self.numbers_ui(ui, "movement", &fields::MOVEMENT, ctx);
                },
            );
            self.models_ui(ui, ctx);
            self.weapons_ui(ui, ctx);
            self.phases_ui(ui, ctx);
            self.skills_ui(ui, ctx);
            self.texts_ui(ui, ctx);
            self.loot_ui(ui, ctx);
            theme::section(
                ui,
                egui::Id::new("npcs roles"),
                theme::icon::INFO,
                "Roles",
                None,
                true,
                |ui| {
                    self.mask_ui(
                        ui,
                        "npcflag",
                        &fields::NPC_FLAGS,
                        ("What it offers", "None ticked: it offers nothing when clicked"),
                        ctx,
                    );
                },
            );
            theme::section(
                ui,
                egui::Id::new("npcs flags"),
                theme::icon::SLIDERS,
                "Flags",
                None,
                false,
                |ui| {
                    self.mask_ui(
                        ui,
                        "unit_flags",
                        &fields::UNIT_FLAGS,
                        ("Of the unit", "None ticked: an ordinary creature"),
                        ctx,
                    );
                    ui.separator();
                    self.mask_ui(
                        ui,
                        "unit_flags2",
                        &fields::UNIT_FLAGS2,
                        ("Of the unit (more)", "None ticked: an ordinary creature"),
                        ctx,
                    );
                    ui.separator();
                    self.mask_ui(
                        ui,
                        "type_flags",
                        &fields::TYPE_FLAGS,
                        ("Of its kind", "None ticked: what its kind gives"),
                        ctx,
                    );
                    ui.separator();
                    self.mask_ui(
                        ui,
                        "flags_extra",
                        &fields::FLAGS_EXTRA,
                        ("Of the server", "None ticked: no flag of the server's own"),
                        ctx,
                    );
                },
            );
        });
    }

    /// Its name and title in the language chosen, the English beside the others.
    fn names_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let locale = prefab::LOCALES[self.locale];
        theme::section(
            ui,
            egui::Id::new("npcs names"),
            theme::icon::INFO,
            "Name",
            Some(locale),
            true,
            |ui| {
                egui::ComboBox::from_id_salt("npcs locale")
                .selected_text(locale)
                .show_ui(ui, |ui| {
                    for (at, name) in prefab::LOCALES.iter().enumerate() {
                        ui.selectable_value(&mut self.locale, at, *name)
                            .on_hover_text("enUS in creature_template, the others in creature_template_locale");
                    }
                })
                .response
                .on_hover_text("The language of the name and title shown: enUS in creature_template, the others in creature_template_locale");
                let prefab = self.prefab();
                egui::Grid::new("npcs names grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        for (part, label, hint, english_column, locale_column) in [
                            (0, "Name", "Its name, over its head and in its tooltip", "name", "Name"),
                            (
                                1,
                                "Title",
                                "The line under its name, as <Innkeeper>; empty for none",
                                "subname",
                                "Title",
                            ),
                        ] {
                            ui.label(RichText::new(label).weak()).on_hover_text(hint);
                            let current = if part == 0 {
                                prefab.name(locale).to_owned()
                            } else {
                                prefab.title(locale).to_owned()
                            };
                            let place = if locale == "enUS" {
                                format!("creature_template.{english_column}")
                            } else {
                                format!("creature_template_locale.{locale_column}, {locale}")
                            };
                            let key = format!("{locale}:{label}");
                            if let Some(typed) = self.typed(ui, &key, &current, 260.0, &format!("{hint} ({place})")) {
                                let mut after = self.prefab();
                                let names = after.names.entry(locale.to_owned()).or_default();
                                if part == 0 {
                                    names.0 = typed;
                                } else {
                                    names.1 = typed;
                                }
                                self.change(
                                    format!("Write the {} of the NPC ({locale})", label.to_lowercase()),
                                    after,
                                    ctx,
                                );
                            }
                            ui.end_row();
                            if locale != "enUS" && current.is_empty() {
                                let english = if part == 0 {
                                    prefab.name("enUS")
                                } else {
                                    prefab.title("enUS")
                                };
                                if !english.is_empty() {
                                    ui.label("");
                                    ui.label(RichText::new(format!("enUS: {english}")).weak().small().italics());
                                    ui.end_row();
                                }
                            }
                        }
                    });
            },
        );
    }

    /// Its level, class, rank and kind.
    fn kind_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        theme::section(
            ui,
            egui::Id::new("npcs kind"),
            theme::icon::INFO,
            "Level and kind",
            None,
            true,
            |ui| {
                self.numbers_ui(
                    ui,
                    "levels",
                    &[
                        Number {
                            column: "minlevel",
                            label: "Level from",
                            hint: "Its lowest level: each spawn takes one between the two",
                            real: false,
                        },
                        Number {
                            column: "maxlevel",
                            label: "Level to",
                            hint: "Its highest level",
                            real: false,
                        },
                    ],
                    ctx,
                );
                self.choice_ui(
                    ui,
                    "exp",
                    &fields::EXPANSIONS,
                    (
                        "Stats of",
                        "The expansion its health and damage by level are taken from",
                    ),
                    ctx,
                );
                self.choice_ui(
                    ui,
                    "unit_class",
                    &fields::CLASSES,
                    ("Class", "Its class: its health and mana by level"),
                    ctx,
                );
                self.choice_ui(
                    ui,
                    "rank",
                    &fields::RANKS,
                    ("Rank", "The dragon around its portrait, and how strong it is"),
                    ctx,
                );
                self.choice_ui(
                    ui,
                    "type",
                    &fields::TYPES,
                    ("Kind", "What it is: the spells that work on it follow"),
                    ctx,
                );
                self.choice_ui(
                    ui,
                    "family",
                    &fields::FAMILIES,
                    ("Family", "The family of a beast: what a hunter tames"),
                    ctx,
                );
                self.choice_ui(
                    ui,
                    "dmgschool",
                    &fields::SCHOOLS,
                    ("Blows", "The school of its blows in melee"),
                    ctx,
                );
            },
        );
    }

    /// Its faction, searched by name among those of the client.
    fn faction_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let faction = self.prefab().whole("faction");
        let client = self.client.clone();
        let locale = client
            .as_ref()
            .map_or("enUS".to_owned(), |client| client.locale.clone());
        let held = client
            .as_ref()
            .and_then(|client| client.faction(faction as u32))
            .cloned();
        let name = held.as_ref().map(|held| held.name(&locale)).unwrap_or_default();
        theme::section(
            ui,
            egui::Id::new("npcs faction"),
            theme::icon::INFO,
            "Faction",
            Some(&name),
            true,
            |ui| {
                let Some(client) = client else {
                    ui.label(RichText::new(format!("Faction {faction}")).strong());
                    ui.weak(
                        "The factions are read from the client's FactionTemplate.dbc and Faction.dbc: open the client.",
                    );
                    return;
                };
                let mut chosen = None;
                ui.horizontal(|ui| {
                ui.label(RichText::new("Faction").weak())
                    .on_hover_text("Who it fights and who it helps: a template of FactionTemplate.dbc (creature_template.faction)");
                let shown = held.as_ref().map_or(format!("{faction} (not in the client)"), |held| held.name(&locale));
                let combo = egui::ComboBox::from_id_salt("npcs faction list")
                    .selected_text(shown)
                    .width(280.0)
                    .height(360.0)
                    .show_ui(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.faction_search)
                                .hint_text("Search by name, in any language, or id")
                                .desired_width(260.0),
                        )
                        .on_hover_text("Lists the factions whose name, in any language of the client, holds the text, or whose id begins with it");
                        let search = self.faction_search.trim().to_lowercase();
                        let found: Vec<&client::Faction> = client
                            .factions
                            .iter()
                            .filter(|entry| {
                                search.is_empty()
                                    || entry.id.to_string().starts_with(&search)
                                    || entry.names.iter().any(|(_, name)| name.to_lowercase().contains(&search))
                            })
                            .collect();
                        egui::ScrollArea::vertical().max_height(320.0).show_rows(ui, 18.0, found.len(), |ui, rows| {
                            for entry in &found[rows] {
                                let label = egui::text::LayoutJob::default();
                                let mut job = label;
                                job.append(&entry.name(&locale), 0.0, egui::TextFormat::default());
                                job.append(
                                    &format!("  {}", entry.id),
                                    0.0,
                                    egui::TextFormat {
                                        font_id: egui::FontId::proportional(10.0),
                                        color: ui.visuals().weak_text_color(),
                                        ..Default::default()
                                    },
                                );
                                if ui
                                    .selectable_label(i64::from(entry.id) == faction, job)
                                    .on_hover_text(faction_said(entry, &locale))
                                    .clicked()
                                {
                                    chosen = Some(entry.id);
                                }
                            }
                        });
                    });
                if let Some(held) = &held {
                    combo.response.on_hover_text(faction_said(held, &locale));
                }
                ui.label(RichText::new(faction.to_string()).weak().small())
                    .on_hover_text("Its id (creature_template.faction)");
            });
                if let Some(held) = &held {
                    ui.label(RichText::new(stances(held)).weak().small());
                }
                if let Some(id) = chosen {
                    self.faction_search.clear();
                    self.write_whole("faction", i64::from(id), "faction", ctx);
                }
            },
        );
    }

    /// Its models: each its display, its scale, how often it is the one shown.
    fn models_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let prefab = self.prefab();
        let count = prefab.models.len().to_string();
        theme::section(
            ui,
            egui::Id::new("npcs models"),
            theme::icon::CUBE,
            "Models",
            Some(&count),
            true,
            |ui| {
                if prefab.models.is_empty() {
                    ui.label(
                        RichText::new("No model: it is not seen in the game. Add one.")
                            .italics()
                            .weak(),
                    );
                }
                let mut removed = None;
                egui::Grid::new("npcs models grid").num_columns(5).spacing([8.0, 4.0]).show(ui, |ui| {
                if !prefab.models.is_empty() {
                    for (title, hint) in [
                        ("Display", "An id of CreatureDisplayInfo.dbc (creature_template_model.CreatureDisplayID)"),
                        ("Scale", "Its size: 1 as the model is made (creature_template_model.DisplayScale)"),
                        ("Chance", "How often this model is the one shown among them (creature_template_model.Probability)"),
                        ("", ""),
                        ("", ""),
                    ] {
                        if title.is_empty() {
                            ui.label("");
                        } else {
                            ui.label(RichText::new(title).weak()).on_hover_text(hint);
                        }
                    }
                    ui.end_row();
                }
                let mut picked = None;
                for (at, model) in prefab.models.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Pick…")
                            .on_hover_text("Chooses its model among all those of the game, each seen in 3D")
                            .clicked()
                        {
                            picked = Some(at);
                        }
                        ui.label(RichText::new(model.display.to_string()).weak().small())
                            .on_hover_text("Its display (creature_template_model.CreatureDisplayID): chosen with Pick");
                    });
                    for (field, value, real) in [
                        ("scale", f64::from(model.scale), true),
                        ("probability", f64::from(model.probability), true),
                    ] {
                        let key = format!("model:{at}:{field}");
                        let current = number(value).to_string();
                        if let Some(typed) = self.typed(ui, &key, &current, 70.0, "Its scale or chance (creature_template_model)") {
                            let parsed = typed.trim().replace(',', ".").parse::<f64>().ok().filter(|value| {
                                value.is_finite() && *value >= 0.0 && (real || value.fract() == 0.0)
                            });
                            let Some(parsed) = parsed else {
                                self.said = Some(format!("'{typed}' is no {}", if real { "number" } else { "whole number" }));
                                continue;
                            };
                            let mut after = self.prefab();
                            let changed = &mut after.models[at];
                            match field {
                                "scale" => changed.scale = parsed as f32,
                                _ => changed.probability = parsed as f32,
                            }
                            self.change("Change a model of the NPC".to_owned(), after, ctx);
                        }
                    }
                    let file = self
                        .client
                        .as_ref()
                        .and_then(|client| client.models.get(&model.display).cloned())
                        .unwrap_or_default();
                    ui.label(RichText::new(file).weak());
                    if ui.small_button("✖").on_hover_text("Takes this model away").clicked() {
                        removed = Some(at);
                    }
                    ui.end_row();
                }
                if let Some(at) = picked {
                    self.pick_model(at);
                }
            });
                if let Some(at) = removed {
                    let mut after = self.prefab();
                    after.models.remove(at);
                    self.change("Take a model of the NPC away".to_owned(), after, ctx);
                }
                if ui
                    .button("✚ Add a model")
                    .on_hover_text("Another model it can have, chosen among those of the game; the client shows one of them, by their chances")
                    .clicked()
                {
                    // chosen in the window, which makes its row
                    let rows = self.prefab().models.len();
                    self.pick_model(rows);
                }
            },
        );
    }

    /// What it leaves when it dies: its loots and its money.
    fn loot_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        theme::section(
            ui,
            egui::Id::new("npcs loot"),
            theme::icon::STACK,
            "Loot and money",
            None,
            false,
            |ui| {
                egui::Grid::new("npcs loots")
                    .num_columns(3)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        for kind in 0..fields::LOOT.len() {
                            self.loot_field(ui, kind, ctx);
                        }
                    });
                egui::Grid::new("npcs gold")
                    .num_columns(3)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        for (column, label, hint) in [
                            (
                                "mingold",
                                "Money from",
                                "The least copper it leaves (creature_template.mingold)",
                            ),
                            (
                                "maxgold",
                                "Money to",
                                "The most copper it leaves (creature_template.maxgold)",
                            ),
                        ] {
                            ui.label(RichText::new(label).weak()).on_hover_text(hint);
                            let copper = self.prefab().whole(column);
                            if let Some(typed) = self.typed(
                                ui,
                                column,
                                &copper.to_string(),
                                90.0,
                                &format!("{hint}, in copper: 10000 is a gold piece"),
                            ) {
                                self.write_number(column, label, &typed, false, ctx);
                            }
                            ui.label(RichText::new(coins(copper)).weak());
                            ui.end_row();
                        }
                    });
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefab::as_number;

    #[test]
    fn copper_is_shown_as_coins() {
        assert_eq!(coins(5), "5c");
        assert_eq!(coins(250), "2s 50c");
        assert_eq!(coins(1_020_304), "102g 3s 4c");
        assert_eq!(as_number(&json!("12")), Some(12.0));
    }
}
