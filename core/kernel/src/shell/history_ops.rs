//! Applying the undoable commands, and Undo and Redo.

use super::*;

impl Shell {
    pub(super) fn apply_pending(&mut self) {
        self.apply_pending_for(None);
    }

    /// Applies the queued undoable commands. Those applied for a caller with an undo group open on
    /// the calling thread go into the group instead of the history.
    pub(super) fn apply_pending_for(&mut self, caller: Option<(&str, ThreadId)>) {
        for (owner, mut command) in std::mem::take(&mut self.host.pending) {
            let Some(index) = self.running_index(&owner) else {
                continue;
            };
            let module = self.slots[index]
                .module
                .as_deref_mut()
                .expect("running modules are loaded");
            match guarded_as(&owner, || command.apply(module)) {
                Ok(()) => {
                    // A label is module code too: if it panics, the module fails.
                    let read = guarded_as(&owner, || (command.label(), command.document()));
                    let (label, document) = match read {
                        Ok(read) => read,
                        Err(message) => {
                            self.fail(index, format!("label of a command: {message}"));
                            continue;
                        }
                    };
                    let part = history::Part {
                        owner,
                        label,
                        document,
                        command,
                    };
                    match caller.and_then(|(caller, thread)| self.groups.parts_of(caller, thread)) {
                        Some(parts) => parts.push(part),
                        None => self.history.push(part),
                    }
                }
                Err(message) => {
                    let label = guarded_as(&owner, || command.label()).unwrap_or_else(|_| "?".to_owned());
                    self.fail(index, format!("command '{label}': {message}"));
                }
            }
        }
        for (owner, document) in std::mem::take(&mut self.host.forgotten) {
            let forgotten = self.history.forget(&owner, &document) + self.groups.forget(&owner, &document);
            if forgotten > 0 {
                log::info!(
                    "{forgotten} changes of '{document}' ({owner}) left the history: it was closed without saving"
                );
            }
        }
    }

    /// Why Undo and Redo cannot run now: an open group holds a change, or a compiled module still
    /// has signals, changes or commands to handle on its thread, which may record changes the
    /// history must have first. Once its work ends, `undo` and `redo` serve the calls waiting, its
    /// changes among them, before going on.
    pub(super) fn blocking_undo(&self) -> Option<String> {
        self.groups.blocking_undo().or_else(|| {
            self.slots
                .iter()
                .find(|slot| {
                    slot.state.is_running() && slot.compiled.is_some_and(|module| module.activity.pending() > 0)
                })
                .map(|slot| format!("'{}' is still working on its thread", slot.id))
        })
    }

    /// Reverts the last entry, its commands in reverse order. A command whose revert fails makes
    /// its module fail; the other commands of the entry are reverted all the same. Refused while
    /// an undo group is open.
    pub(super) fn undo(&mut self) {
        if let Some(reason) = self.blocking_undo() {
            log::warn!("Undo ignored: {reason}");
            return;
        }
        if self.serve_calls(CALL_BUDGET) {
            log::warn!("Undo ignored: calls are still waiting");
            return;
        }
        let running = self.running_ids();
        let Some(entry) = self.history.take_undo(|id| running.contains(id)) else {
            return;
        };
        let parts = self.replay(entry.parts.into_iter().rev().collect(), true);
        let parts: Vec<history::Part> = parts.into_iter().rev().collect();
        if !parts.is_empty() {
            self.history.undone.push(history::Entry {
                label: entry.label,
                parts,
            });
        }
    }

    pub(super) fn redo(&mut self) {
        if let Some(reason) = self.blocking_undo() {
            log::warn!("Redo ignored: {reason}");
            return;
        }
        if self.serve_calls(CALL_BUDGET) {
            log::warn!("Redo ignored: calls are still waiting");
            return;
        }
        let running = self.running_ids();
        let Some(entry) = self.history.take_redo(|id| running.contains(id)) else {
            return;
        };
        let parts = self.replay(entry.parts, false);
        if !parts.is_empty() {
            self.history.done.push(history::Entry {
                label: entry.label,
                parts,
            });
        }
    }

    /// Reverts or applies `parts` in the given order and returns those that succeeded.
    pub(super) fn replay(&mut self, parts: Vec<history::Part>, revert: bool) -> Vec<history::Part> {
        let mut done = Vec::new();
        for mut part in parts {
            // An earlier part may have made this module fail.
            let Some(index) = self.running_index(&part.owner) else {
                continue;
            };
            let module = self.slots[index]
                .module
                .as_deref_mut()
                .expect("running modules are loaded");
            let outcome = guarded_as(&part.owner, || {
                if revert {
                    part.command.revert(module)
                } else {
                    part.command.apply(module)
                }
            });
            match outcome {
                Ok(()) => done.push(part),
                Err(message) => {
                    let verb = if revert { "undo" } else { "redo" };
                    self.fail(index, format!("{verb} of '{}': {message}", part.label));
                }
            }
        }
        done
    }

    pub(super) fn push_closed(&mut self, closed: Closed) {
        self.history.push_group(closed.label, closed.parts);
    }
}

/// A change a module already made, in the history: undoing and redoing hand it back to the module.
pub(super) struct Recorded {
    pub(super) label: String,
    pub(super) change: Box<dyn uniwow_api::AppliedChange>,
}

impl uniwow_api::Command for Recorded {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, _module: &mut dyn std::any::Any) {
        self.change.redo();
    }

    fn revert(&mut self, _module: &mut dyn std::any::Any) {
        self.change.undo();
    }
}
