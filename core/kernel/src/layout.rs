//! Dock layout: the default arrangement, and the restoration of a saved one when modules have
//! come or gone since.

use std::collections::{BTreeSet, HashSet};

use uniwow_api::egui_dock::{DockState, Node, NodeIndex, SurfaceIndex, TabPath, Tree};
use uniwow_api::serde::{Deserialize, Serialize};
use uniwow_api::{DockArea as Area, serde_json};

/// A dock tab: one panel of one module, or of the kernel.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(crate = "uniwow_api::serde")]
pub struct Tab {
    pub module: String,
    pub panel: String,
}

impl Tab {
    pub fn new(module: &str, panel: &str) -> Self {
        Self {
            module: module.to_owned(),
            panel: panel.to_owned(),
        }
    }

    /// `module/panel`, as stored in the settings.
    pub fn key(&self) -> String {
        format!("{}/{}", self.module, self.panel)
    }
}

pub struct PanelEntry {
    pub tab: Tab,
    pub title: String,
    pub area: Area,
    pub open_by_default: bool,
}

/// The saved layout without the panels of absent modules, plus the panels of modules that came
/// back, unless the user closed them.
pub fn restore(saved: Option<serde_json::Value>, entries: &[PanelEntry], closed: &BTreeSet<String>) -> DockState<Tab> {
    let saved = saved.and_then(|v| serde_json::from_value::<DockState<Tab>>(v).ok());
    let Some(mut dock) = saved else {
        return default_layout(entries, closed);
    };
    let known: HashSet<Tab> = entries.iter().map(|e| e.tab.clone()).collect();
    // One tab at a time, as when the user closes them: removing many at once with
    // `retain_tabs` can leave the tree broken.
    let absent: Vec<Tab> = dock
        .iter_all_tabs()
        .map(|(_, tab)| tab.clone())
        .filter(|tab| !known.contains(tab))
        .collect();
    for tab in absent {
        if let Some(path) = dock.find_tab(&tab) {
            dock.remove_tab(path);
        }
    }
    if dock.main_surface().num_tabs() == 0 || !is_consistent(dock.main_surface()) {
        return default_layout(entries, closed);
    }
    let returning: Vec<&PanelEntry> = entries
        .iter()
        .filter(|e| e.open_by_default && !closed.contains(&e.tab.key()) && dock.find_tab(&e.tab).is_none())
        .collect();
    // A whole area came back: the saved arrangement no longer fits, start from the default one.
    if returning
        .iter()
        .any(|e| area_leaf(&dock, entries, e.area, &e.tab).is_none())
    {
        return default_layout(entries, closed);
    }
    for entry in returning {
        place(&mut dock, entries, entry);
    }
    dock
}

pub fn default_layout(entries: &[PanelEntry], closed: &BTreeSet<String>) -> DockState<Tab> {
    let tabs_in = |area: Area| -> Vec<Tab> {
        entries
            .iter()
            .filter(|e| e.area == area && e.open_by_default && !closed.contains(&e.tab.key()))
            .map(|e| e.tab.clone())
            .collect()
    };
    let mut groups = [Area::Center, Area::Right, Area::Bottom, Area::Left].map(|area| (area, tabs_in(area)));
    // The first non-empty group fills the window; the others are split off around it.
    let Some(first) = groups.iter().position(|(_, tabs)| !tabs.is_empty()) else {
        return DockState::new(Vec::new());
    };
    let mut dock = DockState::new(std::mem::take(&mut groups[first].1));
    let tree = dock.main_surface_mut();
    for (area, tabs) in groups {
        if tabs.is_empty() {
            continue;
        }
        match area {
            Area::Right => {
                tree.split_right(NodeIndex::root(), 0.75, tabs);
            }
            Area::Bottom => {
                tree.split_below(NodeIndex::root(), 0.68, tabs);
            }
            Area::Left => {
                tree.split_left(NodeIndex::root(), 0.22, tabs);
            }
            Area::Center => {}
        }
    }
    dock
}

/// Every split has two non-empty children and every other non-empty node hangs from a split.
pub fn is_consistent(tree: &Tree<Tab>) -> bool {
    let nodes: Vec<&Node<Tab>> = tree.iter().collect();
    let empty = |i: usize| nodes.get(i).is_none_or(|n| n.is_empty());
    nodes.iter().enumerate().all(|(i, node)| {
        if node.is_parent() {
            !empty(2 * i + 1) && !empty(2 * i + 2)
        } else {
            node.is_empty() || i == 0 || nodes[(i - 1) / 2].is_parent()
        }
    })
}

/// The leaf holding another panel of `area`, on the main surface.
fn area_leaf(dock: &DockState<Tab>, entries: &[PanelEntry], area: Area, except: &Tab) -> Option<TabPath> {
    entries
        .iter()
        .filter(|e| e.area == area && &e.tab != except)
        .find_map(|e| dock.find_tab(&e.tab))
        .filter(|path| path.surface == SurfaceIndex::main())
}

