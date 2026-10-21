//! The writes of the editor to the world database (step 16.1): rows changed by their keys, in the
//! tables the editor writes alone, as one transaction; each change journaled with the rows it
//! replaced, for the application to be undone and kept as a script that does it again.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// A row: its columns and their values.
pub type Row = BTreeMap<String, Value>;

/// The tables the editor writes, and none else.
pub const TABLES: &[&str] = &[
    "creature",
    "creature_addon",
    "creature_formations",
    "creature_queststarter",
    "creature_questender",
    "creature_template",
    "game_event_creature",
    "game_event_gameobject",
    "gameobject",
    "gameobject_queststarter",
    "gameobject_questender",
    "quest_offer_reward",
    "quest_offer_reward_locale",
    "quest_request_items",
    "quest_request_items_locale",
    "quest_template",
    "quest_template_addon",
    "quest_template_locale",
    "updates",
    "waypoint_data",
];

/// A change of rows, by their key: columns of the table and their values, never none.
#[derive(Clone, Debug, PartialEq)]
pub enum RowChange {
    /// The one row of `key` given the values of `set`; inserted when there is none, as the key, the
    /// row of `from` copied when one is named, and `set`.
    Put {
        table: String,
        key: Row,
        set: Row,
        from: Option<Row>,
    },
    /// Every row of `key` taken away.
    Delete { table: String, key: Row },
    /// `rows` inserted, no row of `key` being there.
    Insert { table: String, key: Row, rows: Vec<Row> },
}

impl RowChange {
    pub fn table(&self) -> &str {
        match self {
            RowChange::Put { table, .. } | RowChange::Delete { table, .. } | RowChange::Insert { table, .. } => table,
        }
    }

    pub fn key(&self) -> &Row {
        match self {
            RowChange::Put { key, .. } | RowChange::Delete { key, .. } | RowChange::Insert { key, .. } => key,
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            RowChange::Put { table, key, set, from } => {
                json!({ "put": table, "key": key, "set": set, "from": from })
            }
            RowChange::Delete { table, key } => json!({ "delete": table, "key": key }),
            RowChange::Insert { table, key, rows } => json!({ "insert": table, "key": key, "rows": rows }),
        }
    }

    pub fn of_json(value: &Value) -> Option<RowChange> {
        let row = |value: &Value| -> Option<Row> {
            Some(
                value
                    .as_object()?
                    .iter()
                    .map(|(column, value)| (column.clone(), value.clone()))
                    .collect(),
            )
        };
        let key = row(&value["key"])?;
        if let Some(table) = value["put"].as_str() {
            return Some(RowChange::Put {
                table: table.to_owned(),
                key,
                set: row(&value["set"])?,
                from: row(&value["from"]),
            });
        }
        if let Some(table) = value["delete"].as_str() {
            return Some(RowChange::Delete {
                table: table.to_owned(),
                key,
            });
        }
        let table = value["insert"].as_str()?;
        Some(RowChange::Insert {
            table: table.to_owned(),
            key,
            rows: value["rows"].as_array()?.iter().map(row).collect::<Option<_>>()?,
        })
    }
}

/// What a change did: its table and key, the rows of the key before and after.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub table: String,
    pub key: Row,
    pub before: Vec<Row>,
    pub after: Vec<Row>,
}

impl Entry {
    pub fn to_json(&self) -> Value {
        json!({ "table": self.table, "key": self.key, "before": self.before, "after": self.after })
    }

    pub fn of_json(value: &Value) -> Option<Entry> {
        let row = |value: &Value| -> Option<Row> {
            Some(
                value
                    .as_object()?
                    .iter()
                    .map(|(column, value)| (column.clone(), value.clone()))
                    .collect(),
            )
        };
        let rows = |value: &Value| -> Option<Vec<Row>> { value.as_array()?.iter().map(row).collect() };
        Some(Entry {
            table: value["table"].as_str()?.to_owned(),
            key: row(&value["key"])?,
            before: rows(&value["before"])?,
            after: rows(&value["after"])?,
        })
    }
}

