use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Mutex;
use std::time::Instant;

use uniwow_api::egui;
use uniwow_api::log::{self, Level, LevelFilter, Log, Metadata, Record};

const MAX_LINES: usize = 5000;
/// The errors kept for the notices until the shell takes them; older ones go first.
const MAX_ERRORS: usize = 32;
/// The target of an error the kernel logs and shows in a notice of its own, not again as an error.
pub const NOTIFIED: &str = "uniwow_kernel::notified";

#[derive(Clone)]
pub struct LogLine {
    pub seconds: f32,
    pub level: Level,
    /// Module id, `kernel`, or the library that logged.
    pub source: String,
    pub message: String,
}

static LINES: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
/// The errors logged since the shell last took them, shown in notices.
static ERRORS: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());
/// The window woken when another thread logs an error, so that its notice shows at once.
static WAKE: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();
/// Whether the logger routes the `log` macros, as in the editor; not in the tests.
static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
        if line.level == Level::Error && record.target() != NOTIFIED {
            keep_error(line.clone());
            // Not from the interface thread, which may be inside egui and draws the notice anyway.
            if !uniwow_api::journal::on_interface_thread()
                && let Some(ctx) = WAKE.get()
            {
                ctx.request_repaint();
            }
        }
        let mut lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() == MAX_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    fn flush(&self) {}
}

/// `uniwow_module_sample_cube::layer` → `sample-cube`; `uniwow_kernel::shell` → `kernel`.
fn source_of(target: &str) -> String {
    let root = target.split("::").next().unwrap_or(target);
    if let Some(module) = root.strip_prefix("uniwow_module_") {
        module.replace('_', "-")
    } else if root == "uniwow_kernel" {
        "kernel".to_owned()
    } else {
        root.to_owned()
    }
}

/// Routes the `log` macros of the kernel and of every module to the Log panel, and logs panics.
pub fn install() {
    START.get_or_init(Instant::now);
    static LOGGER: Logger = Logger;
    INSTALLED.store(log::set_logger(&LOGGER).is_ok(), std::sync::atomic::Ordering::Relaxed);
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
        // Logged under the module the kernel was calling, so the Log panel names it.
        let target = match crate::guard::current_module() {
            Some(id) => format!("uniwow_module_{}", id.replace('-', "_")),
            None => "uniwow_kernel".to_owned(),
        };
        log::error!(target: target.as_str(), "panic{location}: {message}");
    }));
}

pub fn len() -> usize {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// A copy of the lines in `range`, so that the lock is not held while they are drawn: a log or a
/// panic during drawing would otherwise wait for it forever.
pub fn lines(range: Range<usize>) -> Vec<LogLine> {
    let lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
    lines
        .range(range.start.min(lines.len())..range.end.min(lines.len()))
        .cloned()
        .collect()
}

pub fn clear() {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

fn keep_error(line: LogLine) {
    let mut errors = ERRORS.lock().unwrap_or_else(|e| e.into_inner());
    if errors.len() == MAX_ERRORS {
        errors.pop_front();
    }
    errors.push_back(line);
}

/// The errors logged since the last call, the oldest first, for the notices; none while the logger
/// is not installed, as in the tests, whose own errors are not the shell's.
pub fn take_errors() -> Vec<LogLine> {
    if INSTALLED.load(std::sync::atomic::Ordering::Relaxed) {
        drain_errors()
    } else {
        Vec::new()
    }
}

fn drain_errors() -> Vec<LogLine> {
    ERRORS.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
}

/// The window to wake when another thread logs an error.
pub fn wake(ctx: &egui::Context) {
    let _ = WAKE.set(ctx.clone());
}

#[cfg(test)]
mod tests {
    use uniwow_api::log::{Level, Log, Record};

    use super::{Logger, NOTIFIED, drain_errors, take_errors};

    fn log(level: Level, target: &str, message: &str) {
        Logger.log(
            &Record::builder()
                .level(level)
                .target(target)
                .args(format_args!("{message}"))
                .build(),
        );
    }

    #[test]
    fn the_errors_wait_for_the_notices_but_those_the_kernel_shows_itself() {
        log(Level::Error, "uniwow_module_sample_cube", "error of the notices test");
        log(Level::Warn, "uniwow_module_sample_cube", "warning of the notices test");
        log(Level::Error, NOTIFIED, "error shown by the notices test");
        assert!(
            take_errors().is_empty(),
            "the logger not installed, the shell takes none"
        );
        let taken: Vec<(String, String)> = drain_errors()
            .into_iter()
            .filter(|line| line.message.contains("notices test"))
            .map(|line| (line.source, line.message))
            .collect();
        assert_eq!(
            taken,
            [("sample-cube".to_owned(), "error of the notices test".to_owned())]
        );
        assert!(
            !drain_errors().iter().any(|line| line.message.contains("notices test")),
            "taken once"
        );
    }
}
