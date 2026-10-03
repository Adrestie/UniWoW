use uniwow_api::{Command, log};

pub struct Entry {
    pub owner: String,
    pub command: Box<dyn Command>,
}

/// The single undo history shared by every feature.
#[derive(Default)]
pub struct History {
    pub done: Vec<Entry>,
    pub undone: Vec<Entry>,
}

impl History {
    pub fn push(&mut self, owner: String, command: Box<dyn Command>) {
        self.done.push(Entry { owner, command });
        self.undone.clear();
    }

    pub fn undo_label(&self) -> Option<String> {
        self.done.last().map(|entry| entry.command.label())
    }

    pub fn redo_label(&self) -> Option<String> {
        self.undone.last().map(|entry| entry.command.label())
    }

    /// Takes the last command to undo it.
    pub fn take_undo(&mut self, running: impl Fn(&str) -> bool) -> Option<Entry> {
        take(&mut self.done, running)
    }

    /// Takes the last undone command to redo it.
    pub fn take_redo(&mut self, running: impl Fn(&str) -> bool) -> Option<Entry> {
        take(&mut self.undone, running)
    }

    /// Removes every entry of `owner`, done or undone, and returns how many there were. The other
    /// entries stay valid: a command only changes the state of its own feature (F3).
    pub fn purge(&mut self, owner: &str) -> usize {
        let before = self.done.len() + self.undone.len();
        self.done.retain(|entry| entry.owner != owner);
        self.undone.retain(|entry| entry.owner != owner);
        before - self.done.len() - self.undone.len()
    }
}

/// Pops the last entry. One whose feature is not running, which the purge on failure should make
/// impossible, is dropped with a warning and the next one is taken.
fn take(entries: &mut Vec<Entry>, running: impl Fn(&str) -> bool) -> Option<Entry> {
    while let Some(entry) = entries.pop() {
        if running(&entry.owner) {
            return Some(entry);
        }
        log::warn!(
            "'{}' dropped from the history: '{}' is not running",
            entry.command.label(),
            entry.owner
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use std::any::Any;

    use uniwow_api::Command;

    use super::{Entry, History};

    struct Named(&'static str);

    impl Command for Named {
        fn label(&self) -> String {
            self.0.to_owned()
        }

        fn apply(&mut self, _feature: &mut dyn Any) {}

        fn revert(&mut self, _feature: &mut dyn Any) {}
    }

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| format!("{}:{}", e.owner, e.command.label()))
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
        history.push("cube".to_owned(), Box::new(Named("paint")));
        assert_eq!(history.undo_label().as_deref(), Some("paint"));
        let entry = history.take_undo(|_| true).expect("one entry");
        history.undone.push(entry);
        assert_eq!(history.redo_label().as_deref(), Some("paint"));
        assert!(history.take_redo(|_| true).is_some());
    }

    #[test]
    fn a_new_command_clears_redo() {
        let mut history = History::default();
        history.push("cube".to_owned(), Box::new(Named("paint")));
        let entry = history.take_undo(|_| true).expect("one entry");
        history.undone.push(entry);
        history.push("cube".to_owned(), Box::new(Named("speed")));
        assert!(history.undone.is_empty());
    }

    #[test]
    fn the_purge_removes_only_the_failed_feature_and_keeps_the_order() {
        let mut history = History::default();
        history.push("cube".to_owned(), Box::new(Named("a")));
        history.push("terrain".to_owned(), Box::new(Named("b")));
        history.push("cube".to_owned(), Box::new(Named("c")));
        history.push("terrain".to_owned(), Box::new(Named("d")));
        history.push("cube".to_owned(), Box::new(Named("e")));
        let undone = history.take_undo(|_| true).expect("e");
        history.undone.push(undone);
        history.undone.push(Entry {
            owner: "terrain".to_owned(),
            command: Box::new(Named("f")),
        });

        assert_eq!(history.purge("cube"), 3);
        assert_eq!(labels(&history.done), vec!["terrain:b", "terrain:d"]);
        assert_eq!(labels(&history.undone), vec!["terrain:f"]);
        // Undo works again at once, on the other feature's last entry.
        let taken = history.take_undo(|id| id != "cube").map(|e| e.command.label());
        assert_eq!(taken.as_deref(), Some("d"));
    }

    #[test]
    fn an_entry_of_a_stopped_feature_is_dropped_by_the_guard() {
        let mut history = History::default();
        history.push("terrain".to_owned(), Box::new(Named("b")));
        history.push("cube".to_owned(), Box::new(Named("c")));
        let taken = history.take_undo(|id| id != "cube").expect("terrain's entry");
        assert_eq!(taken.command.label(), "b");
        assert!(history.done.is_empty());
    }
}
