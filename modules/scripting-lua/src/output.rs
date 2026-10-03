//! The console output, written by the runs from their worker threads.

use std::collections::VecDeque;
use std::sync::Mutex;

use uniwow_api::egui;

/// Lines kept; the oldest go first.
const CAPACITY: usize = 5_000;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Normal,
    Info,
    Error,
}

#[derive(Default)]
pub struct Output {
    lines: Mutex<VecDeque<(Kind, String)>>,
    /// Repaints the window when a line arrives.
    wake: Mutex<Option<egui::Context>>,
}

impl Output {
    pub fn push(&self, kind: Kind, text: String) {
        {
            let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
            for line in text.lines() {
                if lines.len() == CAPACITY {
                    lines.pop_front();
                }
                lines.push_back((kind, line.to_owned()));
            }
        }
        if let Some(wake) = &*self.wake.lock().unwrap_or_else(|e| e.into_inner()) {
            wake.request_repaint();
        }
    }

    pub fn set_wake(&self, context: &egui::Context) {
        let mut wake = self.wake.lock().unwrap_or_else(|e| e.into_inner());
        if wake.is_none() {
            *wake = Some(context.clone());
        }
    }

    #[cfg(test)]
    pub fn texts(&self) -> Vec<String> {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        lines.iter().map(|(_, text)| text.clone()).collect()
    }

    pub fn clear(&self) {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn show(&self, ui: &mut egui::Ui) {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let height = ui.text_style_height(&egui::TextStyle::Monospace);
        egui::ScrollArea::both()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_rows(ui, height, lines.len(), |ui, rows| {
                for (kind, text) in lines.range(rows) {
                    let text = egui::RichText::new(text).monospace();
                    let text = match kind {
                        Kind::Normal => text,
                        Kind::Info => text.weak(),
                        Kind::Error => text.color(ui.visuals().error_fg_color),
                    };
                    ui.add(egui::Label::new(text).extend());
                }
            });
    }
}
