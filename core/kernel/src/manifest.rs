use std::path::Path;

use uniwow_api::serde::Deserialize;

pub const FILE_NAME: &str = "feature.toml";

/// `feature.toml`, written next to each feature DLL by `cargo xtask build`.
#[derive(Clone, Debug, Deserialize)]
#[serde(crate = "uniwow_api::serde")]
pub struct Manifest {
    pub id: String,
    /// Cargo package name, checked against the DLL.
    pub package: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub description: String,
    /// Services the feature cannot work without.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Services the feature uses when present.
    #[serde(default)]
    pub uses: Vec<String>,
    /// DLL file name, in the same folder.
    pub dll: String,
    /// BLAKE3 hash of that DLL.
    pub dll_hash: String,
    /// BLAKE3 hash of the runtime DLL the feature was built against.
    pub runtime: String,
}

impl Manifest {
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        toml::from_str(&text).map_err(|e| e.to_string())
    }
}

pub fn hash_file(path: &Path) -> std::io::Result<String> {
    Ok(blake3::hash(&std::fs::read(path)?).to_hex().to_string())
}
