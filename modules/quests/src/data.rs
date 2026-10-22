//! The quests as the module keeps them (step 16.3): their rows read from the world database, one
//! quest at a time, as the user opens it; the changes the user made, laid over them, kept in the
//! project; given to the server as rows. The texts in every language of the client: enUS in the
//! quest's own tables, the others in their tables of locales.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use uniwow_api::db::Rows;
use uniwow_api::db::write::{Row, RowChange, row};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, Reload};

/// The ids of the quests the editor creates: its block of `ID_RANGES.md`; none other is created or
/// deleted.
pub const RANGE: RangeInclusive<u32> = 82100..=82199;

/// The block the editor took at first, the quests made there deleted still: it belongs to
/// mod-transmog-retail now.
pub const FORMER: RangeInclusive<u32> = 97000..=97099;

/// Whether the quest `id` is the editor's, deleted here: of its block, or of its former one.
pub fn ours(id: u32) -> bool {
    RANGE.contains(&id) || FORMER.contains(&id)
}

/// The tables of a quest, its id their key `ID`.
pub const TABLES: [&str; 4] = [
    "quest_template",
    "quest_template_addon",
    "quest_offer_reward",
    "quest_request_items",
];

/// The tables of its texts in the other languages, their key `ID` and `locale`.
pub const LOCALE_TABLES: [&str; 3] = [
    "quest_template_locale",
    "quest_offer_reward_locale",
    "quest_request_items_locale",
];

/// The languages of the client, enUS the quest's own tables.
pub const LOCALES: [&str; 5] = ["enUS", "frFR", "deDE", "esES", "ruRU"];

/// The tables naming the givers and enders of a quest, by `quest`.
pub const RELATIONS: [(&str, bool, bool); 4] = [
    ("creature_queststarter", false, false),
    ("creature_questender", false, true),
    ("gameobject_queststarter", true, false),
    ("gameobject_questender", true, true),
];

/// The flag of a creature's template that makes it give quests (`UNIT_NPC_FLAG_QUESTGIVER`).
pub const QUEST_GIVER: u64 = 0x2;

/// A text of a quest: its label, where enUS keeps it (table, column), where the other languages do
/// (table of locales, column), and whether it takes several lines.
pub struct Text {
    pub label: &'static str,
    pub table: &'static str,
    pub column: &'static str,
    pub locale_table: &'static str,
    pub locale_column: &'static str,
    pub lines: bool,
}

const fn text(
    label: &'static str,
    table: &'static str,
    column: &'static str,
    locale_table: &'static str,
    locale_column: &'static str,
    lines: bool,
) -> Text {
    Text {
        label,
        table,
        column,
        locale_table,
        locale_column,
        lines,
    }
}

pub const TEXTS: [Text; 11] = [
    text(
        "Title",
        "quest_template",
        "LogTitle",
        "quest_template_locale",
        "Title",
        false,
    ),
    text(
        "Description",
        "quest_template",
        "QuestDescription",
        "quest_template_locale",
        "Details",
        true,
    ),
    text(
        "Objectives",
        "quest_template",
        "LogDescription",
        "quest_template_locale",
        "Objectives",
        true,
    ),
    text(
        "While not done",
        "quest_request_items",
        "CompletionText",
        "quest_request_items_locale",
        "CompletionText",
        true,
    ),
    text(
        "Reward",
        "quest_offer_reward",
        "RewardText",
        "quest_offer_reward_locale",
        "RewardText",
        true,
    ),
    text(
        "Done (in the log)",
        "quest_template",
        "QuestCompletionLog",
        "quest_template_locale",
        "CompletedText",
        false,
    ),
    text(
        "Where it ends",
        "quest_template",
        "AreaDescription",
        "quest_template_locale",
        "EndText",
        false,
    ),
    text(
        "Objective 1",
        "quest_template",
        "ObjectiveText1",
        "quest_template_locale",
        "ObjectiveText1",
        false,
    ),
    text(
        "Objective 2",
        "quest_template",
        "ObjectiveText2",
        "quest_template_locale",
        "ObjectiveText2",
        false,
    ),
    text(
        "Objective 3",
        "quest_template",
        "ObjectiveText3",
        "quest_template_locale",
        "ObjectiveText3",
        false,
    ),
    text(
        "Objective 4",
        "quest_template",
        "ObjectiveText4",
        "quest_template_locale",
        "ObjectiveText4",
        false,
    ),
];

