//! An NPC given to the server (step 18.4): the prefabs sent, kept by entry until *Apply to the
//! server* writes their rows; each a line of *Changes for the server*, taken away there. Its rows:
//! its template and its names in every language, its models, its weapons, its SmartAI (`link` for
//! the skills that follow one another) and its texts in every language; then the bridge loads the
//! template again and spawns its creatures again (`Reload::Template`).

use std::collections::BTreeMap;

use uniwow_api::db::write::{Row, RowChange, row};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, Reload};

use crate::prefab::{self, Prefab};
use crate::smart::{self, Names, Skill};
use crate::texts;

/// The event a skill following another at once takes (`SMART_EVENT_LINK`).
const LINK: i64 = 61;

/// The rows of `prefab` as the template `entry`: each table's rows of the entry taken away, then
/// written again.
pub fn rows(entry: u32, prefab: &Prefab, names: &dyn Names) -> Vec<RowChange> {
    let mut out = Vec::new();
    let mut set: Row = prefab.template.clone().into_iter().collect();
    for (column, value) in prefab::DEFAULTS {
        set.entry(column.to_owned()).or_insert_with(|| prefab::number(value));
    }
    set.insert("name".to_owned(), json!(prefab.name("enUS")));
    set.insert("subname".to_owned(), json!(prefab.title("enUS")));
    set.insert(
        "AIName".to_owned(),
        json!(if prefab.skills.is_empty() && prefab.phases.is_empty() {
            ""
        } else {
            "SmartAI"
        }),
    );
    out.push(RowChange::Put {
        table: "creature_template".to_owned(),
        key: row([("entry", json!(entry))]),
        set,
        from: None,
    });
    let replace = |out: &mut Vec<RowChange>, table: &str, column: &str, rows: Vec<Row>| {
        let key = row([(column, json!(entry))]);
        out.push(RowChange::Delete {
            table: table.to_owned(),
            key: key.clone(),
        });
        if !rows.is_empty() {
            out.push(RowChange::Insert {
                table: table.to_owned(),
                key,
                rows,
            });
        }
    };
    let locales: Vec<Row> = prefab
        .names
        .iter()
        .filter(|(locale, (name, title))| *locale != "enUS" && (!name.is_empty() || !title.is_empty()))
        .map(|(locale, (name, title))| {
            row([
                ("entry", json!(entry)),
                ("locale", json!(locale)),
                ("Name", json!(name)),
                ("Title", json!(title)),
                ("VerifiedBuild", json!(0)),
            ])
        })
        .collect();
    replace(&mut out, "creature_template_locale", "entry", locales);
    let models: Vec<Row> = prefab
        .models
        .iter()
        .enumerate()
        .map(|(at, model)| {
            row([
                ("CreatureID", json!(entry)),
                ("Idx", json!(at)),
                ("CreatureDisplayID", json!(model.display)),
                ("DisplayScale", json!(model.scale)),
                ("Probability", json!(model.probability)),
                ("VerifiedBuild", json!(0)),
            ])
        })
        .collect();
    replace(&mut out, "creature_template_model", "CreatureID", models);
    // Its weapons, its row written even with empty hands: its spawns name it (equipment_id 1), and
    // the server, which keeps the rows it read, takes its hands emptied too.
    let weapons = vec![row([
        ("CreatureID", json!(entry)),
        ("ID", json!(1)),
        ("ItemID1", json!(prefab.equipment[0])),
        ("ItemID2", json!(prefab.equipment[1])),
        ("ItemID3", json!(prefab.equipment[2])),
        ("VerifiedBuild", json!(0)),
    ])];
    replace(&mut out, "creature_equip_template", "CreatureID", weapons);
    let mut skills: Vec<Skill> = prefab.skills.iter().map(Skill::of_json).collect();
    // the last skill links to none; those of the phases after
    if let Some(last) = skills.last_mut() {
        last.linked = false;
    }
    skills.extend(crate::phases::skills(&crate::phases::phases_of(&prefab.phases)));
    let scripts = scripts(entry, &skills, names);
    let key = row([("entryorguid", json!(entry)), ("source_type", json!(0))]);
    out.push(RowChange::Delete {
        table: "smart_scripts".to_owned(),
        key: key.clone(),
    });
    if !scripts.is_empty() {
        out.push(RowChange::Insert {
            table: "smart_scripts".to_owned(),
            key,
            rows: scripts,
        });
    }
    let groups = texts::groups_of(&prefab.texts);
    let mut said = Vec::new();
    let mut said_locales = Vec::new();
    for group in &groups {
        for (at, line) in group.lines.iter().enumerate() {
            said.push(row([
                ("CreatureID", json!(entry)),
                ("GroupID", json!(group.group)),
                ("ID", json!(at)),
                ("Text", json!(line.text("enUS"))),
                ("Type", json!(line.kind)),
                ("Language", json!(line.language)),
                ("Probability", json!(line.probability)),
                ("Emote", json!(line.emote)),
                ("Duration", json!(line.duration)),
                ("Sound", json!(line.sound)),
                ("BroadcastTextId", json!(0)),
                ("TextRange", json!(0)),
                ("comment", json!(format!("{} - {}", prefab.name("enUS"), group.name))),
            ]));
            for (locale, text) in &line.texts {
                if locale != "enUS" && !text.is_empty() {
                    said_locales.push(row([
                        ("CreatureID", json!(entry)),
                        ("GroupID", json!(group.group)),
                        ("ID", json!(at)),
                        ("Locale", json!(locale)),
                        ("Text", json!(text)),
                    ]));
                }
            }
        }
    }
    replace(&mut out, "creature_text", "CreatureID", said);
    replace(&mut out, "creature_text_locale", "CreatureID", said_locales);
    out
}

