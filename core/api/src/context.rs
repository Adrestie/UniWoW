use std::any::Any;

use crate::{Command, egui_wgpu};

/// What the kernel offers to features. Implemented by the kernel only.
pub trait Host {
    fn publish(&mut self, source: &str, topic: &str, payload: serde_json::Value);
    fn execute(&mut self, owner: &str, command: Box<dyn Command>);
    fn service(&self, id: &str) -> Option<&dyn Any>;
    fn gpu(&self) -> Option<&egui_wgpu::RenderState>;
    fn setting(&self, feature: &str, key: &str) -> Option<serde_json::Value>;
    fn set_setting(&mut self, feature: &str, key: &str, value: serde_json::Value);
    fn report_failure(&mut self, reporter: &str, culprit: &str, message: &str);
}

/// Access to the kernel for one feature, passed to every `Feature` method after `register`.
pub struct Context<'a> {
    host: &'a mut dyn Host,
    feature: &'a str,
}

impl<'a> Context<'a> {
    pub fn new(host: &'a mut dyn Host, feature: &'a str) -> Self {
        Self { host, feature }
    }

    /// Id of the feature this context belongs to.
    pub fn feature_id(&self) -> &str {
        self.feature
    }

    pub fn publish(&mut self, topic: &str, payload: serde_json::Value) {
        self.host.publish(self.feature, topic, payload);
    }

    /// Queues an undoable command on this feature.
    pub fn execute(&mut self, command: impl Command + 'static) {
        self.host.execute(self.feature, Box::new(command));
    }

    /// Returns a clone of the service registered under `id`, if it is present and of type `T`.
    pub fn service<T: Any + Clone>(&self, id: &str) -> Option<T> {
        self.host.service(id)?.downcast_ref::<T>().cloned()
    }

    /// The GPU device used by the editor window.
    pub fn gpu(&self) -> Option<&egui_wgpu::RenderState> {
        self.host.gpu()
    }

    /// Reads a setting of this feature, kept between sessions.
    pub fn setting(&self, key: &str) -> Option<serde_json::Value> {
        self.host.setting(self.feature, key)
    }

    pub fn set_setting(&mut self, key: &str, value: serde_json::Value) {
        self.host.set_setting(self.feature, key, value);
    }

    /// Reports that the feature `culprit` misbehaved in code this feature runs on its behalf, such
    /// as a viewport layer. The kernel disables it as if it had panicked, naming this feature.
    pub fn report_failure(&mut self, culprit: &str, message: &str) {
        self.host.report_failure(self.feature, culprit, message);
    }
}
