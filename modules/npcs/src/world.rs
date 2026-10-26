//! What the world database tells the sheet (step 18.2): the creatures, their names in every
//! language, their kind, the models they wear and the loots they leave, read once a server by a
//! job; a loot's contents read when the pointer is over it.

use std::collections::{BTreeMap, HashMap};

use uniwow_api::db::Rows;
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db;

/// A creature of the database: its entry, its names (the English first), its kind, its loots.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Creature {
    pub entry: u32,
    pub names: Vec<String>,
    pub kind: u8,
    /// Its loots: `lootid`, `pickpocketloot`, `skinloot`.
    pub loots: [u32; 3],
}

impl Creature {
    pub fn name(&self) -> &str {
        self.names.first().map_or("", String::as_str)
    }
}

/// The tables of the loots, in the order of `Creature::loots`, and the columns naming them.
pub const LOOTS: [(&str, &str); 3] = [
    ("creature_loot_template", "lootid"),
    ("pickpocketing_loot_template", "pickpocketloot"),
    ("skinning_loot_template", "skinloot"),
];

/// A thing of the database named in every language: its id, its names (the English first).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Named {
    pub id: u32,
    pub names: Vec<String>,
}

impl Named {
    pub fn name(&self) -> &str {
        self.names.first().map_or("", String::as_str)
    }
}

/// What can be chosen of the database by name: its quests, game objects, items, paths (named by
/// the creatures walking them and their points) and gossip menus (by their first text).
#[derive(Clone, Debug, Default)]
pub struct Lists {
    pub quests: Vec<Named>,
    pub objects: Vec<Named>,
    pub items: Vec<Named>,
    pub paths: Vec<Named>,
    pub gossips: Vec<Named>,
}

/// The things of `rows` (id, name), their names of `locales` (id, name), by id.
pub fn named_of(rows: &Rows, locales: Option<&Rows>) -> Vec<Named> {
    let mut out: Vec<Named> = rows
        .rows
        .iter()
        .filter_map(|row| {
            Some(Named {
                id: number(row.first())? as u32,
                names: vec![text(row.get(1))],
            })
        })
        .collect();
    out.sort_by_key(|named| named.id);
    out.dedup_by_key(|named| named.id);
    let places: HashMap<u32, usize> = out.iter().enumerate().map(|(at, named)| (named.id, at)).collect();
    for row in locales.iter().flat_map(|rows| &rows.rows) {
        let name = text(row.get(1));
        if let Some(at) = number(row.first()).and_then(|id| places.get(&(id as u32)))
            && !name.is_empty()
            && !out[*at].names.contains(&name)
        {
            out[*at].names.push(name);
        }
    }
    out
}

/// The one of `list` of the id `id`.
pub fn find(list: &[Named], id: u32) -> Option<&Named> {
    list.binary_search_by_key(&id, |named| named.id)
        .ok()
        .map(|at| &list[at])
}

/// Those of `list` whose id begins with `search` or one of whose names holds it, `limit` at most.
pub fn search<'a>(list: &'a [Named], search: &str, limit: usize) -> Vec<&'a Named> {
    let search = search.trim().to_lowercase();
    list.iter()
        .filter(|named| {
            search.is_empty()
                || named.id.to_string().starts_with(&search)
                || named.names.iter().any(|name| name.to_lowercase().contains(&search))
        })
        .take(limit)
        .collect()
}

