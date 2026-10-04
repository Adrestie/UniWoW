//! Undo groups (rule S4): the commands applied for one caller on one thread, between its
//! `begin_group` and its `end_group`, become one history entry.

use std::thread::ThreadId;

use crate::history::Part;
use crate::router::module_of;

/// An open group: who opened it, how deeply, and the commands applied so far.
struct Open {
    id: u64,
    caller: String,
    thread: ThreadId,
    label: String,
    /// `begin_group` calls not yet matched by an `end_group`.
    depth: u32,
    parts: Vec<Part>,
}

/// A group taken out of the open ones, to be pushed into the history.
pub struct Closed {
    pub label: String,
    pub parts: Vec<Part>,
}

/// What `end_group` did.
pub enum Ended {
    Closed(Closed),
    /// An inner `end_group`: the group stays open until the outermost one.
    StillOpen,
    /// No group of this caller is open on this thread.
    NotOpen,
}

/// The open groups, in the order they were opened.
#[derive(Default)]
pub struct Groups {
    open: Vec<Open>,
    next_id: u64,
    /// The interface thread, where the modules' own work is never taken into another group.
    interface: Option<ThreadId>,
}

impl Groups {
    /// Groups for an editor whose interface runs on `interface`.
    pub fn new(interface: ThreadId) -> Self {
        Self {
            interface: Some(interface),
            ..Self::default()
        }
    }

    /// Opens a group, or goes one level deeper into the one this caller has open on this thread.
    pub fn begin(&mut self, caller: &str, thread: ThreadId, label: &str) {
        if let Some(group) = self.find(caller, thread) {
            group.depth += 1;
            return;
        }
        self.next_id += 1;
        self.open.push(Open {
            id: self.next_id,
            caller: caller.to_owned(),
            thread,
            label: label.to_owned(),
            depth: 1,
            parts: Vec::new(),
        });
    }

    pub fn end(&mut self, caller: &str, thread: ThreadId) -> Ended {
        let Some(index) = self.position(caller, thread) else {
            return Ended::NotOpen;
        };
        let group = &mut self.open[index];
        group.depth -= 1;
        if group.depth > 0 {
            return Ended::StillOpen;
        }
        Ended::Closed(close(self.open.remove(index)))
    }

    /// Where a command applied for this caller on this thread goes, if it has a group open; else
    /// the group opened last on that thread, as a command delegated to another module runs on its
    /// caller's thread, inside its caller's group. Not on the interface thread, where every module
    /// works: a group left open there would take all the changes made after it.
    pub fn parts_of(&mut self, caller: &str, thread: ThreadId) -> Option<&mut Vec<Part>> {
        let shared = Some(thread) == self.interface;
        let index = self.position(caller, thread).or_else(|| {
            (!shared)
                .then(|| self.open.iter().rposition(|group| group.thread == thread))
                .flatten()
        })?;
        Some(&mut self.open[index].parts)
    }

    /// Closes every group opened on a thread that ended.
    pub fn close_thread(&mut self, thread: ThreadId) -> Vec<Closed> {
        self.close_where(|group| group.thread == thread)
    }

    /// Closes every group of a module that failed.
    pub fn close_module(&mut self, module: &str) -> Vec<Closed> {
        self.close_where(|group| module_of(&group.caller) == module)
    }

    /// Closes the group of this id, whatever its depth: the user's way out of a group left open.
    pub fn close(&mut self, id: u64) -> Option<Closed> {
        let index = self.open.iter().position(|group| group.id == id)?;
        Some(close(self.open.remove(index)))
    }

    /// Removes the commands of a failed module from the open groups; returns how many there were.
    pub fn purge(&mut self, owner: &str) -> usize {
        self.remove(|part| part.owner == owner)
    }

    /// Removes from the open groups the changes of the document `document` of `owner`.
    pub fn forget(&mut self, owner: &str, document: &str) -> usize {
        self.remove(|part| part.of_document(owner, document))
    }

    fn remove(&mut self, doomed: impl Fn(&Part) -> bool) -> usize {
        let mut removed = 0;
        for group in &mut self.open {
            let before = group.parts.len();
            group.parts.retain(|part| !doomed(part));
            removed += before - group.parts.len();
        }
        removed
    }

    /// The id and label of each open group, oldest first.
    pub fn list(&self) -> Vec<(u64, String)> {
        self.open.iter().map(|group| (group.id, group.label.clone())).collect()
    }

    /// Why Undo and Redo cannot run now, if they cannot: an open group already holds a change,
    /// and undoing in the middle of it would mix the order of the history. A group that changed
    /// nothing yet, such as a script waiting for events, blocks nothing.
    pub fn blocking_undo(&self) -> Option<String> {
        let group = self.open.iter().find(|group| !group.parts.is_empty())?;
        Some(format!(
            "a script or a module running has made changes: {}",
            group.label
        ))
    }

