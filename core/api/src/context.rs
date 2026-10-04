use std::any::Any;

use crate::ui::SharedUi;
use crate::{
    CallId, Command, Editor, JobContext, JobFn, JobId, PropertyInfo, PropertyValue, ServiceKey, egui, egui_wgpu,
};

/// What the kernel offers to modules. Implemented by the kernel only.
pub trait Host {
    fn publish(&mut self, source: &str, topic: &str, payload: serde_json::Value);
    fn execute(&mut self, owner: &str, command: Box<dyn Command>);
    fn forget_document(&mut self, owner: &str, document: &str);
    fn service(&self, id: &str) -> Option<&(dyn Any + Send + Sync)>;
    /// The module providing the service `id`.
    fn service_provider(&self, id: &str) -> Option<String>;
    fn gpu(&self) -> Option<&egui_wgpu::RenderState>;
    fn draw_panel(&mut self, owner: &str, objects: &SharedUi, panel: &str, ui: &mut egui::Ui);
    fn draw_dialogs(&mut self, owner: &str, objects: &SharedUi, egui: &egui::Context);
    fn setting(&self, module: &str, key: &str) -> Option<serde_json::Value>;
    fn set_setting(&mut self, module: &str, key: &str, value: serde_json::Value);
    fn report_failure(&mut self, reporter: &str, culprit: &str, message: &str);
    fn spawn(&mut self, owner: &str, label: &str, job: JobFn) -> JobId;
    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId;
    fn cancel(&mut self, owner: &str, job: JobId);
    fn call(&mut self, caller: &str, name: &str, arguments: serde_json::Value) -> CallId;
    fn editor(&self, caller: &str) -> Editor;
}

/// Access to the kernel for one module, passed to every `Module` method after `register`.
pub struct Context<'a> {
    host: &'a mut dyn Host,
    module: &'a str,
    /// Who called the command `Module::on_command` runs.
    caller: Option<&'a str>,
}

impl<'a> Context<'a> {
    pub fn new(host: &'a mut dyn Host, module: &'a str) -> Self {
        Self {
            host,
            module,
            caller: None,
        }
    }

    /// The context of `Module::on_command`, for a command `caller` called.
    pub fn for_command(host: &'a mut dyn Host, module: &'a str, caller: &'a str) -> Self {
        Self {
            host,
            module,
            caller: Some(caller),
        }
    }

    /// In `Module::on_command`, who called the command: a module id, `kernel`, or
    /// `<module>#<name>` for a script run of that module.
    pub fn command_caller(&self) -> Option<&str> {
        self.caller
    }

    /// Id of the module this context belongs to.
    pub fn module_id(&self) -> &str {
        self.module
    }

    pub fn publish(&mut self, topic: &str, payload: serde_json::Value) {
        self.host.publish(self.module, topic, payload);
    }

    /// Publishes `payload` serialised to JSON. Subscribers read it back with `Event::decode` into a
    /// type of their own: modules share no Rust type, only the shape of the JSON.
    pub fn publish_as<T: serde::Serialize>(&mut self, topic: &str, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(value) => self.publish(topic, value),
            Err(error) => log::error!("'{topic}' not published: {error}"),
        }
    }

    /// Queues an undoable command on this module.
    pub fn execute(&mut self, command: impl Command + 'static) {
        self.host.execute(self.module, Box::new(command));
    }

    /// Forgets from the undo history, done or undone, the changes of this module whose
    /// `Command::document` is `document`: for a document closed without saving, which they would
    /// otherwise bring back.
    pub fn forget_document(&mut self, document: &str) {
        self.host.forget_document(self.module, document);
    }

    /// Returns a clone of the service, if a running module provides it.
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

    /// Draws the panel `panel` of interface objects (section 3) in `ui`, as the kernel draws those
    /// of compiled modules, and turns what the user does into their signals.
    pub fn draw_objects(&mut self, objects: &SharedUi, panel: &str, ui: &mut egui::Ui) {
        self.host.draw_panel(self.module, objects, panel, ui);
    }

    /// Draws the dialogs shown among interface objects, each in a modal window over the editor.
    pub fn draw_dialogs(&mut self, objects: &SharedUi, egui: &egui::Context) {
        self.host.draw_dialogs(self.module, objects, egui);
    }

    /// Reads a setting of this module, kept between sessions.
    pub fn setting(&self, key: &str) -> Option<serde_json::Value> {
        self.host.setting(self.module, key)
    }

    pub fn set_setting(&mut self, key: &str, value: serde_json::Value) {
        self.host.set_setting(self.module, key, value);
    }

    /// The module providing a service, to report it when its code fails (F5).
    pub fn service_provider<T>(&self, key: ServiceKey<T>) -> Option<String> {
        self.host.service_provider(key.id())
    }

    /// Reports that the module `culprit` misbehaved in code this module runs on its behalf, such
    /// as a viewport layer. The kernel disables it as if it had panicked, naming this module.
    pub fn report_failure(&mut self, culprit: &str, message: &str) {
        self.host.report_failure(self.module, culprit, message);
    }

    /// Runs `job` on a worker thread (rule T2). Its value, cancellation or panic comes back to
    /// this module on the interface thread through `Module::on_job`.
    pub fn spawn<T: Any + Send>(&mut self, label: &str, job: impl FnOnce(&JobContext) -> T + Send + 'static) -> JobId {
        self.host
            .spawn(self.module, label, Box::new(move |context| Box::new(job(context))))
    }

    /// Like `spawn`, on a thread of its own instead of the pool: for work that waits, such as a
    /// script, so that the pool's threads stay free for computations (rule T2).
    pub fn spawn_thread<T: Any + Send>(
        &mut self,
        label: &str,
        job: impl FnOnce(&JobContext) -> T + Send + 'static,
    ) -> JobId {
        self.host
            .spawn_thread(self.module, label, Box::new(move |context| Box::new(job(context))))
    }

    /// Asks one of this module's jobs to stop (as the Jobs panel's Cancel button does).
    pub fn cancel(&mut self, job: JobId) {
        self.host.cancel(self.module, job);
    }

    /// Calls a named command from the interface thread. The answer comes back at the end of the
    /// frame through `Module::on_reply`, with the returned id.
    pub fn call(&mut self, name: &str, arguments: serde_json::Value) -> CallId {
        self.host.call(self.module, name, arguments)
    }

    /// A handle to the editor for other threads, acting for this module.
    pub fn editor(&self) -> Editor {
        self.host.editor(self.module)
    }

    /// Every animatable property of running modules.
    pub fn properties(&self) -> Vec<PropertyInfo> {
        self.editor().properties()
    }

    pub fn read_property(&self, path: &str) -> Result<PropertyValue, String> {
        self.editor().read_property(path)
    }

    /// Writes an animatable property, without the history.
    pub fn write_property(&self, path: &str, value: PropertyValue) -> Result<(), String> {
        self.editor().write_property(path, value)
    }
}
