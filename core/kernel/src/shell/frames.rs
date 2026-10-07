//! The journal of the frames, written where `UNIWOW_SLOW_FRAMES` names a file: a line for each frame,
//! from the start of one to the start of the next, with what it took (`uniwow_api::journal`), the
//! frames that missed a vertical blank of 60 Hz marked slow.

use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use uniwow_api::{journal, log};

/// A frame longer than this missed a vertical blank of 60 Hz.
const SLOW: Duration = Duration::from_micros(17_500);
/// The shortest time a part must have spent to be written.
const SAID: Duration = Duration::from_micros(20);

/// The frames as they are written.
pub(super) struct Frames {
    file: Option<File>,
    origin: Instant,
    started: Option<Instant>,
    window_drawn: Option<Instant>,
}

impl Frames {
    /// The journal of the frames, written where `UNIWOW_SLOW_FRAMES` names a file; none otherwise.
    pub(super) fn new() -> Self {
        let file = std::env::var_os("UNIWOW_SLOW_FRAMES").and_then(|path| {
            let mut file = File::create(&path)
                .map_err(|error| log::warn!("the journal of the frames is not written to {path:?}: {error}"))
                .ok()?;
            let since = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let _ = writeln!(file, "started at {:.3} s of the Unix epoch", since.as_secs_f64());
            let _ = writeln!(
                file,
                "seconds\tms\tslow\tjobs not handed back\tsent to the GPU\tarenas grown\twaits of the interface \
                 thread\tGPU of the frame timed last\tthe interface thread by part, ms"
            );
            Some(file)
        });
        Self {
            file,
            origin: Instant::now(),
            started: None,
            window_drawn: None,
        }
    }

    /// At the start of a frame: the frame before written with what it took, `jobs` not yet handed
    /// back to their modules.
    pub(super) fn start(&mut self, jobs: usize) {
        let now = Instant::now();
        if let Some(drawn) = self.window_drawn.take() {
            journal::spent("eframe: painting and presenting", now - drawn);
        }
        let frame = journal::take();
        let Some(started) = self.started.replace(now) else {
            return;
        };
        let Some(file) = &mut self.file else {
            return;
        };
        let interval = now - started;
        let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
        let waited: Vec<String> = frame
            .waited
            .iter()
            .map(|(lock, took)| format!("{lock} {:.2}", ms(*took)))
            .collect();
        let parts: Vec<String> = frame
            .spent
            .iter()
            .filter(|(_, took)| *took >= SAID)
            .map(|(part, took)| format!("{part} {:.2}", ms(*took)))
            .collect();
        let _ = writeln!(
            file,
            "{:.3}\t{:.2}\t{}\t{jobs}\t{:.2} MB\t{} {:.1} MB\t{}\t{}\t{}",
            (started - self.origin).as_secs_f64(),
            ms(interval),
            if interval > SLOW { "slow" } else { "" },
            frame.uploaded as f64 / (1024.0 * 1024.0),
            frame.growths,
            frame.grown as f64 / (1024.0 * 1024.0),
            waited.join("; "),
            frame.gpu,
            parts.join("; ")
        );
    }

    /// The window of the frame drawn by the kernel; eframe paints and presents it next.
    pub(super) fn window_drawn(&mut self) {
        self.window_drawn = Some(Instant::now());
    }
}
