//! What the looks share, read once whoever asks for it: the models by their file and the textures
//! by theirs. A load in flight is shared: a thread asking meanwhile waits for it and takes its
//! result. A value is kept while a look holds it, then read again when asked again; one refused is
//! not read again.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, Weak};

use crate::lock;

enum Entry<V> {
    Loading(Arc<Flight<V>>),
    Ready(Weak<V>),
    Refused(String),
}

/// A load in flight, and the threads waiting for it.
struct Flight<V> {
    done: Mutex<Option<Result<Arc<V>, String>>>,
    ready: Condvar,
}

impl<V> Flight<V> {
    fn finish(&self, result: Result<Arc<V>, String>) {
        *lock(&self.done) = Some(result);
        self.ready.notify_all();
    }

    fn wait(&self) -> Result<Arc<V>, String> {
        let mut done = lock(&self.done);
        loop {
            if let Some(result) = done.as_ref() {
                return result.clone();
            }
            done = self.ready.wait(done).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Ends a load that unwinds: those waiting for it are told, and it may be asked again.
struct Landing<'a, K: Eq + Hash, V> {
    cache: &'a Cache<K, V>,
    key: &'a K,
    flight: Arc<Flight<V>>,
    landed: bool,
}

impl<K: Eq + Hash, V> Drop for Landing<'_, K, V> {
    fn drop(&mut self) {
        if !self.landed {
            lock(&self.cache.entries).remove(self.key);
            self.flight.finish(Err("its load failed".to_owned()));
        }
    }
}

pub struct Cache<K, V> {
    entries: Mutex<HashMap<K, Entry<V>>>,
}

impl<K, V> Default for Cache<K, V> {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
        }
    }
}

impl<K: Eq + Hash + Clone, V> Cache<K, V> {
    /// The value of `key`: held by a look, being read by another thread, or read now by `load`.
    pub fn get(&self, key: &K, load: impl FnOnce() -> Result<V, String>) -> Result<Arc<V>, String> {
        let flight = {
            let mut entries = lock(&self.entries);
            match entries.get(key) {
                Some(Entry::Ready(held)) => {
                    if let Some(value) = held.upgrade() {
                        return Ok(value);
                    }
                }
                Some(Entry::Refused(why)) => return Err(why.clone()),
                Some(Entry::Loading(flight)) => {
                    let flight = flight.clone();
                    drop(entries);
                    return flight.wait();
                }
                None => {}
            }
            let flight = Arc::new(Flight {
                done: Mutex::new(None),
                ready: Condvar::new(),
            });
            entries.insert(key.clone(), Entry::Loading(flight.clone()));
            flight
        };
        let mut landing = Landing {
            cache: self,
            key,
            flight: flight.clone(),
            landed: false,
        };
        let result = load().map(Arc::new);
        let entry = match &result {
            Ok(value) => Entry::Ready(Arc::downgrade(value)),
            Err(why) => Entry::Refused(why.clone()),
        };
        lock(&self.entries).insert(key.clone(), entry);
        landing.landed = true;
        flight.finish(result.clone());
        result
    }

    /// Forgets the values no look holds any more.
    pub fn purge(&self) {
        lock(&self.entries).retain(|_, entry| match entry {
            Entry::Ready(held) => held.strong_count() > 0,
            _ => true,
        });
    }

    /// The values held and those refused.
    pub fn counts(&self) -> (usize, usize) {
        let entries = lock(&self.entries);
        let held = entries
            .values()
            .filter(|entry| matches!(entry, Entry::Ready(held) if held.strong_count() > 0))
            .count();
        let refused = entries
            .values()
            .filter(|entry| matches!(entry, Entry::Refused(_)))
            .count();
        (held, refused)
    }
}
