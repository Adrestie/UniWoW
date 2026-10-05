//! The window *Edit > Hotkey*: the hotkeys of the kernel and of the running modules, by module. A
//! click on the keys of one, then the new keys pressed, binds it to them; Escape cancels.

use uniwow_api::hotkey::{self, HotkeyKind, Keys};

use super::*;
use crate::hotkeys::KERNEL;

#[derive(Default)]
pub(super) struct HotkeyWindow {
    pub open: bool,
    /// The hotkey whose new keys are awaited, and the modifiers held since.
    pub awaiting: Option<(usize, Keys)>,
}

/// The modifiers of both.
fn union(a: Keys, b: Keys) -> Keys {
    Keys {
        ctrl: a.ctrl || b.ctrl,
        shift: a.shift || b.shift,
        alt: a.alt || b.alt,
        key: None,
    }
}

impl Shell {
    /// While new keys are awaited, takes the keys pressed this frame before anything reads them,
    /// and binds the hotkey once they are known: a key, with the modifiers held, or for a hotkey
    /// held, modifiers alone once released. Every hotkey is suspended meanwhile.
    pub(super) fn await_keys(&mut self, ctx: &egui::Context) {
        if let Some((index, held)) = self.hotkey_window.awaiting {
            let (pressed, modifiers) = ctx.input_mut(|input| {
                let mut pressed = None;
                input.events.retain(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: down,
                        modifiers,
                        ..
                    } => {
                        if *down && pressed.is_none() {
                            pressed = Some((*key, *modifiers));
                        }
                        false
                    }
                    egui::Event::Text(_) => false,
                    _ => true,
                });
                (pressed, input.modifiers)
            });
            let entry = &self.hotkeys.entries[index];
            let now = Keys::of_modifiers(modifiers);
            let chosen = match pressed {
                Some((egui::Key::Escape, with)) if Keys::of_modifiers(with).is_none() => {
                    self.hotkey_window.awaiting = None;
                    None
                }
                Some((key, with)) => Some(Keys {
                    key: Some(key),
                    ..Keys::of_modifiers(with)
                }),
                None if now.is_none() && !held.is_none() && entry.hotkey.kind() == HotkeyKind::Hold => Some(held),
                None => {
                    let held = if now.is_none() { Keys::NONE } else { union(held, now) };
                    self.hotkey_window.awaiting = Some((index, held));
                    None
                }
            };
            if let Some(keys) = chosen {
                self.hotkeys.bind(index, keys, &mut self.host.settings.hotkeys);
                self.host.settings_changed = true;
                log::info!("hotkey '{}' bound to {keys}", entry.id());
                self.hotkey_window.awaiting = None;
            }
        }
        hotkey::suspend(ctx, self.hotkey_window.awaiting.is_some());
    }

    /// Draws the window when it is open.
    pub(super) fn hotkey_window(&mut self, ctx: &egui::Context) {
        if !self.hotkey_window.open {
            self.hotkey_window.awaiting = None;
            return;
        }
        let running = self.running_ids();
        let is_running = |owner: &str| owner == KERNEL || running.contains(owner);
        let mut owners: Vec<(String, String)> = vec![(KERNEL.to_owned(), "Editor".to_owned())];
        owners.extend(
            self.slots
                .iter()
                .filter(|slot| slot.state.is_running())
                .map(|slot| (slot.id.clone(), slot.name().to_owned())),
        );
        let entries = &self.hotkeys.entries;
        let awaiting = self.hotkey_window.awaiting.map(|(index, _)| index);
        let mut clicked = None;
        let mut reset = None;
        let mut open = true;
        egui::Window::new("Hotkey")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label("Click the keys of an action, then press the new ones; Escape cancels.");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // One grid for all, so that the keys of every module line up.
                    egui::Grid::new("hotkeys").num_columns(4).striped(true).show(ui, |ui| {
                        for (owner, title) in &owners {
                            let indices: Vec<usize> =
                                (0..entries.len()).filter(|&i| entries[i].owner == *owner).collect();
                            if indices.is_empty() {
                                continue;
                            }
                            ui.strong(title);
                            ui.end_row();
                            for index in indices {
                                let entry = &entries[index];
                                let keys = entry.hotkey.keys();
                                ui.label(&entry.label);
                                let text = match (awaiting == Some(index), keys.is_none()) {
                                    (true, _) => "Press the keys…".to_owned(),
                                    (false, true) => "None".to_owned(),
                                    (false, false) => keys.to_string(),
                                };
                                let button = egui::Button::new(text).selected(awaiting == Some(index));
                                if ui.add_sized([150.0, 20.0], button).clicked() {
                                    clicked = Some(index);
                                }
                                let default = if entry.default.is_none() {
                                    "None".to_owned()
                                } else {
                                    entry.default.to_string()
                                };
                                if ui
                                    .add_enabled(keys != entry.default, egui::Button::new("Default"))
                                    .on_hover_text(default)
                                    .clicked()
                                {
                                    reset = Some(index);
                                }
                                let conflicts: Vec<String> = self
                                    .hotkeys
                                    .conflicts(index, is_running)
                                    .into_iter()
                                    .map(|other| {
                                        let other = &entries[other];
                                        let owner = owners.iter().find(|(id, _)| *id == other.owner);
                                        format!("{} ({})", other.label, owner.map_or(other.owner.as_str(), |o| &o.1))
                                    })
                                    .collect();
                                if conflicts.is_empty() {
                                    ui.label("");
                                } else {
                                    ui.colored_label(
                                        ui.visuals().warn_fg_color,
                                        format!("Same keys as {}", conflicts.join(", ")),
                                    );
                                }
                                ui.end_row();
                            }
                        }
                    });
                });
            });
        if let Some(index) = reset {
            let default = self.hotkeys.entries[index].default;
            self.hotkeys.bind(index, default, &mut self.host.settings.hotkeys);
            self.host.settings_changed = true;
        }
        if let Some(index) = clicked {
            self.hotkey_window.awaiting = if awaiting == Some(index) {
                None
            } else {
                Some((index, Keys::NONE))
            };
        }
        if !open {
            self.hotkey_window.open = false;
            self.hotkey_window.awaiting = None;
        }
    }
}
