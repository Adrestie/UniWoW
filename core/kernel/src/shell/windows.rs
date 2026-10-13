//! The panels torn off into windows of their own, outside the main window. Each is a window of the
//! system, drawn by egui at once with the main window (`show_viewport_immediate`), which the user
//! moves to another screen. A tab dragged out of the main window, a tab dropped where the dock
//! makes it a floating window, or *Tear off into its own window* of a tab's menu opens one; closing
//! the window, or its button *Dock back*, puts the panel back in the dock beside the panels of its
//! area. The windows are saved with the layout. Where egui cannot open windows of the system, the
//! floating windows of the dock stay inside the main window, as before.

use uniwow_api::egui_dock::{Surface, SurfaceIndex, WindowState};
use uniwow_api::theme::{self, icon};

use super::*;
use crate::hotkeys::Hotkeys;
use crate::layout::{TORN_LEAST, TORN_SIZE, TornOff};

/// The id of the dock, by which egui names its tabs: a tab's drag is found by it.
pub(super) const DOCK_ID: &str = "egui_dock::DockArea";
/// How far each panel of one floating window opens from the one before, in points.
const CASCADE: f32 = 28.0;
/// Where the window of a tab dragged out opens, from the pointer: its title under it.
const UNDER_POINTER: egui::Vec2 = egui::vec2(-90.0, -14.0);

/// A panel torn off, as it shows.
pub(super) struct TornWindow {
    /// Where its window was last seen, saved with the layout.
    pub seen: TornOff,
    /// What its window was opened with, kept while it shows: egui would move a window whose place
    /// it is given anew at each frame.
    pub opened: TornOff,
    /// Whether its window was told to draw its frame dark.
    pub themed: bool,
}

impl TornWindow {
    pub fn new(window: TornOff) -> Self {
        Self {
            seen: window.clone(),
            opened: window,
            themed: false,
        }
    }
}

/// The id egui gives the floating window of the dock at `surface`: the window's place is kept
/// under it, the dock keeping none.
pub(super) fn floating_id(surface: SurfaceIndex) -> egui::Id {
    egui::Id::new(format!("window {surface:?}"))
}

/// Where a floating window of the dock is in the main window: not drawn yet, where the dock is to
/// draw it, which it keeps to itself but saves; once drawn, `drawn`, as egui keeps it.
pub(super) fn floating_rect(state: &WindowState, drawn: Option<egui::Rect>) -> Option<egui::Rect> {
    let saved = serde_json::to_value(state).ok()?;
    let pair = |key: &str| -> Option<egui::Vec2> {
        let value = saved.get(key)?;
        Some(egui::vec2(
            value.get("x")?.as_f64()? as f32,
            value.get("y")?.as_f64()? as f32,
        ))
    };
    match pair("next_position") {
        Some(min) => {
            let size = pair("next_size").unwrap_or(egui::vec2(TORN_SIZE[0], TORN_SIZE[1]));
            Some(egui::Rect::from_min_size(min.to_pos2(), size))
        }
        None => drawn.filter(|rect| rect.is_finite() && rect.is_positive()),
    }
}

/// The id egui gives the tab at `path` of the dock.
fn tab_id(path: egui_dock::TabPath) -> egui::Id {
    egui::Id::new(DOCK_ID)
        .with((path.surface, "surface"))
        .with((path.node, "node"))
        .with((path.tab.0, "tab"))
}

impl Shell {
    /// Whether panels open in windows of the system: egui opens them, but in the tests, which say
    /// so, and where it embeds them in the main window.
    pub(super) fn native_windows(&self, ctx: &egui::Context) -> bool {
        self.native_windows.unwrap_or_else(|| !ctx.embed_viewports())
    }

    /// Whether `tab` is torn off.
    pub(super) fn is_torn(&self, tab: &Tab) -> bool {
        self.torn.iter().any(|window| window.seen.tab == *tab)
    }

    /// Takes `tab` out of the dock into a window of its own, at `position` on the screens, in
    /// pixels, and of `size`, in points.
    pub(super) fn tear_off(&mut self, tab: Tab, position: Option<[f32; 2]>, size: [f32; 2]) {
        if self.is_torn(&tab) {
            return;
        }
        if let Some(path) = self.dock.find_tab(&tab) {
            self.dock.remove_tab(path);
        }
        log::info!("panel '{}' torn off into a window of its own", tab.key());
        self.torn.push(TornWindow::new(TornOff::new(tab, position, size)));
    }

