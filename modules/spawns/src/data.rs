//! The spawns of the world database as the module keeps them: read from the rows of `creature` and
//! `gameobject` of a map joined to their templates; changed, created and deleted in memory, laid
//! over the rows; given to the scene by tile.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use uniwow_api::db::Rows;
use uniwow_api::db::write::{Row, RowChange, row};
use uniwow_api::formats::TileId;
use uniwow_api::scene::{self, Entity, EntityId, Kind, Piece, PieceBuilder, flags};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db;

/// What the world database's version of AzerothCore names: the column of a creature's template
/// (`id` at first, `id1` while a spawn could take three templates, `id` again since the update
/// `2026_06_16_00`), and whether the looks of the templates are in `creature_template_model` (the
/// template's `modelid1` and `scale` before).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Schema {
    pub entry: &'static str,
    pub models_table: bool,
}

/// Whether `creature` has the column `id1`: rows when it has.
pub const ENTRY_COLUMN: &str = "SHOW COLUMNS FROM creature LIKE 'id1'";
/// Whether the table `creature_template_model` is: rows when it is.
pub const MODELS_TABLE: &str = "SHOW TABLES LIKE 'creature_template_model'";

impl Schema {
    /// The schema the answers to `ENTRY_COLUMN` and `MODELS_TABLE` tell.
    pub fn of(entry_column: &Rows, models_table: &Rows) -> Self {
        Self {
            entry: if entry_column.rows.is_empty() { "id" } else { "id1" },
            models_table: !models_table.rows.is_empty(),
        }
    }
}

/// The creatures of a map, with their template's name and first look, their template's entry as
/// `id1` whatever the schema calls it.
pub fn creatures(schema: Schema) -> String {
    let entry = schema.entry;
    let looks = if schema.models_table {
        format!(
            "m.CreatureDisplayID, m.DisplayScale FROM creature c JOIN creature_template t ON t.entry = c.{entry} \
             LEFT JOIN creature_template_model m ON m.CreatureID = c.{entry} AND m.Idx = 0"
        )
    } else {
        format!(
            "t.modelid1 AS CreatureDisplayID, t.scale AS DisplayScale FROM creature c \
             JOIN creature_template t ON t.entry = c.{entry}"
        )
    };
    format!(
        "SELECT c.guid, c.{entry} AS id1, c.position_x, c.position_y, c.position_z, c.orientation, t.name, \
         {looks} WHERE c.map = ?"
    )
}

/// The game objects of a map, with their template's name, look and size.
pub const OBJECTS: &str = "SELECT g.guid, g.id, g.position_x, g.position_y, g.position_z, g.orientation, \
     g.rotation0, g.rotation1, g.rotation2, g.rotation3, t.name, t.displayId, t.size FROM gameobject g \
     JOIN gameobject_template t ON t.entry = g.id WHERE g.map = ?";
/// The highest guid of each table, so that a spawn created takes one above.
pub const HIGHEST: [&str; 2] = ["SELECT MAX(guid) FROM creature", "SELECT MAX(guid) FROM gameobject"];
/// The highest entry of the templates of game objects, so that a template created takes one above.
pub const HIGHEST_TEMPLATE: &str = "SELECT MAX(entry) FROM gameobject_template";

/// The quests the templates start and end: the tables `creature_queststarter`,
/// `creature_questender`, `gameobject_queststarter`, `gameobject_questender`, in that order.
pub const LINKS: [(Kind, bool, &str); 4] = [
    (Kind::Creature, false, "SELECT id, quest FROM creature_queststarter"),
    (Kind::Creature, true, "SELECT id, quest FROM creature_questender"),
    (Kind::GameObject, false, "SELECT id, quest FROM gameobject_queststarter"),
    (Kind::GameObject, true, "SELECT id, quest FROM gameobject_questender"),
];
/// The titles of the quests.
pub const TITLES: &str = "SELECT ID, LogTitle FROM quest_template";

/// A template that starts or ends a quest: its kind, its entry, the quest, whether it ends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Link {
    pub kind: Kind,
    pub entry: u32,
    pub quest: u32,
    pub ends: bool,
}

impl Link {
    fn role(&self) -> &'static str {
        if self.ends { "ends" } else { "starts" }
    }
}

/// The quests of the database: their titles, and which template starts or ends each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quests {
    pub titles: BTreeMap<u32, String>,
    pub links: BTreeSet<Link>,
}

impl Quests {
    /// The quests as `TITLES` and the rows of `LINKS` give them, in the order of `LINKS`.
    pub fn of_rows(titles: &Rows, links: &[Rows]) -> Self {
        let number = |value: Option<&Value>| match value? {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        };
        let titles = titles
            .rows
            .iter()
            .filter_map(|row| {
                let title = row.get(1).and_then(Value::as_str).unwrap_or_default();
                Some((number(row.first())? as u32, title.to_owned()))
            })
            .collect();
        let links = LINKS
            .iter()
            .zip(links)
            .flat_map(|((kind, ends, _), rows)| {
                rows.rows.iter().filter_map(move |row| {
                    Some(Link {
                        kind: *kind,
                        entry: number(row.first())? as u32,
                        quest: number(row.get(1))? as u32,
                        ends: *ends,
                    })
                })
            })
            .collect();
        Self { titles, links }
    }

    /// The quests whose title holds `search` without case, or whose id it is; `limit` at most, by
    /// id.
    pub fn search(&self, search: &str, limit: usize) -> Vec<(u32, &str)> {
        let search = search.trim().to_lowercase();
        self.titles
            .iter()
            .filter(|(id, title)| {
                search.is_empty() || title.to_lowercase().contains(&search) || id.to_string() == search
            })
            .take(limit)
            .map(|(id, title)| (*id, title.as_str()))
            .collect()
    }
}

/// A spawn: a creature or a game object, its guid, its template and its name, its look, its place.
#[derive(Clone, Debug, PartialEq)]
pub struct Spawn {
    pub kind: Kind,
    pub guid: u64,
    pub entry: u32,
    pub name: String,
    pub display: u32,
    pub position: [f32; 3],
    pub orientation: f32,
    pub scale: f32,
    /// A game object's rotation, a quaternion x, y, z, w in the world; none for a creature.
    pub rotation: Option<[f32; 4]>,
    /// The game event it belongs to (`game_event_creature`, `game_event_gameobject`): spawned
    /// while it runs when above 0, taken away while it runs when below; 0 for none.
    pub event: i32,
}

