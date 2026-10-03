//! UniWoW developer commands, run with `cargo xtask <command>`.

mod check;
mod pe;
mod workspace;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Serialize;

use workspace::{Package, Workspace};

const USAGE: &str = "\
usage: cargo xtask <command>

  new-feature <id>              create features/<id> from the template
  build [--release]             build everything and lay out out/<profile>
  build-feature <id> [--release]  build and deploy one feature, leaving the editor untouched
  run [--release]               build everything, then start the editor
  check                         check the dependency rules and the runtime size";

const RUNTIME_DLL: &str = "uniwow_api.dll";
const EXECUTABLE: &str = "UniWoW.exe";

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    let positional: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();
    let result = match positional.as_slice() {
        ["new-feature", id] => new_feature(id),
        ["build"] => build(release).map(|_| ()),
        ["build-feature", id] => build_feature(id, release),
        ["run"] => run(release),
        ["check"] => check::run(),
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn new_feature(id: &str) -> Result {
    let valid = id
        .split('-')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
        && id.starts_with(|c: char| c.is_ascii_lowercase());
    if !valid {
        return Err(format!(
            "invalid id '{id}': lowercase letters, digits and single hyphens, starting with a letter"
        ));
    }
    let ws = Workspace::load()?;
    let folder = ws.root.join("features").join(id);
    if folder.exists() {
        return Err(format!("{} already exists", folder.display()));
    }
    let name = {
        let spaced = id.replace('-', " ");
        let mut chars = spaced.chars();
        chars
            .next()
            .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
            .unwrap_or_default()
    };
    let struct_name: String = id
        .split('-')
        .map(|part| part[..1].to_ascii_uppercase() + &part[1..])
        .chain(std::iter::once("Feature".to_owned()))
        .collect();
    let fill = |template: &str| {
        template
            .replace("{{id}}", id)
            .replace("{{name}}", &name)
            .replace("{{struct}}", &struct_name)
    };
    std::fs::create_dir_all(folder.join("src")).map_err(|e| e.to_string())?;
    write(
        &folder.join("Cargo.toml"),
        &fill(include_str!("../templates/feature/Cargo.toml.template")),
    )?;
    write(
        &folder.join("src").join("lib.rs"),
        &fill(include_str!("../templates/feature/lib.rs.template")),
    )?;
    println!("Created features/{id}. Build it with: cargo xtask build-feature {id}");
    Ok(())
}

/// Builds everything and lays out `out/<profile>`. Returns the runtime fingerprint.
fn build(release: bool) -> Result<String> {
    cargo_build(release)?;
    let ws = Workspace::load()?;
    let profile = Profile::new(&ws, release);
    let runtime = deploy_runtime(&profile)?;
    let features = ws.features();
    for package in &features {
        deploy_feature(&profile, package, &runtime)?;
    }
    remove_stale(&profile, &features)?;
    let modules = build_modules(&ws, &profile)?;
    let scripts = deploy_scripts(&ws, &profile)?;
    println!(
        "{} ready with {} features, {modules} modules and {scripts} scripts (runtime {})",
        profile.out.display(),
        features.len(),
        &runtime[..12]
    );
    Ok(runtime)
}

/// Compiles each `modules-src/<name>` folder (its `.cpp` and `.c` files) into
/// `out/<profile>/modules/<name>.dll` with the MSVC compiler found on the machine.
fn build_modules(ws: &Workspace, profile: &Profile) -> Result<usize> {
    let mut folders: Vec<PathBuf> = match std::fs::read_dir(ws.root.join("modules-src")) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => return Ok(0),
    };
    folders.sort();
    let mut built = 0;
    for folder in folders {
        let mut sources: Vec<PathBuf> = std::fs::read_dir(&folder)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "cpp" || e == "c"))
            .collect();
        if sources.is_empty() {
            continue;
        }
        sources.sort();
        let name = folder.file_name().expect("folder").to_string_lossy().into_owned();
        let work = profile.target.join("modules").join(&name);
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let compiler = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "cl.exe")
            .ok_or("no MSVC compiler found: install the Visual Studio C++ build tools")?;
        let output = compiler
            .to_command()
            .current_dir(&work)
            .args([
                "/nologo",
                "/LD",
                "/MD",
                "/O2",
                "/EHsc",
                "/std:c++17",
                "/utf-8",
                "/W4",
                "/WX",
            ])
            .arg(format!("/I{}", ws.root.join("sdk").display()))
            .args(&sources)
            .arg(format!("/Fe:{name}.dll"))
            .args(["/link", "/Brepro"])
            .output()
            .map_err(|e| format!("could not start the MSVC compiler: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "module {name} failed to compile:\n{}",
                String::from_utf8_lossy(&output.stdout).trim()
            ));
        }
        let destination = profile.out.join("modules");
        std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
        copy_if_changed(
            &work.join(format!("{name}.dll")),
            &destination.join(format!("{name}.dll")),
        )?;
        built += 1;
    }
    Ok(built)
}

