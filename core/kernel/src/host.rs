use std::any::Any;
use std::collections::HashMap;

use uniwow_api::{Command, Event, Host, egui_wgpu, serde_json};

use crate::settings::Settings;

pub struct Service {
    pub provider: String,
    pub value: Box<dyn Any>,
}

/// Kernel state reachable from features through `Context`.
#[derive(Default)]
pub struct KernelHost {
    /// Published this frame, delivered at the end of it.
    pub events: Vec<Event>,
    /// Queued commands with the id of their feature.
    pub pending: Vec<(String, Box<dyn Command>)>,
    pub services: HashMap<String, Service>,
    pub gpu: Option<egui_wgpu::RenderState>,
    pub settings: Settings,
    pub settings_changed: bool,
}

impl Host for KernelHost {
    fn publish(&mut self, source: &str, topic: &str, payload: serde_json::Value) {
        self.events.push(Event {
            topic: topic.to_owned(),
            source: source.to_owned(),
            payload,
        });
    }

    fn execute(&mut self, owner: &str, command: Box<dyn Command>) {
        self.pending.push((owner.to_owned(), command));
    }

    fn service(&self, id: &str) -> Option<&dyn Any> {
        self.services.get(id).map(|s| s.value.as_ref())
    }

    fn gpu(&self) -> Option<&egui_wgpu::RenderState> {
        self.gpu.as_ref()
    }

    fn setting(&self, feature: &str, key: &str) -> Option<serde_json::Value> {
        self.settings.features.get(feature)?.get(key).cloned()
    }

    fn set_setting(&mut self, feature: &str, key: &str, value: serde_json::Value) {
        self.settings
            .features
            .entry(feature.to_owned())
            .or_default()
            .insert(key.to_owned(), value);
        self.settings_changed = true;
    }
}
