use std::any::Any;
use std::sync::Arc;

use crate::hotkey::{Hotkey, HotkeyKind, Keys};
use crate::{CommandHandler, CommandSpec, PropertyKind, PropertySpec, PropertyValue, RunsOn, ServiceKey};

/// Where a panel goes the first time it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockArea {
    Center,
    Left,
    Right,
    Bottom,
}

#[derive(Clone, Debug)]
pub struct PanelSpec {
    /// Identifier, unique within the module.
    pub id: String,
    pub title: String,
    pub area: DockArea,
    /// Shown at first start, and again after the module comes back, unless the user closed it.
    pub open_by_default: bool,
}

#[derive(Clone, Debug)]
pub struct MenuItemSpec {
    /// Top-level menu, e.g. "View". Created if it does not exist.
    pub menu: String,
    pub label: String,
    /// Passed back to `Module::on_menu`.
    pub action: String,
}

/// A hotkey a module declared, `<module>/<name>`, with the keys it has by default.
#[derive(Clone, Debug)]
pub struct HotkeySpec {
    pub name: String,
    pub label: String,
    pub default: Keys,
    pub hotkey: Hotkey,
}

/// A setting of a module's category of the window *Settings*: its key among the module's settings
/// (`Context::setting`), its label, and its whole values, the least and the most, and the one by
/// default. The kernel draws it and keeps the value chosen; the module reads it with `value`.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingSpec {
    pub key: String,
    pub label: String,
    pub range: [i64; 2],
    pub default: i64,
}

impl SettingSpec {
    pub fn integer(key: &str, label: &str, range: [i64; 2], default: i64) -> Self {
        Self {
            key: key.to_owned(),
            label: label.to_owned(),
            range,
            default: default.clamp(range[0], range[1]),
        }
    }

    /// The value of the module's setting `stored`, kept within the range; the default where none is
    /// stored or it is not a whole number.
    pub fn value(&self, stored: Option<&serde_json::Value>) -> i64 {
        stored
            .and_then(serde_json::Value::as_i64)
            .map_or(self.default, |value| value.clamp(self.range[0], self.range[1]))
    }
}

/// The category of the window *Settings* a module declares: its title and its settings, shown while
/// the module runs.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsCategory {
    pub title: String,
    pub settings: Vec<SettingSpec>,
}

/// Collects what a module contributes during `Module::register`.
#[derive(Default)]
pub struct Registrar {
    pub panels: Vec<PanelSpec>,
    pub menu_items: Vec<MenuItemSpec>,
    /// Service id and implementation. Consumers read it back with `Context::service`.
    pub services: Vec<(String, Box<dyn Any + Send + Sync>)>,
    pub commands: Vec<CommandSpec>,
    /// Event topics; `*` receives every event.
    pub subscriptions: Vec<String>,
    /// Animatable properties.
    pub properties: Vec<PropertySpec>,
    pub hotkeys: Vec<HotkeySpec>,
    /// Its category of the window *Settings*.
    pub settings: Option<SettingsCategory>,
}

impl Registrar {
    /// Declares the module's category of the window *Settings*, titled `title`; declared again, the
    /// last one is kept.
    pub fn settings(&mut self, title: &str, settings: Vec<SettingSpec>) -> &mut Self {
        self.settings = Some(SettingsCategory {
            title: title.to_owned(),
            settings,
        });
        self
    }

    pub fn panel(&mut self, id: &str, title: &str, area: DockArea) -> &mut Self {
        self.panels.push(PanelSpec {
            id: id.to_owned(),
            title: title.to_owned(),
            area,
            open_by_default: true,
        });
        self
    }

    pub fn menu_item(&mut self, menu: &str, label: &str, action: &str) -> &mut Self {
        self.menu_items.push(MenuItemSpec {
            menu: menu.to_owned(),
            label: label.to_owned(),
            action: action.to_owned(),
        });
        self
    }

    /// Provides a service under its key, which fixes the type consumers receive.
    pub fn provide<T: Any + Send + Sync>(&mut self, key: ServiceKey<T>, service: T) -> &mut Self {
        self.services.push((key.id().to_owned(), Box::new(service)));
        self
    }

