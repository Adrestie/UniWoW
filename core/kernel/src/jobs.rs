//! The pool of worker threads that runs the jobs of the modules (rule T2).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use uniwow_api::{Editor, JobContext, JobFn, JobId, JobOutcome, egui};

use crate::guard::guarded_as;
use crate::router::{Bridge, Request};

type Task = Box<dyn FnOnce() + Send>;

/// A job not yet handed back to its module.
pub struct Running {
    pub id: JobId,
    pub owner: String,
    pub label: String,
    pub started: Instant,
    progress: Arc<AtomicU32>,
    cancelled: Arc<AtomicBool>,
}

impl Running {
    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

pub struct Finished {
    pub id: JobId,
    pub owner: String,
    pub label: String,
    pub outcome: JobOutcome,
}

pub struct Pool {
    sender: mpsc::Sender<Task>,
    threads: usize,
    finished: Arc<Mutex<Vec<Finished>>>,
    running: Vec<Running>,
    next_id: u64,
    /// Repaints the window when a job ends, so that its result is handed back at once.
    wake: Option<egui::Context>,
    /// Told when a job ends, so that the undo groups it left open on its thread are closed.
    bridge: Option<Arc<Bridge>>,
}

impl Pool {
    /// Starts `threads` workers. They end with the process; a job running at exit is abandoned.
    pub fn new(threads: usize, wake: Option<egui::Context>, bridge: Option<Arc<Bridge>>) -> Self {
        let (sender, receiver) = mpsc::channel::<Task>();
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..threads {
            let receiver = receiver.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("uniwow-worker-{index}"))
                .spawn(move || {
                    loop {
                        // The lock is released before the task runs.
                        let task = receiver.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        match task {
                            Ok(task) => task(),
                            Err(_) => return,
                        }
                    }
                });
            if let Err(error) = spawned {
                uniwow_api::log::error!("worker thread {index} could not start: {error}");
            }
        }
        Self {
            sender,
            threads,
            finished: Arc::default(),
            running: Vec::new(),
            next_id: 0,
            wake,
            bridge,
        }
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    pub fn running(&self) -> &[Running] {
        &self.running
    }

    /// Runs a job on the pool, for computations.
    pub fn spawn(&mut self, owner: &str, label: &str, job: JobFn, editor: Editor) -> JobId {
        self.start(owner, label, job, editor, false)
    }

    /// Runs a job on a thread of its own, for work that waits, such as a script: waiting there
    /// never holds a thread of the pool (T2, T6).
    pub fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn, editor: Editor) -> JobId {
        self.start(owner, label, job, editor, true)
    }

    fn start(&mut self, owner: &str, label: &str, job: JobFn, editor: Editor, own_thread: bool) -> JobId {
        self.next_id += 1;
        let id = JobId(self.next_id);
        let progress = Arc::new(AtomicU32::new(0f32.to_bits()));
        let cancelled = Arc::new(AtomicBool::new(false));
        self.running.push(Running {
            id,
            owner: owner.to_owned(),
            label: label.to_owned(),
            started: Instant::now(),
            progress: progress.clone(),
            cancelled: cancelled.clone(),
        });

        let finished = self.finished.clone();
        let wake = self.wake.clone();
        let bridge = self.bridge.clone();
        let thread_name = format!("uniwow {owner}: {label}");
        let owner = owner.to_owned();
        let label = label.to_owned();
        let (job_owner, job_label) = (owner.clone(), label.clone());
        let task: Task = Box::new(move || {
            let context = JobContext::new(progress, cancelled.clone(), editor);
            let outcome = match guarded_as(&owner, || job(&context)) {
                Ok(_) if cancelled.load(Ordering::Relaxed) => JobOutcome::Cancelled,
                Ok(value) => JobOutcome::Done(value),
                Err(message) => JobOutcome::Panicked(message),
            };
            // Queued after every request of the job, so served after them.
            if let Some(bridge) = bridge {
                bridge.queue(Request::ThreadEnded {
                    thread: std::thread::current().id(),
                });
            }
            finished.lock().unwrap_or_else(|e| e.into_inner()).push(Finished {
                id,
                owner,
                label,
                outcome,
            });
            if let Some(wake) = wake {
                wake.request_repaint();
            }
        });
        if own_thread {
            if let Err(error) = std::thread::Builder::new().name(thread_name).spawn(task) {
                self.finished.lock().unwrap_or_else(|e| e.into_inner()).push(Finished {
                    id,
                    owner: job_owner,
                    label: job_label,
                    outcome: JobOutcome::Panicked(format!("its thread could not start: {error}")),
                });
            }
        } else if self.sender.send(task).is_err() {
            uniwow_api::log::error!("job '{job_label}' of '{job_owner}' could not be queued: no worker thread");
        }
        id
    }

