//! The data of the tree views and the table views (section 3): items and rows given as JSON, each
//! with an id, a whole number from 1, which the signals give back.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

/// The most items a tree holds, and the deepest it goes.
pub const MAX_ITEMS: usize = 1_000_000;
pub const MAX_DEPTH: usize = 64;
/// The most rows and columns a table holds.
pub const MAX_ROWS: usize = 1_000_000;
pub const MAX_COLUMNS: usize = 1000;
/// The highest id: every id is a whole number a 64-bit float holds exactly.
pub const MAX_ID: u64 = 1 << 53;

/// An item of a tree view, with its children.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeItem {
    pub id: u64,
    pub text: String,
    /// Whether its children are shown.
    pub expanded: bool,
    pub children: Vec<TreeItem>,
}

/// An id read from JSON: a whole number from 1 to `MAX_ID`.
fn id_of(value: &Value) -> Result<u64, String> {
    value["id"]
        .as_u64()
        .filter(|id| (1..=MAX_ID).contains(id))
        .ok_or_else(|| format!("an id is a whole number from 1 to {MAX_ID}"))
}

/// Reads the items of a tree: `[{"id", "text", "expanded", "children": [...]}]`, the ids each of
/// their own, at most `MAX_ITEMS` items and `MAX_DEPTH` levels.
pub fn items_from_json(value: &Value) -> Result<Vec<TreeItem>, String> {
    fn read(value: &Value, depth: usize, ids: &mut HashSet<u64>) -> Result<Vec<TreeItem>, String> {
        if depth > MAX_DEPTH {
            return Err(format!("a tree goes {MAX_DEPTH} levels deep at most"));
        }
        let Some(list) = value.as_array() else {
            return Err("items come as a list".to_owned());
        };
        list.iter()
            .map(|item| {
                let id = id_of(item)?;
                if !ids.insert(id) {
                    return Err(format!("the id {id} is given twice"));
                }
                if ids.len() > MAX_ITEMS {
                    return Err(format!("a tree holds {MAX_ITEMS} items at most"));
                }
                let children = match &item["children"] {
                    Value::Null => Vec::new(),
                    children => read(children, depth + 1, ids)?,
                };
                Ok(TreeItem {
                    id,
                    text: item["text"].as_str().unwrap_or_default().to_owned(),
                    expanded: item["expanded"].as_bool().unwrap_or(false),
                    children,
                })
            })
            .collect()
    }
    read(value, 1, &mut HashSet::new())
}

pub fn items_to_json(items: &[TreeItem]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| {
                json!({
                    "id": item.id,
                    "text": item.text,
                    "expanded": item.expanded,
                    "children": items_to_json(&item.children),
                })
            })
            .collect(),
    )
}

/// The item `id` among `items` and their children.
pub fn find_item(items: &mut [TreeItem], id: u64) -> Option<&mut TreeItem> {
    for item in items {
        if item.id == id {
            return Some(item);
        }
        if let Some(found) = find_item(&mut item.children, id) {
            return Some(found);
        }
    }
    None
}

/// Whether the item `id` is among `items` and their children.
pub fn has_item(items: &[TreeItem], id: u64) -> bool {
    items.iter().any(|item| item.id == id || has_item(&item.children, id))
}

/// A row of a table view.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: u64,
    pub cells: Vec<String>,
}

/// Reads rows: `[{"id", "cells": ["text", ...]}]`, a cell that is a number taken as its text.
pub fn rows_from_json(value: &Value) -> Result<Vec<Row>, String> {
    let list = value.as_array().ok_or("rows come as a list")?;
    if list.len() > MAX_ROWS {
        return Err(format!("a table holds {MAX_ROWS} rows at most"));
    }
    list.iter()
        .map(|row| {
            let cells = row["cells"].as_array().ok_or("a row has a 'cells' list")?;
            if cells.len() > MAX_COLUMNS {
                return Err(format!("a table has {MAX_COLUMNS} columns at most"));
            }
            Ok(Row {
                id: id_of(row)?,
                cells: cells
                    .iter()
                    .map(|cell| match cell {
                        Value::String(text) => text.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    })
                    .collect(),
            })
        })
        .collect()
}

pub fn rows_to_json(rows: &[Row]) -> Value {
    Value::Array(
        rows.iter()
            .map(|row| json!({ "id": row.id, "cells": row.cells }))
            .collect(),
    )
}

/// Reads the headers of the columns: `["text", ...]`.
pub fn columns_from_json(value: &Value) -> Result<Vec<String>, String> {
    let list = value.as_array().ok_or("columns come as a list of headers")?;
    if list.len() > MAX_COLUMNS {
        return Err(format!("a table has {MAX_COLUMNS} columns at most"));
    }
    Ok(list
        .iter()
        .map(|header| header.as_str().map_or_else(|| header.to_string(), str::to_owned))
        .collect())
}

