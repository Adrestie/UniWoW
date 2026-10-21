//! The look of the editor, defined once: a palette, the sizes and the times of the animations, the
//! fonts and the icons. The styles of egui and of the dock are made from them, so that a colour
//! changed here changes everywhere. The kernel applies the theme when it starts; a module draws
//! with egui as before, reads `palette()` for the colours egui does not name, such as the state of
//! a connection, and shows its work under way with the widgets below, which animate for a few
//! tenths of a second and ask for frames only while they move.

use std::f32::consts::TAU;
use std::sync::Arc;

use crate::egui::epaint::text::VariationCoords;
use crate::egui::style::{Selection, TextCursorStyle, WidgetVisuals, Widgets};
use crate::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Margin, Pos2, Rect,
    Sense, Shadow, Shape, Stroke, TextStyle, Vec2, vec2,
};
use crate::egui_dock;

/// Inter 4.001 (SIL Open Font License 1.1, `fonts/Inter-OFL.txt`), its weights in one variable font.
const INTER: &[u8] = include_bytes!("../fonts/Inter-Variable.ttf");
/// Phosphor Icons 2.1, regular (MIT, `fonts/Phosphor-LICENSE.txt`): its icons are characters of the
/// private use area, which `icon` names.
const PHOSPHOR: &[u8] = include_bytes!("../fonts/Phosphor-Regular.ttf");

/// The family of the headings: Inter at weight 600.
pub const HEADING: &str = "heading";

/// The times of the animations, in seconds: short, never in the way of the user.
pub mod time {
    use std::time::Duration;

    /// The pointer coming over a widget or leaving it, a press.
    pub const HOVER: f32 = 0.10;
    /// Menus, popups and windows opening (egui's `animation_time`), a notice coming in, a choice
    /// moving.
    pub const OPEN: f32 = 0.16;
    /// A notice going, a bar of progress following its value.
    pub const CLOSE: f32 = 0.25;
    /// How often an indicator of work under way is drawn again while it shows: 30 times a second.
    pub const TICK: Duration = Duration::from_millis(33);
    /// One beat of a pulsing dot.
    pub const PULSE: f32 = 1.4;
}

/// The icons of the editor, characters of the font Phosphor, written in a text like any other:
/// `format!("{} Settings", icon::GEAR)`.
pub mod icon {
    pub const GEAR: &str = "\u{E272}";
    pub const UNDO: &str = "\u{E08A}";
    pub const REDO: &str = "\u{E08C}";
    pub const CLOSE: &str = "\u{E4F6}";
    pub const CHECK: &str = "\u{E182}";
    pub const SUCCESS: &str = "\u{E184}";
    pub const WARNING: &str = "\u{E4E0}";
    pub const ERROR: &str = "\u{E4F8}";
    pub const INFO: &str = "\u{E2CE}";
    pub const MODULE: &str = "\u{E596}";
    pub const JOBS: &str = "\u{EB44}";
    pub const TEAR_OFF: &str = "\u{E5DE}";
    pub const DOCK_BACK: &str = "\u{E5DC}";
    pub const KEYBOARD: &str = "\u{E2D8}";
    pub const SLIDERS: &str = "\u{E434}";
    pub const WINDOW: &str = "\u{E5DA}";
    pub const QUIT: &str = "\u{E42A}";
    pub const HISTORY: &str = "\u{E2B2}";
    pub const LOG: &str = "\u{EB7A}";
    pub const TERMINAL: &str = "\u{EAE8}";
    pub const CUBE: &str = "\u{E1DA}";
    pub const CONNECTED: &str = "\u{EB5A}";
    pub const DISCONNECTED: &str = "\u{EB56}";
    pub const STACK: &str = "\u{E466}";
    pub const QUESTION: &str = "\u{E3E8}";

    /// Every icon above, for the tests.
    pub const ALL: [&str; 25] = [
        GEAR,
        UNDO,
        REDO,
        CLOSE,
        CHECK,
        SUCCESS,
        WARNING,
        ERROR,
        INFO,
        MODULE,
        JOBS,
        TEAR_OFF,
        DOCK_BACK,
        KEYBOARD,
        SLIDERS,
        WINDOW,
        QUIT,
        HISTORY,
        LOG,
        TERMINAL,
        CUBE,
        CONNECTED,
        DISCONNECTED,
        STACK,
        QUESTION,
    ];
}

/// How much a notice, a state or a message matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    /// Its icon.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Info => icon::INFO,
            Self::Success => icon::SUCCESS,
            Self::Warning => icon::WARNING,
            Self::Error => icon::ERROR,
        }
    }
}

