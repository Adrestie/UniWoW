//! What a quest names, to be chosen (step 16.3, its page): the items, creatures and game objects of
//! the world database, read once a server by a job with their names in every language, then
//! searched in memory as the user types, by id or by any of their names; the icons of the items,
//! of the client's `ItemDisplayInfo.dbc` and `Interface\Icons`, each decoded once by a job.

use std::collections::HashMap;
use std::sync::Arc;

use uniwow_api::db::Rows;
use uniwow_api::egui::Color32;
use uniwow_api::serde_json::Value;
use uniwow_api::world_db;

/// What is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Item,
    Creature,
    GameObject,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Item => "item",
            Kind::Creature => "creature",
            Kind::GameObject => "game object",
        }
    }
}

/// An item, creature or game object: its id, its name, its other names (the client's languages,
/// lowered for the search), an item's quality and display.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub id: u32,
    pub name: String,
    pub names: Vec<String>,
    pub quality: u8,
    pub display: u32,
}

/// Everything that can be chosen, by kind, and each one's place by id.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub entries: HashMap<Kind, Vec<Entry>>,
    pub by_id: HashMap<(Kind, u32), usize>,
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

/// The statements reading a kind: its rows (id, name, quality, display) and its names in the
/// other languages (id, name).
fn statements(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Item => (
            "SELECT entry, name, Quality, displayid FROM item_template",
            "SELECT ID, Name FROM item_template_locale",
        ),
        Kind::Creature => (
            "SELECT entry, name, 0, 0 FROM creature_template",
            "SELECT entry, Name FROM creature_template_locale",
        ),
        Kind::GameObject => (
            "SELECT entry, name, 0, 0 FROM gameobject_template",
            "SELECT entry, name FROM gameobject_template_locale",
        ),
    }
}

impl Catalog {
    /// The catalog of the rows of each kind and of their names in the other languages.
    pub fn of_rows(read: Vec<(Kind, Rows, Option<Rows>)>) -> Self {
        let mut out = Catalog::default();
        for (kind, rows, locales) in read {
            let mut entries: Vec<Entry> = rows
                .rows
                .iter()
                .filter_map(|row| {
                    Some(Entry {
                        id: number(row.first())? as u32,
                        name: text(row.get(1)),
                        names: Vec::new(),
                        quality: number(row.get(2)).unwrap_or(0) as u8,
                        display: number(row.get(3)).unwrap_or(0) as u32,
                    })
                })
                .collect();
            entries.sort_by_key(|entry| entry.id);
            let places: HashMap<u32, usize> = entries.iter().enumerate().map(|(at, entry)| (entry.id, at)).collect();
            for row in locales.iter().flat_map(|rows| &rows.rows) {
                let (Some(id), name) = (number(row.first()), text(row.get(1))) else {
                    continue;
                };
                if let Some(at) = places.get(&(id as u32))
                    && !name.is_empty()
                {
                    entries[*at].names.push(name.to_lowercase());
                }
            }
            for entry in &mut entries {
                entry.names.push(entry.name.to_lowercase());
                entry.names.sort();
                entry.names.dedup();
            }
            for (at, entry) in entries.iter().enumerate() {
                out.by_id.insert((kind, entry.id), at);
            }
            out.entries.insert(kind, entries);
        }
        out
    }

    /// Reads the catalog from the world database, from a job.
    pub fn read(database: &world_db::Handle) -> Result<Catalog, String> {
        let mut read = Vec::new();
        for kind in [Kind::Item, Kind::Creature, Kind::GameObject] {
            let (rows, locales) = statements(kind);
            let rows = database.query(rows, &[])?;
            // A database without the names in other languages: English alone.
            let locales = database.query(locales, &[]).ok();
            read.push((kind, rows, locales));
        }
        Ok(Catalog::of_rows(read))
    }

    pub fn get(&self, kind: Kind, id: u32) -> Option<&Entry> {
        let at = self.by_id.get(&(kind, id))?;
        self.entries.get(&kind)?.get(*at)
    }

    /// The entries of `kind` whose id is `search` or begins with it, or one of whose names holds it
    /// whatever its case; `limit` at most, the ids first.
    pub fn search(&self, kind: Kind, search: &str, limit: usize) -> Vec<&Entry> {
        let Some(entries) = self.entries.get(&kind) else {
            return Vec::new();
        };
        let search = search.trim().to_lowercase();
        if search.is_empty() {
            return entries.iter().take(limit).collect();
        }
        if let Ok(id) = search.parse::<u32>() {
            let mut out: Vec<&Entry> = self.get(kind, id).into_iter().collect();
            out.extend(
                entries
                    .iter()
                    .filter(|entry| entry.id != id && entry.id.to_string().starts_with(&search))
                    .take(limit.saturating_sub(1)),
            );
            return out;
        }
        entries
            .iter()
            .filter(|entry| entry.names.iter().any(|name| name.contains(&search)))
            .take(limit)
            .collect()
    }
}

/// The colour of an item's quality, as the client draws its name.
pub fn quality(quality: u8) -> Color32 {
    match quality {
        0 => Color32::from_rgb(157, 157, 157),
        1 => Color32::from_rgb(255, 255, 255),
        2 => Color32::from_rgb(30, 255, 0),
        3 => Color32::from_rgb(0, 112, 221),
        4 => Color32::from_rgb(163, 53, 238),
        5 => Color32::from_rgb(255, 128, 0),
        6 => Color32::from_rgb(230, 204, 128),
        _ => Color32::from_rgb(0, 204, 255),
    }
}

