use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    App,
    Api,
    Kernel,
    Lib,
    Feature,
    Xtask,
    Unknown,
}

pub struct Dependency {
    pub name: String,
    /// `normal`, `dev` or `build`.
    pub kind: String,
    /// Set for dependencies given by path, inside the repository or not.
    pub path: Option<PathBuf>,
    /// Where a dependency not given by path comes from, e.g. `registry+https://…` or `git+https://…`.
    pub source: Option<String>,
}

pub struct Package {
    pub name: String,
    pub version: String,
    pub description: String,
    pub dir: PathBuf,
    pub layer: Layer,
    /// `[package.metadata.uniwow]`.
    pub uniwow: Value,
    pub dependencies: Vec<Dependency>,
    pub crate_types: Vec<String>,
}

impl Package {
    pub fn feature_id(&self) -> Option<String> {
        self.meta_str("id").map(str::to_owned)
    }

    pub fn meta_str(&self, key: &str) -> Option<&str> {
        self.uniwow.get(key)?.as_str()
    }

    pub fn meta_list(&self, key: &str) -> Vec<String> {
        self.uniwow
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    }
}

pub struct Workspace {
    pub root: PathBuf,
    pub target_dir: PathBuf,
    pub packages: Vec<Package>,
}

impl Workspace {
    pub fn load() -> Result<Self> {
        let output = Command::new(crate::cargo())
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
        Ok(Self::from_metadata(&metadata))
    }

    /// Reads the output of `cargo metadata --format-version 1 --no-deps`.
    pub fn from_metadata(metadata: &Value) -> Self {
        let root = PathBuf::from(metadata["workspace_root"].as_str().unwrap_or_default());
        let target_dir = PathBuf::from(metadata["target_directory"].as_str().unwrap_or_default());
        let packages = metadata["packages"]
            .as_array()
            .map(|packages| packages.iter().map(|p| package(&root, p)).collect())
            .unwrap_or_default();
        Self {
            root,
            target_dir,
            packages,
        }
    }

    pub fn features(&self) -> Vec<&Package> {
        self.packages.iter().filter(|p| p.layer == Layer::Feature).collect()
    }

    pub fn layer_of(&self, path: &Path) -> Layer {
        layer_of(&self.root, path)
    }
}

fn package(root: &Path, value: &Value) -> Package {
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let dir = PathBuf::from(text("manifest_path"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let dependencies = value["dependencies"]
        .as_array()
        .map(|deps| {
            deps.iter()
                .map(|d| Dependency {
                    name: d["name"].as_str().unwrap_or_default().to_owned(),
                    kind: d["kind"].as_str().unwrap_or("normal").to_owned(),
                    path: d["path"].as_str().map(PathBuf::from),
                    source: d["source"].as_str().map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    let crate_types = value["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["kind"].as_array().is_some_and(|k| k.iter().any(|k| k != "bin")))
        .flat_map(|t| t["crate_types"].as_array().cloned().unwrap_or_default())
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    Package {
        name: text("name"),
        version: text("version"),
        description: text("description"),
        layer: layer_of(root, &dir),
        uniwow: value["metadata"]["uniwow"].clone(),
        dir,
        dependencies,
        crate_types,
    }
}

fn layer_of(root: &Path, dir: &Path) -> Layer {
    let Ok(relative) = dir.strip_prefix(root) else {
        return Layer::Unknown;
    };
    let parts: Vec<String> = relative.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    match parts.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["app"] => Layer::App,
        ["core", "api"] => Layer::Api,
        ["core", "kernel"] => Layer::Kernel,
        ["libs", _] => Layer::Lib,
        ["features", _] => Layer::Feature,
        ["xtask"] => Layer::Xtask,
        _ => Layer::Unknown,
    }
}
