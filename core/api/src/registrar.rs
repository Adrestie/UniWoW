use std::any::Any;

use crate::ServiceKey;

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
    /// Identifier, unique within the feature.
    pub id: String,
    pub title: String,
    pub area: DockArea,
    /// Shown at first start, and again after the feature comes back, unless the user closed it.
    pub open_by_default: bool,
}

#[derive(Clone, Debug)]
pub struct MenuItemSpec {
    /// Top-level menu, e.g. "View". Created if it does not exist.
    pub menu: String,
    pub label: String,
    /// Passed back to `Feature::on_menu`.
    pub action: String,
}

/// Collects what a feature contributes during `Feature::register`.
#[derive(Default)]
pub struct Registrar {
    pub panels: Vec<PanelSpec>,
    pub menu_items: Vec<MenuItemSpec>,
    /// Service id and implementation. Consumers read it back with `Context::service`.
    pub services: Vec<(String, Box<dyn Any>)>,
    /// Event topics; `*` receives every event.
    pub subscriptions: Vec<String>,
}

impl Registrar {
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
    pub fn provide<T: Any>(&mut self, key: ServiceKey<T>, service: T) -> &mut Self {
        self.services.push((key.id().to_owned(), Box::new(service)));
        self
    }

    pub fn subscribe(&mut self, topic: &str) -> &mut Self {
        self.subscriptions.push(topic.to_owned());
        self
    }
}