/// The icons of the displays of `ItemDisplayInfo.dbc`: each display's first inventory icon, a name
/// of `Interface\Icons` without its extension.
pub fn icons_of_dbc(bytes: &[u8]) -> Result<HashMap<u32, String>, String> {
    let word = |at: usize| -> Option<u32> { Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?)) };
    if bytes.get(0..4) != Some(b"WDBC") {
        return Err("ItemDisplayInfo.dbc is no DBC".to_owned());
    }
    let (count, fields, size, strings) = (
        word(4).unwrap_or(0) as usize,
        word(8).unwrap_or(0) as usize,
        word(12).unwrap_or(0) as usize,
        word(16).unwrap_or(0) as usize,
    );
    // The inventory icon, the 6th field: id, two models, two textures, then the icons.
    const ICON: usize = 5;
    if fields <= ICON || size < fields * 4 {
        return Err("ItemDisplayInfo.dbc has not the fields of 3.3.5a".to_owned());
    }
    let block = 20 + count * size;
    let table = bytes
        .get(block..block + strings)
        .ok_or("ItemDisplayInfo.dbc is cut short")?;
    let mut out = HashMap::with_capacity(count);
    for record in 0..count {
        let at = 20 + record * size;
        let (Some(id), Some(offset)) = (word(at), word(at + ICON * 4)) else {
            break;
        };
        let start = offset as usize;
        if start == 0 || start >= table.len() {
            continue;
        }
        let end = table[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(table.len(), |length| start + length);
        let name = String::from_utf8_lossy(&table[start..end]).into_owned();
        if !name.is_empty() {
            out.insert(id, name);
        }
    }
    Ok(out)
}

/// An icon decoded: its width, height and pixels, RGBA.
pub type Icon = (usize, usize, Vec<u8>);

/// Decodes the icon `name` of `Interface\Icons` through the client's formats.
pub fn decode(formats: &Arc<dyn uniwow_api::formats::Formats>, name: &str) -> Option<Icon> {
    let file = uniwow_api::formats::FileRef::Path(format!("Interface\\Icons\\{name}.blp"));
    let texture = formats.texture_rgba(&file).ok()?;
    let level = texture.levels.first()?.clone();
    let (width, height) = (texture.width as usize, texture.height as usize);
    (level.len() >= width * height * 4).then_some((width, height, level))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::serde_json::json;

    fn rows(values: Vec<Vec<Value>>) -> Rows {
        Rows {
            columns: Vec::new(),
            rows: values,
        }
    }

    #[test]
    fn the_catalog_is_searched_by_id_and_by_a_name_of_any_language() {
        let catalog = Catalog::of_rows(vec![
            (
                Kind::Item,
                rows(vec![
                    vec![json!(25), json!("Worn Shortsword"), json!(1), json!(1542)],
                    vec![json!(2589), json!("Linen Cloth"), json!(1), json!(7393)],
                    vec![json!(250), json!("Other"), json!(0), json!(0)],
                ]),
                Some(rows(vec![
                    vec![json!(2589), json!("Étoffe de lin")],
                    vec![json!(25), json!("")],
                ])),
            ),
            (
                Kind::Creature,
                rows(vec![vec![json!(15274), json!("Mana Wyrm"), json!(0), json!(0)]]),
                None,
            ),
        ]);
        assert_eq!(
            catalog.search(Kind::Item, "étoffe", 10)[0].id,
            2589,
            "by its French name"
        );
        assert_eq!(catalog.search(Kind::Item, "LINEN", 10)[0].id, 2589);
        let by_id: Vec<u32> = catalog
            .search(Kind::Item, "25", 10)
            .iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(by_id, [25, 250, 2589], "the id first, then those beginning with it");
        assert_eq!(
            catalog.get(Kind::Creature, 15274).map(|entry| entry.name.as_str()),
            Some("Mana Wyrm")
        );
        assert!(catalog.search(Kind::GameObject, "x", 10).is_empty());
        assert_eq!(catalog.get(Kind::Item, 2589).map(|entry| entry.quality), Some(1));
    }

    #[test]
    fn the_icon_of_a_display_is_read_from_its_dbc() {
        // Two records of 6 fields, the icon the 6th; the strings after.
        let strings = b"\0INV_Sword_04\0".to_vec();
        let mut bytes = b"WDBC".to_vec();
        for value in [2u32, 6, 24, strings.len() as u32] {
            bytes.extend(value.to_le_bytes());
        }
        for (id, icon) in [(1542u32, 1u32), (7393, 0)] {
            for value in [id, 0, 0, 0, 0, icon] {
                bytes.extend(value.to_le_bytes());
            }
        }
        bytes.extend(&strings);
        let icons = icons_of_dbc(&bytes).unwrap();
        assert_eq!(icons.get(&1542).map(String::as_str), Some("INV_Sword_04"));
        assert!(!icons.contains_key(&7393), "no icon");
        assert!(icons_of_dbc(b"nope").is_err());
    }
}
