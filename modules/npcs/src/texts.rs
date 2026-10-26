//! What an NPC says (step 18.3): its texts by group (`creature_text`), each group one or several
//! lines the server chooses among by their chance, each line in every language of the client
//! (`creature_text_locale`), said, yelled, whispered or as an emote, with its language, an emote
//! played and a sound. A skill says a group (Say a text).

use uniwow_api::egui::{self, RichText};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::{Context, theme};

use crate::NpcsModule;
use crate::prefab::LOCALES;

/// How a line is said (`creature_text.Type`, `ChatMsg`).
pub const KINDS: [(i64, &str, &str); 6] = [
    (12, "Says", "Said aloud, heard nearby"),
    (14, "Yells", "Yelled, heard in a wider area"),
    (16, "Emote", "A line in the third person, as an emote"),
    (15, "Whispers", "Whispered to its target alone"),
    (41, "Boss emote", "An emote shown in the middle of the screen"),
    (42, "Boss whisper", "A whisper shown in the middle of the screen"),
];

/// The languages a line is said in (`creature_text.Language`, `Languages`).
pub const LANGUAGES: [(i64, &str); 11] = [
    (0, "Universal"),
    (7, "Common"),
    (1, "Orcish"),
    (2, "Darnassian"),
    (3, "Taurahe"),
    (6, "Dwarvish"),
    (10, "Thalassian"),
    (13, "Gnomish"),
    (14, "Troll"),
    (33, "Gutterspeak"),
    (35, "Draenei"),
];

/// A line: its text by language, how it is said, its language, its chance among its group, an
/// emote played, a sound, how long it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub texts: Vec<(String, String)>,
    pub kind: i64,
    pub language: i64,
    pub probability: f32,
    pub emote: i64,
    pub sound: i64,
    pub duration: i64,
}

impl Default for Line {
    fn default() -> Self {
        Self {
            texts: vec![("enUS".to_owned(), String::new())],
            kind: 12,
            language: 0,
            probability: 100.0,
            emote: 0,
            sound: 0,
            duration: 0,
        }
    }
}

impl Line {
    pub fn text(&self, locale: &str) -> &str {
        self.texts
            .iter()
            .find(|(held, _)| held == locale)
            .map_or("", |(_, text)| text.as_str())
    }

    pub fn set(&mut self, locale: &str, text: String) {
        match self.texts.iter_mut().find(|(held, _)| held == locale) {
            Some((_, held)) => *held = text,
            None => self.texts.push((locale.to_owned(), text)),
        }
    }
}

/// A group of lines: its number (`GroupID`), what it is for, its lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub group: i64,
    pub name: String,
    pub lines: Vec<Line>,
}

