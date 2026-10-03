use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use uniwow_api::egui_dock::tab_viewer::OnCloseResponse;
use uniwow_api::egui_dock::{DockArea, DockState, Style, TabViewer};
use uniwow_api::{
    CallId, CommandInfo, Context, DockArea as Area, FEATURE_FAILED_TOPIC, Feature, Host, Registrar, RunsOn, eframe,
    egui, log, serde_json,
};

use crate::groups::{Closed, Ended, Groups};
use crate::guard::{guarded, guarded_as};
use crate::history::{self, History};
use crate::host::{KernelHost, Service};
use crate::jobs::Pool;
use crate::layout::{self, PanelEntry, Tab};
use crate::loader::{self, Slot, State};
use crate::logger;
use crate::order;
use crate::panels::{self, CommandsPanel};
use crate::requirements::{self, Need};
use crate::router::{self, Bridge, Entry, ReplyTo, Request, feature_of};
use crate::settings::Settings;

const KERNEL: &str = "kernel";
const BUILT_IN_MENUS: [&str; 4] = ["File", "Edit", "Window", "Help"];
/// Settings changed by features are written at most this often, and once more at exit.
const SETTINGS_SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// Time the interface thread spends each frame answering calls of other threads.
const CALL_BUDGET: Duration = Duration::from_millis(4);
/// After a call, how long to wait for the next one of a thread calling in a loop.
const CALL_IDLE: Duration = Duration::from_micros(500);

pub struct Shell {
    host: KernelHost,
    slots: Vec<Slot>,
    dock: DockState<Tab>,
    history: History,
    runtime_fingerprint: Option<String>,
    features_dir: PathBuf,
    restart_needed: bool,
    last_settings_save: Instant,
    panels: PanelsHealth,
    /// Calls of other threads and of `Context::call`, answered on this thread.
    requests: Option<mpsc::Receiver<Request>>,
    /// Answers to `Context::call`, delivered after the calls are served.
    replies: Vec<(String, CallId, Result<serde_json::Value, String>)>,
    commands_panel: CommandsPanel,
    /// Undo groups open per caller and thread (S4): their commands become one entry when the
    /// group ends.
    groups: Groups,
}

/// Whether the dock could be drawn this session.
enum PanelsHealth {
    Drawn,
    /// Drawing panicked once and the layout was reset to the default one.
    Reset,
    /// Drawing panicked again after the reset: a fixed message replaces the panels, with the reason.
    Broken(String),
}

