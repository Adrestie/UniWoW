use std::path::Path;

use uniwow_api::serde::Deserialize;

pub const FILE_NAME: &str = "module.toml";

/// How a module is written (section 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(crate = "uniwow_api::serde", rename_all = "lowercase")]
pub enum Kind {
    /// A crate of this project, built against the runtime.
    Rust,
    /// A DLL exporting the entry point of `uniwow.h`.
    Compiled,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Rust => "Rust",
            Kind::Compiled => "compiled",
        }
    }
}

/// `module.toml`, in the folder of each module: written by `cargo xtask build` for the modules of
/// this project, by its author for a compiled module.
#[derive(Clone, Debug, Deserialize)]
#[serde(crate = "uniwow_api::serde")]
pub struct Manifest {
    pub id: String,
    pub kind: Kind,
    /// Cargo package name of a Rust module, checked against the DLL.
    #[serde(default)]
    pub package: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub description: String,
    /// Services the module cannot work without.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Services the module uses when present.
    #[serde(default)]
    pub uses: Vec<String>,
    /// DLL file name, in the same folder.
    pub dll: String,
    /// BLAKE3 hash of the DLL of a Rust module.
    #[serde(default)]
    pub dll_hash: String,
    /// BLAKE3 hash of the runtime DLL a Rust module was built against.
    #[serde(default)]
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

#[cfg(test)]
mod tests {
    use super::{Kind, Manifest};

    fn parse(text: &str) -> Result<Manifest, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    #[test]
    fn a_compiled_module_needs_neither_package_nor_fingerprint() {
        let manifest = parse(
            r#"id = "sample-cpp"
            name = "Sample"
            version = "0.1.0"
            kind = "compiled"
            dll = "sample-cpp.dll""#,
        )
        .expect("valid");
        assert_eq!(manifest.kind, Kind::Compiled);
        assert!(manifest.runtime.is_empty() && manifest.package.is_empty());
    }

    #[test]
    fn the_kind_is_required_and_checked() {
        let base = "id = \"m\"\nname = \"M\"\nversion = \"1\"\ndll = \"m.dll\"\n";
        assert!(parse(base).is_err(), "no kind");
        assert!(
            parse(&format!("{base}kind = \"lua\"")).is_err(),
            "not a kind of this version"
        );
        assert_eq!(parse(&format!("{base}kind = \"rust\"")).map(|m| m.kind), Ok(Kind::Rust));
    }
}
