//! The window *Edit > Settings*: the categories the running modules declare, those of one title
//! shown as one, the settings of each module in the order of their ids, each drawn as declared; a
//! value chosen is kept in its module's settings, which the module reads. A module stopped or
//! failed shows none of its settings.

use uniwow_api::SettingSpec;

use super::*;

#[derive(Default)]
pub(super) struct SettingsWindow {
    pub open: bool,
    /// The title of the category shown; the first when none is chosen.
    pub shown: Option<String>,
}

/// A category of the window: its title, and the settings of every module declaring it, each with
/// its module.
pub(super) struct Category {
    pub title: String,
    pub settings: Vec<(String, SettingSpec)>,
}

impl Shell {
    /// The categories of the running modules, those of one title merged, sorted by their titles.
    pub(super) fn settings_categories(&self) -> Vec<Category> {
        let mut declaring: Vec<&Slot> = self
            .slots
            .iter()
            .filter(|slot| slot.state.is_running() && slot.settings.is_some())
            .collect();
        declaring.sort_by(|a, b| a.id.cmp(&b.id));
        let mut categories: Vec<Category> = Vec::new();
        for slot in declaring {
            let Some(declared) = &slot.settings else {
                continue;
            };
            let settings = declared.settings.iter().map(|spec| (slot.id.clone(), spec.clone()));
            match categories.iter_mut().find(|category| category.title == declared.title) {
                Some(category) => category.settings.extend(settings),
                None => categories.push(Category {
                    title: declared.title.clone(),
                    settings: settings.collect(),
                }),
            }
        }
        categories.sort_by(|a, b| a.title.cmp(&b.title));
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
            .filter(|title| categories.iter().any(|category| category.title == *title))
            .or_else(|| categories.first().map(|category| category.title.clone()));
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
                        for category in &categories {
                            if ui
                                .selectable_label(shown.as_ref() == Some(&category.title), &category.title)
                                .clicked()
                            {
                                chosen = Some(category.title.clone());
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        let Some(category) = categories
                            .iter()
                            .find(|category| shown.as_ref() == Some(&category.title))
                        else {
                            return;
                        };
                        ui.heading(&category.title);
                        egui::Grid::new("settings").num_columns(3).show(ui, |ui| {
                            for (module, spec) in &category.settings {
                                let mut value = spec.value(host.setting(module, &spec.key).as_ref());
                                ui.label(&spec.label);
                                if ui
                                    .add(egui::Slider::new(&mut value, spec.range[0]..=spec.range[1]))
                                    .changed()
                                {
                                    changes.push((module.clone(), spec.key.clone(), value));
                                }
                                if ui
                                    .add_enabled(value != spec.default, egui::Button::new("Default"))
                                    .on_hover_text(spec.default.to_string())
                                    .clicked()
                                {
                                    changes.push((module.clone(), spec.key.clone(), spec.default));
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
