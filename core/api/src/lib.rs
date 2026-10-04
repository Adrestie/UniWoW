//! Contracts between the UniWoW kernel and its modules.
//!
//! This crate is built as the shared runtime DLL. Modules depend on it alone and reach egui,
//! wgpu and the other shared libraries through the re-exports below, so that each of them
//! exists once in memory.

pub use bytemuck;
pub use eframe;
pub use eframe::egui;
pub use eframe::egui_wgpu;
pub use eframe::wgpu;
pub use egui_dock;
pub use glam;
pub use log;
pub use serde;
pub use serde_json;
/// Lua 5.1 (`libs/lua`), part of the runtime.
pub use uniwow_lua::mlua;

/// An enumeration of numbers shared with every language, read back from a number.
macro_rules! numbered {
    ($(#[$meta:meta])* $name:ident { $($(#[$doc:meta])* $variant:ident = $value:literal,)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u32)]
        pub enum $name { $($(#[$doc])* $variant = $value,)* }

        impl $name {
            pub fn from_u32(value: u32) -> Option<Self> {
                match value {
                    $($value => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

mod command;
mod commands;
mod context;
pub mod curve;
mod editor;
mod event;
mod job;
mod module;
mod numbers;
mod property;
mod registrar;
mod service;
pub mod ui;
pub mod viewport;

pub use command::{AppliedChange, Command};
pub use commands::{CallId, CommandHandler, CommandInfo, CommandSpec, RunsOn, decode_arguments};
pub use context::{Context, Host};
pub use editor::{Editor, EditorBackend};
pub use event::Event;
pub use job::{JobContext, JobFn, JobId, JobOutcome};
pub use module::{CREATE_SYMBOL, CreateFn, Module, PACKAGE_SYMBOL, PackageFn};
pub use property::{PropertyInfo, PropertyKind, PropertySpec, PropertyValue, ReadProperty, WriteProperty, range_error};
pub use registrar::{DockArea, MenuItemSpec, PanelSpec, Registrar};
pub use service::ServiceKey;

/// Topic published by the kernel when a module fails while running. Payload: `{ "id": <module id> }`.
pub const MODULE_FAILED_TOPIC: &str = "kernel.module_failed";

/// Command opening a modal window, offered by the module `dialogs` when it runs. Arguments:
/// `{ "title", "text", "buttons": [{ "id", "label" }], "escape": <id of a button>, "first": <bool> }`,
/// `first` putting the window before those waiting, for the kernel's question when the editor
/// closes; result: `{ "dialog": <number> }`.
pub const DIALOG_COMMAND: &str = "ui.dialog";

/// Topic of the answer to a window opened with `DIALOG_COMMAND`. Payload:
/// `{ "dialog": <number>, "button": <id> }`.
pub const DIALOG_ANSWERED_TOPIC: &str = "ui.dialog_answered";

/// Name of the runtime DLL, next to the executable.
pub const RUNTIME_DLL: &str = "uniwow_api.dll";
