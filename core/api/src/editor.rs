use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::{CommandInfo, Event};

/// What the kernel offers behind an `Editor` handle. Implemented by the kernel only. Every request
/// of a caller whose feature no longer runs is refused with an error.
pub trait EditorBackend: Send + Sync {
    fn commands(&self) -> Vec<CommandInfo>;
    /// Calls a command and waits for its answer. Must not be called from the interface thread
    /// for a command running there.
    fn call(&self, caller: &str, name: &str, arguments: Value) -> Result<Value, String>;
    fn publish(&self, source: &str, topic: &str, payload: Value) -> Result<(), String>;
    fn subscribe(&self, caller: &str, topic: &str) -> Result<u64, String>;
    /// The next event, `None` when `timeout` passed first, or an error when the subscription does
    /// not exist or was closed.
    fn next_event(&self, caller: &str, subscription: u64, timeout: Duration) -> Result<Option<Event>, String>;
    fn unsubscribe(&self, subscription: u64);
    fn setting(&self, caller: &str, key: &str) -> Result<Option<Value>, String>;
    fn set_setting(&self, caller: &str, key: &str, value: Value) -> Result<(), String>;
    fn begin_group(&self, caller: &str, label: &str) -> Result<(), String>;
    fn end_group(&self, caller: &str) -> Result<(), String>;
}

/// The generic interface of the editor (rule S1), usable from any thread: jobs, scripts and,
/// through the C interface, native modules. Every value crosses it as JSON.
#[derive(Clone)]
pub struct Editor {
    backend: Arc<dyn EditorBackend>,
    /// Who acts: a feature id, or `feature#name` for a script run or a module of that feature.
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

    /// The feature this handle belongs to: the caller without its `#name` part.
    pub fn feature(&self) -> &str {
        self.caller.split('#').next().unwrap_or(&self.caller)
    }

    /// A handle acting as `name` inside the same feature, e.g. one script run: its calls are
    /// told apart from the others, in undo groups in particular.
    pub fn derive(&self, name: &str) -> Editor {
        Editor::new(self.backend.clone(), &format!("{}#{name}", self.feature()))
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
    pub fn publish(&self, topic: &str, payload: Value) -> Result<(), String> {
        self.backend.publish(&self.caller, topic, payload)
    }

    pub fn publish_as<T: serde::Serialize>(&self, topic: &str, payload: &T) -> Result<(), String> {
        let value = serde_json::to_value(payload).map_err(|error| format!("'{topic}' not published: {error}"))?;
        self.publish(topic, value)
    }

    /// Receives the events of `topic` (`*` for all) from now on; read them with `next_event`.
    pub fn subscribe(&self, topic: &str) -> Result<u64, String> {
        self.backend.subscribe(&self.caller, topic)
    }

    /// The next event of a subscription, waiting at most `timeout`: `None` when the time passed
    /// first, an error when the subscription does not exist or was closed.
    pub fn next_event(&self, subscription: u64, timeout: Duration) -> Result<Option<Event>, String> {
        self.backend.next_event(&self.caller, subscription, timeout)
    }

    pub fn unsubscribe(&self, subscription: u64) {
        self.backend.unsubscribe(subscription);
    }

    /// A setting of the feature, kept between sessions.
    pub fn setting(&self, key: &str) -> Result<Option<Value>, String> {
        self.backend.setting(&self.caller, key)
    }

    pub fn set_setting(&self, key: &str, value: Value) -> Result<(), String> {
        self.backend.set_setting(&self.caller, key, value)
    }

    /// Logs under the feature's name, with the caller's own name when it has one.
    pub fn log(&self, level: log::Level, message: &str) {
        let target = format!("uniwow_feature_{}", self.feature().replace('-', "_"));
        match self.caller.split_once('#') {
            Some((_, name)) => log::log!(target: target.as_str(), level, "[{name}] {message}"),
            None => log::log!(target: target.as_str(), level, "{message}"),
        }
    }

    /// From here to `end_group`, the commands this caller's calls apply form one undo entry.
    pub fn begin_group(&self, label: &str) -> Result<(), String> {
        self.backend.begin_group(&self.caller, label)
    }

    pub fn end_group(&self) -> Result<(), String> {
        self.backend.end_group(&self.caller)
    }
}
