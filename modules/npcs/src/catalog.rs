//! The items an NPC can hold (step 18.2): those of the world database, read once a server by a job
//! with their names in every language, searched in memory as the user types, by id or by any of
//! their names; each its quality, display and kind of slot.

use std::collections::HashMap;

use uniwow_api::db::Rows;
use uniwow_api::egui::Color32;
use uniwow_api::serde_json::Value;
use uniwow_api::world_db;

/// An item: its id, its name, its other names (lowered for the search), its quality, display and
/// slot (`InventoryType`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub id: u32,
    pub name: String,
    pub names: Vec<String>,
    pub quality: u8,
    pub display: u32,
    pub slot: u8,
}

/// The kinds of slot an item of the hands has (`InventoryType`).
pub const SHIELD: u8 = 14;
const HELD: [u8; 10] = [13, 14, 15, 17, 21, 22, 23, 25, 26, 28];
const RANGED: [u8; 4] = [15, 25, 26, 28];

/// Whether an item of `slot` goes in the slot of weapons `at` (0 main hand, 1 off hand, 2 ranged).
pub fn fits(slot: u8, at: usize) -> bool {
    match at {
        0 => matches!(slot, 13 | 17 | 21),
        1 => matches!(slot, 13 | 14 | 22 | 23),
        _ => RANGED.contains(&slot),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub items: Vec<Item>,
    by_id: HashMap<u32, usize>,
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

impl Catalog {
    /// The items of `rows` (entry, name, quality, display, slot) held in the hands, their names in
    /// the other languages from `locales` (id, name).
    pub fn of_rows(rows: &Rows, locales: Option<&Rows>) -> Self {
        let mut items: Vec<Item> = rows
            .rows
            .iter()
            .filter_map(|row| {
                Some(Item {
                    id: number(row.first())? as u32,
                    name: text(row.get(1)),
                    names: Vec::new(),
                    quality: number(row.get(2)).unwrap_or(0) as u8,
                    display: number(row.get(3)).unwrap_or(0) as u32,
                    slot: number(row.get(4)).unwrap_or(0) as u8,
                })
            })
            .filter(|item| HELD.contains(&item.slot))
            .collect();
        items.sort_by_key(|item| item.id);
        let by_id: HashMap<u32, usize> = items.iter().enumerate().map(|(at, item)| (item.id, at)).collect();
        for row in locales.iter().flat_map(|rows| &rows.rows) {
            let (Some(id), name) = (number(row.first()), text(row.get(1))) else {
                continue;
            };
            if let Some(at) = by_id.get(&(id as u32))
                && !name.is_empty()
            {
                items[*at].names.push(name.to_lowercase());
            }
        }
        for item in &mut items {
            item.names.push(item.name.to_lowercase());
            item.names.sort();
            item.names.dedup();
        }
        Self { items, by_id }
    }

    /// Reads the catalog from the world database, from a job.
    pub fn read(database: &world_db::Handle) -> Result<Catalog, String> {
        let rows = database.query(
            "SELECT entry, name, Quality, displayid, InventoryType FROM item_template WHERE InventoryType IN (13, 14, 15, 17, 21, 22, 23, 25, 26, 28)",
            &[],
        )?;
        // A database without the names in other languages: English alone.
        let locales = database.query("SELECT ID, Name FROM item_template_locale", &[]).ok();
        Ok(Catalog::of_rows(&rows, locales.as_ref()))
    }

    pub fn get(&self, id: u32) -> Option<&Item> {
        self.items.get(*self.by_id.get(&id)?)
    }

    /// The items of the slot `at` whose id is `search` or begins with it, or one of whose names
    /// holds it whatever its case; `limit` at most, the id first.
    pub fn search(&self, at: usize, search: &str, limit: usize) -> Vec<&Item> {
        let search = search.trim().to_lowercase();
        let fitting = self.items.iter().filter(|item| fits(item.slot, at));
        if let Ok(id) = search.parse::<u32>() {
            let mut out: Vec<&Item> = self.get(id).filter(|item| fits(item.slot, at)).into_iter().collect();
            out.extend(
                fitting
                    .filter(|item| item.id != id && item.id.to_string().starts_with(&search))
                    .take(limit.saturating_sub(1)),
            );
            return out;
        }
        fitting
            .filter(|item| search.is_empty() || item.names.iter().any(|name| name.contains(&search)))
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

/// An icon decoded: its width, height and pixels, RGBA.
pub type Icon = (usize, usize, Vec<u8>);

/// Decodes the icon `name` of `Interface\Icons` through the client's formats.
pub fn decode(formats: &std::sync::Arc<dyn uniwow_api::formats::Formats>, name: &str) -> Option<Icon> {
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
    fn the_items_of_the_hands_are_searched_by_slot_by_id_and_by_a_name_of_any_language() {
        let catalog = Catalog::of_rows(
            &rows(vec![
                vec![json!(25), json!("Worn Shortsword"), json!(1), json!(1542), json!(21)],
                vec![json!(2504), json!("Worn Shortbow"), json!(1), json!(8106), json!(15)],
                vec![
                    json!(2129),
                    json!("Large Round Shield"),
                    json!(1),
                    json!(18484),
                    json!(14),
                ],
                vec![json!(6125), json!("Brawler's Harness"), json!(1), json!(9891), json!(5)],
            ]),
            Some(&rows(vec![vec![json!(25), json!("Épée courte usée")]])),
        );
        assert!(catalog.get(6125).is_none(), "a harness is not held");
        assert_eq!(catalog.search(0, "épée", 10)[0].id, 25, "by its French name");
        assert!(
            catalog.search(1, "épée", 10).is_empty(),
            "a main hand weapon is not for the off hand"
        );
        assert_eq!(catalog.search(1, "shield", 10)[0].id, 2129);
        assert_eq!(
            catalog.search(2, "25", 10)[0].id,
            2504,
            "the bow, whose id begins with 25"
        );
        assert_eq!(catalog.search(0, "", 10).len(), 1);
        assert!(fits(SHIELD, 1) && !fits(SHIELD, 0));
    }
}
