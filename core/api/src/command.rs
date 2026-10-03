use std::any::Any;

/// An undoable modification of a feature's state.
///
/// Queued with `Context::execute`, applied by the kernel once the current call into the
/// feature has returned, then kept in the single undo history.
pub trait Command {
    /// Shown in the Edit menu, e.g. "Set cube colour".
    fn label(&self) -> String;

    /// `feature` is the feature that queued the command; downcast it to its concrete type.
    fn apply(&mut self, feature: &mut dyn Any);

    fn revert(&mut self, feature: &mut dyn Any);
}