/// The colours of the editor. Every colour of the styles is one of these, or made from them by
/// `Palette::mix` and `Palette::alpha` in the functions below.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Behind everything, the lowest level: the window, the bars along it, the tab bars.
    pub base: Color32,
    /// The panels and the bodies of the tabs.
    pub panel: Color32,
    /// What rises over a panel at rest: buttons, boxes to tick, the handles of sliders.
    pub raised: Color32,
    /// The highest level: windows, menus, popups, notices.
    pub overlay: Color32,
    /// Below the panels: the fields where text is typed, the tracks of the bars.
    pub sunken: Color32,
    /// The lines between areas, the borders at rest.
    pub border: Color32,
    pub text: Color32,
    /// Headings, what is hovered or chosen.
    pub text_strong: Color32,
    /// What matters less: hints, units, what is disabled.
    pub text_weak: Color32,
    /// The one colour of what is chosen, focused or under way.
    pub accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    /// The colour of the shadows, made lighter by `alpha`.
    pub shadow: Color32,
}

impl Palette {
    /// The palette of the editor: cool greys, from the darkest behind to the lightest above, with
    /// a blue accent; the texts read at 11:1 (`text`) and 5:1 (`text_weak`) on the panels.
    pub const DARK: Self = Self {
        base: Color32::from_rgb(14, 16, 20),
        panel: Color32::from_rgb(22, 25, 31),
        raised: Color32::from_rgb(37, 41, 50),
        overlay: Color32::from_rgb(29, 33, 41),
        sunken: Color32::from_rgb(11, 13, 17),
        border: Color32::from_rgb(45, 50, 61),
        text: Color32::from_rgb(204, 209, 219),
        text_strong: Color32::from_rgb(243, 245, 249),
        text_weak: Color32::from_rgb(136, 144, 160),
        accent: Color32::from_rgb(82, 142, 245),
        success: Color32::from_rgb(64, 189, 138),
        warning: Color32::from_rgb(232, 165, 64),
        error: Color32::from_rgb(239, 98, 92),
        shadow: Color32::BLACK,
    };

    /// `from` moved towards `to` by `t`, from 0 to 1, channel by channel as the screen shows them.
    pub fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
        let t = t.clamp(0.0, 1.0);
        let [a, b] = [from.to_array(), to.to_array()];
        let channel = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8;
        Color32::from_rgba_premultiplied(channel(0), channel(1), channel(2), channel(3))
    }

    /// `colour` seen at `alpha`, from 0 to 1, over what lies behind it.
    pub fn alpha(colour: Color32, alpha: f32) -> Color32 {
        colour.gamma_multiply(alpha.clamp(0.0, 1.0))
    }

    /// The colour of a level.
    pub fn of_level(&self, level: Level) -> Color32 {
        match level {
            Level::Info => self.accent,
            Level::Success => self.success,
            Level::Warning => self.warning,
            Level::Error => self.error,
        }
    }

    /// A raised widget under the pointer.
    pub fn hovered(&self) -> Color32 {
        Self::mix(self.raised, self.text_strong, 0.08)
    }

    /// A raised widget pressed.
    pub fn pressed(&self) -> Color32 {
        Self::mix(self.raised, self.accent, 0.35)
    }

    /// The edge of a widget under the pointer.
    pub fn hovered_edge(&self) -> Color32 {
        Self::mix(self.border, self.text_strong, 0.18)
    }

    /// What is selected: rows, entries, text.
    pub fn selection(&self) -> Color32 {
        Self::mix(self.panel, self.accent, 0.42)
    }

    /// Text in the accent colour, light enough to be read: links, what is chosen.
    pub fn accent_text(&self) -> Color32 {
        Self::mix(self.accent, self.text_strong, 0.3)
    }

    /// One row out of two in the striped tables.
    pub fn stripe(&self) -> Color32 {
        Self::mix(self.panel, self.text_strong, 0.025)
    }

    /// The background of a choice under the pointer in a list, such as the tabs.
    pub fn hover_tint(&self) -> Color32 {
        Self::alpha(self.text_strong, 0.06)
    }
}

/// The sizes of the editor, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// The corners of the small widgets: boxes to tick, small buttons.
    pub radius_small: u8,
    /// The corners of the buttons, fields, frames and tabs.
    pub radius: u8,
    /// The corners of the windows, menus and notices.
    pub radius_large: u8,
    pub item_spacing: Vec2,
    pub button_padding: Vec2,
    pub window_margin: i8,
    pub menu_margin: i8,
    /// The height of a widget one interacts with, such as a button.
    pub interact_height: f32,
    pub indent: f32,
    pub tab_bar_height: f32,
    pub body_size: f32,
    pub small_size: f32,
    pub heading_size: f32,
    pub monospace_size: f32,
}

impl Metrics {
    pub const DEFAULT: Self = Self {
        radius_small: 4,
        radius: 6,
        radius_large: 10,
        item_spacing: vec2(8.0, 6.0),
        button_padding: vec2(10.0, 3.0),
        window_margin: 12,
        menu_margin: 6,
        interact_height: 22.0,
        indent: 18.0,
        tab_bar_height: 30.0,
        body_size: 13.0,
        small_size: 11.0,
        heading_size: 17.0,
        monospace_size: 12.5,
    };
}

