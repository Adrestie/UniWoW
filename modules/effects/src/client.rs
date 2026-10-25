//! The client's models edited in a workspace, and exported into the archives they came from.
//!
//! A model of the client (`Spells\X.m2`) is copied into the workspace under its own path, with
//! its skins and textures; every change is made there, never in the archives. An export writes
//! the files of the workspace that differ from the client's into the archive the client reads
//! each from, after backing up the original file (only the file, never the archive) in
//! `_backup\<archive>\<path>`, the first backup kept; "restore" writes that backup back. Refused
//! while the game runs; a v1 archive kept under 4 GB; the disk's free space checked; each file
//! read back after it is written.

// Wired to the browser of the client's archives, the next step.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use uniwow_api::serde_json::{self, Value, json};
use uniwow_api::vfs::Vfs;

use uniwow_api::mpq;

/// The workspace by default: `Documents\UniWoW\Workspace`.
pub fn default_workspace() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    home.join("Documents").join("UniWoW").join("Workspace")
}

/// The path of a file of the client in the workspace.
pub fn in_workspace(workspace: &Path, path: &str) -> PathBuf {
    let mut out = workspace.to_path_buf();
    for part in path.split(['\\', '/']).filter(|p| !p.is_empty()) {
        out.push(part);
    }
    out
}

/// The files a model brings: its skins and the textures it names.
pub fn companions(path: &str, model: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let stem = &path[..path.len().saturating_sub(3)];
    for k in 0..4 {
        out.push(format!("{stem}{k:02}.skin"));
    }
    if let Ok(m) = uniwow_api::vfx::m2::read_without_anims(model) {
        for texture in m.textures.iter().filter(|t| t.kind == 0) {
            let end = texture
                .filename
                .iter()
                .position(|b| *b == 0)
                .unwrap_or(texture.filename.len());
            let name = String::from_utf8_lossy(&texture.filename[..end]).into_owned();
            if !name.is_empty() {
                out.push(name);
            }
        }
    }
    out
}

/// Copies a model of the client and its files into the workspace, those already there kept (they
/// hold the changes): the files copied.
pub fn extract(vfs: &dyn Vfs, workspace: &Path, path: &str) -> Result<Vec<String>, String> {
    let model = vfs.read(path)?.ok_or_else(|| format!("{path}: not in the client"))?;
    let mut copied = Vec::new();
    let mut files = vec![path.to_owned()];
    files.extend(companions(path, &model));
    for file in files {
        let target = in_workspace(workspace, &file);
        if target.exists() {
            continue;
        }
        let Some(bytes) = vfs.read(&file)? else {
            continue;
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
        copied.push(file);
    }
    Ok(copied)
}

/// Whether the game runs (Wow.exe): the archives are then not written.
pub fn game_running() -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq Wow.exe", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("wow.exe"))
        .unwrap_or(false)
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetDiskFreeSpaceExW(path: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32;
}

/// The bytes free on the disk of `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        let folder = if path.is_dir() { path } else { path.parent()? };
        let wide: Vec<u16> = folder.as_os_str().encode_wide_null();
        let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: a path ended by a zero and three numbers to fill.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
        (ok != 0).then_some(available)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

trait Wide {
    fn encode_wide_null(&self) -> Vec<u16>;
}

impl Wide for std::ffi::OsStr {
    fn encode_wide_null(&self) -> Vec<u16> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            self.encode_wide().chain(std::iter::once(0)).collect()
        }
        #[cfg(not(windows))]
        {
            self.to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect()
        }
    }
}

/// The mod of WoW-mods that installs the file at `path`, when the archive is one the installer
/// writes (`patch-Z.MPQ`, `patch-xxXX-Z.MPQ`): its sources mirror the paths of the game, or it
/// owns the folder.
pub fn owner(mods: &Path, archive: &Path, path: &str) -> Option<String> {
    let name = archive.file_name()?.to_string_lossy().to_lowercase();
    if !name.ends_with("-z.mpq") {
        return None;
    }
    let wanted = path.replace('/', "\\").to_lowercase();
    for entry in std::fs::read_dir(mods).ok()?.filter_map(Result::ok) {
        let manifest = entry.path().join("installer.json");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let module = value
            .get("module")
            .and_then(Value::as_str)
            .map_or_else(|| entry.file_name().to_string_lossy().into_owned(), str::to_owned);
        let Some(game) = value.get("game_files") else {
            continue;
        };
        let folders = |key: &str| -> Vec<String> {
            game.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default()
        };
        for owned in folders("owned_folders") {
            let owned = owned.replace('/', "\\").to_lowercase();
            if wanted.starts_with(&format!("{}\\", owned.trim_end_matches('\\'))) {
                return Some(module);
            }
        }
        for source in folders("sources") {
            if in_workspace(&entry.path().join(source), path).exists() {
                return Some(module);
            }
        }
    }
    Some("a mod of WoW-mods (patch-Z)".into())
}

