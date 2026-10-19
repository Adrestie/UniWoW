//! The project (milestone 15, step 15.7): a folder the user chooses, holding `uniwow-project.json`,
//! a section a module, each written and read by its module alone (`Module::project_section`,
//! `Module::open_project`). The kernel writes the file whole, atomically (a temporary file renamed
//! over the last), its keys in order and two spaces of indent, so that two versions differ only where
//! the project did. Without one, an untitled project in memory, saved into a folder chosen when first
//! saved. Saved every minute once it has a folder.

use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;

use super::*;

/// The name of the file of a project in its folder.
pub(super) const FILE: &str = "uniwow-project.json";
/// The version of its format.
const FORMAT: u64 = 1;
/// How often a project with a folder is saved when it changed, and how often the kernel looks
/// whether it changed.
const AUTOSAVE: Duration = Duration::from_secs(60);
const LOOKED: Duration = Duration::from_secs(1);
/// The projects opened last the File menu offers.
const RECENT: usize = 8;

/// What the folder picker of the system was opened for.
pub(super) enum Picking {
    Open,
    SaveAs,
}

pub(super) struct Project {
    /// Its folder; none while untitled.
    pub folder: Option<PathBuf>,
    /// Its sections as last opened or saved, written as the file holds them.
    pub saved: String,
    /// Whether its sections differ from those saved, as last looked, and when.
    pub dirty: bool,
    looked: Instant,
    saved_at: Instant,
    /// The folder picker open, on a thread of its own.
    pub picking: Option<(Picking, Receiver<Option<PathBuf>>)>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            folder: None,
            saved: String::new(),
            dirty: false,
            looked: Instant::now(),
            saved_at: Instant::now(),
            picking: None,
        }
    }
}

impl Project {
    /// Its name: that of its folder, *Untitled* without one.
    pub fn name(&self) -> String {
        self.folder
            .as_ref()
            .and_then(|folder| folder.file_name())
            .map_or_else(|| "Untitled".to_owned(), |name| name.to_string_lossy().into_owned())
    }
}

/// The folder the pickers open in: `Documents\UniWoW\Projects`, made when first asked.
pub(super) fn default_folder() -> Option<PathBuf> {
    let documents = std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Documents"))?;
    let folder = documents.join("UniWoW").join("Projects");
    std::fs::create_dir_all(&folder).ok()?;
    Some(folder)
}

/// The text of the file of the project `name` holding `sections`.
pub(super) fn text(name: &str, sections: &BTreeMap<String, serde_json::Value>) -> String {
    let file = serde_json::json!({ "format": FORMAT, "name": name, "sections": sections });
    let mut text = serde_json::to_string_pretty(&file).unwrap_or_default();
    text.push('\n');
    text
}