impl Shell {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let (bridge, requests) = Bridge::new(Some(cc.egui_ctx.clone()));
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let pool = Pool::new(threads, Some(cc.egui_ctx.clone()), Some(bridge.clone()));
        let host = KernelHost::new(cc.wgpu_render_state.clone(), Settings::load(), pool, bridge);
        // wgpu panics on errors nobody captured; log them instead, the editor must keep running.
        if let Some(gpu) = &host.gpu {
            gpu.device.on_uncaptured_error(Arc::new(|error| {
                log::error!("GPU error not captured by any feature: {error}");
            }));
        }
        let discovery = loader::discover(&exe_dir, &host.settings.disabled_features);
        let mut shell = Self {
            host,
            slots: discovery.slots,
            dock: DockState::new(Vec::new()),
            history: History::default(),
            runtime_fingerprint: discovery.runtime_fingerprint,
            features_dir: exe_dir.join("features"),
            restart_needed: false,
            last_settings_save: Instant::now(),
            panels: PanelsHealth::Drawn,
            requests: Some(requests),
            replies: Vec::new(),
            commands_panel: CommandsPanel::default(),
            groups: Groups::default(),
        };
        shell.register_all();
        shell.resolve_requirements();
        shell.sync_running();
        shell.init_all();
        // A damaged saved layout must never prevent the editor from starting.
        shell.dock = guarded(|| shell.restore_layout()).unwrap_or_else(|message| {
            log::warn!("saved layout discarded: {message}");
            layout::default_layout(&shell.panel_entries(), &shell.host.settings.closed_panels)
        });
        shell.apply_pending();
        shell.apply_reported();
        shell.log_summary();
        shell
    }

    fn register_all(&mut self) {
        for slot in &mut self.slots {
            let Some(feature) = slot.feature.as_deref_mut().filter(|_| slot.state.is_running()) else {
                continue;
            };
            let mut reg = Registrar::default();
            if let Err(message) = guarded_as(&slot.id, || feature.register(&mut reg)) {
                log::error!("feature '{}' failed in register: {message}", slot.id);
                slot.state = State::Failed(format!("register: {message}"));
                continue;
            }
            slot.panels = reg.panels;
            slot.menu_items = reg.menu_items;
            slot.subscriptions = reg.subscriptions;
            let mut catalogue = self.host.bridge.catalogue.write().unwrap_or_else(|e| e.into_inner());
            for spec in reg.commands {
                if let Some(existing) = catalogue.get(&spec.name) {
                    log::warn!(
                        "command '{}' of '{}' ignored: already offered by '{}'",
                        spec.name,
                        slot.id,
                        existing.info.owner
                    );
                    continue;
                }
                let handler = match spec.runs_on {
                    RunsOn::Interface => None,
                    RunsOn::Caller(handler) => Some(handler),
                };
                let info = CommandInfo {
                    name: spec.name.clone(),
                    owner: slot.id.clone(),
                    description: spec.description,
                    arguments: spec.arguments,
                    result: spec.result,
                    on_caller: handler.is_some(),
                };
                catalogue.insert(spec.name, Entry { info, handler });
            }
            drop(catalogue);
            for (id, value) in reg.services {
                if let Some(existing) = self.host.services.get(&id) {
                    log::warn!(
                        "service '{id}' of '{}' ignored: already provided by '{}'",
                        slot.id,
                        existing.provider
                    );
                    continue;
                }
                let provider = slot.id.clone();
                self.host.services.insert(id, Service { provider, value });
            }
        }
    }

    /// Blocks the features whose required services are missing, until nothing changes.
    fn resolve_requirements(&mut self) {
        let running: Vec<usize> = (0..self.slots.len())
            .filter(|&i| self.slots[i].state.is_running() && self.slots[i].manifest.is_some())
            .collect();
        let blocked = {
            let needs: Vec<Need> = running
                .iter()
                .map(|&i| Need {
                    id: &self.slots[i].id,
                    requires: &self.slots[i].manifest.as_ref().expect("filtered above").requires,
                })
                .collect();
            let providers: HashMap<String, String> = self
                .host
                .services
                .iter()
                .map(|(service, s)| (service.clone(), s.provider.clone()))
                .collect();
            requirements::blocked(&needs, &providers)
        };
        for (need, service) in blocked {
            self.block(
                running[need],
                format!("requires the service '{service}', which no running feature provides"),
            );
        }
    }

    /// The first service a running feature requires and nobody provides.
    fn missing_requirement(&self, index: usize) -> Option<String> {
        let slot = &self.slots[index];
        let manifest = slot.manifest.as_ref().filter(|_| slot.state.is_running())?;
        manifest
            .requires
            .iter()
            .find(|s| !self.host.services.contains_key(*s))
            .cloned()
    }

    fn block(&mut self, index: usize, reason: String) {
        let id = self.slots[index].id.clone();
        self.slots[index].state = State::Blocked(reason);
        self.host.services.retain(|_, s| s.provider != id);
        self.sync_running();
    }

    /// Tells the bridge which features are running: only their commands can be called.
    fn sync_running(&self) {
        *self.host.bridge.running.write().unwrap_or_else(|e| e.into_inner()) = self.running_ids();
    }

    /// Running features, providers of a service before the features that require or use it.
    fn init_order(&self) -> Vec<usize> {
        let running: Vec<usize> = (0..self.slots.len())
            .filter(|&i| self.slots[i].state.is_running())
            .collect();
        let providers_of = |i: usize| -> Vec<usize> {
            let Some(manifest) = &self.slots[i].manifest else {
                return Vec::new();
            };
            manifest
                .requires
                .iter()
                .chain(&manifest.uses)
                .filter_map(|service| self.host.services.get(service))
                .filter_map(|s| self.slots.iter().position(|slot| slot.id == s.provider))
                .filter(|&p| p != i)
                .collect()
        };
        let (order, forced) = order::init_order(&running, providers_of);
        if !forced.is_empty() {
            let ids: Vec<&str> = forced.iter().map(|&i| self.slots[i].id.as_str()).collect();
            log::warn!(
                "dependency cycle between features: {} initialised before the providers they use",
                ids.join(", ")
            );
        }
        order
    }

    fn init_all(&mut self) {
        for index in self.init_order() {
            if !self.slots[index].state.is_running() {
                continue;
            }
            // A provider may have failed in its own init since the requirements were resolved.
            if let Some(service) = self.missing_requirement(index) {
                self.block(
                    index,
                    format!("requires the service '{service}', whose provider failed"),
                );
                continue;
            }
            if let Err(message) = call_feature(&mut self.slots[index], &mut self.host, |f, ctx| f.init(ctx)) {
                self.fail(index, format!("init: {message}"));
            }
            self.apply_pending();
            self.apply_reported();
        }
    }

    fn log_summary(&self) {
        let running = self.slots.iter().filter(|s| s.state.is_running()).count();
        log::info!(
            "{running} of {} features running, runtime {}",
            self.slots.len(),
            short(self.runtime_fingerprint.as_deref())
        );
        for slot in &self.slots {
            if !slot.state.is_running() {
                log::warn!("feature '{}': {}", slot.id, state_text(&slot.state));
            }
        }
    }

    fn panel_entries(&self) -> Vec<PanelEntry> {
        let mut entries = vec![
            PanelEntry {
                tab: Tab::new(KERNEL, "features"),
                title: "Features".to_owned(),
                area: Area::Bottom,
                open_by_default: true,
            },
            PanelEntry {
                tab: Tab::new(KERNEL, "log"),
                title: "Log".to_owned(),
                area: Area::Bottom,
                open_by_default: true,
            },
            PanelEntry {
                tab: Tab::new(KERNEL, "jobs"),
                title: "Jobs".to_owned(),
                area: Area::Bottom,
                open_by_default: true,
            },
            PanelEntry {
                tab: Tab::new(KERNEL, "commands"),
                title: "Commands".to_owned(),
                area: Area::Bottom,
                open_by_default: true,
            },
        ];
        for slot in self.slots.iter().filter(|s| s.state.is_running()) {
            for panel in &slot.panels {
                entries.push(PanelEntry {
                    tab: Tab::new(&slot.id, &panel.id),
                    title: panel.title.clone(),
                    area: panel.area,
                    open_by_default: panel.open_by_default,
                });
            }
        }
        entries
    }

    fn restore_layout(&self) -> DockState<Tab> {
        let settings = &self.host.settings;
        layout::restore(settings.layout.clone(), &self.panel_entries(), &settings.closed_panels)
    }

    fn apply_pending(&mut self) {
        self.apply_pending_for(None);
    }

    /// Applies the queued undoable commands. Those applied for a caller with an undo group open on
    /// the calling thread go into the group instead of the history.
    fn apply_pending_for(&mut self, caller: Option<(&str, ThreadId)>) {
        for (owner, mut command) in std::mem::take(&mut self.host.pending) {
            let Some(index) = self.running_index(&owner) else {
                continue;
            };
            let feature = self.slots[index]
                .feature
                .as_deref_mut()
                .expect("running features are loaded");
            match guarded_as(&owner, || command.apply(feature)) {
                Ok(()) => match caller.and_then(|(caller, thread)| self.groups.parts_of(caller, thread)) {
                    Some(parts) => parts.push(history::Part { owner, command }),
                    None => self.history.push(owner, command),
                },
                Err(message) => {
                    let label = command.label();
                    self.fail(index, format!("command '{label}': {message}"));
                }
            }
        }
    }

    /// Disables the features that another feature reported as failed.
    fn apply_reported(&mut self) {
        for reported in std::mem::take(&mut self.host.reported) {
            let Some(index) = self.running_index(&reported.culprit) else {
                continue;
            };
            let message = format!("reported by '{}': {}", reported.reporter, reported.message);
            self.fail(index, message);
        }
    }

    /// Reverts the last entry, its commands in reverse order. A command whose revert fails makes
    /// its feature fail; the other commands of the entry are reverted all the same. Refused while
    /// an undo group is open.
    fn undo(&mut self) {
        if let Some(reason) = self.groups.blocking_undo() {
            log::warn!("Undo ignored: {reason}");
            return;
        }
        let running = self.running_ids();
        let Some(entry) = self.history.take_undo(|id| running.contains(id)) else {
            return;
        };
        let parts = self.replay(entry.parts.into_iter().rev().collect(), true);
        let parts: Vec<history::Part> = parts.into_iter().rev().collect();
        if !parts.is_empty() {
            self.history.undone.push(history::Entry {
                label: entry.label,
                parts,
            });
        }
    }

    fn redo(&mut self) {
        if let Some(reason) = self.groups.blocking_undo() {
            log::warn!("Redo ignored: {reason}");
            return;
        }
        let running = self.running_ids();
        let Some(entry) = self.history.take_redo(|id| running.contains(id)) else {
            return;
        };
        let parts = self.replay(entry.parts, false);
        if !parts.is_empty() {
            self.history.done.push(history::Entry {
                label: entry.label,
                parts,
            });
        }
    }

    /// Reverts or applies `parts` in the given order and returns those that succeeded.
    fn replay(&mut self, parts: Vec<history::Part>, revert: bool) -> Vec<history::Part> {
        let mut done = Vec::new();
        for mut part in parts {
            // An earlier part may have made this feature fail.
            let Some(index) = self.running_index(&part.owner) else {
                continue;
            };
            let feature = self.slots[index]
                .feature
                .as_deref_mut()
                .expect("running features are loaded");
            let outcome = guarded_as(&part.owner, || {
                if revert {
                    part.command.revert(feature)
                } else {
                    part.command.apply(feature)
                }
            });
            match outcome {
                Ok(()) => done.push(part),
                Err(message) => {
                    let verb = if revert { "undo" } else { "redo" };
                    self.fail(index, format!("{verb} of '{}': {message}", part.command.label()));
                }
            }
        }
        done
    }

    /// Disables a feature that failed, withdraws its services, drops its history entries and
    /// tells the others.
    fn fail(&mut self, index: usize, message: String) {
        let slot = &mut self.slots[index];
        log::error!("feature '{}' failed: {message}", slot.id);
        slot.state = State::Failed(message);
        let id = slot.id.clone();
        self.host.services.retain(|_, s| s.provider != id);
        self.sync_running();
        let purged = self.history.purge(&id) + self.groups.purge(&id);
        if purged > 0 {
            log::warn!("{purged} changes of '{id}' can no longer be undone");
        }
        // Its scripts or modules will not end their groups: what they changed elsewhere stays
        // undoable.
        for closed in self.groups.close_feature(&id) {
            log::warn!("undo group '{}' closed: '{id}' failed", closed.label);
            self.push_closed(closed);
        }
        self.host
            .publish(KERNEL, FEATURE_FAILED_TOPIC, serde_json::json!({ "id": id }));
    }

    fn push_closed(&mut self, closed: Closed) {
        self.history.push_group(closed.label, closed.parts);
    }

    /// Moves what other threads left for the interface thread: events and failures of commands.
    fn collect_from_threads(&mut self) {
        let bridge = self.host.bridge.clone();
        let events = std::mem::take(&mut *bridge.events.lock().unwrap_or_else(|e| e.into_inner()));
        self.host.events.extend(events);
        let failures = std::mem::take(&mut *bridge.failures.lock().unwrap_or_else(|e| e.into_inner()));
        self.host.reported.extend(failures);
    }

    /// Hands the jobs that ended back to their features.
    fn deliver_jobs(&mut self) {
        for finished in self.host.pool.take_finished() {
            let Some(index) = self.running_index(&finished.owner) else {
                log::warn!(
                    "job '{}' of '{}' ended after its feature stopped",
                    finished.label,
                    finished.owner
                );
                continue;
            };
            let (id, outcome) = (finished.id, finished.outcome);
            if let Err(message) = call_feature(&mut self.slots[index], &mut self.host, |f, ctx| {
                f.on_job(id, outcome, ctx)
            }) {
                self.fail(index, format!("job '{}': {message}", finished.label));
            }
        }
    }

    /// Answers the queued calls for at most `CALL_BUDGET` (T4), then delivers the answers due to
    /// features.
    fn serve_calls(&mut self) {
        let Some(requests) = self.requests.take() else {
            return;
        };
        router::serve(&requests, CALL_BUDGET, CALL_IDLE, |request| self.answer(request));
        self.requests = Some(requests);
        self.apply_reported();
        for (caller, call, result) in std::mem::take(&mut self.replies) {
            let Some(index) = self.running_index(&caller) else {
                continue;
            };
            if let Err(message) = call_feature(&mut self.slots[index], &mut self.host, |f, ctx| {
                f.on_reply(call, result, ctx)
            }) {
                self.fail(index, format!("reply: {message}"));
            }
        }
    }

    fn answer(&mut self, request: Request) {
        match request {
            Request::Call {
                caller,
                thread,
                name,
                arguments,
                reply,
            } => {
                let result = self.run_command(&caller, thread, &name, arguments);
                if let Err(error) = &result {
                    log::warn!("call of '{name}' by '{caller}' failed: {error}");
                }
                match reply {
                    ReplyTo::Thread(reply) => {
                        // The caller may have given up; nothing to do then.
                        let _ = reply.send(result);
                    }
                    ReplyTo::Feature(caller, call) => self.replies.push((caller, call, result)),
                    ReplyTo::Kernel(call) => self.commands_panel.answer(call, result),
                }
            }
            Request::Setting { caller, key, reply } => {
                let _ = reply.send(self.host.setting(feature_of(&caller), &key));
            }
            Request::SetSetting { caller, key, value } => {
                self.host.set_setting(feature_of(&caller), &key, value);
            }
            Request::BeginGroup { caller, thread, label } => self.groups.begin(&caller, thread, &label),
            Request::EndGroup { caller, thread } => match self.groups.end(&caller, thread) {
                Ended::Closed(closed) => self.push_closed(closed),
                Ended::StillOpen => {}
                Ended::NotOpen => log::warn!("'{caller}' ended an undo group it had not opened"),
            },
            Request::ThreadEnded { thread } => {
                for closed in self.groups.close_thread(thread) {
                    log::warn!("undo group '{}' closed: its job ended without ending it", closed.label);
                    self.push_closed(closed);
                }
            }
        }
    }

    fn run_command(
        &mut self,
        caller: &str,
        thread: ThreadId,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let bridge = self.host.bridge.clone();
        let (owner, handler) = bridge.lookup(name)?;
        if let Some(handler) = handler {
            return bridge.run_on_caller(&owner, name, &handler, arguments);
        }
        let index = self
            .running_index(&owner)
            .ok_or_else(|| format!("'{name}' belongs to '{owner}', which is not running"))?;
        let outcome = call_feature(&mut self.slots[index], &mut self.host, |f, ctx| {
            f.on_command(name, arguments, ctx)
        });
        match outcome {
            Ok(result) => {
                self.apply_pending_for(Some((caller, thread)));
                result
            }
            Err(panic) => {
                self.fail(index, format!("command '{name}': {panic}"));
                Err(format!("'{name}' failed: {panic}"))
            }
        }
    }

    fn running_ids(&self) -> HashSet<String> {
        self.slots
            .iter()
            .filter(|s| s.state.is_running())
            .map(|s| s.id.clone())
            .collect()
    }

    /// Delivers the events published this frame. Events published meanwhile wait for the next one.
    fn dispatch_events(&mut self) {
        for event in std::mem::take(&mut self.host.events) {
            self.host.bridge.deliver(&event);
            for index in 0..self.slots.len() {
                let slot = &self.slots[index];
                if !slot.state.is_running() || !slot.subscribed_to(&event.topic) {
                    continue;
                }
                if let Err(message) =
                    call_feature(&mut self.slots[index], &mut self.host, |f, ctx| f.on_event(&event, ctx))
                {
                    let topic = &event.topic;
                    self.fail(index, format!("event '{topic}': {message}"));
                }
            }
        }
        self.apply_pending();
    }

    fn running_index(&self, id: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.id == id && s.state.is_running())
    }

    fn set_panel_open(&mut self, tab: &Tab, open: bool) {
        if open {
            self.host.settings.closed_panels.remove(&tab.key());
            let entries = self.panel_entries();
            if self.dock.find_tab(tab).is_none()
                && let Some(entry) = entries.iter().find(|e| &e.tab == tab)
            {
                layout::place(&mut self.dock, &entries, entry);
            }
        } else {
            self.host.settings.closed_panels.insert(tab.key());
            if let Some(path) = self.dock.find_tab(tab) {
                self.dock.remove_tab(path);
            }
        }
        self.host.settings_changed = true;
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) -> Vec<MenuAction> {
        let mut actions = Vec::new();
        let entries = self.panel_entries();
        let mut feature_menus: Vec<String> = Vec::new();
        for slot in self.slots.iter().filter(|s| s.state.is_running()) {
            for item in &slot.menu_items {
                if !BUILT_IN_MENUS.contains(&item.menu.as_str()) && !feature_menus.contains(&item.menu) {
                    feature_menus.push(item.menu.clone());
                }
            }
        }

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                self.feature_items(ui, "File", &mut actions);
            });
            ui.menu_button("Edit", |ui| {
                let blocked = self.groups.blocking_undo();
                if history_button(ui, "Undo", "Ctrl+Z", self.history.undo_label(), blocked.as_deref()) {
                    actions.push(MenuAction::Undo);
                }
                if history_button(ui, "Redo", "Ctrl+Y", self.history.redo_label(), blocked.as_deref()) {
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
                self.feature_items(ui, "Edit", &mut actions);
            });
            ui.menu_button("Window", |ui| {
                for entry in &entries {
                    let mut open = self.dock.find_tab(&entry.tab).is_some();
                    let label = if entry.tab.feature == KERNEL {
                        entry.title.clone()
                    } else {
                        format!("{} ({})", entry.title, entry.tab.feature)
                    };
                    if ui.checkbox(&mut open, label).changed() {
                        actions.push(MenuAction::SetPanelOpen(entry.tab.clone(), open));
                    }
                }
                self.feature_items(ui, "Window", &mut actions);
            });
            for menu in &feature_menus {
                ui.menu_button(menu.as_str(), |ui| self.feature_items(ui, menu, &mut actions));
            }
            ui.menu_button("Help", |ui| {
                ui.label(format!("UniWoW {}", env!("CARGO_PKG_VERSION")));
                ui.label(format!("Runtime {}", short(self.runtime_fingerprint.as_deref())));
                self.feature_items(ui, "Help", &mut actions);
            });
        });
        actions
    }

    fn feature_items(&self, ui: &mut egui::Ui, menu: &str, actions: &mut Vec<MenuAction>) {
        let mut first = BUILT_IN_MENUS.contains(&menu);
        for (index, slot) in self.slots.iter().enumerate().filter(|(_, s)| s.state.is_running()) {
            for item in slot.menu_items.iter().filter(|i| i.menu == menu) {
                if std::mem::take(&mut first) {
                    ui.separator();
                }
                if ui.button(&item.label).clicked() {
                    actions.push(MenuAction::Feature(index, item.action.clone()));
                }
            }
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let running = self.slots.iter().filter(|s| s.state.is_running()).count();
        let others = self.slots.len() - running;
        ui.horizontal(|ui| {
            ui.label(format!("{running} features running"));
            if others > 0 {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("{others} not running (see Features)"),
                );
            }
            ui.separator();
            ui.label(format!("{} changes in history", self.history.done.len()));
            let open: Vec<String> = self.groups.list().into_iter().map(|(_, label)| label).collect();
            if !open.is_empty() {
                ui.separator();
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Undo groups open: {}", open.join(", ")),
                );
            }
        });
    }
}