/// The lines of `smart_scripts` of `skills` (source 0, the entry `entry`): each its id, the next
/// linked when it follows at once, whose event is then `SMART_EVENT_LINK`.
pub fn scripts(entry: u32, skills: &[Skill], names: &dyn Names) -> Vec<Row> {
    skills
        .iter()
        .enumerate()
        .map(|(at, skill)| {
            let follows = at > 0 && skills[at - 1].linked;
            let (event, params) = if follows {
                (LINK, [0; 6])
            } else {
                (skill.event, skill.event_params)
            };
            let comment = if skill.comment.is_empty() {
                smart::sentence(skill, names)
            } else {
                skill.comment.clone()
            };
            let mut line = row([
                ("entryorguid", json!(entry)),
                ("source_type", json!(0)),
                ("id", json!(at)),
                (
                    "link",
                    json!(if skill.linked && at + 1 < skills.len() {
                        at + 1
                    } else {
                        0
                    }),
                ),
                ("event_type", json!(event)),
                ("event_phase_mask", json!(skill.phases)),
                ("event_chance", json!(skill.chance)),
                ("event_flags", json!(skill.flags)),
                ("action_type", json!(skill.action)),
                ("target_type", json!(skill.target)),
                ("target_x", json!(skill.position[0])),
                ("target_y", json!(skill.position[1])),
                ("target_z", json!(skill.position[2])),
                ("target_o", json!(skill.position[3])),
                ("comment", json!(comment)),
            ]);
            for (k, value) in params.iter().enumerate() {
                line.insert(format!("event_param{}", k + 1), json!(value));
            }
            for (k, value) in skill.action_params.iter().enumerate() {
                line.insert(format!("action_param{}", k + 1), json!(value));
            }
            for (k, value) in skill.target_params.iter().enumerate() {
                line.insert(format!("target_param{}", k + 1), json!(value));
            }
            line
        })
        .collect()
}

/// The prefabs sent to the server, by entry, as they were sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sent {
    pub prefabs: BTreeMap<u32, Prefab>,
}

impl Sent {
    /// The changes for the panel *World database*: a line a prefab, its rows, the template loaded
    /// again by the bridge; `known` the entries the database has already.
    pub fn pending(&self, known: &dyn Fn(u32) -> bool, names: &dyn Names) -> world_db::Pending {
        let mut out = world_db::Pending {
            owner: "npcs".to_owned(),
            label: "NPCs".to_owned(),
            ..world_db::Pending::default()
        };
        for (entry, prefab) in &self.prefabs {
            if !prefab::RANGE.contains(entry) {
                out.held.push(format!(
                    "the NPC {entry} is outside the block of the editor ({}-{}): not written",
                    prefab::RANGE.start(),
                    prefab::RANGE.end()
                ));
                continue;
            }
            let groups = texts::groups_of(&prefab.texts);
            let lines: usize = groups.iter().map(|group| group.lines.len()).sum();
            let weapons = prefab.equipment.iter().filter(|item| **item != 0).count();
            let summary = format!(
                "{} models, {weapons} weapons, {} skills, {lines} lines of text",
                prefab.models.len(),
                prefab.skills.len()
            );
            out.items.push(world_db::Item {
                key: format!("npc:{entry}"),
                text: format!("NPC {entry} \"{}\": its template", prefab.name("enUS")),
                before: if known(*entry) {
                    "in the database".to_owned()
                } else {
                    "none".to_owned()
                },
                after: summary.clone(),
                detail: format!(
                    "creature_template and its names in every language, creature_template_model, creature_equip_template, smart_scripts, creature_text: {summary}"
                ),
                select: None,
                with: Vec::new(),
            });
            out.changes.extend(rows(*entry, prefab, names));
            out.reloads.push(Reload::Template(*entry));
        }
        out
    }

    /// These prefabs with those of `keys` (as `pending` names them) taken away.
    pub fn discarded(&self, keys: &[String]) -> Sent {
        let mut out = self.clone();
        for key in keys {
            if let Some(entry) = key.strip_prefix("npc:").and_then(|entry| entry.parse().ok()) {
                out.prefabs.remove(&entry);
            }
        }
        out
    }

