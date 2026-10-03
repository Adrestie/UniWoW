use uniwow_api::{Command, log};

/// One undoable command, the module it belongs to, and its label, read once under guard when
/// the command was applied.
pub struct Part {
    pub owner: String,
    pub label: String,
    pub command: Box<dyn Command>,
}

/// One step of the history: a single command, or the commands of one script run or one group
/// (rule S4), possibly from several modules.
pub struct Entry {
    pub label: String,
    /// In the order they were applied.
    pub parts: Vec<Part>,
}

/// The single undo history shared by every module.
#[derive(Default)]
pub struct History {
    pub done: Vec<Entry>,
    pub undone: Vec<Entry>,
}

impl History {
    pub fn push(&mut self, part: Part) {
        self.push_group(part.label.clone(), vec![part]);
    }

    /// Records several commands as one entry; nothing when there is none.
    pub fn push_group(&mut self, label: String, parts: Vec<Part>) {
        if parts.is_empty() {
            return;
        }
        self.done.push(Entry { label, parts });
        self.undone.clear();
    }

    pub fn undo_label(&self) -> Option<String> {
        self.done.last().map(|entry| entry.label.clone())
    }

    pub fn redo_label(&self) -> Option<String> {
        self.undone.last().map(|entry| entry.label.clone())
    }

    /// Takes the last entry to undo it.
    pub fn take_undo(&mut self, running: impl Fn(&str) -> bool) -> Option<Entry> {
        take(&mut self.done, running)
    }

    /// Takes the last undone entry to redo it.
    pub fn take_redo(&mut self, running: impl Fn(&str) -> bool) -> Option<Entry> {
        take(&mut self.undone, running)
    }

    /// Removes every command of `owner`, done or undone, and returns how many there were; entries
    /// left empty disappear. The other commands stay valid: a command only changes the state of
    /// its own module (F3).
    pub fn purge(&mut self, owner: &str) -> usize {
        purge(&mut self.done, owner) + purge(&mut self.undone, owner)
    }
}

fn purge(entries: &mut Vec<Entry>, owner: &str) -> usize {
    let mut removed = 0;
    for entry in entries.iter_mut() {
        let before = entry.parts.len();
        entry.parts.retain(|part| part.owner != owner);
        removed += before - entry.parts.len();
    }
    entries.retain(|entry| !entry.parts.is_empty());
    removed
}

/// Pops the last entry. Commands whose module is not running, which the purge on failure should
/// make impossible, are dropped with a warning; an entry left empty is skipped.
fn take(entries: &mut Vec<Entry>, running: impl Fn(&str) -> bool) -> Option<Entry> {
    while let Some(mut entry) = entries.pop() {
        entry.parts.retain(|part| {
            let keep = running(&part.owner);
            if !keep {
                log::warn!(
                    "'{}' dropped from the history: '{}' is not running",
                    part.label,
                    part.owner
                );
            }
            keep
        });
        if !entry.parts.is_empty() {
            return Some(entry);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::any::Any;

    use uniwow_api::Command;

    use super::{Entry, History, Part};

    struct Named(&'static str);

    impl Command for Named {
        fn label(&self) -> String {
            self.0.to_owned()
        }

        fn apply(&mut self, _module: &mut dyn Any) {}

        fn revert(&mut self, _module: &mut dyn Any) {}
    }

    fn part(owner: &str, label: &'static str) -> Part {
        Part {
            owner: owner.to_owned(),
            label: label.to_owned(),
            command: Box::new(Named(label)),
        }
    }

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| {
                let parts: Vec<String> = e.parts.iter().map(|p| format!("{}:{}", p.owner, p.label)).collect();
                format!("{}[{}]", e.label, parts.join(","))
            })
            .collect()
    }

    #[test]
    fn an_empty_history_has_nothing_to_undo() {
        let mut history = History::default();
        assert_eq!(history.undo_label(), None);
        assert_eq!(history.redo_label(), None);
        assert!(history.take_undo(|_| true).is_none());
    }

    #[test]
    fn the_last_command_is_undone_then_redone() {
        let mut history = History::default();
        history.push(part("cube", "paint"));
        assert_eq!(history.undo_label().as_deref(), Some("paint"));
        let entry = history.take_undo(|_| true).expect("one entry");
        history.undone.push(entry);
        assert_eq!(history.redo_label().as_deref(), Some("paint"));
        assert!(history.take_redo(|_| true).is_some());
    }

    #[test]
    fn a_new_command_clears_redo() {
        let mut history = History::default();
        history.push(part("cube", "paint"));
        let entry = history.take_undo(|_| true).expect("one entry");
        history.undone.push(entry);
        history.push(part("cube", "speed"));
        assert!(history.undone.is_empty());
    }

    #[test]
    fn a_group_is_one_entry_and_an_empty_group_none() {
        let mut history = History::default();
        history.push_group("script".to_owned(), vec![part("cube", "a"), part("terrain", "b")]);
        history.push_group("nothing".to_owned(), Vec::new());
        assert_eq!(labels(&history.done), vec!["script[cube:a,terrain:b]"]);
    }

    #[test]
    fn the_purge_removes_only_the_failed_module_and_keeps_the_order() {
        let mut history = History::default();
        history.push(part("cube", "a"));
        history.push_group("script".to_owned(), vec![part("cube", "b"), part("terrain", "c")]);
        history.push(part("cube", "d"));
        history.push(part("terrain", "e"));
        let undone = history.take_undo(|_| true).expect("e");
        history.undone.push(undone);
        history.undone.push(Entry {
            label: "f".to_owned(),
            parts: vec![part("cube", "f")],
        });

        assert_eq!(history.purge("cube"), 4);
        assert_eq!(labels(&history.done), vec!["script[terrain:c]"]);
        assert_eq!(labels(&history.undone), vec!["e[terrain:e]"]);
        // Undo works again at once, on the other module's commands.
        let taken = history.take_undo(|id| id != "cube").map(|e| e.label);
        assert_eq!(taken.as_deref(), Some("script"));
    }

    #[test]
    fn commands_of_a_stopped_module_are_dropped_by_the_guard() {
        let mut history = History::default();
        history.push(part("terrain", "b"));
        history.push(part("cube", "c"));
        let taken = history.take_undo(|id| id != "cube").expect("terrain's entry");
        assert_eq!(taken.label, "b");
        assert!(history.done.is_empty());
    }
}
