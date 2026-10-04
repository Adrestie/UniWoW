//! A host for the tests of the Timeline: it keeps what the module asks of the kernel.

use std::any::Any;

use uniwow_api::ui::SharedUi;
use uniwow_api::{CallId, Command, Context, Editor, Host, JobFn, JobId, egui, egui_wgpu, serde_json};

#[derive(Default)]
pub struct FakeHost {
    /// The commands the module queued, in order.
    pub executed: Vec<Box<dyn Command>>,
    /// The documents it closed without saving.
    pub forgotten: Vec<String>,
}

impl FakeHost {
    pub fn context(&mut self) -> Context<'_> {
        Context::new(self, "timeline")
    }
}

impl Host for FakeHost {
    fn publish(&mut self, _source: &str, _topic: &str, _payload: serde_json::Value) {}

    fn execute(&mut self, _owner: &str, command: Box<dyn Command>) {
        self.executed.push(command);
    }

    fn forget_document(&mut self, _owner: &str, document: &str) {
        self.forgotten.push(document.to_owned());
    }

    fn service(&self, _id: &str) -> Option<&(dyn Any + Send + Sync)> {
        None
    }

    fn service_provider(&self, _id: &str) -> Option<String> {
        None
    }

    fn gpu(&self) -> Option<&egui_wgpu::RenderState> {
        None
    }

    fn draw_panel(&mut self, _owner: &str, _objects: &SharedUi, _panel: &str, _ui: &mut egui::Ui) {}

    fn draw_dialogs(&mut self, _owner: &str, _objects: &SharedUi, _egui: &egui::Context) {}

    fn adopt_objects(&mut self, _owner: &str, _objects: &SharedUi) {}

    fn setting(&self, _module: &str, _key: &str) -> Option<serde_json::Value> {
        None
    }

    fn set_setting(&mut self, _module: &str, _key: &str, _value: serde_json::Value) {}

    fn report_failure(&mut self, _reporter: &str, _culprit: &str, _message: &str) {}

    fn spawn(&mut self, _owner: &str, _label: &str, _job: JobFn) -> JobId {
        unimplemented!("the Timeline starts no job")
    }

    fn spawn_thread(&mut self, _owner: &str, _label: &str, _job: JobFn) -> JobId {
        unimplemented!("the Timeline starts no job")
    }

    fn cancel(&mut self, _owner: &str, _job: JobId) {}

    fn call(&mut self, _caller: &str, _name: &str, _arguments: serde_json::Value) -> CallId {
        CallId(1)
    }

    fn editor(&self, _caller: &str) -> Editor {
        unimplemented!("the tests give the Timeline no editor")
    }
}