/// Puts a panel next to a panel of the same area, or on its side of the window.
pub fn place(dock: &mut DockState<Tab>, entries: &[PanelEntry], entry: &PanelEntry) {
    if dock.main_surface().num_tabs() == 0 {
        dock.push_to_first_leaf(entry.tab.clone());
        return;
    }
    if let Some(path) = area_leaf(dock, entries, entry.area, &entry.tab) {
        dock[path.surface][path.node].append_tab(entry.tab.clone());
        return;
    }
    let right = area_leaf(dock, entries, Area::Right, &entry.tab);
    let tabs = vec![entry.tab.clone()];
    let tree = dock.main_surface_mut();
    match entry.area {
        Area::Left => {
            tree.split_left(NodeIndex::root(), 0.22, tabs);
        }
        Area::Right => {
            tree.split_right(NodeIndex::root(), 0.75, tabs);
        }
        Area::Bottom => {
            tree.split_below(NodeIndex::root(), 0.68, tabs);
        }
        Area::Center => match right {
            // The fraction is the share of the left node, here the centre panel.
            Some(path) => {
                tree.split_left(path.node, 0.75, tabs);
            }
            None => dock.push_to_first_leaf(entry.tab.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};

    use uniwow_api::egui_dock::DockState;
    use uniwow_api::{DockArea as Area, serde_json};

    use super::{PanelEntry, Tab, default_layout, is_consistent, restore};

    /// The layout egui_dock's `retain_tabs` produced in the first acceptance run: a split root whose
    /// children are empty, with the only remaining leaf orphaned below them.
    const BROKEN: &str = include_str!("fixtures/broken_layout.json");

    fn entry(module: &str, panel: &str, area: Area) -> PanelEntry {
        PanelEntry {
            tab: Tab::new(module, panel),
            title: panel.to_owned(),
            area,
            open_by_default: true,
        }
    }

    fn kernel() -> Vec<PanelEntry> {
        vec![
            entry("kernel", "modules", Area::Bottom),
            entry("kernel", "log", Area::Bottom),
        ]
    }

    fn all() -> Vec<PanelEntry> {
        let mut entries = kernel();
        entries.push(entry("viewport", "view", Area::Center));
        entries.push(entry("sample-cube", "cube", Area::Right));
        entries.push(entry("sample-notes", "events", Area::Left));
        entries
    }

    fn tabs(dock: &DockState<Tab>) -> HashSet<Tab> {
        dock.iter_all_tabs().map(|(_, tab)| tab.clone()).collect()
    }

    fn expected(entries: &[PanelEntry]) -> HashSet<Tab> {
        entries.iter().map(|e| e.tab.clone()).collect()
    }

    fn saved(dock: &DockState<Tab>) -> Option<serde_json::Value> {
        Some(serde_json::to_value(dock).expect("serialisable"))
    }

    #[test]
    fn the_default_layout_holds_every_panel() {
        let dock = default_layout(&all(), &BTreeSet::new());
        assert!(is_consistent(dock.main_surface()));
        assert_eq!(tabs(&dock), expected(&all()));
    }

    #[test]
    fn without_a_saved_layout_the_default_is_used() {
        let dock = restore(None, &all(), &BTreeSet::new());
        assert_eq!(tabs(&dock), expected(&all()));
    }

    #[test]
    fn an_unreadable_saved_layout_gives_the_default() {
        let dock = restore(Some(serde_json::json!("not a layout")), &all(), &BTreeSet::new());
        assert!(is_consistent(dock.main_surface()));
        assert_eq!(tabs(&dock), expected(&all()));
    }

    #[test]
    fn the_broken_layout_is_detected_and_replaced() {
        let broken: DockState<Tab> = serde_json::from_str(BROKEN).expect("fixture");
        assert!(!is_consistent(broken.main_surface()));
        let dock = restore(serde_json::from_str(BROKEN).ok(), &all(), &BTreeSet::new());
        assert!(is_consistent(dock.main_surface()));
        assert_eq!(tabs(&dock), expected(&all()));
    }

    #[test]
    fn panels_of_absent_modules_leave_without_breaking_the_tree() {
        let mut with_third = all();
        with_third.push(entry("third", "main", Area::Right));
        let before = default_layout(&with_third, &BTreeSet::new());
        let dock = restore(saved(&before), &all(), &BTreeSet::new());
        assert!(is_consistent(dock.main_surface()));
        assert_eq!(tabs(&dock), expected(&all()));
    }

    #[test]
    fn removing_almost_every_panel_keeps_the_tree_consistent() {
        // The case where retain_tabs broke the tree: only the kernel panels remain.
        let before = default_layout(&all(), &BTreeSet::new());
        let dock = restore(saved(&before), &kernel(), &BTreeSet::new());
        assert!(is_consistent(dock.main_surface()));
        assert_eq!(tabs(&dock), expected(&kernel()));
    }

    #[test]
    fn a_returning_area_gets_the_default_layout() {
        let without_view: Vec<PanelEntry> = all().into_iter().filter(|e| e.area != Area::Center).collect();
        let before = default_layout(&without_view, &BTreeSet::new());
        let dock = restore(saved(&before), &all(), &BTreeSet::new());
        assert!(is_consistent(dock.main_surface()));
        assert_eq!(tabs(&dock), expected(&all()));
    }

    #[test]
    fn closed_panels_stay_closed() {
        let closed = BTreeSet::from(["sample-cube/cube".to_owned()]);
        let dock = restore(None, &all(), &closed);
        assert!(!tabs(&dock).contains(&Tab::new("sample-cube", "cube")));
        assert_eq!(tabs(&dock).len(), all().len() - 1);
    }
}