enum MenuAction {
    Undo,
    Redo,
    CloseGroup(u64),
    SetPanelOpen(Tab, bool),
    Feature(usize, String),
}

impl eframe::App for Shell {
    /// The work of the kernel. eframe calls it before each `ui`, and also while the window is
    /// minimised whenever a repaint is requested, as other threads do when they need the kernel.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_pending();
        self.apply_reported();
        self.serve_calls();
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
        if self.host.settings_changed {
            let since = self.last_settings_save.elapsed();
            if since >= SETTINGS_SAVE_INTERVAL {
                self.host.settings.save();
                self.host.settings_changed = false;
                self.last_settings_save = Instant::now();
            } else {
                ctx.request_repaint_after(SETTINGS_SAVE_INTERVAL - since);
            }
        }
    }

    /// Draws the window and handles what the user did in it; the rest waits for `logic`.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut actions = Vec::new();
        egui::Panel::top("menu_bar").show(ui, |ui| actions = self.menu_bar(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));

        // Every registered panel, so that the tab of a feature that failed keeps its title.
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
            features_dir: &self.features_dir,
            restart_needed: &mut self.restart_needed,
            commands_panel: &mut self.commands_panel,
        };
        let mut shown = Ok(());
        egui::CentralPanel::default().show(ui, |ui| {
            if let PanelsHealth::Broken(reason) = &self.panels {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("The panels cannot be displayed: {reason}. See the log."),
                );
                return;
            }
            shown = guarded(|| {
                DockArea::new(&mut self.dock)
                    .style(Style::from_egui(ui.style()))
                    .show_inside(ui, &mut viewer);
            });
        });
        let Viewer { failures, closed, .. } = viewer;
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

        let ctx = ui.ctx().clone();
        if !ctx.egui_wants_keyboard_input() {
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
                MenuAction::Feature(index, action) => {
                    if self.slots[index].state.is_running()
                        && let Err(message) =
                            call_feature(&mut self.slots[index], &mut self.host, |f, ctx| f.on_menu(&action, ctx))
                    {
                        self.fail(index, format!("menu '{action}': {message}"));
                    }
                }
            }
        }

        // What the panels and menus queued is applied by `logic` at the next frame.
        let host = &self.host;
        if !host.pending.is_empty() || !host.reported.is_empty() || !host.events.is_empty() || host.settings_changed {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self) {
        for slot in self.slots.iter_mut().filter(|s| s.state.is_running()) {
            let feature = slot.feature.as_deref_mut().expect("running features are loaded");
            if let Err(message) = guarded_as(&slot.id, || feature.shutdown()) {
                log::error!("feature '{}' failed in shutdown: {message}", slot.id);
            }
        }
        self.host.settings.layout = serde_json::to_value(&self.dock).ok();
        self.host.settings.save();
    }
}

