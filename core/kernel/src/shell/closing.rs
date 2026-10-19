//! Closing the editor: the question about the unsaved changes, asked through the command opening
//! a modal window.

use super::*;

impl Shell {
    /// The line of the closing window for the project, when it changed since it was saved.
    pub(super) fn project_unsaved(&mut self) -> Option<String> {
        let now = project::text(&self.project.name(), &self.project_sections());
        (!self.project.saved.is_empty() && now != self.project.saved)
            .then(|| format!("- The project '{}'", self.project.name()))
    }

    /// The unsaved documents of the running modules, with the module's name.
    pub(super) fn unsaved(&mut self) -> Vec<(usize, String, String)> {
        let mut found = Vec::new();
        for index in 0..self.slots.len() {
            if !self.slots[index].state.is_running() {
                continue;
            }
            let slot = &self.slots[index];
            let name = slot
                .manifest
                .as_ref()
                .map_or_else(|| slot.id.clone(), |m| m.name.clone());
            match call_module(&mut self.slots[index], &mut self.host, |f, _| f.unsaved()) {
                Ok(documents) => found.extend(documents.into_iter().map(|d| (index, name.clone(), d))),
                Err(message) => self.fail(index, format!("unsaved changes: {message}")),
            }
        }
        found
    }

    /// Keeps the editor open while modules have unsaved changes, and asks the user in a dialog
    /// window when a module offers them; without one, the changes are lost.
    pub(super) fn close_requested(&mut self, ctx: &egui::Context) {
        match self.closing {
            Closing::Confirmed => return,
            Closing::Asking(_) if self.host.bridge.lookup(DIALOG_COMMAND).is_ok() => {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                show_window(ctx);
                return;
            }
            // Its window went with the module that showed it: the editor closes as without it.
            Closing::Asking(_) => self.closing = Closing::Open,
            Closing::Open => {}
        }
        let unsaved = self.unsaved();
        let project = self.project_unsaved();
        if unsaved.is_empty() && project.is_none() {
            return;
        }
        let list: Vec<String> = project
            .into_iter()
            .chain(
                unsaved
                    .iter()
                    .map(|(_, module, document)| format!("- {module}: {document}")),
            )
            .collect();
        let arguments = serde_json::json!({
            "title": "Unsaved changes",
            "text": format!("These changes are not saved:\n{}", list.join("\n")),
            "buttons": [
                { "id": "save", "label": "Save" },
                { "id": "discard", "label": "Don't save" },
                { "id": "cancel", "label": "Cancel" },
            ],
            "escape": "cancel",
            "first": true,
        });
        match self
            .run_command("kernel", std::thread::current().id(), DIALOG_COMMAND, arguments)
            .map(|answer| answer["dialog"].as_u64())
        {
            Ok(Some(dialog)) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                show_window(ctx);
                self.closing = Closing::Asking(dialog);
            }
            Ok(None) => log::warn!(
                "unsaved changes lost, '{DIALOG_COMMAND}' giving no window number:\n{}",
                list.join("\n")
            ),
            Err(error) => log::warn!("unsaved changes lost, {error}:\n{}", list.join("\n")),
        }
    }

    /// The user's answer about the unsaved changes, when the editor is closed.
    pub(super) fn closing_answered(&mut self, event: &Event) {
        let Closing::Asking(dialog) = self.closing else {
            return;
        };
        if event.payload["dialog"].as_u64() != Some(dialog) {
            return;
        }
        self.closing = Closing::Open;
        match event.payload["button"].as_str() {
            Some("save") => {
                let mut failures = Vec::new();
                if self.project_unsaved().is_some() {
                    // Untitled: into a folder of its own in the folder of the projects.
                    let folder = self.project.folder.clone().or_else(|| {
                        let stamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |since| since.as_secs());
                        project::default_folder().map(|folder| folder.join(format!("Untitled {stamp}")))
                    });
                    match folder.map(|folder| self.save_project_to(folder)) {
                        Some(Ok(())) => {}
                        Some(Err(error)) => failures.push(format!("- the project: {error}")),
                        None => failures.push("- the project: no folder to save it into".to_owned()),
                    }
                }
                let mut saved: Vec<usize> = Vec::new();
                for (index, module, _) in self.unsaved() {
                    if saved.contains(&index) {
                        continue;
                    }
                    saved.push(index);
                    match call_module(&mut self.slots[index], &mut self.host, |f, ctx| f.save_unsaved(ctx)) {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => failures.push(format!("- {module}: {error}")),
                        Err(message) => {
                            failures.push(format!("- {module}: {message}"));
                            self.fail(index, format!("saving: {message}"));
                        }
                    }
                }
                if failures.is_empty() {
                    self.closing = Closing::Confirmed;
                    return;
                }
                let text = format!(
                    "The editor stays open: these changes could not be saved.\n{}",
                    failures.join("\n")
                );
                log::error!("{text}");
                let arguments = serde_json::json!({
                    "title": "Unsaved changes",
                    "text": text,
                    "buttons": [{ "id": "ok", "label": "OK" }],
                    "escape": "ok",
                    "first": true,
                });
                if let Err(error) = self.run_command("kernel", std::thread::current().id(), DIALOG_COMMAND, arguments) {
                    log::error!("{error}");
                }
            }
            Some("discard") => self.closing = Closing::Confirmed,
            _ => {}
        }
    }
}

/// Brings the window back from the taskbar, so that its question is seen.
pub(super) fn show_window(ctx: &egui::Context) {
    if ctx.input(|i| i.viewport().minimized.unwrap_or(false)) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}
