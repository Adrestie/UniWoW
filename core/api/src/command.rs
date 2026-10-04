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

    /// The document the change belongs to, if any: closing that document without saving forgets
    /// its changes (`Context::forget_document`).
    fn document(&self) -> Option<String> {
        None
    }
}

/// A change a module not written in Rust made to its own state, recorded afterwards with
/// `Editor::record_change` (F2). Undo and redo hand it back to the module, on its own thread:
/// they must return at once.
pub trait AppliedChange: Send {
    fn undo(&mut self);
    fn redo(&mut self);
}
