//! The spawns of the world database as the module keeps them: read from the rows of `creature` and
//! `gameobject` of a map joined to their templates; changed, created and deleted in memory, laid
//! over the rows; given to the scene by tile.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use uniwow_api::db::Rows;
use uniwow_api::formats::TileId;
use uniwow_api::scene::{self, Entity, EntityId, Kind, Piece, PieceBuilder, flags};
use uniwow_api::serde_json::{Value, json};

/// The creatures of a map, with their template's name and first look.
pub const CREATURES: &str = "SELECT c.guid, c.id1, c.position_x, c.position_y, c.position_z, c.orientation, \
     t.name, m.CreatureDisplayID, m.DisplayScale FROM creature c JOIN creature_template t ON t.entry = c.id1 \
     LEFT JOIN creature_template_model m ON m.CreatureID = c.id1 AND m.Idx = 0 WHERE c.map = ?";
/// The game objects of a map, with their template's name, look and size.
pub const OBJECTS: &str = "SELECT g.guid, g.id, g.position_x, g.position_y, g.position_z, g.orientation, \
     t.name, t.displayId, t.size FROM gameobject g JOIN gameobject_template t ON t.entry = g.id WHERE g.map = ?";
/// The highest guid of each table, so that a spawn created takes one above.
pub const HIGHEST: [&str; 2] = ["SELECT MAX(guid) FROM creature", "SELECT MAX(guid) FROM gameobject"];

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
}

impl Changes {
    /// `spawns` as the user changed them: those deleted left out, those created added, those moved
    /// at their places.
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
            if let Some((position, orientation)) = self.moved.get(&spawn.id()) {
                spawn.position = *position;
                spawn.orientation = *orientation;
            }
        }
        out
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
        let given = self.given.entry(kind.name().to_owned()).or_insert(0);
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
            })).collect::<Vec<_>>(),
            "deleted": deleted.into_iter().map(ToString::to_string).collect::<Vec<_>>(),
            "guids given": given,
            "quests given": links(&self.linked),
            "quests taken": links(&self.unlinked),
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
        }
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
        "rotation" => match axis {
            Some(1) | None => {
                let degrees = scene::changed_value(orientation.to_degrees(), change)?;
                orientation = degrees.to_radians().rem_euclid(std::f32::consts::TAU);
            }
            Some(_) => return Err("a spawn turns about the vertical alone".to_owned()),
        },
        "scale" => return Err("a spawn takes the size of its template".to_owned()),
        other => return Err(format!("a spawn has no field '{other}'")),
    }
    Ok((position, orientation))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(columns: &[&str], rows: Vec<Vec<Value>>) -> Rows {
        Rows {
            columns: columns.iter().map(|column| (*column).to_owned()).collect(),
            rows,
        }
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
}
