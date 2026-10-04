//! The contract of the dopesheet, as the Dopesheet of Unity: the rows of a sequence with its keys
//! as diamonds, a ruler and a playhead, and on the left the animated properties, whose values set
//! keys. Offered by the module `dopesheet`; the core draws the `DopesheetView` objects of every
//! language with it, and the left of the `CurveView` objects showing a sequence.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use crate::curve::TimeAxis;
use crate::sequence::{Sequence, Track};
use crate::{PropertyValue, ServiceKey, egui};

/// The property of a track, as the left of its row shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct RowProperty {
    pub label: String,
    /// Whether a running module declares it: the track of a property none declares is greyed, and
    /// its values cannot be changed.
    pub declared: bool,
    /// Its value at the playhead: the track's, or the property's own where the track has no key.
    pub value: Option<PropertyValue>,
    /// The value the property has, which a key added at the playhead takes.
    pub current: Option<PropertyValue>,
    /// The range of its numbers, which its fields keep to.
    pub range: [f64; 2],
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
    /// A change going on, such as keys being dragged or a value typed: the tracks as they are now,
    /// which the caller shows from the next frame on.
    Changing(Vec<Track>),
    /// A change done, to record as one undo entry under its label, from the tracks as they were
    /// before it began.
    Finished {
        label: String,
        tracks: Vec<Track>,
    },
}

/// What the user did during one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct DopesheetOutput {
    pub keys: KeysChange,
    /// The frame the user moved the playhead to, by the ruler or by setting a value there.
    pub playhead: Option<f64>,
}

/// What the user did during one frame on the left of a curve view.
#[derive(Clone, Debug, PartialEq)]
pub struct CurveProperties {
    pub keys: KeysChange,
    /// The frame the user set a value at, where the playhead pauses.
    pub playhead: Option<f64>,
    /// The numbers whose curves are hidden, by property and number.
    pub hidden: BTreeSet<(String, usize)>,
}

/// The dopesheet, offered by the module `dopesheet`.
pub trait Dopesheet: Send + Sync {
    /// Draws `input` in the room left in `ui` and lets the user select, move and delete keys, set a
    /// key from a value, add one at the playhead, remove a track and move the playhead; the sequence
    /// itself is left to the caller. `time`, in frames, is read and changed by zooming and
    /// scrolling; with no pixels per frame yet, the dopesheet fits the sequence. `id` tells the
    /// dopesheets of a module apart: each keeps its selection, its unfolded rows and the gesture
    /// under way, which ends when the tracks change outside it.
    fn show(&self, ui: &mut egui::Ui, id: egui::Id, input: &DopesheetInput, time: &mut TimeAxis) -> DopesheetOutput;

    /// Draws, in the room left in `ui`, the left of the rows alone, each with a box showing or
    /// hiding the curves of its numbers: the left of a curve view.
    fn curve_properties(&self, ui: &mut egui::Ui, id: egui::Id, input: &DopesheetInput) -> CurveProperties;

    /// Forgets what it keeps of the dopesheet `id`, whose view is gone.
    fn forget(&self, id: egui::Id);
}

/// The service of the dopesheet.
pub const SERVICE: ServiceKey<Arc<dyn Dopesheet>> = ServiceKey::new("dopesheet");
