//! The files of the client: its archives, in the order the client reads them, with the patches
//! WarcraftXL adds, read from any thread at once through the service `vfs`. The archives are opened
//! by jobs of the pool, one each, their lists merged by another; the interface thread only hands
//! the chain over. Its panel sets the folder of the client and says how far its files are.

mod chain;
mod mpq;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use uniwow_api::vfs::{self, Vfs, VfsState};
use uniwow_api::{Context, DockArea, JobId, JobOutcome, Module, Registrar, egui, log, serde_json};

use chain::{Chain, Source};

/// The setting holding the folder of the client.
const CLIENT_FOLDER: &str = "client_folder";

/// The service `vfs`: the chain once open, given whole to its readers, who read without a lock.
#[derive(Default)]
struct Files {
    state: RwLock<FilesState>,
}

enum FilesState {
    NoClient(String),
    Opening,
    Ready(Arc<Chain>),
}

impl Default for FilesState {
    fn default() -> Self {
        Self::NoClient("no folder of the client set".to_owned())
    }
}

impl Files {
    fn chain(&self) -> Result<Arc<Chain>, String> {
        match &*self.state.read().unwrap_or_else(|e| e.into_inner()) {
            FilesState::Ready(chain) => Ok(chain.clone()),
            FilesState::Opening => Err("the client's archives are being opened".to_owned()),
            FilesState::NoClient(reason) => Err(reason.clone()),
        }
    }

    fn set(&self, state: FilesState) {
        *self.state.write().unwrap_or_else(|e| e.into_inner()) = state;
    }
}

impl Vfs for Files {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        self.chain()?.read(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.chain().is_ok_and(|chain| chain.exists(path))
    }

    fn files_under(&self, folder: &str) -> Vec<String> {
        self.chain().map(|chain| chain.files_under(folder)).unwrap_or_default()
    }

    fn state(&self) -> VfsState {
        match &*self.state.read().unwrap_or_else(|e| e.into_inner()) {
            FilesState::NoClient(reason) => VfsState::NoClient(reason.clone()),
            FilesState::Opening => VfsState::Opening,
            FilesState::Ready(chain) => VfsState::Ready {
                archives: chain.sources().len(),
                files: chain.listed_count(),
            },
        }
    }
}

/// The archives being opened, by the job opening each, then the job merging their lists.
#[derive(Default)]
struct Opening {
    jobs: HashMap<JobId, usize>,
    sources: Vec<Option<Source>>,
    index: Option<JobId>,
}

#[derive(Default)]
struct AssetsModule {
    files: Arc<Files>,
    opening: Opening,
    /// The folder typed in the panel.
    folder: String,
    /// The archives that could not be opened, with why.
    refused: Vec<String>,
}

impl AssetsModule {
    /// Opens the client in `folder`: its locale and the order of its archives found here, each
    /// archive then opened by a job of its own.
    fn open(&mut self, folder: &Path, ctx: &mut Context) {
        for job in self.opening.jobs.keys().chain(&self.opening.index) {
            ctx.cancel(*job);
        }
        self.opening = Opening::default();
        self.refused.clear();
        let locale = match chain::locale(folder) {
            Ok(locale) => locale,
            Err(reason) => {
                self.files
                    .set(FilesState::NoClient(format!("{}: {reason}", folder.display())));
                return;
            }
        };
        let paths = chain::order(folder, &locale);
        if paths.is_empty() {
            self.files.set(FilesState::NoClient(format!(
                "{}: no archive of 3.3.5a",
                folder.display()
            )));
            return;
        }
        self.files.set(FilesState::Opening);
        self.opening.sources = paths.iter().map(|_| None).collect();
        for (place, path) in paths.into_iter().enumerate() {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let job = ctx.spawn(&format!("Open {name}"), move |_| {
                Source::open(&path).map_err(|e| format!("{}: {e}", path.display()))
            });
            self.opening.jobs.insert(job, place);
        }
    }
}

impl Module for AssetsModule {
    fn register(&mut self, reg: &mut Registrar) {
        let files: Arc<dyn Vfs> = self.files.clone();
        reg.provide(vfs::SERVICE, files);
        reg.panel("assets", "Assets", DockArea::Bottom);
    }

    fn init(&mut self, ctx: &mut Context) {
        if let Some(folder) = ctx
            .setting(CLIENT_FOLDER)
            .and_then(|value| value.as_str().map(str::to_owned))
        {
            self.folder = folder.clone();
            self.open(Path::new(&folder), ctx);
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        ui.horizontal(|ui| {
            ui.label("Folder of the client");
            ui.add(egui::TextEdit::singleline(&mut self.folder).desired_width(360.0));
            if ui.button("Open").clicked() {
                let folder = self.folder.trim().to_owned();
                ctx.set_setting(CLIENT_FOLDER, serde_json::json!(folder));
                self.open(&PathBuf::from(folder), ctx);
            }
        });
        match self.files.state() {
            VfsState::NoClient(reason) => ui.weak(reason),
            VfsState::Opening => {
                let left = self.opening.jobs.len() + usize::from(self.opening.index.is_some());
                ui.label(format!("Opening the archives: {left} jobs left"))
            }
            VfsState::Ready { archives, files } => ui.label(format!("{archives} archives, {files} files listed")),
        };
        for reason in &self.refused {
            ui.colored_label(ui.visuals().warn_fg_color, reason);
        }
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if let Some(place) = self.opening.jobs.remove(&job) {
            match outcome.take::<Result<Source, String>>() {
                Some(Ok(source)) => self.opening.sources[place] = Some(source),
                Some(Err(reason)) => {
                    log::warn!("an archive of the client is left out: {reason}");
                    self.refused.push(reason);
                }
                None => {}
            }
            if self.opening.jobs.is_empty() {
                let sources: Vec<Source> = self.opening.sources.drain(..).flatten().collect();
                self.opening.index = Some(ctx.spawn("Index the client's files", move |_| Chain::new(sources)));
            }
        } else if self.opening.index == Some(job) {
            self.opening.index = None;
            if let Some(chain) = outcome.take::<Chain>() {
                self.files.set(FilesState::Ready(Arc::new(chain)));
            }
        }
    }
}

uniwow_api::export_module!(AssetsModule::default());