/// What a number names, for its name to be shown beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Names {
    None,
    Creature,
    /// A creature, or a game object when below 0.
    CreatureOrObject,
    Quest,
}

/// A number of a quest: its label, its table and column, the column of its count beside it, and
/// what it names.
pub struct Number {
    pub label: &'static str,
    pub table: &'static str,
    pub column: &'static str,
    pub count: Option<&'static str>,
    pub names: Names,
    /// What it is and what the player sees of it, for its tooltip.
    pub hint: &'static str,
}

const fn number(label: &'static str, table: &'static str, column: &'static str, hint: &'static str) -> Number {
    Number {
        label,
        table,
        column,
        count: None,
        names: Names::None,
        hint,
    }
}

const fn named(
    label: &'static str,
    table: &'static str,
    column: &'static str,
    count: Option<&'static str>,
    names: Names,
    hint: &'static str,
) -> Number {
    Number {
        label,
        table,
        column,
        count,
        names,
        hint,
    }
}

const T: &str = "quest_template";
const A: &str = "quest_template_addon";

/// The numbers of a quest drawn as a grid under its page, by section: its title, its numbers; the
/// objectives and rewards are on the page.
pub const SECTIONS: [(&str, &[Number]); 2] = [
    (
        "Level and who takes it",
        &[
            number(
                "Level",
                T,
                "QuestLevel",
                "The level of the quest, shown before its title; its colour for the player and its experience follow it",
            ),
            number(
                "Minimum level",
                T,
                "MinLevel",
                "The level the player needs for it to be offered",
            ),
            number(
                "Maximum level",
                A,
                "MaxLevel",
                "Above this level the quest is no longer offered; 0 for none",
            ),
            number(
                "Group of",
                T,
                "SuggestedGroupNum",
                "The players advised, shown as Suggested Players; 0 for none",
            ),
            number(
                "Time allowed (s)",
                T,
                "TimeAllowed",
                "The seconds the player has to finish it once taken, a timer in the log; 0 for none",
            ),
        ],
    ),
    (
        "Chain",
        &[
            named(
                "After",
                A,
                "PrevQuestID",
                None,
                Names::Quest,
                "The quest to finish before this one is offered (below 0: to have taken, not finished); 0 for none",
            ),
            named(
                "Before",
                A,
                "NextQuestID",
                None,
                Names::Quest,
                "The quest this one opens once finished; 0 for none",
            ),
            named(
                "Offered next",
                T,
                "RewardNextQuest",
                None,
                Names::Quest,
                "The quest the ender offers at once when this one is handed in; 0 for none",
            ),
            number(
                "Exclusive group",
                A,
                "ExclusiveGroup",
                "The quests of one group above 0: one taken, the others no longer offered; below 0: all to finish before the next of the chain; 0 for none",
            ),
        ],
    ),
];

/// A flag of a mask: its bit, its name, what it does in the game (as AzerothCore's QuestDef.h
/// says it).
pub struct Flag {
    pub bit: u32,
    pub name: &'static str,
    pub hint: &'static str,
}

const fn flag(bit: u32, name: &'static str, hint: &'static str) -> Flag {
    Flag { bit, name, hint }
}

