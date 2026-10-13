//! The notices over the window: the outcome of what the user asked, the errors logged and the
//! modules that failed. Each comes in from the right in `time::OPEN`, stays a few seconds by its
//! level and goes in `time::CLOSE`, at the bottom right of the window above the status bar, the
//! newest lowest. The pointer over one keeps it; its cross sends it away. Frames are asked for only
//! while one comes or goes, and once when the next one is to go.

use uniwow_api::theme::{self, Level, Palette, time};

use super::*;

/// The most notices shown at once: past it, the oldest go, the errors last.
const MOST: usize = 5;
/// A notice the pointer left stays at least this long, in seconds.
const GRACE: f64 = 1.0;
/// The longest text a notice shows, in characters; the log keeps all of it.
const LONGEST: usize = 300;
/// The width of a notice, in points.
const WIDTH: f32 = 340.0;

/// How long a notice of `level` stays before it goes, in seconds.
pub(super) fn lasting(level: Level) -> f64 {
    match level {
        Level::Info | Level::Success => 4.0,
        Level::Warning => 6.0,
        Level::Error => 9.0,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Toast {
    pub id: u64,
    /// What it is about: a notice of the same key, coming while it shows, takes its place.
    pub key: String,
    pub level: Level,
    pub title: String,
    pub text: String,
    /// How many times the same notice came while it showed.
    pub count: u32,
    /// When it came, and when it starts going, in seconds of egui's clock.
    pub came: f64,
    pub goes: f64,
}

impl Toast {
    /// How much of it shows at `now`, from 0 to 1: rising as it comes, falling as it goes.
    pub fn shown(&self, now: f64) -> f32 {
        let coming = ((now - self.came) / f64::from(time::OPEN)).clamp(0.0, 1.0);
        let going = 1.0 - ((now - self.goes) / f64::from(time::CLOSE)).clamp(0.0, 1.0);
        coming.min(going) as f32
    }

    fn going(&self, now: f64) -> bool {
        now >= self.goes
    }

    fn gone(&self, now: f64) -> bool {
        now >= self.goes + f64::from(time::CLOSE)
    }
}

/// The notices, the oldest first.
#[derive(Default)]
pub(super) struct Toasts {
    list: Vec<Toast>,
    next: u64,
}

/// `text` cut at `LONGEST` characters.
fn shortened(text: &str) -> String {
    match text.char_indices().nth(LONGEST) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

impl Toasts {
    pub fn list(&self) -> &[Toast] {
        &self.list
    }

    /// Shows a notice from `now`. One of the same key still shown takes it instead, the times
    /// counted when it says the same, and stays its time again from `now`.
    pub fn push(&mut self, now: f64, key: &str, level: Level, title: &str, text: &str) {
        let text = shortened(text);
        if let Some(toast) = self.list.iter_mut().find(|toast| toast.key == key && !toast.going(now)) {
            toast.count = if toast.title == title && toast.text == text {
                toast.count + 1
            } else {
                1
            };
            toast.level = level;
            toast.title = title.to_owned();
            toast.text = text;
            toast.goes = now + lasting(level);
            return;
        }
        self.next += 1;
        self.list.push(Toast {
            id: self.next,
            key: key.to_owned(),
            level,
            title: title.to_owned(),
            text,
            count: 1,
            came: now,
            goes: now + lasting(level),
        });
        let mut shown: Vec<usize> = (0..self.list.len()).filter(|&i| !self.list[i].going(now)).collect();
        // The oldest go first, those that are not errors before the errors.
        shown.sort_by_key(|&i| (self.list[i].level == Level::Error, self.list[i].came.to_bits()));
        for &index in shown.iter().take(shown.len().saturating_sub(MOST)) {
            self.list[index].goes = now;
        }
    }

    /// Sends the notice `id` away from `now`.
    pub fn dismiss(&mut self, id: u64, now: f64) {
        if let Some(toast) = self.list.iter_mut().find(|toast| toast.id == id) {
            toast.goes = toast.goes.min(now);
        }
    }

    /// Keeps the notice `id`, under the pointer at `now`, a little longer than the pointer.
    pub fn hold(&mut self, id: u64, now: f64) {
        if let Some(toast) = self.list.iter_mut().find(|toast| toast.id == id && !toast.going(now)) {
            toast.goes = toast.goes.max(now + GRACE);
        }
    }

    /// Forgets the notices gone at `now`.
    pub fn tidy(&mut self, now: f64) {
        self.list.retain(|toast| !toast.gone(now));
    }

    /// In how many seconds from `now` a frame is needed: none at once while one comes or goes,
    /// then when the first one starts going; never without notices.
    pub fn next_frame(&self, now: f64) -> Option<f64> {
        self.list
            .iter()
            .map(|toast| {
                if now < toast.came + f64::from(time::OPEN) || toast.going(now) {
                    0.0
                } else {
                    toast.goes - now
                }
            })
            .min_by(f64::total_cmp)
    }
}

impl Shell {
    /// Shows a notice of the kernel: `key` says what it is about, so that one of the same
    /// replaces it.
    pub(super) fn notify(&mut self, key: &str, level: Level, title: &str, text: &str) {
        self.toasts.push(self.clock, key, level, title, text);
    }

    /// The notices of the errors logged since the last frame, one per source of them, and those
    /// the modules asked for.
    pub(super) fn collect_notices(&mut self) {
        self.notice_errors(logger::take_errors());
        for (owner, level, text) in std::mem::take(&mut self.host.notices) {
            let title = self
                .slots
                .iter()
                .find(|slot| slot.id == owner)
                .map_or_else(|| owner.clone(), |slot| slot.name().to_owned());
            self.notify(&format!("module/{owner}/{text}"), level, &title, &text);
        }
    }

    /// The notices of errors logged, one per source of them: the module, the editor or the library
    /// that logged it.
    pub(super) fn notice_errors(&mut self, lines: Vec<logger::LogLine>) {
        for line in lines {
            let title = match self.slots.iter().find(|slot| slot.id == line.source) {
                Some(slot) => format!("Error in {}", slot.name()),
                None if line.source == KERNEL => "Error in the editor".to_owned(),
                None => format!("Error in {}", line.source),
            };
            self.notify(&format!("source/{}", line.source), Level::Error, &title, &line.message);
        }
    }

    /// Draws the notices over the window, above the status bar `bottom` points high, and asks for
    /// the frames their animations need.
    pub(super) fn toasts_ui(&mut self, ctx: &egui::Context, bottom: f32) {
        let now = ctx.input(|input| input.time);
        self.toasts.tidy(now);
        let theme = theme::current();
        let p = theme.palette;
        let (mut dismissed, mut held) = (Vec::new(), Vec::new());
        let mut offset = bottom + 12.0;
        for toast in self.toasts.list().iter().rev() {
            let shown = toast.shown(now);
            let id = egui::Id::new(("uniwow toast", toast.id));
            // Each slides to its place when one below it goes.
            let lift = ctx.animate_value_with_time(id.with("lift"), offset, time::OPEN);
            let slide = (1.0 - shown) * 32.0;
            let area = egui::Area::new(id)
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0 + slide, -lift))
                .fade_in(false)
                .interactable(true)
                .show(ctx, |ui| {
                    ui.multiply_opacity(shown);
                    ui.set_width(WIDTH);
                    let colour = p.of_level(toast.level);
                    let frame = theme.overlay_frame().show(ui, |ui| {
                        ui.set_width(WIDTH - 24.0);
                        ui.horizontal_top(|ui| {
                            ui.label(egui::RichText::new(toast.level.icon()).size(18.0).color(colour));
                            ui.vertical(|ui| {
                                ui.set_width(WIDTH - 24.0 - 26.0 - 30.0);
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(&toast.title).strong());
                                    if toast.count > 1 {
                                        ui.label(egui::RichText::new(format!("×{}", toast.count)).small().weak());
                                    }
                                });
                                if !toast.text.is_empty() {
                                    ui.add(egui::Label::new(egui::RichText::new(&toast.text).color(p.text)).wrap());
                                }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                                if theme::icon_button(ui, theme::icon::CLOSE, "Dismiss").clicked() {
                                    dismissed.push(toast.id);
                                }
                            });
                        });
                    });
                    // The level, a bar along the left edge.
                    let rect = frame.response.rect;
                    let bar = egui::Rect::from_min_max(
                        rect.left_top() + egui::vec2(1.0, 10.0),
                        egui::pos2(rect.left() + 4.0, rect.bottom() - 10.0),
                    );
                    ui.painter()
                        .rect_filled(bar, egui::CornerRadius::same(2), Palette::alpha(colour, shown));
                });
            if area.response.contains_pointer() {
                held.push(toast.id);
            }
            offset += area.response.rect.height() + 8.0;
        }
        for id in held {
            self.toasts.hold(id, now);
        }
        for id in dismissed {
            self.toasts.dismiss(id, now);
        }
        match self.toasts.next_frame(now) {
            Some(after) if after <= 0.0 => ctx.request_repaint(),
            Some(after) => ctx.request_repaint_after(Duration::from_secs_f64(after)),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use uniwow_api::theme::{Level, time};

    use super::{GRACE, LONGEST, MOST, Toasts, lasting};

    const OPEN: f64 = time::OPEN as f64;
    const CLOSE: f64 = time::CLOSE as f64;

    #[test]
    fn a_notice_comes_in_stays_its_time_and_goes_then_is_forgotten() {
        let mut toasts = Toasts::default();
        toasts.push(10.0, "a", Level::Success, "Saved", "intro");
        let toast = toasts.list()[0].clone();
        assert_eq!(toast.shown(10.0), 0.0, "coming in");
        assert!((toast.shown(10.0 + OPEN / 2.0) - 0.5).abs() < 1e-3);
        assert_eq!(toast.shown(10.0 + OPEN), 1.0);
        let goes = 10.0 + lasting(Level::Success);
        assert_eq!(toast.shown(goes - 0.01), 1.0, "staying");
        assert!((toast.shown(goes + CLOSE / 2.0) - 0.5).abs() < 1e-3, "going");
        assert_eq!(toast.shown(goes + CLOSE), 0.0);
        toasts.tidy(goes + CLOSE / 2.0);
        assert_eq!(toasts.list().len(), 1, "still going");
        toasts.tidy(goes + CLOSE);
        assert!(toasts.list().is_empty(), "forgotten once gone");
    }

    #[test]
    fn the_errors_stay_longer_than_the_rest() {
        assert!(lasting(Level::Info) < lasting(Level::Warning));
        assert!(lasting(Level::Warning) < lasting(Level::Error));
        assert_eq!(lasting(Level::Info), lasting(Level::Success));
    }

    #[test]
    fn a_notice_of_the_same_key_takes_the_place_of_the_one_shown_counting_the_same() {
        let mut toasts = Toasts::default();
        toasts.push(0.0, "history", Level::Info, "Undone", "add 1");
        toasts.push(1.0, "history", Level::Info, "Undone", "add 1");
        assert_eq!(toasts.list().len(), 1);
        assert_eq!(toasts.list()[0].count, 2, "the same, counted");
        assert_eq!(toasts.list()[0].goes, 1.0 + lasting(Level::Info), "its time again");
        assert_eq!(toasts.list()[0].came, 0.0, "not coming in again");
        toasts.push(2.0, "history", Level::Warning, "Undo refused", "a group is open");
        let toast = &toasts.list()[0];
        assert_eq!(
            (toast.count, toast.level, toast.title.as_str()),
            (1, Level::Warning, "Undo refused"),
            "another text starts the count again"
        );
        toasts.push(2.0, "other", Level::Info, "Undone", "add 1");
        assert_eq!(toasts.list().len(), 2, "another key, another notice");
        // One that is going is not taken back: a new one comes.
        toasts.dismiss(toasts.list()[0].id, 3.0);
        toasts.push(3.0, "history", Level::Info, "Undone", "add 2");
        assert_eq!(toasts.list().len(), 3);
    }

    #[test]
    fn past_the_most_shown_the_oldest_go_the_errors_last() {
        let mut toasts = Toasts::default();
        toasts.push(0.0, "error", Level::Error, "Failed", "");
        for i in 1..=MOST {
            toasts.push(0.1 * i as f64, &format!("info {i}"), Level::Info, "Info", "");
        }
        let now = 0.1 * MOST as f64;
        let going: Vec<&str> = toasts
            .list()
            .iter()
            .filter(|toast| toast.goes <= now)
            .map(|toast| toast.key.as_str())
            .collect();
        assert_eq!(going, ["info 1"], "the oldest that is not an error");
        assert_eq!(toasts.list().iter().filter(|toast| toast.goes > now).count(), MOST);
    }

    #[test]
    fn the_pointer_keeps_a_notice_and_its_cross_sends_it_away() {
        let mut toasts = Toasts::default();
        toasts.push(0.0, "a", Level::Info, "A", "");
        let goes = lasting(Level::Info);
        toasts.hold(1, goes - 0.1);
        assert_eq!(
            toasts.list()[0].goes,
            goes - 0.1 + GRACE,
            "kept past its time while hovered"
        );
        toasts.dismiss(1, 2.0);
        assert_eq!(toasts.list()[0].goes, 2.0, "going from the click");
        toasts.hold(1, 2.1);
        assert_eq!(toasts.list()[0].goes, 2.0, "one going is not held back");
    }

    #[test]
    fn frames_are_asked_for_while_a_notice_moves_and_once_when_it_is_to_go() {
        let mut toasts = Toasts::default();
        assert_eq!(toasts.next_frame(0.0), None, "nothing to draw, no frame");
        toasts.push(0.0, "a", Level::Info, "A", "");
        assert_eq!(toasts.next_frame(0.0), Some(0.0), "coming in");
        let staying = toasts.next_frame(1.0).expect("a frame when it goes");
        assert!((staying - (lasting(Level::Info) - 1.0)).abs() < 1e-9, "{staying}");
        assert_eq!(toasts.next_frame(lasting(Level::Info) + 0.01), Some(0.0), "going");
    }

    #[test]
    fn a_long_text_is_cut() {
        let mut toasts = Toasts::default();
        toasts.push(0.0, "a", Level::Error, "Long", &"é".repeat(LONGEST + 50));
        let text = &toasts.list()[0].text;
        assert_eq!(text.chars().count(), LONGEST + 1);
        assert!(text.ends_with('…'));
    }
}
