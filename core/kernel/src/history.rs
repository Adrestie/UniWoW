use uniwow_api::Command;

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
        self.done.last().map(|e| e.command.label())
    }

    pub fn redo_label(&self) -> Option<String> {
        self.undone.last().map(|e| e.command.label())
    }
}
