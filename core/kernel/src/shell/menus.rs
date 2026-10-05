//! The menu bar and the status bar.

use super::*;

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
                if ui.button("Quit").clicked() {
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
                if history_button(ui, "Undo", &undo, self.history.undo_label(), blocked.as_deref()) {
                    actions.push(MenuAction::Undo);
                }
                if history_button(ui, "Redo", &redo, self.history.redo_label(), blocked.as_deref()) {
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
                if ui.button("Hotkey").clicked() {
                    actions.push(MenuAction::Hotkeys);
                }
                self.module_items(ui, "Edit", &mut actions);
            });
            ui.menu_button("Window", |ui| {
                for entry in &entries {
                    let mut open = self.dock.find_tab(&entry.tab).is_some();
                    let label = if entry.tab.module == KERNEL {
                        entry.title.clone()
                    } else {
                        format!("{} ({})", entry.title, entry.tab.module)
                    };
                    if ui.checkbox(&mut open, label).changed() {
                        actions.push(MenuAction::SetPanelOpen(entry.tab.clone(), open));
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

    pub(super) fn status_bar(&self, ui: &mut egui::Ui) {
        let running = self.slots.iter().filter(|s| s.state.is_running()).count();
        let others = self.slots.len() - running;
        ui.horizontal(|ui| {
            ui.label(format!("{running} modules running"));
            if others > 0 {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("{others} not running (see Modules)"),
                );
            }
            ui.separator();
            ui.label(format!("{} changes in history", self.history.done.len()));
            // The oldest group, and how many others: the Edit menu lists them all.
            let open = self.groups.list();
            if let Some((_, oldest)) = open.first() {
                ui.separator();
                let text = match open.len() {
                    1 => format!("Undo group open: {oldest}"),
                    count => format!("{count} undo groups open: {oldest} and {} more", count - 1),
                };
                ui.colored_label(ui.visuals().warn_fg_color, text);
            }
        });
    }
}

pub(super) enum MenuAction {
    Undo,
    Redo,
    CloseGroup(u64),
    SetPanelOpen(Tab, bool),
    Hotkeys,
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