/// The game events of the spawns: the creatures', then the game objects'.
pub const EVENTS: [&str; 2] = [
    "SELECT guid, eventEntry FROM game_event_creature",
    "SELECT guid, eventEntry FROM game_event_gameobject",
];

/// The events of `rows` (guid, event) given to the spawns of `kind` of `spawns`.
pub fn set_events(spawns: &mut [Spawn], kind: Kind, rows: &Rows) {
    let number = |value: Option<&Value>| match value? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    };
    let events: HashMap<u64, i32> = rows
        .rows
        .iter()
        .filter_map(|row| Some((number(row.first())? as u64, number(row.get(1))? as i32)))
        .collect();
    for spawn in spawns.iter_mut().filter(|spawn| spawn.kind == kind) {
        if let Some(event) = events.get(&spawn.guid) {
            spawn.event = *event;
        }
    }
}

/// The rotation about the vertical by `yaw` radians, as a quaternion x, y, z, w.
pub fn yaw_quaternion(yaw: f32) -> [f32; 4] {
    let (sin, cos) = (yaw / 2.0).sin_cos();
    [0.0, 0.0, sin, cos]
}

impl Spawn {
    pub fn id(&self) -> EntityId {
        match self.kind {
            Kind::GameObject => EntityId::game_object(self.guid),
            _ => EntityId::creature(self.guid),
        }
    }
}

/// The spawns of `rows`, of `kind`, as `CREATURES` and `OBJECTS` give them.
pub fn spawns(rows: &Rows, kind: Kind) -> Vec<Spawn> {
    let column = |name: &str| rows.columns.iter().position(|column| column.eq_ignore_ascii_case(name));
    let at =
        |row: &[Value], index: Option<usize>| index.and_then(|index| row.get(index)).cloned().unwrap_or(Value::Null);
    let number = |value: Value| match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    };
    let (guid, entry, x, y, z, orientation, name) = (
        column("guid"),
        column(if kind == Kind::GameObject { "id" } else { "id1" }),
        column("position_x"),
        column("position_y"),
        column("position_z"),
        column("orientation"),
        column("name"),
    );
    let (display, scale) = match kind {
        Kind::GameObject => (column("displayId"), column("size")),
        _ => (column("CreatureDisplayID"), column("DisplayScale")),
    };
    rows.rows
        .iter()
        .filter_map(|row| {
            Some(Spawn {
                kind,
                guid: number(at(row, guid))? as u64,
                entry: number(at(row, entry)).unwrap_or(0.0) as u32,
                name: at(row, name).as_str().unwrap_or_default().to_owned(),
                display: number(at(row, display)).unwrap_or(0.0) as u32,
                position: [
                    number(at(row, x))? as f32,
                    number(at(row, y))? as f32,
                    number(at(row, z))? as f32,
                ],
                orientation: number(at(row, orientation)).unwrap_or(0.0) as f32,
                scale: number(at(row, scale)).filter(|scale| *scale > 0.0).unwrap_or(1.0) as f32,
                rotation: (kind == Kind::GameObject).then(|| {
                    let part = |name: &str| number(at(row, column(name))).unwrap_or(0.0) as f32;
                    let rotation = [
                        part("rotation0"),
                        part("rotation1"),
                        part("rotation2"),
                        part("rotation3"),
                    ];
                    // A row without a rotation of its own turns by its orientation alone.
                    if rotation == [0.0; 4] {
                        yaw_quaternion(number(at(row, orientation)).unwrap_or(0.0) as f32)
                    } else {
                        rotation
                    }
                }),
                event: 0,
            })
        })
        .collect()
}

/// The highest guid of a table, as `HIGHEST` gives it.
pub fn highest(rows: &Rows) -> u64 {
    rows.rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .unwrap_or(0)
}

/// The changes the user made to the spawns, kept in memory: moved (a place and an orientation),
/// created, deleted, and the guids given by kind.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub moved: HashMap<EntityId, ([f32; 3], f32)>,
    pub added: HashMap<EntityId, Spawn>,
    pub deleted: HashSet<EntityId>,
    pub given: HashMap<String, u64>,
    /// The quests given to templates and taken from them.
    pub linked: BTreeSet<Link>,
    pub unlinked: BTreeSet<Link>,
    /// The templates of game objects created (step 15.14), by entry.
    pub templates: BTreeMap<u32, Template>,
    /// The rows of `GameObjectDisplayInfo` prepared for models no row names: their ids and models.
    pub displays: BTreeMap<u32, String>,
    /// The spawn of the database each spawn duplicated was made of, its row copied once applied.
    pub sources: BTreeMap<EntityId, EntityId>,
    /// The patrols as the user changed them (step 16.4).
    pub patrols: crate::patrols::Edits,
}

/// A template of game objects created in memory: its entry, name, display and size. Its type is 2
/// (a giver of quests) once it gives one, 5 (generic) otherwise.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub entry: u32,
    pub name: String,
    pub display: u32,
    pub size: f32,
}

impl Template {
    /// Its type of `gameobject_template`, as its quests make it.
    pub fn kind(&self, quests: &[Link]) -> u32 {
        if quests.is_empty() { 5 } else { 2 }
    }
}

/// What a doodad made a game object gives (decision 21): its spawn and its template; the row of
/// `GameObjectDisplayInfo` prepared for its model when none names it.
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    pub spawn: Spawn,
    pub template: Template,
    pub display: Option<(u32, String)>,
}

