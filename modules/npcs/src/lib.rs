//! Non-player characters made in the editor (milestone 18): each a prefab, a file of its own
//! (`prefab`), opened from the panel *Prefabs* or the menu *NPC*, its sheet filled in the panel
//! *NPC* (`sheet`): its name and title in every language of the client, its level, kind, faction,
//! strength, movement, models, loot, roles and flags, each named and said; one undo entry a field.
//! Saved when asked; the file read again by any editor.

mod catalog;
mod client;
mod fields;
mod figure;
mod gpu;
mod prefab;
mod sheet;
mod skills;
mod smart;
mod stage;
mod texts;
mod view;
mod world;

use std::path::PathBuf;
use std::sync::Arc;

use uniwow_api::serde_json::json;
use uniwow_api::{Arrangement, Command, Context, DockArea, JobId, JobOutcome, Module, Registrar, egui, vfs};

use prefab::Prefab;

/// The prefab opened: its file, its values, those of its file, and which opening it is (an undo
/// entry of another one is not applied to it).
struct Doc {
    path: Option<PathBuf>,
    prefab: Prefab,
    saved: Option<Prefab>,
    opening: u64,
}

impl Doc {
    fn unsaved(&self) -> bool {
        self.saved.as_ref() != Some(&self.prefab)
    }
}

/// A change of the prefab opened, from and to: one undo entry.
struct SetPrefab {
    label: String,
    opening: u64,
    before: Prefab,
    after: Prefab,
}

impl SetPrefab {
    fn put(&self, module: &mut dyn std::any::Any, prefab: &Prefab) {
        let module: &mut NpcsModule = module.downcast_mut().expect("the NPCs");
        if let Some(doc) = module.doc.as_mut().filter(|doc| doc.opening == self.opening) {
            doc.prefab = prefab.clone();
            module.typing = None;
        }
    }
}

impl Command for SetPrefab {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        let after = self.after.clone();
        self.put(module, &after);
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let before = self.before.clone();
        self.put(module, &before);
    }
}

#[derive(Default)]
struct NpcsModule {
    doc: Option<Doc>,
    openings: u64,
    /// The prefabs of the folder, by path, their names; whether they were listed.
    library: Vec<(PathBuf, String)>,
    listed: bool,
    /// The language of the name and title shown.
    locale: usize,
    /// The field being typed: its key and its text, written once left.
    typing: Option<(String, String)>,
    /// The text searched among the factions.
    faction_search: String,
    said: Option<String>,
    /// Whether the NPC's panels are laid out.
    arranged: bool,
    client: Option<Arc<client::Client>>,
    client_job: Option<JobId>,
    /// The NPC in 3D, the window choosing its model, what the database tells.
    stage: stage::Stage,
    egui: Option<egui::Context>,
    /// The texts searched in the lists, by list.
    searches: std::collections::HashMap<String, String>,
    /// The skill edited, by its place.
    skill_chosen: Option<usize>,
}

