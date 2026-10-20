//! The window *Hierarchy* (milestone 15, step 15.4), left of the 3D view: the entities of the scene
//! within its scope by their places (*Main*: the map, its zones and subzones, the terrain and the
//! water of each, the buildings with their rooms and doodads, the doodads), and the live world beside
//! it. Its tree is made by a job from the snapshot of the registry, again when it changes; only the
//! rows in sight are drawn. The selection is shared with the 3D view both ways; a search and a filter
//! of kinds; a double click frames an entity; each row's menu renames it, locks or unlocks it (a
//! place locking what lies in it) and frames it.

#[cfg(test)]
mod tests;
mod tree;
mod views;
mod zones;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use uniwow_api::scene::{self, EntityId, Kind, Select};
use uniwow_api::serde_json::json;
use uniwow_api::{Context, DockArea, JobId, JobOutcome, Module, Registrar, egui, log};

use tree::{Filter, Tree};

/// The height of a row, in points.
const ROW: f32 = 20.0;
/// How far a level is indented, in points.
const INDENT: f32 = 14.0;

/// The kinds the filter offers.
const KINDS: [(&str, &[Kind]); 7] = [
    ("Every kind", &[]),
    ("Terrain", &[Kind::Chunk]),
    ("Water", &[Kind::Water]),
    ("Buildings", &[Kind::Building, Kind::Room, Kind::BuildingDoodad]),
    ("Doodads", &[Kind::Doodad]),
    ("Live", &[Kind::Live]),
    ("Lights", &[Kind::Light, Kind::LightZone]),
];

/// How high over the ground of the middle of a zone the camera is put to show it, in yards.
const CAMERA_HEIGHT: f32 = 30.0;

/// The least time between two trees made for changes of the registry alone.
const REBUILD_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// What a tree was made for: the generation of the registry, the filter, the names given and the
/// name of the map.
#[derive(Clone, Debug, PartialEq)]
struct Made {
    generation: u64,
    filter: Filter,
    names: usize,
    groups: u64,
    map: String,
    map_id: Option<u32>,
    known: u64,
}

#[derive(Default)]
struct HierarchyModule {
    scene: Option<scene::Handle>,
    /// The job reading the ground of the middle of the zone framed, with that middle and the
    /// height to fall back on.
    grounding: Option<(JobId, [f32; 2], Option<f32>)>,
    /// Why the last zone could not be framed.
    said: Option<String>,
    /// The zone chosen in the tree, framed by F; the extents of the zones of the map, read once a
    /// map by a job.
    zone: Option<u32>,
    extents: Arc<zones::Extents>,
    extents_for: Option<String>,
    extents_job: Option<JobId>,
    /// When the tree was asked last.
    built: Option<std::time::Instant>,
    /// What the objects `HierarchyView` are drawn from.
    views: Arc<views::Shared>,
    /// The colours of the notes of the editor by entity, and their number as `notes` last gave them.
    noted: HashMap<EntityId, egui::Color32>,
    notes_generation: Option<u64>,
    tree: Arc<Tree>,
    made: Option<Made>,
    job: Option<(JobId, Made)>,
    /// The nodes unfolded, by their key; the rows shown, made again when the tree or the folds
    /// change.
    unfolded: HashSet<String>,
    rows: Vec<(u32, u16)>,
    folded: bool,
    text: String,
    kinds: usize,
    view: tree::View,
    /// The active entity the tree last unfolded to, the row to bring in sight, the row of the last
    /// click for Shift.
    followed: Option<EntityId>,
    scroll_to: Option<usize>,
    anchor: Option<usize>,
    /// The entity being renamed and its name being typed.
    renaming: Option<(EntityId, String)>,
}

