//! The files of the client: its archives, in the order the client reads them, with the patches
//! WarcraftXL adds, read from any thread at once through the service `vfs`, which also turns a
//! FileDataID into a path as WarcraftXL does; their formats parsed through the service `formats`.
//! The archives are opened by jobs of the pool, one each, their lists merged and the tables of
//! paths read by another; the interface thread only hands the client over. Its panel chooses the
//! folder of the client in the folder picker of the system and says how far its files are.

mod animation;
mod blp;
mod chain;
mod db2;
mod dbc;
mod m2;
#[cfg(test)]
mod m2_tests;
mod mpq;
#[cfg(test)]
mod table_tests;
mod terrain;
#[cfg(test)]
mod terrain_tests;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::ThreadId;

use uniwow_api::formats::{
    self, AnimationRecord, AreaRecord, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair, FileRef,
    Formats, GameObjectDisplay, HairGeoset, MapRecord, Model, Placements, Texture, Tile, Wdl, Wdt,
};
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
    /// The WDT of each map read, by its folder in lower case.
    wdts: Mutex<HashMap<String, Arc<Wdt>>>,
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
            wdts: Mutex::default(),
        };
        (client, refused)
    }

    fn wdt(&self, directory: &str) -> Result<Arc<Wdt>, String> {
        let key = directory.to_ascii_lowercase();
        if let Some(wdt) = self.wdts.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Ok(wdt.clone());
        }
        let path = format!("World\\Maps\\{directory}\\{directory}.wdt");
        let bytes = self
            .chain
            .read(&path)?
            .ok_or_else(|| format!("{path}: not in the client"))?;
        let wdt = Arc::new(terrain::wdt(&bytes).map_err(|e| format!("{path}: {e}"))?);
        self.wdts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, wdt.clone());
        Ok(wdt)
    }

    fn tile(&self, directory: &str, x: u32, y: u32) -> Result<Option<Tile>, String> {
        let wdt = self.wdt(directory)?;
        if x > 63 || y > 63 || !wdt.tiles[(y * 64 + x) as usize] {
            return Ok(None);
        }
        let stem = format!("World\\Maps\\{directory}\\{directory}_{x}_{y}");
        let read = |path: &str| self.chain.read(path).map_err(|e| format!("{path}: {e}"));
        let root_path = format!("{stem}.adt");
        let root = read(&root_path)?.ok_or_else(|| format!("{root_path}: named by its WDT, not in the client"))?;
        // Split, as WarcraftXL loads it, when its `_tex0` exists.
        let tex = read(&format!("{stem}_tex0.adt"))?;
        let obj = match tex {
            Some(_) => read(&format!("{stem}_obj0.adt"))?,
            None => None,
        };
        terrain::tile(&root, tex.as_deref(), obj.as_deref(), wdt.flags)
            .map(Some)
            .map_err(|e| format!("{root_path}: {e}"))
    }

    fn placements(&self, directory: &str, x: u32, y: u32) -> Result<Option<Placements>, String> {
        let wdt = self.wdt(directory)?;
        if x > 63 || y > 63 || !wdt.tiles[(y * 64 + x) as usize] {
            return Ok(None);
        }
        let stem = format!("World\\Maps\\{directory}\\{directory}_{x}_{y}");
        let read = |path: &str| self.chain.read(path).map_err(|e| format!("{path}: {e}"));
        // Split as `tile` reads it, when its `_tex0` exists: its placements in its `_obj0`.
        if self.chain.exists(&format!("{stem}_tex0.adt")) {
            let obj_path = format!("{stem}_obj0.adt");
            let obj = read(&obj_path)?;
            return terrain::placements(&[], Some(obj.as_deref().unwrap_or_default()))
                .map(Some)
                .map_err(|e| format!("{obj_path}: {e}"));
        }
        let root_path = format!("{stem}.adt");
        let root = read(&root_path)?.ok_or_else(|| format!("{root_path}: named by its WDT, not in the client"))?;
        terrain::placements(&root, None)
            .map(Some)
            .map_err(|e| format!("{root_path}: {e}"))
    }

    fn wdl(&self, directory: &str) -> Result<Option<Wdl>, String> {
        let path = format!("World\\Maps\\{directory}\\{directory}.wdl");
        match self.chain.read(&path)? {
            Some(bytes) => terrain::wdl(&bytes).map(Some).map_err(|e| format!("{path}: {e}")),
            None => Ok(None),
        }
    }

    fn path(&self, file: &FileRef) -> Result<String, String> {
        match file {
            FileRef::Path(path) => Ok(path.clone()),
            FileRef::Id(id) => self
                .file_ids
                .path_of(*id)
                .ok_or_else(|| format!("the FileDataID {id}: named by no table of paths")),
        }
    }

    fn texture(&self, file: &FileRef, decode: bool) -> Result<Texture, String> {
        let path = self.path(file)?;
        let bytes = self
            .chain
            .read(&path)?
            .ok_or_else(|| format!("{path}: not in the client"))?;
        blp::texture(&bytes, decode).map_err(|e| format!("{path}: {e}"))
    }

    /// The model `file`, a path of a table read as an `.m2`, its skins beside it or named by
    /// FileDataID.
    fn model(&self, file: &FileRef) -> Result<Model, String> {
        let path = m2::path(&self.path(file)?);
        let bytes = self
            .chain
            .read(&path)?
            .ok_or_else(|| format!("{path}: not in the client"))?;
        m2::read(&bytes, &path, |skin| {
            // By its FileDataID first, then by the name of the model.
            let named = skin.id.and_then(|id| self.file_ids.path_of(id)).unwrap_or(skin.path);
            self.chain
                .read(&named)?
                .ok_or_else(|| format!("{named}: not in the client"))
        })
        .map_err(|e| format!("{path}: {e}"))
    }
}