/// The palette and the sizes: the whole look of the editor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub palette: Palette,
    pub metrics: Metrics,
}

/// The theme of the editor.
pub fn current() -> &'static Theme {
    &Theme::DARK
}

/// The palette of the editor.
pub fn palette() -> Palette {
    current().palette
}

impl Theme {
    pub const DARK: Self = Self {
        palette: Palette::DARK,
        metrics: Metrics::DEFAULT,
    };

    /// The colours of egui: the levels of the backgrounds, the states of the widgets, the
    /// selection, the windows and their shadows, all from the palette.
    pub fn visuals(&self) -> egui::Visuals {
        let (p, m) = (&self.palette, &self.metrics);
        let small = CornerRadius::same(m.radius_small);
        let medium = CornerRadius::same(m.radius);
        let mut visuals = egui::Visuals::dark();
        visuals.dark_mode = true;
        visuals.override_text_color = None;
        visuals.weak_text_color = Some(p.text_weak);
        visuals.widgets = Widgets {
            // Labels, separators and the lines of indentation.
            noninteractive: WidgetVisuals {
                bg_fill: p.panel,
                weak_bg_fill: p.panel,
                bg_stroke: Stroke::new(1.0, p.border),
                fg_stroke: Stroke::new(1.0, p.text),
                corner_radius: small,
                expansion: 0.0,
            },
            inactive: WidgetVisuals {
                bg_fill: p.raised,
                weak_bg_fill: p.raised,
                bg_stroke: Stroke::new(1.0, p.border),
                fg_stroke: Stroke::new(1.0, p.text),
                corner_radius: medium,
                expansion: 0.0,
            },
            hovered: WidgetVisuals {
                bg_fill: p.hovered(),
                weak_bg_fill: p.hovered(),
                bg_stroke: Stroke::new(1.0, p.hovered_edge()),
                fg_stroke: Stroke::new(1.5, p.text_strong),
                corner_radius: medium,
                expansion: 1.0,
            },
            active: WidgetVisuals {
                bg_fill: p.pressed(),
                weak_bg_fill: p.pressed(),
                bg_stroke: Stroke::new(1.0, p.accent),
                fg_stroke: Stroke::new(2.0, p.text_strong),
                corner_radius: medium,
                expansion: 1.0,
            },
            // A menu or a list of choices open.
            open: WidgetVisuals {
                bg_fill: p.hovered(),
                weak_bg_fill: p.hovered(),
                bg_stroke: Stroke::new(1.0, p.accent),
                fg_stroke: Stroke::new(1.0, p.text_strong),
                corner_radius: medium,
                expansion: 0.0,
            },
        };
        visuals.selection = Selection {
            bg_fill: p.selection(),
            stroke: Stroke::new(1.0, p.accent_text()),
        };
        visuals.ime_composition.active_underline_stroke = Stroke::new(2.0, p.accent);
        visuals.ime_composition.inactive_underline_stroke = Stroke::new(2.0, Palette::mix(p.accent, p.panel, 0.5));
        visuals.hyperlink_color = p.accent_text();
        visuals.faint_bg_color = p.stripe();
        visuals.extreme_bg_color = p.sunken;
        visuals.text_edit_bg_color = Some(p.sunken);
        visuals.code_bg_color = p.raised;
        visuals.warn_fg_color = p.warning;
        visuals.error_fg_color = p.error;
        visuals.window_corner_radius = CornerRadius::same(m.radius_large);
        visuals.window_shadow = Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: Palette::alpha(p.shadow, 0.55),
        };
        visuals.window_fill = p.overlay;
        visuals.window_stroke = Stroke::new(1.0, p.border);
        visuals.window_highlight_topmost = true;
        visuals.menu_corner_radius = CornerRadius::same(m.radius_large);
        visuals.panel_fill = p.panel;
        visuals.popup_shadow = Shadow {
            offset: [0, 6],
            blur: 16,
            spread: 0,
            color: Palette::alpha(p.shadow, 0.45),
        };
        visuals.text_cursor = TextCursorStyle {
            stroke: Stroke::new(2.0, p.accent),
            ..TextCursorStyle::default()
        };
        visuals.slider_trailing_fill = true;
        visuals.handle_shape = egui::style::HandleShape::Circle;
        visuals.disabled_alpha = 0.45;
        visuals
    }

    /// The style of egui: the visuals, the sizes of the texts, the spacing, and the time of its
    /// own animations (menus, popups and windows fading in, sections opening).
    pub fn style(&self) -> egui::Style {
        let m = &self.metrics;
        let mut style = egui::Style {
            visuals: self.visuals(),
            animation_time: time::OPEN,
            ..egui::Style::default()
        };
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(m.small_size)),
            (TextStyle::Body, FontId::proportional(m.body_size)),
            (TextStyle::Button, FontId::proportional(m.body_size)),
            (TextStyle::Monospace, FontId::monospace(m.monospace_size)),
            (
                TextStyle::Heading,
                FontId::new(m.heading_size, FontFamily::Name(HEADING.into())),
            ),
        ]
        .into();
        let spacing = &mut style.spacing;
        spacing.item_spacing = m.item_spacing;
        spacing.button_padding = m.button_padding;
        spacing.window_margin = Margin::same(m.window_margin);
        spacing.menu_margin = Margin::same(m.menu_margin);
        spacing.indent = m.indent;
        spacing.interact_size.y = m.interact_height;
        spacing.icon_width = 15.0;
        spacing.icon_width_inner = 9.0;
        spacing.icon_spacing = 6.0;
        spacing.slider_rail_height = 4.0;
        style
    }

    /// The style of the dock: the tab bars on the lowest level, the tab shown on the level of its
    /// panel, the others in the weak text until the pointer comes, the lines and the overlay of a
    /// tab dragged in the accent.
    pub fn dock_style(&self) -> egui_dock::Style {
        let (p, m) = (&self.palette, &self.metrics);
        let style = self.style();
        let mut dock = egui_dock::Style::from_egui(&style);
        let top = CornerRadius {
            nw: m.radius,
            ne: m.radius,
            sw: 0,
            se: 0,
        };
        let tab = |bg_fill: Color32, text_color: Color32, outline_color: Color32| egui_dock::TabInteractionStyle {
            outline_color,
            corner_radius: top,
            bg_fill,
            text_color,
        };
        dock.main_surface_border_stroke = Stroke::NONE;
        dock.tab_bar.bg_fill = p.base;
        dock.tab_bar.height = m.tab_bar_height;
        dock.tab_bar.hline_color = p.border;
        dock.tab_bar.corner_radius = CornerRadius::ZERO;
        // The hover is drawn by the kernel, which animates it: its style is that of a tab at rest.
        dock.tab.active = tab(p.panel, p.text_strong, p.border);
        dock.tab.inactive = tab(p.base, p.text_weak, p.base);
        dock.tab.focused = tab(p.panel, p.text_strong, p.border);
        dock.tab.hovered = tab(p.base, p.text, p.base);
        dock.tab.active_with_kb_focus = tab(p.panel, p.text_strong, p.accent);
        dock.tab.inactive_with_kb_focus = tab(p.base, p.text, p.accent);
        dock.tab.focused_with_kb_focus = tab(p.panel, p.text_strong, p.accent);
        dock.tab.tab_body.bg_fill = p.panel;
        dock.tab.tab_body.stroke = Stroke::NONE;
        dock.tab.tab_body.corner_radius = CornerRadius::ZERO;
        dock.tab.tab_body.inner_margin = Margin::same(8);
        dock.tab.hline_below_active_tab_name = false;
        dock.tab.spacing = 2.0;
        dock.separator.width = 1.0;
        dock.separator.extra_interact_width = 4.0;
        dock.separator.color_idle = p.border;
        dock.separator.color_hovered = p.accent_text();
        dock.separator.color_dragged = p.accent;
        let buttons = &mut dock.buttons;
        buttons.close_tab_color = p.text_weak;
        buttons.close_tab_active_color = p.text_strong;
        buttons.close_tab_bg_fill = p.hovered();
        buttons.add_tab_color = p.text_weak;
        buttons.add_tab_active_color = p.text_strong;
        buttons.add_tab_bg_fill = p.hovered();
        buttons.add_tab_border_color = p.border;
        buttons.close_all_tabs_color = p.text_weak;
        buttons.close_all_tabs_active_color = p.text_strong;
        buttons.close_all_tabs_bg_fill = p.hovered();
        buttons.close_all_tabs_border_color = p.border;
        buttons.close_all_tabs_disabled_color = p.raised;
        buttons.collapse_tabs_color = p.text_weak;
        buttons.collapse_tabs_active_color = p.text_strong;
        buttons.collapse_tabs_bg_fill = p.hovered();
        buttons.collapse_tabs_border_color = p.border;
        buttons.minimize_window_color = p.text_weak;
        buttons.minimize_window_active_color = p.text_strong;
        buttons.minimize_window_bg_fill = p.hovered();
        buttons.minimize_window_border_color = p.border;
        buttons.show_tab_bar_color = p.text_weak;
        buttons.show_tab_bar_active_color = p.text_strong;
        let overlay = &mut dock.overlay;
        overlay.selection_color = Palette::alpha(p.accent, 0.35);
        overlay.button_color = p.text;
        overlay.button_border_stroke = Stroke::new(1.0, p.border);
        overlay.hovered_leaf_highlight.color = Palette::alpha(p.accent, 0.12);
        overlay.hovered_leaf_highlight.stroke = Stroke::new(1.0, p.accent);
        overlay.hovered_leaf_highlight.corner_radius = CornerRadius::same(m.radius);
        dock
    }

    /// The fonts: Inter for the texts, at weight 600 for the headings, Hack for the monospace
    /// texts, each followed by the icons of Phosphor, then by egui's own fonts for the characters
    /// they lack.
    pub fn fonts() -> FontDefinitions {
        let weight = |weight: f32| FontTweak {
            coords: VariationCoords::new([(b"wght", weight)]),
            ..FontTweak::default()
        };
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "Inter".to_owned(),
            Arc::new(FontData::from_static(INTER).tweak(weight(400.0))),
        );
        fonts.font_data.insert(
            "Inter SemiBold".to_owned(),
            Arc::new(FontData::from_static(INTER).tweak(weight(600.0))),
        );
        fonts
            .font_data
            .insert("Phosphor".to_owned(), Arc::new(FontData::from_static(PHOSPHOR)));
        let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
        let fallbacks = proportional.clone();
        proportional.splice(0..0, ["Inter".to_owned(), "Phosphor".to_owned()]);
        let mut heading = vec!["Inter SemiBold".to_owned(), "Phosphor".to_owned()];
        heading.extend(fallbacks);
        fonts.families.insert(FontFamily::Name(HEADING.into()), heading);
        let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
        monospace.insert(monospace.len().min(1), "Phosphor".to_owned());
        fonts
    }

    /// Applies the theme to the editor's window and to every window it opens, whatever theme the
    /// system uses.
    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_fonts(Self::fonts());
        let style = Arc::new(self.style());
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);
        ctx.set_theme(egui::ThemePreference::Dark);
    }

    /// The frame of the bars along the window, the menu bar and the status bar.
    pub fn bar_frame(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.palette.base)
            .inner_margin(Margin::symmetric(8, 3))
    }

    /// The frame of what floats over the editor and is drawn by it, such as a notice: the highest
    /// level, its border, its large corners and its shadow.
    pub fn overlay_frame(&self) -> egui::Frame {
        let visuals = self.visuals();
        egui::Frame::new()
            .fill(self.palette.overlay)
            .stroke(Stroke::new(1.0, self.palette.border))
            .corner_radius(CornerRadius::same(self.metrics.radius_large))
            .shadow(visuals.popup_shadow)
            .inner_margin(Margin::symmetric(12, 10))
    }
}

