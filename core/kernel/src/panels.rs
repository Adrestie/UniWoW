//! The Jobs and Commands panels of the kernel.

use uniwow_api::serde_json::{self, Value};
use uniwow_api::{EditorBackend, egui};

use crate::jobs::Pool;
use crate::router::{Bridge, ReplyTo, Request};

pub fn jobs_panel(ui: &mut egui::Ui, pool: &Pool) {
    ui.label(format!("{} worker threads", pool.threads()));
    ui.separator();
    if pool.running().is_empty() {
        ui.weak("No job running.");
        return;
    }
    egui::Grid::new("jobs").striped(true).num_columns(5).show(ui, |ui| {
        for header in ["Feature", "Job", "Progress", "Time", ""] {
            ui.strong(header);
        }
        ui.end_row();
        for job in pool.running() {
            ui.label(&job.owner);
            ui.label(&job.label);
            ui.add(
                egui::ProgressBar::new(job.progress())
                    .desired_width(160.0)
                    .show_percentage(),
            );
            ui.label(format!("{:.1} s", job.started.elapsed().as_secs_f32()));
            if job.is_cancelled() {
                ui.weak("cancelling…");
            } else if ui.button("Cancel").clicked() {
                pool.cancel(&job.owner, job.id);
            }
            ui.end_row();
        }
    });
}

/// The catalogue of commands, and a field to call one with JSON arguments.
#[derive(Default)]
pub struct CommandsPanel {
    selected: Option<String>,
    arguments: String,
    next_call: u64,
    waiting: Option<u64>,
    answer: Option<Result<Value, String>>,
}

impl CommandsPanel {
    pub fn answer(&mut self, call: u64, result: Result<Value, String>) {
        if self.waiting == Some(call) {
            self.waiting = None;
            self.answer = Some(result);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, bridge: &Bridge) {
        let commands = bridge.commands();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(220.0);
                ui.strong(format!("{} commands", commands.len()));
                egui::ScrollArea::vertical().id_salt("command list").show(ui, |ui| {
                    for command in &commands {
                        let selected = self.selected.as_deref() == Some(command.name.as_str());
                        if ui.selectable_label(selected, &command.name).clicked() {
                            self.selected = Some(command.name.clone());
                            self.arguments = "{}".to_owned();
                            self.answer = None;
                        }
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("command details").show(ui, |ui| {
                ui.vertical(|ui| {
                    let Some(command) = commands
                        .iter()
                        .find(|c| Some(c.name.as_str()) == self.selected.as_deref())
                    else {
                        ui.weak("Select a command.");
                        return;
                    };
                    ui.heading(&command.name);
                    ui.label(format!(
                        "Offered by '{}', runs on the {} thread.",
                        command.owner,
                        if command.on_caller { "calling" } else { "interface" }
                    ));
                    ui.label(&command.description);
                    ui.collapsing("Arguments schema", |ui| ui.monospace(pretty(&command.arguments)));
                    ui.collapsing("Result schema", |ui| ui.monospace(pretty(&command.result)));
                    ui.label("Arguments (JSON):");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.arguments)
                            .code_editor()
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    if ui
                        .add_enabled(self.waiting.is_none(), egui::Button::new("Call"))
                        .clicked()
                    {
                        match serde_json::from_str::<Value>(&self.arguments) {
                            Ok(arguments) => {
                                self.next_call += 1;
                                self.waiting = Some(self.next_call);
                                self.answer = None;
                                bridge.queue(Request::Call {
                                    caller: "kernel".to_owned(),
                                    thread: std::thread::current().id(),
                                    name: command.name.clone(),
                                    arguments,
                                    reply: ReplyTo::Kernel(self.next_call),
                                });
                            }
                            Err(error) => self.answer = Some(Err(format!("invalid JSON: {error}"))),
                        }
                    }
                    match &self.answer {
                        Some(Ok(value)) => {
                            ui.monospace(pretty(value));
                        }
                        Some(Err(error)) => {
                            ui.colored_label(ui.visuals().error_fg_color, error);
                        }
                        None if self.waiting.is_some() => {
                            ui.weak("waiting for the answer…");
                        }
                        None => {}
                    }
                });
            });
        });
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
