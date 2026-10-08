use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::{AppliedChange, CommandInfo, Event, PropertyInfo, PropertyValue};

/// What the kernel offers behind an `Editor` handle. Implemented by the kernel only. Every request
/// of a caller whose module no longer runs is refused with an error.
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
    /// A setting of the space `space`: the caller's module, or a space of its own, such as a
    /// compiled module's.
    fn setting(&self, caller: &str, space: &str, key: &str) -> Result<Option<Value>, String>;
    fn set_setting(&self, caller: &str, space: &str, key: &str, value: Value) -> Result<(), String>;
    fn begin_group(&self, caller: &str, label: &str) -> Result<(), String>;
    fn end_group(&self, caller: &str) -> Result<(), String>;

    /// Records a change the caller's module already made, into the history or the caller's open
    /// undo group.
    fn record_change(&self, _caller: &str, _label: &str, _change: Box<dyn AppliedChange>) -> Result<(), String> {
        Err("changes cannot be recorded here".to_owned())
    }

    /// Makes the caller's module fail, as if it had panicked.
    fn report_failure(&self, _caller: &str, _message: &str) {}

    /// Whether the caller's module still runs.
    fn is_active(&self, _caller: &str) -> bool {
        true
    }

    /// Every animatable property of running modules.
    fn properties(&self) -> Vec<PropertyInfo> {
        Vec::new()
    }

    /// The animatable property `path` of a running module.
    fn property(&self, path: &str) -> Option<PropertyInfo> {
        self.properties().into_iter().find(|info| info.path == path)
    }

    fn read_property(&self, _caller: &str, path: &str) -> Result<PropertyValue, String> {
        Err(format!("no property '{path}' here"))
    }

    /// Writes a property without the history.
    fn write_property(&self, _caller: &str, path: &str, _value: PropertyValue) -> Result<(), String> {
        Err(format!("no property '{path}' here"))
    }
}

/// The generic interface of the editor (rule S1), usable from any thread: jobs, scripts and,
/// through the C interface, compiled modules. Every value crosses it as JSON.
#[derive(Clone)]
pub struct Editor {
    backend: Arc<dyn EditorBackend>,
    /// Who acts: a module id, or `module#name` for a script run of that module or a module it
    /// hosts.
    caller: String,
    /// Where its settings are kept: the module id, or `module#name` for a handle with settings of
    /// its own.
    settings: String,
}

impl Editor {
    pub fn new(backend: Arc<dyn EditorBackend>, caller: &str) -> Self {
        let module = caller.split('#').next().unwrap_or(caller);
        Self {
            backend,
            caller: caller.to_owned(),
            settings: module.to_owned(),
        }
    }

    pub fn caller(&self) -> &str {
        &self.caller
    }

    /// The module this handle belongs to: the caller without its `#name` part.
    pub fn module(&self) -> &str {
        self.caller.split('#').next().unwrap_or(&self.caller)
    }

    /// A handle acting as `name` inside the same module, e.g. one script run: its calls are
    /// told apart from the others, in undo groups in particular. It shares the settings of the
    /// handle it comes from.
    pub fn derive(&self, name: &str) -> Editor {
        Editor {
            settings: self.settings.clone(),
            ..Editor::new(self.backend.clone(), &format!("{}#{name}", self.module()))
        }
    }

    /// Every command of running modules, with its description and schemas.
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

    /// A setting kept between sessions, readable from any thread.
    pub fn setting(&self, key: &str) -> Result<Option<Value>, String> {
        self.backend.setting(&self.caller, &self.settings, key)
    }

    pub fn set_setting(&self, key: &str, value: Value) -> Result<(), String> {
        self.backend.set_setting(&self.caller, &self.settings, key, value)
    }

    /// Logs under the module's name, with the caller's own name when it has one.
    pub fn log(&self, level: log::Level, message: &str) {
        let target = format!("uniwow_module_{}", self.module().replace('-', "_"));
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

    /// Records a change the module already made to its own state (F2); see `AppliedChange`.
    pub fn record_change(&self, label: &str, change: Box<dyn AppliedChange>) -> Result<(), String> {
        self.backend.record_change(&self.caller, label, change)
    }

    /// Makes the module fail, as if it had panicked, for a fault found on one of its threads.
    pub fn report_failure(&self, message: &str) {
        self.backend.report_failure(&self.caller, message);
    }

    /// Whether the module still runs: once it failed, its threads should stop.
    pub fn is_active(&self) -> bool {
        self.backend.is_active(&self.caller)
    }

    /// Every animatable property of running modules.
    pub fn properties(&self) -> Vec<PropertyInfo> {
        self.backend.properties()
    }

    /// The animatable property `path` of a running module.
    pub fn property(&self, path: &str) -> Option<PropertyInfo> {
        self.backend.property(path)
    }

    /// The current value of an animatable property.
    pub fn read_property(&self, path: &str) -> Result<PropertyValue, String> {
        self.backend.read_property(&self.caller, path)
    }

    /// Writes an animatable property, without the history; each number is kept within its range.
    pub fn write_property(&self, path: &str, value: PropertyValue) -> Result<(), String> {
        self.backend.write_property(&self.caller, path, value)
    }
}
