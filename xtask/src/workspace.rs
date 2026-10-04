use std::collections::{HashMap, HashSet};
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
    Module,
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
    /// Cargo package id, as in `workspace_members` and the resolved graph.
    pub id: String,
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
    /// Where the module goes under `modules\` beside the executable: `<id>`, or `UI\<id>` for a
    /// module of `modules/UI/`.
    pub fn module_folder(&self) -> Option<PathBuf> {
        let id = self.module_id()?;
        let group = self.dir.parent()?.file_name()?.to_string_lossy().into_owned();
        Some(if group == "modules" {
            PathBuf::from(id)
        } else {
            PathBuf::from(group).join(id)
        })
    }

    pub fn module_id(&self) -> Option<String> {
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

/// An edge of the resolved dependency graph.
pub struct Resolved {
    pub id: String,
    pub name: String,
    /// A normal dependency for at least one target, as opposed to dev or build only.
    pub normal: bool,
}

pub struct Workspace {
    pub root: PathBuf,
    pub target_dir: PathBuf,
    /// The workspace members only.
    pub packages: Vec<Package>,
    /// Direct dependencies of every package of the build, by package id.
    pub resolve: HashMap<String, Vec<Resolved>>,
}

impl Workspace {
    pub fn load() -> Result<Self> {
        let output = Command::new(crate::cargo())
            .args(["metadata", "--format-version", "1"])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
        Ok(Self::from_metadata(&metadata))
    }

    /// Reads the output of `cargo metadata --format-version 1`.
    pub fn from_metadata(metadata: &Value) -> Self {
        let root = PathBuf::from(metadata["workspace_root"].as_str().unwrap_or_default());
        let target_dir = PathBuf::from(metadata["target_directory"].as_str().unwrap_or_default());
        let members: Option<HashSet<&str>> = metadata["workspace_members"]
            .as_array()
            .map(|ids| ids.iter().filter_map(|id| id.as_str()).collect());
        let packages = metadata["packages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| {
                members
                    .as_ref()
                    .is_none_or(|m| m.contains(p["id"].as_str().unwrap_or_default()))
            })
            .map(|p| package(&root, p))
            .collect();
        let resolve = metadata["resolve"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|node| {
                let edges = node["deps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|dep| Resolved {
                        id: dep["pkg"].as_str().unwrap_or_default().to_owned(),
                        name: dep["name"].as_str().unwrap_or_default().to_owned(),
                        normal: dep["dep_kinds"]
                            .as_array()
                            .is_some_and(|kinds| kinds.iter().any(|k| k["kind"].is_null())),
                    })
                    .collect();
                (node["id"].as_str().unwrap_or_default().to_owned(), edges)
            })
            .collect();
        Self {
            root,
            target_dir,
            packages,
            resolve,
        }
    }

    pub fn is_member(&self, id: &str) -> bool {
        self.packages.iter().any(|p| p.id == id)
    }

    /// Every package `id` reaches through normal dependencies, itself excluded.
    pub fn normal_tree(&self, id: &str) -> HashSet<String> {
        let mut reached = HashSet::new();
        let mut pending = vec![id.to_owned()];
        while let Some(current) = pending.pop() {
            for edge in self.resolve.get(&current).into_iter().flatten().filter(|e| e.normal) {
                if reached.insert(edge.id.clone()) {
                    pending.push(edge.id.clone());
                }
            }
        }
        reached
    }

    pub fn modules(&self) -> Vec<&Package> {
        self.packages.iter().filter(|p| p.layer == Layer::Module).collect()
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
        id: text("id"),
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
        ["modules", _] | ["modules", "UI", _] => Layer::Module,
        ["xtask"] => Layer::Xtask,
        _ => Layer::Unknown,
    }
}