/// The flags of `quest_template.Flags` (`QuestFlags` of AzerothCore), sent to the client.
pub const QUEST_FLAGS: [Flag; 20] = [
    flag(
        0x1,
        "Fails on death",
        "The quest fails when the player dies (QUEST_FLAGS_STAY_ALIVE)",
    ),
    flag(
        0x2,
        "Party accept",
        "Meant to ask the party to accept it too; not used by the server (QUEST_FLAGS_PARTY_ACCEPT)",
    ),
    flag(0x4, "Exploration", "Not used by the server (QUEST_FLAGS_EXPLORATION)"),
    flag(
        0x8,
        "Sharable",
        "The player can share it with his party (QUEST_FLAGS_SHARABLE)",
    ),
    flag(
        0x10,
        "Has condition",
        "Not used by the server (QUEST_FLAGS_HAS_CONDITION)",
    ),
    flag(
        0x20,
        "Hide reward POI",
        "Not used by the server (QUEST_FLAGS_HIDE_REWARD_POI)",
    ),
    flag(0x40, "Raid", "A raid quest; not used by the server (QUEST_FLAGS_RAID)"),
    flag(
        0x80,
        "The Burning Crusade",
        "Meant for the expansion alone; not used by the server (QUEST_FLAGS_TBC)",
    ),
    flag(
        0x100,
        "No money from experience",
        "Not used by the server: at the maximum level the experience is not given as money (QUEST_FLAGS_NO_MONEY_FROM_XP)",
    ),
    flag(
        0x200,
        "Hidden rewards",
        "The items and money given are shown only when it is handed in, not in its details nor the log (QUEST_FLAGS_HIDDEN_REWARDS)",
    ),
    flag(
        0x400,
        "Tracking",
        "Rewarded at once when completed, never shown in the quest log (QUEST_FLAGS_TRACKING)",
    ),
    flag(
        0x800,
        "Deprecate reputation",
        "Not used by the server (QUEST_FLAGS_DEPRECATE_REPUTATION)",
    ),
    flag(
        0x1000,
        "Daily",
        "A daily quest: taken again each day (QUEST_FLAGS_DAILY)",
    ),
    flag(
        0x2000,
        "PvP",
        "Having it in the log flags the player for PvP (QUEST_FLAGS_FLAGS_PVP)",
    ),
    flag(
        0x4000,
        "Unavailable",
        "Not offered by its givers the usual way (QUEST_FLAGS_UNAVAILABLE)",
    ),
    flag(
        0x8000,
        "Weekly",
        "A weekly quest: taken again each week (QUEST_FLAGS_WEEKLY)",
    ),
    flag(
        0x10000,
        "Auto-complete",
        "Completed as soon as it is taken (QUEST_FLAGS_AUTOCOMPLETE)",
    ),
    flag(
        0x20000,
        "Item in tracker",
        "Its usable item is shown in the quest tracker (QUEST_FLAGS_DISPLAY_ITEM_IN_TRACKER)",
    ),
    flag(
        0x40000,
        "Objective text as completion",
        "Its objective text is used as its completion text (QUEST_FLAGS_OBJ_TEXT)",
    ),
    flag(
        0x80000,
        "Auto-accept (client)",
        "Accepted at once by the client; no quest of 3.3.5a has it (QUEST_FLAGS_AUTO_ACCEPT)",
    ),
];

/// The flags of `quest_template_addon.SpecialFlags`, the server's alone (`QuestSpecialFlags`).
pub const SPECIAL_FLAGS: [Flag; 9] = [
    flag(
        0x1,
        "Repeatable",
        "It can be done again and again (QUEST_SPECIAL_FLAGS_REPEATABLE)",
    ),
    flag(
        0x2,
        "Exploration or event",
        "Completed by exploring an area, a spell or a script, not by its objectives (QUEST_SPECIAL_FLAGS_EXPLORATION_OR_EVENT)",
    ),
    flag(
        0x4,
        "Auto-accept",
        "Accepted at once when offered (QUEST_SPECIAL_FLAGS_AUTO_ACCEPT)",
    ),
    flag(
        0x8,
        "Dungeon Finder",
        "Used by the Dungeon Finder for its rewards (QUEST_SPECIAL_FLAGS_DF_QUEST)",
    ),
    flag(0x10, "Monthly", "Taken again each month (QUEST_SPECIAL_FLAGS_MONTHLY)"),
    flag(
        0x20,
        "Credit by a spell",
        "Its creatures or objects are counted by a spell cast on them, not by a kill (QUEST_SPECIAL_FLAGS_CAST)",
    ),
    flag(
        0x40,
        "No reputation spillover",
        "Its reputation is not shared with the allied factions (QUEST_SPECIAL_FLAGS_NO_REP_SPILLOVER)",
    ),
    flag(
        0x80,
        "Can fail in any state",
        "It can fail whatever its state (QUEST_SPECIAL_FLAGS_CAN_FAIL_IN_ANY_STATE)",
    ),
    flag(
        0x100,
        "Not for Loremaster",
        "It does not count towards the Loremaster achievement (QUEST_SPECIAL_FLAGS_NO_LOREMASTER_COUNT)",
    ),
];

