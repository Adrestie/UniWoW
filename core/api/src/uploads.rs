//! The budget of what the jobs send to the GPU in a frame: a job passes it with the bytes it is
//! about to send, before it takes any lock, and waits while the frame has sent its budget, so that
//! a burst of loads is spread over the frames rather than carried by one, with the copies and the
//! buffers they are staged in. The view gives the budget again at each frame signal, what is left
//! of it never more than one frame's. An upload larger than the whole budget passes once the budget
//! is whole, the frames after it paying for it. The jobs pass in the order they came, so that none
//! waits for ever behind smaller ones. The interface thread is never held back nor counted: what it
//! writes is what its frame draws. Until the view gives its first frame signal, as in the tests of a
//! module, nothing is held back; once the view stops drawing, the budget is given again by the time,
//! every `REFILL_AFTER`, so that a job never waits without a limit (T2).
//!
//! What a frame needs at once is not passed through it: the per-frame tables of the layers, the
//! bones the thread of the animations writes, and a range of an arena written again in place.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::journal;

/// The budget of a frame by default, in bytes.
pub const DEFAULT_BUDGET: u64 = 32 << 20;
/// The budget given again by the time when no frame signal has given it for so long.
pub const REFILL_AFTER: Duration = Duration::from_millis(100);

struct State {
    budget: u64,
    /// What is left of the budget this frame; below zero after an upload larger than it.
    left: i64,
    /// Whether a frame signal ever gave the budget: until then nothing is held back.
    started: bool,
    /// When the budget was last given.
    given: Instant,
    /// The tickets given to the jobs passing, and the one whose turn it is.
    next: u64,
    turn: u64,
}

impl State {
    /// The budget of a frame given again, added to what is left up to one budget.
    fn give(&mut self) {
        let budget = self.budget as i64;
        self.left = (self.left + budget).min(budget);
        self.given = Instant::now();
    }
}

/// A budget of uploads a frame, shared by the threads passing it.
pub struct Gate {
    state: Mutex<State>,
    turned: Condvar,
    refill_after: Duration,
    /// The frames that gave the budget, for the tests of the view.
    frames: AtomicU64,
}

