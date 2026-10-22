//! The dock panels: those of the modules and those of the kernel.

use super::*;

pub(super) struct Viewer<'a> {
    pub(super) slots: &'a mut [Slot],
    pub(super) host: &'a mut KernelHost,
    pub(super) titles: &'a [(Tab, String)],
    /// The tabs shown in their part of the dock, and that of the part focused.
    pub(super) shown: &'a HashSet<Tab>,
    pub(super) focused: Option<&'a Tab>,
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
        // The modules drawing sections in this panel lent to the host while it is drawn, for it to
        // draw them where the panel asks (`Context::draw_sections`).
        let owner = self.slots[index].id.clone();
        for (lent, slot) in self.slots.iter_mut().enumerate() {
            if lent == index || !slot.state.is_running() {
                continue;
            }
            let sections: Vec<String> = slot
                .sections
                .iter()
                .filter(|spec| spec.owner == owner && spec.panel == panel)
                .map(|spec| spec.section.clone())
                .collect();
            if sections.is_empty() {
                continue;
            }
            if let Some(module) = slot.module.take() {
                self.host.embedded.push(crate::host::Embedded {
                    slot: lent,
                    id: slot.id.clone(),
                    sections,
                    module: Some(module),
                    failure: None,
                });
            }
        }
        let slot = &mut self.slots[index];
        if let Err(message) = call_module(slot, self.host, |f, ctx| f.panel_ui(&panel, ui, ctx)) {
            self.failures.push((index, format!("panel '{panel}': {message}")));
        }
        for lent in std::mem::take(&mut self.host.embedded) {
            self.slots[lent.slot].module = lent.module;
            if let Some(message) = lent.failure {
                self.failures
                    .push((lent.slot, format!("section in '{owner}/{panel}': {message}")));
            }
        }
    }

    fn on_close(&mut self, tab: &mut Tab) -> OnCloseResponse {
        self.closed.push(tab.clone());
        OnCloseResponse::Close
    }

    /// The pointer lights a tab in `time::HOVER`, and the tab shown takes a line of the accent along
    /// its bottom, growing from its middle in `time::OPEN`, full in the part of the dock focused.
    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        let p = theme::palette();
        let ctx = &response.ctx;
        let painter = ctx.layer_painter(response.layer_id).with_clip_rect(response.rect);
        let lit = ctx.animate_bool_with_time_and_easing(
            response.id.with("uniwow hover"),
            response.hovered(),
            theme::time::HOVER,
            egui::emath::easing::cubic_out,
        );
        let shown = self.shown.contains(tab);
        if lit > 0.0 && !shown {
            painter.rect_filled(
                response.rect,
                egui::CornerRadius::ZERO,
                theme::Palette::alpha(p.hover_tint(), lit),
            );
        }
        let grown = ctx.animate_bool_with_time_and_easing(
            response.id.with("uniwow shown"),
            shown,
            theme::time::OPEN,
            egui::emath::easing::cubic_out,
        );
        if grown > 0.0 {
            let colour = if self.focused == Some(&*tab) {
                p.accent
            } else {
                theme::Palette::mix(p.border, p.accent, 0.45)
            };
            let rect = response.rect;
            let half = rect.width() / 2.0 * grown;
            let line = egui::Rect::from_min_max(
                egui::pos2(rect.center().x - half, rect.bottom() - 2.0),
                egui::pos2(rect.center().x + half, rect.bottom()),
            );
            painter.rect_filled(line, egui::CornerRadius::same(1), colour);
        }
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
