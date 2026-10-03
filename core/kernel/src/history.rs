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
