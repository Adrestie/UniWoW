//! The quests of the world database (milestone 16, step 16.3): searched by title or id in the panel
//! *Quests*, each opened read from its tables by a job; its texts in every language of the client,
//! its level, chain, objectives and rewards changed, one undo entry a field once left; created in
//! the editor's block of ids, blank or a copy, and deleted there alone; its givers and enders the
//! spawns selected, made givers of quests where they were not. Kept in the project, given to the
//! server as rows with the module `world-db`.

mod data;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use uniwow_api::scene::{self, Kind};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, State};
use uniwow_api::{Command, Context, DockArea, JobId, JobOutcome, Module, Registrar, egui, log, theme};

use data::{Edits, Loaded, Names, QuestEdit};

const MODULE: &str = "quests";

/// Published with the titles of the quests the user created or renamed, not yet in the database:
/// `{ "titles": { "<id>": "<title>" } }`, for the other modules to name them (the Inspector).
const TITLES_TOPIC: &str = "quests.titles";

/// The ids and titles of the quests of the database.
type List = Arc<BTreeMap<u32, String>>;

/// The changes made, from and to: one undo entry.
struct SetEdits {
    label: String,
    before: Edits,
    after: Edits,
}

impl Command for SetEdits {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut QuestsModule = module.downcast_mut().expect("the quests");
        module.set_edits(self.after.clone());
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut QuestsModule = module.downcast_mut().expect("the quests");
        module.set_edits(self.before.clone());
    }
}

/// A job of the module: the list read, a quest read, names read, flags of templates read.
enum Asked {
    List(JobId),
    Quest(JobId, u32),
    Names(JobId),
    Flags(JobId),
}

#[derive(Default)]
struct QuestsModule {
    /// The quests of the database, read once a server; for which.
    list: List,
    listed_for: Option<String>,
    /// The quests read, by id.
    loaded: BTreeMap<u32, Loaded>,
    asked: Vec<Asked>,
    /// The quest opened, the text searched and what it found, the language of the texts shown.
    open: Option<u32>,
    search: String,
    found: Vec<u32>,
    searched: Option<(String, u64)>,
    locale: usize,
    edits: Edits,
    /// How many times the edits or the quests read changed, and when the changes for the server
    /// were given last.
    generation: u64,
    given: Option<u64>,
    /// When the titles of the quests the user made were told last.
    titled: Option<u64>,
    /// The field being typed: its key and its text, written once left.
    typing: Option<(String, String)>,
    /// The names of what the numbers name, read by jobs; those asked.
    names: HashMap<(Names, i64), String>,
    named: BTreeSet<(Names, i64)>,
    said: Option<String>,
    scene: Option<scene::Handle>,
}