    /// Asks a job of `owner` to stop; it ends as cancelled even if it returns a value. A job of
    /// another module is left alone.
    pub fn cancel(&self, owner: &str, id: JobId) {
        match self.running.iter().find(|j| j.id == id) {
            Some(job) if job.owner == owner => job.cancelled.store(true, Ordering::Relaxed),
            Some(job) => uniwow_api::log::warn!("'{owner}' cannot cancel the job '{}' of '{}'", job.label, job.owner),
            None => {}
        }
    }

    /// Asks every job of a module to stop, when the module fails.
    pub fn cancel_owner(&self, owner: &str) {
        for job in self.running.iter().filter(|j| j.owner == owner) {
            job.cancelled.store(true, Ordering::Relaxed);
        }
    }

    /// The jobs that ended since the last call, removed from the running list.
    pub fn take_finished(&mut self) -> Vec<Finished> {
        let finished = std::mem::take(&mut *self.finished.lock().unwrap_or_else(|e| e.into_inner()));
        self.running.retain(|job| !finished.iter().any(|f| f.id == job.id));
        finished
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use uniwow_api::{CommandInfo, Editor, EditorBackend, JobOutcome, serde_json::Value};

    use super::{Finished, Pool};

    struct NoEditor;

    impl EditorBackend for NoEditor {
        fn commands(&self) -> Vec<CommandInfo> {
            Vec::new()
        }

        fn call(&self, _caller: &str, _name: &str, _arguments: Value) -> Result<Value, String> {
            Err("no editor in tests".to_owned())
        }

        fn publish(&self, _source: &str, _topic: &str, _payload: Value) -> Result<(), String> {
            Ok(())
        }

        fn subscribe(&self, _caller: &str, _topic: &str) -> Result<u64, String> {
            Ok(0)
        }

        fn next_event(
            &self,
            _caller: &str,
            _subscription: u64,
            _timeout: Duration,
        ) -> Result<Option<uniwow_api::Event>, String> {
            Ok(None)
        }

        fn unsubscribe(&self, _subscription: u64) {}

        fn setting(&self, _caller: &str, _space: &str, _key: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }

        fn set_setting(&self, _caller: &str, _space: &str, _key: &str, _value: Value) -> Result<(), String> {
            Ok(())
        }

        fn begin_group(&self, _caller: &str, _label: &str) -> Result<(), String> {
            Ok(())
        }

        fn end_group(&self, _caller: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn editor() -> Editor {
        Editor::new(Arc::new(NoEditor), "test")
    }

    fn wait_for(pool: &mut Pool, count: usize) -> Vec<Finished> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut finished = Vec::new();
        while finished.len() < count && Instant::now() < deadline {
            finished.extend(pool.take_finished());
            std::thread::sleep(Duration::from_millis(5));
        }
        finished
    }

    #[test]
    fn a_job_returns_its_value() {
        let mut pool = Pool::new(2, None, None);
        let id = pool.spawn("test", "add", Box::new(|_| Box::new(40 + 2)), editor());
        let finished = wait_for(&mut pool, 1);
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].id, id);
        let outcome = finished.into_iter().next().expect("one").outcome;
        assert_eq!(outcome.take::<i32>(), Some(42));
        assert!(pool.running().is_empty());
    }