/// How far the pointer's coming over `response`, or its keyboard focus, has gone, from 0 to 1, in
/// `time::HOVER`: what a widget drawn here lights up by.
pub fn hovered(ui: &egui::Ui, response: &egui::Response) -> f32 {
    ui.ctx().animate_bool_with_time_and_easing(
        response.id.with("uniwow hover"),
        response.hovered() || response.has_focus(),
        time::HOVER,
        egui::emath::easing::cubic_out,
    )
}

/// Asks for the next frame of an indicator that moves while it shows, at `time::TICK`.
fn tick(ui: &egui::Ui) {
    ui.ctx().request_repaint_after(time::TICK);
}

/// A spinner `size` points wide in the accent colour, an arc turning: work under way whose end is
/// not known. Frames come at `time::TICK` while it is drawn, and stop with it.
pub fn spinner(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = palette();
        let seconds = ui.input(|input| input.time) as f32;
        let (centre, radius) = (rect.center(), size * 0.5 - 1.5);
        let painter = ui.painter();
        painter.circle_stroke(centre, radius, Stroke::new(2.0, Palette::alpha(p.accent, 0.2)));
        let start = seconds * TAU * 0.8;
        let sweep = TAU * (0.3 + 0.12 * (seconds * 2.4).sin());
        let points: Vec<Pos2> = (0..=24)
            .map(|i| {
                let angle = start + sweep * i as f32 / 24.0;
                centre + radius * vec2(angle.cos(), angle.sin())
            })
            .collect();
        painter.add(Shape::line(points, Stroke::new(2.0, p.accent)));
        tick(ui);
    }
    response
}

