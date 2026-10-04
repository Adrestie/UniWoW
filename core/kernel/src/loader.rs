use std::collections::BTreeSet;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use uniwow_api::capi;
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
    /// What waits for or runs on its thread, for a compiled module.
    pub activity: Option<Arc<capi::Activity>>,
}

impl Slot {
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
            activity: None,
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

/// Folders of `modules\` that hold modules of one family, such as those of the interface.
const GROUPS: [&str; 1] = ["UI"];

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

/// Scans `<exe dir>\modules`, and its group folders, checks every module against the runtime and
/// loads the valid ones.
pub fn discover(exe_dir: &Path, disabled: &BTreeSet<String>) -> Discovery {
    let runtime_fingerprint = manifest::hash_file(&exe_dir.join(RUNTIME_DLL)).ok();
    let shadow_dir = prepare_shadow_dir();
    let mut slots: Vec<Slot> = Vec::new();

    let mut folders = Vec::new();
    for folder in subfolders(&exe_dir.join("modules")) {
        let name = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if GROUPS.iter().any(|group| group.eq_ignore_ascii_case(&name)) && !folder.join(manifest::FILE_NAME).exists() {
            folders.extend(subfolders(&folder));
        } else {
            folders.push(folder);
        }
    }
    folders.sort();

    for folder in folders {
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
        } else if slots.iter().any(|s| s.id == id && s.manifest.is_some()) {
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
                        slot.activity = Some(module.activity());
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