/// Copies the scripts of `scripts/` into `out/<profile>/scripts`, overwriting those of the same
/// name and leaving the others alone.
fn deploy_scripts(ws: &Workspace, profile: &Profile) -> Result<usize> {
    fn copy_tree(source: &Path, destination: &Path) -> Result<usize> {
        let Ok(entries) = std::fs::read_dir(source) else {
            return Ok(0);
        };
        std::fs::create_dir_all(destination).map_err(|e| e.to_string())?;
        let mut copied = 0;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let target = destination.join(entry.file_name());
            if path.is_dir() {
                copied += copy_tree(&path, &target)?;
            } else {
                copy_if_changed(&path, &target)?;
                copied += 1;
            }
        }
        Ok(copied)
    }
    copy_tree(&ws.root.join("scripts"), &profile.out.join("scripts"))
}

fn build_feature(id: &str, release: bool) -> Result {
    let ws = Workspace::load()?;
    let package = ws
        .features()
        .into_iter()
        .find(|p| p.feature_id().as_deref() == Some(id))
        .ok_or_else(|| format!("no feature '{id}' in features/"))?;
    // Same package selection as `build`, so that shared dependencies keep the same features.
    cargo_build(release)?;
    let profile = Profile::new(&ws, release);
    let deployed = profile.out.join(RUNTIME_DLL);
    if !deployed.exists() {
        return Err(format!(
            "{} has no runtime yet: run `cargo xtask build` first",
            profile.out.display()
        ));
    }
    let runtime = hash(&deployed)?;
    if hash(&profile.target.join(RUNTIME_DLL))? != runtime {
        return Err(
            "the runtime changed since the last full build (core/api or a shared dependency was \
                    modified): run `cargo xtask build`"
                .to_owned(),
        );
    }
    deploy_feature(&profile, package, &runtime)?;
    println!(
        "feature '{id}' deployed to {}",
        profile.out.join("features").join(id).display()
    );
    Ok(())
}

fn run(release: bool) -> Result {
    build(release)?;
    let ws = Workspace::load()?;
    let profile = Profile::new(&ws, release);
    let status = Command::new(profile.out.join(EXECUTABLE))
        .status()
        .map_err(|e| format!("could not start the editor: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the editor exited with {status}"))
    }
}

struct Profile {
    /// `target/<profile>`, where cargo writes.
    target: PathBuf,
    /// `out/<profile>`, the layout the editor runs from.
    out: PathBuf,
}

impl Profile {
    fn new(ws: &Workspace, release: bool) -> Self {
        let name = if release { "release" } else { "debug" };
        Self {
            target: ws.target_dir.join(name),
            out: ws.root.join("out").join(name),
        }
    }
}

fn cargo_build(release: bool) -> Result {
    let mut command = Command::new(cargo());
    command.args(["build", "--workspace", "--exclude", "xtask"]);
    if release {
        command.arg("--release");
    }
    let status = command.status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("cargo build failed".to_owned())
    }
}

