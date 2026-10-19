//! The window *Inspector* (milestone 15, step 15.5), right of the 3D view: the entities selected
//! as components, each a set of fields. Its header gives the name, the kind and the id, the place and
//! the other zones its bounds reach, and the tiles storing it; for several, how many and of which
//! kinds. *Transform*: a field typed is given to every entity selected, `+n`, `-=n`, `*n` and `/n`
//! changing each from its own value, as one gesture, one undo entry whatever modules it reaches; a
//! value that differs shows `—`. *Model* and the raw data as the files store them, read only. The
//! values are read by a job through the commands `<provider>.read`, again when the selection or the
//! registry changes; written through `<provider>.write`. The Inspector can be locked on what it shows.
//! The components the modules declare (step 15.12) are read and written through the commands they
//! name, drawn from their fields.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use uniwow_api::egui;
use uniwow_api::scene::components::{Component, FieldKind};
use uniwow_api::scene::{self, EntityId, Kind, Snapshot};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::{CallId, Context, JobId, JobOutcome, log};

/// The providers whose entities are written through `<provider>.write`, and the fields of each.
const WRITTEN: [(&str, &[&str]); 3] = [
    ("doodads", &["position", "rotation", "scale"]),
    ("buildings", &["position", "rotation"]),
    ("spawns", &["position", "rotation"]),
];

/// The fields of the Transform, in their order: their name, their label, whether a vector.
const TRANSFORM: [(&str, &str, bool); 3] = [
    ("position", "Position", true),
    ("rotation", "Rotation", true),
    ("scale", "Scale", false),
];

/// What a field shows for the entities selected.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    /// The same value for all.
    Same(f64),
    /// Values that differ: `—`.
    Mixed,
    /// None of them has it.
    Missing,
}

/// The value of `field` (on `axis` of a vector) of the component `component` of each of `read`, as
/// the Inspector shows them together.
pub fn shown(read: &[(EntityId, Value)], component: &str, field: &str, axis: Option<usize>) -> Shown {
    let mut found: Option<f64> = None;
    let mut any = false;
    for (_, value) in read {
        let at = &value["components"][component][field];
        let number = match axis {
            Some(axis) => at.get(axis).and_then(Value::as_f64),
            None => at.as_f64(),
        };
        let Some(number) = number else {
            continue;
        };
        any = true;
        match found {
            None => found = Some(number),
            Some(before) if (before - number).abs() > 1e-4 => return Shown::Mixed,
            Some(_) => {}
        }
    }
    match (any, found) {
        (true, Some(value)) => Shown::Same(value),
        _ => Shown::Missing,
    }
}