    /// Declares a named command running on the interface thread, handled by `Module::on_command`.
    /// `arguments` and `result` are JSON Schemas.
    pub fn command(
        &mut self,
        name: &str,
        description: &str,
        arguments: serde_json::Value,
        result: serde_json::Value,
    ) -> &mut Self {
        self.add_command(name, description, arguments, result, RunsOn::Interface, false)
    }

    /// Declares a named command running on the calling thread, at once, through `handler`.
    pub fn command_on_caller(
        &mut self,
        name: &str,
        description: &str,
        arguments: serde_json::Value,
        result: serde_json::Value,
        handler: CommandHandler,
    ) -> &mut Self {
        self.add_command(name, description, arguments, result, RunsOn::Caller(handler), false)
    }

    /// Like `command_on_caller`, for a command offered on behalf of someone else, such as a
    /// module or a script: a command another module declares itself under the same name wins
    /// over it, whatever the order the modules register in.
    pub fn command_on_caller_delegated(
        &mut self,
        name: &str,
        description: &str,
        arguments: serde_json::Value,
        result: serde_json::Value,
        handler: CommandHandler,
    ) -> &mut Self {
        self.add_command(name, description, arguments, result, RunsOn::Caller(handler), true)
    }

    fn add_command(
        &mut self,
        name: &str,
        description: &str,
        arguments: serde_json::Value,
        result: serde_json::Value,
        runs_on: RunsOn,
        delegated: bool,
    ) -> &mut Self {
        self.commands.push(CommandSpec {
            name: name.to_owned(),
            description: description.to_owned(),
            arguments,
            result,
            runs_on,
            delegated,
        });
        self
    }

    /// Declares an animatable property, `<module>/<name>`: `read` gives its current value and
    /// `write` sets it, without the history, from any thread. Each number written is kept within
    /// `range`.
    pub fn animatable(
        &mut self,
        name: &str,
        label: &str,
        kind: PropertyKind,
        range: [f64; 2],
        read: impl Fn() -> PropertyValue + Send + Sync + 'static,
        write: impl Fn(PropertyValue) + Send + Sync + 'static,
    ) -> &mut Self {
        self.properties.push(PropertySpec {
            name: name.to_owned(),
            label: label.to_owned(),
            kind,
            range,
            read: Arc::new(read),
            write: Arc::new(write),
        });
        self
    }

    /// Declares a hotkey, `<module>/<name>`, bound to `default` until the user binds it to other
    /// keys in *Edit > Hotkey*: the module reads it, where it acts on it, through the handle
    /// returned. Modifiers alone are refused for a hotkey of `HotkeyKind::Press`.
    pub fn hotkey(&mut self, name: &str, label: &str, kind: HotkeyKind, default: Keys) -> Hotkey {
        let hotkey = Hotkey::new(kind, default);
        self.hotkeys.push(HotkeySpec {
            name: name.to_owned(),
            label: label.to_owned(),
            default,
            hotkey: hotkey.clone(),
        });
        hotkey
    }

    pub fn subscribe(&mut self, topic: &str) -> &mut Self {
        self.subscriptions.push(topic.to_owned());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::SettingSpec;

    #[test]
    fn a_setting_reads_the_value_stored_within_its_range_or_its_default() {
        let spec = SettingSpec::integer("distance", "Distance (tiles)", [1, 90], 45);
        assert_eq!(spec.value(None), 45, "none stored");
        assert_eq!(spec.value(Some(&serde_json::json!(8))), 8);
        assert_eq!(spec.value(Some(&serde_json::json!(200))), 90, "kept within");
        assert_eq!(spec.value(Some(&serde_json::json!(0))), 1);
        assert_eq!(spec.value(Some(&serde_json::json!(8.5))), 45, "not a whole number");
        assert_eq!(spec.value(Some(&serde_json::json!("8"))), 45);
        assert_eq!(
            SettingSpec::integer("a", "A", [1, 90], 120).default,
            90,
            "a default beyond"
        );
    }
}