/// Copies the executable, the runtime DLL and the Rust standard library DLL.
fn deploy_runtime(profile: &Profile) -> Result<String> {
    std::fs::create_dir_all(profile.out.join("features")).map_err(|e| e.to_string())?;
    for file in [EXECUTABLE, "UniWoW.pdb", RUNTIME_DLL, "uniwow_api.pdb"] {
        let source = profile.target.join(file);
        if source.exists() {
            copy_if_changed(&source, &profile.out.join(file))?;
        }
    }
    let std_dll = std_dll()?;
    copy_if_changed(&std_dll, &profile.out.join(std_dll.file_name().expect("file")))?;
    hash(&profile.out.join(RUNTIME_DLL))
}

#[derive(Serialize)]
struct FeatureManifest<'a> {
    id: &'a str,
    package: &'a str,
    name: &'a str,
    version: &'a str,
    category: &'a str,
    description: &'a str,
    requires: Vec<String>,
    uses: Vec<String>,
    dll: String,
    dll_hash: String,
    runtime: &'a str,
    /// `workspace` for features built from this repository; `cargo xtask build` removes their
    /// folders when the source is gone, and leaves other folders alone.
    origin: &'a str,
}

fn deploy_feature(profile: &Profile, package: &Package, runtime: &str) -> Result {
    let id = package
        .feature_id()
        .ok_or_else(|| format!("{}: missing [package.metadata.uniwow] id", package.name))?;
    let source = profile.target.join(format!("{}.dll", package.name.replace('-', "_")));
    let folder = profile.out.join("features").join(&id);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let dll = format!("{id}.dll");
    copy_if_changed(&source, &folder.join(&dll))?;
    let manifest = FeatureManifest {
        id: &id,
        package: &package.name,
        name: package.meta_str("name").unwrap_or(&id),
        version: &package.version,
        category: package.meta_str("category").unwrap_or(""),
        description: &package.description,
        requires: package.meta_list("requires"),
        uses: package.meta_list("uses"),
        dll_hash: hash(&folder.join(&dll))?,
        dll,
        runtime,
        origin: "workspace",
    };
    let text = toml::to_string(&manifest).map_err(|e| e.to_string())?;
    write(&folder.join("feature.toml"), &text)
}

/// Removes the deployed workspace features whose source folder no longer exists.
fn remove_stale(profile: &Profile, features: &[&Package]) -> Result {
    let ids: Vec<String> = features.iter().filter_map(|p| p.feature_id()).collect();
    let Ok(entries) = std::fs::read_dir(profile.out.join("features")) else {
        return Ok(());
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let Ok(text) = std::fs::read_to_string(entry.path().join("feature.toml")) else {
            continue;
        };
        let Ok(manifest) = toml::from_str::<toml::Table>(&text) else {
            continue;
        };
        let id = manifest.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        let from_workspace = manifest.get("origin").and_then(|v| v.as_str()) == Some("workspace");
        if from_workspace && !ids.iter().any(|i| i == id) {
            std::fs::remove_dir_all(entry.path()).map_err(|e| e.to_string())?;
            println!("removed features/{id}: its source folder no longer exists");
        }
    }
    Ok(())
}

/// The Rust standard library DLL of the active toolchain.
fn std_dll() -> Result<PathBuf> {
    let rustc = |arg: &str| -> Result<String> {
        let output = Command::new("rustc").arg(arg).output().map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let sysroot = PathBuf::from(rustc("--print=sysroot")?.trim());
    let host = rustc("-vV")?
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_owned))
        .ok_or("could not read the rustc host")?;
    let lib = sysroot.join("lib").join("rustlib").join(host).join("lib");
    std::fs::read_dir(&lib)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            name.starts_with("std-") && name.ends_with(".dll")
        })
        .ok_or_else(|| format!("no std-*.dll in {}", lib.display()))
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

fn hash(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Copies unless the destination already has the same content, so unchanged files keep their date.
fn copy_if_changed(source: &Path, destination: &Path) -> Result {
    if destination.exists() && hash(source)? == hash(destination)? {
        return Ok(());
    }
    std::fs::copy(source, destination)
        .map(|_| ())
        .map_err(|e| format!("copy {} to {}: {e}", source.display(), destination.display()))
}

fn write(path: &Path, text: &str) -> Result {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}