    fn find(&mut self, caller: &str, thread: ThreadId) -> Option<&mut Open> {
        self.open
            .iter_mut()
            .find(|group| group.caller == caller && group.thread == thread)
    }

    fn position(&self, caller: &str, thread: ThreadId) -> Option<usize> {
        self.open
            .iter()
            .position(|group| group.caller == caller && group.thread == thread)
    }

    fn close_where(&mut self, matches: impl Fn(&Open) -> bool) -> Vec<Closed> {
        let (closing, open) = std::mem::take(&mut self.open)
            .into_iter()
            .partition(|group| matches(group));
        self.open = open;
        closing.into_iter().map(close).collect()
    }
}

fn close(group: Open) -> Closed {
    Closed {
        label: group.label,
        parts: group.parts,
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::thread::ThreadId;

    use uniwow_api::Command;

    use super::{Closed, Ended, Groups};
    use crate::history::Part;
    use crate::random::Random;
    use crate::router::module_of;

    struct Named(&'static str);

    impl Command for Named {
        fn label(&self) -> String {
            self.0.to_owned()
        }

        fn apply(&mut self, _module: &mut dyn Any) {}

        fn revert(&mut self, _module: &mut dyn Any) {}
    }

    fn part(label: &'static str) -> Part {
        Part {
            owner: "cube".to_owned(),
            label: label.to_owned(),
            document: None,
            command: Box::new(Named(label)),
        }
    }

    #[test]
    fn random_interleavings_put_every_change_where_the_rule_says() {
        let interface = std::thread::current().id();
        let threads = [interface, other_thread(), other_thread(), other_thread()];
        let callers = ["lua#a", "lua#b", "native"];
        for seed in 1..=40 {
            let mut random = Random::new(seed);
            let mut groups = Groups::new(interface);
            // The open groups as the rule has them: caller, thread, label, depth; oldest first.
            let mut model: Vec<(usize, usize, String, u32)> = Vec::new();
            let mut expected: Vec<(String, String)> = Vec::new();
            let mut closed: Vec<Closed> = Vec::new();
            let mut history: Vec<String> = Vec::new();
            for step in 0..300 {
                let (c, t) = (random.below(3) as usize, random.below(4) as usize);
                let (caller, thread) = (callers[c], threads[t]);
                match random.below(6) {
                    0 => {
                        let label = format!("{c}/{t}/{step}");
                        groups.begin(caller, thread, &label);
                        match model.iter_mut().find(|g| (g.0, g.1) == (c, t)) {
                            Some(group) => group.3 += 1,
                            None => model.push((c, t, label, 1)),
                        }
                    }
                    1 => {
                        if let Ended::Closed(group) = groups.end(caller, thread) {
                            closed.push(group);
                        }
                        if let Some(index) = model.iter().position(|g| (g.0, g.1) == (c, t)) {
                            model[index].3 -= 1;
                            if model[index].3 == 0 {
                                model.remove(index);
                            }
                        }
                    }
                    2 => {
                        closed.extend(groups.close_thread(thread));
                        model.retain(|g| g.1 != t);
                    }
                    _ => {
                        let label = step.to_string();
                        let into = model
                            .iter()
                            .find(|g| (g.0, g.1) == (c, t))
                            .or_else(|| (t != 0).then(|| model.iter().rev().find(|g| g.1 == t)).flatten())
                            .map_or_else(|| "history".to_owned(), |g| g.2.clone());
                        expected.push((label.clone(), into));
                        let part = Part {
                            owner: module_of(caller).to_owned(),
                            label: label.clone(),
                            document: None,
                            command: Box::new(Named("change")),
                        };
                        match groups.parts_of(caller, thread) {
                            Some(parts) => parts.push(part),
                            None => history.push(label),
                        }
                    }
                }
            }
            for thread in threads {
                closed.extend(groups.close_thread(thread));
            }
            let mut found: Vec<(String, String)> = history.into_iter().map(|l| (l, "history".to_owned())).collect();
            for group in &closed {
                let numbers: Vec<u64> = group.parts.iter().map(|p| p.label.parse().unwrap()).collect();
                assert!(
                    numbers.windows(2).all(|w| w[0] < w[1]),
                    "seed {seed}: order in {}",
                    group.label
                );
                found.extend(group.parts.iter().map(|p| (p.label.clone(), group.label.clone())));
            }
            found.sort_by_key(|(label, _)| label.parse::<u64>().unwrap());
            assert_eq!(found, expected, "seed {seed}");
        }
    }

    fn other_thread() -> ThreadId {
        std::thread::spawn(|| std::thread::current().id())
            .join()
            .expect("thread")
    }

    fn labels(parts: &[Part]) -> Vec<String> {
        parts.iter().map(|p| p.label.clone()).collect()
    }

    #[test]
    fn a_nested_group_ends_with_the_outermost_end() {
        let here = std::thread::current().id();
        let mut groups = Groups::default();
        groups.begin("lua#run", here, "outer");
        groups.begin("lua#run", here, "inner");
        groups.parts_of("lua#run", here).expect("open").push(part("a"));
        assert!(matches!(groups.end("lua#run", here), Ended::StillOpen));
        groups.parts_of("lua#run", here).expect("still open").push(part("b"));
        let Ended::Closed(closed) = groups.end("lua#run", here) else {
            panic!("the outermost end closes the group");
        };
        assert_eq!(
            (closed.label.as_str(), labels(&closed.parts)),
            ("outer", vec!["a".to_owned(), "b".to_owned()])
        );
        assert!(matches!(groups.end("lua#run", here), Ended::NotOpen));
    }

    #[test]
    fn two_threads_of_one_caller_have_their_own_groups() {
        let (first, second) = (std::thread::current().id(), other_thread());
        let mut groups = Groups::default();
        groups.begin("modules#cpp", first, "first call");
        groups.begin("modules#cpp", second, "second call");
        groups.parts_of("modules#cpp", first).expect("open").push(part("gold"));
        groups
            .parts_of("modules#cpp", second)
            .expect("open")
            .push(part("green"));
        let Ended::Closed(closed) = groups.end("modules#cpp", second) else {
            panic!("closed");
        };
        assert_eq!(
            (closed.label.as_str(), labels(&closed.parts)),
            ("second call", vec!["green".to_owned()])
        );
        assert_eq!(groups.list().len(), 1, "the first thread's group stays open");
    }

    #[test]
    fn a_change_made_for_another_module_on_the_thread_joins_its_group() {
        let (here, elsewhere) = (std::thread::current().id(), other_thread());
        let mut groups = Groups::default();
        groups.begin("scripting-lua#cards.lua #1", here, "cards.lua");
        groups
            .parts_of("sample-scene", here)
            .expect("joined")
            .push(part("card"));
        assert!(
            groups.parts_of("sample-scene", elsewhere).is_none(),
            "not on another thread"
        );
        let Ended::Closed(closed) = groups.end("scripting-lua#cards.lua #1", here) else {
            panic!("closed");
        };
        assert_eq!(labels(&closed.parts), vec!["card".to_owned()]);
    }

    #[test]
    fn a_group_left_open_on_the_interface_thread_takes_no_other_change() {
        let here = std::thread::current().id();
        let mut groups = Groups::new(here);
        groups.begin("scripting-lua", here, "left open");
        assert!(groups.parts_of("sample-cube", here).is_none());
        assert!(
            groups.parts_of("scripting-lua", here).is_some(),
            "its own changes still go in"
        );
    }

    #[test]
    fn an_abandoned_group_blocks_undo_until_it_is_closed() {
        let here = std::thread::current().id();
        let ended = other_thread();
        let mut groups = Groups::default();
        assert_eq!(groups.blocking_undo(), None);
        groups.begin("modules#cpp", ended, "left open");
        assert_eq!(
            groups.blocking_undo(),
            None,
            "a group that changed nothing blocks nothing"
        );
        groups.parts_of("modules#cpp", ended).expect("open").push(part("a"));
        assert!(
            groups
                .blocking_undo()
                .is_some_and(|reason| reason.contains("left open"))
        );

        // Closed when its thread ends…
        let closed = groups.close_thread(ended);
        assert_eq!(labels(&closed[0].parts), vec!["a".to_owned()]);
        assert_eq!(groups.blocking_undo(), None);

        // …or by the user, at any depth.
        groups.begin("lua#run", here, "stuck");
        groups.begin("lua#run", here, "deeper");
        groups.parts_of("lua#run", here).expect("open").push(part("b"));
        assert!(groups.blocking_undo().is_some());
        let id = groups.list()[0].0;
        assert_eq!(groups.close(id).map(|c| c.label).as_deref(), Some("stuck"));
        assert_eq!(groups.blocking_undo(), None);
    }

    #[test]
    fn the_groups_of_a_failed_module_close_and_lose_its_commands() {
        let here = std::thread::current().id();
        let mut groups = Groups::default();
        groups.begin("scripting-lua#paint.lua #1", here, "Lua: paint.lua");
        groups.begin("native-modules#cpp", here, "module");
        let parts = groups.parts_of("scripting-lua#paint.lua #1", here).expect("open");
        parts.push(part("painted"));
        parts.push(Part {
            owner: "scripting-lua".to_owned(),
            label: "own".to_owned(),
            document: None,
            command: Box::new(Named("own")),
        });
        assert_eq!(groups.purge("scripting-lua"), 1);
        let closed = groups.close_module("scripting-lua");
        assert_eq!(labels(&closed[0].parts), vec!["painted".to_owned()]);
        assert_eq!(groups.list().len(), 1, "the module's group stays open");
    }
}
