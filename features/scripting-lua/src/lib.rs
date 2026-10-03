//! Lua 5.1 scripts (rules S1 to S9): a console, the scripts of `scripts\lua-5.1\` beside the
//! executable, each run on a thread of its own in a Lua state of its own, and Stop.

mod loading;
mod output;
mod run;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use uniwow_api::{Context, DockArea, Feature, JobId, JobOutcome, Registrar, egui};

use output::{Kind, Output};
use run::Source;

struct Running {
    name: String,
    started: Instant,
}

#[derive(Default)]
struct ScriptingLua {
    output: Arc<Output>,
    scripts: Vec<String>,
    line: String,
    /// By job number, so in starting order.
    running: BTreeMap<u64, Running>,
    next_run: u64,
}

impl Feature for ScriptingLua {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("lua", "Lua", DockArea::Bottom);
    }

    fn init(&mut self, _ctx: &mut Context) {
        self.refresh();
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        self.output.set_wake(ui.ctx());
        egui::Panel::left("lua-scripts")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| self.scripts_ui(ui, ctx));
        egui::CentralPanel::default().show(ui, |ui| self.console_ui(ui, ctx));
        if !self.running.is_empty() {
            // Keeps the elapsed times moving.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        let Some(run) = self.running.remove(&job.0) else {
            return;
        };
        if let JobOutcome::Panicked(message) = outcome {
            self.output
                .push(Kind::Error, format!("{}: the editor failed: {message}", run.name));
        }
    }
}

impl ScriptingLua {
    fn refresh(&mut self) {
        self.scripts = std::fs::read_dir(scripts_dir())
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lua")))
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        self.scripts.sort();
    }

    fn scripts_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        ui.horizontal(|ui| {
            ui.strong("Scripts");
            if ui.button("Refresh").clicked() {
                self.refresh();
            }
        });
        ui.weak(scripts_dir().display().to_string());
        ui.separator();
        let mut start = None;
        // The list takes the height the running section below leaves, and at least half of it:
        // with many runs, that section scrolls instead.
        let row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        let running_height = (1 + self.running.len().max(1)) as f32 * row + 2.0 * ui.spacing().item_spacing.y + 1.0;
        let available = ui.available_height();
        egui::ScrollArea::vertical()
            .id_salt("lua-script-list")
            .max_height((available - running_height).max(available / 2.0).max(row))
            .show(ui, |ui| {
                if self.scripts.is_empty() {
                    ui.weak("No script.");
                }
                for script in &self.scripts {
                    ui.horizontal(|ui| {
                        if ui.button("Run").clicked() {
                            start = Some(script.clone());
                        }
                        ui.label(script);
                    });
                }
            });
        if let Some(script) = start {
            match std::fs::read(scripts_dir().join(&script)) {
                Ok(text) => self.start(ctx, Source::Script { name: script, text }),
                Err(error) => self.output.push(Kind::Error, format!("{script}: {error}")),
            }
        }

        ui.separator();
        ui.strong("Running");
        if self.running.is_empty() {
            ui.weak("Nothing.");
        }
        let mut stop = None;
        egui::ScrollArea::vertical().id_salt("lua-running").show(ui, |ui| {
            for (job, run) in &self.running {
                ui.horizontal(|ui| {
                    if ui.button("Stop").clicked() {
                        stop = Some(JobId(*job));
                    }
                    ui.label(format!("{} ({:.1} s)", run.name, run.started.elapsed().as_secs_f64()));
                });
            }
        });
        if let Some(job) = stop {
            ctx.cancel(job);
        }
    }

    fn console_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let mut evaluate = false;
        ui.horizontal(|ui| {
            ui.monospace(">");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.line)
                    .code_editor()
                    .hint_text("Lua: an expression or a statement, Enter to run")
                    .desired_width(ui.available_width() - 110.0),
            );
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                evaluate = true;
                field.request_focus();
            }
            if ui.button("Run").clicked() {
                evaluate = true;
            }
            if ui.button("Clear").clicked() {
                self.output.clear();
            }
        });
        if evaluate && !self.line.trim().is_empty() {
            let line = self.line.trim().to_owned();
            self.output.push(Kind::Info, format!("> {line}"));
            self.start(ctx, Source::Console(line));
        }
        ui.separator();
        self.output.show(ui);
    }

    fn start(&mut self, ctx: &mut Context, source: Source) {
        self.next_run += 1;
        let name = match &source {
            Source::Script { name, .. } => format!("{name} #{}", self.next_run),
            Source::Console(_) => format!("console #{}", self.next_run),
        };
        let output = self.output.clone();
        let run_name = name.clone();
        let editor = ctx.editor().derive(&name);
        // A run may wait for events as long as it likes: it does not hold a thread of the pool.
        let job = ctx.spawn_thread(&name, move |context| {
            run::run(source, editor, context.cancellation(), output, &run_name)
        });
        self.running.insert(
            job.0,
            Running {
                name,
                started: Instant::now(),
            },
        );
    }
}

/// `scripts\lua-5.1\` beside the executable.
pub(crate) fn scripts_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("scripts").join("lua-5.1")))
        .unwrap_or_else(|| PathBuf::from("scripts").join("lua-5.1"))
}

uniwow_api::export_feature!(ScriptingLua::default());