    /// Puts the panel of the window `index` back in the dock, beside the panels of its area; that
    /// of a module no longer running, in the first place of the dock.
    pub(super) fn dock_back(&mut self, index: usize) {
        if index >= self.torn.len() {
            return;
        }
        let tab = self.torn.remove(index).seen.tab;
        log::info!("panel '{}' docked back", tab.key());
        if self.dock.find_tab(&tab).is_some() {
            return;
        }
        let entries = self.panel_entries();
        match entries.iter().find(|entry| entry.tab == tab) {
            Some(entry) => layout::place(&mut self.dock, &entries, entry),
            None => self.dock.push_to_first_leaf(tab),
        }
    }

    /// Makes the floating windows of the dock windows of the system at the same place on the
    /// screens, `origin` being where the main window's content starts on them, in its points,
    /// `scale` its pixels by point, and `drawn` telling where egui drew each: a panel per window,
    /// those of one floating window each a little lower and to the right of the one before.
    pub(super) fn take_floating(
        &mut self,
        origin: Option<egui::Pos2>,
        scale: f32,
        drawn: impl Fn(SurfaceIndex) -> Option<egui::Rect>,
    ) {
        let floating: Vec<SurfaceIndex> = self
            .dock
            .iter_surfaces_indexed()
            .filter(|(_, surface)| matches!(surface, Surface::Window(..)))
            .map(|(index, _)| index)
            .collect();
        // The last first: removing a surface leaves the indices before it as they are.
        for index in floating.into_iter().rev() {
            let rect = self
                .dock
                .get_window_state(index)
                .and_then(|state| floating_rect(state, drawn(index)));
            let Some(surface) = self.dock.remove_surface(index) else {
                continue;
            };
            let tabs: Vec<Tab> = surface.iter_all_tabs().map(|(_, tab)| tab.clone()).collect();
            for (rank, tab) in tabs.into_iter().enumerate() {
                let position = rect.zip(origin).map(|(rect, origin)| {
                    let at = origin + rect.min.to_vec2() + egui::Vec2::splat(CASCADE * rank as f32);
                    [at.x * scale, at.y * scale]
                });
                let size = rect.map_or(TORN_SIZE, |rect| [rect.width(), rect.height()]);
                self.tear_off(tab, position, size);
            }
        }
    }

    /// The tab whose drag stops in this frame with the pointer outside the main window, where the
    /// window it opens goes, in pixels, `origin` being where the main window's content starts on
    /// the screens, in its points, and its size: four fifths of its place in the dock. Found
    /// before the dock is drawn, which leaves the tab where it is.
    pub(super) fn dragged_out(
        &self,
        ctx: &egui::Context,
        origin: Option<egui::Pos2>,
    ) -> Option<(Tab, Option<[f32; 2]>, [f32; 2])> {
        let scale = ctx.pixels_per_point();
        let stopped = ctx.drag_stopped_id()?;
        let pointer = ctx.input(|input| input.pointer.latest_pos())?;
        if ctx.viewport_rect().contains(pointer) {
            return None;
        }
        self.dock.iter_all_tabs().find_map(|(path, tab)| {
            (tab_id(path) == stopped).then(|| {
                let position = origin.map(|origin| {
                    let at = origin + pointer.to_vec2() + UNDER_POINTER;
                    [at.x * scale, at.y * scale]
                });
                let size = self.dock[path.node_path()]
                    .rect()
                    .filter(|rect| rect.is_finite() && rect.is_positive())
                    .map_or(TORN_SIZE, |rect| [rect.width() * 0.8, rect.height() * 0.8]);
                (tab.clone(), position, size)
            })
        })
    }
}

/// What the user did in the torn-off windows of a frame.
#[derive(Default)]
pub(super) struct TornActions {
    /// The windows whose panel goes back to the dock.
    pub docked: Vec<usize>,
    /// Undo or Redo pressed in one of them.
    pub keys: Vec<MenuAction>,
}