    #[test]
    fn jobs_run_in_parallel() {
        // Each job waits for the other at the barrier: only parallel execution lets both end.
        let mut pool = Pool::new(2, None, None);
        let barrier = Arc::new(Barrier::new(2));
        for _ in 0..2 {
            let barrier = barrier.clone();
            pool.spawn(
                "test",
                "meet",
                Box::new(move |_| {
                    barrier.wait();
                    Box::new(())
                }),
                editor(),
            );
        }
        assert_eq!(wait_for(&mut pool, 2).len(), 2);
    }

    #[test]
    fn a_panic_becomes_the_outcome() {
        let mut pool = Pool::new(1, None, None);
        pool.spawn("test", "boom", Box::new(|_| panic!("job failed")), editor());
        let finished = wait_for(&mut pool, 1);
        assert!(matches!(&finished[0].outcome, JobOutcome::Panicked(m) if m == "job failed"));
    }

    #[test]
    fn a_cancelled_job_ends_as_cancelled() {
        let mut pool = Pool::new(1, None, None);
        let id = pool.spawn(
            "test",
            "loop",
            Box::new(|context| {
                while !context.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Box::new(())
            }),
            editor(),
        );
        std::thread::sleep(Duration::from_millis(20));
        pool.cancel("test", id);
        let finished = wait_for(&mut pool, 1);
        assert!(matches!(finished[0].outcome, JobOutcome::Cancelled));
    }

    #[test]
    fn a_job_on_its_own_thread_leaves_the_pool_free() {
        let mut pool = Pool::new(1, None, None);
        let waiting = pool.spawn_thread(
            "lua",
            "events.lua",
            Box::new(|context| {
                while !context.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Box::new(())
            }),
            editor(),
        );
        let computed = pool.spawn("notes", "add", Box::new(|_| Box::new(40 + 2)), editor());
        let finished = wait_for(&mut pool, 1);
        assert_eq!(finished[0].id, computed, "the only pool thread was free");
        pool.cancel("lua", waiting);
        assert!(matches!(wait_for(&mut pool, 1)[0].outcome, JobOutcome::Cancelled));
    }

    #[test]
    fn a_module_cannot_cancel_the_job_of_another() {
        let mut pool = Pool::new(1, None, None);
        let id = pool.spawn(
            "notes",
            "loop",
            Box::new(|context| {
                while !context.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Box::new(())
            }),
            editor(),
        );
        pool.cancel("lua", id);
        assert!(!pool.running()[0].is_cancelled());
        pool.cancel("notes", id);
        assert!(pool.running()[0].is_cancelled());
        wait_for(&mut pool, 1);
    }

    #[test]
    fn a_failed_module_has_its_jobs_cancelled() {
        let mut pool = Pool::new(2, None, None);
        let wait = |context: &uniwow_api::JobContext| {
            while !context.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            Box::new(()) as Box<dyn std::any::Any + Send>
        };
        let failed = pool.spawn("lua", "script", Box::new(wait), editor());
        let other = pool.spawn("notes", "computation", Box::new(wait), editor());
        pool.cancel_owner("lua");
        let finished = wait_for(&mut pool, 1);
        assert_eq!(finished[0].id, failed);
        assert!(
            pool.running().iter().any(|job| job.id == other),
            "other modules' jobs go on"
        );
        pool.cancel_owner("notes");
        wait_for(&mut pool, 1);
    }

    #[test]
    fn progress_is_visible_while_the_job_runs() {
        let mut pool = Pool::new(1, None, None);
        let gate = Arc::new(Barrier::new(2));
        let inside = gate.clone();
        pool.spawn(
            "test",
            "half",
            Box::new(move |context| {
                context.set_progress(0.5);
                inside.wait();
                inside.wait();
                Box::new(())
            }),
            editor(),
        );
        gate.wait();
        assert_eq!(pool.running()[0].progress(), 0.5);
        gate.wait();
        wait_for(&mut pool, 1);
    }
}
