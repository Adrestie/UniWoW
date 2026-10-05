//! The shell: the editor's window, its modules, and the kernel's work at each frame.

mod calls;
mod closing;
mod frame;
mod history_ops;
mod hotkey_window;
mod menus;
mod modules_panel;
mod tabs;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use uniwow_api::egui_dock::tab_viewer::OnCloseResponse;
use uniwow_api::egui_dock::{DockArea, DockState, Style, TabViewer};
use uniwow_api::{
    CallId, CommandHandler, CommandInfo, Context, DIALOG_ANSWERED_TOPIC, DIALOG_COMMAND, DockArea as Area, Event, Host,
    MODULE_FAILED_TOPIC, Module, PropertyInfo, Registrar, RunsOn, eframe, egui, log, range_error, serde_json,
};

use crate::capi;
use crate::groups::{Closed, Ended, Groups};
use crate::guard::{guarded, guarded_as};
use crate::history::{self, History};
use crate::host::{KernelHost, Service};
use crate::hotkeys::Hotkeys;
use crate::jobs::Pool;
use crate::layout::{self, PanelEntry, Tab};
use crate::loader::{self, Slot, State};
use crate::logger;
use crate::order;
use crate::panels::{self, CommandsPanel};
use crate::players::{Players, Store};
use crate::requirements::{self, Need};
use crate::router::{self, Bridge, Entry, PropertyEntry, ReplyTo, Request};
use crate::settings::Settings;

use history_ops::Recorded;
use hotkey_window::HotkeyWindow;
use menus::MenuAction;
use modules_panel::{short, state_text};
use tabs::{Viewer, log_panel};

#[cfg(test)]
mod tests;

const KERNEL: &str = "kernel";
const BUILT_IN_MENUS: [&str; 4] = ["File", "Edit", "Window", "Help"];
/// Settings changed by modules are written at most this often, and once more at exit.
const SETTINGS_SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// Time the interface thread spends each frame answering calls of other threads.
const CALL_BUDGET: Duration = Duration::from_millis(4);
/// The same while the window is minimised: eframe then wakes the editor at most every 100 ms, and
/// nothing is drawn, so most of that time can go to the calls.
const MINIMISED_CALL_BUDGET: Duration = Duration::from_millis(80);
/// After a call, how long to wait for the next one of a thread calling in a loop.
const CALL_IDLE: Duration = Duration::from_micros(500);
/// How long a compiled module's thread may run one job before the Modules panel shows it as not
/// responding.
const NOT_RESPONDING: Duration = Duration::from_secs(3);

pub struct Shell {
    host: KernelHost,
    slots: Vec<Slot>,
    dock: DockState<Tab>,
    history: History,
    runtime_fingerprint: Option<String>,
    modules_dir: PathBuf,
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
    closing: Closing,
    players: Players,
    hotkeys: Hotkeys,
    hotkey_window: HotkeyWindow,
}

