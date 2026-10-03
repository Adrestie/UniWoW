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

/// Kernel state reachable from features through `Context`.
pub struct KernelHost {
    /// Published this frame, delivered at the end of it.
    pub events: Vec<Event>,
    /// Queued commands with the id of their feature.
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
    pub fn new(gpu: Option<egui_wgpu::RenderState>, settings: Settings, pool: Pool, bridge: Arc<Bridge>) -> Self {
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
            reply: ReplyTo::Feature(caller.to_owned(), id),
        });
        id
    }

    fn cancel(&mut self, job: JobId) {
        self.pool.cancel(job);
    }

    fn editor(&self, caller: &str) -> Editor {
        Editor::new(self.bridge.clone(), caller)
    }
}