impl Gate {
    /// A gate of `budget` bytes a frame, given again by the time after `refill_after` without a
    /// frame signal.
    pub fn new(budget: u64, refill_after: Duration) -> Self {
        Self {
            state: Mutex::new(State {
                budget: budget.max(1),
                left: budget.max(1) as i64,
                started: false,
                given: Instant::now(),
                next: 0,
                turn: 0,
            }),
            turned: Condvar::new(),
            refill_after,
            frames: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the budget of a frame, in bytes, from the next frame on.
    pub fn set_budget(&self, bytes: u64) {
        let mut state = self.lock();
        if state.budget != bytes.max(1) {
            state.budget = bytes.max(1);
            self.turned.notify_all();
        }
    }

    pub fn budget(&self) -> u64 {
        self.lock().budget
    }

    /// What is left of the budget this frame, below zero after an upload larger than it.
    pub fn left(&self) -> i64 {
        self.lock().left
    }

    /// How many frames gave the budget.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// The budget of a new frame given, at its frame signal.
    pub fn refill(&self) {
        let mut state = self.lock();
        state.started = true;
        state.give();
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.turned.notify_all();
    }

    /// Waits for the turn of `bytes` the calling thread is about to send: until the jobs that came
    /// before it passed and this frame's budget holds it, or is whole for one larger than the
    /// budget. Returns how long it waited. The interface thread passes at once, not counted; so
    /// does every thread before the first frame signal.
    pub fn pass(&self, bytes: u64) -> Duration {
        if bytes == 0 || journal::on_interface_thread() {
            return Duration::ZERO;
        }
        let mut state = self.lock();
        if !state.started {
            return Duration::ZERO;
        }
        let ticket = state.next;
        state.next += 1;
        let mut waited: Option<Instant> = None;
        loop {
            if state.turn == ticket {
                let whole = state.left >= state.budget as i64;
                if state.left >= bytes as i64 || whole {
                    state.left = state.left.saturating_sub(bytes as i64);
                    state.turn += 1;
                    self.turned.notify_all();
                    return waited.map_or(Duration::ZERO, |since| since.elapsed());
                }
            }
            waited.get_or_insert_with(Instant::now);
            // The view not drawing: the budget given again by the time.
            let since = state.given.elapsed();
            if since >= self.refill_after {
                state.give();
                self.turned.notify_all();
                continue;
            }
            state = self
                .turned
                .wait_timeout(state, self.refill_after - since)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

static GATE: LazyLock<Gate> = LazyLock::new(|| Gate::new(DEFAULT_BUDGET, REFILL_AFTER));

/// Waits for the turn of `bytes` a job is about to send to the GPU, before it takes any lock: the
/// budget of the uploads of a frame, shared by every module (see the module). What it waited is
/// told to the journal of the frames.
pub fn pass(bytes: u64) {
    let waited = GATE.pass(bytes);
    if !waited.is_zero() {
        journal::held(waited);
    }
}

/// The budget of a new frame given: by the view, at each frame signal.
pub fn refill() {
    GATE.refill();
}

/// Sets the budget of the uploads of a frame, in bytes: by the view, from its settings.
pub fn set_budget(bytes: u64) {
    GATE.set_budget(bytes);
}

/// The budget of the uploads of a frame, in bytes.
pub fn budget() -> u64 {
    GATE.budget()
}

/// How many frames gave the budget of the uploads.
pub fn frames() -> u64 {
    GATE.frames()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{Duration, Instant};

    use super::Gate;

    const MB: u64 = 1 << 20;

    /// A gate of 32 MB a frame, never given again by the time within a test.
    fn gate() -> Gate {
        Gate::new(32 * MB, Duration::from_secs(3600))
    }

    /// What the thread `job` returns, which must end within a few seconds.
    fn joined<T>(job: std::thread::JoinHandle<T>, what: &str) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !job.is_finished() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
        job.join().unwrap()
    }

    #[test]
    fn nothing_is_held_back_before_the_first_frame_then_each_frame_gives_its_budget_again() {
        let gate = gate();
        assert_eq!(gate.pass(100 * MB), Duration::ZERO, "no frame signal yet");
        assert_eq!(gate.left(), 32 * MB as i64, "nor counted");
        gate.refill();
        gate.pass(20 * MB);
        gate.pass(12 * MB);
        assert_eq!(gate.left(), 0);
        gate.refill();
        assert_eq!(gate.left(), 32 * MB as i64);
        gate.refill();
        assert_eq!(gate.left(), 32 * MB as i64, "never more than one frame's");
        assert_eq!(gate.frames(), 3);
    }

    #[test]
    fn a_job_past_the_budget_waits_for_the_next_frame() {
        let gate = Arc::new(gate());
        gate.refill();
        gate.pass(30 * MB);
        let passed = Arc::new(AtomicBool::new(false));
        let job = {
            let (gate, passed) = (gate.clone(), passed.clone());
            std::thread::spawn(move || {
                let waited = gate.pass(4 * MB);
                passed.store(true, Ordering::Release);
                waited
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(!passed.load(Ordering::Acquire), "2 MB left of the frame");
        gate.refill();
        let waited = joined(job, "passed at the next frame");
        assert!(waited >= Duration::from_millis(40), "waited {waited:?}");
        assert_eq!(gate.left(), 28 * MB as i64);
    }

    #[test]
    fn an_upload_larger_than_the_budget_passes_once_it_is_whole_and_the_frames_after_pay_for_it() {
        let gate = Arc::new(gate());
        gate.refill();
        gate.pass(MB);
        let job = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.pass(100 * MB))
        };
        std::thread::sleep(Duration::from_millis(30));
        assert!(!job.is_finished(), "the budget not whole");
        gate.refill();
        joined(job, "passed once the budget is whole");
        assert_eq!(gate.left(), 32 * MB as i64 - 100 * MB as i64);
        for _ in 0..3 {
            gate.refill();
        }
        assert_eq!(gate.left(), 28 * MB as i64, "paid over three frames");
    }

    #[test]
    fn the_jobs_pass_in_the_order_they_came_a_small_one_not_before_a_large_one_waiting() {
        let gate = Arc::new(gate());
        gate.refill();
        gate.pass(22 * MB);
        let passed = Arc::new(Mutex::new(Vec::new()));
        let job = |bytes: u64, name: &'static str| {
            let (gate, passed) = (gate.clone(), passed.clone());
            std::thread::spawn(move || {
                gate.pass(bytes);
                passed.lock().unwrap().push(name);
            })
        };
        let large = job(20 * MB, "large");
        std::thread::sleep(Duration::from_millis(30));
        let small = job(MB, "small");
        std::thread::sleep(Duration::from_millis(30));
        assert!(passed.lock().unwrap().is_empty(), "10 MB left would hold the small one");
        assert_eq!(gate.left(), 10 * MB as i64);
        gate.refill();
        joined(large, "the large one passed");
        joined(small, "the small one passed");
        assert_eq!(gate.left(), 11 * MB as i64, "both passed in the frame given");
        assert_eq!(passed.lock().unwrap().len(), 2);
    }

    #[test]
    fn the_interface_thread_is_never_held_back_nor_counted() {
        let gate = gate();
        gate.refill();
        gate.pass(32 * MB);
        let left = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::journal::mark_interface_thread();
                    assert_eq!(gate.pass(500 * MB), Duration::ZERO);
                    gate.left()
                })
                .join()
                .unwrap()
        });
        assert_eq!(left, 0);
    }

    #[test]
    fn without_a_frame_signal_the_budget_is_given_again_by_the_time() {
        let gate = Gate::new(32 * MB, Duration::from_millis(50));
        gate.refill();
        gate.pass(32 * MB);
        let started = Instant::now();
        let waited = gate.pass(MB);
        assert!(started.elapsed() >= Duration::from_millis(40) && waited >= Duration::from_millis(40));
        assert!(waited < Duration::from_secs(5), "{waited:?}");
    }

    #[test]
    fn many_jobs_passing_while_the_frames_come_all_pass_none_waiting_for_ever() {
        let gate = Arc::new(Gate::new(8 * MB, Duration::from_secs(3600)));
        gate.refill();
        let passed = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let start = Arc::new(Barrier::new(9));
        let jobs: Vec<_> = (0..8u64)
            .map(|job| {
                let (gate, passed, start) = (gate.clone(), passed.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    for upload in 0..40u64 {
                        // From a few KB to twice the budget.
                        let bytes = ((job * 41 + upload * 7) % 17 + 1) * MB / 8 + u64::from(upload % 13 == 0) * 16 * MB;
                        gate.pass(bytes);
                        passed.fetch_add(bytes, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        let frames = {
            let (gate, done) = (gate.clone(), done.clone());
            std::thread::spawn(move || {
                while !done.load(Ordering::Acquire) {
                    gate.refill();
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        start.wait();
        let deadline = Instant::now() + Duration::from_secs(30);
        for job in jobs {
            while !job.is_finished() {
                assert!(Instant::now() < deadline, "the jobs never passed");
                std::thread::sleep(Duration::from_millis(5));
            }
            job.join().unwrap();
        }
        done.store(true, Ordering::Release);
        frames.join().unwrap();
        assert!(passed.load(Ordering::Relaxed) > 0);
        assert!(gate.left() <= 8 * MB as i64);
    }
}
