use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

use uniwow_api::log::{self, Level, LevelFilter, Log, Metadata, Record};

const MAX_LINES: usize = 5000;

pub struct LogLine {
    pub seconds: f32,
    pub level: Level,
    /// Feature id, `kernel`, or the library that logged.
    pub source: String,
    pub message: String,
}

static LINES: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

struct Logger;

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let noisy = ["wgpu", "naga", "eframe", "egui"]
            .iter()
            .any(|prefix| metadata.target().starts_with(prefix));
        metadata.level() <= if noisy { Level::Warn } else { Level::Info }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = LogLine {
            seconds: START.get_or_init(Instant::now).elapsed().as_secs_f32(),
            level: record.level(),
            source: source_of(record.target()),
            message: record.args().to_string(),
        };
        eprintln!("[{:>5}] {}: {}", line.level, line.source, line.message);
        let mut lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() == MAX_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    fn flush(&self) {}
}

/// `uniwow_feature_sample_cube::layer` → `sample-cube`; `uniwow_kernel::shell` → `kernel`.
fn source_of(target: &str) -> String {
    let root = target.split("::").next().unwrap_or(target);
    if let Some(feature) = root.strip_prefix("uniwow_feature_") {
        feature.replace('_', "-")
    } else if root == "uniwow_kernel" {
        "kernel".to_owned()
    } else {
        root.to_owned()
    }
}

/// Routes the `log` macros of the kernel and of every feature to the Log panel, and logs panics.
pub fn install() {
    START.get_or_init(Instant::now);
    static LOGGER: Logger = Logger;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Info);
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic without message".to_owned());
        log::error!(target: "uniwow_kernel", "panic{location}: {message}");
    }));
}

pub fn with_lines<R>(f: impl FnOnce(&VecDeque<LogLine>) -> R) -> R {
    f(&LINES.lock().unwrap_or_else(|e| e.into_inner()))
}

pub fn clear() {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
