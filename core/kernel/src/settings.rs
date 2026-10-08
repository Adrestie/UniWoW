use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use uniwow_api::serde::{Deserialize, Serialize};
use uniwow_api::{log, serde_json};

/// User settings, in `%APPDATA%\UniWoW\settings.json` (or `UNIWOW_SETTINGS_DIR` when set).
#[derive(Default, Serialize, Deserialize)]
#[serde(crate = "uniwow_api::serde")]
pub struct Settings {
    /// Modules not loaded at start.
    #[serde(default)]
    pub disabled_modules: BTreeSet<String>,
    /// Panels the user closed, as `module/panel`; not reopened automatically.
    #[serde(default)]
    pub closed_panels: BTreeSet<String>,
    /// Dock layout of the last session.
    #[serde(default)]
    pub layout: Option<serde_json::Value>,
    /// Settings of each module, by module id then key.
    #[serde(default)]
    pub modules: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    /// The keys the user bound hotkeys to, by `<owner>/<name>`, those other than their own.
    #[serde(default)]
    pub hotkeys: BTreeMap<String, String>,
    /// Never written to disk, for the tests.
    #[serde(skip)]
    pub in_memory: bool,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        let dir = std::env::var_os("UNIWOW_SETTINGS_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("UniWoW")))?;
        Some(dir.join("settings.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::warn!("{} is invalid and was ignored: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path().filter(|_| !self.in_memory) else {
            return;
        };
        // Written beside, then renamed over the old file: a crash never leaves it half written.
        let temporary = path.with_extension("json.tmp");
        let result = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
            let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
            std::fs::write(&temporary, text)?;
            std::fs::rename(&temporary, &path)
        });
        if let Err(e) = result {
            log::warn!("could not save {}: {e}", path.display());
        }
    }
}
