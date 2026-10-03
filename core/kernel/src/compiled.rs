//! A compiled module seen by the kernel as any other module (section 3): a DLL written in C, C++,
//! C# or any language able to export the entry point of `uniwow.h`.

use std::path::Path;
use std::sync::Arc;

use libloading::os::windows::{LOAD_WITH_ALTERED_SEARCH_PATH, Library};
use uniwow_api::capi::{self, InitFn, Started};
use uniwow_api::{Context, Module, Registrar, log};

/// Its commands are declared on its behalf (delegated, F6), and the C interface acts through an
/// `Editor` of the module itself.
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
    }

    fn init(&mut self, ctx: &mut Context) {
        let _ = self.started.context.editor.set(ctx.editor());
    }
}

/// Loads the DLL of a compiled module where it is, so that the DLLs it needs are found in its
/// folder, then starts it. A loaded module is never unloaded.
pub fn load(dll: &Path) -> Result<CompiledModule, String> {
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
    let started = capi::start(init)?;
    log::info!(
        "compiled module '{}' {} started from {}",
        started.name,
        started.version,
        file.to_string_lossy()
    );
    Ok(CompiledModule { started })
}
