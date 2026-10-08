//! The window *Edit > Settings*: a category for each running module that declares one, titled as it
//! says, its settings drawn as it declared them; a value chosen is kept in the module's settings,
//! which the module reads. A module stopped or failed shows no category.

use uniwow_api::SettingsCategory;

use super::*;

#[derive(Default)]
pub(super) struct SettingsWindow {
    pub open: bool,
    /// The module whose category is shown; the first by its title when none is chosen.
    pub shown: Option<String>,
}

impl Shell {
    /// The categories of the running modules, by module, sorted by their titles.
    pub(super) fn settings_categories(&self) -> Vec<(String, SettingsCategory)> {
        let mut categories: Vec<(String, SettingsCategory)> = self
            .slots
            .iter()
            .filter(|slot| slot.state.is_running())
            .filter_map(|slot| Some((slot.id.clone(), slot.settings.clone()?)))
            .collect();
        categories.sort_by(|a, b| a.1.title.cmp(&b.1.title).then(a.0.cmp(&b.0)));
        categories
    }

    /// Draws the window when it is open.
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_window.open {
            return;
        }
        let categories = self.settings_categories();
        let shown = self
            .settings_window
            .shown
            .clone()
            .filter(|id| categories.iter().any(|(owner, _)| owner == id))
            .or_else(|| categories.first().map(|(id, _)| id.clone()));
        let mut chosen = None;
        let mut changes: Vec<(String, String, i64)> = Vec::new();
        let mut open = true;
        let host = &self.host;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                if categories.is_empty() {
                    ui.label("No running module has settings.");
                    return;
                }
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        for (id, category) in &categories {
                            if ui
                                .selectable_label(shown.as_deref() == Some(id.as_str()), &category.title)
                                .clicked()
                            {
                                chosen = Some(id.clone());
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        let Some((id, category)) = categories.iter().find(|(id, _)| shown.as_ref() == Some(id)) else {
                            return;
                        };
                        ui.heading(&category.title);
                        egui::Grid::new("settings").num_columns(3).show(ui, |ui| {
                            for spec in &category.settings {
                                let mut value = spec.value(host.setting(id, &spec.key).as_ref());
                                ui.label(&spec.label);
                                if ui
                                    .add(egui::Slider::new(&mut value, spec.range[0]..=spec.range[1]))
                                    .changed()
                                {
                                    changes.push((id.clone(), spec.key.clone(), value));
                                }
                                if ui
                                    .add_enabled(value != spec.default, egui::Button::new("Default"))
                                    .on_hover_text(spec.default.to_string())
                                    .clicked()
                                {
                                    changes.push((id.clone(), spec.key.clone(), spec.default));
                                }
                                ui.end_row();
                            }
                        });
                    });
                });
            });
        for (module, key, value) in changes {
            self.host.set_setting(&module, &key, serde_json::json!(value));
        }
        if chosen.is_some() {
            self.settings_window.shown = chosen;
        }
        if !open {
            self.settings_window.open = false;
        }
    }
}
