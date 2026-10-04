//! The frame: the kernel's work in `logic`, then the window in `ui`.

use super::*;

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.logic_pass(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_pass(ui);
    }

    fn on_exit(&mut self) {
        for slot in self.slots.iter_mut().filter(|s| s.state.is_running()) {
            let module = slot.module.as_deref_mut().expect("running modules are loaded");
            if let Err(message) = guarded_as(&slot.id, || module.shutdown()) {
                log::error!("module '{}' failed in shutdown: {message}", slot.id);
            }
        }
        self.host.settings.layout = serde_json::to_value(&self.dock).ok();
        self.host.save_settings();
    }
}

impl Shell {
    /// The work of the kernel. eframe calls it before each `ui`, and also while the window is
    /// minimised whenever a repaint is requested, as other threads do when they need the kernel.
    pub(super) fn logic_pass(&mut self, ctx: &egui::Context) {
        // Before the calls and jobs: an event a thread published before a call is delivered
        // before the events that call causes.
        self.collect_from_threads();
        self.apply_pending();
        self.apply_reported();
        let minimised = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let budget = if minimised { MINIMISED_CALL_BUDGET } else { CALL_BUDGET };
        if self.serve_calls(budget) {
            // Calls were left for the next frame: it must come even if nothing else asks for it.
            ctx.request_repaint();
        }
        self.deliver_jobs();
        self.collect_from_threads();
        self.apply_reported();
        self.dispatch_events();
        self.apply_reported();
        if !self.host.events.is_empty() {
            ctx.request_repaint();
        }
        if !self.host.pool.running().is_empty() {
            // Keeps the progress of the jobs moving in the Jobs panel.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let busy = self
            .slots
            .iter()
            .any(|slot| slot.compiled.is_some_and(|module| module.activity.pending() > 0));
        if busy {
            // Undo comes back, and the Modules panel tells a module not responding, without input.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self
            .host
            .bridge
            .settings_changed
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.host.settings_changed = true;
        }
        if self.host.settings_changed {
            let since = self.last_settings_save.elapsed();
            if since >= SETTINGS_SAVE_INTERVAL {
                self.host.save_settings();
                self.host.settings_changed = false;
                self.last_settings_save = Instant::now();
            } else {
                ctx.request_repaint_after(SETTINGS_SAVE_INTERVAL - since);
            }
        }
        // Here rather than in `ui`: eframe calls only `logic` while the window is minimised, then
        // closes it unless told otherwise.
        if ctx.input(|i| i.viewport().close_requested()) {
            self.close_requested(ctx);
        }
        if matches!(self.closing, Closing::Confirmed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Draws the window and handles what the user did in it; the rest waits for `logic`.
    pub(super) fn ui_pass(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        egui::Panel::top("menu_bar").show(ui, |ui| actions = self.menu_bar(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));

        // Every registered panel, so that the tab of a module that failed keeps its title.
        let mut titles: Vec<(Tab, String)> = self.panel_entries().into_iter().map(|e| (e.tab, e.title)).collect();
        for slot in self.slots.iter().filter(|s| !s.state.is_running()) {
            titles.extend(slot.panels.iter().map(|p| (Tab::new(&slot.id, &p.id), p.title.clone())));
        }
        let mut viewer = Viewer {
            slots: &mut self.slots,
            host: &mut self.host,
            titles: &titles,
            failures: Vec::new(),
            closed: Vec::new(),
            runtime: self.runtime_fingerprint.as_deref(),
            modules_dir: &self.modules_dir,
            restart_needed: &mut self.restart_needed,
            commands_panel: &mut self.commands_panel,
        };
        let mut shown = Ok(());
        egui::CentralPanel::default().show(ui, |ui| {
            if let PanelsHealth::Broken(reason) = &self.panels {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("The panels cannot be displayed: {reason}. The log follows."),
                );
                ui.separator();
                log_panel(ui);
                return;
            }
            shown = guarded(|| {
                DockArea::new(&mut self.dock)
                    .style(Style::from_egui(ui.style()))
                    .show_inside(ui, &mut viewer);
            });
        });
        let Viewer { failures, closed, .. } = viewer;
        let ctx = ui.ctx().clone();
        // A layout egui_dock cannot draw would otherwise stop the editor at every start. The reset is
        // tried once; a panel that panics at every frame then leaves a fixed message instead.
        if let Err(message) = shown {
            match self.panels {
                PanelsHealth::Drawn => {
                    log::error!("the panels could not be drawn, the layout was reset: {message}");
                    self.dock = layout::default_layout(&self.panel_entries(), &self.host.settings.closed_panels);
                    self.panels = PanelsHealth::Reset;
                }
                _ => {
                    log::error!("the panels still cannot be drawn and are no longer shown: {message}");
                    self.panels = PanelsHealth::Broken(message);
                }
            }
        }

        for (index, message) in failures {
            self.fail(index, message);
        }
        for tab in closed {
            self.host.settings.closed_panels.insert(tab.key());
            self.host.settings_changed = true;
        }

        // The modules' floating windows, over the panels.
        for index in 0..self.slots.len() {
            if self.slots[index].state.is_running()
                && let Err(message) = call_module(&mut self.slots[index], &mut self.host, |f, c| f.windows_ui(&ctx, c))
            {
                self.fail(index, format!("windows: {message}"));
            }
        }
        // A modal window takes the keyboard from the editor: one the kernel draws, from the frame it
        // is drawn in to the last one; one a Rust module draws with egui, as egui knows it, from
        // the frame after.
        let kernel_modal = std::mem::take(&mut self.host.modal_shown);
        let module_modal = ctx
            .memory(|memory| memory.top_modal_layer())
            .is_some_and(|layer| !self.host.kernel_modals.contains(&layer));
        let modal = kernel_modal || module_modal;
        // Nor while something is dragged: undoing under a drag would change what it moves.
        let dragging = ctx.dragged_id().is_some();
        if !ctx.egui_wants_keyboard_input() && !modal && !dragging {
            if ctx
                .input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z)))
            {
                actions.push(MenuAction::Undo);
            }
            if ctx
                .input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y)))
            {
                actions.push(MenuAction::Redo);
            }
        }
        for action in actions {
            match action {
                MenuAction::Undo => self.undo(),
                MenuAction::Redo => self.redo(),
                MenuAction::CloseGroup(id) => {
                    if let Some(closed) = self.groups.close(id) {
                        log::info!("undo group '{}' closed from the Edit menu", closed.label);
                        self.push_closed(closed);
                    }
                }
                MenuAction::SetPanelOpen(tab, open) => self.set_panel_open(&tab, open),
                MenuAction::Module(index, action) => {
                    if self.slots[index].state.is_running()
                        && let Err(message) =
                            call_module(&mut self.slots[index], &mut self.host, |f, ctx| f.on_menu(&action, ctx))
                    {
                        self.fail(index, format!("menu '{action}': {message}"));
                    }
                }
            }
        }

        // What the panels and menus queued is applied by `logic` at the next frame.
        let host = &self.host;
        if !host.pending.is_empty()
            || !host.forgotten.is_empty()
            || !host.reported.is_empty()
            || !host.events.is_empty()
            || host.settings_changed
        {
            ctx.request_repaint();
        }
    }
}
