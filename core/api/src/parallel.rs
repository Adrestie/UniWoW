//! Fork-join on the kernel's pool (rule T2): `parallel_for` splits work into slices that the
//! workers of the pool share with the calling thread, which works on its own slices while it
//! waits, never on other jobs. The pool runs a job before any slice, and a worker helping with
//! slices goes back to the jobs between two slices, so that a job of another module waits for at
//! most one slice; once the job is done, it comes back to the slices left.

use std::any::Any;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// A helper of a `parallel_for`, run by a worker of the pool: it takes slices until none is left,
/// or until its argument says that a job waits for the worker. Returns whether it left slices to
/// take: the pool then queues it again, to come back once the job is done.
pub type Helper = Arc<dyn Fn(&dyn Fn() -> bool) -> bool + Send + Sync>;

/// The workers of the pool that help with the slices: how many, and how to queue a helper.
#[derive(Clone)]
pub struct Workers {
    pub threads: usize,
    pub queue: Arc<dyn Fn(Helper) + Send + Sync>,
}

static WORKERS: OnceLock<Workers> = OnceLock::new();

/// The workers `parallel_for` uses: the kernel's pool, which the kernel sets when it starts.
pub fn set_workers(workers: Workers) {
    let _ = WORKERS.set(workers);
}

/// Runs `work` over `0..count` in slices of `slice` indices (the last one shorter), on the workers
/// of the kernel's pool and the calling thread; before the kernel sets them, as in the tests of a
/// module, on the calling thread alone. Slices are meant to be short, under a millisecond.
///
/// `work` may borrow the caller's data: this returns once every slice has ended. After a panic the
/// slices left are skipped, and the first panic is resumed here, in the caller. A slice may call
/// `parallel_for` in turn.
pub fn parallel_for(count: usize, slice: usize, work: impl Fn(Range<usize>) + Sync) {
    split(WORKERS.get(), count, slice, &work);
}

impl Workers {
    /// `parallel_for` on these workers.
    pub fn parallel_for(&self, count: usize, slice: usize, work: impl Fn(Range<usize>) + Sync) {
        split(Some(self), count, slice, &work);
    }
}

/// The work of a `parallel_for`, its lifetime erased: only a slice taken before the caller has
/// seen every slice end reaches it, and the caller waits for those.
struct Work(*const (dyn Fn(Range<usize>) + Sync + 'static));

// The work is `Sync`, and reached only while the caller waits (see `Work`).
unsafe impl Send for Work {}
unsafe impl Sync for Work {}

/// The slices of one `parallel_for`, shared by the caller and its helpers.
struct Slices {
    work: Work,
    count: usize,
    slice: usize,
    total: usize,
    next: AtomicUsize,
    failed: AtomicBool,
    panic: Mutex<Option<Box<dyn Any + Send>>>,
    /// The slices not ended yet, and the caller waiting for none to be left.
    unfinished: Mutex<usize>,
    ended: Condvar,
}

impl Slices {
    /// Takes and runs slices until none is left, or until `leave` says that the thread is wanted
    /// elsewhere; returns whether it left slices to take.
    fn help(&self, leave: &dyn Fn() -> bool) -> bool {
        loop {
            if leave() {
                return self.next.load(Ordering::Acquire) < self.total;
            }
            let index = self.next.fetch_add(1, Ordering::AcqRel);
            if index >= self.total {
                return false;
            }
            if !self.failed.load(Ordering::Acquire) {
                let range = index * self.slice..((index + 1) * self.slice).min(self.count);
                // SAFETY: this slice was taken before the last one ended, so the caller still waits
                // in `split`, and the borrows of its work hold.
                let work = unsafe { &*self.work.0 };
                if let Err(payload) = catch_unwind(AssertUnwindSafe(|| work(range))) {
                    self.failed.store(true, Ordering::Release);
                    self.panic
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_or_insert(payload);
                }
            }
            let mut unfinished = self.unfinished.lock().unwrap_or_else(|e| e.into_inner());
            *unfinished -= 1;
            if *unfinished == 0 {
                self.ended.notify_all();
            }
        }
    }
}

fn split(workers: Option<&Workers>, count: usize, slice: usize, work: &(dyn Fn(Range<usize>) + Sync)) {
    let slice = slice.max(1);
    let total = count.div_ceil(slice);
    let Some(workers) = workers.filter(|_| total > 1) else {
        for index in 0..total {
            work(index * slice..((index + 1) * slice).min(count));
        }
        return;
    };
    // SAFETY: the lifetime is erased for the helpers queued on the pool; `Work` says why no slice
    // reaches the work once this function returns.
    let work: &(dyn Fn(Range<usize>) + Sync + 'static) = unsafe { std::mem::transmute(work) };
    let slices = Arc::new(Slices {
        work: Work(work),
        count,
        slice,
        total,
        next: AtomicUsize::new(0),
        failed: AtomicBool::new(false),
        panic: Mutex::new(None),
        unfinished: Mutex::new(total),
        ended: Condvar::new(),
    });
    for _ in 0..(total - 1).min(workers.threads) {
        let slices = slices.clone();
        (workers.queue)(Arc::new(move |leave| slices.help(leave)));
    }
    // The caller never leaves its own slices.
    let _ = slices.help(&|| false);
    let mut unfinished = slices.unfinished.lock().unwrap_or_else(|e| e.into_inner());
    while *unfinished > 0 {
        unfinished = slices.ended.wait(unfinished).unwrap_or_else(|e| e.into_inner());
    }
    drop(unfinished);
    if let Some(payload) = slices.panic.lock().unwrap_or_else(|e| e.into_inner()).take() {
        resume_unwind(payload);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::parallel_for;

    #[test]
    fn without_workers_the_slices_run_on_the_calling_thread() {
        let caller = std::thread::current().id();
        let seen: Vec<AtomicUsize> = (0..10).map(|_| AtomicUsize::new(0)).collect();
        parallel_for(10, 3, |range| {
            assert_eq!(std::thread::current().id(), caller);
            for index in range {
                seen[index].fetch_add(1, Ordering::Relaxed);
            }
        });
        assert!(seen.iter().all(|count| count.load(Ordering::Relaxed) == 1));
        parallel_for(0, 3, |_| panic!("no slice for no work"));
    }
}