/// What a cell is sorted by: the numbers first, by value, then the texts, ignoring their case. An
/// order over all cells, as sorting requires: numbers and texts compared each their way would not
/// be one ("9" < "10" < "1a" < "9").
enum SortKey {
    Number(f64),
    Text(String),
}

impl SortKey {
    fn of(cell: &str) -> Self {
        match cell.trim().parse::<f64>() {
            Ok(number) if number.is_finite() => Self::Number(number),
            _ => Self::Text(cell.to_lowercase()),
        }
    }

    fn compare(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.total_cmp(b),
            (Self::Number(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Number(_)) => Ordering::Greater,
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
        }
    }
}

/// The data of a table view: its columns, its rows in the module's order, and the order they are
/// shown in, sorted by a column. The kernel sorts; the module and the signals name rows by id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    columns: Vec<String>,
    rows: Vec<Row>,
    /// The column sorted by, and whether from the highest.
    sort: Option<(usize, bool)>,
    /// The places in `rows` of the rows shown, in order.
    order: Vec<usize>,
    /// The place in `rows` of each id.
    index: HashMap<u64, usize>,
}

impl Table {
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The rows in the module's order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn sort(&self) -> Option<(usize, bool)> {
        self.sort
    }

    /// The row shown at `position`, in the order shown.
    pub fn shown(&self, position: usize) -> Option<&Row> {
        self.rows.get(*self.order.get(position)?)
    }

    pub fn row(&self, id: u64) -> Option<&Row> {
        self.rows.get(*self.index.get(&id)?)
    }

    pub fn set_columns(&mut self, columns: Vec<String>) {
        self.columns = columns;
        if self.sort.is_some_and(|(column, _)| column >= self.columns.len()) {
            self.sort = None;
        }
        self.reorder();
    }

    /// The rows, each id of its own.
    pub fn set_rows(&mut self, rows: Vec<Row>) -> Result<(), String> {
        check_ids(rows.iter().map(|row| row.id), &HashMap::new())?;
        self.rows = rows;
        self.reorder();
        Ok(())
    }

    pub fn set_cell(&mut self, row: u64, column: usize, text: &str) -> Result<(), String> {
        if column >= MAX_COLUMNS {
            return Err(format!("a table has {MAX_COLUMNS} columns at most"));
        }
        let place = *self.index.get(&row).ok_or_else(|| format!("no row {row}"))?;
        let cells = &mut self.rows[place].cells;
        if cells.len() <= column {
            cells.resize(column + 1, String::new());
        }
        text.clone_into(&mut cells[column]);
        if self.sort.is_some_and(|(sorted, _)| sorted == column) {
            self.reorder();
        }
        Ok(())
    }

    /// Inserts `rows` at `at` in the module's order, or at its end; their ids are new.
    pub fn insert_rows(&mut self, at: usize, rows: Vec<Row>) -> Result<(), String> {
        if self.rows.len() + rows.len() > MAX_ROWS {
            return Err(format!("a table holds {MAX_ROWS} rows at most"));
        }
        check_ids(rows.iter().map(|row| row.id), &self.index)?;
        let at = at.min(self.rows.len());
        self.rows.splice(at..at, rows);
        self.reorder();
        Ok(())
    }

    /// Removes the rows of these ids; returns how many there were.
    pub fn remove_rows(&mut self, ids: &[u64]) -> usize {
        let gone: HashSet<u64> = ids.iter().copied().collect();
        let before = self.rows.len();
        self.rows.retain(|row| !gone.contains(&row.id));
        self.reorder();
        before - self.rows.len()
    }

    /// Sorted by a column, from the highest when `descending`, or in the module's order.
    pub fn set_sort(&mut self, sort: Option<(usize, bool)>) {
        self.sort = sort.filter(|(column, _)| *column < self.columns.len());
        self.reorder();
    }

    fn reorder(&mut self) {
        self.index = self
            .rows
            .iter()
            .enumerate()
            .map(|(place, row)| (row.id, place))
            .collect();
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        if let Some((column, descending)) = self.sort {
            let keys: Vec<SortKey> = self
                .rows
                .iter()
                .map(|row| SortKey::of(row.cells.get(column).map_or("", String::as_str)))
                .collect();
            // Stable: rows of equal cells keep the module's order.
            order.sort_by(|a, b| {
                let ordering = keys[*a].compare(&keys[*b]);
                if descending { ordering.reverse() } else { ordering }
            });
        }
        self.order = order;
    }
}

