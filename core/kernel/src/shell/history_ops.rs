//! Applying the undoable commands, and Undo and Redo.

use super::*;

impl Shell {
    pub(super) fn apply_pending(&mut self) {
        self.apply_pending_for(None);
    }

    /// Applies the queued undoable commands. Those applied for a caller with an undo group open on
    /// the calling thread go into the group instead of the history.
    pub(super) fn apply_pending_for(&mut self, caller: Option<(&str, ThreadId)>) {
        for (owner, mut command, gesture) in std::mem::take(&mut self.host.pending) {
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
                    if let Some(open) = gesture.and_then(|gesture| self.host.gestures.get_mut(&gesture)) {
                        open.parts.push(part);
                        continue;
                    }
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
        if let Some(open) = self.host.gestures.values().find(|open| !open.parts.is_empty()) {
            return Some(format!("a gesture is under way: {}", open.label));
        }
        self.groups.blocking_undo().or_else(|| {
            self.slots
                .iter()
                .find(|slot| {
                    slot.state.is_running() && slot.compiled.is_some_and(|module| module.activity.pending() > 0)
                })
                .map(|slot| format!("'{}' is still working on its thread", slot.id))
        })
    }

    /// Whether Undo or Redo, named `verb` in the log, may run now. The calls waiting are served
    /// first, so that the changes they make reach the history before it; what they start, work on a
    /// compiled module's thread or a first change in a group, blocks it again.
    fn may_replay(&mut self, verb: &str) -> bool {
        let refused = if let Some(reason) = self.blocking_undo() {
            reason
        } else if self.serve_calls(CALL_BUDGET) {
            "calls are still waiting".to_owned()
        } else {
            self.apply_pending();
            match self.blocking_undo() {
                Some(reason) => reason,
                None => return true,
            }
        };
        log::warn!("{verb} ignored: {refused}");
        self.notify("history", Level::Warning, &format!("{verb} refused"), &refused);
        false
    }

    /// Reverts the last entry, its commands in reverse order. A command whose revert fails makes
    /// its module fail; the other commands of the entry are reverted all the same. Refused while
    /// an undo group is open.
    pub(super) fn undo(&mut self) {
        if !self.may_replay("Undo") {
            return;
        }
        let running = self.running_ids();
        let Some(entry) = self.history.take_undo(|id| running.contains(id)) else {
            return;
        };
        let parts = self.replay(entry.parts.into_iter().rev().collect(), true);
        let parts: Vec<history::Part> = parts.into_iter().rev().collect();
        if !parts.is_empty() {
            self.notify("history", Level::Info, "Undone", &entry.label);
            self.history.undone.push(history::Entry {
                label: entry.label,
                parts,
            });
        }
    }

    pub(super) fn redo(&mut self) {
        if !self.may_replay("Redo") {
            return;
        }
        let running = self.running_ids();
        let Some(entry) = self.history.take_redo(|id| running.contains(id)) else {
            return;
        };
        let parts = self.replay(entry.parts, false);
        if !parts.is_empty() {
            self.notify("history", Level::Info, "Redone", &entry.label);
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

    /// Makes the entry of each gesture ended whose calls are all answered; a gesture left open with
    /// no call waiting closed after two frames, said in the log.
    pub(super) fn settle_gestures(&mut self) {
        let mut done = Vec::new();
        // A gesture is its module's own only within the call it was opened in.
        self.host.opened.clear();
        for (id, open) in &mut self.host.gestures {
            if open.waiting > 0 {
                open.idle = 0;
                continue;
            }
            if open.ended {
                done.push(*id);
            } else {
                open.idle += 1;
                if open.idle > 2 {
                    log::warn!("the gesture '{}' of '{}' was left open: closed", open.label, open.owner);
                    done.push(*id);
                }
            }
        }
        done.sort_unstable();
        for id in done {
            if let Some(open) = self.host.gestures.remove(&id) {
                self.history.push_group(open.label, open.parts);
            }
        }
    }

    /// The gestures of the module `id`, failed, closed with what they hold; its changes in the
    /// others taken out, as from the history.
    pub(super) fn close_gestures_of(&mut self, id: &str) {
        for open in self.host.gestures.values_mut() {
            open.parts.retain(|part| part.owner != id);
        }
        self.host.opened.remove(id);
        let mut theirs: Vec<u64> = self
            .host
            .gestures
            .iter()
            .filter(|(_, open)| open.owner == id)
            .map(|(gesture, _)| *gesture)
            .collect();
        theirs.sort_unstable();
        for gesture in theirs {
            if let Some(open) = self.host.gestures.remove(&gesture) {
                log::warn!("the gesture '{}' closed: '{id}' failed", open.label);
                self.history.push_group(open.label, open.parts);
            }
        }
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
