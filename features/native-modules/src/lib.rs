//! Loads the compiled modules of `modules\` beside the executable and gives each the C interface
//! of `sdk/uniwow.h` (rules S1, S10, T7). Their commands join the catalogue and run on the
//! calling thread.

mod capi;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use uniwow_api::{Context, DockArea, Feature, Registrar, egui, log};

const FEATURE: &str = "native-modules";

/// A command a module offers, and why it is refused, if it is.
struct Offered {
    name: String,
    refused: Option<String>,
}

struct Module {
    file: String,
    /// Name, version and commands, or why the module was refused.
    state: Result<(String, String, Vec<Offered>), String>,
}

#[derive(Default)]
struct NativeModules {
    modules: Vec<Module>,
    contexts: Vec<&'static capi::ModuleContext>,
}

impl Feature for NativeModules {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("modules", "Modules", DockArea::Bottom);
        // Command name → the module that offers it.
        let mut offered_by: HashMap<String, String> = HashMap::new();
        for path in module_files() {
            let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let state = match capi::load(&path) {
                Ok(loaded) => {
                    let mut commands = Vec::new();
                    for command in loaded.commands {
                        if let Some(first) = offered_by.get(&command.name) {
                            commands.push(Offered {
                                name: command.name,
                                refused: Some(format!("already offered by the module '{first}'")),
                            });
                            continue;
                        }
                        let handler = command.handler;
                        reg.command_on_caller(
                            &command.name,
                            &command.description,
                            command.arguments,
                            command.result,
                            Arc::new(move |arguments| handler.invoke(&arguments)),
                        );
                        offered_by.insert(command.name.clone(), loaded.name.clone());
                        commands.push(Offered {
                            name: command.name,
                            refused: None,
                        });
                    }
                    self.contexts.push(loaded.context);
                    log::info!("module '{}' {} loaded from {file}", loaded.name, loaded.version);
                    Ok((loaded.name, loaded.version, commands))
                }
                Err(reason) => {
                    log::warn!("module {file} refused: {reason}");
                    Err(reason)
                }
            };
            self.modules.push(Module { file, state });
        }
    }

    fn init(&mut self, ctx: &mut Context) {
        let editor = ctx.editor();
        for context in &self.contexts {
            let _ = context.editor.set(editor.derive_with_settings(&context.name));
        }
        // A name another feature offered first stays that feature's: the kernel kept it.
        let owners: HashMap<String, String> = editor.commands().into_iter().map(|c| (c.name, c.owner)).collect();
        for module in &mut self.modules {
            let Ok((_, _, commands)) = &mut module.state else {
                continue;
            };
            for command in commands.iter_mut().filter(|c| c.refused.is_none()) {
                if let Some(owner) = owners.get(&command.name).filter(|owner| *owner != FEATURE) {
                    log::warn!(
                        "command '{}' of {} refused: offered by '{owner}'",
                        command.name,
                        module.file
                    );
                    command.refused = Some(format!("already offered by '{owner}'"));
                }
            }
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, _ctx: &mut Context) {
        ui.label(format!("Modules of {}", modules_dir().display()));
        ui.separator();
        if self.modules.is_empty() {
            ui.weak("No module.");
            return;
        }
        let error = ui.visuals().error_fg_color;
        egui::Grid::new("modules").striped(true).num_columns(5).show(ui, |ui| {
            for header in ["File", "State", "Module", "Version", "Commands or reason"] {
                ui.strong(header);
            }
            ui.end_row();
            for module in &self.modules {
                ui.label(&module.file);
                match &module.state {
                    Ok((name, version, commands)) => {
                        ui.colored_label(egui::Color32::from_rgb(90, 170, 90), "running");
                        ui.label(name);
                        ui.label(version);
                        ui.horizontal_wrapped(|ui| {
                            for command in commands {
                                match &command.refused {
                                    None => ui.label(&command.name),
                                    Some(reason) => {
                                        ui.colored_label(error, format!("{} (refused: {reason})", command.name))
                                    }
                                };
                            }
                        });
                    }
                    Err(reason) => {
                        ui.colored_label(error, "refused");
                        ui.label("");
                        ui.label("");
                        ui.colored_label(error, reason);
                    }
                }
                ui.end_row();
            }
        });
    }
}

fn modules_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("modules")))
        .unwrap_or_else(|| PathBuf::from("modules"))
}

fn module_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(modules_dir())
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

uniwow_api::export_feature!(NativeModules::default());