impl HierarchyModule {
    /// The tree made again by a job when what it is made of changed, one job at a time.
    fn steer(&mut self, ctx: &mut Context) {
        let Some(scene) = self.scene.clone() else {
            return;
        };
        let snapshot = scene.snapshot();
        let selection = scene.selection();
        let names = scene.names();
        let shown = ctx.editor().call("terrain.map", json!({})).ok();
        let map = shown
            .as_ref()
            .and_then(|map| Some(map.get("name")?.as_str()?.to_owned()))
            .unwrap_or_default();
        let map_id = shown
            .as_ref()
            .and_then(|map| map.get("id")?.as_u64())
            .map(|id| id as u32);
        let directory = shown
            .as_ref()
            .and_then(|map| Some(map.get("directory")?.as_str()?.to_owned()));
        if directory != self.extents_for
            && let (Some(folder), Some(formats)) = (directory.clone(), ctx.service(uniwow_api::formats::SERVICE))
            && !snapshot.areas.is_empty()
        {
            self.extents_for = directory;
            self.extents = Arc::default();
            self.zone = None;
            let areas = snapshot.areas.clone();
            if let Some(job) = self.extents_job.take() {
                ctx.cancel(job);
            }
            self.extents_job = Some(ctx.spawn("Read the extents of the zones of the map", move |job| {
                let tiles = zones::read(&*formats, &folder, || job.is_cancelled());
                zones::extents(&tiles, &areas)
            }));
        }
        let known = scene.known();
        let kept: Vec<EntityId> = selection
            .ids
            .iter()
            .filter(|id| !self.tree.by_entity.contains_key(id))
            .copied()
            .collect();
        let made = Made {
            generation: snapshot.generation,
            filter: Filter {
                view: self.view,
                text: self.text.trim().to_lowercase(),
                kinds: KINDS[self.kinds].1.to_vec(),
                // The places view whole, the map's every zone; the others about the camera.
                scope: if self.view == tree::View::Main {
                    None
                } else {
                    scene.scope()
                },
                kept: if kept.is_empty() {
                    self.made
                        .as_ref()
                        .map(|made| made.filter.kept.clone())
                        .unwrap_or_default()
                } else {
                    selection.ids.clone()
                },
            },
            names: Arc::as_ptr(&names) as usize,
            groups: scene.groups().generation,
            map,
            map_id,
            known: known.generation,
        };
        if self.made.as_ref() == Some(&made) || self.job.as_ref().is_some_and(|(_, job)| *job == made) {
            return;
        }
        // The registry alone changed, as the live world does many times a second: the tree made
        // again a second after the last at most, not at each change.
        let only_registry = self.made.as_ref().is_some_and(|before| {
            Made {
                generation: before.generation,
                ..made.clone()
            } == *before
        });
        if only_registry && self.built.is_some_and(|built| built.elapsed() < REBUILD_EVERY) {
            return;
        }
        self.built = Some(std::time::Instant::now());
        if let Some((job, _)) = self.job.take() {
            ctx.cancel(job);
        }
        let (filter, map) = (made.filter.clone(), made.map.clone());
        let groups = scene.groups();
        let job = ctx.spawn("Make the tree of the Hierarchy", move |job| {
            let tree = tree::build(&snapshot, &filter, &names, &map, &groups, &known, map_id);
            (!job.is_cancelled()).then_some(tree)
        });
        self.job = Some((job, made));
    }

    /// Follows the selection made elsewhere: the path to the active entity unfolded and its row
    /// brought in sight.
    fn follow(&mut self, active: Option<EntityId>) {
        if active == self.followed {
            return;
        }
        let Some(node) = active.and_then(|id| self.tree.by_entity.get(&id).copied()) else {
            return;
        };
        self.followed = active;
        let path = self.tree.path(node);
        for at in &path[..path.len() - 1] {
            self.unfolded.insert(self.tree.nodes[*at as usize].key.clone());
        }
        self.folded = true;
        self.rows = self.tree.rows(&self.unfolded);
        self.scroll_to = self.rows.iter().position(|(row, _)| *row == node);
    }