/// A dot of `colour`, the state of something such as a connection; `pulsing`, a halo beats around
/// it once every `time::PULSE` while it is drawn, for a state that waits on something.
pub fn status_dot(ui: &mut egui::Ui, colour: Color32, pulsing: bool) -> egui::Response {
    let height = ui.text_style_height(&TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(vec2(height * 0.75, height), Sense::hover());
    if ui.is_rect_visible(rect) {
        let (centre, radius) = (rect.center(), 3.5);
        let painter = ui.painter();
        if pulsing {
            let beat = ((ui.input(|input| input.time) as f32 / time::PULSE).fract()).clamp(0.0, 1.0);
            let halo = radius * (1.0 + 1.4 * beat);
            painter.circle_filled(centre, halo, Palette::alpha(colour, 0.5 * (1.0 - beat)));
            tick(ui);
        }
        painter.circle_filled(centre, radius, colour);
    }
    response
}

/// A bar of progress `width` points wide: filled to `fraction`, which it follows in `time::CLOSE`;
/// without one, a part of it slides from one end to the other, for work whose share done is not
/// known yet.
pub fn progress_bar(ui: &mut egui::Ui, fraction: Option<f32>, width: f32) -> egui::Response {
    let height = ui
        .spacing()
        .interact_size
        .y
        .min(ui.text_style_height(&TextStyle::Body) + 4.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = palette();
        let track = Rect::from_center_size(rect.center(), vec2(width, 6.0));
        let radius = CornerRadius::same(3);
        let painter = ui.painter();
        painter.rect_filled(track, radius, p.sunken);
        match fraction {
            Some(fraction) => {
                let shown = ui.ctx().animate_value_with_time(
                    response.id.with("uniwow progress"),
                    fraction.clamp(0.0, 1.0),
                    time::CLOSE,
                );
                let filled = Rect::from_min_size(track.min, vec2(track.width() * shown, track.height()));
                if shown > 0.0 {
                    painter.rect_filled(filled, radius, p.accent);
                }
            }
            None => {
                let seconds = ui.input(|input| input.time) as f32;
                let part = track.width() * 0.3;
                let left = track.left() - part + (seconds * 0.7).fract() * (track.width() + part);
                let moving = Rect::from_min_max(
                    Pos2::new(left.max(track.left()), track.top()),
                    Pos2::new((left + part).min(track.right()), track.bottom()),
                );
                if moving.width() > 0.0 {
                    painter.rect_filled(moving, radius, p.accent);
                }
                tick(ui);
            }
        }
    }
    response
}

/// A card of a panel without a header, as `section` draws its body: what leads a panel, such as
/// the name of what an inspector shows.
pub fn card<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let (p, m) = (palette(), current().metrics);
    let shown = egui::Frame::new()
        .fill(Palette::mix(p.panel, p.raised, 0.6))
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(m.radius))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui)
        });
    ui.add_space(6.0);
    shown.inner
}

