//! An NPC put in the world (step 18.4): a prefab dropped from the panel *Prefabs* into the 3D view
//! at the ground under the pointer, or placed by its sheet's button in the middle of the view; given
//! an entry of the editor's block the first time, sent to the server (its rows waiting for Apply),
//! a creature of its template made there by the module `spawns`, moved and copied as any spawn.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use uniwow_api::egui;
use uniwow_api::serde_json::json;
use uniwow_api::viewport::{Tool, ToolInput};
use uniwow_api::{Context, scene, viewport, world_db};

use crate::NpcsModule;
use crate::prefab::Prefab;
use crate::server;

/// What is dragged from the panel Prefabs: the file of a prefab.
#[derive(Clone, Debug)]
pub struct Dragged(pub PathBuf);

/// The drops into the 3D view, from the tool to the module: the prefab, where on the ground.
pub type Drops = Arc<Mutex<Vec<(PathBuf, [f32; 3])>>>;

/// The tool of the 3D view taking a prefab dropped: where the pointer meets the ground.
pub struct DropTool {
    pub drops: Drops,
    pub scene: Option<scene::Handle>,
}

impl Tool for DropTool {
    fn pointer(&mut self, input: &ToolInput<'_>, painter: &egui::Painter) {
        let response = input.response;
        let ground = |at: egui::Pos2| -> Option<[f32; 3]> {
            let ray = input.ray_at(at)?;
            let distance = self.scene.as_ref().and_then(|scene| scene.ground(&ray)).unwrap_or(30.0);
            Some(ray.at(distance).to_array())
        };
        if let (Some(dragged), Some(at)) = (response.dnd_hover_payload::<Dragged>(), response.hover_pos()) {
            let name = dragged
                .0
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            painter.circle_stroke(at, 14.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 200, 40)));
            painter.text(
                at + egui::vec2(18.0, -8.0),
                egui::Align2::LEFT_CENTER,
                format!("Place {name} here"),
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            );
        }
        if let Some(dragged) = response.dnd_release_payload::<Dragged>()
            && let Some(at) = response.hover_pos().or_else(|| response.interact_pointer_pos())
            && let Some(position) = ground(at)
            && let Ok(mut drops) = self.drops.lock()
        {
            drops.push((dragged.0.clone(), position));
        }
    }
}

impl NpcsModule {
    /// The tool of drops added to the 3D view, once.
    pub(crate) fn start_drops(&mut self, ctx: &mut Context) {
        let Some(view) = ctx.service(viewport::SERVICE) else {
            return;
        };
        view.add_tool(
            ctx.module_id(),
            Box::new(DropTool {
                drops: self.drops.clone(),
                scene: ctx.service(scene::SERVICE),
            }),
        );
    }

    /// The entries the database has and those the prefabs of the folder and those sent hold.
    fn used_entries(&self) -> Vec<u32> {
        let mut used: Vec<u32> = self.sent.prefabs.keys().copied().collect();
        for (path, _) in &self.library {
            if let Ok(prefab) = Prefab::read(path)
                && let Some(entry) = prefab.entry
            {
                used.push(entry);
            }
        }
        used
    }

    /// Whether the database has the template `entry`.
    pub(crate) fn known_entry(&self, entry: u32) -> bool {
        self.world().is_some_and(|world| {
            world
                .creatures
                .binary_search_by_key(&entry, |creature| creature.entry)
                .is_ok()
        })
    }

    /// The entry of the prefab opened, given one of the editor's block the first time, one undo
    /// entry; none said why when it cannot be.
    fn ensure_entry(&mut self, ctx: &mut Context) -> Option<u32> {
        let doc = self.doc.as_ref()?;
        if let Some(entry) = doc.prefab.entry {
            return Some(entry);
        }
        if self.world().is_none() {
            self.said =
                Some("The world database is not read yet: its entries are not known (panel World database)".to_owned());
            return None;
        }
        let used = self.used_entries();
        let Some(entry) = server::free_entry(&|entry| self.known_entry(entry), &used) else {
            self.said = Some("The editor's block of NPCs (82800-82999) is full".to_owned());
            return None;
        };
        let mut after = doc.prefab.clone();
        after.entry = Some(entry);
        self.change(format!("Give the NPC the entry {entry}"), after, ctx);
        Some(entry)
    }