/// A value as a field shows it.
fn shown(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

impl QuestsModule {
    fn set_edits(&mut self, edits: Edits) {
        self.edits = edits;
        self.generation += 1;
        self.searched = None;
    }

    /// Makes `after` the edits, one undo entry.
    fn change(&mut self, label: String, after: Edits, ctx: &mut Context) {
        if after != self.edits {
            let before = self.edits.clone();
            ctx.execute(SetEdits { label, before, after });
        }
    }

    fn value(&self, id: u32, table: &str, locale: &str, column: &str) -> Option<Value> {
        self.edits.value(&self.loaded, id, table, locale, column)
    }

    fn title(&self, id: u32) -> String {
        shown(self.value(id, "quest_template", "", "LogTitle").as_ref())
            .chars()
            .take(80)
            .collect::<String>()
            .or_list(self.list.get(&id))
    }

    /// Reads the list of the quests once the database is connected, again for another server.
    fn steer(&mut self, ctx: &mut Context) {
        if self.scene.is_none() {
            self.scene = ctx.service(scene::SERVICE);
        }
        let Some(database) = ctx.service(world_db::SERVICE) else {
            return;
        };
        let State::Connected(server) = database.state() else {
            return;
        };
        if self.listed_for.as_ref() == Some(&server) || self.asked.iter().any(|asked| matches!(asked, Asked::List(_))) {
            return;
        }
        self.listed_for = Some(server);
        let job = ctx.spawn(
            "Read the quests of the world database",
            move |_| -> Result<List, String> {
                let rows = database.query("SELECT ID, LogTitle FROM quest_template", &[])?;
                Ok(Arc::new(
                    data::rows_of(&rows)
                        .iter()
                        .filter_map(|row| {
                            let id = data::as_number(row.get("ID")?)? as u32;
                            Some((id, shown(row.get("LogTitle"))))
                        })
                        .collect(),
                ))
            },
        );
        self.asked.push(Asked::List(job));
    }

    /// Opens the quest `id`, read by a job unless it was, with the quest it copies.
    fn open(&mut self, id: u32, ctx: &mut Context) {
        self.open = Some(id);
        self.typing = None;
        let copied = self.edits.quests.get(&id).and_then(|edit| edit.created).flatten();
        let created = self.edits.quests.get(&id).is_some_and(|edit| edit.created.is_some());
        let wanted = [(!created).then_some(id), copied].into_iter().flatten();
        for id in wanted {
            if self.loaded.contains_key(&id)
                || self
                    .asked
                    .iter()
                    .any(|asked| matches!(asked, Asked::Quest(_, read) if *read == id))
            {
                continue;
            }
            let Some(database) = ctx.service(world_db::SERVICE) else {
                return;
            };
            let job = ctx.spawn("Read a quest of the world database", move |_| -> Loaded {
                let answers: Vec<(String, uniwow_api::db::Rows)> = data::queries(id)
                    .into_iter()
                    .filter_map(|(table, sql, parameters)| match database.query(&sql, &parameters) {
                        Ok(rows) => Some((table, rows)),
                        Err(reason) => {
                            log::warn!("{table} of the quest {id} is not read: {reason}");
                            None
                        }
                    })
                    .collect();
                Loaded::of_answers(&answers)
            });
            self.asked.push(Asked::Quest(job, id));
        }
    }

    /// The names of what the numbers of the quest opened name, read by a job, once each.
    fn read_names(&mut self, wanted: Vec<(Names, i64)>, ctx: &mut Context) {
        let wanted: Vec<(Names, i64)> = wanted
            .into_iter()
            .filter(|key| key.1 != 0 && !self.named.contains(key) && key.0 != Names::Quest)
            .collect();
        if wanted.is_empty() {
            return;
        }
        let Some(database) = ctx.service(world_db::SERVICE) else {
            return;
        };
        self.named.extend(wanted.iter().copied());
        let job = ctx.spawn(
            "Read names of the world database",
            move |_| -> Vec<((Names, i64), String)> {
                let mut out = Vec::new();
                let mut by_table: BTreeMap<&str, Vec<(Names, i64)>> = BTreeMap::new();
                for (names, id) in wanted {
                    let table = match (names, id < 0) {
                        (Names::Item, _) => "item_template",
                        (Names::CreatureOrObject, true) => "gameobject_template",
                        _ => "creature_template",
                    };
                    by_table.entry(table).or_default().push((names, id));
                }
                for (table, keys) in by_table {
                    let marks = vec!["?"; keys.len()].join(", ");
                    let parameters: Vec<Value> = keys.iter().map(|(_, id)| json!(id.abs())).collect();
                    let Ok(rows) = database.query(
                        &format!("SELECT entry, name FROM {table} WHERE entry IN ({marks})"),
                        &parameters,
                    ) else {
                        continue;
                    };
                    for row in data::rows_of(&rows) {
                        let Some(entry) = row.get("entry").and_then(data::as_number) else {
                            continue;
                        };
                        for key in keys.iter().filter(|(_, id)| id.unsigned_abs() == entry as u64) {
                            out.push((*key, shown(row.get("name"))));
                        }
                    }
                }
                out
            },
        );
        self.asked.push(Asked::Names(job));
    }

    /// The titles of the quests the user created or renamed told the other modules, once changed.
    fn tell_titles(&mut self, ctx: &mut Context) {
        if self.titled == Some(self.generation) {
            return;
        }
        self.titled = Some(self.generation);
        let titles: BTreeMap<String, String> = self
            .edits
            .quests
            .iter()
            .filter(|(_, edit)| !edit.deleted)
            .filter_map(|(id, _)| {
                let title = shown(self.value(*id, "quest_template", "", "LogTitle").as_ref());
                (!title.is_empty()).then(|| (id.to_string(), title))
            })
            .collect();
        ctx.publish(TITLES_TOPIC, json!({ "titles": titles }));
    }

    fn give_pending(&mut self, ctx: &mut Context) {
        if self.given == Some(self.generation) {
            return;
        }
        let Some(database) = ctx.service(world_db::SERVICE) else {
            return;
        };
        self.given = Some(self.generation);
        database.set_pending(self.edits.pending(&self.loaded));
    }

    /// The field `key` showing `current`: typed, written once left; the text written, if changed.
    fn typed(&mut self, ui: &mut egui::Ui, key: String, current: &str, lines: bool, width: f32) -> Option<String> {
        let mut text = match &self.typing {
            Some((typing, text)) if *typing == key => text.clone(),
            _ => current.to_owned(),
        };
        let edit = if lines {
            egui::TextEdit::multiline(&mut text)
                .desired_rows(3)
                .desired_width(width)
        } else {
            egui::TextEdit::singleline(&mut text).desired_width(width)
        };
        let response = ui
            .add(edit)
            .on_hover_text("Written once you leave the field, one undo entry; to the database by Apply");
        if response.changed() {
            self.typing = Some((key.clone(), text.clone()));
        }
        if response.lost_focus()
            && let Some((_, typed)) = self.typing.take_if(|(typing, _)| *typing == key)
            && typed != current
        {
            return Some(typed);
        }
        None
    }

    /// The width of a field of numbers: 7 digits.
    fn number_width(ui: &egui::Ui) -> f32 {
        let font = egui::TextStyle::Body.resolve(ui.style());
        ui.fonts_mut(|fonts| fonts.glyph_width(&font, '0')) * 7.0 + ui.spacing().button_padding.x * 2.0 + 2.0
    }

    /// The list: the text searched, the quests found, the user's own first; a click opens one.
    fn list_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search a title or an id")
                    .desired_width(ui.available_width() - 110.0),
            );
            if ui
                .button("New quest")
                .on_hover_text(format!(
                    "A blank quest in the editor's block, {}-{}",
                    data::RANGE.start(),
                    data::RANGE.end()
                ))
                .clicked()
            {
                self.create(None, ctx);
            }
        });
        let key = (self.search.trim().to_lowercase(), self.generation);
        if self.searched.as_ref() != Some(&key) {
            let text = &key.0;
            let mut found: Vec<u32> = self.edits.quests.keys().copied().collect();
            found.extend(
                self.list
                    .iter()
                    .filter(|(id, title)| {
                        text.is_empty()
                            || id.to_string().starts_with(text.as_str())
                            || title.to_lowercase().contains(text.as_str())
                    })
                    .map(|(id, _)| *id)
                    .filter(|id| !self.edits.quests.contains_key(id))
                    .take(300),
            );
            self.found = found;
            self.searched = Some(key);
        }
        let mut opened = None;
        egui::ScrollArea::vertical()
            .id_salt("quests list")
            .max_height(180.0)
            .auto_shrink([false, true])
            .show_rows(ui, 18.0, self.found.len(), |ui, range| {
                for id in self.found[range].iter().copied() {
                    let edited = self.edits.quests.get(&id);
                    let mark = match edited {
                        Some(edit) if edit.deleted => "✖ ",
                        Some(edit) if edit.created.is_some() => "✚ ",
                        Some(_) => "● ",
                        None => "",
                    };
                    let label = format!("{mark}{id}  {}", self.title(id));
                    if ui
                        .selectable_label(self.open == Some(id), label)
                        .on_hover_text("Opens the quest, read from the database of the server chosen")
                        .clicked()
                    {
                        opened = Some(id);
                    }
                }
            });
        if let Some(id) = opened {
            self.open(id, ctx);
        }
    }

    /// A quest made in the editor's block: blank, or a copy of `copied`; opened.
    fn create(&mut self, copied: Option<u32>, ctx: &mut Context) {
        let Some(id) = data::free_id(&self.list, &self.edits) else {
            self.said = Some("The editor's block of quests is full".to_owned());
            return;
        };
        let mut after = self.edits.clone();
        after.quests.insert(
            id,
            QuestEdit {
                created: Some(copied),
                ..QuestEdit::default()
            },
        );
        if copied.is_none() {
            // Its texts, empty, that the tables want.
            for text in &data::TEXTS {
                after.set(id, text.table, "", text.column, json!(""));
            }
            after.set(id, "quest_template", "", "LogTitle", json!("New quest"));
            after.set(id, "quest_template", "", "QuestLevel", json!(1));
            after.set(id, "quest_template", "", "MinLevel", json!(1));
        } else {
            let title = self.title(copied.unwrap_or_default());
            after.set(id, "quest_template", "", "LogTitle", json!(format!("{title} (copy)")));
        }
        self.change(format!("Create the quest {id}"), after, ctx);
        self.open(id, ctx);
    }

    /// The quest opened: its header, its texts in the language chosen, its numbers by section, its
    /// givers and enders.
    fn quest_ui(&mut self, ui: &mut egui::Ui, id: u32, ctx: &mut Context) {
        let edit = self.edits.quests.get(&id).cloned().unwrap_or_default();
        let created = edit.created.is_some();
        let reading = self
            .asked
            .iter()
            .any(|asked| matches!(asked, Asked::Quest(_, read) if *read == id));
        if !created && !self.loaded.contains_key(&id) {
            ui.label(if reading { "Reading the quest…" } else { "Not read." });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.heading(format!("{id}  {}", self.title(id)));
        });
        ui.horizontal(|ui| {
            if ui
                .button("Duplicate")
                .on_hover_text("A copy in the editor's block")
                .clicked()
            {
                self.create(Some(id), ctx);
            }
            let ours = data::RANGE.contains(&id);
            if edit.deleted {
                ui.colored_label(ui.visuals().warn_fg_color, "Deleted once applied");
                if ui
                    .button("Keep it")
                    .on_hover_text("It is no longer deleted when applied")
                    .clicked()
                {
                    let mut after = self.edits.clone();
                    if let Some(edit) = after.quests.get_mut(&id) {
                        edit.deleted = false;
                    }
                    self.change(format!("Keep the quest {id}"), after, ctx);
                }
            } else if ui
                .add_enabled(ours, egui::Button::new("Delete"))
                .on_hover_text("Deleted with its texts and givers once applied (in memory until then); it is also taken out of the quest logs and the quests done of every character of the server, by its bridge")
                .on_disabled_hover_text("Only the quests of the editor's block (97000-97099) are deleted here")
                .clicked()
            {
                let mut after = self.edits.clone();
                after.quests.entry(id).or_default().deleted = true;
                self.change(format!("Delete the quest {id}"), after, ctx);
            }
            egui::ComboBox::from_id_salt("quests locale")
                .selected_text(data::LOCALES[self.locale])
                .show_ui(ui, |ui| {
                    for (at, locale) in data::LOCALES.iter().enumerate() {
                        ui.selectable_value(&mut self.locale, at, *locale);
                    }
                })
                .response
                .on_hover_text(
                    "The language of the texts: enUS in the quest's tables, the others in their tables of locales",
                );
        });
        ui.add_space(4.0);
        let locale = data::LOCALES[self.locale];
        theme::section(
            ui,
            egui::Id::new("quests texts"),
            theme::icon::LOG,
            "Texts",
            Some(locale),
            true,
            |ui| {
                ui.label(
                    egui::RichText::new("$B a new line, $N the player's name, $C his class, $R his race")
                        .weak()
                        .small(),
                );
                for text in &data::TEXTS {
                    let (table, column, locale_key) = if locale == "enUS" {
                        (text.table, text.column, "")
                    } else {
                        (text.locale_table, text.locale_column, locale)
                    };
                    let current = shown(self.value(id, table, locale_key, column).as_ref());
                    ui.label(egui::RichText::new(text.label).weak());
                    if locale != "enUS" && current.is_empty() {
                        let english = shown(self.value(id, text.table, "", text.column).as_ref());
                        if !english.is_empty() {
                            ui.label(egui::RichText::new(format!("enUS: {english}")).weak().small().italics());
                        }
                    }
                    let key = format!("{id}:{table}:{locale_key}:{column}");
                    if let Some(typed) = self.typed(ui, key, &current, text.lines, ui.available_width()) {
                        let mut after = self.edits.clone();
                        after.set(id, table, locale_key, column, json!(typed));
                        self.change(
                            format!("Write the {} of the quest {id}", text.label.to_lowercase()),
                            after,
                            ctx,
                        );
                    }
                }
            },
        );
        let width = Self::number_width(ui);
        let mut wanted = Vec::new();
        for (title, numbers) in data::SECTIONS {
            theme::section(
                ui,
                egui::Id::new(("quests section", title)),
                theme::icon::SLIDERS,
                title,
                None,
                true,
                |ui| {
                    egui::Grid::new(("quests numbers", title))
                        .num_columns(4)
                        .spacing([6.0, 4.0])
                        .show(ui, |ui| {
                            for number in numbers {
                                ui.label(egui::RichText::new(number.label).weak());
                                let value = self.value(id, number.table, "", number.column);
                                let current = shown(value.as_ref());
                                let key = format!("{id}:{}::{}", number.table, number.column);
                                if let Some(typed) = self.typed(ui, key, &current, false, width) {
                                    self.write_number(id, number.table, number.column, number.label, &typed, ctx);
                                }
                                match number.count {
                                    Some(count) => {
                                        let current = shown(self.value(id, number.table, "", count).as_ref());
                                        let key = format!("{id}:{}::{count}", number.table);
                                        if let Some(typed) = self.typed(ui, key, &current, false, width * 0.6) {
                                            self.write_number(id, number.table, count, number.label, &typed, ctx);
                                        }
                                    }
                                    None => {
                                        ui.label("");
                                    }
                                }
                                let named = value
                                    .as_ref()
                                    .and_then(data::as_number)
                                    .map(|value| value as i64)
                                    .unwrap_or(0);
                                let name = match number.names {
                                    Names::None => String::new(),
                                    _ if named == 0 => String::new(),
                                    Names::Quest => self.list.get(&(named as u32)).cloned().unwrap_or_default(),
                                    names => {
                                        wanted.push((names, named));
                                        self.names.get(&(names, named)).cloned().unwrap_or_default()
                                    }
                                };
                                ui.label(egui::RichText::new(name).weak());
                                ui.end_row();
                            }
                        });
                },
            );
        }
        self.read_names(wanted, ctx);
        self.givers_ui(ui, id, ctx);
    }

    /// `typed` written into `column` of `table` of the quest `id`, a number; refused, said.
    fn write_number(&mut self, id: u32, table: &str, column: &str, label: &str, typed: &str, ctx: &mut Context) {
        let typed = typed.trim();
        let value = if typed.is_empty() {
            json!(0)
        } else if let Ok(whole) = typed.parse::<i64>() {
            json!(whole)
        } else if let Ok(real) = typed.parse::<f64>() {
            json!(real)
        } else {
            self.said = Some(format!("{label}: '{typed}' is no number"));
            return;
        };
        let mut after = self.edits.clone();
        after.set(id, table, "", column, value);
        self.change(
            format!("Set the {} of the quest {id}", label.to_lowercase()),
            after,
            ctx,
        );
    }

    /// The givers and enders of the quest as the database has them; the spawns selected made
    /// givers or enders, their templates givers of quests where they were not.
    fn givers_ui(&mut self, ui: &mut egui::Ui, id: u32, ctx: &mut Context) {
        let relations = self
            .loaded
            .get(&id)
            .map(|loaded| loaded.relations.clone())
            .unwrap_or_default();
        let mut wanted = Vec::new();
        theme::section(
            ui,
            egui::Id::new("quests givers"),
            theme::icon::QUESTION,
            "Givers and enders",
            None,
            true,
            |ui| {
                for (table, object, ends) in data::RELATIONS {
                    let entries = relations.get(table).cloned().unwrap_or_default();
                    if entries.is_empty() {
                        continue;
                    }
                    let role = if ends { "Ends it" } else { "Starts it" };
                    ui.label(
                        egui::RichText::new(format!(
                            "{role} ({})",
                            if object { "game objects" } else { "creatures" }
                        ))
                        .weak(),
                    );
                    for entry in entries {
                        let key = (
                            if object {
                                Names::CreatureOrObject
                            } else {
                                Names::Creature
                            },
                            if object { -i64::from(entry) } else { i64::from(entry) },
                        );
                        wanted.push(key);
                        let name = self.names.get(&key).cloned().unwrap_or_default();
                        ui.label(format!("  {entry}  {name}"));
                    }
                }
                ui.label(
                    egui::RichText::new(
                        "Given or taken to the spawns selected: kept with the spawns; one taken in their Inspector.",
                    )
                    .weak()
                    .small(),
                );
                let selected: Vec<String> = self
                    .scene
                    .as_ref()
                    .map(|scene| {
                        scene
                            .selection()
                            .ids
                            .iter()
                            .filter(|id| matches!(id.kind, Kind::Creature | Kind::GameObject))
                            .map(ToString::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    for (role, label) in [("starts", "The selected start it"), ("ends", "The selected end it")] {
                        if ui
                            .add_enabled(!selected.is_empty(), egui::Button::new(label))
                            .on_hover_text("The creatures and game objects selected in the scene give or take it; in memory with the spawns, written by Apply")
                            .clicked()
                        {
                            self.give(id, role, &selected, ctx);
                        }
                    }
                });
            },
        );
        self.read_names(wanted, ctx);
    }

    /// The quest `id` given to the spawns `ids` as `role`, through the spawns; the templates of the
    /// creatures among them read, to be made givers of quests.
    fn give(&mut self, id: u32, role: &str, ids: &[String], ctx: &mut Context) {
        ctx.call(
            "spawns.quests",
            json!({ "ids": ids, "quest": id, "role": role, "given": true }),
        );
        let Some(scene) = &self.scene else {
            return;
        };
        let snapshot = scene.snapshot();
        let entries: BTreeSet<u32> = ids
            .iter()
            .filter_map(|id| id.parse::<scene::EntityId>().ok())
            .filter(|id| id.kind == Kind::Creature)
            .filter_map(|id| snapshot.get(&id).map(|entity| entity.extra()))
            .collect();
        let (Some(database), false) = (ctx.service(world_db::SERVICE), entries.is_empty()) else {
            return;
        };
        let job = ctx.spawn("Read the flags of the givers of a quest", move |_| -> Vec<(u32, u64)> {
            let marks = vec!["?"; entries.len()].join(", ");
            let parameters: Vec<Value> = entries.iter().map(|entry| json!(entry)).collect();
            database
                .query(
                    &format!("SELECT entry, npcflag FROM creature_template WHERE entry IN ({marks})"),
                    &parameters,
                )
                .map(|rows| {
                    data::rows_of(&rows)
                        .iter()
                        .filter_map(|row| {
                            Some((
                                data::as_number(row.get("entry")?)? as u32,
                                data::as_number(row.get("npcflag")?)? as u64,
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        self.asked.push(Asked::Flags(job));
    }
}

/// A text, or the one of the list when it is empty.
trait OrList {
    fn or_list(self, listed: Option<&String>) -> String;
}

impl OrList for String {
    fn or_list(self, listed: Option<&String>) -> String {
        if self.is_empty() {
            listed.cloned().unwrap_or_default()
        } else {
            self
        }
    }
}

impl Module for QuestsModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.subscribe(world_db::APPLIED_TOPIC);
        reg.panel("quests", "Quests", DockArea::Right).command(
            "quests.open",
            "Opens the quest 'quest' in the panel Quests, read from the world database, and shows the panel.",
            json!({ "type": "object", "properties": { "quest": { "type": "integer" } }, "required": ["quest"] }),
            json!({ "type": "object" }),
        );
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        self.steer(ctx);
        self.give_pending(ctx);
        self.tell_titles(ctx);
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        match ctx.service(world_db::SERVICE).map(|database| database.state()) {
            None => {
                ui.label("No module world-db: the quests are not read.");
                return;
            }
            Some(State::Connected(_)) => {}
            Some(_) => {
                ui.label("The world database is not connected: see the panel World database.");
                return;
            }
        }
        if let Some(said) = &self.said {
            ui.colored_label(ui.visuals().warn_fg_color, said);
        }
        if self.listed_for.is_some()
            && self.list.is_empty()
            && self.asked.iter().any(|asked| matches!(asked, Asked::List(_)))
        {
            ui.label("Reading the quests…");
        }
        self.list_ui(ui, ctx);
        ui.separator();
        let Some(id) = self.open else {
            ui.weak("Open a quest of the list, or make a new one.");
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("quests opened")
            .auto_shrink([false, false])
            .show(ui, |ui| self.quest_ui(ui, id, ctx));
    }

    fn on_event(&mut self, event: &uniwow_api::Event, ctx: &mut Context) {
        let ours = event.payload["owners"]
            .as_array()
            .is_some_and(|owners| owners.iter().any(|owner| owner.as_str() == Some(MODULE)));
        if event.topic == world_db::APPLIED_TOPIC && ours {
            // The changes are the database's rows now: read again.
            self.set_edits(Edits::default());
            self.loaded.clear();
            self.listed_for = None;
            self.open = None;
        }
        // Rows written by another (an import): read again, the changes kept.
        let read = event.payload["read"]
            .as_array()
            .is_some_and(|read| read.iter().any(|module| module.as_str() == Some(MODULE)));
        if event.topic == world_db::APPLIED_TOPIC && read {
            self.loaded.clear();
            self.listed_for = None;
            if let Some(id) = self.open {
                self.open(id, ctx);
            }
        }
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        let Some(at) = self.asked.iter().position(|asked| match asked {
            Asked::List(id) | Asked::Quest(id, _) | Asked::Names(id) | Asked::Flags(id) => *id == job,
        }) else {
            return;
        };
        match self.asked.remove(at) {
            Asked::List(_) => match outcome.take::<Result<List, String>>() {
                Some(Ok(list)) => {
                    log::info!("{} quests read from the world database", list.len());
                    self.list = list;
                    self.searched = None;
                }
                Some(Err(reason)) => self.said = Some(format!("The quests are not read: {reason}")),
                None => {}
            },
            Asked::Quest(_, id) => {
                if let Some(loaded) = outcome.take::<Loaded>() {
                    self.loaded.insert(id, loaded);
                    self.generation += 1;
                }
            }
            Asked::Names(_) => {
                if let Some(names) = outcome.take::<Vec<((Names, i64), String)>>() {
                    self.names.extend(names);
                }
            }
            Asked::Flags(_) => {
                let Some(flags) = outcome.take::<Vec<(u32, u64)>>() else {
                    return;
                };
                let mut after = self.edits.clone();
                for (entry, flag) in flags {
                    let held = after.flags.get(&entry).copied().unwrap_or(flag);
                    if held & data::QUEST_GIVER == 0 {
                        after.flags.insert(entry, held | data::QUEST_GIVER);
                    }
                }
                self.change("Make givers of quests".to_owned(), after, ctx);
            }
        }
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        match name {
            "quests.open" => {
                let id = arguments["quest"].as_u64().ok_or("'quest' is the id of a quest")? as u32;
                self.search.clear();
                self.searched = None;
                self.open(id, ctx);
                ctx.show_panel("quests");
                Ok(json!({}))
            }
            other => Err(format!("'{other}' is not handled")),
        }
    }

    fn project_section(&self) -> Option<Value> {
        Some(self.edits.section())
    }

    fn open_project(&mut self, section: Option<Value>, _ctx: &mut Context) {
        self.set_edits(Edits::of_section(&section.unwrap_or_default()));
        self.open = None;
    }
}

uniwow_api::export_module!(QuestsModule::default());