/// Whether `ids` are each of their own and none is among `taken`.
fn check_ids(ids: impl Iterator<Item = u64>, taken: &HashMap<u64, usize>) -> Result<(), String> {
    let mut seen = HashSet::new();
    for id in ids {
        if !(1..=MAX_ID).contains(&id) {
            return Err(format!("an id is a whole number from 1 to {MAX_ID}"));
        }
        if taken.contains_key(&id) || !seen.insert(id) {
            return Err(format!("the id {id} is given twice"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Row, Table, find_item, items_from_json, items_to_json, rows_from_json};

    fn rows(cells: &[(u64, &str)]) -> Vec<Row> {
        cells
            .iter()
            .map(|(id, cell)| Row {
                id: *id,
                cells: vec![(*cell).to_owned()],
            })
            .collect()
    }

    fn shown(table: &Table) -> Vec<u64> {
        (0..table.len())
            .map(|position| table.shown(position).unwrap().id)
            .collect()
    }

    #[test]
    fn a_tree_crosses_its_json_and_wrong_ones_are_refused() {
        let value = json!([{ "id": 1, "text": "a", "expanded": true, "children": [{ "id": 2, "text": "b" }] }]);
        let mut items = items_from_json(&value).unwrap();
        assert_eq!(items_from_json(&items_to_json(&items)).unwrap(), items);
        find_item(&mut items, 2).unwrap().expanded = true;
        assert!(items[0].children[0].expanded);
        assert!(
            items_from_json(&json!([{ "id": 1 }, { "id": 1 }])).is_err(),
            "an id twice"
        );
        assert!(items_from_json(&json!([{ "id": 0 }])).is_err(), "ids from 1");
        let mut deep = json!([]);
        for id in 1..=70 {
            deep = json!([{ "id": id, "children": deep }]);
        }
        assert!(items_from_json(&deep).is_err(), "too deep");
    }

    #[test]
    fn a_table_sorts_by_number_or_text_and_keeps_the_ids() {
        let mut table = Table::default();
        table.set_columns(vec!["value".to_owned()]);
        table
            .set_rows(rows(&[(1, "10"), (2, "9"), (3, "b"), (4, "A")]))
            .unwrap();
        assert_eq!(shown(&table), vec![1, 2, 3, 4], "the module's order");
        table.set_sort(Some((0, false)));
        assert_eq!(shown(&table), vec![2, 1, 4, 3], "9 before 10, then A before b");
        table.set_sort(Some((0, true)));
        assert_eq!(shown(&table), vec![3, 4, 1, 2]);
        table.set_cell(2, 0, "100").unwrap();
        assert_eq!(shown(&table), vec![3, 4, 2, 1], "sorted again");
        assert_eq!(table.row(2).unwrap().cells[0], "100");
    }

    #[test]
    fn rows_inserted_and_removed_keep_the_order_shown() {
        let mut table = Table::default();
        table.set_columns(vec!["value".to_owned()]);
        table.set_rows(rows(&[(1, "1"), (2, "3")])).unwrap();
        table.set_sort(Some((0, false)));
        table.insert_rows(0, rows(&[(3, "2")])).unwrap();
        assert_eq!(shown(&table), vec![1, 3, 2]);
        assert!(table.insert_rows(0, rows(&[(1, "x")])).is_err(), "an id taken");
        assert_eq!(table.remove_rows(&[3, 9]), 1);
        assert_eq!(shown(&table), vec![1, 2]);
        assert!(table.set_rows(rows(&[(5, "a"), (5, "b")])).is_err());
        assert!(rows_from_json(&json!([{ "id": 1, "cells": [1.5, null, "x"] }])).unwrap()[0].cells == ["1.5", "", "x"]);
    }

    #[test]
    fn numbers_sort_before_texts_so_that_any_cells_sort() {
        let mut table = Table::default();
        table.set_columns(vec!["value".to_owned()]);
        table.set_rows(rows(&[(1, "1a"), (2, "10"), (3, "9")])).unwrap();
        table.set_sort(Some((0, false)));
        assert_eq!(shown(&table), vec![3, 2, 1], "9 < 10 < 1a, though 1a < 9 as texts");
        let cells: Vec<String> = (0..2000u64)
            .map(|i| {
                if i % 3 == 0 {
                    format!("{}a", i % 17)
                } else {
                    (i * 7919 % 1000).to_string()
                }
            })
            .collect();
        let many: Vec<Row> = cells
            .iter()
            .enumerate()
            .map(|(place, cell)| Row {
                id: place as u64 + 1,
                cells: vec![cell.clone()],
            })
            .collect();
        table.set_rows(many).unwrap();
        let sorted: Vec<&str> = (0..table.len())
            .map(|position| table.shown(position).unwrap().cells[0].as_str())
            .collect();
        let numbers = sorted.iter().take_while(|cell| cell.parse::<f64>().is_ok()).count();
        assert_eq!(
            numbers,
            cells.iter().filter(|cell| cell.parse::<f64>().is_ok()).count(),
            "numbers first"
        );
        let values: Vec<f64> = sorted[..numbers].iter().map(|cell| cell.parse().unwrap()).collect();
        assert!(values.is_sorted(), "by value");
        assert!(sorted[numbers..].is_sorted(), "then the texts");
    }
}