    /// The places of the entities the tree lists under the zone `zone`, its subzones' with them:
    /// held by the scene, or known far from the camera.
    fn places_under(&self, zone: u32) -> Vec<[f32; 3]> {
        let Some(scene) = &self.scene else {
            return Vec::new();
        };
        let key = format!("area:{zone}");
        let Some(start) = self.tree.nodes.iter().position(|node| node.key == key) else {
            return Vec::new();
        };
        let snapshot = scene.snapshot();
        let (mut out, mut stack) = (Vec::new(), vec![start as u32]);
        while let Some(at) = stack.pop() {
            let node = &self.tree.nodes[at as usize];
            if let Some(id) = node.entity {
                let place = snapshot
                    .get(&id)
                    .map(|entity| entity.origin())
                    .or_else(|| self.tree.known.get(&id).copied());
                out.extend(place);
            }
            stack.extend(node.children.iter().copied());
        }
        out
    }

    /// Frames the zone chosen in the 3D view: the extent of its chunks, else that of the entities
    /// listed under it (a zone of the inside of buildings, which no chunk names); its height that
    /// of those entities, else 0 to 150 yards. Whether it could.
    fn frame_zone(&mut self, ctx: &mut Context) {
        let Some(zone) = self.zone else {
            return;
        };
        let places = self.places_under(zone);
        let from_places = || {
            let first = places.first()?;
            let mut extent = [[first[0], first[1]], [first[0], first[1]]];
            for at in &places {
                for axis in 0..2 {
                    extent[0][axis] = extent[0][axis].min(at[axis]);
                    extent[1][axis] = extent[1][axis].max(at[axis]);
                }
            }
            Some(extent)
        };
        let Some([low, high]) = self.extents.get(&zone).copied().or_else(from_places) else {
            self.said = Some(if self.extents_job.is_some() {
                "The extents of the zones are still being read: F again in a moment.".to_owned()
            } else {
                "This zone has no chunk of terrain and nothing listed in it: nothing to frame.".to_owned()
            });
            return;
        };
        // The camera at the middle of the zone, 30 yards over its ground, read from the tile
        // there by a job; else over the lowest of what the zone holds.
        let middle = [(low[0] + high[0]) / 2.0, (low[1] + high[1]) / 2.0];
        let inside = |at: &&[f32; 3]| (low[0]..=high[0]).contains(&at[0]) && (low[1]..=high[1]).contains(&at[1]);
        let lowest = places.iter().filter(inside).map(|at| at[2]).reduce(f32::min);
        let (Some(folder), Some(formats)) = (self.extents_for.clone(), ctx.service(uniwow_api::formats::SERVICE))
        else {
            self.place_camera(middle, lowest, ctx);
            return;
        };
        let job = ctx.spawn("Read the ground of the middle of a zone", move |_| -> Option<f32> {
            let (x, y) = zones::tile_of(middle)?;
            let tile = formats.tile(&folder, x, y).ok()??;
            zones::ground_at(&tile, x, y, middle)
        });
        self.grounding = Some((job, middle, lowest));
    }

    /// The camera put `CAMERA_HEIGHT` yards over `ground` at `middle`, looking ahead as it looked,
    /// a little down.
    fn place_camera(&mut self, middle: [f32; 2], ground: Option<f32>, ctx: &mut Context) {
        let ground = ground.unwrap_or(0.0);
        let position = [middle[0], middle[1], ground + CAMERA_HEIGHT];
        // The direction it looked, flat.
        let heading = ctx
            .editor()
            .call("viewport.camera", json!({}))
            .ok()
            .and_then(|camera| {
                let at = |key: &str, axis: usize| camera[key][axis].as_f64().map(|value| value as f32);
                let (dx, dy) = (
                    at("target", 0)? - at("position", 0)?,
                    at("target", 1)? - at("position", 1)?,
                );
                let length = (dx * dx + dy * dy).sqrt();
                (length > 1e-3).then(|| [dx / length, dy / length])
            })
            .unwrap_or([1.0, 0.0]);
        let target = [
            position[0] + heading[0] * 100.0,
            position[1] + heading[1] * 100.0,
            ground,
        ];
        self.said = match ctx
            .editor()
            .call("viewport.look_at", json!({ "position": position, "target": target }))
        {
            Ok(_) => None,
            Err(reason) => Some(format!("The zone is not framed: {reason}")),
        };
    }