/// A section of a panel: a card a little raised over it, rounded and edged, its header the icon
/// in the accent, the title strong and a note weak, folded and unfolded by a click on it, its body
/// under a line; the cards of a panel a few points apart. Whether it is open kept by `id`. What the
/// body gave, while open.
pub fn section<R>(
    ui: &mut egui::Ui,
    id: egui::Id,
    icon: &str,
    title: &str,
    note: Option<&str>,
    open: bool,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let (p, m) = (palette(), current().metrics);
    let shown = egui::Frame::new()
        .fill(Palette::mix(p.panel, p.raised, 0.35))
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(m.radius))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open);
            let header = ui
                .horizontal(|ui| {
                    let arrow = if state.is_open() { "\u{25BE}" } else { "\u{25B8}" };
                    ui.label(egui::RichText::new(arrow).color(p.text_weak));
                    ui.label(egui::RichText::new(icon).color(p.accent));
                    ui.label(egui::RichText::new(title).strong().color(p.text_strong));
                    if let Some(note) = note {
                        ui.label(egui::RichText::new(note).small().color(p.text_weak));
                    }
                    ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
                })
                .response
                .interact(Sense::click());
            if header.clicked() {
                state.toggle(ui);
            }
            let out = state.is_open().then(|| {
                let line = ui.available_rect_before_wrap();
                ui.painter()
                    .hline(line.x_range(), line.top() + 2.0, Stroke::new(1.0, p.border));
                ui.add_space(6.0);
                body(ui)
            });
            state.store(ui.ctx());
            out
        });
    ui.add_space(6.0);
    shown.inner
}

/// A square button showing `icon`, lit by the pointer in `time::HOVER`, tinted by the accent while
/// pressed, `tooltip` told on hover.
pub fn icon_button(ui: &mut egui::Ui, icon: &str, tooltip: &str) -> egui::Response {
    let side = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    if ui.is_rect_visible(rect) {
        let (p, m) = (palette(), current().metrics);
        let lit = hovered(ui, &response);
        let fill = if response.is_pointer_button_down_on() {
            p.pressed()
        } else {
            Palette::alpha(p.hovered(), lit)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(m.radius_small), fill);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(side * 0.62),
            Palette::mix(p.text_weak, p.text_strong, lit),
        );
    }
    response.on_hover_text(tooltip)
}

/// A switch for a choice that is on or off, its knob sliding in `time::OPEN`.
pub struct Toggle<'a>(pub &'a mut bool);

impl egui::Widget for Toggle<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let height = (ui.spacing().interact_size.y * 0.72).round();
        let (rect, mut response) = ui.allocate_exact_size(vec2(height * 1.8, height), Sense::click());
        if response.clicked() {
            *self.0 = !*self.0;
            response.mark_changed();
        }
        let on = *self.0;
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, ""));
        if ui.is_rect_visible(rect) {
            let p = palette();
            let moved = ui.ctx().animate_bool_with_time_and_easing(
                response.id.with("uniwow toggle"),
                on,
                time::OPEN,
                egui::emath::easing::cubic_out,
            );
            let lit = hovered(ui, &response);
            let track = Palette::mix(Palette::mix(p.raised, p.accent, moved), p.text_strong, 0.08 * lit);
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same((height / 2.0) as u8), track);
            let radius = height / 2.0 - 2.5;
            let x = egui::lerp((rect.left() + height / 2.0)..=(rect.right() - height / 2.0), moved);
            painter.circle_filled(
                Pos2::new(x, rect.center().y),
                radius,
                Palette::mix(p.text, p.text_strong, moved.max(lit)),
            );
        }
        response
    }
}

