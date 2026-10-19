//! The menu bar and the status bar.

use uniwow_api::theme::{Palette, icon};

use super::*;

/// A text of a menu after its icon.
fn item(icon: &str, text: &str) -> String {
    format!("{icon}  {text}")
}

impl Shell {
    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) -> Vec<MenuAction> {
        let mut actions = Vec::new();
        let entries = self.panel_entries();
        let mut module_menus: Vec<String> = Vec::new();
        for slot in self.slots.iter().filter(|s| s.state.is_running()) {
            for item in &slot.menu_items {
                if !BUILT_IN_MENUS.contains(&item.menu.as_str()) && !module_menus.contains(&item.menu) {
                    module_menus.push(item.menu.clone());
                }
            }
        }

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                self.project_menu(ui);
                if ui.button(item(icon::QUIT, "Quit")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                self.module_items(ui, "File", &mut actions);
            });
            ui.menu_button("Edit", |ui| {
                let blocked = self.blocking_undo();
                let (undo, redo) = (
                    self.hotkeys.undo.keys().to_string(),
                    self.hotkeys.redo.keys().to_string(),
                );
                let verbs = (item(icon::UNDO, "Undo"), item(icon::REDO, "Redo"));
                if history_button(ui, &verbs.0, &undo, self.history.undo_label(), blocked.as_deref()) {
                    actions.push(MenuAction::Undo);
                }
                if history_button(ui, &verbs.1, &redo, self.history.redo_label(), blocked.as_deref()) {
                    actions.push(MenuAction::Redo);
                }
                // A group a module's thread never ended would block Undo for good.
                let open = self.groups.list();
                if !open.is_empty() {
                    ui.separator();
                }
                for (id, label) in open {
                    if ui.button(format!("Close the undo group '{label}'")).clicked() {
                        actions.push(MenuAction::CloseGroup(id));
                    }
                }
                ui.separator();
                if ui.button(item(icon::KEYBOARD, "Hotkey")).clicked() {
                    actions.push(MenuAction::Hotkeys);
                }
                if ui.button(item(icon::SLIDERS, "Settings")).clicked() {
                    actions.push(MenuAction::Settings);
                }
                self.module_items(ui, "Edit", &mut actions);
            });
            ui.menu_button("Window", |ui| {
                for entry in &entries {
                    let torn = self.is_torn(&entry.tab);
                    let mut open = torn || self.dock.find_tab(&entry.tab).is_some();
                    let mut label = if entry.tab.module == KERNEL {
                        entry.title.clone()
                    } else {
                        format!("{} ({})", entry.title, entry.tab.module)
                    };
                    if torn {
                        label = format!("{label}  {}", icon::WINDOW);
                    }
                    let response = ui.checkbox(&mut open, label);
                    if response.changed() {
                        actions.push(MenuAction::SetPanelOpen(entry.tab.clone(), open));
                    }
                    if torn {
                        response.on_hover_text("In a window of its own");
                    }
                }
                if !self.torn.is_empty() {
                    ui.separator();
                    if ui.button(item(icon::DOCK_BACK, "Dock every window back")).clicked() {
                        actions.push(MenuAction::DockAllBack);
                    }
                }
                self.module_items(ui, "Window", &mut actions);
            });
            for menu in &module_menus {
                ui.menu_button(menu.as_str(), |ui| self.module_items(ui, menu, &mut actions));
            }
            ui.menu_button("Help", |ui| {
                ui.label(format!("UniWoW {}", env!("CARGO_PKG_VERSION")));
                ui.label(format!("Runtime {}", short(self.runtime_fingerprint.as_deref())));
                self.module_items(ui, "Help", &mut actions);
            });
        });
        actions
    }

    pub(super) fn module_items(&self, ui: &mut egui::Ui, menu: &str, actions: &mut Vec<MenuAction>) {
        let mut first = BUILT_IN_MENUS.contains(&menu);
        for (index, slot) in self.slots.iter().enumerate().filter(|(_, s)| s.state.is_running()) {
            for item in slot.menu_items.iter().filter(|i| i.menu == menu) {
                if std::mem::take(&mut first) {
                    ui.separator();
                }
                if ui.button(&item.label).clicked() {
                    actions.push(MenuAction::Module(index, item.action.clone()));
                }
            }
        }
    }

    /// The state of the editor along the bottom of the window: the modules, the history, the undo
    /// groups open, then on the right the windows torn off and the jobs under way, with a spinner
    /// and a bar of their progress while they run.
    pub(super) fn status_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<MenuAction>) {
        let p = theme::palette();
        let running = self.slots.iter().filter(|s| s.state.is_running()).count();
        let others = self.slots.len() - running;
        let weak = |text: String| egui::RichText::new(text).color(p.text_weak);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            theme::status_dot(ui, p.success, false);
            ui.label(weak(format!("{running} modules running")));
            if others > 0 {
                theme::status_dot(ui, p.warning, false);
                let link = ui
                    .add(
                        egui::Label::new(egui::RichText::new(format!("{others} not running")).color(p.warning))
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text("Show the Modules panel");
                if link.clicked() {
                    actions.push(MenuAction::SetPanelOpen(Tab::new(KERNEL, "modules"), true));
                }
            }
            ui.label(weak("·".to_owned()));
            ui.label(weak(format!(
                "{}  {} changes in history",
                icon::HISTORY,
                self.history.done.len()
            )));
            // The oldest group, and how many others: the Edit menu lists them all.
            let open = self.groups.list();
            if let Some((_, oldest)) = open.first() {
                ui.label(weak("·".to_owned()));
                let text = match open.len() {
                    1 => format!("Undo group open: {oldest}"),
                    count => format!("{count} undo groups open: {oldest} and {} more", count - 1),
                };
                ui.colored_label(p.warning, text);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.jobs_status(ui);
                if !self.torn.is_empty() {
                    let count = self.torn.len();
                    let text = if count == 1 {
                        "1 window".to_owned()
                    } else {
                        format!("{count} windows")
                    };
                    ui.label(weak(format!("{}  {text}", icon::WINDOW)))
                        .on_hover_text("Panels torn off into windows of their own");
                }
            });
        });
    }

    /// The jobs under way, drawn from the right: their count with a spinner and the bar of their
    /// progress, each job told on hover; nothing moves while none runs.
    fn jobs_status(&self, ui: &mut egui::Ui) {
        let p = theme::palette();
        let jobs = self.host.pool.running();
        if jobs.is_empty() {
            ui.label(egui::RichText::new("No job running").color(Palette::alpha(p.text_weak, 0.8)));
            return;
        }
        // The share done of the jobs that tell it; none while one has told nothing yet.
        let fraction = jobs
            .iter()
            .all(|job| job.progress() > 0.0)
            .then(|| jobs.iter().map(|job| job.progress()).sum::<f32>() / jobs.len() as f32);
        let response = ui
            .scope(|ui| {
                theme::progress_bar(ui, fraction, 90.0);
                let count = if jobs.len() == 1 {
                    "1 job".to_owned()
                } else {
                    format!("{} jobs", jobs.len())
                };
                ui.label(egui::RichText::new(count).color(p.text));
                theme::spinner(ui, 14.0);
            })
            .response;
        response.on_hover_ui(|ui| {
            for job in jobs {
                ui.label(format!(
                    "{}: {} — {:.0} %, {:.1} s",
                    job.owner,
                    job.label,
                    job.progress() * 100.0,
                    job.started.elapsed().as_secs_f32()
                ));
            }
        });
    }
}

pub(super) enum MenuAction {
    Undo,
    Redo,
    CloseGroup(u64),
    SetPanelOpen(Tab, bool),
    DockAllBack,
    Hotkeys,
    Settings,
    Module(usize, String),
}

/// An Undo or Redo entry of the Edit menu, greyed with the reason when `blocked`; returns whether
/// it was clicked.
pub(super) fn history_button(
    ui: &mut egui::Ui,
    verb: &str,
    shortcut: &str,
    label: Option<String>,
    blocked: Option<&str>,
) -> bool {
    let text = match (&label, blocked) {
        (_, Some(reason)) => format!("{verb} ({reason})"),
        (Some(label), None) => format!("{verb} {label}"),
        (None, None) => verb.to_owned(),
    };
    ui.add_enabled(
        label.is_some() && blocked.is_none(),
        egui::Button::new(text).shortcut_text(shortcut),
    )
    .clicked()
}
