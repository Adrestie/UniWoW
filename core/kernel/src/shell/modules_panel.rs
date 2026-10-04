//! The Modules panel: every module found, its state and its commands.

use super::*;

impl Viewer<'_> {
    pub(super) fn modules_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!("Runtime {}", short(self.runtime)));
            ui.separator();
            ui.label(self.modules_dir.display().to_string());
        });
        if *self.restart_needed {
            ui.colored_label(ui.visuals().warn_fg_color, "Restart UniWoW to apply the changes.");
        }
        ui.separator();
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("modules").striped(true).num_columns(8).show(ui, |ui| {
                for header in ["On", "Module", "Kind", "Category", "Id", "Version", "State", "Commands"] {
                    ui.strong(header);
                }
                ui.end_row();
                let mut disable = None;
                for (index, slot) in self.slots.iter().enumerate() {
                    let mut enabled = !self.host.settings.disabled_modules.contains(&slot.id);
                    let toggle = ui.add_enabled(slot.manifest.is_some(), egui::Checkbox::without_text(&mut enabled));
                    if toggle.changed() {
                        if enabled {
                            self.host.settings.disabled_modules.remove(&slot.id);
                        } else {
                            self.host.settings.disabled_modules.insert(slot.id.clone());
                        }
                        self.host.settings_changed = true;
                        *self.restart_needed = true;
                    }
                    let name = ui.label(slot.name());
                    let folder = slot.folder.display().to_string();
                    match &slot.manifest {
                        Some(manifest) if !manifest.description.is_empty() => {
                            name.on_hover_text(format!("{}\n{folder}", manifest.description));
                        }
                        _ => {
                            name.on_hover_text(folder);
                        }
                    }
                    ui.label(slot.manifest.as_ref().map_or("", |m| m.kind.name()));
                    ui.label(slot.manifest.as_ref().map_or("", |m| m.category.as_str()));
                    ui.label(&slot.id);
                    ui.label(slot.manifest.as_ref().map_or("", |m| m.version.as_str()));
                    let stuck = slot
                        .compiled
                        .and_then(|module| module.activity.running_for())
                        .filter(|running| *running >= NOT_RESPONDING);
                    match stuck {
                        Some(running) if slot.state.is_running() => {
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    format!("not responding: busy for {} s", running.as_secs()),
                                );
                                // Its work never ends: disabled, its changes leave the history and
                                // Undo comes back.
                                if ui.button("Disable this module").clicked() {
                                    disable = Some((
                                        index,
                                        format!(
                                            "disabled from the Modules panel: not responding for {} s",
                                            running.as_secs()
                                        ),
                                    ));
                                }
                            });
                        }
                        _ => {
                            let color = match slot.state {
                                State::Running => egui::Color32::from_rgb(90, 170, 90),
                                State::Disabled => ui.visuals().weak_text_color(),
                                State::Ignored(_) => ui.visuals().warn_fg_color,
                                _ => ui.visuals().error_fg_color,
                            };
                            ui.colored_label(color, state_text(&slot.state));
                        }
                    }
                    let error = ui.visuals().error_fg_color;
                    ui.horizontal_wrapped(|ui| {
                        for (command, refused) in &slot.commands {
                            match refused {
                                None => ui.label(command),
                                Some(reason) => ui.colored_label(error, format!("{command} (refused: {reason})")),
                            };
                        }
                    });
                    ui.end_row();
                }
                self.failures.extend(disable);
            });
        });
    }
}

pub(super) fn state_text(state: &State) -> String {
    match state {
        State::Running => "running".to_owned(),
        State::Disabled => "disabled".to_owned(),
        State::Ignored(reason) => format!("ignored: {reason}"),
        State::Refused(reason) => format!("refused: {reason}"),
        State::Blocked(reason) => format!("not loaded: {reason}"),
        State::Failed(reason) => format!("failed: {reason}"),
    }
}

pub(super) fn short(fingerprint: Option<&str>) -> String {
    fingerprint.map_or("unavailable".to_owned(), |f| f.chars().take(12).collect())
}
