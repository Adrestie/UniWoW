//! The data of the tree views and the table views (section 3): items and rows given as JSON, each
//! with an id, a whole number from 1, which the signals give back.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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

pub fn rows_to_json<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Value {
    Value::Array(
        rows.into_iter()
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
/// be one ("9" < "10" < "1a" < "9"). `compare_cells` gives the same order without making keys.
enum SortKey {
    Number(f64),
    Text(String),
}

/// The number a cell holds, if it holds one.
fn number_of(cell: &str) -> Option<f64> {
    cell.trim().parse::<f64>().ok().filter(|number| number.is_finite())
}

/// A text without its case, character by character.
fn folded(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().flat_map(char::to_lowercase)
}

impl SortKey {
    fn of(cell: &str) -> Self {
        match number_of(cell) {
            Some(number) => Self::Number(number),
            None => Self::Text(folded(cell).collect()),
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

/// Two cells in the order of their keys, without making them.
fn compare_cells(a: &str, b: &str) -> Ordering {
    match (number_of(a), number_of(b)) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => folded(a).cmp(folded(b)),
    }
}

fn cell_of(row: &Row, column: usize) -> &str {
    row.cells.get(column).map_or("", String::as_str)
}

/// From this many rows, a table is sorted whole off the lock of its objects and off the
/// interface's thread, its former order shown meanwhile; below, at once.
pub const BACKGROUND_SORT_ROWS: usize = 50_000;
/// The step between the ranks of rows ranked together.
const RANK_STEP: u64 = 1 << 32;
/// From this many rows inserted at once in a sorted table, they are sorted and merged into the
/// order shown rather than placed one by one.
const MERGED_ROWS: usize = 16;

/// The rows of a table, each at a place of its own; a place freed holds none.
type Slots = Arc<Vec<Option<Arc<Row>>>>;

#[cfg(test)]
thread_local! {
    /// How many times every row of a table was sorted, on this thread.
    static FULL_SORTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The places of the rows of `slots` in the order of `sort`, rows of equal cells by rank.
fn sorted_places(slots: &[Option<Arc<Row>>], ranks: &[u64], (column, descending): (usize, bool)) -> Vec<usize> {
    #[cfg(test)]
    FULL_SORTS.with(|count| count.set(count.get() + 1));
    let mut keyed: Vec<(SortKey, u64, usize)> = slots
        .iter()
        .enumerate()
        .filter_map(|(place, row)| {
            let row = row.as_ref()?;
            Some((SortKey::of(cell_of(row, column)), ranks[place], place))
        })
        .collect();
    keyed.sort_unstable_by(|a, b| {
        let ordering = a.0.compare(&b.0);
        let ordering = if descending { ordering.reverse() } else { ordering };
        ordering.then(a.1.cmp(&b.1))
    });
    keyed.into_iter().map(|(_, _, place)| place).collect()
}

/// Checks an id is a whole number from 1 to `MAX_ID`.
fn check_id(id: u64) -> Result<(), String> {
    if (1..=MAX_ID).contains(&id) {
        Ok(())
    } else {
        Err(format!("an id is a whole number from 1 to {MAX_ID}"))
    }
}

/// Rows ready to be set in a table, their places, ranks and index made beforehand, so that setting
/// them takes no time under the lock of the objects. A table gives back those it replaces, to be
/// freed after the lock.
#[derive(Default)]
pub struct Rows {
    slots: Slots,
    module: Vec<usize>,
    ranks: Arc<Vec<u64>>,
    index: HashMap<u64, usize>,
}

impl Rows {
    /// The rows, each id of its own.
    pub fn new(rows: Vec<Row>) -> Result<Self, String> {
        if rows.len() > MAX_ROWS {
            return Err(format!("a table holds {MAX_ROWS} rows at most"));
        }
        let mut index = HashMap::with_capacity(rows.len());
        for (place, row) in rows.iter().enumerate() {
            check_id(row.id)?;
            if index.insert(row.id, place).is_some() {
                return Err(format!("the id {} is given twice", row.id));
            }
        }
        Ok(Self {
            module: (0..rows.len()).collect(),
            ranks: Arc::new((1..=rows.len() as u64).map(|rank| rank * RANK_STEP).collect()),
            slots: Arc::new(rows.into_iter().map(|row| Some(Arc::new(row))).collect()),
            index,
        })
    }
}

/// The rows of a table in the module's order as they were, to read off the lock.
pub struct RowsInOrder {
    slots: Slots,
    module: Vec<usize>,
}

impl RowsInOrder {
    pub fn iter(&self) -> impl Iterator<Item = &Row> {
        self.module.iter().filter_map(|&place| self.slots[place].as_deref())
    }
}

/// A sort of a table to make off the lock: the rows as they were when it was asked, and what it
/// sorts by.
pub struct SortJob {
    slots: Slots,
    ranks: Arc<Vec<u64>>,
    sort: (usize, bool),
    version: u64,
}

impl SortJob {
    /// The places of the rows in the order asked.
    pub fn run(&self) -> Vec<usize> {
        sorted_places(&self.slots, &self.ranks, self.sort)
    }
}

/// The data of a table view: its columns, its rows in the module's order, and the order they are
/// shown in, sorted by a column. The kernel sorts; the module and the signals name rows by id. A
/// change of rows is made where it falls: no index made again, no sort of every row.
#[derive(Clone, Debug, Default)]
pub struct Table {
    columns: Vec<String>,
    slots: Slots,
    /// The places freed, taken again by rows inserted.
    free: Vec<usize>,
    /// The places of the rows in the module's order.
    module: Vec<usize>,
    /// The rank of each place: increasing along the module's order with room between, so that a
    /// row inserted takes one between its neighbours and rows of equal cells sort in that order.
    ranks: Arc<Vec<u64>>,
    /// The place of each id.
    index: HashMap<u64, usize>,
    /// The sort asked for: the column and whether from the highest.
    sort: Option<(usize, bool)>,
    /// The sort of the order shown, `sort` once made, and the places in that order.
    shown: Option<(usize, bool)>,
    order: Vec<usize>,
    /// Whether a sort is being made off the lock.
    sorting: bool,
    /// Changed when the rows a sort reads change.
    version: u64,
}

impl Table {
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The rows in the module's order.
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.module.iter().filter_map(|&place| self.slots[place].as_deref())
    }

    /// The rows in the module's order as they are now, to read off the lock.
    pub fn rows_in_order(&self) -> RowsInOrder {
        RowsInOrder {
            slots: self.slots.clone(),
            module: self.module.clone(),
        }
    }

    pub fn len(&self) -> usize {
        self.module.len()
    }

    pub fn is_empty(&self) -> bool {
        self.module.is_empty()
    }

    /// The sort asked for.
    pub fn sort(&self) -> Option<(usize, bool)> {
        self.sort
    }

    /// The sort of the order shown: the former one while the sort asked for is being made.
    pub fn shown_sort(&self) -> Option<(usize, bool)> {
        self.shown
    }

    pub fn is_sorting(&self) -> bool {
        self.sort != self.shown
    }

    /// The row shown at `position`, in the order shown.
    pub fn shown(&self, position: usize) -> Option<&Row> {
        let places = if self.shown.is_some() {
            &self.order
        } else {
            &self.module
        };
        self.slots.get(*places.get(position)?)?.as_deref()
    }

    pub fn row(&self, id: u64) -> Option<&Row> {
        self.slots.get(*self.index.get(&id)?)?.as_deref()
    }

    /// The headers; a sort by a column gone ends.
    pub fn set_columns(&mut self, columns: Vec<String>) {
        self.columns = columns;
        self.set_sort(self.sort);
    }

    /// Replaces the rows, and gives back those replaced, to be freed after the lock.
    pub fn set_rows(&mut self, rows: Rows) -> Rows {
        let replaced = Rows {
            slots: std::mem::replace(&mut self.slots, rows.slots),
            module: std::mem::replace(&mut self.module, rows.module),
            ranks: std::mem::replace(&mut self.ranks, rows.ranks),
            index: std::mem::replace(&mut self.index, rows.index),
        };
        self.free.clear();
        self.version += 1;
        self.shown = None;
        self.order = Vec::new();
        if let Some(sort) = self.sort
            && self.len() < BACKGROUND_SORT_ROWS
        {
            self.order = sorted_places(&self.slots, &self.ranks, sort);
            self.shown = Some(sort);
        }
        replaced
    }

    pub fn set_cell(&mut self, row: u64, column: usize, text: &str) -> Result<(), String> {
        if column >= MAX_COLUMNS {
            return Err(format!("a table has {MAX_COLUMNS} columns at most"));
        }
        let place = *self.index.get(&row).ok_or_else(|| format!("no row {row}"))?;
        // In a table shown sorted by this column, the row leaves the order and comes back where its
        // new cell puts it.
        let moved = self
            .shown
            .filter(|(sorted, _)| *sorted == column)
            .map(|sort| (sort, self.position(place, sort)));
        let slot = Arc::make_mut(&mut self.slots)[place]
            .as_mut()
            .ok_or_else(|| format!("no row {row}"))?;
        let cells = &mut Arc::make_mut(slot).cells;
        if cells.len() <= column {
            cells.resize(column + 1, String::new());
        }
        text.clone_into(&mut cells[column]);
        if let Some((sort, from)) = moved {
            self.order.remove(from);
            let to = self.position(place, sort);
            self.order.insert(to, place);
        }
        if self.sort.is_some_and(|(sorted, _)| sorted == column) {
            self.version += 1;
        }
        Ok(())
    }

    /// Inserts `rows` at `at` in the module's order, or at its end; their ids are new.
    pub fn insert_rows(&mut self, at: usize, rows: Vec<Row>) -> Result<(), String> {
        if self.len() + rows.len() > MAX_ROWS {
            return Err(format!("a table holds {MAX_ROWS} rows at most"));
        }
        let mut seen = HashSet::new();
        for row in &rows {
            check_id(row.id)?;
            if self.index.contains_key(&row.id) || !seen.insert(row.id) {
                return Err(format!("the id {} is given twice", row.id));
            }
        }
        if rows.is_empty() {
            return Ok(());
        }
        let at = at.min(self.len());
        let slots = Arc::make_mut(&mut self.slots);
        let mut placed = Vec::with_capacity(rows.len());
        for row in rows {
            let id = row.id;
            let place = match self.free.pop() {
                Some(place) => {
                    slots[place] = Some(Arc::new(row));
                    place
                }
                None => {
                    slots.push(Some(Arc::new(row)));
                    slots.len() - 1
                }
            };
            self.index.insert(id, place);
            placed.push(place);
        }
        let places = slots.len();
        Arc::make_mut(&mut self.ranks).resize(places, 0);
        self.module.splice(at..at, placed.iter().copied());
        self.rank(at, placed.len());
        if let Some(sort) = self.shown {
            self.place_shown(&placed, sort);
        }
        self.version += 1;
        Ok(())
    }

    /// Removes the rows of these ids, in one pass over the rows; returns how many there were.
    pub fn remove_rows(&mut self, ids: &[u64]) -> usize {
        let places: Vec<usize> = ids.iter().filter_map(|id| self.index.remove(id)).collect();
        if places.is_empty() {
            return 0;
        }
        let slots = Arc::make_mut(&mut self.slots);
        for &place in &places {
            slots[place] = None;
        }
        self.module.retain(|&place| slots[place].is_some());
        self.order.retain(|&place| slots[place].is_some());
        self.free.extend_from_slice(&places);
        self.version += 1;
        places.len()
    }

    /// Sorted by a column, from the highest when `descending`, or in the module's order. A table of
    /// `BACKGROUND_SORT_ROWS` rows or more keeps showing its former order until the sort is made off
    /// the lock: `sort_job`, then `finish_sort`.
    pub fn set_sort(&mut self, sort: Option<(usize, bool)>) {
        self.sort = sort.filter(|(column, _)| *column < self.columns.len());
        if self.sort == self.shown {
            return;
        }
        match self.sort {
            None => {
                self.shown = None;
                self.order = Vec::new();
            }
            Some(sort) if self.len() < BACKGROUND_SORT_ROWS => {
                self.order = sorted_places(&self.slots, &self.ranks, sort);
                self.shown = Some(sort);
            }
            Some(_) => {}
        }
    }

    /// The sort asked for, to make off the lock, when it is neither made nor being made.
    pub fn sort_job(&mut self) -> Option<SortJob> {
        let sort = self.sort?;
        if self.shown == Some(sort) || self.sorting {
            return None;
        }
        self.sorting = true;
        Some(SortJob {
            slots: self.slots.clone(),
            ranks: self.ranks.clone(),
            sort,
            version: self.version,
        })
    }

    /// Takes the order a sort job made: shown when its sort is still the one asked for and the rows
    /// it read have not changed meanwhile, otherwise dropped, a new job then to be made. Returns
    /// whether it is shown.
    pub fn finish_sort(&mut self, job: &SortJob, order: Vec<usize>) -> bool {
        self.sorting = false;
        if self.sort != Some(job.sort) || self.version != job.version {
            return false;
        }
        self.order = order;
        self.shown = self.sort;
        true
    }

    /// Gives up the sort being made, after its job failed: the order shown stays, and is the sort.
    pub fn abandon_sort(&mut self) {
        self.sorting = false;
        self.sort = self.shown;
    }

    /// Where the row at `place` goes in the order shown, sorted by `sort`.
    fn position(&self, place: usize, sort: (usize, bool)) -> usize {
        self.order
            .partition_point(|&other| self.compare_places(other, place, sort) == Ordering::Less)
    }

    /// The rows at two places in the order of `sort`, rows of equal cells by rank.
    fn compare_places(&self, a: usize, b: usize, (column, descending): (usize, bool)) -> Ordering {
        let cell = |place: usize| self.slots[place].as_deref().map_or("", |row| cell_of(row, column));
        let ordering = compare_cells(cell(a), cell(b));
        let ordering = if descending { ordering.reverse() } else { ordering };
        ordering.then(self.ranks[a].cmp(&self.ranks[b]))
    }

    /// Ranks the `count` rows inserted at `at` in the module's order between their neighbours, or
    /// ranks every row again when there is no room left between them.
    fn rank(&mut self, at: usize, count: usize) {
        let ranks = Arc::make_mut(&mut self.ranks);
        let low = if at == 0 { 0 } else { ranks[self.module[at - 1]] };
        let between = count as u64 + 1;
        let step = match self.module.get(at + count) {
            Some(&next) => (ranks[next] - low) / between,
            None => low.checked_add(RANK_STEP * between).map_or(0, |_| RANK_STEP),
        };
        if step > 0 {
            for (offset, &place) in self.module[at..at + count].iter().enumerate() {
                ranks[place] = low + step * (offset as u64 + 1);
            }
        } else {
            for (position, &place) in self.module.iter().enumerate() {
                ranks[place] = (position as u64 + 1) * RANK_STEP;
            }
        }
    }

    /// Places the rows inserted at `placed` in the order shown, sorted by `sort`: one by one where
    /// each goes, or, many, sorted and merged in one pass.
    fn place_shown(&mut self, placed: &[usize], sort: (usize, bool)) {
        if placed.len() < MERGED_ROWS {
            for &place in placed {
                let at = self.position(place, sort);
                self.order.insert(at, place);
            }
            return;
        }
        let mut inserted = placed.to_vec();
        inserted.sort_unstable_by(|&a, &b| self.compare_places(a, b, sort));
        let shown = std::mem::take(&mut self.order);
        let mut merged = Vec::with_capacity(shown.len() + inserted.len());
        let (mut old, mut new) = (shown.iter().peekable(), inserted.iter().peekable());
        while let (Some(&&a), Some(&&b)) = (old.peek(), new.peek()) {
            if self.compare_places(b, a, sort) == Ordering::Less {
                merged.push(b);
                new.next();
            } else {
                merged.push(a);
                old.next();
            }
        }
        merged.extend(old);
        merged.extend(new);
        self.order = merged;
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::{
        BACKGROUND_SORT_ROWS, FULL_SORTS, Row, Rows, SortKey, Table, cell_of, compare_cells, find_item,
        items_from_json, items_to_json, rows_from_json,
    };

    fn rows(cells: &[(u64, &str)]) -> Vec<Row> {
        cells
            .iter()
            .map(|(id, cell)| Row {
                id: *id,
                cells: vec![(*cell).to_owned()],
            })
            .collect()
    }

    fn table_of(cells: &[(u64, &str)]) -> Table {
        let mut table = Table::default();
        table.set_columns(vec!["value".to_owned()]);
        table.set_rows(Rows::new(rows(cells)).unwrap());
        table
    }

    fn shown(table: &Table) -> Vec<u64> {
        (0..table.len())
            .map(|position| table.shown(position).unwrap().id)
            .collect()
    }

    /// The ids in the order a whole sort gives: by cell, rows of equal cells in the module's order.
    fn sorted_ids(table: &Table, (column, descending): (usize, bool)) -> Vec<u64> {
        let mut rows: Vec<&Row> = table.rows().collect();
        rows.sort_by(|a, b| {
            let ordering = compare_cells(cell_of(a, column), cell_of(b, column));
            if descending { ordering.reverse() } else { ordering }
        });
        rows.iter().map(|row| row.id).collect()
    }

    /// The order shown is the one a whole sort gives, and every id finds its row.
    fn check(table: &Table) {
        match table.shown_sort() {
            Some(sort) => assert_eq!(shown(table), sorted_ids(table, sort), "sorted by {sort:?}"),
            None => assert_eq!(shown(table), table.rows().map(|row| row.id).collect::<Vec<_>>()),
        }
        for row in table.rows() {
            assert_eq!(table.row(row.id).map(|found| found.id), Some(row.id));
        }
    }

    /// Numbers from a seed, the same each run.
    struct Random(u64);

    impl Random {
        fn below(&mut self, bound: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % bound
        }

        /// A cell among numbers, texts differing by their case only, and the empty one.
        fn cell(&mut self) -> String {
            match self.below(4) {
                0 => self.below(40).to_string(),
                1 => ["a", "B", "b", "Été", "été", "ß", "1a"][self.below(7) as usize].to_owned(),
                2 => format!("{}.5", self.below(9)),
                _ => String::new(),
            }
        }
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
        let mut table = table_of(&[(1, "10"), (2, "9"), (3, "b"), (4, "A")]);
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
        let mut table = table_of(&[(1, "1"), (2, "3")]);
        table.set_sort(Some((0, false)));
        table.insert_rows(0, rows(&[(3, "2")])).unwrap();
        assert_eq!(shown(&table), vec![1, 3, 2]);
        assert!(table.insert_rows(0, rows(&[(1, "x")])).is_err(), "an id taken");
        assert_eq!(table.remove_rows(&[3, 9, 3]), 1);
        assert_eq!(shown(&table), vec![1, 2]);
        assert!(Rows::new(rows(&[(5, "a"), (5, "b")])).is_err());
        assert!(rows_from_json(&json!([{ "id": 1, "cells": [1.5, null, "x"] }])).unwrap()[0].cells == ["1.5", "", "x"]);
    }

    #[test]
    fn numbers_sort_before_texts_so_that_any_cells_sort() {
        let mut table = table_of(&[(1, "1a"), (2, "10"), (3, "9")]);
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
        table.set_rows(Rows::new(many).unwrap());
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
        assert!(
            sorted[numbers..].is_sorted_by_key(|cell| cell.to_lowercase()),
            "then the texts"
        );
    }

    #[test]
    fn cells_compared_one_by_one_sort_as_their_keys() {
        let cells = [
            "", "1", "1.5", "-2", "1a", "a", "A", "b", "Été", "été", "ß", "SS", " 3 ", "NaN", "inf",
        ];
        for a in cells {
            for b in cells {
                assert_eq!(
                    compare_cells(a, b),
                    SortKey::of(a).compare(&SortKey::of(b)),
                    "{a:?} and {b:?}"
                );
            }
        }
    }

    #[test]
    fn changes_made_where_they_fall_give_the_order_of_a_whole_sort() {
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        let mut table = Table::default();
        table.set_columns(vec!["a".to_owned(), "b".to_owned()]);
        let mut next = 1;
        let mut new_rows = |random: &mut Random, count: u64| -> Vec<Row> {
            (0..count)
                .map(|_| {
                    next += 1;
                    Row {
                        id: next,
                        cells: vec![random.cell(), random.cell()],
                    }
                })
                .collect()
        };
        let first = new_rows(&mut random, 300);
        table.set_rows(Rows::new(first).unwrap());
        table.set_sort(Some((0, false)));
        for _ in 0..600 {
            let ids: Vec<u64> = table.rows().map(|row| row.id).collect();
            let any = |random: &mut Random| ids.get(random.below(ids.len().max(1) as u64) as usize).copied();
            match random.below(7) {
                0 => {
                    let at = random.below(ids.len() as u64 + 2) as usize;
                    let count = if random.below(2) == 0 { 1 } else { 20 };
                    table.insert_rows(at, new_rows(&mut random, count)).unwrap();
                }
                1 => {
                    let gone: Vec<u64> = (0..3).filter_map(|_| any(&mut random)).chain([u64::MAX - 1]).collect();
                    table.remove_rows(&gone);
                }
                2 | 3 => {
                    if let Some(id) = any(&mut random) {
                        let column = table.shown_sort().map_or(0, |(column, _)| column);
                        table.set_cell(id, column, &random.cell()).unwrap();
                    }
                }
                4 => {
                    if let Some(id) = any(&mut random) {
                        table.set_cell(id, 1, &random.cell()).unwrap();
                    }
                }
                5 => {
                    let sorts = [
                        Some((0, false)),
                        Some((0, true)),
                        Some((1, false)),
                        Some((1, true)),
                        None,
                    ];
                    table.set_sort(sorts[random.below(5) as usize]);
                }
                _ => {
                    // At the same place again and again, until the rows are ranked again.
                    for _ in 0..40 {
                        table.insert_rows(0, new_rows(&mut random, 1)).unwrap();
                    }
                }
            }
            check(&table);
        }
    }

    #[test]
    fn a_thousand_rows_inserted_one_by_one_in_a_sorted_table_sort_nothing_whole() {
        let many: Vec<Row> = (1..=100_000u64)
            .map(|id| Row {
                id,
                cells: vec![(id * 7919 % 1000).to_string()],
            })
            .collect();
        let mut table = Table::default();
        table.set_columns(vec!["value".to_owned()]);
        table.set_rows(Rows::new(many).unwrap());
        table.set_sort(Some((0, false)));
        let job = table.sort_job().unwrap();
        assert!(table.finish_sort(&job, job.run()));
        let sorts = FULL_SORTS.with(std::cell::Cell::get);
        let mut random = Random(7);
        let started = Instant::now();
        for id in 100_001..=101_000u64 {
            let at = random.below(table.len() as u64) as usize;
            let row = Row {
                id,
                cells: vec![random.below(1000).to_string()],
            };
            table.insert_rows(at, vec![row]).unwrap();
        }
        let took = started.elapsed();
        assert_eq!(
            FULL_SORTS.with(std::cell::Cell::get),
            sorts,
            "no insertion sorted every row"
        );
        assert!(took < Duration::from_secs(1), "{took:?} for a thousand rows");
        check(&table);
    }

    #[test]
    fn a_large_table_is_sorted_by_a_job_made_again_when_its_rows_change_meanwhile() {
        let count = BACKGROUND_SORT_ROWS as u64;
        let many: Vec<Row> = (1..=count)
            .map(|id| Row {
                id,
                cells: vec![(count - id).to_string()],
            })
            .collect();
        let mut table = Table::default();
        table.set_columns(vec!["value".to_owned()]);
        table.set_rows(Rows::new(many).unwrap());
        table.set_sort(Some((0, false)));
        assert!(
            table.is_sorting() && table.shown_sort().is_none(),
            "the former order shown"
        );
        assert_eq!(table.shown(0).unwrap().id, 1);
        let job = table.sort_job().unwrap();
        assert!(table.sort_job().is_none(), "one job at a time");
        table.insert_rows(0, rows(&[(count + 1, "5")])).unwrap();
        assert!(!table.finish_sort(&job, job.run()), "its rows changed meanwhile");
        let job = table.sort_job().unwrap();
        table.set_sort(Some((0, true)));
        assert!(!table.finish_sort(&job, job.run()), "another sort asked meanwhile");
        let job = table.sort_job().unwrap();
        assert!(table.finish_sort(&job, job.run()));
        assert_eq!(table.shown_sort(), Some((0, true)));
        assert!(!table.is_sorting() && table.sort_job().is_none());
        check(&table);
        table.set_sort(None);
        assert_eq!(table.shown(0).unwrap().id, count + 1, "the module's order at once");
    }
}