    /// The prefab opened sent to the server: its rows wait for Apply, kept as it changes.
    pub(crate) fn send(&mut self, ctx: &mut Context) -> Option<u32> {
        let entry = self.ensure_entry(ctx)?;
        let prefab = self.doc.as_ref()?.prefab.clone();
        self.sent.prefabs.insert(entry, prefab);
        self.sent_generation += 1;
        Some(entry)
    }

    /// The prefab opened placed at `position`: sent, a creature of its template made there.
    pub(crate) fn place(&mut self, position: [f32; 3], ctx: &mut Context) {
        let Some(entry) = self.send(ctx) else {
            return;
        };
        let Some(prefab) = self.doc.as_ref().map(|doc| doc.prefab.clone()) else {
            return;
        };
        let call = ctx.call(
            "spawns.create_creature",
            json!({
                "entry": entry,
                "position": position,
                "orientation": 0.0,
                "name": prefab.name("enUS"),
                "display": prefab.models.first().map_or(0, |model| model.display),
                // its weapons those of its template's row, written even with empty hands
                "armed": true,
            }),
        );
        self.placing.push(call);
        if self.doc.as_ref().is_some_and(|doc| doc.unsaved()) {
            self.said = Some(format!(
                "Placed, the template {entry}: save the NPC for its file to keep its entry"
            ));
        }
    }

    /// The ground in the middle of the 3D view.
    pub(crate) fn middle_of_view(&self, ctx: &mut Context) -> Option<[f32; 3]> {
        let view = ctx.service(viewport::SERVICE)?;
        let seen = view.last_view()?;
        let ray = seen.ray([seen.size[0] / 2.0, seen.size[1] / 2.0])?;
        let distance = ctx
            .service(scene::SERVICE)
            .and_then(|scene| scene.ground(&ray))
            .unwrap_or(30.0);
        Some(ray.at(distance).to_array())
    }

    /// The drops of the 3D view taken: each prefab opened (the one opened kept when it has unsaved
    /// changes and another is dropped) and placed.
    pub(crate) fn take_drops(&mut self, ctx: &mut Context) {
        let drops: Vec<(PathBuf, [f32; 3])> = self
            .drops
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default();
        for (path, position) in drops {
            let opened = self.doc.as_ref().and_then(|doc| doc.path.clone());
            if opened.as_ref() != Some(&path) {
                if self.doc.as_ref().is_some_and(|doc| doc.unsaved()) {
                    self.said = Some("The NPC opened has unsaved changes: save it before placing another".to_owned());
                    continue;
                }
                self.open(Some(path), ctx);
            }
            self.place(position, ctx);
        }
    }

    /// The sent prefab of the one opened kept as it changes; the changes for the server given
    /// once they changed.
    pub(crate) fn give_pending(&mut self, ctx: &mut Context) {
        // The prefab opened kept sent as it changes: once sent, or once in the database (as it was
        // applied or opened, its changes since sent again).
        // A prefab opened whose entry the database has: its file as the database has it.
        if let Some(doc) = self.doc.as_ref()
            && let Some(entry) = doc.prefab.entry
            && !self.sent.prefabs.contains_key(&entry)
            && !self.written.contains_key(&entry)
            && self.known_entry(entry)
        {
            let baseline = doc.saved.clone().unwrap_or_else(|| doc.prefab.clone());
            self.written.insert(entry, baseline);
        }
        if let Some(doc) = self.doc.as_ref()
            && let Some(entry) = doc.prefab.entry
        {
            let sent = self.sent.prefabs.get(&entry);
            let written = self.written.get(&entry);
            let changed = match (sent, written) {
                (Some(sent), _) => *sent != doc.prefab,
                (None, Some(written)) => *written != doc.prefab,
                (None, None) => false,
            };
            if changed {
                self.sent.prefabs.insert(entry, doc.prefab.clone());
                self.sent_generation += 1;
            }
        }
        if self.given == Some(self.sent_generation) {
            return;
        }
        let Some(database) = ctx.service(world_db::SERVICE) else {
            return;
        };
        self.given = Some(self.sent_generation);
        ctx.set_setting(PENDING, self.sent.section());
        let names = crate::skills::named(self);
        let pending = self.sent.pending(&|entry| self.known_entry(entry), &names);
        database.set_pending(pending);
    }
}

/// The setting keeping the NPCs sent and not applied, for them to outlive the editor closed.
pub const PENDING: &str = "pending";
