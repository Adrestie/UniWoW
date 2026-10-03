//! Named commands: the actions features offer to each other, to scripts and to native modules.

use std::sync::Arc;

use serde_json::Value;

/// Handler of a command that runs on the calling thread.
pub type CommandHandler = Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>;

/// Where a named command runs (rule T4).
#[derive(Clone)]
pub enum RunsOn {
    /// On the interface thread, through `Feature::on_command`: it may change the feature's state.
    Interface,
    /// On the thread that calls it, at once: it only reads, or synchronises itself.
    Caller(CommandHandler),
}

/// A named command as declared in `Feature::register`.
#[derive(Clone)]
pub struct CommandSpec {
    /// Unique name, by convention prefixed with the feature's id, e.g. `cube.paint`.
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments.
    pub arguments: Value,
    /// JSON Schema of the result.
    pub result: Value,
    pub runs_on: RunsOn,
}

/// A command of the catalogue, as listed to callers.
#[derive(Clone, Debug)]
pub struct CommandInfo {
    pub name: String,
    /// Id of the feature offering it.
    pub owner: String,
    pub description: String,
    pub arguments: Value,
    pub result: Value,
    /// True when it runs on the calling thread.
    pub on_caller: bool,
}

/// Identifies a call made from the interface thread with `Context::call`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CallId(pub u64);

/// Reads command arguments into a type declared by the command's feature.
pub fn decode_arguments<T: serde::de::DeserializeOwned>(arguments: &Value) -> Result<T, String> {
    T::deserialize(arguments).map_err(|e| format!("invalid arguments: {e}"))
}
