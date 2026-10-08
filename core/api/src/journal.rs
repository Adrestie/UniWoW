//! The journal of a frame: what each part of the editor spent on the interface thread, what was
//! sent to the GPU from any thread (`Queue::write_buffer`, `Queue::write_texture`), the arenas grown
//! and what they copied, and the locks the interface thread waited for; with what the GPU last spent
//! on each layer. The kernel takes it at the start of each frame and writes the frames that missed
//! their deadline where `UNIWOW_SLOW_FRAMES` names a file. Kept cheap whether written or not: sums
//! by part, and counters. The waits for locks are timed on the interface thread only, the thread
//! whose frames the journal is about; the workers' waits are not seen.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

thread_local! {
    static INTERFACE: Cell<bool> = const { Cell::new(false) };
}

static SPENT: LazyLock<Mutex<HashMap<String, Duration>>> = LazyLock::new(Mutex::default);
static WAITED: LazyLock<Mutex<HashMap<&'static str, Duration>>> = LazyLock::new(Mutex::default);
static GPU: Mutex<String> = Mutex::new(String::new());
static UPLOADED: AtomicU64 = AtomicU64::new(0);
static GROWN: AtomicU64 = AtomicU64::new(0);
static GROWTHS: AtomicU64 = AtomicU64::new(0);

fn lock_plain<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Marks the calling thread as the interface thread; the kernel calls it once.
pub fn mark_interface_thread() {
    INTERFACE.with(|interface| interface.set(true));
}

/// Whether the calling thread is the interface thread.
pub fn on_interface_thread() -> bool {
    INTERFACE.with(Cell::get)
}

/// `took` spent by `part` on the interface thread during this frame, added to what it spent before.
pub fn spent(part: &str, took: Duration) {
    let mut spent = lock_plain(&SPENT);
    match spent.get_mut(part) {
        Some(sum) => *sum += took,
        None => {
            spent.insert(part.to_owned(), took);
        }
    }
}

/// `bytes` sent to the GPU.
pub fn uploaded(bytes: u64) {
    UPLOADED.fetch_add(bytes, Ordering::Relaxed);
}

/// An arena grown, copying `bytes` from its buffer before.
pub fn grown(bytes: u64) {
    GROWN.fetch_add(bytes, Ordering::Relaxed);
    GROWTHS.fetch_add(1, Ordering::Relaxed);
}

/// What the GPU spent on the last frame it timed, by layer, as the view says it.
pub fn set_gpu(said: String) {
    *lock_plain(&GPU) = said;
}

/// The lock of `mutex`, poisoned or not; the time the interface thread waited for it, when it
/// waited, kept under `name`.
pub fn lock<'a, T>(mutex: &'a Mutex<T>, name: &'static str) -> MutexGuard<'a, T> {
    if !on_interface_thread() {
        return lock_plain(mutex);
    }
    match mutex.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => {
            let start = Instant::now();
            let guard = lock_plain(mutex);
            let took = start.elapsed();
            *lock_plain(&WAITED).entry(name).or_default() += took;
            guard
        }
    }
}

/// What a frame took: each part's time on the interface thread, the longest first; the bytes sent
/// to the GPU; the arenas grown and the bytes they copied; the waits of the interface thread for a
/// lock, the longest first; what the GPU last spent by layer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub spent: Vec<(String, Duration)>,
    pub uploaded: u64,
    pub growths: u64,
    pub grown: u64,
    pub waited: Vec<(&'static str, Duration)>,
    pub gpu: String,
}

/// What the frame took since it was last taken, the journal left empty for the next.
pub fn take() -> Frame {
    let mut spent: Vec<(String, Duration)> = lock_plain(&SPENT).drain().collect();
    spent.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut waited: Vec<(&'static str, Duration)> = lock_plain(&WAITED).drain().collect();
    waited.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    Frame {
        spent,
        uploaded: UPLOADED.swap(0, Ordering::Relaxed),
        growths: GROWTHS.swap(0, Ordering::Relaxed),
        grown: GROWN.swap(0, Ordering::Relaxed),
        waited,
        gpu: lock_plain(&GPU).clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    #[test]
    fn a_frame_says_what_each_part_spent_what_was_sent_and_grown_and_what_the_interface_waited_for() {
        // One test for the whole journal, which is shared by the process.
        take();
        mark_interface_thread();
        spent("models", Duration::from_millis(2));
        spent("terrain", Duration::from_millis(1));
        spent("models", Duration::from_millis(3));
        uploaded(100);
        std::thread::spawn(|| uploaded(28)).join().unwrap();
        grown(1 << 20);
        set_gpu("terrain 0.50".to_owned());
        // A lock held by another thread for a while: waited for, and said under its name.
        let held = Arc::new(Mutex::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let holder = {
            let (held, barrier) = (held.clone(), barrier.clone());
            std::thread::spawn(move || {
                let guard = held.lock().unwrap();
                barrier.wait();
                std::thread::sleep(Duration::from_millis(30));
                drop(guard);
            })
        };
        barrier.wait();
        *lock(&held, "test lock") += 1;
        holder.join().unwrap();
        *lock(&held, "free lock") += 1;
        let frame = take();
        assert_eq!(
            frame.spent,
            [
                ("models".to_owned(), Duration::from_millis(5)),
                ("terrain".to_owned(), Duration::from_millis(1))
            ]
        );
        // At least: the arenas of other tests send and grow meanwhile.
        assert!(
            frame.uploaded >= 128 && frame.growths >= 1 && frame.grown >= 1 << 20,
            "{frame:?}"
        );
        assert_eq!(frame.waited.len(), 1, "a lock free not waited for");
        assert!(frame.waited[0].0 == "test lock" && frame.waited[0].1 >= Duration::from_millis(20));
        assert_eq!(frame.gpu, "terrain 0.50");
        assert_eq!(take().spent, Vec::new(), "the next frame from nothing");
    }
}