struct Viewer<'a> {
    slots: &'a mut [Slot],
    host: &'a mut KernelHost,
    titles: &'a [(Tab, String)],
    failures: Vec<(usize, String)>,
    closed: Vec<Tab>,
    runtime: Option<&'a str>,
    features_dir: &'a Path,
    restart_needed: &'a mut bool,
    commands_panel: &'a mut CommandsPanel,
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
        if tab.feature == KERNEL {
            match tab.panel.as_str() {
                "features" => self.features_panel(ui),
                "log" => log_panel(ui),
                "jobs" => panels::jobs_panel(ui, &self.host.pool),
                "commands" => self.commands_panel.ui(ui, &self.host.bridge),
                _ => {}
            }
            return;
        }
        let Some(index) = self.slots.iter().position(|s| s.id == tab.feature) else {
            ui.label(format!("The feature '{}' is not present.", tab.feature));
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
        if let Err(message) = call_feature(slot, self.host, |f, ctx| f.panel_ui(&panel, ui, ctx)) {
            self.failures.push((index, format!("panel '{panel}': {message}")));
        }
    }

    fn on_close(&mut self, tab: &mut Tab) -> OnCloseResponse {
        self.closed.push(tab.clone());
        OnCloseResponse::Close
    }
}

impl Viewer<'_> {
    fn features_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!("Runtime {}", short(self.runtime)));
            ui.separator();
            ui.label(self.features_dir.display().to_string());
        });
        if *self.restart_needed {
            ui.colored_label(ui.visuals().warn_fg_color, "Restart UniWoW to apply the changes.");
        }
        ui.separator();
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("features").striped(true).num_columns(7).show(ui, |ui| {
                for header in ["On", "Feature", "Category", "Id", "Version", "State", "Folder"] {
                    ui.strong(header);
                }
                ui.end_row();
                for slot in self.slots.iter() {
                    let mut enabled = !self.host.settings.disabled_features.contains(&slot.id);
                    let toggle = ui.add_enabled(slot.manifest.is_some(), egui::Checkbox::without_text(&mut enabled));
                    if toggle.changed() {
                        if enabled {
                            self.host.settings.disabled_features.remove(&slot.id);
                        } else {
                            self.host.settings.disabled_features.insert(slot.id.clone());
                        }
                        self.host.settings_changed = true;
                        *self.restart_needed = true;
                    }
                    let name = ui.label(slot.name());
                    if let Some(manifest) = &slot.manifest
                        && !manifest.description.is_empty()
                    {
                        name.on_hover_text(&manifest.description);
                    }
                    ui.label(slot.manifest.as_ref().map_or("", |m| m.category.as_str()));
                    ui.label(&slot.id);
                    ui.label(slot.manifest.as_ref().map_or("", |m| m.version.as_str()));
                    let color = match slot.state {
                        State::Running => egui::Color32::from_rgb(90, 170, 90),
                        State::Disabled => ui.visuals().weak_text_color(),
                        State::Ignored(_) => ui.visuals().warn_fg_color,
                        _ => ui.visuals().error_fg_color,
                    };
                    ui.colored_label(color, state_text(&slot.state));
                    ui.label(slot.folder.file_name().unwrap_or_default().to_string_lossy());
                    ui.end_row();
                }
            });
        });
    }
}

