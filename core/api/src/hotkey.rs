//! Hotkeys: the keys a module acts on, declared with the keys they have by default
//! (`Registrar::hotkey`), which the user binds to others in *Edit > Hotkey*. The kernel keeps the
//! keys the user chose in its settings and sets them on the handles the modules read.

use std::fmt;
use std::sync::{Arc, RwLock};

use crate::egui;

/// Where the memory of egui notes that the user chooses new keys for a hotkey: no hotkey acts
/// meanwhile.
fn suspended_id() -> egui::Id {
    egui::Id::new("uniwow hotkeys suspended")
}

/// Suspends every hotkey of the window of `ctx`, or lets them act again; the kernel calls it.
pub fn suspend(ctx: &egui::Context, suspended: bool) {
    ctx.data_mut(|data| data.insert_temp(suspended_id(), suspended));
}

/// How a hotkey acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyKind {
    /// Once per press, such as Undo.
    Press,
    /// For as long as it is held, such as flying; it may be modifiers alone.
    Hold,
}

/// A key and the modifiers held with it, or modifiers alone, or nothing (`Keys::NONE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Keys {
    /// Ctrl, or Cmd on a Mac.
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: Option<egui::Key>,
}

impl Keys {
    pub const NONE: Self = Self {
        ctrl: false,
        shift: false,
        alt: false,
        key: None,
    };

    /// A key alone.
    pub const fn key(key: egui::Key) -> Self {
        Self {
            key: Some(key),
            ..Self::NONE
        }
    }

    /// A key with Ctrl, or Cmd on a Mac.
    pub const fn ctrl(key: egui::Key) -> Self {
        Self {
            ctrl: true,
            key: Some(key),
            ..Self::NONE
        }
    }

    pub const SHIFT: Self = Self {
        shift: true,
        ..Self::NONE
    };

    pub const ALT: Self = Self {
        alt: true,
        ..Self::NONE
    };

    pub fn is_none(&self) -> bool {
        *self == Self::NONE
    }

    /// Whether it is modifiers without a key.
    pub fn modifiers_alone(&self) -> bool {
        self.key.is_none() && !self.is_none()
    }

    /// The modifiers of a key pressed, as `Keys` without the key.
    pub fn of_modifiers(modifiers: egui::Modifiers) -> Self {
        Self {
            ctrl: modifiers.command || modifiers.ctrl,
            shift: modifiers.shift,
            alt: modifiers.alt,
            key: None,
        }
    }

    fn modifiers(&self) -> egui::Modifiers {
        let mut modifiers = egui::Modifiers::NONE;
        if self.ctrl {
            modifiers |= egui::Modifiers::COMMAND;
        }
        if self.shift {
            modifiers |= egui::Modifiers::SHIFT;
        }
        if self.alt {
            modifiers |= egui::Modifiers::ALT;
        }
        modifiers
    }

    /// Reads keys as `Display` writes them, such as `Ctrl+Shift+Z` or `Shift`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut keys = Self::NONE;
        for part in text.split('+') {
            if keys.key.is_some() {
                return None;
            }
            match part {
                "Ctrl" => keys.ctrl = true,
                "Shift" => keys.shift = true,
                "Alt" => keys.alt = true,
                name => keys.key = Some(egui::Key::from_name(name)?),
            }
        }
        (!keys.is_none()).then_some(keys)
    }
}

impl fmt::Display for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = [
            (self.ctrl, "Ctrl"),
            (self.shift, "Shift"),
            (self.alt, "Alt"),
            (self.key.is_some(), self.key.map_or("", egui::Key::name)),
        ];
        let parts: Vec<&str> = names.iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect();
        f.write_str(&parts.join("+"))
    }
}

/// A hotkey as its module reads it: the keys bound to it now. It never acts while a text field has
/// the keyboard, nor while the user chooses new keys for a hotkey.
#[derive(Clone, Debug)]
pub struct Hotkey {
    kind: HotkeyKind,
    keys: Arc<RwLock<Keys>>,
}

