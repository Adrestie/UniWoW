//! The files of the client: its archives, in the order the client reads them, with the patches
//! WarcraftXL adds, read from any thread at once through the service `vfs`, which also turns a
//! FileDataID into a path as WarcraftXL does; their formats parsed through the service `formats`.
//! The archives are opened by jobs of the pool, one each, their lists merged and the tables of
//! paths read by another; the interface thread only hands the client over. Its panel chooses the
//! folder of the client in the folder picker of the system and says how far its files are.

mod chain;
mod db2;
mod dbc;
mod mpq;
#[cfg(test)]
mod table_tests;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use uniwow_api::formats::{self, AreaRecord, CreatureDisplay, CreatureModel, Formats, MapRecord};
use uniwow_api::vfs::{self, Vfs, VfsState};
use uniwow_api::{Context, DockArea, JobId, JobOutcome, Module, Registrar, egui, log, rfd, serde_json};

use chain::{Chain, Source};
use db2::FileIds;
use dbc::Tables;

/// The setting holding the folder of the client.
const CLIENT_FOLDER: &str = "client_folder";

/// The client once open: its archives, its FileDataIDs, and its tables, each read when first asked.
struct Client {
    chain: Chain,
    file_ids: FileIds,
    tables: Tables,
}

impl Client {
    /// The client of `folder` and `locale`, its archives `sources`, the first read first; with the
    /// tables of paths that could not be read, and why.
    fn open(sources: Vec<Source>, folder: &Path, locale: &str) -> (Self, Vec<String>) {
        let chain = Chain::new(sources);
        let (file_ids, refused) = FileIds::load(folder, &chain);
        let client = Self {
            chain,
            file_ids,
            tables: Tables::new(locale),
        };
        (client, refused)
    }
}

/// The services `vfs` and `formats`: the client once open, given whole to its readers, who read
/// without a lock.
#[derive(Default)]
struct Files {
    state: RwLock<FilesState>,
}

enum FilesState {
    NoClient(String),
    Opening,
    Ready(Arc<Client>),
}

impl Default for FilesState {
    fn default() -> Self {
        Self::NoClient("no folder of the client set".to_owned())
    }
}

impl Files {
    fn client(&self) -> Result<Arc<Client>, String> {
        match &*self.state.read().unwrap_or_else(|e| e.into_inner()) {
            FilesState::Ready(client) => Ok(client.clone()),
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
        self.client()?.chain.read(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.client().is_ok_and(|client| client.chain.exists(path))
    }

    fn files_under(&self, folder: &str) -> Vec<String> {
        self.client()
            .map(|client| client.chain.files_under(folder))
            .unwrap_or_default()
    }

    fn path_of(&self, file_data_id: u32) -> Option<String> {
        self.client().ok()?.file_ids.path_of(file_data_id)
    }

    fn state(&self) -> VfsState {
        match &*self.state.read().unwrap_or_else(|e| e.into_inner()) {
            FilesState::NoClient(reason) => VfsState::NoClient(reason.clone()),
            FilesState::Opening => VfsState::Opening,
            FilesState::Ready(client) => VfsState::Ready {
                archives: client.chain.sources().len(),
                files: client.chain.listed_count(),
            },
        }
    }
}

impl Formats for Files {
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String> {
        let client = self.client()?;
        client.tables.maps(&client.chain)
    }

    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String> {
        let client = self.client()?;
        client.tables.areas(&client.chain)
    }

    fn creature_displays(&self) -> Result<Arc<Vec<CreatureDisplay>>, String> {
        let client = self.client()?;
        client.tables.creature_displays(&client.chain)
    }

    fn creature_models(&self) -> Result<Arc<Vec<CreatureModel>>, String> {
        let client = self.client()?;
        client.tables.creature_models(&client.chain)
    }
}

/// The archives being opened, by the job opening each, then the job merging their lists and
/// reading the tables of paths; the folder and the locale of the client.
#[derive(Default)]
struct Opening {
    jobs: HashMap<JobId, usize>,
    sources: Vec<Option<Source>>,
    index: Option<JobId>,
    folder: PathBuf,
    locale: String,
}

#[derive(Default)]
struct AssetsModule {
    files: Arc<Files>,
    opening: Opening,
    /// The folder shown in the panel.
    folder: String,
    /// The archives that could not be opened, with why.
    refused: Vec<String>,
    /// The job showing the folder picker, while it is open.
    picking: Option<JobId>,
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
        self.opening.folder = folder.to_owned();
        self.opening.locale = locale;
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
        let tables: Arc<dyn Formats> = self.files.clone();
        reg.provide(formats::SERVICE, tables);
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
            // The picker waits for the user on a job, the interface going on meanwhile.
            if ui
                .add_enabled(self.picking.is_none(), egui::Button::new("Open"))
                .clicked()
            {
                // The picker of Windows starts in a folder only when its separators are its own.
                let start = PathBuf::from(self.folder.trim().replace('/', "\\"));
                self.picking = Some(ctx.spawn("Choose the folder of the client", move |_| {
                    let picker = rfd::FileDialog::new().set_title("Folder of the client");
                    let picker = if start.is_dir() {
                        picker.set_directory(&start)
                    } else {
                        picker
                    };
                    picker.pick_folder()
                }));
            }
        });
        match self.files.state() {
            VfsState::NoClient(reason) => ui.weak(reason),
            VfsState::Opening => {
                let left = self.opening.jobs.len() + usize::from(self.opening.index.is_some());
                ui.label(format!("Opening the archives: {left} jobs left"))
            }
            VfsState::Ready { archives, files } => {
                let ids = self.files.client().map(|client| client.file_ids.len()).unwrap_or(0);
                ui.label(format!(
                    "{archives} archives, {files} files listed, {ids} FileDataIDs named"
                ))
            }
        };
        for reason in &self.refused {
            ui.colored_label(ui.visuals().warn_fg_color, reason);
        }
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if self.picking == Some(job) {
            self.picking = None;
            if let Some(Some(folder)) = outcome.take::<Option<PathBuf>>() {
                self.folder = folder.display().to_string();
                ctx.set_setting(CLIENT_FOLDER, serde_json::json!(self.folder));
                self.open(&folder, ctx);
            }
        } else if let Some(place) = self.opening.jobs.remove(&job) {
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
                let folder = std::mem::take(&mut self.opening.folder);
                let locale = std::mem::take(&mut self.opening.locale);
                self.opening.index = Some(ctx.spawn("Index the client's files", move |_| {
                    Client::open(sources, &folder, &locale)
                }));
            }
        } else if self.opening.index == Some(job) {
            self.opening.index = None;
            if let Some((client, refused)) = outcome.take::<(Client, Vec<String>)>() {
                for reason in &refused {
                    log::warn!("a table of paths of the client is left out: {reason}");
                }
                self.refused.extend(refused);
                self.files.set(FilesState::Ready(Arc::new(client)));
            }
        }
    }
}

uniwow_api::export_module!(AssetsModule::default());
