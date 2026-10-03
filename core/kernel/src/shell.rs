use std::collections::HashSet;
use std::path::{Path, PathBuf};

use uniwow_api::egui_dock::tab_viewer::OnCloseResponse;
use uniwow_api::egui_dock::{DockArea, DockState, Node, NodeIndex, Style, SurfaceIndex, TabPath, TabViewer, Tree};
use uniwow_api::serde::{Deserialize, Serialize};
use uniwow_api::{
    Context, DockArea as Area, FEATURE_FAILED_TOPIC, Feature, Host, Registrar, eframe, egui, log, serde_json,
};

use crate::guard::guarded;
use crate::history::History;
use crate::host::{KernelHost, Service};
use crate::loader::{self, Slot, State};
use crate::logger;
use crate::settings::Settings;

const KERNEL: &str = "kernel";
const BUILT_IN_MENUS: [&str; 4] = ["File", "Edit", "Window", "Help"];

/// A dock tab: one panel of one feature, or of the kernel.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(crate = "uniwow_api::serde")]
pub struct Tab {
    feature: String,
    panel: String,
}

impl Tab {
    fn new(feature: &str, panel: &str) -> Self {
        Self {
            feature: feature.to_owned(),
            panel: panel.to_owned(),
        }
    }

    /// `feature/panel`, as stored in the settings.
    fn key(&self) -> String {
        format!("{}/{}", self.feature, self.panel)
    }
}

struct PanelEntry {
    tab: Tab,
    title: String,
    area: Area,
    open_by_default: bool,
}

pub struct Shell {
    host: KernelHost,
    slots: Vec<Slot>,
    dock: DockState<Tab>,
    history: History,
    runtime_fingerprint: Option<String>,
    features_dir: PathBuf,
    restart_needed: bool,
}