/// The text of a field shown.
pub fn text(shown: &Shown) -> String {
    match shown {
        Shown::Same(value) => {
            let text = format!("{value:.3}");
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
        Shown::Mixed => "—".to_owned(),
        Shown::Missing => String::new(),
    }
}

/// What a job read of the entities shown: their components by provider, and those no provider read
/// (shown from the registry, read only).
#[derive(Default)]
pub struct Read {
    pub values: Vec<(EntityId, Value)>,
    pub refused: Vec<String>,
    /// The components declared that fit the entities, each with their fields by entity.
    pub declared: Vec<(Component, Vec<(EntityId, Value)>)>,
}

/// Reads the components of `ids` through the read commands of their providers, `snapshot` naming
/// them.
fn read(editor: &uniwow_api::Editor, snapshot: &Snapshot, ids: &[EntityId], components: &[Component]) -> Read {
    let mut by_provider: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in ids {
        if let Some(entity) = snapshot.editable(id) {
            by_provider
                .entry(entity.provider().to_owned())
                .or_default()
                .push(id.to_string());
        }
    }
    let mut out = Read::default();
    for (provider, ids) in by_provider {
        if !WRITTEN.iter().any(|(written, _)| *written == provider) {
            continue;
        }
        match editor.call(&format!("{provider}.read"), json!({ "ids": ids })) {
            Ok(answer) => {
                for value in answer["entities"].as_array().cloned().unwrap_or_default() {
                    if let Some(id) = value["id"].as_str().and_then(|id| id.parse().ok()) {
                        out.values.push((id, value));
                    }
                }
            }
            Err(reason) => out.refused.push(format!("{provider}: {reason}")),
        }
    }
    let texts: Vec<String> = ids.iter().map(ToString::to_string).collect();
    for component in components
        .iter()
        .filter(|component| component.fits(ids.iter().map(|id| id.kind)))
    {
        match editor.call(&component.read, json!({ "ids": texts })) {
            Ok(answer) => out.declared.push((component.clone(), fields_of(&answer))),
            Err(reason) => out.refused.push(format!("{}: {reason}", component.label)),
        }
    }
    out
}

/// The fields of each entity of an answer of a component's read command.
fn fields_of(answer: &Value) -> Vec<(EntityId, Value)> {
    answer["entities"]
        .as_array()
        .map(|entities| {
            entities
                .iter()
                .filter_map(|entity| Some((entity["id"].as_str()?.parse().ok()?, entity["fields"].clone())))
                .collect()
        })
        .unwrap_or_default()
}

/// The value of `field` (on `axis` of a vector) common to `values` as text, `—` when they differ,
/// nothing when none has it.
pub fn common(values: &[(EntityId, Value)], field: &str, axis: Option<usize>) -> Option<String> {
    let mut shown = values.iter().map(|(_, fields)| {
        let value = &fields[field];
        match axis {
            Some(axis) => value.get(axis).cloned().unwrap_or(Value::Null),
            None => value.clone(),
        }
    });
    let first = shown.next().filter(|value| !value.is_null())?;
    Some(if shown.all(|value| value == first) {
        match &first {
            Value::Number(number) => text(&Shown::Same(number.as_f64().unwrap_or(0.0))),
            other => plain(other),
        }
    } else {
        "—".to_owned()
    })
}

/// What the Inspector read for, so that it reads again only when it changes.
#[derive(Clone, Debug, PartialEq)]
struct ReadFor {
    ids: Vec<EntityId>,
    registry: u64,
    written: u64,
    /// The declarations read with, by their address.
    declared: usize,
}

#[derive(Default)]
pub struct Inspector {
    scene: Option<scene::Handle>,
    /// The entities it keeps showing while locked.
    locked: Option<Vec<EntityId>>,
    raw: bool,
    read: Arc<Read>,
    read_for: Option<ReadFor>,
    reading: Option<(JobId, ReadFor)>,
    /// Counts the writes answered, so that the values are read again after each.
    written: u64,
    /// The text typed in each field not yet confirmed, by its key.
    typed: HashMap<String, String>,
    /// The writes waiting for their answers.
    waiting: Vec<CallId>,
    /// What the last writes refused.
    said: Vec<String>,
    /// The Transform copied, its fields by name.
    copied: Option<Value>,
    /// The search among the quests, the call answering it and the quests found.
    quest_search: String,
    searched: String,
    searching: Option<(CallId, String)>,
    found: Vec<(u64, String)>,
}

impl Inspector {
    pub fn init(&mut self, ctx: &mut Context) {
        self.scene = ctx.service(scene::SERVICE);
    }

    /// The entities shown: those it is locked on, else the selection; a group by what it holds.
    fn ids(&self, scene: &scene::Handle) -> Vec<EntityId> {
        let ids = match &self.locked {
            Some(ids) => ids.clone(),
            None => scene.selection().ids.clone(),
        };
        scene.groups().expanded(&ids)
    }

    /// The values read again by a job when what they are read for changed.
    fn steer(&mut self, scene: &scene::Handle, ids: &[EntityId], ctx: &mut Context) {
        let snapshot = scene.snapshot();
        let components = scene.components();
        let wanted = ReadFor {
            ids: ids.to_vec(),
            registry: snapshot.generation,
            written: self.written,
            declared: Arc::as_ptr(&components) as usize,
        };
        if self.read_for.as_ref() == Some(&wanted)
            || self.reading.as_ref().is_some_and(|(_, running)| *running == wanted)
        {
            return;
        }
        if let Some((job, _)) = self.reading.take() {
            ctx.cancel(job);
        }
        let editor = ctx.editor();
        let ids = ids.to_vec();
        let job = ctx.spawn("Read the components of the selection", move |_| {
            read(&editor, &snapshot, &ids, &components)
        });
        self.reading = Some((job, wanted));
    }

    /// Writes `change` into the field `field` (on `axis`) of the entities shown, a call to the
    /// provider of each, in one gesture.
    fn write(
        &mut self,
        writes: &[(&str, Option<usize>, String)],
        ids: &[EntityId],
        snapshot: &Snapshot,
        ctx: &mut Context,
    ) {
        let mut by_provider: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for id in ids {
            if let Some(entity) = snapshot.editable(id) {
                by_provider
                    .entry(entity.provider().to_owned())
                    .or_default()
                    .push(id.to_string());
            }
        }
        let label = match (writes, ids.len()) {
            ([(field, _, _)], 1) => format!("Set the {field}"),
            ([(field, _, _)], n) => format!("Set the {field} of {n} objects"),
            (_, n) => format!("Paste the Transform on {n} objects"),
        };
        let gesture = ctx.begin_gesture(&label);
        for (provider, ids) in by_provider {
            let Some((_, fields)) = WRITTEN.iter().find(|(written, _)| *written == provider) else {
                continue;
            };
            for (field, axis, change) in writes {
                if !fields.contains(field) {
                    continue;
                }
                let call = ctx.call(
                    &format!("{provider}.write"),
                    json!({ "ids": ids, "field": field, "axis": axis, "change": change }),
                );
                self.waiting.push(call);
            }
        }
        ctx.end_gesture(gesture);
        self.said.clear();
    }

    /// Takes the answer to a write; whether it was one.
    pub fn on_reply(&mut self, call: CallId, result: Result<Value, String>) -> bool {
        if self.searching.as_ref().is_some_and(|(searching, _)| *searching == call) {
            self.searching = None;
            self.found = result.map(|answer| quests_found(&answer)).unwrap_or_else(|reason| {
                self.said.push(reason);
                Vec::new()
            });
            return true;
        }
        let Some(at) = self.waiting.iter().position(|waiting| *waiting == call) else {
            return false;
        };
        self.waiting.remove(at);
        match result {
            Ok(answer) => {
                for refused in answer["refused"].as_array().cloned().unwrap_or_default() {
                    if let Some(refused) = refused.as_str() {
                        self.said.push(refused.to_owned());
                    }
                }
            }
            Err(reason) => self.said.push(reason),
        }
        if self.waiting.is_empty() {
            self.written += 1;
        }
        true
    }

    /// Takes the end of a reading job; whether it was one.
    pub fn on_job(&mut self, job: JobId, outcome: JobOutcome) -> bool {
        if self.reading.as_ref().is_none_or(|(running, _)| *running != job) {
            return false;
        }
        let Some((_, read_for)) = self.reading.take() else {
            return true;
        };
        match outcome {
            JobOutcome::Panicked(message) => log::error!("the components of the selection were not read: {message}"),
            JobOutcome::Cancelled => {}
            outcome => {
                if let Some(read) = outcome.take::<Read>() {
                    self.read = Arc::new(read);
                    self.read_for = Some(read_for);
                    // The values typed and not confirmed are those of what was shown before.
                    self.typed.clear();
                }
            }
        }
        true
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(scene) = self.scene.clone() else {
            ui.label("No scene: the module scene is not running.");
            return;
        };
        let ids = self.ids(&scene);
        self.steer(&scene, &ids, ctx);
        let snapshot = scene.snapshot();
        ui.horizontal(|ui| {
            let mut locked = self.locked.is_some();
            if ui
                .toggle_value(&mut locked, "🔒 Lock")
                .on_hover_text("Keep showing these entities while the selection changes")
                .changed()
            {
                self.locked = locked.then(|| ids.clone());
            }
            ui.toggle_value(&mut self.raw, "Raw")
                .on_hover_text("The fields as the files store them");
        });
        ui.separator();
        if ids.is_empty() {
            ui.label("Nothing selected.");
            return;
        }
        self.header(ui, &snapshot, &scene, &ids);
        ui.separator();
        let read = self.read.clone();
        let values: Vec<(EntityId, Value)> = read.values.iter().filter(|(id, _)| ids.contains(id)).cloned().collect();
        let unread = ids.len() - values.len();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if self.raw {
                self.raw_ui(ui, &values);
            } else {
                self.transform_ui(ui, &values, unread, &ids, &snapshot, ctx);
                self.model_ui(ui, &values);
                self.quests_ui(ui, &values, ctx);
                self.declared_ui(ui, &read, &ids, ctx);
            }
            for refused in read.refused.iter().chain(&self.said) {
                ui.colored_label(ui.visuals().warn_fg_color, refused);
            }
        });
    }

    fn header(&self, ui: &mut egui::Ui, snapshot: &Snapshot, scene: &scene::Handle, ids: &[EntityId]) {
        let names = scene.names();
        if let [id] = ids {
            let Some(entity) = snapshot.editable(id) else {
                ui.label(format!("{id}"));
                ui.colored_label(ui.visuals().warn_fg_color, "Out of reach: its tile is not loaded.");
                return;
            };
            let name = names.get(id).cloned().unwrap_or_else(|| entity.name());
            ui.heading(name);
            ui.label(egui::RichText::new(format!("{} · {id}", id.kind.name())).weak());
            let place: Vec<&str> = snapshot
                .place(entity.area())
                .into_iter()
                .map(|(_, name)| name)
                .collect();
            ui.label(if place.is_empty() {
                "No area".to_owned()
            } else {
                place.join(" > ")
            });
            let others: Vec<&str> = snapshot
                .areas_within(entity.placed.key.map, entity.bounds())
                .into_iter()
                .filter(|area| *area != entity.area())
                .filter_map(|area| snapshot.areas.get(&area).map(|found| found.name.as_str()))
                .collect();
            if !others.is_empty() {
                ui.label(format!("Reaches also: {}", others.join(", ")));
            }
            let tiles: Vec<String> = entity
                .tiles()
                .iter()
                .map(|tile| format!("{}_{}", tile.x, tile.y))
                .collect();
            if !tiles.is_empty() {
                ui.label(egui::RichText::new(format!("Stored in the tiles {}", tiles.join(", "))).weak());
            }
            if let Some(live) = snapshot.live(id).filter(|live| live.id().kind != Kind::Live) {
                let [x, y, z] = live.origin();
                ui.label(format!("● Live: now at {x:.1}, {y:.1}, {z:.1}"));
            } else if entity.flags() & scene::flags::READ_ONLY != 0 {
                ui.label(egui::RichText::new("Read only").weak());
            }
        } else {
            let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
            for id in ids {
                *kinds.entry(id.kind.name()).or_default() += 1;
            }
            let kinds: Vec<String> = kinds
                .into_iter()
                .map(|(kind, count)| format!("{count} {kind}"))
                .collect();
            ui.heading(format!("{} objects", ids.len()));
            ui.label(kinds.join(", "));
        }
    }

    fn transform_ui(
        &mut self,
        ui: &mut egui::Ui,
        values: &[(EntityId, Value)],
        unread: usize,
        ids: &[EntityId],
        snapshot: &Snapshot,
        ctx: &mut Context,
    ) {
        egui::CollapsingHeader::new("Transform").default_open(true).show(ui, |ui| {
            if values.is_empty() {
                // Shown from the registry, read only: the live world, the rooms, the doodads of the
                // buildings, the terrain.
                if let [id] = ids
                    && let Some(entity) = snapshot.get(id)
                {
                    let [x, y, z] = entity.origin();
                    ui.label(format!("Position {x:.2}, {y:.2}, {z:.2}"));
                }
                let doodad_of_building = ids.iter().any(|id| id.kind == Kind::BuildingDoodad);
                ui.label(
                    egui::RichText::new(if doodad_of_building {
                        "Read only: a doodad of a building belongs to the building's file, changed for every placement of it in a later step."
                    } else {
                        "Read only."
                    })
                    .weak(),
                );
                return;
            }
            if unread > 0 {
                ui.label(egui::RichText::new(format!("{unread} of the selection read only, left as they are")).weak());
            }
            let mut writes = Vec::new();
            egui::Grid::new("inspector transform").num_columns(4).show(ui, |ui| {
                for (field, label, vector) in TRANSFORM {
                    if shown(values, "transform", field, if vector { Some(0) } else { None }) == Shown::Missing {
                        continue;
                    }
                    ui.label(label);
                    let axes: Vec<Option<usize>> = if vector { vec![Some(0), Some(1), Some(2)] } else { vec![None] };
                    for axis in axes {
                        let key = format!("{field}:{axis:?}");
                        let value = text(&shown(values, "transform", field, axis));
                        let typed = self.typed.entry(key.clone()).or_insert_with(|| value.clone());
                        let response = ui.add(egui::TextEdit::singleline(typed).desired_width(70.0));
                        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            if *typed != value && !typed.trim().is_empty() && typed.trim() != "—" {
                                writes.push((field, axis, typed.trim().to_owned()));
                            }
                        } else if !response.has_focus() && *typed != value {
                            *typed = value;
                        }
                    }
                    ui.end_row();
                }
            });
            ui.label(egui::RichText::new("Type a value, or +n, -=n, *n, /n to change each from its own.").weak().small());
            ui.horizontal(|ui| {
                if ui.button("Copy").clicked() {
                    self.copied = values.last().map(|(_, value)| value["components"]["transform"].clone());
                }
                if ui.add_enabled(self.copied.is_some(), egui::Button::new("Paste")).clicked()
                    && let Some(copied) = &self.copied
                {
                    for (field, _, vector) in TRANSFORM {
                        let axes: Vec<Option<usize>> = if vector { vec![Some(0), Some(1), Some(2)] } else { vec![None] };
                        for axis in axes {
                            let value = match axis {
                                Some(axis) => copied[field].get(axis).and_then(Value::as_f64),
                                None => copied[field].as_f64(),
                            };
                            if let Some(value) = value {
                                writes.push((field, axis, format!("{value}")));
                            }
                        }
                    }
                }
            });
            if !writes.is_empty() {
                let writable: Vec<EntityId> = values.iter().map(|(id, _)| *id).collect();
                self.write(&writes, &writable, snapshot, ctx);
            }
        });
    }

    fn model_ui(&self, ui: &mut egui::Ui, values: &[(EntityId, Value)]) {
        let models: Vec<&Value> = values.iter().map(|(_, value)| &value["components"]["model"]).collect();
        let Some(first) = models.first() else {
            return;
        };
        egui::CollapsingHeader::new("Model").default_open(true).show(ui, |ui| {
            let Some(fields) = first.as_object() else {
                return;
            };
            egui::Grid::new("inspector model").num_columns(2).show(ui, |ui| {
                for (name, value) in fields {
                    let same = models.iter().all(|model| model[name] == *value);
                    ui.label(name);
                    ui.label(if same { plain(value) } else { "—".to_owned() });
                    ui.end_row();
                }
            });
        });
    }

    /// The quests the template of the spawns shown starts and ends, one template at a time: each
    /// taken away by its button, and those a search finds given.
    fn quests_ui(&mut self, ui: &mut egui::Ui, values: &[(EntityId, Value)], ctx: &mut Context) {
        let Some((_, first)) = values.first() else {
            return;
        };
        let Some(quests) = first["components"]["quests"].as_array() else {
            return;
        };
        egui::CollapsingHeader::new("Quests").default_open(true).show(ui, |ui| {
            let template = &first["components"]["model"]["template"];
            if values
                .iter()
                .any(|(_, value)| value["components"]["model"]["template"] != *template)
            {
                ui.label(egui::RichText::new("The quests of one template at a time.").weak());
                return;
            }
            let ids: Vec<String> = values.iter().map(|(id, _)| id.to_string()).collect();
            let mut changes = Vec::new();
            if quests.is_empty() {
                ui.label(egui::RichText::new("No quest.").weak());
            }
            egui::Grid::new("inspector quests").num_columns(3).show(ui, |ui| {
                for quest in quests {
                    let (id, role) = (quest["quest"].as_u64().unwrap_or(0), plain(&quest["role"]));
                    ui.label(format!("{role} {id}"));
                    ui.label(plain(&quest["title"]));
                    if ui.small_button("✖").on_hover_text("Take this quest away").clicked() {
                        changes.push((id, role, false));
                    }
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                ui.label("🔍");
                ui.text_edit_singleline(&mut self.quest_search)
                    .on_hover_text("A quest by its title or its id");
            });
            let search = self.quest_search.trim().to_owned();
            if !search.is_empty() && self.searched != search {
                let call = ctx.call("spawns.quest_titles", json!({ "search": search }));
                self.searching = Some((call, search.clone()));
                self.searched = search.clone();
            }
            if search.is_empty() {
                self.found.clear();
                self.searched.clear();
            }
            for (id, title) in &self.found {
                ui.horizontal(|ui| {
                    ui.label(format!("{id} {title}"));
                    for role in ["starts", "ends"] {
                        if ui.small_button(format!("+ {role}")).clicked() {
                            changes.push((*id, role.to_owned(), true));
                        }
                    }
                });
            }
            for (quest, role, given) in changes {
                let call = ctx.call(
                    "spawns.quests",
                    json!({ "ids": ids, "quest": quest, "role": role, "given": given }),
                );
                self.waiting.push(call);
            }
        });
    }

    /// The components the modules declared that fit the entities shown, each field drawn by its
    /// kind; a value typed written through the component's command, in a gesture.
    fn declared_ui(&mut self, ui: &mut egui::Ui, read: &Read, ids: &[EntityId], ctx: &mut Context) {
        for (component, values) in &read.declared {
            let values: Vec<(EntityId, Value)> = values.iter().filter(|(id, _)| ids.contains(id)).cloned().collect();
            let mut writes: Vec<(String, Option<usize>, String)> = Vec::new();
            egui::CollapsingHeader::new(&component.label)
                .id_salt(("declared", &component.module, &component.name))
                .default_open(true)
                .show(ui, |ui| {
                    if values.is_empty() {
                        ui.label(egui::RichText::new("Nothing read.").weak());
                        return;
                    }
                    egui::Grid::new(("inspector declared", &component.name))
                        .num_columns(4)
                        .show(ui, |ui| {
                            for field in &component.fields {
                                ui.label(&field.label);
                                let writable = !field.read_only && component.write.is_some();
                                let axes: Vec<Option<usize>> = if field.kind == FieldKind::Vector {
                                    vec![Some(0), Some(1), Some(2)]
                                } else {
                                    vec![None]
                                };
                                for axis in axes {
                                    let value = common(&values, &field.name, axis).unwrap_or_default();
                                    if field.kind == FieldKind::Boolean {
                                        let mut checked = value == "true";
                                        let response =
                                            ui.add_enabled(writable, egui::Checkbox::without_text(&mut checked));
                                        if response.changed() {
                                            writes.push((field.name.clone(), None, checked.to_string()));
                                        }
                                        continue;
                                    }
                                    if field.kind == FieldKind::Colour {
                                        let colour = values[0].1[&field.name].as_array().map(|rgb| {
                                            let at = |i: usize| {
                                                (rgb.get(i).and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1.0)
                                                    * 255.0) as u8
                                            };
                                            egui::Color32::from_rgb(at(0), at(1), at(2))
                                        });
                                        let (rect, _) =
                                            ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                                        ui.painter().rect_filled(
                                            rect,
                                            2.0,
                                            colour.unwrap_or(egui::Color32::TRANSPARENT),
                                        );
                                    }
                                    if !writable {
                                        ui.label(value);
                                        continue;
                                    }
                                    let key =
                                        format!("{}/{}/{}:{axis:?}", component.module, component.name, field.name);
                                    let shown = if field.kind == FieldKind::Colour {
                                        colour_text(&values, &field.name)
                                    } else {
                                        value
                                    };
                                    let typed = self.typed.entry(key).or_insert_with(|| shown.clone());
                                    let response = ui.add(egui::TextEdit::singleline(typed).desired_width(70.0));
                                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                        if *typed != shown && typed.trim() != "—" {
                                            writes.push((field.name.clone(), axis, typed.trim().to_owned()));
                                        }
                                    } else if !response.has_focus() && *typed != shown {
                                        *typed = shown;
                                    }
                                }
                                ui.end_row();
                            }
                        });
                });
            if let (Some(command), false) = (&component.write, writes.is_empty()) {
                let written: Vec<String> = values.iter().map(|(id, _)| id.to_string()).collect();
                let gesture = ctx.begin_gesture(&format!("Set the {}", writes[0].0));
                for (field, axis, change) in writes {
                    let call = ctx.call(
                        command,
                        json!({ "ids": written, "field": field, "axis": axis, "change": change }),
                    );
                    self.waiting.push(call);
                }
                ctx.end_gesture(gesture);
                self.said.clear();
            }
        }
    }

    fn raw_ui(&self, ui: &mut egui::Ui, values: &[(EntityId, Value)]) {
        let [(id, value)] = values else {
            ui.label(egui::RichText::new("The raw data of one entity read at a time.").weak());
            return;
        };
        egui::CollapsingHeader::new(format!("Raw: {id}"))
            .default_open(true)
            .show(ui, |ui| {
                let Some(fields) = value["components"]["raw"].as_object() else {
                    return;
                };
                egui::Grid::new("inspector raw").num_columns(2).show(ui, |ui| {
                    for (name, value) in fields {
                        ui.label(name);
                        ui.label(plain(value));
                        ui.end_row();
                    }
                });
            });
    }
}