impl Lists {
    /// Reads the lists from the world database, from a job; a table refused left empty.
    pub fn read(database: &world_db::Handle) -> Lists {
        let query = |sql: &str| database.query(sql, &[]).ok();
        let pair = |rows: &str, locales: &str| -> Vec<Named> {
            query(rows).map_or_else(Vec::new, |rows| named_of(&rows, query(locales).as_ref()))
        };
        Lists {
            quests: pair(
                "SELECT ID, LogTitle FROM quest_template",
                "SELECT ID, Title FROM quest_template_locale",
            ),
            objects: pair(
                "SELECT entry, name FROM gameobject_template",
                "SELECT entry, name FROM gameobject_template_locale",
            ),
            items: pair("SELECT entry, name FROM item_template", "SELECT ID, Name FROM item_template_locale"),
            // the creature of a spawn is `id1`, or `id` (as the spawns read it)
            paths: ["id1", "id"]
                .iter()
                .find_map(|column| {
                    query(&format!(
                        "SELECT w.id, CONCAT(COALESCE(MAX(t.name), 'no creature'), ', ', COUNT(DISTINCT w.point), ' points') FROM waypoint_data w LEFT JOIN creature_addon a ON a.path_id = w.id LEFT JOIN creature c ON c.guid = a.guid LEFT JOIN creature_template t ON t.entry = c.{column} GROUP BY w.id"
                    ))
                })
                .map_or_else(Vec::new, |rows| named_of(&rows, None)),
            gossips: query(
                "SELECT g.MenuID, LEFT(COALESCE(MAX(n.text0_0), MAX(n.text0_1), ''), 60) FROM gossip_menu g LEFT JOIN npc_text n ON n.ID = g.TextID GROUP BY g.MenuID",
            )
            .map_or_else(Vec::new, |rows| named_of(&rows, None)),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct World {
    pub creatures: Vec<Creature>,
    /// The creatures wearing each display, by their place in `creatures`.
    pub wearers: HashMap<u32, Vec<usize>>,
    pub lists: Lists,
}

fn number(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64().or_else(|| number.as_f64().map(|real| real as u64)),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

impl World {
    /// The creatures of `templates` (entry, name, type, lootid, pickpocketloot, skinloot), their
    /// names of `locales` (entry, name), the displays of `models` (creature, display).
    pub fn of_rows(templates: &Rows, locales: Option<&Rows>, models: Option<&Rows>) -> Self {
        let mut creatures: Vec<Creature> = templates
            .rows
            .iter()
            .filter_map(|row| {
                let field = |at: usize| number(row.get(at)).unwrap_or(0) as u32;
                Some(Creature {
                    entry: number(row.first())? as u32,
                    names: vec![text(row.get(1))],
                    kind: field(2) as u8,
                    loots: [field(3), field(4), field(5)],
                })
            })
            .collect();
        creatures.sort_by_key(|creature| creature.entry);
        let places: HashMap<u32, usize> = creatures
            .iter()
            .enumerate()
            .map(|(at, creature)| (creature.entry, at))
            .collect();
        for row in locales.iter().flat_map(|rows| &rows.rows) {
            let name = text(row.get(1));
            if let Some(at) = number(row.first()).and_then(|entry| places.get(&(entry as u32)))
                && !name.is_empty()
                && !creatures[*at].names.contains(&name)
            {
                creatures[*at].names.push(name);
            }
        }
        let mut wearers: HashMap<u32, Vec<usize>> = HashMap::new();
        for row in models.iter().flat_map(|rows| &rows.rows) {
            if let (Some(at), Some(display)) = (
                number(row.first()).and_then(|entry| places.get(&(entry as u32))),
                number(row.get(1)),
            ) {
                let wearing = wearers.entry(display as u32).or_default();
                if !wearing.contains(at) {
                    wearing.push(*at);
                }
            }
        }
        Self {
            creatures,
            wearers,
            lists: Lists::default(),
        }
    }

    /// Reads the creatures from the world database, from a job.
    pub fn read(database: &world_db::Handle) -> Result<World, String> {
        let templates = database.query(
            "SELECT entry, name, type, lootid, pickpocketloot, skinloot FROM creature_template",
            &[],
        )?;
        let locales = database
            .query("SELECT entry, Name FROM creature_template_locale", &[])
            .ok();
        let models = database
            .query("SELECT CreatureID, CreatureDisplayID FROM creature_template_model", &[])
            .ok();
        let mut world = World::of_rows(&templates, locales.as_ref(), models.as_ref());
        world.lists = Lists::read(database);
        Ok(world)
    }

    /// The creatures whose loot of kind `kind` (0 to 2) is not none and whose entry begins with
    /// `search`, or one of whose names holds it whatever its case; `limit` at most.
    pub fn loot_owners(&self, kind: usize, search: &str, limit: usize) -> Vec<&Creature> {
        let search = search.trim().to_lowercase();
        self.creatures
            .iter()
            .filter(|creature| creature.loots[kind] != 0)
            .filter(|creature| {
                search.is_empty()
                    || creature.entry.to_string().starts_with(&search)
                    || creature.names.iter().any(|name| name.to_lowercase().contains(&search))
            })
            .take(limit)
            .collect()
    }

    /// The creatures whose loot of kind `kind` is `loot`.
    pub fn owners_of(&self, kind: usize, loot: u32) -> impl Iterator<Item = &Creature> {
        self.creatures
            .iter()
            .filter(move |creature| loot != 0 && creature.loots[kind] == loot)
    }
}

/// A line of a loot: the item (or the loot it refers to, `reference`), its chance in percent, how
/// many; the item's name, quality and display.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drop {
    pub item: u32,
    pub reference: u32,
    pub chance: f32,
    pub counts: (u32, u32),
    pub name: String,
    pub quality: u8,
    pub display: u32,
}

/// Reads the loot `loot` of the table `table` and its items' names, from a job.
pub fn read_loot(database: &world_db::Handle, table: &str, loot: u32) -> Result<Vec<Drop>, String> {
    let rows = database.query(
        &format!("SELECT Item, Reference, Chance, MinCount, MaxCount FROM {table} WHERE Entry = ?"),
        &[json!(loot)],
    )?;
    let mut drops: Vec<Drop> = rows
        .rows
        .iter()
        .filter_map(|row| {
            Some(Drop {
                item: number(row.first())? as u32,
                reference: number(row.get(1)).unwrap_or(0) as u32,
                chance: row.get(2).and_then(Value::as_f64).unwrap_or(0.0) as f32,
                counts: (
                    number(row.get(3)).unwrap_or(1) as u32,
                    number(row.get(4)).unwrap_or(1) as u32,
                ),
                ..Drop::default()
            })
        })
        .collect();
    let items: Vec<Value> = drops
        .iter()
        .filter(|d| d.reference == 0)
        .map(|d| json!(d.item))
        .collect();
    if !items.is_empty() {
        let marks = vec!["?"; items.len()].join(", ");
        let named = database.query(
            &format!("SELECT entry, name, Quality, displayid FROM item_template WHERE entry IN ({marks})"),
            &items,
        )?;
        let by_entry: BTreeMap<u32, (String, u8, u32)> = named
            .rows
            .iter()
            .filter_map(|row| {
                Some((
                    number(row.first())? as u32,
                    (
                        text(row.get(1)),
                        number(row.get(2)).unwrap_or(0) as u8,
                        number(row.get(3)).unwrap_or(0) as u32,
                    ),
                ))
            })
            .collect();
        for drop in &mut drops {
            if let Some((name, quality, display)) = by_entry.get(&drop.item) {
                (drop.name, drop.quality, drop.display) = (name.clone(), *quality, *display);
            }
        }
    }
    drops.sort_by(|a, b| b.chance.total_cmp(&a.chance));
    Ok(drops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(values: Vec<Vec<Value>>) -> Rows {
        Rows {
            columns: Vec::new(),
            rows: values,
        }
    }

    #[test]
    fn the_creatures_are_found_by_a_name_of_any_language_with_their_models_and_loots() {
        let world = World::of_rows(
            &rows(vec![
                vec![
                    json!(15274),
                    json!("Mana Wyrm"),
                    json!(1),
                    json!(15274),
                    json!(0),
                    json!(0),
                ],
                vec![
                    json!(15271),
                    json!("Springpaw Cub"),
                    json!(1),
                    json!(0),
                    json!(0),
                    json!(15271),
                ],
            ]),
            Some(&rows(vec![vec![json!(15274), json!("Wyrm de mana")]])),
            Some(&rows(vec![
                vec![json!(15274), json!(15468)],
                vec![json!(15271), json!(15468)],
            ])),
        );
        assert_eq!(world.wearers[&15468].len(), 2);
        let owners: Vec<u32> = world.loot_owners(0, "wyrm de", 10).iter().map(|c| c.entry).collect();
        assert_eq!(owners, [15274], "by its French name");
        assert!(world.loot_owners(0, "springpaw", 10).is_empty(), "no loot of a kill");
        assert_eq!(world.loot_owners(2, "1527", 10)[0].entry, 15271);
        assert_eq!(world.owners_of(0, 15274).count(), 1);
        assert_eq!(world.creatures[1].name(), "Mana Wyrm");
        let quests = named_of(
            &rows(vec![vec![json!(783), json!("A Threat Within")]]),
            Some(&rows(vec![vec![json!(783), json!("Une menace intérieure")]])),
        );
        assert_eq!(search(&quests, "menace", 10)[0].id, 783);
        assert_eq!(find(&quests, 783).map(Named::name), Some("A Threat Within"));
    }
}