/// Where the rows are: the database, in a transaction, or rows in memory for the tests.
pub trait Store {
    /// The rows of `table` whose columns hold the values of `key`.
    fn select(&mut self, table: &str, key: &Row) -> Result<Vec<Row>, String>;
    fn insert(&mut self, table: &str, row: &Row) -> Result<(), String>;
    /// The one row of `key` given the values of `set`.
    fn update(&mut self, table: &str, key: &Row, set: &Row) -> Result<(), String>;
    fn delete(&mut self, table: &str, key: &Row) -> Result<(), String>;
}

/// Whether `name` may name a table or a column: letters, digits and `_`, 64 at most.
pub fn identifier(name: &str) -> Result<&str, String> {
    if !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(name)
    } else {
        Err(format!("'{name}' names no table or column"))
    }
}

/// Whether `change` may be made: a table the editor writes, a key, names of columns, values that
/// are no arrays or objects.
pub fn check(change: &RowChange) -> Result<(), String> {
    let table = change.table();
    if !TABLES.contains(&table) {
        return Err(format!("the editor writes no table '{table}'"));
    }
    if change.key().is_empty() {
        return Err(format!("a change of {table} without a key"));
    }
    let rows: Vec<&Row> = match change {
        RowChange::Put { key, set, from, .. } => [Some(key), Some(set), from.as_ref()].into_iter().flatten().collect(),
        RowChange::Delete { key, .. } => vec![key],
        RowChange::Insert { key, rows, .. } => std::iter::once(key).chain(rows).collect(),
    };
    for row in rows {
        for (column, value) in row {
            identifier(column)?;
            if value.is_array() || value.is_object() {
                return Err(format!("the value of {table}.{column} is no value of a column"));
            }
        }
    }
    if let RowChange::Insert { key, rows, .. } = change
        && rows
            .iter()
            .any(|row| key.iter().any(|(column, value)| row.get(column) != Some(value)))
    {
        return Err(format!("a row inserted into {table} that its key does not name"));
    }
    Ok(())
}

/// Makes `changes` in `store`, in their order: what each did. The first refused stops them; the
/// caller rolls back what was made.
pub fn apply(store: &mut dyn Store, changes: &[RowChange]) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for change in changes {
        check(change)?;
        let (table, key) = (change.table(), change.key());
        let before = store.select(table, key)?;
        let after = match change {
            RowChange::Put { set, from, .. } => match before.as_slice() {
                [] => {
                    let mut row = match from {
                        Some(from) => match store.select(table, from)?.as_slice() {
                            [copied] => copied.clone(),
                            found => {
                                return Err(format!("the row of {table} copied is {} rows, not one", found.len()));
                            }
                        },
                        None => Row::new(),
                    };
                    row.extend(key.iter().map(|(column, value)| (column.clone(), value.clone())));
                    row.extend(set.iter().map(|(column, value)| (column.clone(), value.clone())));
                    store.insert(table, &row)?;
                    vec![row]
                }
                [one] => {
                    let changed: Row = set
                        .iter()
                        .filter(|(column, value)| one.get(*column) != Some(*value))
                        .map(|(column, value)| (column.clone(), value.clone()))
                        .collect();
                    if !changed.is_empty() {
                        store.update(table, key, &changed)?;
                    }
                    let mut row = one.clone();
                    row.extend(changed);
                    vec![row]
                }
                many => {
                    return Err(format!(
                        "the key of {table} {} names {} rows, not one",
                        json!(key),
                        many.len()
                    ));
                }
            },
            RowChange::Delete { .. } => {
                if !before.is_empty() {
                    store.delete(table, key)?;
                }
                Vec::new()
            }
            RowChange::Insert { rows, .. } => {
                if !before.is_empty() {
                    return Err(format!("{table} has a row of the key {} already", json!(key)));
                }
                for row in rows {
                    store.insert(table, row)?;
                }
                rows.clone()
            }
        };
        entries.push(Entry {
            table: table.to_owned(),
            key: key.clone(),
            before,
            after,
        });
    }
    Ok(entries)
}

