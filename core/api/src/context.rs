use std::any::Any;

use crate::{CallId, Command, Editor, JobContext, JobFn, JobId, ServiceKey, egui_wgpu};

/// What the kernel offers to features. Implemented by the kernel only.
pub trait Host {
    fn publish(&mut self, source: &str, topic: &str, payload: serde_json::Value);
    fn execute(&mut self, owner: &str, command: Box<dyn Command>);
    fn service(&self, id: &str) -> Option<&(dyn Any + Send + Sync)>;
    fn gpu(&self) -> Option<&egui_wgpu::RenderState>;
    fn setting(&self, feature: &str, key: &str) -> Option<serde_json::Value>;
    fn set_setting(&mut self, feature: &str, key: &str, value: serde_json::Value);
    fn report_failure(&mut self, reporter: &str, culprit: &str, message: &str);
    fn spawn(&mut self, owner: &str, label: &str, job: JobFn) -> JobId;
    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId;
    fn cancel(&mut self, job: JobId);
    fn call(&mut self, caller: &str, name: &str, arguments: serde_json::Value) -> CallId;
    fn editor(&self, caller: &str) -> Editor;
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

    /// Publishes `payload` serialised to JSON. Subscribers read it back with `Event::decode` into a
    /// type of their own: features share no Rust type, only the shape of the JSON.
    pub fn publish_as<T: serde::Serialize>(&mut self, topic: &str, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(value) => self.publish(topic, value),
            Err(error) => log::error!("'{topic}' not published: {error}"),
        }
    }

    /// Queues an undoable command on this feature.
    pub fn execute(&mut self, command: impl Command + 'static) {
        self.host.execute(self.feature, Box::new(command));
    }

    /// Returns a clone of the service, if a running feature provides it.
    pub fn service<T: Any + Clone + Send + Sync>(&self, key: ServiceKey<T>) -> Option<T> {
        let service = self.host.service(key.id())?;
        let typed = service.downcast_ref::<T>().cloned();
        if typed.is_none() {
            log::error!(
                "the service '{}' is not provided as {}: provider and consumer use different keys",
                key.id(),
                std::any::type_name::<T>()
            );
        }
        typed
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

    /// Runs `job` on a worker thread (rule T2). Its value, cancellation or panic comes back to
    /// this feature on the interface thread through `Feature::on_job`.
    pub fn spawn<T: Any + Send>(&mut self, label: &str, job: impl FnOnce(&JobContext) -> T + Send + 'static) -> JobId {
        self.host
            .spawn(self.feature, label, Box::new(move |context| Box::new(job(context))))
    }

    /// Like `spawn`, on a thread of its own instead of the pool: for work that waits, such as a
    /// script, so that the pool's threads stay free for computations (rule T2).
    pub fn spawn_thread<T: Any + Send>(
        &mut self,
        label: &str,
        job: impl FnOnce(&JobContext) -> T + Send + 'static,
    ) -> JobId {
        self.host
            .spawn_thread(self.feature, label, Box::new(move |context| Box::new(job(context))))
    }

    /// Asks one of this feature's jobs to stop (as the Jobs panel's Cancel button does).
    pub fn cancel(&mut self, job: JobId) {
        self.host.cancel(job);
    }

    /// Calls a named command from the interface thread. The answer comes back at the end of the
    /// frame through `Feature::on_reply`, with the returned id.
    pub fn call(&mut self, name: &str, arguments: serde_json::Value) -> CallId {
        self.host.call(self.feature, name, arguments)
    }

    /// A handle to the editor for other threads, acting for this feature.
    pub fn editor(&self) -> Editor {
        self.host.editor(self.feature)
    }
}
