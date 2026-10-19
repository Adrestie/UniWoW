//! The objects `HierarchyView` of every language (step 15.12b), drawn through the service
//! `hierarchy-view`: the tree of the window Hierarchy, its view, filter and selection, each object
//! folded on its own; a click selects, as in the window.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uniwow_api::egui;
use uniwow_api::scene::{self, EntityId, Select};
use uniwow_api::scene_view::SceneView;

use crate::tree::Tree;

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the module shares with the service: the tree it made last, the colours of the notes, and
/// whether a view was drawn since the module last looked, for it to keep the tree made.
#[derive(Default)]
pub struct Shared {
    pub tree: Mutex<Arc<Tree>>,
    pub noted: Mutex<Arc<HashMap<EntityId, egui::Color32>>>,
    pub drawn: AtomicBool,
    /// The scene, once the module is started.
    pub scene: std::sync::OnceLock<scene::Handle>,
}

/// The service: the folds of each view by its id.
pub struct Views {
    pub shared: Arc<Shared>,
    folds: Mutex<HashMap<egui::Id, HashSet<String>>>,
}

impl Views {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            folds: Mutex::default(),
        }
    }
}

/// The height of a row.
const ROW: f32 = 18.0;

impl SceneView for Views {
    fn show(&self, ui: &mut egui::Ui, id: egui::Id, _entities: &[String]) {
        self.shared.drawn.store(true, Ordering::Relaxed);
        let Some(scene) = self.shared.scene.get() else {
            ui.weak("No scene: the module scene is not running.");
            return;
        };
        let tree = lock(&self.shared.tree).clone();
        let noted = lock(&self.shared.noted).clone();
        let mut folds = lock(&self.folds);
        let unfolded = folds
            .entry(id)
            .or_insert_with(|| HashSet::from(["map".to_owned(), "live".to_owned()]));
        let rows = tree.rows(unfolded);
        if rows.is_empty() {
            ui.weak("Nothing in the scope.");
            return;
        }
        let selection = scene.selection();
        let (mut toggled, mut clicked) = (None, None);
        egui::ScrollArea::vertical()
            .id_salt(id)
            .auto_shrink(false)
            .show_rows(ui, ROW, rows.len(), |ui, range| {
                for (node, depth) in &rows[range] {
                    let at = &tree.nodes[*node as usize];
                    ui.horizontal(|ui| {
                        ui.add_space(f32::from(*depth) * 12.0);
                        if at.children.is_empty() {
                            ui.add_space(18.0);
                        } else if ui
                            .small_button(if unfolded.contains(&at.key) { "▼" } else { "▶" })
                            .clicked()
                        {
                            toggled = Some(at.key.clone());
                        }
                        let mut text = egui::RichText::new(&at.label);
                        if let Some(colour) = at.entity.and_then(|entity| noted.get(&entity)) {
                            text = text.color(*colour);
                        }
                        if at.entity.is_none() {
                            text = text.strong();
                        }
                        let selected = at.entity.is_some_and(|entity| selection.contains(&entity));
                        if ui.selectable_label(selected, text).clicked()
                            && let Some(entity) = at.entity
                        {
                            clicked = Some(entity);
                        }
                    });
                }
            });
        if let Some(key) = toggled
            && !unfolded.remove(&key)
        {
            unfolded.insert(key);
        }
        drop(folds);
        if let Some(entity) = clicked {
            scene.select(Select::Set(vec![entity]));
        }
    }

    fn forget(&self, id: egui::Id) {
        lock(&self.folds).remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Node;

    fn node(key: &str, entity: Option<EntityId>, parent: Option<u32>, children: Vec<u32>) -> Node {
        Node {
            key: key.to_owned(),
            label: key.to_owned(),
            entity,
            area: None,
            parent,
            children,
        }
    }

    #[test]
    fn each_view_is_folded_on_its_own_and_forgotten_once_gone() {
        let tree = Tree {
            nodes: vec![
                node("map", None, None, vec![1]),
                node("doodad:0:1", Some(EntityId::doodad(0, 1)), Some(0), Vec::new()),
            ],
            roots: vec![0],
            by_entity: HashMap::from([(EntityId::doodad(0, 1), 1)]),
            entities: 1,
        };
        let shared = Arc::new(Shared::default());
        *lock(&shared.tree) = Arc::new(tree);
        let views = Views::new(shared.clone());
        let ctx = egui::Context::default();
        let (first, second) = (egui::Id::new("first"), egui::Id::new("second"));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            views.show(ui, first, &[]);
            views.show(ui, second, &[]);
        });
        output.textures_delta.clear();
        assert!(shared.drawn.load(Ordering::Relaxed), "the module told a view was drawn");
        // No scene: nothing folded is kept, the views say why.
        assert!(lock(&views.folds).is_empty());
        lock(&views.folds).insert(first, HashSet::from(["map".to_owned()]));
        lock(&views.folds).insert(second, HashSet::new());
        assert_eq!(
            lock(&shared.tree).rows(&lock(&views.folds)[&first]).len(),
            2,
            "unfolded"
        );
        assert_eq!(lock(&shared.tree).rows(&lock(&views.folds)[&second]).len(), 1, "folded");
        views.forget(first);
        assert!(!lock(&views.folds).contains_key(&first));
    }
}