/// The ways a quest is taken (`quest_template.QuestType`).
pub const QUEST_TYPES: [(i64, &str, &str); 3] = [
    (0, "Auto-complete", "Completed as soon as it is taken (QuestType 0)"),
    (1, "Disabled", "Never offered (QuestType 1)"),
    (2, "Enabled", "Offered and done the usual way (QuestType 2)"),
];

/// The rows of a quest the module read: by table and locale (empty for the quest's own tables),
/// none where it has none; and its givers and enders, by relation table, their templates' entries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub rows: BTreeMap<(String, String), Row>,
    pub relations: BTreeMap<String, Vec<u32>>,
}

/// The rows of `rows` as maps of their columns.
pub fn rows_of(rows: &Rows) -> Vec<Row> {
    rows.rows
        .iter()
        .map(|values| rows.columns.iter().cloned().zip(values.iter().cloned()).collect())
        .collect()
}

/// What the module reads of the quest `id`: its tables, its tables of locales, its givers and
/// enders; each a statement and its parameters.
pub fn queries(id: u32) -> Vec<(String, String, Vec<Value>)> {
    let mut out = Vec::new();
    for table in TABLES.iter().chain(LOCALE_TABLES.iter()) {
        out.push((
            (*table).to_owned(),
            format!("SELECT * FROM {table} WHERE ID = ?"),
            vec![json!(id)],
        ));
    }
    for (table, _, _) in RELATIONS {
        out.push((
            table.to_owned(),
            format!("SELECT id FROM {table} WHERE quest = ?"),
            vec![json!(id)],
        ));
    }
    out
}

impl Loaded {
    /// What `answers` (a table, its rows, by `queries`) tell of a quest; a table refused left out.
    pub fn of_answers(answers: &[(String, Rows)]) -> Self {
        let mut out = Loaded::default();
        for (table, rows) in answers {
            if RELATIONS.iter().any(|(relation, _, _)| relation == table) {
                let entries = rows_of(rows)
                    .iter()
                    .filter_map(|row| row.get("id").and_then(as_number).map(|id| id as u32))
                    .collect();
                out.relations.insert(table.clone(), entries);
                continue;
            }
            let locales = LOCALE_TABLES.contains(&table.as_str());
            for row in rows_of(rows) {
                let locale = if locales {
                    row.get("locale").and_then(Value::as_str).unwrap_or_default().to_owned()
                } else {
                    String::new()
                };
                out.rows.insert((table.clone(), locale), row);
            }
        }
        out
    }
}

pub fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// What the user did to a quest: created (blank, or a copy of the quest named), deleted, and its
/// values changed by table and locale.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuestEdit {
    pub created: Option<Option<u32>>,
    pub deleted: bool,
    pub set: BTreeMap<(String, String), Row>,
}

/// The changes of the quests the user made, and the flags given to the templates of creatures that
/// give them, by entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edits {
    pub quests: BTreeMap<u32, QuestEdit>,
    pub flags: BTreeMap<u32, u64>,
}

