//! The files read last, kept up to a number of bytes in all, those read longest ago let go first:
//! the root of a tile is read by the terrain, the doodads, the buildings and the liquids, each in a
//! job of its own, at about the same moment. A file being read is read once: a thread asking for it
//! meanwhile waits for it and takes it. A file that could not be read is not kept.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

/// A file read, or why not; none where no archive holds it.
pub type Read = Result<Option<Arc<[u8]>>, String>;

/// A file of the cache, read once by the first thread asking.
type Cell = Arc<OnceLock<Read>>;

#[derive(Default)]
struct Kept {
    cells: HashMap<String, Cell>,
    /// The files read, the longest ago first, with their bytes; and the bytes of all.
    order: VecDeque<(String, u64)>,
    bytes: u64,
}

pub struct Recent {
    most: u64,
    kept: Mutex<Kept>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Recent {
    /// The files read last, `most` bytes of them at most.
    pub fn new(most: u64) -> Self {
        Self {
            most,
            kept: Mutex::default(),
        }
    }

    /// The file `path`, case and slashes ignored: kept, being read by another thread, or read now by
    /// `read`, then kept, those read longest ago let go past the bytes it keeps.
    pub fn get(&self, path: &str, read: impl FnOnce() -> Result<Option<Vec<u8>>, String>) -> Read {
        let key = path.replace('/', "\\").to_ascii_lowercase();
        let cell = {
            let mut kept = lock(&self.kept);
            if let Some(cell) = kept.cells.get(&key).cloned() {
                // Read again: the last to be let go.
                if let Some(at) = kept.order.iter().position(|(file, _)| *file == key) {
                    let file = kept.order.remove(at).expect("found");
                    kept.order.push_back(file);
                }
                cell
            } else {
                let cell = Cell::default();
                kept.cells.insert(key.clone(), cell.clone());
                cell
            }
        };
        let mut reader = false;
        let read = cell
            .get_or_init(|| {
                reader = true;
                read().map(|file| file.map(Arc::from))
            })
            .clone();
        if reader {
            let mut kept = lock(&self.kept);
            match &read {
                Err(_) => {
                    kept.cells.remove(&key);
                }
                Ok(file) => {
                    let bytes = file.as_ref().map_or(0, |file| file.len() as u64);
                    kept.order.push_back((key, bytes));
                    kept.bytes += bytes;
                    // The longest ago let go past the bytes kept, the one just read kept.
                    while kept.bytes > self.most && kept.order.len() > 1 {
                        let (file, bytes) = kept.order.pop_front().expect("more than one");
                        kept.cells.remove(&file);
                        kept.bytes -= bytes;
                    }
                }
            }
        }
        read
    }

    /// The files kept and their bytes in all.
    #[cfg(test)]
    pub fn kept(&self) -> (usize, u64) {
        let kept = lock(&self.kept);
        (kept.order.len(), kept.bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::Recent;

    #[test]
    fn a_file_read_again_is_taken_from_those_kept_case_and_slashes_ignored() {
        let recent = Recent::new(100);
        let reads = AtomicUsize::new(0);
        let read = || {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(Some(vec![1, 2, 3]))
        };
        let first = recent.get("World\\Maps\\A\\A_1_2.adt", read).unwrap().unwrap();
        let again = recent.get("world/maps/a/a_1_2.ADT", read).unwrap().unwrap();
        assert_eq!((&*first, reads.load(Ordering::SeqCst)), (&[1u8, 2, 3][..], 1));
        assert!(Arc::ptr_eq(&first, &again), "the same bytes, not copied");
        // Absent from the archives: kept as absent.
        assert_eq!(recent.get("none.adt", || Ok(None)), Ok(None));
        assert_eq!(recent.get("none.adt", || panic!("not read again")), Ok(None));
        // Not read: not kept, read again when asked again.
        assert!(recent.get("broken.adt", || Err("damaged".to_owned())).is_err());
        assert_eq!(
            recent.get("broken.adt", || Ok(Some(vec![9]))),
            Ok(Some(Arc::from(&[9u8][..])))
        );
        assert_eq!(recent.kept(), (3, 4));
    }

    #[test]
    fn past_the_bytes_kept_those_read_longest_ago_are_let_go_first() {
        let recent = Recent::new(10);
        let file = |byte: u8| move || Ok(Some(vec![byte; 4]));
        recent.get("a", file(1)).unwrap();
        recent.get("b", file(2)).unwrap();
        // `a` read again: the last to be let go.
        recent.get("a", || panic!("kept")).unwrap();
        recent.get("c", file(3)).unwrap();
        assert_eq!(recent.kept(), (2, 8), "b let go, the longest ago read");
        recent.get("a", || panic!("kept")).unwrap();
        assert_eq!(recent.get("b", file(4)).unwrap().unwrap()[0], 4, "read again");
        // Larger than all it keeps: kept alone, until the next.
        recent.get("large", || Ok(Some(vec![0; 64]))).unwrap();
        assert_eq!(recent.kept(), (1, 64));
        recent.get("d", file(5)).unwrap();
        assert_eq!(recent.kept(), (1, 4));
    }

    #[test]
    fn a_file_being_read_is_read_once_those_asking_meanwhile_waiting_for_it() {
        let recent = Arc::new(Recent::new(1 << 20));
        let reads = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (recent, reads, start) = (recent.clone(), reads.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    recent
                        .get("root.adt", || {
                            reads.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(50));
                            Ok(Some(vec![7; 16]))
                        })
                        .unwrap()
                        .unwrap()
                })
            })
            .collect();
        let read: Vec<Arc<[u8]>> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
        assert_eq!(reads.load(Ordering::SeqCst), 1, "read once");
        assert!(read.iter().all(|file| Arc::ptr_eq(file, &read[0])));
    }
}
