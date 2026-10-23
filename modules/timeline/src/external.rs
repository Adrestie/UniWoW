//! The tracks other modules present (`uniwow_api::timeline`): a sequence object of the Timeline's
//! store for each, set from what its module presents, its changes by the user handed back.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::sequence::{Sequence, Track};
use uniwow_api::timeline::{Playhead, Presented, Timeline};
use uniwow_api::ui::Handle;

/// What a module presents, and where the Timeline is with it.
pub struct Shown {
    pub presented: Presented,
    /// Its sequence object, once made.
    pub handle: Option<Handle>,
    /// The version its object holds.
    pub applied: Option<u64>,
    /// The tracks the object was last given, or last handed back: others are the user's change.
    pub set: Vec<Track>,
    pub edit: Option<(u64, Sequence)>,
    pub playhead: Option<Playhead>,
}

#[derive(Default)]
pub struct Externals {
    pub shown: BTreeMap<String, Shown>,
    /// The objects of the modules that took back what they showed, to destroy.
    pub gone: Vec<Handle>,
    /// The modules that presented for the first time, to show.
    pub fresh: Vec<String>,
}

pub fn lock(externals: &Mutex<Externals>) -> MutexGuard<'_, Externals> {
    externals.lock().unwrap_or_else(|e| e.into_inner())
}

/// The service: what the modules present, kept until the Timeline's frame takes it in.
pub struct Service(pub Arc<Mutex<Externals>>);

impl Timeline for Service {
    fn present(&self, owner: &str, presented: Presented) {
        let mut externals = lock(&self.0);
        match externals.shown.get_mut(owner) {
            Some(shown) => shown.presented = presented,
            None => {
                externals.shown.insert(
                    owner.to_owned(),
                    Shown {
                        presented,
                        handle: None,
                        applied: None,
                        set: Vec::new(),
                        edit: None,
                        playhead: None,
                    },
                );
                externals.fresh.push(owner.to_owned());
            }
        }
    }

    fn withdraw(&self, owner: &str) {
        let mut externals = lock(&self.0);
        if let Some(handle) = externals.shown.remove(owner).and_then(|shown| shown.handle) {
            externals.gone.push(handle);
        }
    }

    fn playhead(&self, owner: &str) -> Option<Playhead> {
        lock(&self.0).shown.get(owner).and_then(|shown| shown.playhead)
    }

    fn take_edit(&self, owner: &str) -> Option<(u64, Sequence)> {
        lock(&self.0).shown.get_mut(owner).and_then(|shown| shown.edit.take())
    }
}
