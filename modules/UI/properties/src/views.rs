//! The objects `InspectorView` of every language (step 15.12b), drawn through the service
//! `inspector-view`: the components of the entities a view names, of the selection when none; read by
//! jobs of the module as the Inspector's, again when the scene changes or after a write. A value
//! typed is written as in the Inspector, in a gesture the module opens at its next frame.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use uniwow_api::egui;
use uniwow_api::scene::components::FieldKind;
use uniwow_api::scene::{self, EntityId};
use uniwow_api::scene_view::SceneView;
use uniwow_api::serde_json::{Value, json};

use crate::inspector::{self, Read};

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a view's values were read for: the scene's generation and the writes made.
pub type ReadFor = (u64, u64);

/// The values read for each list of entities, and what for.
pub type Reads = HashMap<Vec<EntityId>, (ReadFor, Arc<Read>)>;

/// A write asked by a view: the command, its arguments, the label of its gesture.
pub type Write = (String, Value, String);

/// What the module shares with the service.
#[derive(Default)]
pub struct Shared {
    pub scene: OnceLock<scene::Handle>,
    /// The values read for each list of entities, and what for.
    pub reads: Mutex<Reads>,
    /// The lists of entities to read again, the views showing them.
    pub wanted: Mutex<HashSet<Vec<EntityId>>>,
    pub writes: Mutex<Vec<Write>>,
    /// Counts the writes made, so that the values are read again after each.
    pub written: AtomicU64,
}

/// The service: the text typed in each field of each view.
pub struct Views {
    pub shared: Arc<Shared>,
    typed: Mutex<HashMap<(egui::Id, String), String>>,
}

impl Views {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            typed: Mutex::default(),
        }
    }

    /// A field of `key` in the view `id` showing `value`: what was typed and confirmed by Enter.
    fn field(&self, ui: &mut egui::Ui, id: egui::Id, key: String, value: &str) -> Option<String> {
        let mut typed = lock(&self.typed);
        let text = typed.entry((id, key)).or_insert_with(|| value.to_owned());
        let response = ui.add(egui::TextEdit::singleline(text).desired_width(70.0));
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let confirmed = text.trim().to_owned();
            return (confirmed != value && !confirmed.is_empty() && confirmed != "—").then_some(confirmed);
        }
        if !response.has_focus() && text != value {
            value.clone_into(text);
        }
        None
    }
}

/// The ids of a view: those it names that parse, else the selection, a group by what it holds.
pub fn shown_ids(scene: &scene::Handle, entities: &[String]) -> Vec<EntityId> {
    let ids: Vec<EntityId> = if entities.is_empty() {
        scene.selection().ids.clone()
    } else {
        entities.iter().filter_map(|id| id.parse().ok()).collect()
    };
    scene.groups().expanded(&ids)
}

impl SceneView for Views {
    fn show(&self, ui: &mut egui::Ui, id: egui::Id, entities: &[String]) {
        let Some(scene) = self.shared.scene.get() else {
            ui.weak("No scene: the module scene is not running.");
            return;
        };
        let ids = shown_ids(scene, entities);
        if ids.is_empty() {
            ui.weak("Nothing selected.");
            return;
        }
        let snapshot = scene.snapshot();
        let wanted = (snapshot.generation, self.shared.written.load(Ordering::Relaxed));
        let read = lock(&self.shared.reads).get(&ids).cloned();
        if read.as_ref().is_none_or(|(read_for, _)| *read_for != wanted) {
            lock(&self.shared.wanted).insert(ids.clone());
        }
        let names = scene.names();
        match ids.as_slice() {
            [one] => {
                let name = snapshot
                    .editable(one)
                    .map(|entity| names.get(one).cloned().unwrap_or_else(|| entity.name()))
                    .unwrap_or_else(|| one.to_string());
                ui.strong(name);
                ui.weak(one.to_string());
            }
            _ => {
                ui.strong(format!("{} objects", ids.len()));
            }
        }
        let Some((_, read)) = read else {
            ui.weak("Reading…");
            return;
        };
        let texts: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let mut writes = Vec::new();
        egui::ScrollArea::vertical().id_salt(id).auto_shrink(false).show(ui, |ui| {
            // The Transform, written through the providers as the Inspector writes it.
            if !read.values.is_empty() {
                egui::CollapsingHeader::new("Transform")
                    .id_salt((id, "transform"))
                    .default_open(true)
                    .show(ui, |ui| {
                        for (field, label, vector) in inspector::TRANSFORM {
                            let axes: Vec<Option<usize>> = if vector { vec![Some(0), Some(1), Some(2)] } else { vec![None] };
                            if inspector::shown(&read.values, "transform", field, axes[0]) == inspector::Shown::Missing {
                                continue;
                            }
                            ui.horizontal(|ui| {
                                ui.label(label);
                                for axis in axes {
                                    let value = inspector::text(&inspector::shown(&read.values, "transform", field, axis));
                                    if let Some(change) = self.field(ui, id, format!("transform/{field}:{axis:?}"), &value) {
                                        for (provider, ids) in inspector::by_provider(&snapshot, &ids) {
                                            if inspector::writes(&provider, field) {
                                                writes.push((
                                                    format!("{provider}.write"),
                                                    json!({ "ids": ids, "field": field, "axis": axis, "change": change }),
                                                    format!("Set the {field}"),
                                                ));
                                            }
                                        }
                                    }
                                }
                            });
                        }
                    });
            }
            for (component, values) in &read.declared {
                egui::CollapsingHeader::new(&component.label)
                    .id_salt((id, &component.module, &component.name))
                    .default_open(true)
                    .show(ui, |ui| {
                        for field in &component.fields {
                            ui.horizontal(|ui| {
                                ui.label(&field.label);
                                let axes: Vec<Option<usize>> = if field.kind == FieldKind::Vector {
                                    vec![Some(0), Some(1), Some(2)]
                                } else {
                                    vec![None]
                                };
                                for axis in axes {
                                    let value = inspector::common(values, &field.name, axis).unwrap_or_default();
                                    let writable = !field.read_only && component.write.is_some();
                                    if !writable || field.kind == FieldKind::Boolean || field.kind == FieldKind::Colour {
                                        ui.label(value);
                                        continue;
                                    }
                                    let key = format!("{}/{}/{}:{axis:?}", component.module, component.name, field.name);
                                    if let (Some(change), Some(command)) = (self.field(ui, id, key, &value), &component.write) {
                                        writes.push((
                                            command.clone(),
                                            json!({ "ids": texts, "field": field.name, "axis": axis, "change": change }),
                                            format!("Set the {}", field.name),
                                        ));
                                    }
                                }
                            });
                        }
                    });
            }
            for refused in &read.refused {
                ui.colored_label(ui.visuals().warn_fg_color, refused);
            }
        });
        lock(&self.shared.writes).extend(writes);
    }

    fn forget(&self, id: egui::Id) {
        lock(&self.typed).retain(|(view, _), _| *view != id);
    }
}
