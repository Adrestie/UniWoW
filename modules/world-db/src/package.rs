//! A change kept as a file (step 16.9), `.uniwow`, JSON a person can read: the rows a change made
//! (each key's rows before and after), the rows it refers to and those that belong to what it made
//! (a quest's texts and givers, a patrol's addon and spawn, a group's leader and members), what the
//! server loads again. Imported into another server, the rows of its keys there are read first:
//! an id taken by something else, a dependency missing, a row changed there are problems the user
//! resolves (renumber within a free id, the references followed; overwrite; keep the target's; link
//! to an entity there; leave out) before one transaction writes the rest.

use std::collections::{BTreeMap, BTreeSet};

use uniwow_api::db::write::{Entry, Row, RowChange, row};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::Reload;

/// The extension of the files.
pub const EXTENSION: &str = "uniwow";

/// What a row can name: a spawn, a quest, a path, a template.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Creature,
    GameObject,
    Quest,
    Path,
    CreatureTemplate,
    ObjectTemplate,
    Item,
}

impl Kind {
    /// The table and column whose rows are the entity.
    pub fn home(self) -> (&'static str, &'static str) {
        match self {
            Kind::Creature => ("creature", "guid"),
            Kind::GameObject => ("gameobject", "guid"),
            Kind::Quest => ("quest_template", "ID"),
            Kind::Path => ("waypoint_data", "id"),
            Kind::CreatureTemplate => ("creature_template", "entry"),
            Kind::ObjectTemplate => ("gameobject_template", "entry"),
            Kind::Item => ("item_template", "entry"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Creature => "creature",
            Kind::GameObject => "game object",
            Kind::Quest => "quest",
            Kind::Path => "path",
            Kind::CreatureTemplate => "template of creature",
            Kind::ObjectTemplate => "template of game object",
            Kind::Item => "item",
        }
    }

    /// Whether the editor may give it another id on import.
    pub fn renumbered(self) -> bool {
        matches!(self, Kind::Creature | Kind::GameObject | Kind::Quest | Kind::Path)
    }
}

/// The columns that name an entity: a table, a column, what it names. The columns of a table keyed
/// by the entity name it too.
pub const NAMES: &[(&str, &str, Kind)] = &[
    ("creature", "guid", Kind::Creature),
    ("creature", "id", Kind::CreatureTemplate),
    ("creature", "id1", Kind::CreatureTemplate),
    ("creature_addon", "guid", Kind::Creature),
    ("creature_addon", "path_id", Kind::Path),
    ("game_event_creature", "guid", Kind::Creature),
    ("creature_formations", "leaderGUID", Kind::Creature),
    ("creature_formations", "memberGUID", Kind::Creature),
    ("gameobject", "guid", Kind::GameObject),
    ("gameobject", "id", Kind::ObjectTemplate),
    ("game_event_gameobject", "guid", Kind::GameObject),
    ("waypoint_data", "id", Kind::Path),
    ("creature_template", "entry", Kind::CreatureTemplate),
    ("quest_template", "ID", Kind::Quest),
    ("quest_template", "RewardNextQuest", Kind::Quest),
    ("quest_template", "StartItem", Kind::Item),
    ("quest_template", "RequiredItemId1", Kind::Item),
    ("quest_template", "RequiredItemId2", Kind::Item),
    ("quest_template", "RequiredItemId3", Kind::Item),
    ("quest_template", "RequiredItemId4", Kind::Item),
    ("quest_template", "RequiredItemId5", Kind::Item),
    ("quest_template", "RequiredItemId6", Kind::Item),
    ("quest_template", "RewardItem1", Kind::Item),
    ("quest_template", "RewardItem2", Kind::Item),
    ("quest_template", "RewardItem3", Kind::Item),
    ("quest_template", "RewardItem4", Kind::Item),
    ("quest_template", "RewardChoiceItemID1", Kind::Item),
    ("quest_template", "RewardChoiceItemID2", Kind::Item),
    ("quest_template", "RewardChoiceItemID3", Kind::Item),
    ("quest_template", "RewardChoiceItemID4", Kind::Item),
    ("quest_template", "RewardChoiceItemID5", Kind::Item),
    ("quest_template", "RewardChoiceItemID6", Kind::Item),
    ("quest_template_addon", "ID", Kind::Quest),
    ("quest_template_addon", "PrevQuestID", Kind::Quest),
    ("quest_template_addon", "NextQuestID", Kind::Quest),
    ("quest_offer_reward", "ID", Kind::Quest),
    ("quest_request_items", "ID", Kind::Quest),
    ("quest_template_locale", "ID", Kind::Quest),
    ("quest_offer_reward_locale", "ID", Kind::Quest),
    ("quest_request_items_locale", "ID", Kind::Quest),
    ("creature_queststarter", "id", Kind::CreatureTemplate),
    ("creature_queststarter", "quest", Kind::Quest),
    ("creature_questender", "id", Kind::CreatureTemplate),
    ("creature_questender", "quest", Kind::Quest),
    ("gameobject_queststarter", "id", Kind::ObjectTemplate),
    ("gameobject_queststarter", "quest", Kind::Quest),
    ("gameobject_questender", "id", Kind::ObjectTemplate),
    ("gameobject_questender", "quest", Kind::Quest),
];

/// What belongs to an entity the change made, kept with it: the tables keyed by it.
pub const BELONGS: &[(Kind, &str, &str)] = &[
    (Kind::Creature, "creature_addon", "guid"),
    (Kind::Creature, "game_event_creature", "guid"),
    (Kind::Creature, "creature_formations", "leaderGUID"),
    (Kind::GameObject, "game_event_gameobject", "guid"),
    (Kind::Path, "creature_addon", "path_id"),
    (Kind::Quest, "quest_template", "ID"),
    (Kind::Quest, "quest_template_addon", "ID"),
    (Kind::Quest, "quest_offer_reward", "ID"),
    (Kind::Quest, "quest_request_items", "ID"),
    (Kind::Quest, "quest_template_locale", "ID"),
    (Kind::Quest, "quest_offer_reward_locale", "ID"),
    (Kind::Quest, "quest_request_items_locale", "ID"),
    (Kind::Quest, "creature_queststarter", "quest"),
    (Kind::Quest, "creature_questender", "quest"),
    (Kind::Quest, "gameobject_queststarter", "quest"),
    (Kind::Quest, "gameobject_questender", "quest"),
];

fn whole(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| number.as_f64().map(|real| real as i64)),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// The entities a row of `table` names: what, by which column, which id; none for 0.
pub fn named(table: &str, row: &Row) -> Vec<(Kind, &'static str, i64)> {
    NAMES
        .iter()
        .filter(|(named, _, _)| *named == table)
        .filter_map(|(_, column, kind)| {
            let id = whole(row.get(*column)?)?;
            (id > 0).then_some((*kind, *column, id))
        })
        .collect()
}

/// A row kept with a change: its table and key, its rows on the server it came from, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    pub table: String,
    pub key: Row,
    pub rows: Vec<Row>,
    pub why: String,
}

