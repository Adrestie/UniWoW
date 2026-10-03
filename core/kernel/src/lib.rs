//! UniWoW kernel: loads the module DLLs and provides the core services.
//!
//! The kernel is linked into the executable. Modules never depend on it: they only see the
//! contracts of `uniwow-api`.

mod compiled;
mod groups;
mod guard;
mod history;
mod host;
mod jobs;
mod layout;
mod loader;
mod logger;
mod manifest;
mod order;
mod panels;
mod requirements;
mod router;
mod settings;
mod shell;

use uniwow_api::{eframe, egui};

pub fn run() -> eframe::Result {
    logger::install();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UniWoW")
            .with_inner_size([1400.0, 900.0]),
        ..Default::default()
    };
    eframe::run_native("UniWoW", options, Box::new(|cc| Ok(Box::new(shell::Shell::new(cc)))))
}
