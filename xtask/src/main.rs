//! UniWoW developer commands, run with `cargo xtask <command>`.

mod bindings;
mod check;
mod pe;
mod workspace;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Serialize;

use workspace::{Package, Workspace};

const USAGE: &str = "\
usage: cargo xtask <command>

  new-module <id>              create modules/<id> from the template
  build [--release]             build everything and lay out out/<profile>
  build-module <id> [--release]  build and deploy one module, leaving the editor untouched
  run [--release]               build everything, then start the editor
  check                         check the dependency rules and the runtime size
  test-sdk                      run the tests of the C++ and C# classes of sdk/
  bindings [--check]            write the numbers of sdk/bindings.toml into every language, or check them";

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
        ["new-module", id] => new_module(id),
        ["build"] => build(release).map(|_| ()),
        ["build-module", id] => build_module(id, release),
        ["run"] => run(release),
        ["check"] => check::run(),
        ["test-sdk"] => test_sdk(),
        ["bindings"] => bindings::run(args.iter().any(|a| a == "--check")),
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

fn new_module(id: &str) -> Result {
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
    let folder = ws.root.join("modules").join(id);
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
        .chain(std::iter::once("Module".to_owned()))
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
        &fill(include_str!("../templates/module/Cargo.toml.template")),
    )?;
    write(
        &folder.join("src").join("lib.rs"),
        &fill(include_str!("../templates/module/lib.rs.template")),
    )?;
    println!("Created modules/{id}. Build it with: cargo xtask build-module {id}");
    Ok(())
}

/// Builds everything and lays out `out/<profile>`. Returns the runtime fingerprint.
fn build(release: bool) -> Result<String> {
    cargo_build(release)?;
    let ws = Workspace::load()?;
    let profile = Profile::new(&ws, release);
    let runtime = deploy_runtime(&profile)?;
    let rust_modules = ws.modules();
    for package in &rust_modules {
        deploy_module(&profile, package, &runtime)?;
    }
    remove_stale(&profile, &rust_modules)?;
    let examples = build_examples(&ws, &profile)?;
    let scripts = deploy_scripts(&ws, &profile)?;
    println!(
        "{} ready with {} Rust modules, {examples} example modules and {scripts} scripts (runtime {})",
        profile.out.display(),
        rust_modules.len(),
        &runtime[..12]
    );
    Ok(runtime)
}

/// Builds and installs the sample modules of `examples/modules/<id>/` as their author would: their
/// C and C++ sources compiled with the MSVC compiler found on the machine, or their C# project
/// published with NativeAOT by the .NET SDK, into the DLL their `module.toml` names, both copied
/// into `out/<profile>/modules/<id>/`. Without the .NET 10 SDK, the C# modules are left out.
fn build_examples(ws: &Workspace, profile: &Profile) -> Result<usize> {
    let mut folders: Vec<PathBuf> = match std::fs::read_dir(ws.root.join("examples").join("modules")) {
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
        let mut files: Vec<PathBuf> = std::fs::read_dir(&folder)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        files.sort();
        let with_extension = |wanted: &[&str]| -> Vec<PathBuf> {
            files
                .iter()
                .filter(|p| p.extension().is_some_and(|e| wanted.iter().any(|w| e == *w)))
                .cloned()
                .collect()
        };
        let sources = with_extension(&["cpp", "c"]);
        let project = with_extension(&["csproj"]).into_iter().next();
        if sources.is_empty() && project.is_none() {
            continue;
        }
        let name = folder.file_name().expect("folder").to_string_lossy().into_owned();
        let manifest_path = folder.join("module.toml");
        let manifest: toml::Table = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))
            .and_then(|text| toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display())))?;
        let dll = manifest
            .get("dll")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{}: no dll", manifest_path.display()))?
            .to_owned();
        let work = profile.target.join("examples").join(&name);
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let output = match &project {
            Some(project) => {
                let Some(published) = publish_csharp(project, &work)? else {
                    println!("module {name} left out: the .NET 10 SDK is not installed");
                    continue;
                };
                published
            }
            None => compile_c(ws, &name, &sources, &dll, &work)?,
        };
        let destination = profile.out.join("modules").join(&name);
        std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
        copy_if_changed(&output.join(&dll), &destination.join(&dll))?;
        copy_if_changed(&manifest_path, &destination.join("module.toml"))?;
        built += 1;
    }
    Ok(built)
}

/// The MSVC compiler, with the options of the C and C++ modules and `sdk/` to include from.
fn msvc(ws: &Workspace, work: &Path) -> Result<Command> {
    let compiler = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "cl.exe")
        .ok_or("no MSVC compiler found: install the Visual Studio C++ build tools")?;
    let mut command = compiler.to_command();
    command
        .current_dir(work)
        .args(["/nologo", "/MD", "/O2", "/EHsc", "/std:c++17", "/utf-8", "/W4", "/WX"])
        .arg(format!("/I{}", ws.root.join("sdk").display()));
    Ok(command)
}

/// Compiles C and C++ sources into `work/<dll>`; returns `work`.
fn compile_c(ws: &Workspace, name: &str, sources: &[PathBuf], dll: &str, work: &Path) -> Result<PathBuf> {
    let output = msvc(ws, work)?
        .arg("/LD")
        .args(sources)
        .arg(format!("/Fe:{dll}"))
        .args(["/link", "/Brepro"])
        .output()
        .map_err(|e| format!("could not start the MSVC compiler: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "module {name} failed to compile:\n{}",
            String::from_utf8_lossy(&output.stdout).trim()
        ));
    }
    Ok(work.to_owned())
}