/// Writes `text` as the file of the project in `folder`, through a temporary file renamed over the
/// last.
pub(super) fn write(folder: &Path, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(FILE);
    let temporary = folder.join(format!("{FILE}.tmp"));
    std::fs::write(&temporary, text).map_err(|e| format!("{}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The sections of the file of the project in `folder`.
pub(super) fn read(folder: &Path) -> Result<BTreeMap<String, serde_json::Value>, String> {
    let path = folder.join(FILE);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if file["format"].as_u64().is_some_and(|format| format > FORMAT) {
        return Err(format!(
            "{}: made by a later editor (format {})",
            path.display(),
            file["format"]
        ));
    }
    Ok(file["sections"]
        .as_object()
        .map(|sections| {
            sections
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default())
}

impl Shell {
    /// The section of each running module that keeps one.
    pub(super) fn project_sections(&mut self) -> BTreeMap<String, serde_json::Value> {
        let mut sections = BTreeMap::new();
        for index in 0..self.slots.len() {
            if !self.slots[index].state.is_running() {
                continue;
            }
            let id = self.slots[index].id.clone();
            match call_module(&mut self.slots[index], &mut self.host, |f, _| f.project_section()) {
                Ok(Some(section)) => {
                    sections.insert(id, section);
                }
                Ok(None) => {}
                Err(message) => self.fail(index, format!("its section of the project: {message}")),
            }
        }
        sections
    }

    /// Gives each running module its section of `sections`, none for a new project; the history
    /// emptied, its changes belonging to the project left.
    fn open_sections(&mut self, sections: &BTreeMap<String, serde_json::Value>) {
        for index in 0..self.slots.len() {
            if !self.slots[index].state.is_running() {
                continue;
            }
            let section = sections.get(&self.slots[index].id).cloned();
            if let Err(message) = call_module(&mut self.slots[index], &mut self.host, |f, ctx| {
                f.open_project(section, ctx)
            }) {
                self.fail(index, format!("opening the project: {message}"));
            }
        }
        self.apply_pending();
        self.history = History::default();
    }

    /// A new project, untitled.
    pub(super) fn new_project(&mut self) {
        self.open_sections(&BTreeMap::new());
        self.project.folder = None;
        self.project.saved = text(&self.project.name(), &self.project_sections());
        self.project.dirty = false;
    }

    /// Opens the project in `folder`.
    pub(super) fn open_project(&mut self, folder: PathBuf) {
        match read(&folder) {
            Ok(sections) => {
                self.open_sections(&sections);
                self.project.folder = Some(folder.clone());
                self.project.saved = text(&self.project.name(), &self.project_sections());
                self.project.dirty = false;
                self.remember_project(&folder);
                self.host.notify(
                    KERNEL,
                    Level::Info,
                    &format!("Project '{}' opened", self.project.name()),
                );
            }
            Err(error) => {
                log::error!("the project is not opened: {error}");
                self.host
                    .notify(KERNEL, Level::Error, &format!("The project is not opened: {error}"));
            }
        }
    }

    /// Saves the project into `folder`, its folder from then on; an error is said.
    pub(super) fn save_project_to(&mut self, folder: PathBuf) -> Result<(), String> {
        let name = folder
            .file_name()
            .map_or_else(|| "Project".to_owned(), |name| name.to_string_lossy().into_owned());
        let text = text(&name, &self.project_sections());
        write(&folder, &text)?;
        self.project.folder = Some(folder.clone());
        self.project.saved = text;
        self.project.dirty = false;
        self.project.saved_at = Instant::now();
        self.remember_project(&folder);
        Ok(())
    }

    /// Saves the project: into its folder, else into a folder chosen first.
    pub(super) fn save_project(&mut self) {
        match self.project.folder.clone() {
            Some(folder) => {
                if let Err(error) = self.save_project_to(folder) {
                    log::error!("the project is not saved: {error}");
                    self.host
                        .notify(KERNEL, Level::Error, &format!("The project is not saved: {error}"));
                }
            }
            None => self.pick_folder(Picking::SaveAs),
        }
    }

    /// Opens the folder picker of the system on a thread of its own, its answer taken at a frame to
    /// come.
    pub(super) fn pick_folder(&mut self, picking: Picking) {
        if self.project.picking.is_some() {
            return;
        }
        let (answer, answered) = std::sync::mpsc::channel();
        let start = default_folder();
        let title = match picking {
            Picking::Open => "Open a project: its folder",
            Picking::SaveAs => "Save the project into a folder",
        };
        std::thread::spawn(move || {
            let mut dialog = uniwow_api::rfd::FileDialog::new().set_title(title);
            if let Some(start) = start {
                dialog = dialog.set_directory(start);
            }
            let _ = answer.send(dialog.pick_folder());
        });
        self.project.picking = Some((picking, answered));
    }

    fn remember_project(&mut self, folder: &Path) {
        let recent = &mut self.host.settings.recent_projects;
        recent.retain(|kept| kept != folder);
        recent.insert(0, folder.to_path_buf());
        recent.truncate(RECENT);
        self.host.settings_changed = true;
    }

    /// At each frame: the folder chosen, if one was; Ctrl+S; whether the project changed, once a
    /// second; saved every minute once it has a folder.
    pub(super) fn steer_project(&mut self, ctx: &egui::Context) {
        if let Some((_, answered)) = &self.project.picking
            && let Ok(chosen) = answered.try_recv()
        {
            let (picking, _) = self.project.picking.take().expect("taken above");
            if let Some(folder) = chosen {
                match picking {
                    Picking::Open => self.open_project(folder),
                    Picking::SaveAs => {
                        if let Err(error) = self.save_project_to(folder) {
                            log::error!("the project is not saved: {error}");
                            self.host
                                .notify(KERNEL, Level::Error, &format!("The project is not saved: {error}"));
                        }
                    }
                }
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save_project();
        }
        if self.project.looked.elapsed() >= LOOKED {
            self.project.looked = Instant::now();
            let now = text(&self.project.name(), &self.project_sections());
            if self.project.saved.is_empty() {
                // The modules' state at start: what an untitled project holds unchanged.
                self.project.saved = now;
            } else {
                self.project.dirty = now != self.project.saved;
            }
        }
        if self.project.dirty
            && self.project.saved_at.elapsed() >= AUTOSAVE
            && let Some(folder) = self.project.folder.clone()
        {
            self.project.saved_at = Instant::now();
            if let Err(error) = self.save_project_to(folder) {
                log::warn!("the project was not saved on its own: {error}");
            }
        }
    }

    /// The File menu of the project.
    pub(super) fn project_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("New project").clicked() {
            self.new_project();
            ui.close();
        }
        if ui.button("Open project…").clicked() {
            self.pick_folder(Picking::Open);
            ui.close();
        }
        let recent = self.host.settings.recent_projects.clone();
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            ui.menu_button("Open recent", |ui| {
                for folder in recent {
                    if ui.button(folder.display().to_string()).clicked() {
                        self.open_project(folder);
                        ui.close();
                    }
                }
            });
        });
        let dirty = if self.project.dirty { " •" } else { "" };
        if ui
            .add(egui::Button::new(format!("Save project '{}'{dirty}", self.project.name())).shortcut_text("Ctrl+S"))
            .clicked()
        {
            self.save_project();
            ui.close();
        }
        if ui.button("Save project as…").clicked() {
            self.pick_folder(Picking::SaveAs);
            ui.close();
        }
        ui.separator();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_of_a_project_is_written_in_order_atomically_and_read_back() {
        let folder = std::env::temp_dir().join(format!("uniwow-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let sections = BTreeMap::from([
            (
                "scene".to_owned(),
                serde_json::json!({ "names": { "doodad:0:7": "The old oak" } }),
            ),
            ("doodads".to_owned(), serde_json::json!({ "moved": [] })),
        ]);
        let written = text("Elwynn", &sections);
        write(&folder, &written).unwrap();
        assert!(
            !folder.join(format!("{FILE}.tmp")).exists(),
            "the temporary file renamed"
        );
        assert_eq!(read(&folder).unwrap(), sections);
        // The same sections, the same text: keys in order.
        assert_eq!(text("Elwynn", &sections), written);
        assert!(written.find("\"doodads\"").unwrap() < written.find("\"scene\"").unwrap());
        std::fs::write(folder.join(FILE), r#"{ "format": 9, "sections": {} }"#).unwrap();
        assert!(read(&folder).unwrap_err().contains("later editor"));
        assert!(read(&folder.join("none")).is_err());
        let _ = std::fs::remove_dir_all(&folder);
    }
}