/// The services `vfs` and `formats`: the client once open, given whole to its readers, who read
/// without a lock.
#[derive(Default)]
struct Files {
    state: RwLock<FilesState>,
    /// The interface thread, which a table read would hold up.
    interface: OnceLock<ThreadId>,
    /// Whether a table was asked from the interface thread, which is said once.
    warned: AtomicBool,
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

impl Files {
    /// In debug, warns once that `what` was asked from the interface thread, which waits while it
    /// is read.
    fn check_thread(&self, what: &str) {
        if cfg!(debug_assertions)
            && self.interface.get() == Some(&std::thread::current().id())
            && !self.warned.swap(true, Ordering::Relaxed)
        {
            log::warn!("formats: {what} asked from the interface thread, which waits while it is read: ask from a job");
        }
    }
}

impl Formats for Files {
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String> {
        self.check_thread("Map.dbc");
        let client = self.client()?;
        client.tables.maps(&client.chain)
    }

    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String> {
        self.check_thread("AreaTable.dbc");
        let client = self.client()?;
        client.tables.areas(&client.chain)
    }

    fn creature_displays(&self) -> Result<Arc<Vec<CreatureDisplay>>, String> {
        self.check_thread("CreatureDisplayInfo.dbc");
        let client = self.client()?;
        client.tables.creature_displays(&client.chain)
    }

    fn creature_models(&self) -> Result<Arc<Vec<CreatureModel>>, String> {
        self.check_thread("CreatureModelData.dbc");
        let client = self.client()?;
        client.tables.creature_models(&client.chain)
    }

    fn creature_looks(&self) -> Result<Arc<Vec<CreatureLook>>, String> {
        self.check_thread("CreatureDisplayInfoExtra.dbc");
        let client = self.client()?;
        client.tables.creature_looks(&client.chain)
    }

    fn hair_geosets(&self) -> Result<Arc<Vec<HairGeoset>>, String> {
        self.check_thread("CharHairGeosets.dbc");
        let client = self.client()?;
        client.tables.hair_geosets(&client.chain)
    }

    fn facial_hairs(&self) -> Result<Arc<Vec<FacialHair>>, String> {
        self.check_thread("CharacterFacialHairStyles.dbc");
        let client = self.client()?;
        client.tables.facial_hairs(&client.chain)
    }

    fn game_object_displays(&self) -> Result<Arc<Vec<GameObjectDisplay>>, String> {
        self.check_thread("GameObjectDisplayInfo.dbc");
        let client = self.client()?;
        client.tables.game_object_displays(&client.chain)
    }

    fn char_sections(&self) -> Result<Arc<Vec<CharSection>>, String> {
        self.check_thread("CharSections.dbc");
        let client = self.client()?;
        client.tables.char_sections(&client.chain)
    }

    fn animations(&self) -> Result<Arc<Vec<AnimationRecord>>, String> {
        self.check_thread("AnimationData.dbc");
        let client = self.client()?;
        client.tables.animations(&client.chain)
    }

    fn model(&self, file: &FileRef) -> Result<Model, String> {
        self.check_thread("a model");
        self.client()?.model(file)
    }

    fn wdt(&self, directory: &str) -> Result<Arc<Wdt>, String> {
        self.check_thread("a WDT");
        self.client()?.wdt(directory)
    }

    fn tile(&self, directory: &str, x: u32, y: u32) -> Result<Option<Tile>, String> {
        self.check_thread("a tile");
        self.client()?.tile(directory, x, y)
    }

    fn placements(&self, directory: &str, x: u32, y: u32) -> Result<Option<Placements>, String> {
        self.check_thread("the placements of a tile");
        self.client()?.placements(directory, x, y)
    }

    fn wdl(&self, directory: &str) -> Result<Option<Wdl>, String> {
        self.check_thread("a WDL");
        self.client()?.wdl(directory)
    }

    fn texture(&self, file: &FileRef) -> Result<Texture, String> {
        self.check_thread("a texture");
        self.client()?.texture(file, false)
    }

    fn texture_rgba(&self, file: &FileRef) -> Result<Texture, String> {
        self.check_thread("a texture");
        self.client()?.texture(file, true)
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
        let _ = self.files.interface.set(std::thread::current().id());
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
            // The picker waits for the user on a thread of its own (T2), the interface going on.
            if ui
                .add_enabled(self.picking.is_none(), egui::Button::new("Open"))
                .clicked()
            {
                // The picker of Windows starts in a folder only when its separators are its own.
                let start = PathBuf::from(self.folder.trim().replace('/', "\\"));
                self.picking = Some(ctx.spawn_thread("Choose the folder of the client", move |_| {
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