/// The game object made of the doodad of the model `model` placed at `position` (in the world)
/// turned by `rotation` (degrees about the axes of its file) and scaled by `scale`: its spawn at the
/// same place and rotation (its quaternion, its yaw as its orientation), a template of its size and
/// its model's display, of an existing row of `GameObjectDisplayInfo` (`displays`) or a row prepared
/// above the highest of them. Its guid and entry given above `highest` and those given before.
pub fn made_of_doodad(
    changes: &mut Changes,
    model: &str,
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
    highest: [u64; 3],
    displays: &[(u32, String)],
) -> Made {
    let highest_display = displays.iter().map(|(id, _)| *id).max().unwrap_or(0);
    let placed = uniwow_api::formats::placement(scene::file_of_world(position), rotation, 1.0);
    let (_, turned, _) = placed.to_scale_rotation_translation();
    let forward = turned * uniwow_api::glam::Vec3::X;
    let yaw = forward.y.atan2(forward.x).rem_euclid(std::f32::consts::TAU);
    // The model as the table names it, `.mdx` for an M2.
    let lowered = model.to_ascii_lowercase();
    let named = match lowered.strip_suffix(".m2") {
        Some(stem) => format!("{stem}.mdx"),
        None => lowered.clone(),
    };
    let same = |path: &str| {
        let path = path.to_ascii_lowercase().replace('/', "\\");
        path == named || path == lowered
    };
    let display = displays
        .iter()
        .map(|(id, path)| (*id, path.as_str()))
        .chain(changes.displays.iter().map(|(id, path)| (*id, path.as_str())))
        .find(|(_, path)| same(path))
        .map(|(id, _)| id);
    let prepared = match display {
        Some(_) => None,
        None => {
            let id = changes.give_named("display", u64::from(highest_display)) as u32;
            changes.displays.insert(id, model.to_owned());
            Some((id, model.to_owned()))
        }
    };
    let display = display.or(prepared.as_ref().map(|(id, _)| *id)).unwrap_or(0);
    let stem = model
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(model)
        .rsplit_once('.')
        .map_or(model, |(stem, _)| stem)
        .to_owned();
    let entry = changes.give_named("gameobject_template", highest[2]) as u32;
    let template = Template {
        entry,
        name: stem.clone(),
        display,
        size: scale,
    };
    changes.templates.insert(entry, template.clone());
    let spawn = Spawn {
        kind: Kind::GameObject,
        guid: changes.give(Kind::GameObject, highest[1]),
        entry,
        name: stem,
        display,
        position,
        orientation: yaw,
        scale,
        rotation: Some(turned.to_array()),
        event: 0,
    };
    changes.added.insert(spawn.id(), spawn.clone());
    Made {
        spawn,
        template,
        display: prepared,
    }
}

impl Changes {
    /// `spawns` as the user changed them: those deleted left out, those created added, those moved
    /// at their places.
    #[cfg(test)]
    pub fn laid_over(&self, spawns: &[Spawn]) -> Vec<Spawn> {
        let mut out: Vec<Spawn> = spawns
            .iter()
            .filter(|spawn| !self.deleted.contains(&spawn.id()))
            .cloned()
            .collect();
        let mut added: Vec<&Spawn> = self.added.values().collect();
        added.sort_by_key(|spawn| spawn.id());
        out.extend(
            added
                .into_iter()
                .filter(|spawn| !self.deleted.contains(&spawn.id()))
                .cloned(),
        );
        for spawn in &mut out {
            self.moved_as(spawn);
        }
        out
    }

    /// `spawn` at the place the user moved it to, if he did.
    pub fn moved_as(&self, spawn: &mut Spawn) {
        if let Some((position, orientation)) = self.moved.get(&spawn.id()) {
            spawn.position = *position;
            if spawn.orientation != *orientation && spawn.rotation.is_some() {
                // Turned about the vertical by the user: its rotation that turn alone.
                spawn.rotation = Some(yaw_quaternion(*orientation));
            }
            spawn.orientation = *orientation;
        }
    }

    /// The spawn `id` of `spawns` (whose places by id `index` gives) as the user changed it: none
    /// once deleted.
    pub fn find(&self, spawns: &[Spawn], index: &HashMap<EntityId, usize>, id: &EntityId) -> Option<Spawn> {
        if self.deleted.contains(id) {
            return None;
        }
        let mut spawn = match self.added.get(id) {
            Some(added) => added.clone(),
            None => spawns.get(*index.get(id)?)?.clone(),
        };
        self.moved_as(&mut spawn);
        Some(spawn)
    }

    /// The quests of the template of `kind` and `entry` as the user changed them, by quest.
    pub fn quests_of(&self, quests: &Quests, kind: Kind, entry: u32) -> Vec<Link> {
        let of = |link: &&Link| link.kind == kind && link.entry == entry;
        let mut out: BTreeSet<Link> = quests
            .links
            .iter()
            .filter(of)
            .filter(|link| !self.unlinked.contains(link))
            .copied()
            .collect();
        out.extend(self.linked.iter().filter(of));
        let mut out: Vec<Link> = out.into_iter().collect();
        out.sort_by_key(|link| (link.quest, link.ends));
        out
    }

    /// `link` given (`given`) or taken, the quests of the database being `quests`; whether it
    /// changed anything.
    pub fn link(&mut self, quests: &Quests, link: Link, given: bool) -> bool {
        let held = quests.links.contains(&link);
        match (given, held) {
            (true, true) => self.unlinked.remove(&link),
            (true, false) => self.linked.insert(link),
            (false, true) => self.unlinked.insert(link),
            (false, false) => self.linked.remove(&link),
        }
    }

    /// A guid of `kind` above `highest` and every one given before.
    pub fn give(&mut self, kind: Kind, highest: u64) -> u64 {
        self.give_named(kind.name(), highest)
    }

    /// A number of the series `series` above `highest` and every one given before.
    pub fn give_named(&mut self, series: &str, highest: u64) -> u64 {
        let given = self.given.entry(series.to_owned()).or_insert(0);
        *given = (*given).max(highest) + 1;
        *given
    }

    /// The changes as the project keeps them, in order.
    pub fn section(&self) -> Value {
        let mut moved: Vec<_> = self.moved.iter().collect();
        moved.sort_by_key(|(id, _)| **id);
        let mut added: Vec<_> = self.added.values().collect();
        added.sort_by_key(|spawn| spawn.id());
        let mut deleted: Vec<_> = self.deleted.iter().collect();
        deleted.sort();
        let given: BTreeMap<&String, &u64> = self.given.iter().collect();
        let links = |links: &BTreeSet<Link>| -> Vec<Value> {
            links
                .iter()
                .map(|link| {
                    json!({ "kind": link.kind.name(), "entry": link.entry, "quest": link.quest, "role": link.role() })
                })
                .collect()
        };
        json!({
            "moved": moved.into_iter().map(|(id, (position, orientation))| json!({
                "id": id.to_string(), "position": position, "orientation": orientation,
            })).collect::<Vec<_>>(),
            "created": added.into_iter().map(|spawn| json!({
                "id": spawn.id().to_string(), "entry": spawn.entry, "name": spawn.name, "display": spawn.display,
                "position": spawn.position, "orientation": spawn.orientation, "scale": spawn.scale,
                "rotation": spawn.rotation,
            })).collect::<Vec<_>>(),
            "templates": self.templates.values().map(|template| json!({
                "entry": template.entry, "name": template.name, "display": template.display, "size": template.size,
            })).collect::<Vec<_>>(),
            "displays prepared": self.displays.iter().map(|(id, model)| json!({ "id": id, "model": model }))
                .collect::<Vec<_>>(),
            "deleted": deleted.into_iter().map(ToString::to_string).collect::<Vec<_>>(),
            "guids given": given,
            "quests given": links(&self.linked),
            "quests taken": links(&self.unlinked),
            "copied from": self.sources.iter().map(|(id, source)| json!({
                "id": id.to_string(), "source": source.to_string(),
            })).collect::<Vec<_>>(),
            "patrols": self.patrols.section(),
        })
    }

