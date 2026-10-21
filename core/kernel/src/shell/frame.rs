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
        self.keep_layout();
        self.host.save_settings();
    }

    /// What shows where nothing is drawn, such as a window being resized: the lowest level.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::palette().base.to_normalized_gamma_f32()
    }
}

impl Shell {
    /// The work of the kernel. eframe calls it before each `ui`, and also while the window is
    /// minimised whenever a repaint is requested, as other threads do when they need the kernel.
    pub(super) fn logic_pass(&mut self, ctx: &egui::Context) {
        self.frames.start(self.host.pool.running().len());
        self.clock = ctx.input(|input| input.time);
        let timed = |part: &str, start: Instant| uniwow_api::journal::spent(part, start.elapsed());
        // Before the calls and jobs: an event a thread published before a call is delivered
        // before the events that call causes.
        let start = Instant::now();
        self.collect_from_threads();
        self.apply_pending();
        self.apply_reported();
        self.collect_notices();
        timed("kernel: what the threads reported", start);
        // A window torn off may show while the main window is minimised: frames are drawn for it.
        let minimised = ctx.input(|i| i.viewport().minimized.unwrap_or(false)) && self.torn.is_empty();
        // Told to the modules, which draw nothing meanwhile, such as the sounds to silence.
        if minimised != self.minimised {
            self.minimised = minimised;
            self.host
                .publish(KERNEL, MINIMISED_TOPIC, serde_json::json!({ "minimised": minimised }));
        }
        let budget = if minimised { MINIMISED_CALL_BUDGET } else { CALL_BUDGET };
        let start = Instant::now();
        if self.serve_calls(budget) {
            // Calls were left for the next frame: it must come even if nothing else asks for it.
            ctx.request_repaint();
        }
        timed("kernel: calls", start);
        let start = Instant::now();
        let jobs_budget = if minimised { MINIMISED_JOBS_BUDGET } else { JOBS_BUDGET };
        if self.deliver_jobs(jobs_budget) {
            // Jobs were left for the next frame: it must come even if nothing else asks for it.
            ctx.request_repaint();
        }
        timed("kernel: jobs handed back", start);
        let start = Instant::now();
        self.collect_from_threads();
        self.apply_reported();
        self.dispatch_events();
        self.apply_reported();
        self.settle_gestures();
        self.steer_project(ctx);
        timed("kernel: events", start);
        let start = Instant::now();
        self.play(ctx, Instant::now());
        timed("kernel: players", start);
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
        let ctx = ui.ctx().clone();
        if !self.themed {
            // The frame of the main window, drawn by the system, dark as the rest.
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
            self.themed = true;
        }
        self.await_keys(&ctx);
        let theme = theme::current();
        let mut actions = Vec::new();
        egui::Panel::top("menu_bar")
            .frame(theme.bar_frame())
            .show(ui, |ui| actions = self.menu_bar(ui));
        let status = egui::Panel::bottom("status_bar")
            .frame(theme.bar_frame())
            .show(ui, |ui| self.status_bar(ui, &mut actions));
        let status_height = status.response.rect.height();

        // Every registered panel, so that the tab of a module that failed keeps its title.
        let mut titles: Vec<(Tab, String)> = self.panel_entries().into_iter().map(|e| (e.tab, e.title)).collect();
        for slot in self.slots.iter().filter(|s| !s.state.is_running()) {
            titles.extend(slot.panels.iter().map(|p| (Tab::new(&slot.id, &p.id), p.title.clone())));
        }
        // Where the main window's content starts on the screens, for the windows torn off.
        let native = self.native_windows(&ctx);
        let origin = ctx.input(|input| input.viewport().inner_rect).map(|rect| rect.min);
        let dragged_out = if native { self.dragged_out(&ctx, origin) } else { None };
        let eject = if native {
            format!("{}  Tear off into its own window", theme::icon::TEAR_OFF)
        } else {
            egui_dock::Translations::english().tab_context_menu.eject_button
        };
        if self.dock.translations.tab_context_menu.eject_button != eject {
            self.dock.translations.tab_context_menu.eject_button = eject;
        }
        let active: HashSet<Tab> = self
            .dock
            .iter_leaves()
            .filter_map(|(_, leaf)| leaf.tabs.get(leaf.active.0).cloned())
            .collect();
        let focused = self
            .dock
            .focused_leaf()
            .and_then(|path| self.dock.leaf(path).ok())
            .and_then(|leaf| leaf.tabs.get(leaf.active.0).cloned());
        let restart_needed = self.restart_needed;
        let mut viewer = Viewer {
            slots: &mut self.slots,
            host: &mut self.host,
            titles: &titles,
            shown: &active,
            focused: focused.as_ref(),
            failures: Vec::new(),
            closed: Vec::new(),
            runtime: self.runtime_fingerprint.as_deref(),
            modules_dir: &self.modules_dir,
            restart_needed: &mut self.restart_needed,
            commands_panel: &mut self.commands_panel,
        };
        let mut shown = Ok(());
        let panels = Instant::now();
        let dock_style = &self.dock_style;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme.palette.base)
                    .inner_margin(egui::Margin::symmetric(4, 2)),
            )
            .show(ui, |ui| {
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
                        .id(egui::Id::new(windows::DOCK_ID))
                        .style(dock_style.clone())
                        .show_inside(ui, &mut viewer);
                });
            });
        uniwow_api::journal::spent("kernel: panels", panels.elapsed());
        let torn_off = Instant::now();
        let torn = windows::torn_windows_ui(&mut self.torn, &ctx, &mut viewer, &self.hotkeys);
        uniwow_api::journal::spent("kernel: windows torn off", torn_off.elapsed());
        let Viewer { failures, closed, .. } = viewer;
        // A layout egui_dock cannot draw would otherwise stop the editor at every start. The reset is
        // tried once; a panel that panics at every frame then leaves a fixed message instead.
        if let Err(message) = shown {
            match self.panels {
                PanelsHealth::Drawn => {
                    log::error!(target: logger::NOTIFIED, "the panels could not be drawn, the layout was reset: {message}");
                    self.notify("layout", Level::Warning, "The layout was reset", &message);
                    self.dock = layout::default_layout(&self.panel_entries(), &self.host.settings.closed_panels);
                    self.panels = PanelsHealth::Reset;
                }
                _ => {
                    log::error!("the panels still cannot be drawn and are no longer shown: {message}");
                    self.panels = PanelsHealth::Broken(message);
                }
            }
        }
        if self.restart_needed && !restart_needed {
            self.notify(
                "restart",
                Level::Info,
                "Restart UniWoW to apply the changes",
                "A module enabled or disabled starts or stops at the next start.",
            );
        }
        // The panels torn off this frame, and those docked back.
        for index in torn.docked.into_iter().rev() {
            self.dock_back(index);
        }
        if let Some((tab, position, size)) = dragged_out {
            self.tear_off(tab, position, size);
        }
        if native {
            self.take_floating(origin, ctx.pixels_per_point(), |surface| {
                ctx.memory(|memory| memory.area_rect(windows::floating_id(surface)))
            });
        }

        for (index, message) in failures {
            self.fail(index, message);
        }
        for tab in closed {
            self.host.settings.closed_panels.insert(tab.key());
            self.host.settings_changed = true;
        }

        // The modules' floating windows, over the panels.
        let windows = Instant::now();
        for index in 0..self.slots.len() {
            if !self.slots[index].state.is_running() {
                continue;
            }
            let start = Instant::now();
            let done = call_module(&mut self.slots[index], &mut self.host, |f, c| f.windows_ui(&ctx, c));
            uniwow_api::journal::spent(&format!("windows of {}", self.slots[index].id), start.elapsed());
            if let Err(message) = done {
                self.fail(index, format!("windows: {message}"));
            }
        }
        uniwow_api::journal::spent("kernel: windows of the modules", windows.elapsed());
        // The panels the modules asked to show: opened, in front.
        for (owner, panel) in std::mem::take(&mut self.host.shown) {
            let tab = Tab::new(&owner, &panel);
            self.set_panel_open(&tab, true);
            if let Some(path) = self.dock.find_tab(&tab) {
                let _ = self.dock.set_active_tab(path);
            }
        }
        self.hotkey_window(&ctx);
        self.settings_window(&ctx);
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
        if !modal && !dragging {
            if self.hotkeys.undo.pressed(&ctx) {
                actions.push(MenuAction::Undo);
            }
            if self.hotkeys.redo.pressed(&ctx) {
                actions.push(MenuAction::Redo);
            }
            actions.extend(torn.keys);
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
                MenuAction::DockAllBack => {
                    while !self.torn.is_empty() {
                        self.dock_back(self.torn.len() - 1);
                    }
                }
                MenuAction::Hotkeys => self.hotkey_window.open = true,
                MenuAction::Settings => self.settings_window.open = true,
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

        self.toasts_ui(&ctx, status_height);
        // What the panels and menus queued is applied by `logic` at the next frame.
        let host = &self.host;
        if !host.pending.is_empty()
            || !host.forgotten.is_empty()
            || !host.reported.is_empty()
            || !host.events.is_empty()
            || host.settings_changed
            || !host.notices.is_empty()
        {
            ctx.request_repaint();
        }
        self.frames.window_drawn();
    }
}
