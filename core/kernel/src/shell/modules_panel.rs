//! The Modules panel: every module found, its state and its commands.

use uniwow_api::theme::{Palette, Toggle};

use super::*;

/// The colour of a module's state, and whether its dot pulses: a module running a job too long.
fn state_colour(state: &State, stuck: bool) -> (egui::Color32, bool) {
    let p = theme::palette();
    match state {
        State::Running if stuck => (p.warning, true),
        State::Running => (p.success, false),
        State::Disabled => (p.text_weak, false),
        State::Ignored(_) => (p.warning, false),
        _ => (p.error, false),
    }
}

impl Viewer<'_> {
    pub(super) fn modules_panel(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{}  Runtime {}", theme::icon::STACK, short(self.runtime)))
                    .color(p.text_weak),
            );
            ui.separator();
            ui.label(egui::RichText::new(self.modules_dir.display().to_string()).color(p.text_weak));
        });
        if *self.restart_needed {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("{}  Restart UniWoW to apply the changes.", theme::icon::WARNING),
            );
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
                    let toggle = ui.add_enabled(slot.manifest.is_some(), Toggle(&mut enabled));
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
                                let (colour, pulsing) = state_colour(&slot.state, true);
                                theme::status_dot(ui, colour, pulsing);
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
                            let (colour, pulsing) = state_colour(&slot.state, false);
                            ui.horizontal(|ui| {
                                theme::status_dot(ui, colour, pulsing);
                                let text = if matches!(slot.state, State::Running) {
                                    colour
                                } else {
                                    Palette::mix(colour, p.text, 0.25)
                                };
                                ui.colored_label(text, state_text(&slot.state));
                            });
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
