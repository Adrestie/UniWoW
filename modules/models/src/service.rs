//! The service `models`: the looks numbered once each with their states, the owners of instances,
//! and what the module steering the loads and its layer read of them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use uniwow_api::formats::Formats;
use uniwow_api::models::{Instance, Look, LookId, LookState, Models};
use uniwow_api::wgpu;

use crate::display;
use crate::groups::{self, Slot};
use crate::lock;

#[derive(Default)]
struct LookTable {
    ids: HashMap<Look, LookId>,
    looks: Vec<(Look, LookState)>,
}

#[derive(Default)]
pub struct Service {
    looks: Mutex<LookTable>,
    owners: Mutex<HashMap<String, Arc<Slot>>>,
    numbers: AtomicU32,
    /// The device of the view, which the owners' buffers are made on; set once: a device made again
    /// after a loss is not taken, the models asking for a restart of the editor then, as the
    /// terrain and the markers do.
    pub gpu: OnceLock<(wgpu::Device, wgpu::Queue)>,
    /// The tables `display` reads, once the module finds them.
    pub formats: Mutex<Option<Arc<dyn Formats>>>,
}

impl Service {
    /// The owners, in no order.
    pub fn owners(&self) -> Vec<Arc<Slot>> {
        lock(&self.owners).values().cloned().collect()
    }

    /// The look `id` and its state.
    pub fn look_of(&self, id: LookId) -> Option<(Look, LookState)> {
        lock(&self.looks).looks.get(id.0 as usize).cloned()
    }

    pub fn set_state(&self, id: LookId, state: LookState) {
        if let Some(entry) = lock(&self.looks).looks.get_mut(id.0 as usize) {
            entry.1 = state;
        }
    }

    fn slot(&self, owner: &str) -> Arc<Slot> {
        lock(&self.owners)
            .entry(owner.to_owned())
            .or_insert_with(|| Arc::new(Slot::new(self.numbers.fetch_add(1, Ordering::Relaxed))))
            .clone()
    }

    fn device(&self) -> Option<(&wgpu::Device, &wgpu::Queue)> {
        self.gpu.get().map(|(device, queue)| (device, queue))
    }
}

impl Models for Service {
    fn look(&self, look: &Look) -> LookId {
        let mut table = lock(&self.looks);
        if let Some(id) = table.ids.get(look) {
            return *id;
        }
        let id = LookId(table.looks.len() as u32);
        table.ids.insert(look.clone(), id);
        table.looks.push((look.clone(), LookState::Waiting));
        id
    }

    fn display(&self, display: u32) -> Result<(Look, f32), String> {
        let formats = lock(&self.formats).clone().ok_or("the client's files are not open")?;
        display::display(&*formats, display)
    }

    fn place(&self, owner: &str, instances: &[Instance]) {
        self.slot(owner)
            .update(self.device(), |kept| *kept = instances.to_vec());
    }

    fn change(&self, owner: &str, changed: &[Instance], removed: &[u64]) {
        self.slot(owner)
            .update(self.device(), |kept| *kept = groups::merge(kept, changed, removed));
    }

    fn clear(&self, owner: &str) {
        lock(&self.owners).remove(owner);
    }

    fn state(&self, look: LookId) -> LookState {
        self.look_of(look).map_or_else(
            || LookState::Refused("no look has this id".to_owned()),
            |(_, state)| state,
        )
    }
}
