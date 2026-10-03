use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::Editor;

/// Identifies a job started with `Context::spawn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobId(pub u64);

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

    /// The editor, usable from this worker thread.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }
}

/// How a job ended, delivered to its feature on the interface thread by `Feature::on_job`.
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