impl Shell {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let host = KernelHost {
            gpu: cc.wgpu_render_state.clone(),
            settings: Settings::load(),
            ..Default::default()
        };
        let discovery = loader::discover(&exe_dir, &host.settings.disabled_features);
        let mut shell = Self {
            host,
            slots: discovery.slots,
            dock: DockState::new(Vec::new()),
            history: History::default(),
            runtime_fingerprint: discovery.runtime_fingerprint,
            features_dir: exe_dir.join("features"),
            restart_needed: false,
        };
        shell.register_all();
        shell.resolve_requirements();
        shell.init_all();
        // A damaged saved layout must never prevent the editor from starting.
        shell.dock = guarded(|| shell.restore_layout()).unwrap_or_else(|message| {
            log::warn!("saved layout discarded: {message}");
            default_layout(&shell.panel_entries(), &shell.host.settings.closed_panels)
        });
        shell.apply_pending();
        shell.log_summary();
        shell
    }

    fn register_all(&mut self) {
        for slot in &mut self.slots {
            let Some(feature) = slot.feature.as_deref_mut().filter(|_| slot.state.is_running()) else {
                continue;
            };
            let mut reg = Registrar::default();
            if let Err(message) = guarded(|| feature.register(&mut reg)) {
                log::error!("feature '{}' failed in register: {message}", slot.id);
                slot.state = State::Failed(format!("register: {message}"));
                continue;
            }
            slot.panels = reg.panels;
            slot.menu_items = reg.menu_items;
            slot.subscriptions = reg.subscriptions;
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
        loop {
            let mut changed = false;
            for slot in &mut self.slots {
                let Some(manifest) = slot.manifest.as_ref().filter(|_| slot.state.is_running()) else {
                    continue;
                };
                let missing = manifest
                    .requires
                    .iter()
                    .find(|s| !self.host.services.contains_key(*s))
                    .cloned();
                if let Some(service) = missing {
                    slot.state = State::Blocked(format!(
                        "requires the service '{service}', which no running feature provides"
                    ));
                    self.host.services.retain(|_, s| s.provider != slot.id);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
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
        let mut order = Vec::new();
        let mut remaining = running;
        while !remaining.is_empty() {
            let ready = remaining
                .iter()
                .position(|&i| providers_of(i).iter().all(|p| order.contains(p)))
                .unwrap_or(0);
            order.push(remaining.remove(ready));
        }
        order
    }

    fn init_all(&mut self) {
        for index in self.init_order() {
            if let Err(message) = call_feature(&mut self.slots[index], &mut self.host, |f, ctx| f.init(ctx)) {
                fail(&mut self.slots, &mut self.host, index, format!("init: {message}"));
            }
            self.apply_pending();
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

    /// The saved layout without the panels of absent features, plus the panels of features that
    /// came back, unless the user closed them.
    fn restore_layout(&self) -> DockState<Tab> {
        let entries = self.panel_entries();
        let closed = &self.host.settings.closed_panels;
        let saved = self
            .host
            .settings
            .layout
            .clone()
            .and_then(|v| serde_json::from_value::<DockState<Tab>>(v).ok());
        let Some(mut dock) = saved else {
            return default_layout(&entries, closed);
        };
        let known: HashSet<Tab> = entries.iter().map(|e| e.tab.clone()).collect();
        // One tab at a time, as when the user closes them: removing many at once with
        // `retain_tabs` can leave the tree broken.
        let absent: Vec<Tab> = dock
            .iter_all_tabs()
            .map(|(_, tab)| tab.clone())
            .filter(|tab| !known.contains(tab))
            .collect();
        for tab in absent {
            if let Some(path) = dock.find_tab(&tab) {
                dock.remove_tab(path);
            }
        }
        if dock.main_surface().num_tabs() == 0 || !is_consistent(dock.main_surface()) {
            return default_layout(&entries, closed);
        }
        let returning: Vec<&PanelEntry> = entries
            .iter()
            .filter(|e| e.open_by_default && !closed.contains(&e.tab.key()) && dock.find_tab(&e.tab).is_none())
            .collect();
        // A whole area came back: the saved arrangement no longer fits, start from the default one.
        if returning
            .iter()
            .any(|e| area_leaf(&dock, &entries, e.area, &e.tab).is_none())
        {
            return default_layout(&entries, closed);
        }
        for entry in returning {
            place(&mut dock, &entries, entry);
        }
        dock
    }

    fn apply_pending(&mut self) {
        for (owner, mut command) in std::mem::take(&mut self.host.pending) {
            let Some(index) = self.running_index(&owner) else {
                continue;
            };
            let feature = self.slots[index]
                .feature
                .as_deref_mut()
                .expect("running features are loaded");
            match guarded(|| command.apply(feature)) {
                Ok(()) => self.history.push(owner, command),
                Err(message) => {
                    let label = command.label();
                    fail(
                        &mut self.slots,
                        &mut self.host,
                        index,
                        format!("command '{label}': {message}"),
                    );
                }
            }
        }
    }

    fn undo(&mut self) {
        while let Some(mut entry) = self.history.done.pop() {
            let Some(index) = self.running_index(&entry.owner) else {
                log::warn!("'{}' skipped: '{}' is not running", entry.command.label(), entry.owner);
                continue;
            };
            let feature = self.slots[index]
                .feature
                .as_deref_mut()
                .expect("running features are loaded");
            match guarded(|| entry.command.revert(feature)) {
                Ok(()) => self.history.undone.push(entry),
                Err(message) => fail(&mut self.slots, &mut self.host, index, format!("undo: {message}")),
            }
            return;
        }
    }

    fn redo(&mut self) {
        while let Some(mut entry) = self.history.undone.pop() {
            let Some(index) = self.running_index(&entry.owner) else {
                continue;
            };
            let feature = self.slots[index]
                .feature
                .as_deref_mut()
                .expect("running features are loaded");
            match guarded(|| entry.command.apply(feature)) {
                Ok(()) => self.history.done.push(entry),
                Err(message) => fail(&mut self.slots, &mut self.host, index, format!("redo: {message}")),
            }
            return;
        }
    }

    /// Delivers the events published this frame. Events published meanwhile wait for the next one.
    fn dispatch_events(&mut self) {
        for event in std::mem::take(&mut self.host.events) {
            for index in 0..self.slots.len() {
                let slot = &self.slots[index];
                if !slot.state.is_running() || !slot.subscribed_to(&event.topic) {
                    continue;
                }
                if let Err(message) =
                    call_feature(&mut self.slots[index], &mut self.host, |f, ctx| f.on_event(&event, ctx))
                {
                    let topic = &event.topic;
                    fail(
                        &mut self.slots,
                        &mut self.host,
                        index,
                        format!("event '{topic}': {message}"),
                    );
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
                place(&mut self.dock, &entries, entry);
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
                let undo = self.history.undo_label();
                let undo_text = undo.map_or("Undo".to_owned(), |l| format!("Undo {l}"));
                if ui
                    .add_enabled(
                        !self.history.done.is_empty(),
                        egui::Button::new(undo_text).shortcut_text("Ctrl+Z"),
                    )
                    .clicked()
                {
                    actions.push(MenuAction::Undo);
                }
                let redo = self.history.redo_label();
                let redo_text = redo.map_or("Redo".to_owned(), |l| format!("Redo {l}"));
                if ui
                    .add_enabled(
                        !self.history.undone.is_empty(),
                        egui::Button::new(redo_text).shortcut_text("Ctrl+Y"),
                    )
                    .clicked()
                {
                    actions.push(MenuAction::Redo);
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
        });
    }
}

enum MenuAction {
    Undo,
    Redo,
    SetPanelOpen(Tab, bool),
    Feature(usize, String),
}

impl eframe::App for Shell {
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
        };
        egui::CentralPanel::default().show(ui, |ui| {
            DockArea::new(&mut self.dock)
                .style(Style::from_egui(ui.style()))
                .show_inside(ui, &mut viewer);
        });
        let Viewer { failures, closed, .. } = viewer;

        for (index, message) in failures {
            fail(&mut self.slots, &mut self.host, index, message);
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
                MenuAction::SetPanelOpen(tab, open) => self.set_panel_open(&tab, open),
                MenuAction::Feature(index, action) => {
                    if self.slots[index].state.is_running()
                        && let Err(message) =
                            call_feature(&mut self.slots[index], &mut self.host, |f, ctx| f.on_menu(&action, ctx))
                    {
                        fail(
                            &mut self.slots,
                            &mut self.host,
                            index,
                            format!("menu '{action}': {message}"),
                        );
                    }
                }
            }
        }

        self.apply_pending();
        self.dispatch_events();
        if !self.host.events.is_empty() {
            ctx.request_repaint();
        }
        if std::mem::take(&mut self.host.settings_changed) {
            self.host.settings.save();
        }
    }

    fn on_exit(&mut self) {
        for index in 0..self.slots.len() {
            if self.slots[index].state.is_running() {
                let feature = self.slots[index]
                    .feature
                    .as_deref_mut()
                    .expect("running features are loaded");
                if let Err(message) = guarded(|| feature.shutdown()) {
                    log::error!("feature '{}' failed in shutdown: {message}", self.slots[index].id);
                }
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
    logger::with_lines(|lines| {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_rows(ui, row_height, lines.len(), |ui, range| {
                for line in lines.range(range) {
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
    });
}

fn default_layout(entries: &[PanelEntry], closed: &std::collections::BTreeSet<String>) -> DockState<Tab> {
    let tabs_in = |area: Area| -> Vec<Tab> {
        entries
            .iter()
            .filter(|e| e.area == area && e.open_by_default && !closed.contains(&e.tab.key()))
            .map(|e| e.tab.clone())
            .collect()
    };
    let mut groups = [Area::Center, Area::Right, Area::Bottom, Area::Left].map(|area| (area, tabs_in(area)));
    // The first non-empty group fills the window; the others are split off around it.
    let Some(first) = groups.iter().position(|(_, tabs)| !tabs.is_empty()) else {
        return DockState::new(Vec::new());
    };
    let mut dock = DockState::new(std::mem::take(&mut groups[first].1));
    let tree = dock.main_surface_mut();
    for (area, tabs) in groups {
        if tabs.is_empty() {
            continue;
        }
        match area {
            Area::Right => {
                tree.split_right(NodeIndex::root(), 0.75, tabs);
            }
            Area::Bottom => {
                tree.split_below(NodeIndex::root(), 0.68, tabs);
            }
            Area::Left => {
                tree.split_left(NodeIndex::root(), 0.22, tabs);
            }
            Area::Center => {}
        }
    }
    dock
}

/// Every split has two non-empty children and every other non-empty node hangs from a split.
fn is_consistent(tree: &Tree<Tab>) -> bool {
    let nodes: Vec<&Node<Tab>> = tree.iter().collect();
    let empty = |i: usize| nodes.get(i).is_none_or(|n| n.is_empty());
    nodes.iter().enumerate().all(|(i, node)| {
        if node.is_parent() {
            !empty(2 * i + 1) && !empty(2 * i + 2)
        } else {
            node.is_empty() || i == 0 || nodes[(i - 1) / 2].is_parent()
        }
    })
}

/// The leaf holding another panel of `area`, on the main surface.
fn area_leaf(dock: &DockState<Tab>, entries: &[PanelEntry], area: Area, except: &Tab) -> Option<TabPath> {
    entries
        .iter()
        .filter(|e| e.area == area && &e.tab != except)
        .find_map(|e| dock.find_tab(&e.tab))
        .filter(|path| path.surface == SurfaceIndex::main())
}

/// Puts a panel next to a panel of the same area, or on its side of the window.
fn place(dock: &mut DockState<Tab>, entries: &[PanelEntry], entry: &PanelEntry) {
    if dock.main_surface().num_tabs() == 0 {
        dock.push_to_first_leaf(entry.tab.clone());
        return;
    }
    if let Some(path) = area_leaf(dock, entries, entry.area, &entry.tab) {
        dock[path.surface][path.node].append_tab(entry.tab.clone());
        return;
    }
    let right = area_leaf(dock, entries, Area::Right, &entry.tab);
    let tabs = vec![entry.tab.clone()];
    let tree = dock.main_surface_mut();
    match entry.area {
        Area::Left => {
            tree.split_left(NodeIndex::root(), 0.22, tabs);
        }
        Area::Right => {
            tree.split_right(NodeIndex::root(), 0.75, tabs);
        }
        Area::Bottom => {
            tree.split_below(NodeIndex::root(), 0.68, tabs);
        }
        Area::Center => match right {
            // The fraction is the share of the left node, here the centre panel.
            Some(path) => {
                tree.split_left(path.node, 0.75, tabs);
            }
            None => dock.push_to_first_leaf(entry.tab.clone()),
        },
    }
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
    guarded(|| f(feature, &mut ctx))
}

/// Disables a feature that failed and tells the others.
fn fail(slots: &mut [Slot], host: &mut KernelHost, index: usize, message: String) {
    let slot = &mut slots[index];
    log::error!("feature '{}' failed: {message}", slot.id);
    slot.state = State::Failed(message);
    host.publish(KERNEL, FEATURE_FAILED_TOPIC, serde_json::json!({ "id": slot.id }));
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