impl Edits {
    /// The value of `column` of `table` in `locale` of the quest `id`, as the user changed it: his
    /// value, or the row read, or for a quest created that of the quest it copies as the user
    /// changed it, else none.
    pub fn value(
        &self,
        loaded: &BTreeMap<u32, Loaded>,
        id: u32,
        table: &str,
        locale: &str,
        column: &str,
    ) -> Option<Value> {
        let key = (table.to_owned(), locale.to_owned());
        let edit = self.quests.get(&id);
        if let Some(value) = edit.and_then(|edit| edit.set.get(&key)?.get(column)) {
            return Some(value.clone());
        }
        match edit.and_then(|edit| edit.created) {
            Some(Some(copied)) if copied != id => self.value(loaded, copied, table, locale, column),
            Some(_) => None,
            None => loaded.get(&id)?.rows.get(&key)?.get(column).cloned(),
        }
    }

    /// `column` of `table` in `locale` of the quest `id` given `value`, its row made where it had
    /// none (the key given then).
    pub fn set(&mut self, id: u32, table: &str, locale: &str, column: &str, value: Value) {
        let edit = self.quests.entry(id).or_default();
        let row = edit.set.entry((table.to_owned(), locale.to_owned())).or_default();
        row.insert(column.to_owned(), value);
    }

    /// The rows of the changes, and what the server loads again; a quest created or deleted outside
    /// `RANGE` refused, said.
    /// The changes as lines for the panel *World database*: each quest created, deleted or changed
    /// (its fields before and after in its detail), each template given the flag of a giver.
    pub fn items(&self, loaded: &BTreeMap<u32, Loaded>) -> Vec<world_db::Item> {
        let shown = |value: Option<&Value>| match value {
            None | Some(Value::Null) => "—".to_owned(),
            Some(Value::String(text)) => format!("\"{text}\""),
            Some(other) => other.to_string(),
        };
        let mut out = Vec::new();
        for (id, edit) in &self.quests {
            let title = self
                .value(loaded, *id, "quest_template", "", "LogTitle")
                .and_then(|title| title.as_str().map(str::to_owned))
                .unwrap_or_default();
            let read = loaded.get(id);
            let mut fields = Vec::new();
            for ((table, locale), set) in &edit.set {
                for (column, value) in set {
                    let before = read.and_then(|read| read.rows.get(&(table.clone(), locale.clone()))?.get(column));
                    if before == Some(value) {
                        continue;
                    }
                    let place = if locale.is_empty() {
                        format!("{table}.{column}")
                    } else {
                        format!("{table}.{column} ({locale})")
                    };
                    fields.push(format!("{place}: {} → {}", shown(before), shown(Some(value))));
                }
            }
            let (what, before, after) = if edit.deleted {
                ("deleted", "in the database".to_owned(), "none".to_owned())
            } else if edit.created.is_some() && !loaded.contains_key(id) {
                ("created", "none".to_owned(), format!("{} fields", fields.len()))
            } else {
                (
                    "changed",
                    format!("{} fields", fields.len()),
                    format!("{} fields changed", fields.len()),
                )
            };
            out.push(world_db::Item {
                key: format!("quest:{id}"),
                text: format!("Quest {id} \"{title}\": {what}"),
                before,
                after,
                detail: fields.join(
                    "
",
                ),
                select: None,
                with: Vec::new(),
            });
        }
        for (entry, flags) in &self.flags {
            out.push(world_db::Item {
                key: format!("flags:{entry}"),
                text: format!("Template of creature {entry}: the flags of a giver of quests"),
                before: "—".to_owned(),
                after: format!("npcflag {flags}"),
                detail: String::new(),
                select: None,
                with: Vec::new(),
            });
        }
        out
    }

    /// These edits with the changes of `keys` (as `items` names them) taken away.
    pub fn discarded(&self, keys: &[String]) -> Edits {
        let mut out = self.clone();
        for key in keys {
            if let Some(id) = key.strip_prefix("quest:").and_then(|id| id.parse().ok()) {
                out.quests.remove(&id);
            }
            if let Some(entry) = key.strip_prefix("flags:").and_then(|entry| entry.parse().ok()) {
                out.flags.remove(&entry);
            }
        }
        out
    }

