use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use uniwow_api::{CallId, Command, Editor, Event, Host, JobFn, JobId, egui_wgpu, serde_json};

use crate::jobs::Pool;
use crate::router::{Bridge, ReplyTo, Request};
use crate::settings::Settings;

pub struct Service {
    pub provider: String,
    pub value: Box<dyn Any + Send + Sync>,
}

/// A failure of `culprit` noticed by `reporter`, handled like a panic of `culprit`.
pub struct Reported {
    pub reporter: String,
    pub culprit: String,
    pub message: String,
}

/// Kernel state reachable from modules through `Context`.
pub struct KernelHost {
    /// Published this frame, delivered at the end of it.
    pub events: Vec<Event>,
    /// Queued commands with the id of their module.
    pub pending: Vec<(String, Box<dyn Command>)>,
    pub services: HashMap<String, Service>,
    pub gpu: Option<egui_wgpu::RenderState>,
    pub settings: Settings,
    pub settings_changed: bool,
    pub reported: Vec<Reported>,
    pub pool: Pool,
    pub bridge: Arc<Bridge>,
    next_call: u64,
}

impl KernelHost {
    /// The settings of the modules move to the bridge, where every thread reads them.
    pub fn new(gpu: Option<egui_wgpu::RenderState>, mut settings: Settings, pool: Pool, bridge: Arc<Bridge>) -> Self {
        *bridge.settings.write().unwrap_or_else(|e| e.into_inner()) = std::mem::take(&mut settings.modules);
        Self {
            events: Vec::new(),
            pending: Vec::new(),
            services: HashMap::new(),
            gpu,
            settings,
            settings_changed: false,
            reported: Vec::new(),
            pool,
            bridge,
            next_call: 0,
        }
    }

    /// Writes the settings, the modules' ones back from the bridge.
    pub fn save_settings(&mut self) {
        self.settings.modules = self.bridge.settings.read().unwrap_or_else(|e| e.into_inner()).clone();
        self.settings.save();
    }
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

    fn service(&self, id: &str) -> Option<&(dyn Any + Send + Sync)> {
        self.services.get(id).map(|s| s.value.as_ref())
    }

    fn gpu(&self) -> Option<&egui_wgpu::RenderState> {
        self.gpu.as_ref()
    }

    fn setting(&self, module: &str, key: &str) -> Option<serde_json::Value> {
        let settings = self.bridge.settings.read().unwrap_or_else(|e| e.into_inner());
        settings.get(module)?.get(key).cloned()
    }

    fn set_setting(&mut self, module: &str, key: &str, value: serde_json::Value) {
        self.bridge
            .settings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(module.to_owned())
            .or_default()
            .insert(key.to_owned(), value);
        self.settings_changed = true;
    }

    fn report_failure(&mut self, reporter: &str, culprit: &str, message: &str) {
        self.reported.push(Reported {
            reporter: reporter.to_owned(),
            culprit: culprit.to_owned(),
            message: message.to_owned(),
        });
    }

    fn spawn(&mut self, owner: &str, label: &str, job: JobFn) -> JobId {
        let editor = self.editor(owner);
        self.pool.spawn(owner, label, job, editor)
    }

    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId {
        let editor = self.editor(owner);
        self.pool.spawn_thread(owner, label, job, editor)
    }

    fn call(&mut self, caller: &str, name: &str, arguments: serde_json::Value) -> CallId {
        self.next_call += 1;
        let id = CallId(self.next_call);
        self.bridge.queue(Request::Call {
            caller: caller.to_owned(),
            thread: std::thread::current().id(),
            name: name.to_owned(),
            arguments,
            reply: ReplyTo::Module(caller.to_owned(), id),
        });
        id
    }

    fn cancel(&mut self, owner: &str, job: JobId) {
        self.pool.cancel(owner, job);
    }

    fn editor(&self, caller: &str) -> Editor {
        Editor::new(self.bridge.clone(), caller)
    }
}