/// Draws each window of `torn`, its panel drawn by `viewer` as in the dock under a bar with its
/// title and the button *Dock back*, and keeps where it is; Undo and Redo act from it as from the
/// main window.
pub(super) fn torn_windows_ui(
    torn: &mut [TornWindow],
    ctx: &egui::Context,
    viewer: &mut Viewer,
    hotkeys: &Hotkeys,
) -> TornActions {
    let mut actions = TornActions::default();
    let theme = theme::current();
    // A window is placed in the points of the main window; it saves where it is in pixels.
    let scale = ctx.pixels_per_point();
    for (index, window) in torn.iter_mut().enumerate() {
        let mut tab = window.seen.tab.clone();
        let title = viewer.title(&mut tab).text().to_owned();
        let id = egui::ViewportId::from_hash_of(("uniwow torn-off", tab.key()));
        let mut builder = egui::ViewportBuilder::default()
            .with_title(format!("{title} — UniWoW"))
            .with_inner_size(window.opened.size)
            .with_min_inner_size(TORN_LEAST);
        if let Some([x, y]) = window.opened.position {
            builder = builder.with_position([x / scale, y / scale]);
        }
        ctx.show_viewport_immediate(id, builder, |ui, class| {
            let ctx = ui.ctx().clone();
            let native = class != egui::ViewportClass::EmbeddedWindow;
            if native {
                if !window.themed {
                    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
                    window.themed = true;
                }
                let (outer, inner, close) = ctx.input(|input| {
                    let viewport = input.viewport();
                    (viewport.outer_rect, viewport.inner_rect, viewport.close_requested())
                });
                if let Some(outer) = outer {
                    let pixels = ctx.pixels_per_point();
                    window.seen.position = Some([outer.min.x * pixels, outer.min.y * pixels]);
                }
                if let Some(inner) = inner {
                    window.seen.size = [inner.width(), inner.height()];
                }
                if close {
                    actions.docked.push(index);
                }
            }
            egui::Panel::top(id.0.with("bar"))
                .frame(theme.bar_frame())
                .show_separator_line(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&title).color(theme.palette.text_weak));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::icon_button(ui, icon::DOCK_BACK, "Dock back into the main window").clicked() {
                                actions.docked.push(index);
                            }
                        });
                    });
                });
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme.palette.panel).inner_margin(8))
                .show(ui, |ui| {
                    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                        let available = ui.available_rect_before_wrap();
                        ui.expand_to_include_rect(available);
                        viewer.ui(ui, &mut tab);
                    });
                });
            if native {
                if hotkeys.undo.pressed(&ctx) {
                    actions.keys.push(MenuAction::Undo);
                }
                if hotkeys.redo.pressed(&ctx) {
                    actions.keys.push(MenuAction::Redo);
                }
            }
        });
    }
    actions.docked.sort_unstable();
    actions.docked.dedup();
    actions
}

#[cfg(test)]
mod tests {
    use uniwow_api::egui_dock::{DockState, NodeIndex, SurfaceIndex, TabIndex, TabPath, WindowState};
    use uniwow_api::{egui, serde_json};

    use super::{floating_id, floating_rect};

    #[test]
    fn the_place_of_a_floating_window_is_read_from_what_the_dock_saves_then_from_egui() {
        let mut dock = DockState::new(vec!["a", "b"]);
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(400.0, 300.0));
        let surface = dock.detach_tab(TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(1)), rect);
        let state = dock.get_window_state(surface).expect("a floating window");
        let elsewhere = egui::Rect::from_min_size(egui::pos2(9.0, 9.0), egui::vec2(50.0, 50.0));
        let read = floating_rect(state, Some(elsewhere)).expect("its place");
        assert_eq!(read.min, rect.min, "not drawn yet: where the dock is to draw it");
        // The dock opens a tab of its main surface at four fifths of its size.
        assert_eq!(read.size(), rect.size() * 0.8);
        assert_eq!(
            floating_id(surface),
            egui::Id::new("window SurfaceIndex(1)"),
            "as the dock names it"
        );
        // Once drawn, the dock forgets where; egui keeps it.
        let mut saved = serde_json::to_value(state).expect("saved");
        saved["next_position"] = serde_json::Value::Null;
        let drawn: WindowState = serde_json::from_value(saved).expect("read back");
        assert_eq!(floating_rect(&drawn, Some(elsewhere)), Some(elsewhere));
        assert_eq!(floating_rect(&drawn, None), None);
        assert_eq!(
            floating_rect(&drawn, Some(egui::Rect::NOTHING)),
            None,
            "nothing drawn yet"
        );
    }
}