/// What an export would write: by archive, its files, and what to say before.
#[derive(Clone, Debug, Default)]
pub struct Export {
    /// (archive, [(path in the game, file of the workspace)]).
    pub archives: Vec<(PathBuf, Vec<(String, PathBuf)>)>,
    /// What the user is told before writing: a mod that will write the file again, an archive
    /// near its limit...
    pub warnings: Vec<String>,
    /// The files of the workspace the same as the client's: not written.
    pub unchanged: Vec<String>,
    /// The bytes the archives grow by.
    pub grows: u64,
}

/// The files of the workspace under `paths` that differ from the client's, by the archive each is
/// read from; checked, nothing written.
pub fn plan(vfs: &dyn Vfs, workspace: &Path, mods: &Path, paths: &[String]) -> Result<Export, String> {
    let mut out = Export::default();
    for path in paths {
        let file = in_workspace(workspace, path);
        let Ok(ours) = std::fs::read(&file) else {
            continue;
        };
        let archive = vfs
            .source_of(path)
            .ok_or_else(|| format!("{path}: no archive of the client holds it"))?;
        if !archive.extension().is_some_and(|e| e.eq_ignore_ascii_case("mpq")) {
            return Err(format!("{path}: read from {}, not an archive", archive.display()));
        }
        if vfs.read(path)?.as_deref() == Some(ours.as_slice()) {
            out.unchanged.push(path.clone());
            continue;
        }
        if let Some(module) = owner(mods, &archive, path) {
            out.warnings.push(format!(
                "{path} comes from {} ({module}): the installer will write it again at its next install; better change the mod's own file",
                archive.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        match out.archives.iter_mut().find(|(a, _)| *a == archive) {
            Some((_, files)) => files.push((path.clone(), file)),
            None => out.archives.push((archive, vec![(path.clone(), file)])),
        }
    }
    for (archive, files) in &out.archives {
        let contents = files
            .iter()
            .map(|(path, file)| Ok((path.clone(), std::fs::read(file).map_err(|e| e.to_string())?)))
            .collect::<Result<Vec<_>, String>>()?;
        let plan = mpq::write_into(archive, &contents, true)?;
        out.grows += plan.grows;
        if plan.end > 0xE000_0000 {
            out.warnings.push(format!(
                "{} reaches {:.2} GB of its 4: a compaction will soon be needed",
                archive.display(),
                plan.end as f64 / 1e9
            ));
        }
    }
    Ok(out)
}

fn backup_dir(workspace: &Path) -> PathBuf {
    workspace.join("_backup")
}

/// The backup of the file at `path` of `archive`.
pub fn backup_of(workspace: &Path, archive: &Path, path: &str) -> PathBuf {
    let name = archive.file_name().unwrap_or_default().to_string_lossy().into_owned();
    in_workspace(&backup_dir(workspace).join(name), path)
}

/// Where each backup came from: `_backup\record.json`, {path: archive}.
fn record(workspace: &Path, archive: &Path, path: &str) -> Result<(), String> {
    let file = backup_dir(workspace).join("record.json");
    let mut value: Value = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    if let Some(map) = value.as_object_mut() {
        map.entry(path.to_owned())
            .or_insert_with(|| json!(archive.display().to_string()));
    }
    std::fs::write(&file, serde_json::to_string_pretty(&value).unwrap_or_default()).map_err(|e| e.to_string())
}

/// Writes the export: each original backed up first (the first backup kept), each archive written,
/// each file read back and compared. Refused while the game runs or without the room on the disk.
pub fn write(workspace: &Path, export: &Export) -> Result<Vec<String>, String> {
    if game_running() {
        return Err("the game runs (Wow.exe): close it before writing into its archives".into());
    }
    let mut done = Vec::new();
    for (archive, files) in &export.archives {
        let opened = mpq::Archive::open(archive)?;
        // the originals, alone, before anything changes
        for (path, _) in files {
            let backup = backup_of(workspace, archive, path);
            if !backup.exists() {
                let original = opened.read(path)?;
                if let Some(parent) = backup.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                std::fs::write(&backup, original).map_err(|e| format!("{}: {e}", backup.display()))?;
                record(workspace, archive, path)?;
            }
        }
        let contents = files
            .iter()
            .map(|(path, file)| Ok((path.clone(), std::fs::read(file).map_err(|e| e.to_string())?)))
            .collect::<Result<Vec<_>, String>>()?;
        let plan = mpq::write_into(archive, &contents, true)?;
        if let Some(free) = free_space(archive) {
            // the room for what is written, and a margin
            if plan.grows + 64 * 1024 * 1024 > free {
                return Err(format!(
                    "{}: {} MB to write, {} MB free on its disk",
                    archive.display(),
                    plan.grows / 1_000_000,
                    free / 1_000_000
                ));
            }
        }
        mpq::write_into(archive, &contents, false)?;
        let back = mpq::Archive::open(archive)?;
        for (path, content) in &contents {
            if back.read(path)? != *content {
                return Err(format!(
                    "{path}: read back from {}, not what was written",
                    archive.display()
                ));
            }
            done.push(format!(
                "{path} → {}",
                archive.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    Ok(done)
}

/// Writes the backups of `paths` back into the archives they came from.
pub fn restore(workspace: &Path, paths: &[String]) -> Result<Vec<String>, String> {
    if game_running() {
        return Err("the game runs (Wow.exe): close it before writing into its archives".into());
    }
    let record: Value = std::fs::read_to_string(backup_dir(workspace).join("record.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or("no backup recorded")?;
    let mut done = Vec::new();
    for path in paths {
        let archive = record
            .get(path)
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| format!("{path}: no backup"))?;
        let original = std::fs::read(backup_of(workspace, &archive, path)).map_err(|e| format!("{path}: {e}"))?;
        mpq::write_into(&archive, &[(path.clone(), original.clone())], false)?;
        if mpq::Archive::open(&archive)?.read(path)? != original {
            return Err(format!("{path}: read back, not the original"));
        }
        done.push(path.clone());
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client of one archive, read as the chain reads it.
    struct OneArchive(PathBuf);

    impl Vfs for OneArchive {
        fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
            let a = mpq::Archive::open(&self.0)?;
            a.contains(path).then(|| a.read(path)).transpose()
        }
        fn exists(&self, path: &str) -> bool {
            mpq::Archive::open(&self.0).is_ok_and(|a| a.contains(path))
        }
        fn files_under(&self, _: &str) -> Vec<String> {
            Vec::new()
        }
        fn path_of(&self, _: u32) -> Option<String> {
            None
        }
        fn state(&self) -> uniwow_api::vfs::VfsState {
            uniwow_api::vfs::VfsState::Opening
        }
        fn source_of(&self, path: &str) -> Option<PathBuf> {
            self.exists(path).then(|| self.0.clone())
        }
    }

    #[test]
    fn an_export_backs_up_writes_and_restores() {
        let dir = std::env::temp_dir().join(format!("uniwow-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("patch-3.MPQ");
        mpq::create(&archive, &[("Spells\\Nova.m2".into(), b"original".to_vec())], 16).unwrap();
        let vfs = OneArchive(archive.clone());
        let workspace = dir.join("Workspace");
        assert_eq!(
            extract(&vfs, &workspace, "Spells\\Nova.m2").unwrap(),
            ["Spells\\Nova.m2"]
        );
        let copy = in_workspace(&workspace, "Spells\\Nova.m2");
        assert_eq!(std::fs::read(&copy).unwrap(), b"original");
        // unchanged: nothing to write
        let export = plan(&vfs, &workspace, &dir, &["Spells\\Nova.m2".into()]).unwrap();
        assert!(export.archives.is_empty() && export.unchanged.len() == 1);
        std::fs::write(&copy, b"changed in the workspace").unwrap();
        let export = plan(&vfs, &workspace, &dir, &["Spells\\Nova.m2".into()]).unwrap();
        assert_eq!(export.archives.len(), 1);
        assert!(export.warnings.is_empty(), "{:?}", export.warnings);
        if game_running() {
            return; // the test does not write while the game runs, as the export
        }
        write(&workspace, &export).unwrap();
        assert_eq!(
            vfs.read("Spells\\Nova.m2").unwrap().unwrap(),
            b"changed in the workspace"
        );
        let backup = backup_of(&workspace, &archive, "Spells\\Nova.m2");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
        // a second export keeps the first backup
        std::fs::write(&copy, b"changed again").unwrap();
        let export = plan(&vfs, &workspace, &dir, &["Spells\\Nova.m2".into()]).unwrap();
        write(&workspace, &export).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
        restore(&workspace, &["Spells\\Nova.m2".into()]).unwrap();
        assert_eq!(vfs.read("Spells\\Nova.m2").unwrap().unwrap(), b"original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_of_a_mod_names_its_mod() {
        let dir = std::env::temp_dir().join(format!("uniwow-owner-{}", std::process::id()));
        let module = dir.join("mod-test");
        std::fs::create_dir_all(module.join("data/art/Spells")).unwrap();
        std::fs::write(module.join("data/art/Spells/Glow.blp"), b"x").unwrap();
        std::fs::write(
            module.join("installer.json"),
            r#"{"module": "mod-test", "game_files": {"sources": ["data/art"], "owned_folders": ["Interface/Test"]}}"#,
        )
        .unwrap();
        let z = Path::new("C:\\Game\\Data\\patch-Z.MPQ");
        assert_eq!(owner(&dir, z, "Spells\\Glow.blp").as_deref(), Some("mod-test"));
        assert_eq!(owner(&dir, z, "Interface\\Test\\a.blp").as_deref(), Some("mod-test"));
        assert_eq!(
            owner(&dir, Path::new("C:\\Game\\Data\\patch-3.MPQ"), "Spells\\Glow.blp"),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