impl NpcsModule {
    /// Makes `after` the prefab opened, one undo entry.
    fn change(&mut self, label: String, after: Prefab, ctx: &mut Context) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        if after != doc.prefab {
            ctx.execute(SetPrefab {
                label,
                opening: doc.opening,
                before: doc.prefab.clone(),
                after,
            });
        }
    }

    fn opened(&mut self, path: Option<PathBuf>, prefab: Prefab, saved: bool, ctx: &mut Context) {
        self.openings += 1;
        self.doc = Some(Doc {
            path,
            saved: saved.then(|| prefab.clone()),
            prefab,
            opening: self.openings,
        });
        self.typing = None;
        self.said = None;
        if !self.arranged {
            self.arrange(ctx);
        }
    }

    fn new_npc(&mut self, ctx: &mut Context) {
        self.opened(None, Prefab::default(), false, ctx);
    }

    /// Opens the prefab at `path`, or one chosen.
    fn open(&mut self, path: Option<PathBuf>, ctx: &mut Context) {
        let path = match path {
            Some(path) => path,
            None => {
                let folder = prefab::folder();
                let _ = std::fs::create_dir_all(&folder);
                let Some(path) = uniwow_api::rfd::FileDialog::new()
                    .set_title("Open an NPC")
                    .set_directory(&folder)
                    .add_filter("NPC prefab", &["json"])
                    .pick_file()
                else {
                    return;
                };
                path
            }
        };
        match Prefab::read(&path) {
            Ok(prefab) => self.opened(Some(path), prefab, true, ctx),
            Err(reason) => self.said = Some(reason),
        }
    }

    /// Saves the prefab to its file, or to one chosen (`choose`).
    fn save(&mut self, choose: bool) -> Result<(), String> {
        let Some(doc) = self.doc.as_mut() else {
            return Ok(());
        };
        let path = match doc.path.clone().filter(|_| !choose) {
            Some(path) => path,
            None => {
                let folder = prefab::folder();
                let _ = std::fs::create_dir_all(&folder);
                let Some(path) = uniwow_api::rfd::FileDialog::new()
                    .set_title("Save the NPC")
                    .set_directory(&folder)
                    .add_filter("NPC prefab", &["json"])
                    .set_file_name(doc.prefab.file_name())
                    .save_file()
                else {
                    return Ok(());
                };
                path
            }
        };
        doc.prefab.write(&path)?;
        doc.path = Some(path);
        doc.saved = Some(doc.prefab.clone());
        self.listed = false;
        Ok(())
    }

    fn save_said(&mut self, choose: bool) {
        self.said = self.save(choose).err();
    }

    /// The layout of the NPCs: the NPC in 3D in the middle, the prefabs on the left, its sheet on the
    /// right; the layout before
    /// kept for "Back to the world".
    fn arrange(&mut self, ctx: &mut Context) {
        let own = |panel: &str| ("npcs".to_owned(), panel.to_owned());
        ctx.arrange(Arrangement {
            center: vec![own("view")],
            left: vec![own("prefabs")],
            right: vec![own("npc")],
            bottom: Vec::new(),
        });
        self.arranged = true;
    }

    /// The client's tables read once its archives are open.
    fn read_client(&mut self, ctx: &mut Context) {
        if self.client.is_some() || self.client_job.is_some() {
            return;
        }
        let Some(vfs) = ctx.service(vfs::SERVICE) else {
            return;
        };
        if !matches!(vfs.state(), vfs::VfsState::Ready { .. }) {
            return;
        }
        self.client_job = Some(ctx.spawn("Read the client's factions and models", move |_| {
            client::Client::read(&vfs)
        }));
    }

    /// When nothing is open: what to do, and what each panel is for.
    fn welcome(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.heading("Non-player characters");
            ui.label("Made here, kept as files (prefabs), placed in the world as many times as you like.");
            ui.add_space(16.0);
            let big = |ui: &mut egui::Ui, text: &str| {
                ui.add_sized([260.0, 40.0], egui::Button::new(egui::RichText::new(text).size(16.0)))
            };
            if big(ui, "✚  New NPC")
                .on_hover_text("A humanoid of level 1, friendly, to start from; not saved until asked")
                .clicked()
            {
                self.new_npc(ctx);
            }
            ui.add_space(6.0);
            if big(ui, "📂  Open an NPC…")
                .on_hover_text(format!("A prefab (*{}) on the disk", prefab::EXTENSION))
                .clicked()
            {
                self.open(None, ctx);
            }
            ui.add_space(20.0);
            ui.weak(
                "Prefabs (left): the NPCs of the folder, a click opens one. NPC (middle): the sheet of the one opened.",
            );
            ui.weak(format!(
                "Saved in {} by default. Menu NPC: the same, from anywhere.",
                prefab::folder().display()
            ));
        });
    }

    /// The panel Prefabs: those of the folder, a click opens one.
    fn library_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        if !self.listed {
            self.library = prefab::listed(&prefab::folder());
            self.listed = true;
        }
        ui.horizontal(|ui| {
            if ui
                .button("✚ New")
                .on_hover_text("A new NPC, not saved until asked")
                .clicked()
            {
                self.new_npc(ctx);
            }
            if ui
                .button("📂 Open…")
                .on_hover_text("A prefab anywhere on the disk")
                .clicked()
            {
                self.open(None, ctx);
            }
            if ui
                .button("⟳")
                .on_hover_text(format!("Lists again the prefabs of {}", prefab::folder().display()))
                .clicked()
            {
                self.listed = false;
            }
        });
        ui.separator();
        if self.library.is_empty() {
            ui.weak("No prefab saved yet.");
        }
        let current = self.doc.as_ref().and_then(|doc| doc.path.clone());
        let mut chosen = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (path, name) in &self.library {
                let shown = if name.is_empty() { "(no name)" } else { name.as_str() };
                if ui
                    .selectable_label(current.as_ref() == Some(path), shown)
                    .on_hover_text(path.display().to_string())
                    .clicked()
                {
                    chosen = Some(path.clone());
                }
            }
        });
        if let Some(path) = chosen {
            if self.doc.as_ref().is_some_and(Doc::unsaved) {
                self.said = Some("The NPC opened has unsaved changes: save it first (menu NPC > Save)".to_owned());
            } else {
                self.open(Some(path), ctx);
            }
        }
    }
}