/// The changes undoing `entries`, the last first: each key's rows taken away, those it had before
/// put back.
pub fn revert(entries: &[Entry]) -> Vec<RowChange> {
    let mut changes = Vec::new();
    for entry in entries.iter().rev() {
        if entry.before == entry.after {
            continue;
        }
        changes.push(RowChange::Delete {
            table: entry.table.clone(),
            key: entry.key.clone(),
        });
        if !entry.before.is_empty() {
            changes.push(RowChange::Insert {
                table: entry.table.clone(),
                key: entry.key.clone(),
                rows: entry.before.clone(),
            });
        }
    }
    changes
}

/// `value` written in SQL.
pub fn literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Bool(value) => if *value { "1" } else { "0" }.to_owned(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => {
            let mut out = String::with_capacity(text.len() + 2);
            out.push('\'');
            for c in text.chars() {
                match c {
                    '\'' => out.push_str("\\'"),
                    '\\' => out.push_str("\\\\"),
                    '\0' => out.push_str("\\0"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\u{1a}' => out.push_str("\\Z"),
                    c => out.push(c),
                }
            }
            out.push('\'');
            out
        }
        other => literal(&Value::String(other.to_string())),
    }
}

/// The condition of `key`: each column equal to its value, `?` for each in the order of the key.
pub fn condition(key: &Row) -> String {
    key.keys()
        .map(|column| format!("`{column}` = ?"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// The script making again what `entries` did, each key's rows taken away and those after
/// inserted: as many times as it runs, the rows are those after. Headed by `title`.
pub fn script(title: &str, entries: &[Entry]) -> String {
    let mut out = String::new();
    for line in title.lines() {
        out.push_str("-- ");
        out.push_str(line);
        out.push('\n');
    }
    for entry in entries.iter().filter(|entry| entry.before != entry.after) {
        let condition = entry
            .key
            .iter()
            .map(|(column, value)| format!("`{column}` = {}", literal(value)))
            .collect::<Vec<_>>()
            .join(" AND ");
        out.push_str(&format!("DELETE FROM `{}` WHERE {condition};\n", entry.table));
        for row in &entry.after {
            let columns = row
                .keys()
                .map(|column| format!("`{column}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let values = row.values().map(literal).collect::<Vec<_>>().join(", ");
            out.push_str(&format!(
                "INSERT INTO `{}` ({columns}) VALUES ({values});\n",
                entry.table
            ));
        }
    }
    out
}

/// The SHA-1 of `bytes` in hexadecimal capitals, as AzerothCore's updater writes it in `updates`.
pub fn sha1_hex(bytes: &[u8]) -> String {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut message = bytes.to_vec();
    let length = (bytes.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&length.to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (at, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[at] = u32::from_be_bytes(*word);
        }
        for at in 16..80 {
            w[at] = (w[at - 3] ^ w[at - 8] ^ w[at - 14] ^ w[at - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (at, word) in w.iter().enumerate() {
            let (f, k) = match at {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (held, value) in h.iter_mut().zip([a, b, c, d, e]) {
            *held = held.wrapping_add(value);
        }
    }
    h.iter().map(|word| format!("{word:08X}")).collect()
}

/// The row of `updates` telling AzerothCore's updater that the script `name` of `content`, of a
/// module, is applied already: it never plays it again, the rows changed in the game since kept.
pub fn applied_script(name: &str, content: &str) -> RowChange {
    RowChange::Put {
        table: "updates".to_owned(),
        key: row([("name", json!(name))]),
        set: row([
            ("hash", json!(sha1_hex(content.as_bytes()))),
            ("state", json!("MODULE")),
            ("speed", json!(0)),
        ]),
        from: None,
    }
}

/// Rows in memory, by table, for the tests.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Memory {
    pub tables: BTreeMap<String, Vec<Row>>,
}

fn holds(row: &Row, key: &Row) -> bool {
    key.iter().all(|(column, value)| row.get(column) == Some(value))
}

impl Store for Memory {
    fn select(&mut self, table: &str, key: &Row) -> Result<Vec<Row>, String> {
        Ok(self
            .tables
            .get(table)
            .map(|rows| rows.iter().filter(|row| holds(row, key)).cloned().collect())
            .unwrap_or_default())
    }

    fn insert(&mut self, table: &str, row: &Row) -> Result<(), String> {
        self.tables.entry(table.to_owned()).or_default().push(row.clone());
        Ok(())
    }

    fn update(&mut self, table: &str, key: &Row, set: &Row) -> Result<(), String> {
        for row in self.tables.entry(table.to_owned()).or_default() {
            if holds(row, key) {
                row.extend(set.iter().map(|(column, value)| (column.clone(), value.clone())));
            }
        }
        Ok(())
    }

    fn delete(&mut self, table: &str, key: &Row) -> Result<(), String> {
        self.tables
            .entry(table.to_owned())
            .or_default()
            .retain(|row| !holds(row, key));
        Ok(())
    }
}

/// A row of `pairs`.
pub fn row<const N: usize>(pairs: [(&str, Value); N]) -> Row {
    pairs
        .into_iter()
        .map(|(column, value)| (column.to_owned(), value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creature(guid: u64, x: f64) -> Row {
        row([
            ("guid", json!(guid)),
            ("id1", json!(68)),
            ("map", json!(0)),
            ("position_x", json!(x)),
            ("spawntimesecs", json!(300)),
        ])
    }

    fn base() -> Memory {
        let mut memory = Memory::default();
        memory
            .tables
            .insert("creature".to_owned(), vec![creature(1, 10.0), creature(2, 20.0)]);
        memory.tables.insert(
            "game_event_creature".to_owned(),
            vec![
                row([("guid", json!(2)), ("eventEntry", json!(7))]),
                row([("guid", json!(2)), ("eventEntry", json!(8))]),
            ],
        );
        memory
    }

    #[test]
    fn rows_are_put_copied_deleted_and_undone_to_what_they_were() {
        let mut memory = base();
        let changes = vec![
            RowChange::Put {
                table: "creature".to_owned(),
                key: row([("guid", json!(1))]),
                set: row([("position_x", json!(11.5))]),
                from: None,
            },
            // A duplicate: the row of 1 copied, its guid and place its own.
            RowChange::Put {
                table: "creature".to_owned(),
                key: row([("guid", json!(9))]),
                set: row([("position_x", json!(99.0))]),
                from: Some(row([("guid", json!(1))])),
            },
            RowChange::Delete {
                table: "creature".to_owned(),
                key: row([("guid", json!(2))]),
            },
            RowChange::Delete {
                table: "game_event_creature".to_owned(),
                key: row([("guid", json!(2))]),
            },
        ];
        let entries = apply(&mut memory, &changes).unwrap();
        let rows = &memory.tables["creature"];
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["position_x"], json!(11.5));
        assert_eq!(rows[1]["guid"], json!(9));
        assert_eq!(rows[1]["spawntimesecs"], json!(300), "copied from the row named");
        assert_eq!(rows[1]["position_x"], json!(99.0));
        assert!(memory.tables["game_event_creature"].is_empty(), "every row of the key");
        assert_eq!(entries[3].before.len(), 2);

        // Undone: the rows as they were, in another order maybe.
        let undone = revert(&entries);
        apply(&mut memory, &undone).unwrap();
        let sorted = |memory: &Memory, table: &str| {
            let mut rows = memory.tables[table].clone();
            rows.sort_by_key(|row| json!(row).to_string());
            rows
        };
        assert_eq!(sorted(&memory, "creature"), sorted(&base(), "creature"));
        assert_eq!(
            sorted(&memory, "game_event_creature"),
            sorted(&base(), "game_event_creature")
        );

        // The journal kept as JSON reads back the same.
        for entry in &entries {
            assert_eq!(Entry::of_json(&entry.to_json()).as_ref(), Some(entry));
        }
        for change in changes.iter().chain(&undone) {
            assert_eq!(RowChange::of_json(&change.to_json()).as_ref(), Some(change));
        }
    }

    #[test]
    fn a_change_outside_the_tables_of_the_editor_or_without_a_key_is_refused_and_stops_the_others() {
        let mut memory = base();
        let refused = [
            RowChange::Delete {
                table: "account".to_owned(),
                key: row([("id", json!(1))]),
            },
            RowChange::Delete {
                table: "creature".to_owned(),
                key: Row::new(),
            },
            RowChange::Put {
                table: "creature".to_owned(),
                key: row([("guid`; DROP TABLE creature; --", json!(1))]),
                set: Row::new(),
                from: None,
            },
            RowChange::Put {
                table: "creature".to_owned(),
                key: row([("id1", json!(68))]),
                set: row([("map", json!(1))]),
                from: None,
            },
            RowChange::Insert {
                table: "creature".to_owned(),
                key: row([("guid", json!(1))]),
                rows: vec![creature(1, 0.0)],
            },
            RowChange::Insert {
                table: "creature".to_owned(),
                key: row([("guid", json!(5))]),
                rows: vec![creature(6, 0.0)],
            },
        ];
        for change in refused {
            let mut changes = vec![RowChange::Put {
                table: "creature".to_owned(),
                key: row([("guid", json!(2))]),
                set: row([("position_x", json!(0.0))]),
                from: None,
            }];
            changes.push(change.clone());
            let mut tried = memory.clone();
            assert!(apply(&mut tried, &changes).is_err(), "{change:?}");
        }
        assert_eq!(memory, base());
        assert!(apply(&mut memory, &[]).unwrap().is_empty());
    }

    #[test]
    fn a_script_is_told_applied_with_the_hash_azerothcore_gives_it() {
        assert_eq!(sha1_hex(b""), "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709");
        assert_eq!(sha1_hex(b"abc"), "A9993E364706816ABA3E25717850C26C9CD0D89D");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983E441C3BD26EBAAE4AA1F95129E5E54670F1"
        );
        let RowChange::Put { table, key, set, .. } = applied_script("uniwow_2026_10_21_120000.sql", "abc") else {
            panic!("a put")
        };
        assert_eq!(
            (table.as_str(), &key["name"]),
            ("updates", &json!("uniwow_2026_10_21_120000.sql"))
        );
        assert_eq!(set["state"], json!("MODULE"));
    }

    #[test]
    fn the_script_does_again_what_was_done_as_many_times_as_it_runs() {
        let mut memory = base();
        let entries = apply(
            &mut memory,
            &[
                RowChange::Put {
                    table: "creature".to_owned(),
                    key: row([("guid", json!(1))]),
                    set: row([("position_x", json!(1.5))]),
                    from: None,
                },
                RowChange::Put {
                    table: "creature".to_owned(),
                    key: row([("guid", json!(2))]),
                    set: row([("position_x", json!(20.0))]),
                    from: None,
                },
            ],
        )
        .unwrap();
        let script = script("Applied by UniWoW\nsecond line", &entries);
        assert_eq!(
            script,
            "-- Applied by UniWoW\n-- second line\n\
             DELETE FROM `creature` WHERE `guid` = 1;\n\
             INSERT INTO `creature` (`guid`, `id1`, `map`, `position_x`, `spawntimesecs`) VALUES (1, 68, 0, 1.5, 300);\n",
            "the row unchanged left out"
        );
        assert_eq!(
            literal(&json!("Elwynn's \\ \"Forest\"\n")),
            "'Elwynn\\'s \\\\ \"Forest\"\\n'"
        );
        assert_eq!(literal(&Value::Null), "NULL");
        assert_eq!(
            condition(&row([("id", json!(1)), ("point", json!(2))])),
            "`id` = ? AND `point` = ?"
        );
    }
}