/// The quests of an answer of `spawns.quest_titles`: their ids and titles.
fn quests_found(answer: &Value) -> Vec<(u64, String)> {
    answer["quests"]
        .as_array()
        .map(|quests| {
            quests
                .iter()
                .filter_map(|quest| Some((quest["quest"].as_u64()?, plain(&quest["title"]))))
                .collect()
        })
        .unwrap_or_default()
}

/// The colour `field` common to `values` as `#rrggbb`, `—` when they differ.
fn colour_text(values: &[(EntityId, Value)], field: &str) -> String {
    let hex = |value: &Value| {
        let rgb = value.as_array()?;
        let at = |i: usize| Some((rgb.get(i)?.as_f64()?.clamp(0.0, 1.0) * 255.0).round() as u8);
        Some(format!("#{:02x}{:02x}{:02x}", at(0)?, at(1)?, at(2)?))
    };
    let mut colours = values.iter().map(|(_, fields)| hex(&fields[field]));
    match colours.next().flatten() {
        Some(first) if colours.all(|colour| colour.as_deref() == Some(&first)) => first,
        Some(_) => "—".to_owned(),
        None => String::new(),
    }
}

/// A value of JSON as text, a string without its quotes.
fn plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fields_of_several_entities_show_their_common_value_or_a_dash() {
        let entity = |n: u32, x: f64, scale: Option<f64>| {
            let mut transform = json!({ "position": [x, 2.0, 3.0], "rotation": [0.0, 90.0, 0.0] });
            if let Some(scale) = scale {
                transform["scale"] = json!(scale);
            }
            (
                EntityId::doodad(0, n),
                json!({ "components": { "transform": transform } }),
            )
        };
        let read = vec![
            entity(1, 1.0, Some(1.0)),
            entity(2, 5.0, Some(1.0)),
            entity(3, 1.0, None),
        ];
        assert_eq!(shown(&read, "transform", "position", Some(0)), Shown::Mixed);
        assert_eq!(shown(&read, "transform", "position", Some(1)), Shown::Same(2.0));
        assert_eq!(
            shown(&read, "transform", "scale", None),
            Shown::Same(1.0),
            "the building has none"
        );
        assert_eq!(shown(&read, "transform", "colour", None), Shown::Missing);
        let fields = [
            (
                EntityId::doodad(0, 1),
                json!({ "tag": "lamp", "weight": 2.0, "at": [1.0, 2.0, 3.0], "colour": [1.0, 0.5, 0.0] }),
            ),
            (
                EntityId::doodad(0, 2),
                json!({ "tag": "lamp", "weight": 3.0, "at": [1.0, 5.0, 3.0], "colour": [1.0, 0.5, 0.0] }),
            ),
        ];
        assert_eq!(common(&fields, "tag", None).as_deref(), Some("lamp"));
        assert_eq!(common(&fields, "weight", None).as_deref(), Some("—"));
        assert_eq!(common(&fields, "at", Some(0)).as_deref(), Some("1"));
        assert_eq!(common(&fields, "at", Some(1)).as_deref(), Some("—"));
        assert_eq!(common(&fields, "missing", None), None);
        assert_eq!(colour_text(&fields, "colour"), "#ff8000");
        let answer = json!({ "entities": [{ "id": "doodad:0:1", "fields": { "tag": "lamp" } }, { "id": "x" }] });
        assert_eq!(fields_of(&answer), [(EntityId::doodad(0, 1), json!({ "tag": "lamp" }))]);
        assert_eq!(text(&Shown::Same(2.5)), "2.5");
        assert_eq!(text(&Shown::Same(90.0)), "90");
        assert_eq!(text(&Shown::Mixed), "—");
    }
}
