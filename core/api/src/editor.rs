use std::sync::Arc;

use serde_json::Value;

use crate::CommandInfo;

/// What the kernel offers behind an `Editor` handle. Implemented by the kernel only.
pub trait EditorBackend: Send + Sync {
    fn commands(&self) -> Vec<CommandInfo>;
    /// Calls a command and waits for its answer. Must not be called from the interface thread.
    fn call(&self, caller: &str, name: &str, arguments: Value) -> Result<Value, String>;
    fn publish(&self, source: &str, topic: &str, payload: Value);
}

/// The generic interface of the editor (rule S1), usable from any thread: jobs, and later scripts
/// and native modules. Every value crosses it as JSON.
#[derive(Clone)]
pub struct Editor {
    backend: Arc<dyn EditorBackend>,
    /// Id of the feature this handle acts for.
    caller: String,
}

impl Editor {
    pub fn new(backend: Arc<dyn EditorBackend>, caller: &str) -> Self {
        Self {
            backend,
            caller: caller.to_owned(),
        }
    }

    pub fn caller(&self) -> &str {
        &self.caller
    }

    /// Every command of running features, with its description and schemas.
    pub fn commands(&self) -> Vec<CommandInfo> {
        self.backend.commands()
    }

    /// Calls a command by name and waits for its answer: at once for a command running on the
    /// calling thread, within the next frame for one running on the interface thread.
    /// From the interface thread, use `Context::call` instead.
    pub fn call(&self, name: &str, arguments: Value) -> Result<Value, String> {
        self.backend.call(&self.caller, name, arguments)
    }

    /// Publishes an event, delivered on the interface thread.
    pub fn publish(&self, topic: &str, payload: Value) {
        self.backend.publish(&self.caller, topic, payload);
    }

    pub fn publish_as<T: serde::Serialize>(&self, topic: &str, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(value) => self.publish(topic, value),
            Err(error) => log::error!("'{topic}' not published: {error}"),
        }
    }
}