/// A change kept as a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub label: String,
    pub made: String,
    pub server: String,
    pub database: String,
    pub rows: Vec<Entry>,
    pub references: Vec<Reference>,
    pub reloads: Vec<Reload>,
}

impl Package {
    pub fn to_json(&self) -> Value {
        json!({
            "format": "uniwow-change",
            "version": 1,
            "label": self.label,
            "made": self.made,
            "from": { "server": self.server, "database": self.database },
            "rows": self.rows.iter().map(Entry::to_json).collect::<Vec<_>>(),
            "references": self.references.iter().map(|reference| json!({
                "table": reference.table, "key": reference.key, "rows": reference.rows, "why": reference.why,
            })).collect::<Vec<_>>(),
            "reloads": self.reloads.iter().map(crate::reload_json).collect::<Vec<_>>(),
        })
    }

    pub fn of_json(value: &Value) -> Result<Package, String> {
        if value["format"].as_str() != Some("uniwow-change") {
            return Err("not a change of UniWoW".to_owned());
        }
        if value["version"].as_u64() != Some(1) {
            return Err(format!(
                "a change of the version {} of the format, this editor reads the 1",
                value["version"]
            ));
        }
        let row_of = |value: &Value| -> Option<Row> {
            Some(
                value
                    .as_object()?
                    .iter()
                    .map(|(column, value)| (column.clone(), value.clone()))
                    .collect(),
            )
        };
        let list = |key: &str| value[key].as_array().cloned().unwrap_or_default();
        let rows = list("rows")
            .iter()
            .map(Entry::of_json)
            .collect::<Option<Vec<_>>>()
            .ok_or("a row of the change is not whole")?;
        let references = list("references")
            .iter()
            .map(|reference| {
                Some(Reference {
                    table: reference["table"].as_str()?.to_owned(),
                    key: row_of(&reference["key"])?,
                    rows: reference["rows"]
                        .as_array()?
                        .iter()
                        .map(row_of)
                        .collect::<Option<_>>()?,
                    why: reference["why"].as_str().unwrap_or_default().to_owned(),
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or("a reference of the change is not whole")?;
        Ok(Package {
            label: value["label"].as_str().unwrap_or_default().to_owned(),
            made: value["made"].as_str().unwrap_or_default().to_owned(),
            server: value["from"]["server"].as_str().unwrap_or_default().to_owned(),
            database: value["from"]["database"].as_str().unwrap_or_default().to_owned(),
            rows,
            references,
            reloads: list("reloads").iter().filter_map(crate::reload_of).collect(),
        })
    }
}

/// The rows to keep with `rows` as references: the entities they name (a template, a quest of the
/// chain, the spawn a path or a group is of), and what belongs to the entities they make (a quest's
/// texts and givers, a spawn's addon and events), each a table, a key and why; once each, those of
/// `rows` left out.
pub fn wanted(rows: &[Entry]) -> Vec<(String, Row, String)> {
    let held: BTreeSet<(String, String)> = rows
        .iter()
        .map(|entry| (entry.table.clone(), json!(entry.key).to_string()))
        .collect();
    let tables: BTreeSet<&str> = rows.iter().map(|entry| entry.table.as_str()).collect();
    let mut out: Vec<(String, Row, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut add = |table: &str, key: Row, why: String| {
        let id = (table.to_owned(), json!(key).to_string());
        if !held.contains(&id) && seen.insert(id) {
            out.push((table.to_owned(), key, why));
        }
    };
    for entry in rows {
        for after in entry.after.iter().chain(&entry.before) {
            for (kind, column, id) in named(&entry.table, after) {
                let (home, key) = kind.home();
                let what = format!("{} {id}, named by {}.{column}", kind.name(), entry.table);
                if home != entry.table {
                    add(home, row([(key, json!(id))]), what.clone());
                }
                for (owner, table, column) in BELONGS {
                    if *owner == kind && !tables.contains(table) {
                        add(
                            table,
                            row([(*column, json!(id))]),
                            format!("of the {} {id}", kind.name()),
                        );
                    }
                }
            }
        }
    }
    out
}

/// A problem met importing a change, by the place of its row or reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A row the change made where none was: the target has rows of its key, something else.
    Taken(usize),
    /// A row the change changed: on the target not what it was before.
    Changed(usize),
    /// A reference the target has none of.
    Missing(usize),
    /// A reference the target has otherwise.
    Differs(usize),
}

/// How a problem is resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The entity the row makes given a free id on the target, the references followed.
    Renumber,
    /// The change's rows written over the target's.
    Overwrite,
    /// The target's rows kept, the change's left out.
    Keep,
    /// The reference's rows inserted on the target.
    Insert,
    /// The entity named another, of this id, on the target.
    Link(String),
    /// Left out.
    Ignore,
}

fn sorted(rows: &[Row]) -> Vec<String> {
    let mut out: Vec<String> = rows.iter().map(|row| json!(row).to_string()).collect();
    out.sort();
    out
}

/// The problems of importing `package` into a target whose rows of each row's key are `rows` and of
/// each reference's are `references`, in their orders.
pub fn problems(package: &Package, rows: &[Vec<Row>], references: &[Vec<Row>]) -> Vec<Problem> {
    let mut out = Vec::new();
    for (at, (entry, there)) in package.rows.iter().zip(rows).enumerate() {
        if sorted(&entry.before) == sorted(there) || sorted(&entry.after) == sorted(there) {
            continue;
        }
        out.push(if entry.before.is_empty() {
            Problem::Taken(at)
        } else {
            Problem::Changed(at)
        });
    }
    for (at, (reference, there)) in package.references.iter().zip(references).enumerate() {
        if there.is_empty() {
            if !reference.rows.is_empty() {
                out.push(Problem::Missing(at));
            }
        } else if sorted(&reference.rows) != sorted(there) {
            out.push(Problem::Differs(at));
        }
    }
    out
}

/// The choices offered for `problem`, the first the default.
pub fn choices(problem: &Problem, package: &Package) -> Vec<Choice> {
    match problem {
        Problem::Taken(at) => {
            let entry = &package.rows[*at];
            let renumbered = entity_of(&entry.table, &entry.key).is_some_and(|(kind, _)| kind.renumbered());
            let mut out = Vec::new();
            if renumbered {
                out.push(Choice::Renumber);
            }
            out.extend([Choice::Overwrite, Choice::Keep]);
            out
        }
        Problem::Changed(_) => vec![Choice::Overwrite, Choice::Keep],
        Problem::Missing(at) => {
            let reference = &package.references[*at];
            let mut out = Vec::new();
            if uniwow_api::db::write::TABLES.contains(&reference.table.as_str()) {
                out.push(Choice::Insert);
            }
            if entity_of(&reference.table, &reference.key).is_some() {
                out.push(Choice::Link(String::new()));
            }
            out.push(Choice::Ignore);
            out
        }
        Problem::Differs(_) => vec![Choice::Keep, Choice::Overwrite],
    }
}

/// The entity a key of `table` is the home of: its kind and id.
pub fn entity_of(table: &str, key: &Row) -> Option<(Kind, i64)> {
    [
        Kind::Creature,
        Kind::GameObject,
        Kind::Quest,
        Kind::Path,
        Kind::CreatureTemplate,
        Kind::ObjectTemplate,
        Kind::Item,
    ]
    .into_iter()
    .find_map(|kind| {
        let (home, column) = kind.home();
        (home == table && key.len() == 1)
            .then(|| Some((kind, whole(key.get(column)?)?)))
            .flatten()
    })
}

/// `row` of `table` with the entities `renamed` (a kind and an old id, the new id) named anew.
fn renamed_row(table: &str, row: &Row, renamed: &BTreeMap<(Kind, i64), i64>) -> Row {
    let mut out = row.clone();
    for (kind, column, id) in named(table, row) {
        if let Some(new) = renamed.get(&(kind, id)) {
            out.insert(column.to_owned(), json!(new));
        }
    }
    out
}

/// The changes of an import, what the server loads again, the entities renamed: a kind and an old
/// id, the new.
pub type Resolved = (Vec<RowChange>, Vec<Reload>, BTreeMap<(Kind, i64), i64>);

/// What importing `package` writes once `problems` are resolved by `chosen` (one for each, in their
/// order), the ids of the entities renumbered taken from `fresh` (a kind, the next free id): the
/// changes, what the server loads again, the entities renumbered.
pub fn resolved(
    package: &Package,
    problems: &[Problem],
    chosen: &[Choice],
    mut fresh: impl FnMut(Kind) -> Option<i64>,
) -> Result<Resolved, String> {
    let mut renamed: BTreeMap<(Kind, i64), i64> = BTreeMap::new();
    let (mut left_rows, mut left_references, mut inserted, mut overwritten) =
        (BTreeSet::new(), BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for (problem, choice) in problems.iter().zip(chosen) {
        match (problem, choice) {
            (Problem::Taken(at), Choice::Renumber) => {
                let entry = &package.rows[*at];
                let (kind, old) =
                    entity_of(&entry.table, &entry.key).ok_or("only a spawn, a quest or a path is renumbered")?;
                if let std::collections::btree_map::Entry::Vacant(vacant) = renamed.entry((kind, old)) {
                    vacant.insert(fresh(kind).ok_or_else(|| format!("no free id for the {} {old}", kind.name()))?);
                }
            }
            (Problem::Taken(at) | Problem::Changed(at), Choice::Keep | Choice::Ignore) => {
                left_rows.insert(*at);
            }
            (Problem::Taken(_) | Problem::Changed(_), _) => {}
            (Problem::Missing(at), Choice::Insert) => {
                inserted.insert(*at);
            }
            (Problem::Missing(at), Choice::Link(id)) => {
                let reference = &package.references[*at];
                let (kind, old) =
                    entity_of(&reference.table, &reference.key).ok_or("only an entity is linked to another")?;
                let new: i64 = id
                    .trim()
                    .parse()
                    .map_err(|_| format!("'{id}' is no id of a {}", kind.name()))?;
                renamed.insert((kind, old), new);
                left_references.insert(*at);
            }
            (Problem::Missing(at), _) => {
                left_references.insert(*at);
            }
            (Problem::Differs(at), Choice::Overwrite) => {
                overwritten.insert(*at);
            }
            (Problem::Differs(at), _) => {
                left_references.insert(*at);
            }
        }
    }
    let mut changes = Vec::new();
    // The references to put first, what the rows need.
    for (at, reference) in package.references.iter().enumerate() {
        if !(inserted.contains(&at) || overwritten.contains(&at)) || left_references.contains(&at) {
            continue;
        }
        if !uniwow_api::db::write::TABLES.contains(&reference.table.as_str()) {
            continue;
        }
        let key = renamed_row(&reference.table, &reference.key, &renamed);
        changes.push(RowChange::Delete {
            table: reference.table.clone(),
            key: key.clone(),
        });
        changes.push(RowChange::Insert {
            table: reference.table.clone(),
            key,
            rows: reference
                .rows
                .iter()
                .map(|row| renamed_row(&reference.table, row, &renamed))
                .collect(),
        });
    }
    for (at, entry) in package.rows.iter().enumerate() {
        if left_rows.contains(&at) {
            continue;
        }
        let key = renamed_row(&entry.table, &entry.key, &renamed);
        changes.push(RowChange::Delete {
            table: entry.table.clone(),
            key: key.clone(),
        });
        if !entry.after.is_empty() {
            changes.push(RowChange::Insert {
                table: entry.table.clone(),
                key,
                rows: entry
                    .after
                    .iter()
                    .map(|row| renamed_row(&entry.table, row, &renamed))
                    .collect(),
            });
        }
    }
    let renamed_id = |kind: Kind, id: u64| renamed.get(&(kind, id as i64)).map_or(id, |new| *new as u64);
    let reloads = package
        .reloads
        .iter()
        .map(|reload| match reload {
            Reload::Creature(guid) => Reload::Creature(renamed_id(Kind::Creature, *guid)),
            Reload::Home(guid) => Reload::Home(renamed_id(Kind::Creature, *guid)),
            Reload::GameObject(guid) => Reload::GameObject(renamed_id(Kind::GameObject, *guid)),
            Reload::Formation(leader) => Reload::Formation(renamed_id(Kind::Creature, *leader)),
            Reload::Path(id) => Reload::Path(renamed_id(Kind::Path, u64::from(*id)) as u32),
            Reload::Template(entry) => Reload::Template(renamed_id(Kind::CreatureTemplate, u64::from(*entry)) as u32),
            other => other.clone(),
        })
        .collect();
    Ok((changes, reloads, renamed))
}

/// The statement reading the rows of `key` of `table` on the target, and its parameters.
pub fn select(table: &str, key: &Row) -> Option<(String, Vec<Value>)> {
    uniwow_api::db::write::identifier(table).ok()?;
    for column in key.keys() {
        uniwow_api::db::write::identifier(column).ok()?;
    }
    Some((
        format!(
            "SELECT * FROM `{table}` WHERE {}",
            uniwow_api::db::write::condition(key)
        ),
        key.values().cloned().collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::db::write::{Memory, apply};

    /// A patrol made for the creature 79: its path 790, its addon.
    fn patrol() -> Package {
        Package {
            label: "A patrol".to_owned(),
            made: "2026-10-21 12:00:00".to_owned(),
            server: "Test".to_owned(),
            database: "acore_world_test".to_owned(),
            rows: vec![
                Entry {
                    table: "waypoint_data".to_owned(),
                    key: row([("id", json!(790))]),
                    before: Vec::new(),
                    after: vec![
                        row([("id", json!(790)), ("point", json!(1)), ("position_x", json!(1.0))]),
                        row([("id", json!(790)), ("point", json!(2)), ("position_x", json!(2.0))]),
                    ],
                },
                Entry {
                    table: "creature_addon".to_owned(),
                    key: row([("guid", json!(79))]),
                    before: vec![row([("guid", json!(79)), ("path_id", json!(0))])],
                    after: vec![row([("guid", json!(79)), ("path_id", json!(790))])],
                },
            ],
            references: Vec::new(),
            reloads: vec![Reload::Path(790), Reload::Creature(79)],
        }
    }

    #[test]
    fn a_change_keeps_what_it_names_and_what_belongs_to_what_it_makes_and_reads_back_from_its_file() {
        let wanted = wanted(&patrol().rows);
        let tables: Vec<&str> = wanted.iter().map(|(table, _, _)| table.as_str()).collect();
        assert!(tables.contains(&"creature"), "the spawn whose path it is: {tables:?}");
        assert!(tables.contains(&"game_event_creature"));
        assert!(!tables.contains(&"creature_addon"), "already a row of the change");
        let mut package = patrol();
        package.references.push(Reference {
            table: "creature".to_owned(),
            key: row([("guid", json!(79))]),
            rows: vec![row([("guid", json!(79)), ("id", json!(68))])],
            why: "creature 79".to_owned(),
        });
        let read = Package::of_json(&package.to_json()).unwrap();
        assert_eq!(read, package);
        assert!(Package::of_json(&json!({ "format": "other" })).is_err());
    }

    #[test]
    fn an_import_whose_path_is_taken_there_renumbers_it_and_follows_its_references() {
        let mut package = patrol();
        package.references.push(Reference {
            table: "creature".to_owned(),
            key: row([("guid", json!(79))]),
            rows: vec![row([("guid", json!(79)), ("id", json!(68))])],
            why: "creature 79".to_owned(),
        });
        // There: the path 790 is another's; the addon of 79 as it was; the creature 79 missing.
        let rows = vec![
            vec![row([
                ("id", json!(790)),
                ("point", json!(1)),
                ("position_x", json!(50.0)),
            ])],
            vec![row([("guid", json!(79)), ("path_id", json!(0))])],
        ];
        let references = vec![Vec::new()];
        let problems = problems(&package, &rows, &references);
        assert_eq!(problems, [Problem::Taken(0), Problem::Missing(0)]);
        assert_eq!(choices(&problems[0], &package)[0], Choice::Renumber);
        assert_eq!(choices(&problems[1], &package)[0], Choice::Insert);
        let (changes, reloads, renamed) =
            resolved(&package, &problems, &[Choice::Renumber, Choice::Insert], |_| Some(900)).unwrap();
        assert_eq!(renamed[&(Kind::Path, 790)], 900);
        assert_eq!(reloads, [Reload::Path(900), Reload::Creature(79)]);
        let mut target = Memory::default();
        target.tables.insert("waypoint_data".to_owned(), rows[0].clone());
        target.tables.insert("creature_addon".to_owned(), rows[1].clone());
        apply(&mut target, &changes).unwrap();
        assert_eq!(target.tables["waypoint_data"].len(), 3, "the other's path kept");
        assert!(target.tables["creature_addon"].contains(&row([("guid", json!(79)), ("path_id", json!(900))])));
        assert_eq!(target.tables["creature"].len(), 1, "the creature inserted");

        // Linked to another creature there instead: the rows name it.
        let (changes, _, renamed) = resolved(
            &package,
            &problems,
            &[Choice::Keep, Choice::Link("81".to_owned())],
            |_| None,
        )
        .unwrap();
        assert_eq!(renamed[&(Kind::Creature, 79)], 81);
        assert!(changes.iter().any(|change| match change {
            RowChange::Insert { table, rows, .. } => table == "creature_addon" && rows[0]["guid"] == json!(81),
            _ => false,
        }));
        assert!(
            !changes.iter().any(|change| change.table() == "waypoint_data"),
            "the path left as it is there"
        );
        assert!(resolved(&package, &problems, &[Choice::Renumber, Choice::Insert], |_| None).is_err());
    }
}
