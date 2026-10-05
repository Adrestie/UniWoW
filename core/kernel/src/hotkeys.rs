//! The hotkeys of the kernel and of the modules, bound to the keys the user chose in
//! *Edit > Hotkey*: the settings keep those keys by `<owner>/<name>`, for the hotkeys bound to other
//! keys than their own by default.

use std::collections::BTreeMap;

use uniwow_api::hotkey::{Hotkey, HotkeyKind, Keys};
use uniwow_api::{HotkeySpec, egui, log};

pub const KERNEL: &str = "kernel";

pub struct Entry {
    pub owner: String,
    pub name: String,
    pub label: String,
    pub default: Keys,
    pub hotkey: Hotkey,
}

impl Entry {
    pub fn id(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Whether a hotkey of `kind` may be bound to `keys`: modifiers alone are never pressed as such.
pub fn allowed(kind: HotkeyKind, keys: Keys) -> bool {
    !(kind == HotkeyKind::Press && keys.modifiers_alone())
}

pub struct Hotkeys {
    pub entries: Vec<Entry>,
    pub undo: Hotkey,
    pub redo: Hotkey,
}

impl Hotkeys {
    /// The kernel's own hotkeys, bound to the keys `saved` for them.
    pub fn new(saved: &BTreeMap<String, String>) -> Self {
        let undo = Hotkey::new(HotkeyKind::Press, Keys::ctrl(egui::Key::Z));
        let redo = Hotkey::new(HotkeyKind::Press, Keys::ctrl(egui::Key::Y));
        let mut hotkeys = Self {
            entries: Vec::new(),
            undo: undo.clone(),
            redo: redo.clone(),
        };
        for (name, label, hotkey) in [("undo", "Undo", undo), ("redo", "Redo", redo)] {
            let spec = HotkeySpec {
                name: name.to_owned(),
                label: label.to_owned(),
                default: hotkey.keys(),
                hotkey,
            };
            hotkeys.declare(KERNEL, spec, saved);
        }
        hotkeys
    }

    /// Adds a hotkey `owner` declared, bound to the keys `saved` for it, or else to its own by
    /// default. One declared twice is left without keys, and so is one of `HotkeyKind::Press` by
    /// default bound to modifiers alone.
    pub fn declare(&mut self, owner: &str, spec: HotkeySpec, saved: &BTreeMap<String, String>) {
        let id = format!("{owner}/{}", spec.name);
        let kind = spec.hotkey.kind();
        if self.entries.iter().any(|entry| entry.id() == id) {
            log::warn!("hotkey '{id}' declared twice: the second is left without keys");
            spec.hotkey.set_keys(Keys::NONE);
            return;
        }
        let default = if allowed(kind, spec.default) {
            spec.default
        } else {
            log::warn!("hotkey '{id}' is pressed, not held: it cannot be modifiers alone, it is left without keys");
            Keys::NONE
        };
        let keys = match saved.get(&id) {
            Some(text) => Keys::parse(text)
                .filter(|keys| allowed(kind, *keys))
                .unwrap_or_else(|| {
                    log::warn!("the keys '{text}' saved for the hotkey '{id}' are not valid: its own are kept");
                    default
                }),
            None => default,
        };
        spec.hotkey.set_keys(keys);
        self.entries.push(Entry {
            owner: owner.to_owned(),
            name: spec.name,
            label: spec.label,
            default,
            hotkey: spec.hotkey,
        });
    }

    /// Binds the hotkey `index` to `keys`, noted in `saved` unless they are its own by default.
    pub fn bind(&self, index: usize, keys: Keys, saved: &mut BTreeMap<String, String>) {
        let entry = &self.entries[index];
        entry.hotkey.set_keys(keys);
        if keys == entry.default {
            saved.remove(&entry.id());
        } else {
            saved.insert(entry.id(), keys.to_string());
        }
    }

    /// The other hotkeys of running owners bound to the same keys as `index` that may act at
    /// once: those of the same owner, all of them when either is the kernel's, which act anywhere.
    pub fn conflicts(&self, index: usize, running: impl Fn(&str) -> bool) -> Vec<usize> {
        let entry = &self.entries[index];
        let keys = entry.hotkey.keys();
        if keys.is_none() {
            return Vec::new();
        }
        (0..self.entries.len())
            .filter(|&other| other != index)
            .filter(|&other| {
                let them = &self.entries[other];
                running(&them.owner)
                    && them.hotkey.keys() == keys
                    && (them.owner == entry.owner || them.owner == KERNEL || entry.owner == KERNEL)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, kind: HotkeyKind, default: Keys) -> HotkeySpec {
        HotkeySpec {
            name: name.to_owned(),
            label: name.to_owned(),
            default,
            hotkey: Hotkey::new(kind, default),
        }
    }

    #[test]
    fn the_keys_saved_bind_a_hotkey_and_only_those_changed_are_saved() {
        let saved = BTreeMap::from([
            ("kernel/undo".to_owned(), "Ctrl+U".to_owned()),
            ("view/forward".to_owned(), "W".to_owned()),
            ("view/back".to_owned(), "Ctrl+Nothing".to_owned()),
        ]);
        let mut hotkeys = Hotkeys::new(&saved);
        assert_eq!(hotkeys.undo.keys(), Keys::ctrl(egui::Key::U));
        assert_eq!(hotkeys.redo.keys(), Keys::ctrl(egui::Key::Y));
        let forward = spec("forward", HotkeyKind::Hold, Keys::key(egui::Key::Z));
        let back = spec("back", HotkeyKind::Hold, Keys::key(egui::Key::S));
        let (forward_key, back_key) = (forward.hotkey.clone(), back.hotkey.clone());
        hotkeys.declare("view", forward, &saved);
        hotkeys.declare("view", back, &saved);
        assert_eq!(forward_key.keys(), Keys::key(egui::Key::W));
        assert_eq!(back_key.keys(), Keys::key(egui::Key::S), "keys not valid: its own");

        let mut saved = saved;
        hotkeys.bind(2, Keys::key(egui::Key::Z), &mut saved);
        assert_eq!(forward_key.keys(), Keys::key(egui::Key::Z));
        assert!(!saved.contains_key("view/forward"), "back to its own: not saved");
        hotkeys.bind(3, Keys::SHIFT, &mut saved);
        assert_eq!(saved["view/back"], "Shift");
    }

    #[test]
    fn a_hotkey_declared_twice_or_pressed_as_modifiers_alone_has_no_keys() {
        let mut hotkeys = Hotkeys::new(&BTreeMap::new());
        let first = spec("act", HotkeyKind::Press, Keys::key(egui::Key::A));
        let second = spec("act", HotkeyKind::Press, Keys::key(egui::Key::B));
        let alone = spec("alone", HotkeyKind::Press, Keys::SHIFT);
        let saved_alone = spec("saved", HotkeyKind::Press, Keys::key(egui::Key::C));
        let (second_key, alone_key, saved_key) =
            (second.hotkey.clone(), alone.hotkey.clone(), saved_alone.hotkey.clone());
        let saved = BTreeMap::from([("m/saved".to_owned(), "Alt".to_owned())]);
        for spec in [first, second, alone, saved_alone] {
            hotkeys.declare("m", spec, &saved);
        }
        assert_eq!(second_key.keys(), Keys::NONE);
        assert_eq!(alone_key.keys(), Keys::NONE);
        assert_eq!(
            saved_key.keys(),
            Keys::key(egui::Key::C),
            "modifiers alone saved: its own kept"
        );
        assert_eq!(hotkeys.entries.len(), 5, "the second 'act' is not listed");
        assert!(allowed(HotkeyKind::Hold, Keys::SHIFT));
    }

    #[test]
    fn same_keys_conflict_within_a_module_or_with_the_kernel_while_running() {
        let mut hotkeys = Hotkeys::new(&BTreeMap::new());
        let none = BTreeMap::new();
        hotkeys.declare("a", spec("one", HotkeyKind::Press, Keys::key(egui::Key::F)), &none);
        hotkeys.declare("a", spec("two", HotkeyKind::Press, Keys::key(egui::Key::F)), &none);
        hotkeys.declare("b", spec("three", HotkeyKind::Press, Keys::key(egui::Key::F)), &none);
        hotkeys.declare("b", spec("four", HotkeyKind::Press, Keys::ctrl(egui::Key::Z)), &none);
        let all = |_: &str| true;
        assert_eq!(hotkeys.conflicts(2, all), vec![3], "same module");
        assert_eq!(hotkeys.conflicts(4, all), Vec::<usize>::new(), "another module's panel");
        assert_eq!(hotkeys.conflicts(5, all), vec![0], "the kernel's Undo acts anywhere");
        assert_eq!(hotkeys.conflicts(0, all), vec![5]);
        assert_eq!(
            hotkeys.conflicts(0, |owner: &str| owner != "b"),
            Vec::<usize>::new(),
            "b stopped"
        );
    }
}
