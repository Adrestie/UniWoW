//! The Timeline offered to the other modules: a module presents the tracks of what it edits (the
//! keys of an effect, say), and the Timeline shows them beside its own sequences, with its
//! playhead, its playback, its dopesheet and its Curves view. What the user changes there comes
//! back to the module, which keeps the data: the Timeline holds a copy only.
//!
//! The tracks presented are not written to properties as they play: their property paths start
//! with `EXTERNAL`, and the module reads the playhead itself.

use std::sync::Arc;

use crate::ServiceKey;
use crate::sequence::Sequence;

/// The start of the property path of a track presented by a module: shown, not played into a
/// property.
pub const EXTERNAL: &str = "@";

/// A stretch of the time named (the phases of an effect: birth, hold, decay), in frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub label: String,
    pub start: u32,
    pub end: u32,
}

/// What a module shows in the Timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Presented {
    /// Its name in the list of the sequences.
    pub title: String,
    pub sequence: Sequence,
    pub segments: Vec<Segment>,
    /// Changes with each presentation: the edits that come back say which they started from.
    pub version: u64,
}

/// Where the playhead is on what a module presents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playhead {
    /// In seconds.
    pub time: f64,
    pub playing: bool,
}

pub trait Timeline: Send + Sync {
    /// Shows `presented` in place of what `owner` showed before; the Timeline shows it at once
    /// when it shows nothing else of the user's.
    fn present(&self, owner: &str, presented: Presented);

    /// Takes back what `owner` showed.
    fn withdraw(&self, owner: &str);

    /// The playhead on what `owner` presents, while the Timeline shows it.
    fn playhead(&self, owner: &str) -> Option<Playhead>;

    /// The tracks as the user changed them in the Timeline since `owner` last presented, with the
    /// version they started from; taken once.
    fn take_edit(&self, owner: &str) -> Option<(u64, Sequence)>;

    /// Shows what `owner` presents, whatever the Timeline showed (an effect just opened).
    fn focus(&self, owner: &str) {
        let _ = owner;
    }
}

pub type Handle = Arc<dyn Timeline>;

/// Provided by the module `timeline`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("timeline");