/// Publishes a C# project with NativeAOT into `work/publish`; returns that folder, or `None` when
/// the .NET 10 SDK is not installed.
fn publish_csharp(project: &Path, work: &Path) -> Result<Option<PathBuf>> {
    if !dotnet_10()? {
        return Ok(None);
    }
    // NativeAOT finds the MSVC linker through vswhere.exe, which needs `ProgramFiles(x86)`: some
    // shells, such as Git Bash, do not pass that variable on.
    let program_files = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
    let mut path = program_files
        .join("Microsoft Visual Studio")
        .join("Installer")
        .into_os_string();
    if let Some(current) = std::env::var_os("PATH") {
        path.push(";");
        path.push(current);
    }
    let published = work.join("publish");
    let output = Command::new("dotnet")
        .arg("publish")
        .arg(project)
        .args(["-c", "Release", "-r", "win-x64", "--nologo", "--artifacts-path"])
        .arg(work)
        .arg("-o")
        .arg(&published)
        .env("PATH", path)
        .env("ProgramFiles(x86)", &program_files)
        .output()
        .map_err(|e| format!("could not start dotnet: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed to publish:\n{}",
            project.display(),
            String::from_utf8_lossy(&output.stdout).trim()
        ));
    }
    Ok(Some(published))
}

/// Whether the .NET 10 SDK is installed.
fn dotnet_10() -> Result<bool> {
    match Command::new("dotnet").arg("--list-sdks").output() {
        Ok(output) => Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.starts_with("10."))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("could not start dotnet: {error}")),
    }
}

/// Compiles and runs the tests of `sdk/tests/` over a fake table: those of the C++ classes with
/// the MSVC compiler, those of the C# classes with the .NET 10 SDK, left out without it.
fn test_sdk() -> Result {
    let ws = Workspace::load()?;
    let tests = ws.root.join("sdk").join("tests");
    let work = ws.target_dir.join("sdk-tests");
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let output = msvc(&ws, &work)?
        .arg(tests.join("registry.cpp"))
        .arg("/Fe:registry.exe")
        .output()
        .map_err(|e| format!("could not start the MSVC compiler: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "the C++ tests failed to compile:\n{}",
            String::from_utf8_lossy(&output.stdout).trim()
        ));
    }
    let cpp = Command::new(work.join("registry.exe"))
        .status()
        .map_err(|e| format!("could not start the C++ tests: {e}"))?;
    if !cpp.success() {
        return Err("the C++ tests failed".to_owned());
    }
    if !dotnet_10()? {
        println!("C# tests left out: the .NET 10 SDK is not installed");
        return Ok(());
    }
    let csharp = Command::new("dotnet")
        .args(["run", "-c", "Release", "--project"])
        .arg(tests.join("csharp").join("sdk-tests.csproj"))
        .arg("--artifacts-path")
        .arg(work.join("csharp"))
        .status()
        .map_err(|e| format!("could not start dotnet: {e}"))?;
    if !csharp.success() {
        return Err("the C# tests failed".to_owned());
    }
    Ok(())
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

fn build_module(id: &str, release: bool) -> Result {
    let ws = Workspace::load()?;
    let package = ws
        .modules()
        .into_iter()
        .find(|p| p.module_id().as_deref() == Some(id))
        .ok_or_else(|| format!("no module '{id}' in modules/"))?;
    // Same package selection as `build`, so that shared dependencies keep the same modules.
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
    deploy_module(&profile, package, &runtime)?;
    println!(
        "module '{id}' deployed to {}",
        profile.out.join("modules").join(id).display()
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
    std::fs::create_dir_all(profile.out.join("modules")).map_err(|e| e.to_string())?;
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
struct ModuleManifest<'a> {
    id: &'a str,
    kind: &'a str,
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
    /// `workspace` for modules built from this repository; `cargo xtask build` removes their
    /// folders when the source is gone, and leaves other folders alone.
    origin: &'a str,
}

fn deploy_module(profile: &Profile, package: &Package, runtime: &str) -> Result {
    let id = package
        .module_id()
        .ok_or_else(|| format!("{}: missing [package.metadata.uniwow] id", package.name))?;
    let source = profile.target.join(format!("{}.dll", package.name.replace('-', "_")));
    let folder = profile
        .out
        .join("modules")
        .join(package.module_folder().unwrap_or_else(|| PathBuf::from(&id)));
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let dll = format!("{id}.dll");
    copy_if_changed(&source, &folder.join(&dll))?;
    let manifest = ModuleManifest {
        id: &id,
        kind: "rust",
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
    write(&folder.join("module.toml"), &text)
}

/// Removes the deployed workspace modules whose source folder no longer exists, or which are no
/// longer deployed there, such as a module moved into `modules/UI/`.
fn remove_stale(profile: &Profile, modules: &[&Package]) -> Result {
    let root = profile.out.join("modules");
    let expected: Vec<PathBuf> = modules.iter().filter_map(|p| p.module_folder()).collect();
    let subfolders = |folder: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(folder)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut deployed = Vec::new();
    for folder in subfolders(&root) {
        if folder.join("module.toml").exists() {
            deployed.push(folder);
        } else {
            deployed.extend(subfolders(&folder));
        }
    }
    for folder in deployed {
        let Ok(text) = std::fs::read_to_string(folder.join("module.toml")) else {
            continue;
        };
        let Ok(manifest) = toml::from_str::<toml::Table>(&text) else {
            continue;
        };
        let from_workspace = manifest.get("origin").and_then(|v| v.as_str()) == Some("workspace");
        let relative = folder.strip_prefix(&root).map(Path::to_path_buf).unwrap_or_default();
        if from_workspace && !expected.contains(&relative) {
            std::fs::remove_dir_all(&folder).map_err(|e| e.to_string())?;
            println!(
                "removed modules/{}: no module of the workspace goes there",
                relative.display()
            );
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