    pub fn of_section(section: &Value) -> Self {
        let vector = |value: &Value| -> Option<[f32; 3]> {
            let values = value.as_array()?;
            Some([0, 1, 2].map(|at| values.get(at).and_then(Value::as_f64).unwrap_or(0.0) as f32))
        };
        let list = |key: &str| section[key].as_array().cloned().unwrap_or_default();
        let id = |entry: &Value| entry["id"].as_str()?.parse::<EntityId>().ok();
        let links = |key: &str| -> BTreeSet<Link> {
            list(key)
                .iter()
                .filter_map(|entry| {
                    Some(Link {
                        kind: if entry["kind"].as_str()? == Kind::GameObject.name() {
                            Kind::GameObject
                        } else {
                            Kind::Creature
                        },
                        entry: entry["entry"].as_u64()? as u32,
                        quest: entry["quest"].as_u64()? as u32,
                        ends: entry["role"].as_str()? == "ends",
                    })
                })
                .collect()
        };
        Self {
            moved: list("moved")
                .iter()
                .filter_map(|entry| {
                    Some((
                        id(entry)?,
                        (vector(&entry["position"])?, entry["orientation"].as_f64()? as f32),
                    ))
                })
                .collect(),
            added: list("created")
                .iter()
                .filter_map(|entry| {
                    let id = id(entry)?;
                    Some((
                        id,
                        Spawn {
                            kind: id.kind,
                            guid: id.key,
                            entry: entry["entry"].as_u64()? as u32,
                            name: entry["name"].as_str().unwrap_or_default().to_owned(),
                            display: entry["display"].as_u64().unwrap_or(0) as u32,
                            position: vector(&entry["position"])?,
                            orientation: entry["orientation"].as_f64().unwrap_or(0.0) as f32,
                            scale: entry["scale"].as_f64().unwrap_or(1.0) as f32,
                            rotation: entry["rotation"]
                                .as_array()
                                .filter(|parts| parts.len() == 4)
                                .map(|parts| [0, 1, 2, 3].map(|at| parts[at].as_f64().unwrap_or(0.0) as f32)),
                            event: 0,
                        },
                    ))
                })
                .collect(),
            deleted: list("deleted")
                .iter()
                .filter_map(|id| id.as_str()?.parse().ok())
                .collect(),
            given: section["guids given"]
                .as_object()
                .map(|given| {
                    given
                        .iter()
                        .filter_map(|(kind, id)| Some((kind.clone(), id.as_u64()?)))
                        .collect()
                })
                .unwrap_or_default(),
            linked: links("quests given"),
            unlinked: links("quests taken"),
            templates: list("templates")
                .iter()
                .filter_map(|template| {
                    let entry = template["entry"].as_u64()? as u32;
                    Some((
                        entry,
                        Template {
                            entry,
                            name: template["name"].as_str().unwrap_or_default().to_owned(),
                            display: template["display"].as_u64().unwrap_or(0) as u32,
                            size: template["size"].as_f64().unwrap_or(1.0) as f32,
                        },
                    ))
                })
                .collect(),
            displays: list("displays prepared")
                .iter()
                .filter_map(|display| Some((display["id"].as_u64()? as u32, display["model"].as_str()?.to_owned())))
                .collect(),
            sources: list("copied from")
                .iter()
                .filter_map(|entry| Some((id(entry)?, entry["source"].as_str()?.parse().ok()?)))
                .collect(),
            patrols: crate::patrols::Edits::of_section(&section["patrols"]),
        }
    }

