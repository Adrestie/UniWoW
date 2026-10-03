use uniwow_api::Command;

pub struct Entry {
    pub owner: String,
    pub command: Box<dyn Command>,
}

/// What Undo or Redo would do now.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// Label of the command.
    Ready(String),
    /// The command belongs to a feature that is not running; the reason is shown, the entry kept.
    Blocked(String),
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

    pub fn undo_step(&self, running: impl Fn(&str) -> bool) -> Step {
        step(self.done.last(), running)
    }

    pub fn redo_step(&self, running: impl Fn(&str) -> bool) -> Step {
        step(self.undone.last(), running)
    }

    /// Takes the last command to undo it, unless its feature is not running: it then stays.
    pub fn take_undo(&mut self, running: impl Fn(&str) -> bool) -> Option<Entry> {
        take(&mut self.done, running)
    }

    /// Takes the last undone command to redo it, unless its feature is not running.
    pub fn take_redo(&mut self, running: impl Fn(&str) -> bool) -> Option<Entry> {
        take(&mut self.undone, running)
    }
}

fn take(entries: &mut Vec<Entry>, running: impl Fn(&str) -> bool) -> Option<Entry> {
    if entries.last().is_some_and(|entry| running(&entry.owner)) {
        entries.pop()
    } else {
        None
    }
}

fn step(entry: Option<&Entry>, running: impl Fn(&str) -> bool) -> Step {
    match entry {
        None => Step::Nothing,
        Some(entry) if running(&entry.owner) => Step::Ready(entry.command.label()),
        Some(entry) => Step::Blocked(format!(
            "'{}' belongs to '{}', which is not running",
            entry.command.label(),
            entry.owner
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;

    use uniwow_api::Command;

    use super::{History, Step};

    struct Paint;

    impl Command for Paint {
        fn label(&self) -> String {
            "paint".to_owned()
        }

        fn apply(&mut self, _feature: &mut dyn Any) {}

        fn revert(&mut self, _feature: &mut dyn Any) {}
    }

    fn history() -> History {
        let mut history = History::default();
        history.push("sample-cube".to_owned(), Box::new(Paint));
        history
    }

    #[test]
    fn an_empty_history_has_nothing_to_undo() {
        let history = History::default();
        assert_eq!(history.undo_step(|_| true), Step::Nothing);
        assert_eq!(history.redo_step(|_| true), Step::Nothing);
    }

    #[test]
    fn the_last_command_of_a_running_feature_can_be_undone() {
        let mut history = history();
        assert_eq!(history.undo_step(|_| true), Step::Ready("paint".to_owned()));
        assert!(history.take_undo(|_| true).is_some());
        assert!(history.done.is_empty());
    }

    #[test]
    fn the_entry_of_a_stopped_feature_is_kept() {
        let mut history = history();
        let step = history.undo_step(|_| false);
        assert!(matches!(&step, Step::Blocked(reason) if reason.contains("sample-cube")));
        assert!(history.take_undo(|_| false).is_none());
        assert_eq!(history.done.len(), 1);
    }

    #[test]
    fn redo_also_keeps_the_entry_of_a_stopped_feature() {
        let mut history = history();
        let entry = history.take_undo(|_| true).expect("running");
        history.undone.push(entry);
        assert!(history.take_redo(|_| false).is_none());
        assert_eq!(history.undone.len(), 1);
        assert!(history.take_redo(|_| true).is_some());
    }

    #[test]
    fn a_new_command_clears_redo() {
        let mut history = history();
        let entry = history.take_undo(|_| true).expect("running");
        history.undone.push(entry);
        history.push("sample-cube".to_owned(), Box::new(Paint));
        assert!(history.undone.is_empty());
        assert_eq!(history.done.len(), 1);
    }
}
