use std::collections::BTreeSet;
use std::ffi::CStr;
use std::path::{Path, PathBuf};

use crate::capi;
use uniwow_api::{CREATE_SYMBOL, CreateFn, MenuItemSpec, Module, PACKAGE_SYMBOL, PackageFn, PanelSpec, RUNTIME_DLL};

use crate::compiled;
use crate::guard::guarded_as;
use crate::manifest::{self, Kind, Manifest};

pub enum State {
    Running,
    /// Turned off in the Modules panel; not loaded.
    Disabled,
    /// Folder without a usable manifest or DLL.
    Ignored(String),
    /// Not loaded: incompatible or unreadable DLL.
    Refused(String),
    /// A required service is missing.
    Blocked(String),
    /// Panicked while loading or running.
    Failed(String),
}

impl State {
    pub fn is_running(&self) -> bool {
        matches!(self, State::Running)
    }
}

pub struct Slot {
    /// Module id from the manifest, or the folder name when there is no manifest.
    pub id: String,
    pub folder: PathBuf,
    pub manifest: Option<Manifest>,
    pub state: State,
    pub module: Option<Box<dyn Module>>,
    pub panels: Vec<PanelSpec>,
    pub menu_items: Vec<MenuItemSpec>,
    pub subscriptions: Vec<String>,
    /// The commands it declared, with the reason of those the catalogue set aside (F6).
    pub commands: Vec<(String, Option<String>)>,
    /// For a compiled module, what the C interface knows of it: its thread and the work there.
    pub compiled: Option<&'static capi::ModuleContext>,
}

impl Slot {
    /// A running module given as it is, without a folder nor a manifest, for the tests.
    #[cfg(test)]
    pub fn loaded(id: &str, module: Box<dyn Module>) -> Self {
        let mut slot = Self::new(id.to_owned(), PathBuf::new(), None, State::Running);
        slot.module = Some(module);
        slot
    }

    /// A compiled module already started, for the tests.
    #[cfg(test)]
    pub fn compiled(id: &str, module: compiled::CompiledModule) -> Self {
        let mut slot = Self::new(id.to_owned(), PathBuf::new(), None, State::Running);
        slot.compiled = Some(module.context());
        slot.module = Some(Box::new(module));
        slot
    }

    fn new(id: String, folder: PathBuf, manifest: Option<Manifest>, state: State) -> Self {
        Self {
            id,
            folder,
            manifest,
            state,
            module: None,
            panels: Vec::new(),
            menu_items: Vec::new(),
            subscriptions: Vec::new(),
            commands: Vec::new(),
            compiled: None,
        }
    }

    pub fn name(&self) -> &str {
        self.manifest.as_ref().map_or(&self.id, |m| &m.name)
    }

    pub fn subscribed_to(&self, topic: &str) -> bool {
        self.subscriptions.iter().any(|s| s == "*" || s == topic)
    }
}

pub struct Discovery {
    pub runtime_fingerprint: Option<String>,
    pub slots: Vec<Slot>,
}

