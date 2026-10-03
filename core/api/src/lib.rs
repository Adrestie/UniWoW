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
mod context;
mod event;
mod feature;
mod registrar;
pub mod viewport;

pub use command::Command;
pub use context::{Context, Host};
pub use event::Event;
pub use feature::{CREATE_SYMBOL, CreateFn, Feature, PACKAGE_SYMBOL, PackageFn};
pub use registrar::{DockArea, MenuItemSpec, PanelSpec, Registrar};

/// Topic published by the kernel when a feature fails while running. Payload: `{ "id": <feature id> }`.
pub const FEATURE_FAILED_TOPIC: &str = "kernel.feature_failed";

/// Name of the runtime DLL, next to the executable.
pub const RUNTIME_DLL: &str = "uniwow_api.dll";
