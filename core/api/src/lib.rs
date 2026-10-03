//! Contracts between the UniWoW kernel and its features.
//!
//! This crate is built as the shared runtime DLL. Features depend on it alone and reach egui,
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

mod command;
mod commands;
mod context;
mod editor;
mod event;
mod feature;
mod job;
mod registrar;
mod service;
pub mod viewport;

pub use command::Command;
pub use commands::{CallId, CommandHandler, CommandInfo, CommandSpec, RunsOn, decode_arguments};
pub use context::{Context, Host};
pub use editor::{Editor, EditorBackend};
pub use event::Event;
pub use feature::{CREATE_SYMBOL, CreateFn, Feature, PACKAGE_SYMBOL, PackageFn};
pub use job::{JobContext, JobFn, JobId, JobOutcome};
pub use registrar::{DockArea, MenuItemSpec, PanelSpec, Registrar};
pub use service::ServiceKey;

/// Topic published by the kernel when a feature fails while running. Payload: `{ "id": <feature id> }`.
pub const FEATURE_FAILED_TOPIC: &str = "kernel.feature_failed";

/// Name of the runtime DLL, next to the executable.
pub const RUNTIME_DLL: &str = "uniwow_api.dll";