/// Where the closing of the editor stands while modules have unsaved changes.
enum Closing {
    Open,
    /// The user is asked, in the dialog window of this number.
    Asking(u64),
    /// The user said to close.
    Confirmed,
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
        uniwow_api::ui::set_background(pool.background());
        uniwow_api::parallel::set_workers(pool.workers());
        let host = KernelHost::new(cc.wgpu_render_state.clone(), Settings::load(), pool, bridge);
        // wgpu panics on errors nobody captured; log them instead, the editor must keep running.
        if let Some(gpu) = &host.gpu {
            gpu.device.on_uncaptured_error(Arc::new(|error| {
                log::error!("GPU error not captured by any module: {error}");
            }));
            let info = gpu.adapter.get_info();
            match host.gpu_memory {
                Some(bytes) => log::info!("GPU: {} ({:?}), {} MB of its own", info.name, info.backend, bytes >> 20),
                None => log::info!("GPU: {} ({:?}), its memory not told", info.name, info.backend),
            }
        }
        let discovery = loader::discover(&exe_dir, &host.settings.disabled_modules);
        Self::start(
            host,
            requests,
            discovery.slots,
            discovery.runtime_fingerprint,
            exe_dir.join("modules"),
        )
    }

    /// Starts the modules of `slots` found in `modules_dir`, the kernel's state in `host`, the
    /// calls of other threads coming through `requests`.
    fn start(
        host: KernelHost,
        requests: mpsc::Receiver<Request>,
        slots: Vec<Slot>,
        runtime_fingerprint: Option<String>,
        modules_dir: PathBuf,
    ) -> Self {
        let hotkeys = Hotkeys::new(&host.settings.hotkeys);
        let mut shell = Self {
            host,
            slots,
            dock: DockState::new(Vec::new()),
            history: History::default(),
            runtime_fingerprint,
            modules_dir,
            restart_needed: false,
            last_settings_save: Instant::now(),
            panels: PanelsHealth::Drawn,
            requests: Some(requests),
            replies: Vec::new(),
            commands_panel: CommandsPanel::default(),
            groups: Groups::new(std::thread::current().id()),
            closing: Closing::Open,
            players: Players::default(),
            hotkeys,
            hotkey_window: HotkeyWindow::default(),
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
        // Name conflicts are settled once every module has declared its commands.
        let mut declared = Vec::new();
        for slot in &mut self.slots {
            let Some(module) = slot.module.as_deref_mut().filter(|_| slot.state.is_running()) else {
                continue;
            };
            let mut reg = Registrar::default();
            if let Err(message) = guarded_as(&slot.id, || module.register(&mut reg)) {
                log::error!("module '{}' failed in register: {message}", slot.id);
                slot.state = State::Failed(format!("register: {message}"));
                continue;
            }
            slot.panels = reg.panels;
            slot.menu_items = reg.menu_items;
            slot.subscriptions = reg.subscriptions;
            for spec in reg.hotkeys {
                self.hotkeys.declare(&slot.id, spec, &self.host.settings.hotkeys);
            }
            declared.extend(reg.commands.into_iter().map(|spec| (slot.id.clone(), spec)));
            let mut properties = self.host.bridge.properties.write().unwrap_or_else(|e| e.into_inner());
            for spec in reg.properties {
                let path = format!("{}/{}", slot.id, spec.name);
                // Its module's mistake, refused here: clamping by it would fail in whoever writes.
                if let Some(reason) = range_error(spec.range) {
                    log::warn!("property '{path}' refused: {reason}");
                    continue;
                }
                let info = PropertyInfo {
                    path: path.clone(),
                    owner: slot.id.clone(),
                    label: spec.label,
                    kind: spec.kind,
                    range: spec.range,
                };
                let entry = PropertyEntry {
                    info,
                    read: spec.read,
                    write: spec.write,
                };
                if properties.insert(path.clone(), entry).is_some() {
                    log::warn!("property '{path}' declared twice: the last one is kept");
                }
            }
            drop(properties);
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
        let names: Vec<(String, String)> = declared
            .iter()
            .map(|(owner, spec)| (owner.clone(), spec.name.clone()))
            .collect();
        let kept = router::choose_commands(declared);
        // Each module keeps the list of its commands, those set aside with the module that won.
        let winners: HashMap<&str, &str> = kept
            .iter()
            .map(|(owner, spec)| (spec.name.as_str(), owner.as_str()))
            .collect();
        for slot in &mut self.slots {
            slot.commands = names
                .iter()
                .filter(|(owner, _)| *owner == slot.id)
                .map(|(_, name)| {
                    let refused = winners
                        .get(name.as_str())
                        .filter(|winner| **winner != slot.id)
                        .map(|winner| format!("offered by '{winner}'"));
                    (name.clone(), refused)
                })
                .collect();
        }
        drop(winners);
        let mut catalogue = self.host.bridge.catalogue.write().unwrap_or_else(|e| e.into_inner());
        for (owner, spec) in kept {
            let handler = match spec.runs_on {
                RunsOn::Interface => None,
                RunsOn::Caller(handler) => Some(handler),
            };
            let info = CommandInfo {
                name: spec.name.clone(),
                owner,
                description: spec.description,
                arguments: spec.arguments,
                result: spec.result,
                on_caller: handler.is_some(),
            };
            catalogue.insert(spec.name, Entry { info, handler });
        }
    }

    /// Blocks the modules whose required services are missing, until nothing changes.
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
                format!("requires the service '{service}', which no running module provides"),
            );
        }
    }

    /// The first service a running module requires and nobody provides.
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
        // What the interface kept of its objects goes, their textures with it.
        self.host.views.retain(|(owner, _), _| *owner != id);
        self.sync_running();
    }

    /// Tells the bridge which modules are running: only their commands can be called.
    fn sync_running(&self) {
        *self.host.bridge.running.write().unwrap_or_else(|e| e.into_inner()) = self.running_ids();
    }

    /// Running modules, providers of a service before the modules that require or use it.
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
                "dependency cycle between modules: {} initialised before the providers they use",
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
            if let Err(message) = call_module(&mut self.slots[index], &mut self.host, |f, ctx| f.init(ctx)) {
                self.fail(index, format!("init: {message}"));
            }
            self.apply_pending();
            self.apply_reported();
        }
    }

    fn log_summary(&self) {
        let running = self.slots.iter().filter(|s| s.state.is_running()).count();
        log::info!(
            "{running} of {} modules running, runtime {}",
            self.slots.len(),
            short(self.runtime_fingerprint.as_deref())
        );
        for slot in &self.slots {
            if !slot.state.is_running() {
                log::warn!("module '{}': {}", slot.id, state_text(&slot.state));
            }
        }
    }

    fn panel_entries(&self) -> Vec<PanelEntry> {
        let mut entries = vec![
            PanelEntry {
                tab: Tab::new(KERNEL, "modules"),
                title: "Modules".to_owned(),
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

    /// Disables the modules that another module reported as failed.
    fn apply_reported(&mut self) {
        for reported in std::mem::take(&mut self.host.reported) {
            let Some(index) = self.running_index(&reported.culprit) else {
                continue;
            };
            let message = format!("reported by '{}': {}", reported.reporter, reported.message);
            self.fail(index, message);
        }
    }

    /// Disables a module that failed, withdraws its services, drops its history entries and
    /// tells the others.
    fn fail(&mut self, index: usize, message: String) {
        let slot = &mut self.slots[index];
        log::error!("module '{}' failed: {message}", slot.id);
        // What it had not saved can no longer be: at least the log says what.
        if let Some(module) = slot.module.as_deref()
            && let Ok(lost) = guarded(|| module.unsaved())
            && !lost.is_empty()
        {
            log::warn!(
                "'{}' failed with unsaved changes, now lost: {}",
                slot.id,
                lost.join(", ")
            );
        }
        slot.state = State::Failed(message);
        let id = slot.id.clone();
        self.host.services.retain(|_, s| s.provider != id);
        self.sync_running();
        // Its jobs and script runs stop; from now on the bridge refuses whatever they still ask.
        self.host.pool.cancel_owner(&id);
        self.host.bridge.close_subscriptions(&id);
        let purged = self.history.purge(&id) + self.groups.purge(&id);
        if purged > 0 {
            log::warn!("{purged} changes of '{id}' can no longer be undone");
        }
        // Its scripts or modules will not end their groups: what they changed elsewhere stays
        // undoable.
        for closed in self.groups.close_module(&id) {
            log::warn!("undo group '{}' closed: '{id}' failed", closed.label);
            self.push_closed(closed);
        }
        self.host
            .publish(KERNEL, MODULE_FAILED_TOPIC, serde_json::json!({ "id": id }));
    }

    fn running_ids(&self) -> HashSet<String> {
        self.slots
            .iter()
            .filter(|s| s.state.is_running())
            .map(|s| s.id.clone())
            .collect()
    }

    /// The interface objects the kernel adopted, of the modules still running.
    fn adopted_stores(&mut self) -> Vec<Store> {
        let running = self.running_ids();
        self.host.adopted.retain(|(_, objects)| objects.strong_count() > 0);
        self.host
            .adopted
            .iter()
            .filter(|(owner, _)| running.contains(owner))
            .filter_map(|(owner, objects)| {
                Some(Store {
                    owner: owner.clone(),
                    objects: objects.upgrade()?,
                    editor: self.host.editor(owner),
                })
            })
            .collect()
    }

    /// Moves the players on and writes their values; frames keep coming while one plays.
    fn play(&mut self, ctx: &egui::Context, now: Instant) {
        let stores = self.adopted_stores();
        if self.players.tick(&stores, now) {
            ctx.request_repaint();
        }
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
}

/// Calls the module of `slot` with a context, catching panics.
fn call_module<R>(
    slot: &mut Slot,
    host: &mut KernelHost,
    f: impl FnOnce(&mut dyn Module, &mut Context) -> R,
) -> Result<R, String> {
    let Slot { id, module, .. } = slot;
    let module = module.as_deref_mut().expect("running modules are loaded");
    let mut ctx = Context::new(host, id);
    guarded_as(id, || f(module, &mut ctx))
}