    pub fn section(&self) -> Value {
        json!({
            "prefabs": self.prefabs.iter().map(|(entry, prefab)| json!({ "entry": entry, "prefab": prefab.to_json() }))
                .collect::<Vec<_>>(),
        })
    }

    pub fn of_section(section: &Value) -> Self {
        Self {
            prefabs: section["prefabs"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|held| {
                            Some((held["entry"].as_u64()? as u32, Prefab::of_json(&held["prefab"]).ok()?))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// The first entry of the editor's block that neither the database (`taken`) nor a prefab
/// (`used`) has.
pub fn free_entry(taken: &dyn Fn(u32) -> bool, used: &[u32]) -> Option<u32> {
    prefab::RANGE
        .clone()
        .find(|entry| !taken(*entry) && !used.contains(entry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smart::Kind;
    use uniwow_api::db::write::{Memory, apply};

    struct NoNames;
    impl Names for NoNames {
        fn named(&self, _: Kind, _: i64) -> Option<String> {
            None
        }
    }

    #[test]
    fn a_prefab_is_the_rows_of_its_template_its_scripts_linked_and_its_texts() {
        let mut prefab = Prefab::default();
        prefab
            .names
            .insert("frFR".to_owned(), ("Garde".to_owned(), String::new()));
        prefab.models.push(prefab::Model {
            display: 16480,
            scale: 1.0,
            probability: 1.0,
        });
        prefab.equipment = [1899, 0, 0];
        let first = Skill {
            linked: true,
            ..Skill::default()
        };
        let second = Skill {
            action: 1,
            ..Skill::default()
        };
        prefab.skills = vec![first.to_json(), second.to_json()];
        let mut line = texts::Line::default();
        line.set("enUS", "Die!".to_owned());
        line.set("frFR", "Meurs !".to_owned());
        prefab.texts = vec![
            texts::Group {
                group: 0,
                name: "aggro".to_owned(),
                lines: vec![line],
            }
            .to_json(),
        ];
        let mut memory = Memory::default();
        // an older script of the entry, and another entry's, kept
        memory.tables.insert(
            "smart_scripts".to_owned(),
            vec![
                row([
                    ("entryorguid", json!(82800)),
                    ("source_type", json!(0)),
                    ("id", json!(5)),
                ]),
                row([("entryorguid", json!(1)), ("source_type", json!(0)), ("id", json!(0))]),
            ],
        );
        apply(&mut memory, &rows(82800, &prefab, &NoNames)).unwrap();
        let template = &memory.tables["creature_template"][0];
        assert_eq!(template["AIName"], "SmartAI");
        assert_eq!(template["name"], "New NPC");
        assert_eq!(template["faction"], 35, "the value a prefab starts with");
        assert_eq!(memory.tables["creature_template_locale"].len(), 1);
        assert_eq!(memory.tables["creature_equip_template"][0]["ItemID1"], 1899);
        // its weapons changed, then taken: its row written again each time, emptied
        let mut changed = prefab.clone();
        changed.equipment = [2504, 0, 0];
        apply(&mut memory, &rows(82800, &changed, &NoNames)).unwrap();
        assert_eq!(memory.tables["creature_equip_template"].len(), 1);
        assert_eq!(memory.tables["creature_equip_template"][0]["ItemID1"], 2504);
        changed.equipment = [0; 3];
        apply(&mut memory, &rows(82800, &changed, &NoNames)).unwrap();
        assert_eq!(
            memory.tables["creature_equip_template"][0]["ItemID1"], 0,
            "emptied, not gone"
        );
        let scripts = &memory.tables["smart_scripts"];
        assert_eq!(scripts.len(), 3, "the other entry's kept, the older one gone");
        let ours: Vec<&Row> = scripts
            .iter()
            .filter(|line| line["entryorguid"] == json!(82800))
            .collect();
        assert_eq!(
            (ours[0]["link"].clone(), ours[1]["event_type"].clone()),
            (json!(1), json!(LINK))
        );
        assert_eq!(memory.tables["creature_text"][0]["Text"], "Die!");
        assert_eq!(memory.tables["creature_text_locale"][0]["Text"], "Meurs !");

        let sent = Sent {
            prefabs: BTreeMap::from([(82800, prefab)]),
        };
        let pending = sent.pending(&|_| false, &NoNames);
        assert_eq!(pending.items[0].key, "npc:82800");
        assert_eq!(pending.reloads, [Reload::Template(82800)]);
        assert_eq!(Sent::of_section(&sent.section()), sent);
        assert!(sent.discarded(&["npc:82800".to_owned()]).prefabs.is_empty());
        assert_eq!(free_entry(&|entry| entry == 82800, &[82801]), Some(82802));
    }
}