    /// Frames the entity `id` in the 3D view: one far from the camera by its known place, which
    /// brings its tiles.
    fn frame(&self, id: EntityId, ctx: &mut Context) {
        let held = self.scene.as_ref().and_then(|scene| {
            let snapshot = scene.snapshot();
            snapshot.get(&id).map(|entity| entity.bounds())
        });
        let known = || {
            self.tree.known.get(&id).map(|at| {
                [
                    [at[0] - 2.0, at[1] - 2.0, at[2]],
                    [at[0] + 2.0, at[1] + 2.0, at[2] + 4.0],
                ]
            })
        };
        let Some(entity) = held.or_else(known) else {
            return;
        };
        let [low, high] = entity;
        if let Err(reason) = ctx.editor().call("viewport.frame", json!({ "min": low, "max": high })) {
            log::warn!("{id} is not framed: {reason}");
        }
    }

    /// The prefabs of the project, each placed on the ground in the middle of the view by a click.
    fn prefabs_ui(&self, ui: &mut egui::Ui, scene: &scene::Handle, ctx: &mut Context) {
        let groups = scene.groups();
        if groups.prefabs.is_empty() {
            return;
        }
        egui::CollapsingHeader::new(format!("Prefabs ({})", groups.prefabs.len()))
            .id_salt("hierarchy prefabs")
            .show(ui, |ui| {
                for prefab in &groups.prefabs {
                    ui.horizontal(|ui| {
                        ui.label(format!("{} ({} objects)", prefab.name, prefab.objects.len()));
                        if ui
                            .small_button("Place")
                            .on_hover_text("An instance on the ground in the middle of the 3D view")
                            .clicked()
                            && let Some(at) = self.middle_of_view(scene, ctx)
                        {
                            ctx.call("scene.place_prefab", json!({ "prefab": prefab.id, "position": at }));
                        }
                    });
                }
            });
    }

    /// The point of the ground in the middle of the 3D view, or 30 yards ahead of the eye.
    fn middle_of_view(&self, scene: &scene::Handle, ctx: &mut Context) -> Option<[f32; 3]> {
        let seen = ctx.service(uniwow_api::viewport::SERVICE)?.last_view()?;
        let ray = seen.ray([seen.size[0] / 2.0, seen.size[1] / 2.0])?;
        Some(ray.at(scene.ground(&ray).unwrap_or(30.0)).to_array())
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (view, label, hint) in [
                (tree::View::Main, "Main", "By the places of the game"),
                (tree::View::Files, "Files", "By the tiles whose files store them"),
                (tree::View::ByType, "By type", "A list a kind"),
                (
                    tree::View::Server,
                    "Server",
                    "The spawns of the world database by their template",
                ),
            ] {
                ui.selectable_value(&mut self.view, view, label).on_hover_text(hint);
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.text)
                    .hint_text("Search")
                    .desired_width(ui.available_width() - 130.0),
            );
            egui::ComboBox::from_id_salt("hierarchy kinds")
                .width(110.0)
                .selected_text(KINDS[self.kinds].0)
                .show_ui(ui, |ui| {
                    for (index, (name, _)) in KINDS.iter().enumerate() {
                        ui.selectable_value(&mut self.kinds, index, *name);
                    }
                });
        });
        let scope = self.made.as_ref().and_then(|made| made.filter.scope);
        let building = if self.job.is_some() { ", made again…" } else { "" };
        ui.label(
            egui::RichText::new(match scope {
                Some((_, _, tiles)) => format!("{} entities within {tiles} tiles{building}", self.tree.entities),
                None => format!("{} entities{building}", self.tree.entities),
            })
            .small()
            .weak(),
        );
    }
}

