//! The players of the modules' interface objects, moved on by the kernel at each frame: the value
//! of each track of a player's sequence at its time goes to its property, through the catalogue of
//! animatable properties (milestone 8).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use uniwow_api::ui::{self, Handle, SharedUi};
use uniwow_api::{Editor, PropertyValue};

/// The objects of one module, and the module's `Editor`, which writes their values.
pub struct Store {
    pub owner: String,
    pub objects: SharedUi,
    pub editor: Editor,
}

/// What a player last wrote: where in its sequence, and each track's value.
#[derive(Default)]
struct Written {
    /// Its time, its sequence and the version of that sequence.
    at: Option<(f64, Handle, u64)>,
    values: HashMap<String, PropertyValue>,
}

#[derive(Default)]
pub struct Players {
    /// When a player was last moved on; none when none played.
    last: Option<Instant>,
    /// By module and player.
    written: HashMap<(String, Handle), Written>,
}

impl Players {
    /// Moves the players of `stores` on to `now`, then writes the values of their tracks wherever
    /// their time or their sequence changed, and only the values that changed. Returns whether one
    /// plays on, for the next frame to come.
    pub fn tick(&mut self, stores: &[Store], now: Instant) -> bool {
        let seconds = self.last.map_or(0.0, |last| now.duration_since(last).as_secs_f64());
        let mut playing = false;
        let mut seen = HashSet::new();
        for store in stores {
            let (frames, plays) = ui::lock(&store.objects).advance_players(seconds);
            playing |= plays;
            for frame in frames {
                let key = (store.owner.clone(), frame.player);
                seen.insert(key.clone());
                let written = self.written.entry(key).or_default();
                let at = (frame.time, frame.sequence, frame.generation);
                if written.at == Some(at) {
                    continue;
                }
                written.at = Some(at);
                for track in &frame.data.tracks {
                    // A number without keys keeps the value the property has.
                    let current = if track.has_bare_number() {
                        store.editor.read_property(&track.property).ok()
                    } else {
                        None
                    };
                    let Some(value) = track.evaluate(frame.time, current) else {
                        continue;
                    };
                    if written.values.get(&track.property) == Some(&value) {
                        continue;
                    }
                    // A property no running module declares has nothing to take the value.
                    if store.editor.write_property(&track.property, value).is_ok() {
                        written.values.insert(track.property.clone(), value);
                    }
                }
            }
        }
        self.written.retain(|key, _| seen.contains(key));
        self.last = playing.then_some(now);
        playing
    }
}