fn log_panel(ui: &mut egui::Ui) {
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

/// An Undo or Redo entry of the Edit menu, greyed with the reason when `blocked`; returns whether
/// it was clicked.
fn history_button(ui: &mut egui::Ui, verb: &str, shortcut: &str, label: Option<String>, blocked: Option<&str>) -> bool {
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

/// Calls the feature of `slot` with a context, catching panics.
fn call_feature<R>(
    slot: &mut Slot,
    host: &mut KernelHost,
    f: impl FnOnce(&mut dyn Feature, &mut Context) -> R,
) -> Result<R, String> {
    let Slot { id, feature, .. } = slot;
    let feature = feature.as_deref_mut().expect("running features are loaded");
    let mut ctx = Context::new(host, id);
    guarded_as(id, || f(feature, &mut ctx))
}

fn state_text(state: &State) -> String {
    match state {
        State::Running => "running".to_owned(),
        State::Disabled => "disabled".to_owned(),
        State::Ignored(reason) => format!("ignored: {reason}"),
        State::Refused(reason) => format!("refused: {reason}"),
        State::Blocked(reason) => format!("not loaded: {reason}"),
        State::Failed(reason) => format!("failed: {reason}"),
    }
}

fn short(fingerprint: Option<&str>) -> String {
    fingerprint.map_or("unavailable".to_owned(), |f| f.chars().take(12).collect())
}