    /// The changes for the server (step 16.1): the rows of the spawns moved, duplicated and deleted
    /// and of the quests given and taken, the creatures' entry in their column `entry_column`; the
    /// spawns as the database has them `spawns`, their places by id `index`. What is not written
    /// yet said: the game objects made of doodads, their templates and displays to come.
    pub fn pending(
        &self,
        spawns: &[Spawn],
        index: &HashMap<EntityId, usize>,
        entry_column: &str,
        patrols: &crate::patrols::Patrols,
    ) -> world_db::Pending {
        let mut out = world_db::Pending {
            owner: "spawns".to_owned(),
            label: "Spawns".to_owned(),
            ..world_db::Pending::default()
        };
        let table = |kind: Kind| {
            if kind == Kind::GameObject {
                "gameobject"
            } else {
                "creature"
            }
        };
        let key = |spawn: &EntityId| row([("guid", json!(spawn.key))]);
        // The columns of a spawn's place, its rotation a game object's.
        let place = |spawn: &Spawn| {
            let mut set = row([
                ("position_x", json!(spawn.position[0])),
                ("position_y", json!(spawn.position[1])),
                ("position_z", json!(spawn.position[2])),
                ("orientation", json!(spawn.orientation)),
            ]);
            if let Some(rotation) = spawn.rotation {
                for (at, part) in rotation.iter().enumerate() {
                    set.insert(format!("rotation{at}"), json!(part));
                }
            }
            set
        };
        let reload = |id: &EntityId| match id.kind {
            Kind::GameObject => world_db::Reload::GameObject(id.key),
            _ => world_db::Reload::Creature(id.key),
        };
        let mut moved: Vec<&EntityId> = self.moved.keys().collect();
        moved.sort();
        for id in moved {
            if self.added.contains_key(id) || self.deleted.contains(id) {
                continue;
            }
            let Some(spawn) = self.find(spawns, index, id) else {
                continue;
            };
            out.changes.push(RowChange::Put {
                table: table(id.kind).to_owned(),
                key: key(id),
                set: place(&spawn),
                from: None,
            });
            // A creature moved keeps its place in the game, its spawn point there; a game object is
            // spawned again.
            out.reloads.push(match id.kind {
                Kind::Creature => world_db::Reload::Home(id.key),
                _ => reload(id),
            });
        }
        let mut added: Vec<&EntityId> = self.added.keys().collect();
        added.sort();
        let mut made = 0;
        for id in added {
            if self.deleted.contains(id) {
                continue;
            }
            let Some(spawn) = self.find(spawns, index, id) else {
                continue;
            };
            // A copy of a spawn of the database: its row copied, its guid, entry and place its own.
            let Some(source) = self.sources.get(id).filter(|source| index.contains_key(source)) else {
                made += 1;
                continue;
            };
            let mut set = place(&spawn);
            let entry = if id.kind == Kind::GameObject {
                "id"
            } else {
                entry_column
            };
            set.insert(entry.to_owned(), json!(spawn.entry));
            out.changes.push(RowChange::Put {
                table: table(id.kind).to_owned(),
                key: key(id),
                set,
                from: Some(key(source)),
            });
            out.reloads.push(reload(id));
        }
        if made > 0 {
            out.held.push(format!(
                "{made} game objects made of doodads: their templates and displays are written in a later step"
            ));
        }
        let mut deleted: Vec<&EntityId> = self.deleted.iter().collect();
        deleted.sort();
        for id in deleted {
            // Never in the database: nothing to take away.
            if self.added.contains_key(id) || !index.contains_key(id) {
                continue;
            }
            let tables: &[&str] = match id.kind {
                Kind::GameObject => &["gameobject", "game_event_gameobject"],
                _ => &["creature", "creature_addon", "game_event_creature"],
            };
            for table in tables {
                out.changes.push(RowChange::Delete {
                    table: (*table).to_owned(),
                    key: key(id),
                });
            }
            out.reloads.push(reload(id));
        }
        let link_table = |link: &Link| {
            format!(
                "{}_quest{}",
                if link.kind == Kind::GameObject {
                    "gameobject"
                } else {
                    "creature"
                },
                if link.ends { "ender" } else { "starter" }
            )
        };
        let link_key = |link: &Link| row([("id", json!(link.entry)), ("quest", json!(link.quest))]);
        for link in &self.linked {
            out.changes.push(RowChange::Put {
                table: link_table(link),
                key: link_key(link),
                set: Row::new(),
                from: None,
            });
        }
        for link in &self.unlinked {
            out.changes.push(RowChange::Delete {
                table: link_table(link),
                key: link_key(link),
            });
        }
        if !self.linked.is_empty() || !self.unlinked.is_empty() {
            out.reloads.push(world_db::Reload::Quests);
        }
        if !self.templates.is_empty() {
            out.held.push(format!(
                "{} templates of game objects: written in a later step",
                self.templates.len()
            ));
        }
        let (changes, reloads) = self.patrols.rows(patrols);
        out.changes.extend(changes);
        out.reloads.extend(reloads);
        out.reloads.sort();
        out.reloads.dedup();
        out
    }
}

/// The tile of a point of the world.
pub fn tile_of(position: [f32; 3]) -> Option<TileId> {
    scene::chunk_at(position[0], position[1]).map(|(tile, _)| tile)
}

/// The spawns `spawns` as entities of the scene, by tile: named by their
/// template, their extra number their template's entry; their bounds a box of their scale.
pub fn pieces(spawns: &[Spawn]) -> BTreeMap<TileId, Piece> {
    let mut builders: BTreeMap<TileId, PieceBuilder> = BTreeMap::new();
    for spawn in spawns {
        let Some(tile) = tile_of(spawn.position) else {
            continue;
        };
        let builder = builders.entry(tile).or_insert_with(|| PieceBuilder::new(Vec::new()));
        builder.push(Entity {
            id: spawn.id(),
            flags: flags::SERVER,
            parent: None,
            label: &spawn.name,
            extra: spawn.entry,
            origin: spawn.position,
            bounds: scene::box_around(
                [spawn.position[0], spawn.position[1], spawn.position[2] + spawn.scale],
                spawn.scale.clamp(0.5, 10.0),
            ),
        });
    }
    builders
        .into_iter()
        .map(|(tile, builder)| (tile, builder.build()))
        .collect()
}

/// The components of the spawn `spawn` for the Inspector: *Transform* (its position in the world,
/// its orientation in degrees about the vertical as the rotation about the axis Y of the files, no
/// scale), *Model* (its template), *Quests* (those its template starts and ends) and its columns.
pub fn read(spawn: &Spawn, changed: bool, quests: &[Link], titles: &BTreeMap<u32, String>) -> Value {
    let table = if spawn.kind == Kind::GameObject {
        "gameobject"
    } else {
        "creature"
    };
    json!({
        "id": spawn.id().to_string(),
        "changed": changed,
        "components": {
            "transform": {
                "position": spawn.position,
                "rotation": [0.0, spawn.orientation.to_degrees(), 0.0],
            },
            "model": {
                "template": format!("{table}_template {}", spawn.entry),
                "name": spawn.name,
                "display": spawn.display,
            },
            "quests": quests.iter().map(|link| json!({
                "quest": link.quest,
                "title": titles.get(&link.quest).cloned().unwrap_or_default(),
                "role": link.role(),
            })).collect::<Vec<_>>(),
            "raw": {
                "guid": spawn.guid,
                "table": table,
                "id": spawn.entry,
                "position_x": spawn.position[0],
                "position_y": spawn.position[1],
                "position_z": spawn.position[2],
                "orientation": spawn.orientation,
                "game event": spawn.event,
            },
        },
    })
}