fn subfolders(folder: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// The folders of the modules in `root`, sorted: each subfolder, but for a group folder, one
/// without a manifest whose subfolders hold manifests (such as `UI`), the subfolders of the group.
fn module_folders(root: &Path) -> Vec<PathBuf> {
    let has_manifest = |folder: &Path| folder.join(manifest::FILE_NAME).exists();
    let mut folders = Vec::new();
    for folder in subfolders(root) {
        let inner = subfolders(&folder);
        if !has_manifest(&folder) && inner.iter().any(|f| has_manifest(f)) {
            folders.extend(inner);
        } else {
            folders.push(folder);
        }
    }
    folders.sort();
    folders
}

/// Scans `<exe dir>\modules`, and its group folders, checks every module against the runtime and
/// loads the valid ones.
pub fn discover(exe_dir: &Path, disabled: &BTreeSet<String>) -> Discovery {
    let runtime_fingerprint = manifest::hash_file(&exe_dir.join(RUNTIME_DLL)).ok();
    let shadow_dir = prepare_shadow_dir();
    let mut slots: Vec<Slot> = Vec::new();

    for folder in module_folders(&exe_dir.join("modules")) {
        let folder_name = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let manifest = match Manifest::read(&folder.join(manifest::FILE_NAME)) {
            Ok(m) => m,
            Err(e) => {
                let reason = if folder.join(manifest::FILE_NAME).exists() {
                    format!("invalid {}: {e}", manifest::FILE_NAME)
                } else {
                    format!("no {}", manifest::FILE_NAME)
                };
                slots.push(Slot::new(folder_name, folder, None, State::Ignored(reason)));
                continue;
            }
        };
        let id = manifest.id.clone();
        let dll = folder.join(&manifest.dll);
        let state = if !dll.exists() {
            State::Ignored(format!("missing {}", manifest.dll))
        } else if id_taken(&slots, &id) {
            State::Ignored(format!("another folder already provides the id '{id}'"))
        } else if disabled.contains(&id) {
            State::Disabled
        } else if manifest.kind == Kind::Rust && runtime_fingerprint.as_deref() != Some(manifest.runtime.as_str()) {
            State::Refused("built for another runtime: rebuild it".to_owned())
        } else {
            State::Running
        };
        let kind = manifest.kind;
        let mut slot = Slot::new(id, folder, Some(manifest), state);
        if slot.state.is_running() {
            let loaded = match kind {
                Kind::Rust => load(&dll, &shadow_dir, &slot),
                Kind::Compiled => compiled::load(&dll, &slot.id)
                    .map(|module| {
                        slot.compiled = Some(module.context());
                        Box::new(module) as Box<dyn Module>
                    })
                    .map_err(State::Refused),
            };
            match loaded {
                Ok(module) => slot.module = Some(module),
                Err(state) => slot.state = state,
            }
        }
        slots.push(slot);
    }

    Discovery {
        runtime_fingerprint,
        slots,
    }
}

/// Whether a folder found before, among `slots`, provides the id `id`: only a module loaded keeps
/// its id, never a folder ignored, refused or disabled.
fn id_taken(slots: &[Slot], id: &str) -> bool {
    slots.iter().any(|slot| slot.id == id && slot.state.is_running())
}

/// Loads a copy of the DLL, so that the original can be rebuilt while the editor runs.
fn load(dll: &Path, shadow_dir: &Path, slot: &Slot) -> Result<Box<dyn Module>, State> {
    let manifest = slot.manifest.as_ref().expect("checked by the caller");
    // Prefixed so that a module named like a system DLL (version, dbghelp…) cannot be confused with it.
    let copy = shadow_dir.join(format!("uniwow-module-{}.dll", slot.id));
    std::fs::copy(dll, &copy).map_err(|e| State::Refused(format!("could not copy the DLL: {e}")))?;
    // The copy is what gets loaded, so it is the copy that must match the manifest.
    if manifest::hash_file(&copy).ok().as_deref() != Some(manifest.dll_hash.as_str()) {
        return Err(State::Refused(format!(
            "{} does not match its {}: rebuild it",
            manifest.dll,
            manifest::FILE_NAME
        )));
    }

    // SAFETY: the DLL was built against this exact runtime (fingerprint checked by the caller).
    let library =
        unsafe { libloading::Library::new(&copy) }.map_err(|e| State::Refused(format!("could not be loaded: {e}")))?;

    // SAFETY: C function returning a static NUL-terminated string, exported by `export_module!`.
    let package = unsafe {
        let package: libloading::Symbol<PackageFn> = library
            .get(PACKAGE_SYMBOL)
            .map_err(|_| State::Refused("not a UniWoW module (no entry point)".to_owned()))?;
        CStr::from_ptr(package().cast()).to_string_lossy().into_owned()
    };
    if package != manifest.package {
        return Err(State::Refused(format!(
            "the DLL is the package '{package}', the manifest expects '{}'",
            manifest.package
        )));
    }

    // SAFETY: same runtime, so the Rust ABI of the entry point matches.
    let create: CreateFn = unsafe {
        *library
            .get::<CreateFn>(CREATE_SYMBOL)
            .map_err(|_| State::Refused("not a UniWoW module (no entry point)".to_owned()))?
    };
    // A loaded module is never unloaded: its code must outlive every object it created.
    std::mem::forget(library);

    guarded_as(&slot.id, create).map_err(State::Failed)
}

/// `%TEMP%\UniWoW\<process id>`, after removing the copies left by editors no longer running.
fn prepare_shadow_dir() -> PathBuf {
    let root = std::env::temp_dir().join("UniWoW");
    let own = root.join(std::process::id().to_string());
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.path() != own {
                // Fails on the folders of running editors, whose DLLs are locked; that is intended.
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let _ = std::fs::create_dir_all(&own);
    own
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use uniwow_api::RUNTIME_DLL;

    use super::{Slot, State, discover, id_taken};
    use crate::manifest::{self, FILE_NAME};

    /// A folder standing for the executable's, with a runtime DLL of made-up bytes and an empty
    /// `modules`; removed when dropped.
    struct ExeDir(PathBuf);

    impl ExeDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("uniwow-loader-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join("modules")).expect("modules folder");
            std::fs::write(path.join(RUNTIME_DLL), b"runtime").expect("runtime");
            Self(path)
        }

        fn runtime(&self) -> String {
            manifest::hash_file(&self.0.join(RUNTIME_DLL)).expect("runtime hash")
        }

        /// The folder `modules\<folder>` of a Rust module `id` built against `runtime`, its
        /// manifest giving `dll_hash`, or the hash of its DLL; its DLL holding `dll`, or absent.
        fn module(&self, folder: &str, id: &str, runtime: &str, dll: Option<&[u8]>, dll_hash: Option<&str>) {
            let folder = self.0.join("modules").join(folder);
            std::fs::create_dir_all(&folder).expect("module folder");
            let file = folder.join(format!("{id}.dll"));
            if let Some(bytes) = dll {
                std::fs::write(&file, bytes).expect("dll");
            }
            let hash = match dll_hash {
                Some(hash) => hash.to_owned(),
                None => manifest::hash_file(&file).unwrap_or_default(),
            };
            let text = format!(
                "id = \"{id}\"\nkind = \"rust\"\npackage = \"uniwow-module-{id}\"\nname = \"{id}\"\nversion = \"1\"\n\
                 dll = \"{id}.dll\"\ndll_hash = \"{hash}\"\nruntime = \"{runtime}\"\n"
            );
            std::fs::write(folder.join(FILE_NAME), text).expect("manifest");
        }

        fn folder(&self, path: &str) -> PathBuf {
            let folder = self.0.join("modules").join(path);
            std::fs::create_dir_all(&folder).expect("folder");
            folder
        }

        fn discover(&self, disabled: &[&str]) -> Vec<(String, String)> {
            let disabled: BTreeSet<String> = disabled.iter().map(|id| (*id).to_owned()).collect();
            discover(&self.0, &disabled).slots.iter().map(described).collect()
        }
    }

    impl Drop for ExeDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The id of a slot and its state, with the reason.
    fn described(slot: &Slot) -> (String, String) {
        let state = match &slot.state {
            State::Running => "running".to_owned(),
            State::Disabled => "disabled".to_owned(),
            State::Ignored(reason) => format!("ignored: {reason}"),
            State::Refused(reason) => format!("refused: {reason}"),
            State::Blocked(reason) => format!("blocked: {reason}"),
            State::Failed(reason) => format!("failed: {reason}"),
        };
        (slot.id.clone(), state)
    }

    fn state_of<'a>(slots: &'a [(String, String)], id: &str) -> Vec<&'a str> {
        slots
            .iter()
            .filter(|(slot, _)| slot == id)
            .map(|(_, state)| state.as_str())
            .collect()
    }

    #[test]
    fn a_folder_without_a_usable_manifest_or_dll_is_ignored_with_the_reason() {
        let exe = ExeDir::new("ignored");
        exe.folder("bare");
        std::fs::write(
            exe.folder("lua").join(FILE_NAME),
            "id = \"lua\"\nkind = \"lua\"\nname = \"L\"\nversion = \"1\"\ndll = \"lua.dll\"\n",
        )
        .expect("manifest");
        exe.module("missing", "missing", &exe.runtime(), None, Some("0"));
        let slots = exe.discover(&[]);
        assert_eq!(state_of(&slots, "bare"), ["ignored: no module.toml"]);
        let lua = state_of(&slots, "lua");
        assert!(
            lua.len() == 1 && lua[0].starts_with("ignored: invalid module.toml"),
            "{lua:?}"
        );
        assert_eq!(state_of(&slots, "missing"), ["ignored: missing missing.dll"]);
    }

    #[test]
    fn a_module_built_for_another_runtime_or_whose_dll_changed_is_refused() {
        let exe = ExeDir::new("refused");
        exe.module("other", "other", "0000", Some(b"dll"), None);
        exe.module("changed", "changed", &exe.runtime(), Some(b"dll"), Some("0000"));
        exe.module("unreadable", "unreadable", &exe.runtime(), Some(b"not a dll"), None);
        let slots = exe.discover(&[]);
        assert_eq!(
            state_of(&slots, "other"),
            ["refused: built for another runtime: rebuild it"]
        );
        assert_eq!(
            state_of(&slots, "changed"),
            ["refused: changed.dll does not match its module.toml: rebuild it"]
        );
        let unreadable = state_of(&slots, "unreadable");
        assert!(
            unreadable.len() == 1 && unreadable[0].starts_with("refused: could not be loaded"),
            "{unreadable:?}"
        );
    }

    #[test]
    fn a_disabled_module_is_not_loaded() {
        let exe = ExeDir::new("disabled");
        exe.module("off", "off", &exe.runtime(), Some(b"dll"), None);
        assert_eq!(state_of(&exe.discover(&["off"]), "off"), ["disabled"]);
    }

    #[test]
    fn a_folder_ignored_refused_or_disabled_keeps_no_id() {
        let exe = ExeDir::new("twice");
        // Sorted by folder: `a` is ignored for want of its DLL, `b` refused, so `c` is tried; its
        // DLL, made up, cannot be loaded.
        exe.module("a", "m", &exe.runtime(), None, Some("0"));
        exe.module("b", "m", "0000", Some(b"dll"), None);
        exe.module("c", "m", &exe.runtime(), Some(b"dll"), None);
        let slots = exe.discover(&[]);
        let states = state_of(&slots, "m");
        assert_eq!(
            states[..2],
            [
                "ignored: missing m.dll",
                "refused: built for another runtime: rebuild it"
            ]
        );
        assert!(states[2].starts_with("refused: could not be loaded"), "{states:?}");
    }

    #[test]
    fn only_a_module_loaded_keeps_its_id() {
        let slot = |id: &str, state| Slot::new(id.to_owned(), PathBuf::new(), None, state);
        let found = [
            slot("ignored", State::Ignored(String::new())),
            slot("refused", State::Refused(String::new())),
            slot("disabled", State::Disabled),
            slot("loaded", State::Running),
        ];
        for id in ["ignored", "refused", "disabled", "unknown"] {
            assert!(!id_taken(&found, id), "{id}");
        }
        assert!(id_taken(&found, "loaded"));
    }

    #[test]
    fn the_modules_of_a_group_folder_are_found_whatever_its_name() {
        let exe = ExeDir::new("groups");
        exe.module(
            &Path::new("UI").join("ui-one").to_string_lossy(),
            "ui-one",
            "0000",
            Some(b"dll"),
            None,
        );
        exe.module(
            &Path::new("Tools").join("tool").to_string_lossy(),
            "tool",
            "0000",
            Some(b"dll"),
            None,
        );
        let slots = exe.discover(&[]);
        let ids: Vec<&str> = slots.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["tool", "ui-one"], "Tools before UI, by path");
    }
}
