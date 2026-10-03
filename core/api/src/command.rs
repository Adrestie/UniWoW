use std::any::Any;

/// An undoable modification of a module's state.
///
/// Queued with `Context::execute`, applied by the kernel once the current call into the
/// module has returned, then kept in the single undo history.
///
/// `apply` reads the value it replaces at that moment and keeps it for `revert`. Several commands
/// can be queued before any is applied (events, answers and jobs handled in the same pass): each
/// applies over the result of the one before, so a value read when the command was created would
/// be out of date, and undoing would restore a wrong state.
pub trait Command {
    /// Shown in the Edit menu, e.g. "Set cube colour".
    fn label(&self) -> String;

    /// `module` is the module that queued the command; downcast it to its concrete type.
    fn apply(&mut self, module: &mut dyn Any);

    fn revert(&mut self, module: &mut dyn Any);
}
