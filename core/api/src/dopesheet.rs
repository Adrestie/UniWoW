//! The contract of the dopesheet, as the Dopesheet of Unity: the rows of a sequence with its keys
//! as diamonds, a ruler and a playhead. Offered by the module `dopesheet`; the core draws the
//! `DopesheetView` objects of every language with it.

use std::collections::HashMap;
use std::sync::Arc;

use crate::curve::TimeAxis;
use crate::sequence::{Sequence, Track};
use crate::{PropertyValue, ServiceKey, egui};

/// The property of a track, as the left of its row shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct RowProperty {
    pub label: String,
    /// Whether a running module declares it: the track of a property none declares is greyed.
    pub declared: bool,
    /// Its value at the playhead: the track's, or the property's own where the track has no key.
    pub value: Option<PropertyValue>,
}

/// What a dopesheet shows.
pub struct DopesheetInput<'a> {
    pub sequence: &'a Sequence,
    /// The property of each track, by path.
    pub properties: &'a HashMap<String, RowProperty>,
    /// In frames; none without a player.
    pub playhead: Option<f64>,
    /// The name of the row of every key.
    pub title: &'a str,
}

/// What the user did to the keys during one frame.
#[derive(Clone, Debug, PartialEq)]
pub enum KeysChange {
    None,
    /// A change going on, such as keys being dragged: the tracks as they would be.
    Changing(Vec<Track>),
    /// A change done, to record as one undo entry under its label.
    Finished {
        label: String,
        tracks: Vec<Track>,
    },
}

/// What the user did during one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct DopesheetOutput {
    pub keys: KeysChange,
    /// The frame the user moved the playhead to.
    pub playhead: Option<f64>,
}

/// The dopesheet, offered by the module `dopesheet`.
pub trait Dopesheet: Send + Sync {
    /// Draws `input` in the room left in `ui` and lets the user select, move and delete keys and
    /// move the playhead; the sequence itself is left as it is. `time`, in frames, is read and
    /// changed by zooming and scrolling; with no pixels per frame yet, the dopesheet fits the
    /// sequence. `id` tells the dopesheets of a module apart: each keeps its selection, its
    /// unfolded rows and the gesture under way, which ends when the tracks change outside it.
    fn show(&self, ui: &mut egui::Ui, id: egui::Id, input: &DopesheetInput, time: &mut TimeAxis) -> DopesheetOutput;
}

/// The service of the dopesheet.
pub const SERVICE: ServiceKey<Arc<dyn Dopesheet>> = ServiceKey::new("dopesheet");