    pub fn pending(&self, loaded: &BTreeMap<u32, Loaded>) -> world_db::Pending {
        let mut out = world_db::Pending {
            owner: "quests".to_owned(),
            label: "Quests".to_owned(),
            items: self.items(loaded),
            ..world_db::Pending::default()
        };
        for (id, edit) in &self.quests {
            if (edit.created.is_some() && !RANGE.contains(id)) || (edit.deleted && !ours(*id)) {
                out.held.push(format!(
                    "the quest {id} is outside the block of the editor ({}-{}): not created nor deleted",
                    RANGE.start(),
                    RANGE.end()
                ));
                continue;
            }
            if edit.deleted {
                // Never in the database: nothing to take away.
                if edit.created.is_some() && !loaded.contains_key(id) {
                    continue;
                }
                for table in TABLES.iter().chain(LOCALE_TABLES.iter()) {
                    out.changes.push(RowChange::Delete {
                        table: (*table).to_owned(),
                        key: row([("ID", json!(id))]),
                    });
                }
                for (table, _, _) in RELATIONS {
                    out.changes.push(RowChange::Delete {
                        table: table.to_owned(),
                        key: row([("quest", json!(id))]),
                    });
                }
                // Taken from every character too, for a quest of its id made again to be offered.
                out.reloads.push(Reload::ForgetQuest(*id));
                continue;
            }
            // The tables of a quest created: each made, a copy of the quest it copies where it has it.
            let mut keys: Vec<(String, String)> = edit.set.keys().cloned().collect();
            if let Some(copied) = edit.created {
                for table in TABLES {
                    keys.push((table.to_owned(), String::new()));
                }
                if let Some(source) = copied.and_then(|copied| loaded.get(&copied)) {
                    keys.extend(source.rows.keys().cloned());
                }
            }
            keys.sort();
            keys.dedup();
            for (table, locale) in keys {
                let key = if locale.is_empty() {
                    row([("ID", json!(id))])
                } else {
                    row([("ID", json!(id)), ("locale", json!(locale))])
                };
                // A copy takes the quest it copies as the user changed it, then its own values.
                let mut set = edit
                    .created
                    .flatten()
                    .and_then(|copied| {
                        self.quests
                            .get(&copied)?
                            .set
                            .get(&(table.clone(), locale.clone()))
                            .cloned()
                    })
                    .unwrap_or_default();
                set.extend(
                    edit.set
                        .get(&(table.clone(), locale.clone()))
                        .cloned()
                        .unwrap_or_default(),
                );
                let from = edit
                    .created
                    .flatten()
                    .filter(|copied| {
                        loaded
                            .get(copied)
                            .is_some_and(|source| source.rows.contains_key(&(table.clone(), locale.clone())))
                    })
                    .map(|copied| {
                        let mut from = key.clone();
                        from.insert("ID".to_owned(), json!(copied));
                        from
                    });
                // A quest read: its changes alone; one created: every table.
                if set.is_empty() && edit.created.is_none() {
                    continue;
                }
                out.changes.push(RowChange::Put { table, key, set, from });
            }
        }
        for (entry, flags) in &self.flags {
            out.changes.push(RowChange::Put {
                table: "creature_template".to_owned(),
                key: row([("entry", json!(entry))]),
                set: row([("npcflag", json!(flags))]),
                from: None,
            });
        }
        if !self.flags.is_empty() {
            out.held.push(format!(
                "{} creatures made givers of quests: their marks shown once they spawn again",
                self.flags.len()
            ));
        }
        if !out.changes.is_empty() {
            out.reloads.push(Reload::Quests);
        }
        out.reloads.sort();
        out.reloads.dedup();
        out
    }

