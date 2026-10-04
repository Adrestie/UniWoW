//! The dock panels: those of the modules and those of the kernel.

use super::*;

pub(super) struct Viewer<'a> {
    pub(super) slots: &'a mut [Slot],
    pub(super) host: &'a mut KernelHost,
    pub(super) titles: &'a [(Tab, String)],
    pub(super) failures: Vec<(usize, String)>,
    pub(super) closed: Vec<Tab>,
    pub(super) runtime: Option<&'a str>,
    pub(super) modules_dir: &'a Path,
    pub(super) restart_needed: &'a mut bool,
    pub(super) commands_panel: &'a mut CommandsPanel,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(tab.key())
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        self.titles
            .iter()
            .find(|(t, _)| t == tab)
            .map_or_else(|| tab.key(), |(_, title)| title.clone())
            .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        if tab.module == KERNEL {
            match tab.panel.as_str() {
                "modules" => self.modules_panel(ui),
                "log" => log_panel(ui),
                "jobs" => panels::jobs_panel(ui, &self.host.pool),
                "commands" => self.commands_panel.ui(ui, &self.host.bridge),
                _ => {}
            }
            return;
        }
        let Some(index) = self.slots.iter().position(|s| s.id == tab.module) else {
            ui.label(format!("The module '{}' is not present.", tab.module));
            return;
        };
        let slot = &mut self.slots[index];
        if !slot.state.is_running() {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("{} is not running: {}", slot.name(), state_text(&slot.state)),
            );
            return;
        }
        let panel = tab.panel.clone();
        if let Err(message) = call_module(slot, self.host, |f, ctx| f.panel_ui(&panel, ui, ctx)) {
            self.failures.push((index, format!("panel '{panel}': {message}")));
        }
    }

    fn on_close(&mut self, tab: &mut Tab) -> OnCloseResponse {
        self.closed.push(tab.clone());
        OnCloseResponse::Close
    }
}

pub(super) fn log_panel(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        if ui.button("Clear").clicked() {
            logger::clear();
        }
    });
    ui.separator();
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .stick_to_bottom(true)
        .show_rows(ui, row_height, logger::len(), |ui, range| {
            for line in logger::lines(range) {
                let color = match line.level {
                    log::Level::Error => ui.visuals().error_fg_color,
                    log::Level::Warn => ui.visuals().warn_fg_color,
                    _ => ui.visuals().text_color(),
                };
                let text = format!(
                    "{:>9.3}  {:<5}  {:<14}  {}",
                    line.seconds, line.level, line.source, line.message
                );
                ui.label(egui::RichText::new(text).monospace().color(color));
            }
        });
}
