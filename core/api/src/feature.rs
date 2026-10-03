use std::any::Any;

use crate::{CallId, Context, Event, JobId, JobOutcome, Registrar, egui};

/// A feature loaded from its own DLL.
///
/// The kernel calls `register` once, then `init` once every feature has registered, then the
/// other methods as the user works. Any panic is caught: the feature is then disabled.
pub trait Feature: Any {
    /// Declares what the feature contributes: panels, menu items, services, subscriptions.
    fn register(&mut self, reg: &mut Registrar);

    /// Starts the feature. Services provided by other features are available from here.
    fn init(&mut self, _ctx: &mut Context) {}

    /// Draws one of the panels declared in `register`, identified by `panel`.
    fn panel_ui(&mut self, _panel: &str, _ui: &mut egui::Ui, _ctx: &mut Context) {}

    /// Receives an event whose topic the feature subscribed to.
    fn on_event(&mut self, _event: &Event, _ctx: &mut Context) {}

    /// Handles a click on one of the menu items declared in `register`, identified by `action`.
    fn on_menu(&mut self, _action: &str, _ctx: &mut Context) {}

    /// Runs one of the named commands declared with `Registrar::command`, on the interface thread.
    fn on_command(
        &mut self,
        name: &str,
        _arguments: serde_json::Value,
        _ctx: &mut Context,
    ) -> Result<serde_json::Value, String> {
        Err(format!("'{name}' is not handled"))
    }

    /// Receives the end of a job started with `Context::spawn`.
    fn on_job(&mut self, _job: JobId, _outcome: JobOutcome, _ctx: &mut Context) {}

    /// Receives the answer to a call made with `Context::call`.
    fn on_reply(&mut self, _call: CallId, _result: Result<serde_json::Value, String>, _ctx: &mut Context) {}

    /// Called once when the editor closes.
    fn shutdown(&mut self) {}
}

/// Signature of the function that creates the feature, exported by every feature DLL.
pub type CreateFn = fn() -> Box<dyn Feature>;

/// Signature of the function returning the feature's package name, as a NUL-terminated string.
pub type PackageFn = extern "C" fn() -> *const u8;

pub const CREATE_SYMBOL: &[u8] = b"uniwow_feature_create\0";
pub const PACKAGE_SYMBOL: &[u8] = b"uniwow_feature_package\0";

/// Exports the entry points of a feature DLL.
///
/// `$create` is an expression building the feature, e.g. `MyFeature::default()`.
#[macro_export]
macro_rules! export_feature {
    ($create:expr) => {
        #[unsafe(no_mangle)]
        pub fn uniwow_feature_create() -> Box<dyn $crate::Feature> {
            Box::new($create)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn uniwow_feature_package() -> *const u8 {
            concat!(env!("CARGO_PKG_NAME"), "\0").as_ptr()
        }
    };
}