    pub fn section(&self) -> Value {
        json!({
            "quests": self.quests.iter().map(|(id, edit)| json!({
                "id": id,
                "created": edit.created.map(|copied| json!({ "copied": copied })),
                "deleted": edit.deleted,
                "set": edit.set.iter().map(|((table, locale), values)| json!({
                    "table": table, "locale": locale, "values": values,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "flags": self.flags.iter().map(|(entry, flags)| json!({ "entry": entry, "npcflag": flags }))
                .collect::<Vec<_>>(),
        })
    }

    pub fn of_section(section: &Value) -> Self {
        let list = |value: &Value| value.as_array().cloned().unwrap_or_default();
        Self {
            quests: list(&section["quests"])
                .iter()
                .filter_map(|quest| {
                    let edit = QuestEdit {
                        created: quest["created"]
                            .as_object()
                            .map(|created| created.get("copied").and_then(Value::as_u64).map(|id| id as u32)),
                        deleted: quest["deleted"].as_bool().unwrap_or(false),
                        set: list(&quest["set"])
                            .iter()
                            .filter_map(|set| {
                                let values = set["values"]
                                    .as_object()?
                                    .iter()
                                    .map(|(column, value)| (column.clone(), value.clone()))
                                    .collect();
                                Some((
                                    (set["table"].as_str()?.to_owned(), set["locale"].as_str()?.to_owned()),
                                    values,
                                ))
                            })
                            .collect(),
                    };
                    Some((quest["id"].as_u64()? as u32, edit))
                })
                .collect(),
            flags: list(&section["flags"])
                .iter()
                .filter_map(|flag| Some((flag["entry"].as_u64()? as u32, flag["npcflag"].as_u64()?)))
                .collect(),
        }
    }
}

/// The first id of `RANGE` neither the database (`taken`) nor the user has.
pub fn free_id(taken: &BTreeMap<u32, String>, edits: &Edits) -> Option<u32> {
    RANGE
        .clone()
        .find(|id| !taken.contains_key(id) && !edits.quests.contains_key(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::db::write::{Memory, apply};

    fn rows(columns: &[&str], values: Vec<Vec<Value>>) -> Rows {
        Rows {
            columns: columns.iter().map(|column| (*column).to_owned()).collect(),
            rows: values,
        }
    }

    #[test]
    fn each_quest_changed_is_a_line_its_fields_before_and_after_in_its_detail() {
        let loaded = loaded();
        let mut edits = Edits::default();
        edits.set(783, "quest_template", "", "LogTitle", json!("A Threat Without"));
        edits.set(783, "quest_template", "", "QuestLevel", json!(1));
        let found = edits.items(&loaded);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "quest:783");
        assert_eq!(found[0].text, "Quest 783 \"A Threat Without\": changed");
        assert_eq!(
            found[0].detail, "quest_template.LogTitle: \"A Threat Within\" → \"A Threat Without\"",
            "the level unchanged, left out"
        );
        assert_eq!(edits.discarded(&["quest:783".to_owned()]), Edits::default());
    }

    fn loaded() -> BTreeMap<u32, Loaded> {
        let read = Loaded::of_answers(&[
            (
                "quest_template".to_owned(),
                rows(
                    &["ID", "LogTitle", "QuestLevel"],
                    vec![vec![json!(783), json!("A Threat Within"), json!(1)]],
                ),
            ),
            (
                "quest_template_locale".to_owned(),
                rows(
                    &["ID", "locale", "Title"],
                    vec![vec![json!(783), json!("frFR"), json!("Une menace intérieure")]],
                ),
            ),
            ("quest_template_addon".to_owned(), rows(&["ID"], Vec::new())),
            (
                "creature_queststarter".to_owned(),
                rows(&["id"], vec![vec![json!(823)]]),
            ),
        ]);
        BTreeMap::from([(783, read)])
    }

    #[test]
    fn each_flag_is_one_bit_of_its_own() {
        for flags in [&QUEST_FLAGS[..], &SPECIAL_FLAGS[..]] {
            let mut seen = 0u32;
            for flag in flags {
                assert_eq!(flag.bit.count_ones(), 1, "{}", flag.name);
                assert_eq!(seen & flag.bit, 0, "{} twice", flag.name);
                assert!(!flag.hint.is_empty());
                seen |= flag.bit;
            }
        }
    }

    #[test]
    fn a_quest_is_read_by_table_and_language_its_givers_with_it() {
        let loaded = loaded();
        let quest = &loaded[&783];
        assert_eq!(quest.rows.len(), 2);
        assert_eq!(quest.relations["creature_queststarter"], [823]);
        let edits = Edits::default();
        assert_eq!(
            edits.value(&loaded, 783, "quest_template_locale", "frFR", "Title"),
            Some(json!("Une menace intérieure"))
        );
        assert_eq!(
            edits.value(&loaded, 783, "quest_template_locale", "deDE", "Title"),
            None
        );
        assert_eq!(queries(783).len(), TABLES.len() + LOCALE_TABLES.len() + RELATIONS.len());
    }

    #[test]
    fn a_quest_changed_one_copied_and_one_deleted_are_the_rows_the_server_is_given() {
        let loaded = loaded();
        let mut memory = Memory::default();
        memory.tables.insert(
            "quest_template".to_owned(),
            vec![row([
                ("ID", json!(783)),
                ("LogTitle", json!("A Threat Within")),
                ("QuestLevel", json!(1)),
            ])],
        );
        memory.tables.insert(
            "quest_template_locale".to_owned(),
            vec![row([
                ("ID", json!(783)),
                ("locale", json!("frFR")),
                ("Title", json!("Une menace intérieure")),
            ])],
        );
        let mut edits = Edits::default();
        // 783 changed: its level, its German title.
        edits.set(783, "quest_template", "", "QuestLevel", json!(3));
        edits.set(783, "quest_template_locale", "deDE", "Title", json!("Eine Bedrohung"));
        // A copy of 783 in the editor's block, its title its own.
        let id = free_id(&BTreeMap::from([(82100, "taken".to_owned())]), &edits).unwrap();
        assert_eq!(id, 82101);
        edits.quests.insert(
            id,
            QuestEdit {
                created: Some(Some(783)),
                ..QuestEdit::default()
            },
        );
        edits.set(id, "quest_template", "", "LogTitle", json!("A Threat Without"));
        assert_eq!(
            edits.value(&loaded, id, "quest_template", "", "QuestLevel"),
            Some(json!(3)),
            "copied as the user changed it"
        );
        // Outside the block: not created.
        edits.quests.insert(
            5,
            QuestEdit {
                created: Some(None),
                ..QuestEdit::default()
            },
        );
        // A giver made one.
        edits.flags.insert(823, 3);
        let pending = edits.pending(&loaded);
        assert_eq!(pending.held.len(), 2, "{:?}", pending.held);
        assert_eq!(pending.reloads, [Reload::Quests]);
        apply(&mut memory, &pending.changes).unwrap();

        let quests = &memory.tables["quest_template"];
        assert_eq!(quests.len(), 2);
        assert_eq!(quests[0]["QuestLevel"], json!(3));
        assert_eq!(
            (
                quests[1]["ID"].clone(),
                quests[1]["LogTitle"].clone(),
                quests[1]["QuestLevel"].clone()
            ),
            (json!(82101), json!("A Threat Without"), json!(3))
        );
        let locales = &memory.tables["quest_template_locale"];
        assert_eq!(locales.len(), 3, "deDE added; frFR copied");
        assert!(
            locales
                .iter()
                .any(|row| row["ID"] == json!(82101) && row["locale"] == json!("frFR"))
        );
        assert_eq!(memory.tables["quest_template_addon"].len(), 1, "made for the copy");
        assert_eq!(
            memory.tables["creature_template"],
            [row([("entry", json!(823)), ("npcflag", json!(3))])]
        );

        // Deleted: every row of it.
        let mut deleted = Edits::default();
        deleted.quests.insert(
            82101,
            QuestEdit {
                deleted: true,
                ..QuestEdit::default()
            },
        );
        let loaded_copy = BTreeMap::from([(82101, Loaded::default())]);
        let pending = deleted.pending(&loaded_copy);
        assert!(
            pending.reloads.contains(&Reload::ForgetQuest(82101)),
            "taken from the characters too"
        );
        apply(&mut memory, &pending.changes).unwrap();
        assert_eq!(memory.tables["quest_template"].len(), 1);
        assert!(
            memory.tables["quest_template_locale"]
                .iter()
                .all(|row| row["ID"] != json!(82101))
        );

        // Kept in the project as they were.
        assert_eq!(Edits::of_section(&edits.section()), edits);
    }
}
