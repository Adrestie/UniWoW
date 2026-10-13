use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::Editor;

/// Identifies a job started with `Context::spawn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobId(pub u64);

/// When a job of the pool runs among those waiting: the lowest first, those of one priority in the
/// order they were started. `Context::spawn` gives `Priority::FIRST`, for the short jobs that are
/// tied to no place, such as building a pipeline or freeing what was let go; a job reading or
/// putting on the GPU what lies somewhere gives the distance of that place from the camera
/// (`Priority::distance`), so that the nearest is loaded first, whatever its module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Priority(pub u32);

impl Priority {
    /// Before every job given a distance.
    pub const FIRST: Priority = Priority(0);

    /// For what lies `yards` from the camera, by whole yards, after `FIRST`; a distance that is not
    /// a number counts as none.
    pub fn distance(yards: f32) -> Priority {
        let yards = if yards.is_nan() { 0.0 } else { yards.max(0.0) };
        Priority((yards as u32).saturating_add(1))
    }
}

/// What a job sees while it runs on a worker thread.
pub struct JobContext {
    progress: Arc<AtomicU32>,
    cancelled: Arc<AtomicBool>,
    editor: Editor,
}

impl JobContext {
    pub fn new(progress: Arc<AtomicU32>, cancelled: Arc<AtomicBool>, editor: Editor) -> Self {
        Self {
            progress,
            cancelled,
            editor,
        }
    }

    /// Progress between 0 and 1, shown in the Jobs panel.
    pub fn set_progress(&self, fraction: f32) {
        self.progress
            .store(fraction.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// True once the user asked to cancel: the job should return as soon as it can.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// The cancellation flag itself, for code that must check it on its own, such as a hook of
    /// a script interpreter.
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    /// The editor, usable from this worker thread.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }
}

/// How a job ended, delivered to its module on the interface thread by `Module::on_job`.
pub enum JobOutcome {
    /// The value the job returned.
    Done(Box<dyn Any + Send>),
    /// Cancelled before it returned; its value, if any, is dropped.
    Cancelled,
    /// The job panicked, with the panic message.
    Panicked(String),
}

impl JobOutcome {
    /// The value of a job that returned a `T`.
    pub fn take<T: Any>(self) -> Option<T> {
        match self {
            JobOutcome::Done(value) => value.downcast::<T>().ok().map(|value| *value),
            _ => None,
        }
    }
}

/// The work of a job, as the kernel receives it.
pub type JobFn = Box<dyn FnOnce(&JobContext) -> Box<dyn Any + Send> + Send>;

#[cfg(test)]
mod tests {
    use super::Priority;

    #[test]
    fn the_jobs_tied_to_no_place_come_first_then_the_nearest() {
        assert!(Priority::FIRST < Priority::distance(0.0));
        assert!(Priority::distance(10.4) < Priority::distance(11.0));
        assert_eq!(Priority::distance(10.4), Priority::distance(10.9), "by whole yards");
        assert_eq!(Priority::distance(-5.0), Priority::distance(0.0));
        assert_eq!(Priority::distance(f32::NAN), Priority::distance(0.0));
        assert_eq!(Priority::distance(f32::INFINITY), Priority(u32::MAX));
    }
}