impl Group {
    pub fn to_json(&self) -> Value {
        json!({
            "group": self.group,
            "name": self.name,
            "lines": self.lines.iter().map(|line| json!({
                "texts": line.texts.iter().filter(|(_, text)| !text.is_empty())
                    .map(|(locale, text)| (locale.clone(), json!(text)))
                    .collect::<uniwow_api::serde_json::Map<String, Value>>(),
                "type": line.kind,
                "language": line.language,
                "probability": line.probability,
                "emote": line.emote,
                "sound": line.sound,
                "duration": line.duration,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn of_json(value: &Value) -> Self {
        let lines = value["lines"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .map(|line| Line {
                        texts: line["texts"]
                            .as_object()
                            .map(|texts| {
                                texts
                                    .iter()
                                    .map(|(locale, text)| {
                                        (locale.clone(), text.as_str().unwrap_or_default().to_owned())
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        kind: line["type"].as_i64().unwrap_or(12),
                        language: line["language"].as_i64().unwrap_or(0),
                        probability: line["probability"].as_f64().unwrap_or(100.0) as f32,
                        emote: line["emote"].as_i64().unwrap_or(0),
                        sound: line["sound"].as_i64().unwrap_or(0),
                        duration: line["duration"].as_i64().unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            group: value["group"].as_i64().unwrap_or(0),
            name: value["name"].as_str().unwrap_or_default().to_owned(),
            lines,
        }
    }

    /// The group said in a line: its number, what it is for, its first line in English.
    pub fn label(&self) -> String {
        let first = self.lines.first().map_or("", |line| line.text("enUS"));
        let first: String = first.chars().take(40).collect();
        match (self.name.is_empty(), first.is_empty()) {
            (false, _) => format!("{}: {}", self.group, self.name),
            (true, false) => format!("{}: \"{first}\"", self.group),
            (true, true) => format!("{}: (empty)", self.group),
        }
    }
}

pub fn groups_of(texts: &[Value]) -> Vec<Group> {
    texts.iter().map(Group::of_json).collect()
}

impl NpcsModule {
    fn write_texts(&mut self, groups: &[Group], label: &str, ctx: &mut Context) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let mut after = doc.prefab.clone();
        after.texts = groups.iter().map(Group::to_json).collect();
        self.change(label.to_owned(), after, ctx);
    }

    /// The card of its texts: each group, its lines in the language chosen.
    pub(crate) fn texts_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let groups = self
            .doc
            .as_ref()
            .map(|doc| groups_of(&doc.prefab.texts))
            .unwrap_or_default();
        let locale = LOCALES[self.locale];
        let count = groups.len().to_string();
        theme::section(
            ui,
            egui::Id::new("npcs texts"),
            theme::icon::LOG,
            "Texts",
            Some(&count),
            false,
            |ui| {
                ui.label(
                RichText::new(format!(
                    "What it says, by group: a skill says a group, the server takes one of its lines by their chance. Language of the texts: {locale} (chosen in the card Name). $n the player's name, $c his class, $r his race."
                ))
                .weak()
                .small(),
            );
                let mut changed: Option<(Vec<Group>, String)> = None;
                for (at, group) in groups.iter().enumerate() {
                    egui::CollapsingHeader::new(RichText::new(group.label()).strong())
                    .id_salt(("npcs text group", at))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("For").weak())
                                .on_hover_text("What the group is for, for you alone: not written to the server");
                            let key = format!("text:{at}:name");
                            if let Some(typed) = self.typed(ui, &key, &group.name, 200.0, "What the group is for (aggro, death, a phase…); kept in the prefab alone") {
                                let mut after = groups.clone();
                                after[at].name = typed;
                                changed = Some((after, "Name a group of texts".to_owned()));
                            }
                            if ui.small_button("✖").on_hover_text("Takes the group and its lines away").clicked() {
                                let mut after = groups.clone();
                                after.remove(at);
                                changed = Some((after, "Take a group of texts away".to_owned()));
                            }
                        });
                        for (line_at, line) in group.lines.iter().enumerate() {
                            ui.separator();
                            let current = line.text(locale).to_owned();
                            let key = format!("text:{at}:{line_at}:{locale}");
                            let place = if locale == "enUS" {
                                "creature_text.Text".to_owned()
                            } else {
                                format!("creature_text_locale.Text, {locale}")
                            };
                            ui.horizontal(|ui| {
                                if let Some(typed) = self.typed(ui, &key, &current, ui.available_width() - 30.0, &format!("The line, in {locale} ({place})")) {
                                    let mut after = groups.clone();
                                    after[at].lines[line_at].set(locale, typed);
                                    changed = Some((after, format!("Write a text of the NPC ({locale})")));
                                }
                                if ui.small_button("✖").on_hover_text("Takes the line away").clicked() {
                                    let mut after = groups.clone();
                                    after[at].lines.remove(line_at);
                                    changed = Some((after, "Take a line away".to_owned()));
                                }
                            });
                            if locale != "enUS" && current.is_empty() && !line.text("enUS").is_empty() {
                                ui.label(RichText::new(format!("enUS: {}", line.text("enUS"))).weak().small().italics());
                            }
                            ui.horizontal_wrapped(|ui| {
                                let mut after_line = line.clone();
                                egui::ComboBox::from_id_salt(("npcs text kind", at, line_at))
                                    .selected_text(KINDS.iter().find(|(v, _, _)| *v == line.kind).map_or("?", |(_, name, _)| *name))
                                    .show_ui(ui, |ui| {
                                        for (value, name, hint) in KINDS {
                                            ui.selectable_value(&mut after_line.kind, value, name)
                                                .on_hover_text(format!("{hint} (Type {value})"));
                                        }
                                    })
                                    .response
                                    .on_hover_text("How it is said (creature_text.Type)");
                                egui::ComboBox::from_id_salt(("npcs text language", at, line_at))
                                    .selected_text(LANGUAGES.iter().find(|(v, _)| *v == line.language).map_or("?", |(_, name)| *name))
                                    .show_ui(ui, |ui| {
                                        for (value, name) in LANGUAGES {
                                            ui.selectable_value(&mut after_line.language, value, name)
                                                .on_hover_text(format!("Understood by those who speak it; Universal by all (Language {value})"));
                                        }
                                    })
                                    .response
                                    .on_hover_text("The language it is said in (creature_text.Language)");
                                ui.add(egui::DragValue::new(&mut after_line.probability).range(0.0..=100.0).suffix(" %"))
                                    .on_hover_text("Its chance among the lines of its group (creature_text.Probability)");
                                let emote = self.client.as_ref().and_then(|client| client.emote(after_line.emote as u32));
                                ui.label(RichText::new("emote").weak())
                                    .on_hover_text("An emote it plays as it says the line (creature_text.Emote)");
                                if let Some(chosen) = self.emote_combo(ui, ("npcs text emote", at, line_at), after_line.emote, emote) {
                                    after_line.emote = chosen;
                                }
                                ui.label(RichText::new("sound").weak())
                                    .on_hover_text("A sound played with it, of SoundEntries.dbc (creature_text.Sound)");
                                let sound = crate::smart::Param {
                                    name: "sound",
                                    hint: "A sound played with it, of SoundEntries.dbc; none for none (creature_text.Sound)",
                                    kind: crate::smart::Kind::Sound,
                                };
                                if let Some(chosen) = self.list_combo(ui, ("npcs text sound", at, line_at), &sound, after_line.sound) {
                                    after_line.sound = chosen;
                                }
                                if after_line != *line {
                                    let mut after = groups.clone();
                                    after[at].lines[line_at] = after_line;
                                    changed = Some((after, "Change a line of the NPC".to_owned()));
                                }
                            });
                        }
                        if ui.small_button("✚ Add a line").on_hover_text("Another line of the group, one of which is said by chance").clicked() {
                            let mut after = groups.clone();
                            after[at].lines.push(Line::default());
                            changed = Some((after, "Add a line".to_owned()));
                        }
                    });
                }
                if ui
                    .button("✚ Add a group of texts")
                    .on_hover_text("A new group: what the NPC says on aggro, at its death, at a phase…")
                    .clicked()
                {
                    let mut after = groups.clone();
                    let next = groups.iter().map(|group| group.group + 1).max().unwrap_or(0);
                    after.push(Group {
                        group: next,
                        name: String::new(),
                        lines: vec![Line::default()],
                    });
                    changed = Some((after, "Add a group of texts".to_owned()));
                }
                if let Some((after, label)) = changed {
                    self.write_texts(&after, &label, ctx);
                }
            },
        );
    }

    /// A list of the client's emotes searched by name; the emote chosen.
    pub(crate) fn emote_combo(
        &mut self,
        ui: &mut egui::Ui,
        salt: impl egui::AsIdSalt,
        value: i64,
        shown: Option<String>,
    ) -> Option<i64> {
        let client = self.client.clone()?;
        let id = ui.make_persistent_id(salt);
        let mut chosen = None;
        let label = if value == 0 {
            "none".to_owned()
        } else {
            shown.unwrap_or_else(|| value.to_string())
        };
        egui::ComboBox::from_id_salt(id)
            .selected_text(label)
            .height(320.0)
            .show_ui(ui, |ui| {
                let search = self.searches.entry(format!("{id:?}")).or_default();
                ui.add(
                    egui::TextEdit::singleline(search)
                        .hint_text("Search an emote")
                        .desired_width(180.0),
                )
                .on_hover_text(
                    "The emotes of the client's Emotes.dbc whose name holds the text, or whose id begins with it",
                );
                let search = search.trim().to_lowercase();
                if ui
                    .selectable_label(value == 0, "none")
                    .on_hover_text("No emote")
                    .clicked()
                {
                    chosen = Some(0);
                }
                for (emote, raw) in &client.emotes {
                    let name = crate::client::emote_name(raw);
                    if !search.is_empty() && !name.contains(&search) && !emote.to_string().starts_with(&search) {
                        continue;
                    }
                    if ui
                        .selectable_label(i64::from(*emote) == value, format!("{name}  {emote}"))
                        .on_hover_text(raw.as_str())
                        .clicked()
                    {
                        chosen = Some(i64::from(*emote));
                    }
                }
            })
            .response
            .on_hover_text("The emotes of the client, searched by name");
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_of_texts_is_kept_as_json_and_said_by_its_first_line() {
        let mut line = Line::default();
        line.set("enUS", "You will die!".to_owned());
        line.set("frFR", "Vous allez mourir !".to_owned());
        let group = Group {
            group: 0,
            name: String::new(),
            lines: vec![line],
        };
        assert_eq!(Group::of_json(&group.to_json()), group);
        assert_eq!(group.label(), "0: \"You will die!\"");
        assert_eq!(group.lines[0].text("deDE"), "");
    }
}
