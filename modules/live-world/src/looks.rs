//! The looks of the entities by their display, read once each from the tables by a job of the
//! module, for the thread placing the entities: the displays it finds unread it asks for, once.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use uniwow_api::formats::Formats;
use uniwow_api::models::{LookId, Models};
use uniwow_api::server_link::protocol::Kind;

use crate::lock;

/// A display of a kind of entity.
pub type Display = (Kind, u32);

/// What the display of an entity gives it.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolved {
    /// Drawn as `look`, at the scale of its display and model, with the alpha of its display.
    Look { look: LookId, scale: f32, alpha: f32 },
    /// A marker, and why: a WMO, a display the tables do not have.
    Marker(String),
}

#[derive(Default)]
pub struct Looks {
    resolved: Mutex<HashMap<Display, Resolved>>,
    /// Asked by the thread placing the entities and not read yet, then being read.
    wanted: Mutex<HashSet<Display>>,
    asked: Mutex<HashSet<Display>>,
}

impl Looks {
    /// The displays read, held while a frame is built.
    pub fn resolved(&self) -> MutexGuard<'_, HashMap<Display, Resolved>> {
        lock(&self.resolved)
    }

    /// Asks for `displays` to be read, those not asked before.
    pub fn want(&self, displays: impl IntoIterator<Item = Display>) {
        let asked = lock(&self.asked);
        lock(&self.wanted).extend(displays.into_iter().filter(|display| !asked.contains(display)));
    }

    /// The displays to read, given once.
    pub fn take_wanted(&self) -> Vec<Display> {
        let wanted: Vec<Display> = lock(&self.wanted).drain().collect();
        lock(&self.asked).extend(wanted.iter().copied());
        wanted
    }

    pub fn insert(&self, read: Vec<(Display, Resolved)>) {
        lock(&self.resolved).extend(read);
    }
}

/// The looks of `displays`: a creature's by `Models::display`, with the alpha of its display, a
/// game object's by `Models::object`; a player's is not drawn as a model.
pub fn read(models: &dyn Models, formats: &dyn Formats, displays: &[Display]) -> Vec<(Display, Resolved)> {
    let creatures = formats.creature_displays().ok();
    let alpha = |id: u32| {
        creatures
            .as_ref()
            .and_then(|rows| rows.binary_search_by_key(&id, |row| row.id).ok().map(|at| &rows[at]))
            .map_or(1.0, |row| row.alpha as f32 / 255.0)
    };
    displays
        .iter()
        .map(|&(kind, id)| {
            let resolved = match kind {
                Kind::Creature => match models.display(id) {
                    Ok((look, scale)) => Resolved::Look {
                        look: models.look(&look),
                        scale,
                        alpha: alpha(id),
                    },
                    Err(why) => Resolved::Marker(why),
                },
                Kind::GameObject => match models.object(id) {
                    Ok(Some(look)) => Resolved::Look {
                        look: models.look(&look),
                        scale: 1.0,
                        alpha: 1.0,
                    },
                    Ok(None) => Resolved::Marker(format!("the game object display {id}: a WMO")),
                    Err(why) => Resolved::Marker(why),
                },
                Kind::Player => Resolved::Marker("a player".to_owned()),
            };
            ((kind, id), resolved)
        })
        .collect()
}