impl Module for NpcsModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("npc", "NPC", DockArea::Right)
            .panel("view", "NPC view", DockArea::Right)
            .panel("prefabs", "Prefabs", DockArea::Left);
        reg.menu_item("NPC", "New NPC", "new")
            .menu_item("NPC", "Open an NPC…", "open")
            .menu_item("NPC", "Save", "save")
            .menu_item("NPC", "Save as…", "save_as")
            .menu_item("NPC", "Show the NPC panels", "focus")
            .menu_item("NPC", "Back to the world", "world");
        reg.command(
            "npcs.open",
            "Opens the NPC prefab at 'path' in the panel NPC.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
            json!({ "type": "object" }),
        );
    }

    fn init(&mut self, ctx: &mut Context) {
        // checks without the mouse: a prefab opened at the start
        if let Some(path) = std::env::var_os("UNIWOW_NPCS_OPEN") {
            self.open(Some(PathBuf::from(path)), ctx);
        }
        self.start_stage(ctx);
    }

    fn windows_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        self.tick_stage(egui, ctx);
    }

    fn panel_ui(&mut self, panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        self.read_client(ctx);
        if let Some(said) = self.said.clone() {
            ui.horizontal(|ui| {
                ui.colored_label(ui.visuals().warn_fg_color, &said);
                if ui.small_button("✖").on_hover_text("Hides the message").clicked() {
                    self.said = None;
                }
            });
        }
        match panel {
            "prefabs" => self.library_ui(ui, ctx),
            "view" => self.view_ui(ui, ctx),
            _ if self.doc.is_none() => self.welcome(ui, ctx),
            _ => self.sheet_ui(ui, ctx),
        }
    }

    fn on_menu(&mut self, action: &str, ctx: &mut Context) {
        match action {
            "new" => self.new_npc(ctx),
            "open" => self.open(None, ctx),
            "save" => self.save_said(false),
            "save_as" => self.save_said(true),
            "focus" => self.arrange(ctx),
            "world" => {
                ctx.restore_arrangement();
                self.arranged = false;
            }
            _ => {}
        }
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        let mut outcome = Some(outcome);
        if self.stage_job(job, &mut outcome) {
            return;
        }
        let outcome = outcome.expect("not taken");
        if self.client_job == Some(job) {
            self.client_job = None;
            if let Some(client) = outcome.take::<client::Client>() {
                uniwow_api::log::info!(
                    "the client's tables of the NPCs: {} factions, {} models of displays",
                    client.factions.len(),
                    client.models.len()
                );
                self.client = Some(Arc::new(client));
            }
        }
    }

    fn on_command(
        &mut self,
        name: &str,
        arguments: uniwow_api::serde_json::Value,
        ctx: &mut Context,
    ) -> Result<uniwow_api::serde_json::Value, String> {
        match name {
            "npcs.open" => {
                let path = arguments["path"].as_str().ok_or("'path' is the file of a prefab")?;
                let prefab = Prefab::read(std::path::Path::new(path))?;
                self.opened(Some(PathBuf::from(path)), prefab, true, ctx);
                ctx.show_panel("npc");
                Ok(json!({}))
            }
            other => Err(format!("'{other}' is not handled")),
        }
    }

    fn unsaved(&self) -> Vec<String> {
        match &self.doc {
            Some(doc) if doc.unsaved() => vec![format!("NPC '{}'", doc.prefab.name("enUS"))],
            _ => Vec::new(),
        }
    }

    fn save_unsaved(&mut self, _ctx: &mut Context) -> Result<(), String> {
        self.save(false)
    }
}

uniwow_api::export_module!(NpcsModule::default());

#[cfg(test)]
mod tests {
    /// The controls of the panels, as their code makes them.
    const CONTROLS: [&str; 9] = [
        ".button(",
        "Button::new(",
        "selectable_label(",
        "selectable_value(",
        "ComboBox::",
        "small_button(",
        "checkbox(",
        "Sense::click",
        "TextEdit::",
    ];

    /// egui keeps no tooltip to be asked back: each control is looked for in the code, a tooltip
    /// within the lines that make it, else the test fails and names it.
    #[test]
    fn every_control_of_the_npcs_says_what_it_is() {
        for (file, code) in [
            ("lib.rs", include_str!("lib.rs")),
            ("sheet.rs", include_str!("sheet.rs")),
            ("stage.rs", include_str!("stage.rs")),
            ("skills.rs", include_str!("skills.rs")),
            ("texts.rs", include_str!("texts.rs")),
        ] {
            let lines: Vec<&str> = code.lines().collect();
            let end = lines
                .iter()
                .position(|line| line.starts_with("#[cfg(test)]"))
                .unwrap_or(lines.len());
            for (at, line) in lines[..end].iter().enumerate() {
                if !CONTROLS.iter().any(|control| line.contains(control)) || line.trim_start().starts_with("//") {
                    continue;
                }
                // The text fields pass through `typed`, whose tooltip is required.
                if line.contains("TextEdit::")
                    && lines[at.saturating_sub(14)..at]
                        .iter()
                        .any(|line| line.contains("fn typed("))
                {
                    continue;
                }
                let said = lines[at..(at + 25).min(end)]
                    .iter()
                    .any(|line| line.contains("on_hover_text") || line.contains("on_hover_ui"));
                assert!(said, "{file}:{}: a control without a tooltip: {}", at + 1, line.trim());
            }
        }
    }
}