impl Module for HierarchyModule {
    fn register(&mut self, reg: &mut Registrar) {
        let service: Arc<dyn uniwow_api::scene_view::SceneView> = Arc::new(views::Views::new(self.views.clone()));
        reg.panel("hierarchy", "Hierarchy", DockArea::Left)
            .provide(uniwow_api::scene_view::HIERARCHY, service);
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // The tree kept made while an object HierarchyView shows it, the window hidden.
        if self.views.drawn.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.steer(ctx);
            self.read_notes(ctx);
        }
    }

    fn init(&mut self, ctx: &mut Context) {
        self.scene = ctx.service(scene::SERVICE);
        if let Some(scene) = &self.scene {
            let _ = self.views.scene.set(scene.clone());
        }
        self.unfolded.insert("map".to_owned());
        self.unfolded.insert("live".to_owned());
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        // The panel is drawn by `HierarchyModule::ui` below.
        self.ui(ui, ctx);
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if let Some((waited, middle, lowest)) = self.grounding
            && waited == job
        {
            self.grounding = None;
            let ground = outcome.take::<Option<f32>>().flatten().or(lowest);
            self.place_camera(middle, ground, ctx);
            return;
        }
        if self.extents_job == Some(job) {
            self.extents_job = None;
            if let Some(extents) = outcome.take::<zones::Extents>() {
                self.extents = Arc::new(extents);
            }
            return;
        }
        if self.job.as_ref().is_none_or(|(running, _)| *running != job) {
            return;
        }
        let Some((_, made)) = self.job.take() else {
            return;
        };
        match outcome {
            JobOutcome::Panicked(message) => log::error!("the tree of the Hierarchy failed: {message}"),
            outcome => {
                if let Some(Some(tree)) = outcome.take::<Option<Tree>>() {
                    self.tree = Arc::new(tree);
                    *views::lock(&self.views.tree) = self.tree.clone();
                    self.made = Some(made);
                    self.folded = true;
                }
            }
        }
    }
}

impl HierarchyModule {
    /// The colours of the notes of the editor, when `notes` runs and they changed.
    fn read_notes(&mut self, ctx: &mut Context) {
        let Ok(answer) = ctx
            .editor()
            .call("notes.colours", json!({ "generation": self.notes_generation }))
        else {
            self.noted.clear();
            self.notes_generation = None;
            return;
        };
        if let Some(colours) = answer["colours"].as_object() {
            self.noted = noted_colours(colours);
            *views::lock(&self.views.noted) = Arc::new(self.noted.clone());
        }
        self.notes_generation = answer["generation"].as_u64();
    }

    fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(scene) = self.scene.clone() else {
            ui.label("No scene: the module scene is not running.");
            return;
        };
        self.steer(ctx);
        self.read_notes(ctx);
        let selection = scene.selection();
        self.follow(selection.active());
        if self.folded {
            self.folded = false;
            self.rows = self.tree.rows(&self.unfolded);
        }
        self.header(ui);
        if let Some(said) = &self.said {
            ui.colored_label(ui.visuals().warn_fg_color, said);
        }
        let selected: Vec<String> = selection.ids.iter().map(ToString::to_string).collect();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!selected.is_empty(), egui::Button::new("Group the selection"))
                .on_hover_text("A group of the entities selected, moved as one")
                .clicked()
            {
                ctx.call("scene.group", json!({ "ids": selected }));
            }
        });
        self.prefabs_ui(ui, &scene, ctx);
        ui.separator();
        let locks = scene.locks();
        let snapshot = scene.snapshot();
        let tree = self.tree.clone();
        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        if let Some(row) = self.scroll_to.take() {
            area = area.vertical_scroll_offset((row as f32 * ROW - 3.0 * ROW).max(0.0));
        }
        let mut clicked: Option<(usize, egui::Modifiers)> = None;
        let mut toggled: Option<u32> = None;
        let mut framed: Option<EntityId> = None;
        let mut locking: Option<(Vec<EntityId>, Vec<u32>, bool)> = None;
        let mut renamed: Option<(EntityId, Option<String>)> = None;
        let mut asked: Vec<(&str, uniwow_api::serde_json::Value)> = Vec::new();
        let groups = scene.groups();
        area.show_rows(ui, ROW, self.rows.len(), |ui, range| {
            for index in range {
                let (node_index, depth) = self.rows[index];
                let node = &tree.nodes[node_index as usize];
                ui.horizontal(|ui| {
                    ui.set_height(ROW);
                    ui.add_space(f32::from(depth) * INDENT);
                    if node.children.is_empty() {
                        ui.add_space(18.0);
                    } else {
                        let open = self.unfolded.contains(&node.key);
                        if ui.small_button(if open { "▼" } else { "▶" }).clicked() {
                            toggled = Some(node_index);
                        }
                    }
                    let locked = node.entity.is_some_and(|id| locks.entities.contains(&id))
                        || node.area.is_some_and(|area| locks.areas.contains(&area));
                    if let (Some(id), Some((renaming, text))) = (node.entity, self.renaming.as_mut())
                        && id == *renaming
                    {
                        let edit = ui.text_edit_singleline(text);
                        edit.request_focus();
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            renamed = Some((id, Some(text.clone())));
                        } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) || edit.lost_focus() {
                            self.renaming = None;
                        }
                        return;
                    }
                    let selected = node.entity.is_some_and(|id| selection.contains(&id))
                        || (node.entity.is_none() && node.area.is_some() && node.area == self.zone);
                    let label = if locked {
                        format!("🔒 {}", node.label)
                    } else {
                        node.label.clone()
                    };
                    // A spawn of the database the world as it runs shows now.
                    let live = node
                        .entity
                        .is_some_and(|id| id.kind != Kind::Live && snapshot.live(&id).is_some());
                    let label = if live { format!("{label} ●") } else { label };
                    let mut text = egui::RichText::new(label);
                    if let Some(colour) = node.entity.and_then(|id| self.noted.get(&id)) {
                        text = text.color(*colour);
                    }
                    if node.entity.is_none() {
                        text = text.strong();
                    }
                    let response = ui.selectable_label(selected, text);
                    if response.clicked() {
                        clicked = Some((index, ui.input(|i| i.modifiers)));
                        // One far from the camera: gone to, so that its tiles are loaded.
                        if let Some(id) = node.entity.filter(|id| tree.known.contains_key(id)) {
                            framed = Some(id);
                        }
                    }
                    if response.double_clicked() {
                        match node.entity {
                            Some(id) => framed = Some(id),
                            None => toggled = Some(node_index),
                        }
                    }
                    response.context_menu(|ui| {
                        if let Some(id) = node.entity.filter(|id| id.kind == Kind::Group) {
                            for (label, command) in [
                                ("Ungroup", "scene.ungroup"),
                                ("Make a prefab", "scene.prefab"),
                                ("Apply to its prefab", "scene.apply"),
                                ("Revert to its prefab", "scene.revert"),
                                ("Add the selection", "scene.group_add"),
                            ] {
                                if ui.button(label).clicked() {
                                    let ids: Vec<String> = selection.ids.iter().map(ToString::to_string).collect();
                                    asked.push((command, json!({ "group": id.to_string(), "ids": ids })));
                                    ui.close();
                                }
                            }
                            ui.separator();
                        }
                        if let Some(id) = node.entity
                            && let Some(group) = groups.of(&id)
                            && ui.button("Take out of its group").clicked()
                        {
                            asked.push((
                                "scene.group_remove",
                                json!({ "group": EntityId::group(group.id).to_string(), "ids": [id.to_string()] }),
                            ));
                            ui.close();
                        }
                        if let Some(id) = node.entity {
                            if ui.button("Rename").clicked() {
                                self.renaming = Some((id, node.label.clone()));
                                ui.close();
                            }
                            if ui.button("Default name").clicked() {
                                renamed = Some((id, None));
                                ui.close();
                            }
                            if ui.button("Frame").clicked() {
                                framed = Some(id);
                                ui.close();
                            }
                        }
                        let (ids, areas) = match (node.entity, node.area) {
                            (Some(id), _) => (vec![id], Vec::new()),
                            (None, Some(area)) => (Vec::new(), vec![area]),
                            _ => (Vec::new(), Vec::new()),
                        };
                        if (!ids.is_empty() || !areas.is_empty())
                            && ui.button(if locked { "Unlock" } else { "Lock" }).clicked()
                        {
                            locking = Some((ids, areas, !locked));
                            ui.close();
                        }
                    });
                });
            }
        });
        if let Some(node) = toggled {
            let key = tree.nodes[node as usize].key.clone();
            if !self.unfolded.remove(&key) {
                self.unfolded.insert(key);
            }
            self.folded = true;
        }
        if let Some((index, modifiers)) = clicked {
            let entity = |row: usize| tree.nodes[self.rows[row].0 as usize].entity;
            match (modifiers.shift, self.anchor, entity(index)) {
                (true, Some(anchor), _) if anchor < self.rows.len() => {
                    let (from, to) = (anchor.min(index), anchor.max(index));
                    let ids: Vec<EntityId> = (from..=to).filter_map(entity).collect();
                    scene.select(if modifiers.command {
                        Select::Add(ids)
                    } else {
                        Select::Set(ids)
                    });
                }
                (_, _, Some(id)) if modifiers.command => {
                    self.zone = None;
                    scene.select(Select::Toggle(vec![id]))
                }
                (_, _, Some(id)) => {
                    self.zone = None;
                    scene.select(Select::Set(vec![id]))
                }
                // A zone chosen: F frames it; the entities selected let go, for F to be the zone's.
                (_, _, None)
                    if tree.nodes[self.rows[index].0 as usize]
                        .area
                        .is_some_and(|area| area != 0) =>
                {
                    self.zone = tree.nodes[self.rows[index].0 as usize].area;
                    self.said = None;
                    scene.select(Select::Clear);
                }
                (_, _, None) => {
                    let key = tree.nodes[self.rows[index].0 as usize].key.clone();
                    if !self.unfolded.remove(&key) {
                        self.unfolded.insert(key);
                    }
                    self.folded = true;
                }
            }
            self.anchor = Some(index);
            // A choice made here is not followed again.
            self.followed = scene.selection().active();
        }
        if let Some(id) = framed {
            self.frame(id, ctx);
        }
        // F frames the zone chosen, no field being typed in.
        let typing = ui.ctx().egui_wants_keyboard_input();
        if !typing
            && self.zone.is_some()
            && scene.selection().ids.is_empty()
            && ui.input(|i| i.key_pressed(egui::Key::F))
        {
            self.frame_zone(ctx);
        }
        if let Some((ids, areas, locked)) = locking {
            scene.set_locked(&ids, &areas, locked);
        }
        if let Some((id, name)) = renamed {
            scene.rename(id, name);
            self.renaming = None;
        }
        for (command, arguments) in asked {
            ctx.call(command, arguments);
        }
    }
}

uniwow_api::export_module!(HierarchyModule::default());

/// The colours of `notes.colours`, `#rrggbb` by id.
fn noted_colours(
    colours: &uniwow_api::serde_json::Map<String, uniwow_api::serde_json::Value>,
) -> HashMap<EntityId, egui::Color32> {
    colours
        .iter()
        .filter_map(|(id, colour)| {
            let hex = colour.as_str()?.strip_prefix('#')?;
            let at = |from: usize| u8::from_str_radix(hex.get(from..from + 2)?, 16).ok();
            Some((id.parse().ok()?, egui::Color32::from_rgb(at(0)?, at(2)?, at(4)?)))
        })
        .collect()
}