impl Hotkey {
    /// A hotkey bound to `keys`; a module declares its own with `Registrar::hotkey`, so that the
    /// user can bind them to other keys.
    pub fn new(kind: HotkeyKind, keys: Keys) -> Self {
        Self {
            kind,
            keys: Arc::new(RwLock::new(keys)),
        }
    }

    pub fn kind(&self) -> HotkeyKind {
        self.kind
    }

    pub fn keys(&self) -> Keys {
        *self.keys.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Binds it to `keys`; the kernel calls it.
    pub fn set_keys(&self, keys: Keys) {
        *self.keys.write().unwrap_or_else(|e| e.into_inner()) = keys;
    }

    fn active(&self, ctx: &egui::Context) -> Option<Keys> {
        let keys = self.keys();
        let suspended = ctx.data(|data| data.get_temp(suspended_id())).unwrap_or(false);
        let blocked = keys.is_none() || suspended || ctx.egui_wants_keyboard_input();
        (!blocked).then_some(keys)
    }

    /// Whether its keys were pressed this frame, exactly those: the presses are then taken, so
    /// that nothing else acts on them.
    pub fn pressed(&self, ctx: &egui::Context) -> bool {
        let Some(keys) = self.active(ctx) else {
            return false;
        };
        let (Some(key), modifiers) = (keys.key, keys.modifiers()) else {
            return false;
        };
        ctx.input_mut(|input| {
            let before = input.events.len();
            input.events.retain(|event| {
                !matches!(event, egui::Event::Key { key: pressed, pressed: true, modifiers: held, .. }
                    if *pressed == key && held.matches_exact(modifiers))
            });
            input.events.len() < before
        })
    }

    /// Whether its keys are held now: its modifiers, with others allowed but Ctrl, and its key if
    /// it has one.
    pub fn held(&self, ctx: &egui::Context) -> bool {
        let Some(keys) = self.active(ctx) else {
            return false;
        };
        let modifiers = keys.modifiers();
        ctx.input(|input| {
            input.modifiers.matches_logically(modifiers) && keys.key.is_none_or(|key| input.key_down(key))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egui::{Key, Modifiers};

    /// A frame of `ctx` with `events`, the modifiers `held` down, giving what `act` finds.
    fn frame<R>(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        held: Modifiers,
        act: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        let mut result = None;
        let input = egui::RawInput {
            events: [vec![egui::Event::ModifiersChanged(held)], events].concat(),
            ..Default::default()
        };
        let mut act = Some(act);
        let mut output = ctx.run_ui(input, |ui| {
            if let Some(act) = act.take() {
                result = Some(act(ui));
            }
        });
        output.textures_delta.clear();
        result.expect("the frame ran")
    }

    fn key(key: Key, pressed: bool, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    /// Ctrl as Windows gives it.
    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        command: true,
        ..Modifiers::NONE
    };

    #[test]
    fn keys_are_written_and_read_back() {
        for text in ["Ctrl+Shift+Z", "Shift", "Space", "Alt+F4", "Ctrl+Alt"] {
            assert_eq!(Keys::parse(text).map(|keys| keys.to_string()).as_deref(), Some(text));
        }
        assert_eq!(Keys::parse("Ctrl+Z"), Some(Keys::ctrl(Key::Z)));
        for wrong in ["", "Ctrl+Nothing", "Z+Ctrl", "Z+X"] {
            assert_eq!(Keys::parse(wrong), None, "{wrong}");
        }
        assert!(Keys::SHIFT.modifiers_alone() && !Keys::key(Key::A).modifiers_alone() && !Keys::NONE.modifiers_alone());
    }

    #[test]
    fn a_hotkey_is_pressed_by_its_keys_exactly_and_takes_the_press() {
        let ctx = egui::Context::default();
        let undo = Hotkey::new(HotkeyKind::Press, Keys::ctrl(Key::Z));
        let shifted = CTRL | Modifiers::SHIFT;
        assert!(
            !frame(&ctx, vec![key(Key::Z, true, shifted)], shifted, |ui| undo
                .pressed(ui.ctx())),
            "Ctrl+Shift+Z is not Ctrl+Z"
        );
        assert!(!frame(
            &ctx,
            vec![key(Key::Z, true, Modifiers::NONE)],
            Modifiers::NONE,
            |ui| undo.pressed(ui.ctx())
        ));
        let (first, second) = frame(&ctx, vec![key(Key::Z, true, CTRL)], CTRL, |ui| {
            (undo.pressed(ui.ctx()), undo.pressed(ui.ctx()))
        });
        assert!(first && !second, "the press is taken");
        undo.set_keys(Keys::ctrl(Key::U));
        assert!(frame(&ctx, vec![key(Key::U, true, CTRL)], CTRL, |ui| undo.pressed(ui.ctx())));
    }

    #[test]
    fn a_hotkey_is_held_with_other_modifiers_but_ctrl() {
        let ctx = egui::Context::default();
        let forward = Hotkey::new(HotkeyKind::Hold, Keys::key(Key::Z));
        let faster = Hotkey::new(HotkeyKind::Hold, Keys::SHIFT);
        let held = |ui: &mut egui::Ui| (forward.held(ui.ctx()), faster.held(ui.ctx()));
        assert_eq!(
            frame(&ctx, vec![key(Key::Z, true, Modifiers::NONE)], Modifiers::NONE, held),
            (true, false)
        );
        assert_eq!(
            frame(&ctx, Vec::new(), Modifiers::SHIFT, held),
            (true, true),
            "Shift as well"
        );
        assert_eq!(
            frame(&ctx, Vec::new(), CTRL, held),
            (false, false),
            "Ctrl+Z is another hotkey"
        );
        assert_eq!(
            frame(&ctx, vec![key(Key::Z, false, Modifiers::NONE)], Modifiers::NONE, held),
            (false, false)
        );
    }

    #[test]
    fn no_hotkey_acts_while_suspended_while_a_field_has_the_keyboard_nor_without_keys() {
        let ctx = egui::Context::default();
        let act = Hotkey::new(HotkeyKind::Press, Keys::key(Key::F));
        let walk = Hotkey::new(HotkeyKind::Hold, Keys::key(Key::W));
        let both = |ui: &mut egui::Ui| (act.pressed(ui.ctx()), walk.held(ui.ctx()));
        let keys = || vec![key(Key::F, true, Modifiers::NONE), key(Key::W, true, Modifiers::NONE)];
        suspend(&ctx, true);
        assert_eq!(frame(&ctx, keys(), Modifiers::NONE, both), (false, false), "suspended");
        suspend(&ctx, false);
        assert_eq!(frame(&ctx, keys(), Modifiers::NONE, both), (true, true));

        let mut text = String::new();
        let mut field = |ui: &mut egui::Ui| ui.add(egui::TextEdit::singleline(&mut text).id(egui::Id::new("field")));
        frame(&ctx, Vec::new(), Modifiers::NONE, |ui| field(ui).request_focus());
        let typed = frame(&ctx, keys(), Modifiers::NONE, |ui| {
            field(ui);
            both(ui)
        });
        assert_eq!(typed, (false, false), "a field has the keyboard");

        let none = Hotkey::new(HotkeyKind::Press, Keys::NONE);
        frame(&ctx, Vec::new(), Modifiers::NONE, |ui| {
            ui.memory_mut(|memory| memory.surrender_focus(egui::Id::new("field")))
        });
        assert!(!frame(&ctx, keys(), Modifiers::NONE, |ui| none.pressed(ui.ctx())
            || none.held(ui.ctx())));
    }
}
