//! The contract of the property grid, as the Inspector of Unity: animatable properties of the
//! catalogue, one row each, with its label and a field for its kind. Offered by the module
//! `properties`; the core draws the `PropertyGrid` objects of every language with it, writes the
//! values changed and records each change done as one undo entry.

use std::ops::Range;
use std::sync::Arc;

use crate::{PropertyKind, PropertyValue, ServiceKey, egui};

/// A property as a row of the grid shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct GridRow {
    /// `<module>/<name>`.
    pub path: String,
    pub label: String,
    /// Its kind, none when no running module declares it: the row is then greyed.
    pub kind: Option<PropertyKind>,
    /// The range of its numbers, which its fields keep to.
    pub range: [f64; 2],
    /// Its value, read once a frame; none when it could not be read.
    pub value: Option<PropertyValue>,
}

/// What a grid shows: how many rows it has, and those around the rows in sight, made and read for
/// this frame from the `first` on; the others are drawn empty until they come in sight.
pub struct GridInput<'a> {
    pub count: usize,
    pub first: usize,
    pub rows: &'a [GridRow],
}

/// What the user did to a value during one frame.
#[derive(Clone, Debug, PartialEq)]
pub enum GridChange {
    None,
    /// A value being changed, such as a number dragged: written at once, recording nothing. The
    /// grid shows it until the change is done.
    Changing {
        path: String,
        value: PropertyValue,
    },
    /// A value changed, done: written, and recorded as one undo entry from the value the property
    /// had before the change began.
    Finished {
        path: String,
        value: PropertyValue,
    },
}

/// What the grid did during one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct GridOutput {
    pub change: GridChange,
    /// The rows in sight, those around which are made and read for the next frame.
    pub shown: Range<usize>,
}

/// The property grid, offered by the module `properties`.
pub trait PropertyGrid: Send + Sync {
    /// Draws the rows of `input` in the room left in `ui`, those in sight only, and lets the user
    /// change their values; writing them is left to the caller. `id` tells the grids of a module
    /// apart: each keeps the value being changed, done once the user does nothing any more.
    fn show(&self, ui: &mut egui::Ui, id: egui::Id, input: &GridInput) -> GridOutput;

    /// Forgets what it keeps of the grid `id`, whose view is gone.
    fn forget(&self, id: egui::Id);
}

/// The service of the property grid.
pub const SERVICE: ServiceKey<Arc<dyn PropertyGrid>> = ServiceKey::new("property-grid");