/// `spawn`'s place with its `field` changed as `change` says: the position in the world on `axis`
/// or all, the rotation about the vertical (the axis 1 alone, in degrees); a scale refused, a spawn
/// taking the size of its template.
pub fn written(spawn: &Spawn, field: &str, axis: Option<usize>, change: &str) -> Result<([f32; 3], f32), String> {
    let (mut position, mut orientation) = (spawn.position, spawn.orientation);
    match field {
        "position" => {
            let axes: Vec<usize> = match axis {
                Some(axis) if axis < 3 => vec![axis],
                Some(axis) => return Err(format!("no axis {axis}")),
                None => vec![0, 1, 2],
            };
            for axis in axes {
                position[axis] = scene::changed_value(position[axis], change)?;
            }
        }
        "rotation" => {
            let degrees = match (scene::turn_of(change), axis) {
                // Turned by a gizmo about an axis of the world: the vertical alone.
                (Some(([x, y, z], degrees)), _) => {
                    if x.abs() > 1e-4 || y.abs() > 1e-4 || z == 0.0 {
                        return Err("a spawn turns about the vertical alone".to_owned());
                    }
                    orientation.to_degrees() + degrees * z.signum()
                }
                (None, Some(1) | None) => scene::changed_value(orientation.to_degrees(), change)?,
                (None, Some(_)) => return Err("a spawn turns about the vertical alone".to_owned()),
            };
            orientation = degrees.to_radians().rem_euclid(std::f32::consts::TAU);
        }
        "scale" => return Err("a spawn takes the size of its template".to_owned()),
        other => return Err(format!("a spawn has no field '{other}'")),
    }
    Ok((position, orientation))
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_spawn_is_turned_by_a_gizmo_about_the_vertical_alone() {
        let mut spawn = spawns(&guard(), Kind::Creature).remove(0);
        spawn.orientation = 0.5;
        let (_, turned) = written(&spawn, "rotation", None, &scene::turn_change([0.0, 0.0, 1.0], 90.0)).unwrap();
        assert!((turned - (0.5 + std::f32::consts::FRAC_PI_2)).abs() < 1e-5);
        let (_, back) = written(&spawn, "rotation", None, &scene::turn_change([0.0, 0.0, -1.0], 90.0)).unwrap();
        assert!((back - (0.5 - std::f32::consts::FRAC_PI_2).rem_euclid(std::f32::consts::TAU)).abs() < 1e-5);
        assert!(written(&spawn, "rotation", None, &scene::turn_change([1.0, 0.0, 0.0], 90.0)).is_err());
    }

    use super::*;

    fn rows(columns: &[&str], rows: Vec<Vec<Value>>) -> Rows {
        Rows {
            columns: columns.iter().map(|column| (*column).to_owned()).collect(),
            rows,
        }
    }

    #[test]
    fn the_changes_of_the_spawns_are_the_rows_the_server_is_given() {
        use uniwow_api::db::write::{Memory, apply};

        let spawns = spawns(&guard(), Kind::Creature);
        let index: HashMap<EntityId, usize> = spawns.iter().enumerate().map(|(at, spawn)| (spawn.id(), at)).collect();
        let (first, second) = (spawns[0].id(), spawns[1].id());
        let database_row = |spawn: &Spawn| {
            row([
                ("guid", json!(spawn.guid)),
                ("id", json!(spawn.entry)),
                ("map", json!(0)),
                ("position_x", json!(spawn.position[0])),
                ("position_y", json!(spawn.position[1])),
                ("position_z", json!(spawn.position[2])),
                ("orientation", json!(spawn.orientation)),
                ("spawntimesecs", json!(120)),
            ])
        };
        let mut memory = Memory::default();
        memory
            .tables
            .insert("creature".to_owned(), spawns.iter().map(database_row).collect());
        memory.tables.insert(
            "creature_addon".to_owned(),
            vec![row([("guid", json!(spawns[1].guid)), ("path_id", json!(0))])],
        );
        memory.tables.insert(
            "creature_queststarter".to_owned(),
            vec![row([("id", json!(68)), ("quest", json!(100))])],
        );

        let mut changes = Changes::default();
        changes.moved.insert(first, ([1.0, 2.0, 3.0], 1.5));
        let mut copy = spawns[0].clone();
        copy.guid = 900_000;
        copy.position = [5.0, 6.0, 7.0];
        changes.sources.insert(copy.id(), first);
        changes.added.insert(copy.id(), copy.clone());
        changes.deleted.insert(second);
        let link = |quest: u32| Link {
            kind: Kind::Creature,
            entry: 68,
            quest,
            ends: false,
        };
        changes.linked.insert(link(200));
        changes.unlinked.insert(link(100));
        let pending = changes.pending(&spawns, &index, "id", &Default::default());
        assert!(pending.held.is_empty(), "{:?}", pending.held);
        assert_eq!(
            pending.reloads,
            [
                world_db::Reload::Quests,
                world_db::Reload::Creature(spawns[1].guid),
                world_db::Reload::Creature(900_000),
                world_db::Reload::Home(spawns[0].guid),
            ],
            "in the order the server makes them"
        );
        apply(&mut memory, &pending.changes).unwrap();

        let creatures = &memory.tables["creature"];
        assert_eq!(creatures.len(), 2, "one deleted, one copied");
        let moved = creatures
            .iter()
            .find(|row| row["guid"] == json!(spawns[0].guid))
            .unwrap();
        assert_eq!(
            (moved["position_x"].as_f64(), moved["orientation"].as_f64()),
            (Some(1.0), Some(1.5))
        );
        let copied = creatures.iter().find(|row| row["guid"] == json!(900_000)).unwrap();
        assert_eq!(copied["spawntimesecs"], json!(120), "the row of its source copied");
        assert_eq!(copied["position_z"].as_f64(), Some(7.0));
        assert!(memory.tables["creature_addon"].is_empty());
        assert_eq!(
            memory.tables["creature_queststarter"],
            [row([("id", json!(68)), ("quest", json!(200))])]
        );

        // A game object made of a doodad: said, not written.
        let mut made = Changes::default();
        made_of_doodad(
            &mut made,
            r"World\Barrel.m2",
            [0.0; 3],
            [0.0; 3],
            1.0,
            [10, 10, 10],
            &[],
        );
        let pending = made.pending(&spawns, &index, "id", &Default::default());
        assert!(pending.changes.is_empty());
        assert_eq!(pending.held.len(), 2);
        // Kept in the project with the spawn each was copied from.
        assert_eq!(Changes::of_section(&changes.section()).sources, changes.sources);
    }

    /// A guard of Stormwind as `CREATURES` gives it.
    fn guard() -> Rows {
        rows(
            &[
                "guid",
                "id1",
                "position_x",
                "position_y",
                "position_z",
                "orientation",
                "name",
                "CreatureDisplayID",
                "DisplayScale",
            ],
            vec![
                vec![
                    json!(79753),
                    json!(68),
                    json!(-8832.79),
                    json!(631.42),
                    json!(94.15),
                    json!(0.48),
                    json!("Stormwind City Guard"),
                    json!(3167),
                    json!(1.0),
                ],
                vec![
                    json!(79754),
                    json!(68),
                    json!("-8840.0"),
                    json!(620.0),
                    json!(94.0),
                    json!(1.0),
                    json!("Stormwind City Guard"),
                    Value::Null,
                    Value::Null,
                ],
            ],
        )
    }

    #[test]
    fn the_rows_of_the_database_are_spawns_by_tile_with_their_template() {
        let spawns = spawns(&guard(), Kind::Creature);
        assert_eq!(spawns.len(), 2);
        assert_eq!(
            (spawns[0].entry, spawns[0].display, spawns[0].name.as_str()),
            (68, 3167, "Stormwind City Guard")
        );
        assert_eq!(spawns[1].position[0], -8840.0, "a number written as text read");
        assert_eq!(spawns[1].scale, 1.0, "no model: its scale by default");
        let pieces = pieces(&spawns);
        let (tile, piece) = pieces.iter().next().unwrap();
        assert_eq!(*tile, tile_of(spawns[0].position).unwrap());
        assert_eq!(piece.ids[0], EntityId::creature(79753));
        assert_eq!(piece.name(0), "Stormwind City Guard #79753");
        assert_eq!(piece.extra[0], 68);
        let objects = spawns_of_objects();
        assert_eq!(objects[0].id(), EntityId::game_object(5714));
        assert_eq!(objects[0].scale, 2.0);
        assert_eq!(highest(&rows(&["MAX(guid)"], vec![vec![json!(213_000)]])), 213_000);
    }

    fn spawns_of_objects() -> Vec<Spawn> {
        spawns(
            &rows(
                &[
                    "guid",
                    "id",
                    "position_x",
                    "position_y",
                    "position_z",
                    "orientation",
                    "name",
                    "displayId",
                    "size",
                ],
                vec![vec![
                    json!(5714),
                    json!(1731),
                    json!(1.0),
                    json!(2.0),
                    json!(3.0),
                    json!(0.0),
                    json!("Copper Vein"),
                    json!(310),
                    json!(2.0),
                ]],
            ),
            Kind::GameObject,
        )
    }

    #[test]
    fn a_spawn_moved_created_and_deleted_in_memory_is_laid_over_and_kept_in_the_project() {
        let spawns = spawns(&guard(), Kind::Creature);
        let mut changes = Changes::default();
        let (position, orientation) = written(&spawns[0], "position", Some(2), "+2").unwrap();
        assert_eq!(position[2], spawns[0].position[2] + 2.0);
        changes.moved.insert(spawns[0].id(), (position, orientation));
        let turned = written(&spawns[0], "rotation", Some(1), "180").unwrap().1;
        assert!((turned - std::f32::consts::PI).abs() < 1e-4);
        assert!(written(&spawns[0], "scale", None, "2").is_err());
        let guid = changes.give(Kind::Creature, 80_000);
        assert_eq!((guid, changes.give(Kind::Creature, 80_000)), (80_001, 80_002));
        let mut created = spawns[1].clone();
        created.guid = guid;
        changes.added.insert(created.id(), created);
        changes.deleted.insert(spawns[1].id());
        let laid = changes.laid_over(&spawns);
        let ids: Vec<EntityId> = laid.iter().map(Spawn::id).collect();
        assert_eq!(ids, [EntityId::creature(79753), EntityId::creature(80_001)]);
        assert_eq!(laid[0].position, position);
        assert_eq!(Changes::of_section(&changes.section()), changes);
        let read = read(&laid[0], true, &[], &BTreeMap::new());
        assert_eq!(read["components"]["model"]["template"], json!("creature_template 68"));
    }

    #[test]
    fn the_quests_of_a_template_are_read_given_and_taken_in_memory() {
        let titles = rows(
            &["ID", "LogTitle"],
            vec![
                vec![json!(783), json!("A Threat Within")],
                vec![json!(7), json!("Kobold Camp Cleanup")],
                vec![json!("33"), json!("Wolves Across the Border")],
            ],
        );
        let links = [
            rows(&["id", "quest"], vec![vec![json!(197), json!(7)]]),
            rows(
                &["id", "quest"],
                vec![vec![json!(197), json!(7)], vec![json!(823), json!(783)]],
            ),
            rows(&["id", "quest"], vec![]),
            rows(&["id", "quest"], vec![]),
        ];
        let quests = Quests::of_rows(&titles, &links);
        assert_eq!(quests.titles.len(), 3, "an id written as text read");
        assert_eq!(quests.search("wolves", 10), [(33, "Wolves Across the Border")]);
        assert_eq!(quests.search("783", 10), [(783, "A Threat Within")]);
        assert_eq!(quests.search("", 2).len(), 2);
        let mut changes = Changes::default();
        let starts = |quest| Link {
            kind: Kind::Creature,
            entry: 197,
            quest,
            ends: false,
        };
        let held = |changes: &Changes| -> Vec<(u32, bool)> {
            changes
                .quests_of(&quests, Kind::Creature, 197)
                .iter()
                .map(|link| (link.quest, link.ends))
                .collect()
        };
        assert_eq!(held(&changes), [(7, false), (7, true)]);
        assert!(changes.link(&quests, starts(33), true));
        assert!(changes.link(&quests, starts(7), false));
        assert!(!changes.link(&quests, starts(7), false), "taken twice changes nothing");
        assert_eq!(held(&changes), [(7, true), (33, false)]);
        assert_eq!(Changes::of_section(&changes.section()), changes);
        let spawn = &spawns(&guard(), Kind::Creature)[0];
        let read = read(
            spawn,
            false,
            &changes.quests_of(&quests, Kind::Creature, 197),
            &quests.titles,
        );
        assert_eq!(
            read["components"]["quests"][1],
            json!({ "quest": 33, "title": "Wolves Across the Border", "role": "starts" })
        );
        assert!(changes.link(&quests, starts(7), true), "given back");
        assert!(changes.unlinked.is_empty());
    }

    #[test]
    fn a_doodad_made_a_game_object_keeps_its_place_rotation_and_size_and_its_model_s_display() {
        use uniwow_api::glam::{Mat4, Quat, Vec3};
        let mut changes = Changes::default();
        let (position, rotation, scale) = ([-8913.25, -120.5, 82.75], [12.0, 137.5, -7.25], 1.75);
        let displays = vec![(310, r"World\Generic\Lamp.mdx".to_owned())];
        let made = made_of_doodad(
            &mut changes,
            r"world\generic\lamp.m2",
            position,
            rotation,
            scale,
            [10, 5000, 190_000],
            &displays,
        );
        assert_eq!(made.display, None, "the display that names its model");
        assert_eq!(made.template.display, 310);
        assert_eq!((made.template.entry, made.spawn.guid), (190_001, 5001));
        assert_eq!(made.template.size, 1.75, "a template of its size");
        assert_eq!(made.template.name, "lamp");
        assert_eq!(made.template.kind(&[]), 5);
        let link = Link {
            kind: Kind::GameObject,
            entry: 190_001,
            quest: 7,
            ends: false,
        };
        assert_eq!(made.template.kind(&[link]), 2, "a giver of quests");
        // Drawn as the doodad was, to a hundredth of a yard and of a degree.
        let doodad = uniwow_api::formats::placement(scene::file_of_world(position), rotation, scale);
        let object = Mat4::from_scale_rotation_translation(
            Vec3::splat(made.template.size),
            Quat::from_array(made.spawn.rotation.unwrap()),
            Vec3::from(made.spawn.position),
        );
        let (_, doodad_turn, doodad_at) = doodad.to_scale_rotation_translation();
        let (_, object_turn, object_at) = object.to_scale_rotation_translation();
        assert!(doodad_at.distance(object_at) < 0.01, "{doodad_at} {object_at}");
        assert!(doodad_turn.angle_between(object_turn).to_degrees() < 0.01);
        assert!(doodad.abs_diff_eq(object, 0.01));
        let forward = object_turn * Vec3::X;
        assert!((made.spawn.orientation - forward.y.atan2(forward.x).rem_euclid(std::f32::consts::TAU)).abs() < 1e-4);
        // A model no row names: a row prepared above the highest, the same for the next of it.
        let first = made_of_doodad(
            &mut changes,
            r"world\new\totem.m2",
            position,
            [0.0; 3],
            1.0,
            [10, 5000, 190_000],
            &displays,
        );
        assert_eq!(first.display, Some((311, r"world\new\totem.m2".to_owned())));
        let second = made_of_doodad(
            &mut changes,
            r"world\new\totem.m2",
            position,
            [0.0; 3],
            1.0,
            [10, 5000, 190_000],
            &displays,
        );
        assert_eq!((second.display, second.template.display), (None, 311));
        assert_eq!(second.template.entry, 190_003);
        assert_eq!(changes.added.len(), 3);
        // Kept in the project whole.
        assert_eq!(Changes::of_section(&changes.section()), changes);
        // Turned about the vertical: its rotation that turn alone.
        let mut turned = changes.clone();
        turned.added.clear();
        let spawn = made.spawn.clone();
        turned.moved.insert(spawn.id(), (spawn.position, 1.0));
        let laid = turned.laid_over(std::slice::from_ref(&spawn));
        assert_eq!(laid[0].rotation, Some(yaw_quaternion(1.0)));
    }

    #[test]
    fn the_creatures_are_read_by_the_names_of_each_version_of_azerothcore() {
        let rows_of = |count: usize| Rows {
            columns: vec!["Field".to_owned()],
            rows: vec![vec![json!("x")]; count],
        };
        let recent = Schema::of(&rows_of(1), &rows_of(1));
        assert_eq!(
            recent,
            Schema {
                entry: "id1",
                models_table: true
            }
        );
        let query = creatures(recent);
        assert!(
            query.contains("c.id1 AS id1") && query.contains("creature_template_model m"),
            "{query}"
        );
        let older = Schema::of(&rows_of(0), &rows_of(0));
        assert_eq!(
            older,
            Schema {
                entry: "id",
                models_table: false
            }
        );
        let query = creatures(older);
        assert!(query.contains("c.id AS id1"), "{query}");
        assert!(
            query.contains("t.modelid1 AS CreatureDisplayID") && !query.contains("creature_template_model"),
            "{query}"
        );
        assert!(!query.contains("c.id1"), "{query}");
        for paths in [crate::paths::addon_paths("id"), crate::paths::smart_paths("id")] {
            assert!(
                paths.contains("c.id ")
                    || paths.contains(
                        "c.id
"
                    )
                    || paths.contains("= c.id"),
                "{paths}"
            );
            assert!(!paths.contains("c.id1"), "{paths}");
        }
        // Read as the rows of either give them: the entry under `id1`.
        let rows = Rows {
            columns: [
                "guid",
                "id1",
                "position_x",
                "position_y",
                "position_z",
                "orientation",
                "name",
                "CreatureDisplayID",
                "DisplayScale",
            ]
            .map(str::to_owned)
            .to_vec(),
            rows: vec![vec![
                json!(79666),
                json!(68),
                json!(-8800.0),
                json!(640.0),
                json!(94.0),
                json!(0.0),
                json!("Stormwind City Guard"),
                json!(3167),
                json!(1.0),
            ]],
        };
        assert_eq!(spawns(&rows, Kind::Creature)[0].entry, 68);
        assert!(uniwow_api::db::allowed(ENTRY_COLUMN).is_ok() && uniwow_api::db::allowed(MODELS_TABLE).is_ok());
    }

    #[test]
    fn the_spawns_of_game_events_are_told_by_their_event() {
        let mut spawns = spawns(&guard(), Kind::Creature);
        spawns.extend(spawns_of_objects());
        let rows = |pairs: Vec<(u64, i64)>| Rows {
            columns: vec!["guid".to_owned(), "eventEntry".to_owned()],
            rows: pairs
                .into_iter()
                .map(|(guid, event)| vec![json!(guid), json!(event)])
                .collect(),
        };
        set_events(&mut spawns, Kind::Creature, &rows(vec![(79753, 2), (5714, 9)]));
        set_events(&mut spawns, Kind::GameObject, &rows(vec![(5714, -12)]));
        let events: Vec<(EntityId, i32)> = spawns.iter().map(|spawn| (spawn.id(), spawn.event)).collect();
        assert_eq!(
            events,
            [
                (EntityId::creature(79753), 2),
                (EntityId::creature(79754), 0),
                (EntityId::game_object(5714), -12),
            ],
            "a guid of the other table left alone"
        );
        assert_eq!(
            read(&spawns[0], false, &[], &BTreeMap::new())["components"]["raw"]["game event"],
            2
        );
    }
}
