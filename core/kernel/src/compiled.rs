//! A compiled module seen by the kernel as any other module (section 3): a DLL written in C, C++,
//! C# or any language able to export the entry point of `uniwow.h`.

use std::path::Path;
use std::sync::Arc;

use libloading::os::windows::{LOAD_WITH_ALTERED_SEARCH_PATH, Library};
use uniwow_api::{Context, Module, Registrar, egui, log};

use crate::capi::{self, InitFn, Started};

/// Its commands are declared on its behalf (delegated, F6), its panels are drawn from its
/// interface objects, and the C interface acts through an `Editor` of the module itself.
pub struct CompiledModule {
    started: Started,
}

impl Module for CompiledModule {
    fn register(&mut self, reg: &mut Registrar) {
        for command in &self.started.commands {
            let handler = command.handler;
            reg.command_on_caller_delegated(
                &command.name,
                &command.description,
                command.arguments.clone(),
                command.result.clone(),
                Arc::new(move |arguments| handler.invoke(&arguments)),
            );
        }
        for panel in &self.started.panels {
            reg.panel(&panel.id, &panel.title, panel.area);
        }
        for property in &self.started.properties {
            let (read, write) = (property.clone(), property.clone());
            reg.animatable(
                &property.name,
                &property.label,
                property.kind,
                property.range,
                move || read.read(),
                move |value| write.write(value),
            );
        }
    }

    fn init(&mut self, ctx: &mut Context) {
        let _ = self.started.context.editor.set(ctx.editor());
        ctx.adopt_objects(&self.started.context.ui);
    }

    fn panel_ui(&mut self, panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        ctx.draw_objects(&self.started.context.ui, panel, ui);
    }

    fn windows_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        ctx.draw_dialogs(&self.started.context.ui, egui);
    }
}

impl CompiledModule {
    /// A module started from an entry point in the process, for the tests.
    #[cfg(test)]
    pub fn started(started: Started) -> Self {
        Self { started }
    }

    /// What the C interface knows of it: its objects, through which jobs reach its thread, and
    /// the work waiting there.
    pub fn context(&self) -> &'static capi::ModuleContext {
        self.started.context
    }
}

/// Loads the DLL of a compiled module where it is, so that the DLLs it needs are found in its
/// folder, then starts it. A loaded module is never unloaded.
pub fn load(dll: &Path, id: &str) -> Result<CompiledModule, String> {
    let file = dll.file_name().unwrap_or_default();
    // Windows would hand back the DLL of that name already in the process instead of the module.
    if Library::open_already_loaded(file).is_ok() {
        return Err(format!(
            "a DLL named {} is already loaded in the editor: rename the module's DLL",
            file.to_string_lossy()
        ));
    }
    // SAFETY: loading runs the DLL's initialisation, the accepted risk of compiled modules (S6).
    let library = unsafe { Library::load_with_flags(dll, LOAD_WITH_ALTERED_SEARCH_PATH) }
        .map_err(|error| format!("could not be loaded: {error}"))?;
    // SAFETY: uniwow.h fixes the signature of the entry point.
    let init: InitFn = unsafe {
        *library
            .get::<InitFn>(capi::INIT_SYMBOL)
            .map_err(|_| "no uniwow_module_init entry point: not a UniWoW compiled module".to_owned())?
    };
    std::mem::forget(library);
    let started = capi::start(init, id)?;
    log::info!(
        "compiled module '{}' {} started from {}",
        started.name,
        started.version,
        file.to_string_lossy()
    );
    Ok(CompiledModule { started })
}