/// An entry of a list of choices, such as the categories of a window: the pointer lights it, and
/// the chosen one shows a bar of the accent on its left, each in `time::HOVER` and `time::OPEN`.
pub fn choice(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    let (p, m) = (palette(), current().metrics);
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        TextStyle::Body.resolve(ui.style()),
        Color32::PLACEHOLDER,
    );
    let width = ui.available_width().max(galley.size().x + 24.0);
    let height = m.interact_height + 4.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    if ui.is_rect_visible(rect) {
        let lit = hovered(ui, &response);
        let chosen = ui.ctx().animate_bool_with_time_and_easing(
            response.id.with("uniwow choice"),
            selected,
            time::OPEN,
            egui::emath::easing::cubic_out,
        );
        let painter = ui.painter();
        let radius = CornerRadius::same(m.radius);
        painter.rect_filled(rect, radius, Palette::alpha(p.hovered(), lit * (1.0 - chosen)));
        if chosen > 0.0 {
            painter.rect_filled(
                rect,
                radius,
                Palette::alpha(Palette::mix(p.panel, p.accent, 0.22), chosen),
            );
            let bar = Rect::from_center_size(
                Pos2::new(rect.left() + 2.0, rect.center().y),
                vec2(3.0, (height - 10.0) * chosen),
            );
            painter.rect_filled(bar, CornerRadius::same(2), p.accent);
        }
        let colour = Palette::mix(Palette::mix(p.text, p.text_strong, lit), p.text_strong, chosen);
        let at = Pos2::new(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0);
        painter.galley(at, galley, colour);
    }
    response
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Every colour of `value`, a style written as JSON, by its path: egui writes a colour as an
    /// array of four bytes, which nothing else of the styles is.
    fn colours(value: &serde_json::Value, path: &str, found: &mut BTreeMap<String, [u8; 4]>) {
        match value {
            serde_json::Value::Array(items) if items.len() == 4 => {
                let bytes: Vec<u8> = items
                    .iter()
                    .filter_map(|item| item.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                    .collect();
                match <[u8; 4]>::try_from(bytes) {
                    Ok(bytes) => {
                        found.insert(path.to_owned(), bytes);
                    }
                    Err(_) => {
                        for (index, item) in items.iter().enumerate() {
                            colours(item, &format!("{path}[{index}]"), found);
                        }
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    colours(item, &format!("{path}[{index}]"), found);
                }
            }
            serde_json::Value::Object(fields) => {
                for (key, item) in fields {
                    colours(item, &format!("{path}.{key}"), found);
                }
            }
            _ => {}
        }
    }

    fn of(value: impl serde::Serialize) -> BTreeMap<String, [u8; 4]> {
        let mut found = BTreeMap::new();
        colours(
            &serde_json::to_value(value).expect("a style is written as JSON"),
            "",
            &mut found,
        );
        found
    }

    /// Every colour of the palette changed.
    fn other() -> Palette {
        let turn = |colour: Color32| {
            let [r, g, b, a] = colour.to_array();
            Color32::from_rgba_premultiplied(255 - g / 2, r / 3 + 40, 200 - b / 2, a)
        };
        let p = Palette::DARK;
        Palette {
            base: turn(p.base),
            panel: turn(p.panel),
            raised: turn(p.raised),
            overlay: turn(p.overlay),
            sunken: turn(p.sunken),
            border: turn(p.border),
            text: turn(p.text),
            text_strong: turn(p.text_strong),
            text_weak: turn(p.text_weak),
            accent: turn(p.accent),
            success: turn(p.success),
            warning: turn(p.warning),
            error: turn(p.error),
            shadow: Color32::from_rgb(40, 0, 60),
        }
    }

    /// The colours of `a` and `b`, two styles made from two palettes, none the same in both but
    /// those that show nothing.
    fn none_fixed(what: &str, a: &BTreeMap<String, [u8; 4]>, b: &BTreeMap<String, [u8; 4]>) {
        assert_eq!(
            a.keys().collect::<Vec<_>>(),
            b.keys().collect::<Vec<_>>(),
            "{what}: the same colours"
        );
        assert!(a.len() > 20, "{what}: its colours found ({})", a.len());
        let fixed: Vec<&String> = a
            .iter()
            .filter(|(path, colour)| colour[3] != 0 && b[*path] == **colour)
            .map(|(path, _)| path)
            .collect();
        assert!(fixed.is_empty(), "{what}: colours not made from the palette: {fixed:?}");
    }

    #[test]
    fn every_colour_of_the_styles_of_egui_and_of_the_dock_is_made_from_the_palette() {
        let theirs = Theme {
            palette: other(),
            ..Theme::DARK
        };
        none_fixed("egui", &of(Theme::DARK.style()), &of(theirs.style()));
        none_fixed("dock", &of(Theme::DARK.dock_style()), &of(theirs.dock_style()));
    }

    #[test]
    fn the_levels_and_the_widgets_take_the_colours_of_the_palette_they_name() {
        let (theme, p) = (Theme::DARK, Palette::DARK);
        let visuals = theme.visuals();
        assert_eq!(visuals.panel_fill, p.panel);
        assert_eq!(visuals.window_fill, p.overlay);
        assert_eq!(visuals.extreme_bg_color, p.sunken);
        assert_eq!(visuals.widgets.inactive.weak_bg_fill, p.raised, "a button at rest");
        assert_eq!(visuals.widgets.noninteractive.fg_stroke.color, p.text);
        assert_eq!(visuals.weak_text_color(), p.text_weak);
        assert_eq!(visuals.strong_text_color(), p.text_strong, "the text hovered");
        assert_eq!(visuals.selection.bg_fill, p.selection());
        assert_eq!(visuals.text_cursor.stroke.color, p.accent);
        assert_eq!((visuals.warn_fg_color, visuals.error_fg_color), (p.warning, p.error));
        let dock = theme.dock_style();
        assert_eq!(dock.tab_bar.bg_fill, p.base);
        assert_eq!(
            dock.tab.active.bg_fill, p.panel,
            "the tab shown, on the level of its panel"
        );
        assert_eq!(dock.tab.tab_body.bg_fill, p.panel);
        assert_eq!(
            dock.tab.hovered.bg_fill, dock.tab.inactive.bg_fill,
            "the hover drawn by the kernel, animated"
        );
        for level in [Level::Info, Level::Success, Level::Warning, Level::Error] {
            assert_ne!(p.of_level(level), p.panel);
        }
    }

    #[test]
    fn the_sizes_and_the_times_are_those_of_the_metrics() {
        let (theme, m) = (Theme::DARK, Metrics::DEFAULT);
        let style = theme.style();
        assert_eq!(style.animation_time, time::OPEN);
        const { assert!(time::HOVER <= time::OPEN && time::OPEN <= time::CLOSE && time::CLOSE <= 0.25) };
        assert_eq!(style.spacing.interact_size.y, m.interact_height);
        assert_eq!(style.spacing.item_spacing, m.item_spacing);
        assert_eq!(
            style.visuals.widgets.inactive.corner_radius,
            CornerRadius::same(m.radius)
        );
        assert_eq!(style.visuals.window_corner_radius, CornerRadius::same(m.radius_large));
        assert_eq!(style.text_styles[&TextStyle::Body].size, m.body_size);
        assert_eq!(
            style.text_styles[&TextStyle::Heading].family,
            FontFamily::Name(HEADING.into())
        );
        assert_eq!(theme.dock_style().tab_bar.height, m.tab_bar_height);
    }

    #[test]
    fn a_mix_goes_from_one_colour_to_the_other_and_an_alpha_lightens() {
        let (black, white) = (Color32::BLACK, Color32::WHITE);
        assert_eq!(Palette::mix(black, white, 0.0), black);
        assert_eq!(Palette::mix(black, white, 1.0), white);
        assert_eq!(Palette::mix(black, white, 0.5), Color32::from_gray(128));
        assert_eq!(Palette::mix(black, white, 7.0), white, "kept within the two");
        assert_eq!(Palette::alpha(white, 0.0), Color32::TRANSPARENT);
        assert_eq!(Palette::alpha(white, 1.0), white);
    }

    #[test]
    fn the_fonts_hold_inter_for_the_texts_and_the_headings_and_every_icon() {
        let fonts = Theme::fonts();
        assert_eq!(fonts.families[&FontFamily::Proportional][..2], ["Inter", "Phosphor"]);
        assert_eq!(
            fonts.families[&FontFamily::Name(HEADING.into())][..2],
            ["Inter SemiBold", "Phosphor"]
        );
        assert!(fonts.families[&FontFamily::Monospace].contains(&"Phosphor".to_owned()));
        assert!(
            fonts.families[&FontFamily::Proportional].len() > 2,
            "egui's own fonts after them, for what they lack"
        );
        let ctx = egui::Context::default();
        Theme::DARK.apply(&ctx);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        for icon in icon::ALL {
            let character = icon.chars().next().expect("an icon is a character");
            assert!(
                ctx.fonts_mut(|fonts| fonts.has_glyph(&FontId::proportional(13.0), character)),
                "the icon {:X} is in Phosphor",
                character as u32
            );
        }
        assert_eq!(ctx.theme(), egui::Theme::Dark, "whatever the system uses");
        assert_eq!(ctx.global_style().visuals.panel_fill, Palette::DARK.panel);
    }
}
