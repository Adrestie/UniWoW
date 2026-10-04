//! The chain of the client's archives, in the order the client reads them: the first holding a file
//! gives it, and a delete marker there means that the file is no more.
//!
//! The order of the patches is that of Wow.exe 12340: `Data\patch-?.MPQ` and
//! `Data\<locale>\patch-<locale>-?.MPQ` sorted together by their path, from the last, case
//! ignored, then `Data\patch.MPQ` and `Data\<locale>\patch-<locale>.MPQ`. WarcraftXL widens the
//! patches to any name, `Data\Patch-<name>.MPQ`, which may also be a folder mounted as an archive.
//! The base archives follow, in the order the community documents.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::mpq::{Archive, Entry};

/// A path as a key: lower case, backslashes.
pub fn key(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

/// The base archives, after the patches, in the folder of the locale (`{}`) then in `Data`.
const BASE_LOCALE: [&str; 6] = [
    "lichking-locale-{}.mpq",
    "expansion-locale-{}.mpq",
    "locale-{}.mpq",
    "lichking-speech-{}.mpq",
    "expansion-speech-{}.mpq",
    "base-{}.mpq",
];
const BASE: [&str; 4] = ["lichking.mpq", "expansion.mpq", "common-2.mpq", "common.mpq"];

/// The entries of `folder` whose name, lower case, starts with `prefix` and ends with `.mpq`, with
/// at least one character between, files or folders.
fn patches(folder: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            name.len() > prefix.len() + 4 && name.starts_with(prefix) && name.ends_with(".mpq")
        })
        .map(|entry| entry.path())
        .collect()
}

/// The archives of the client in `client`, for `locale`, the first read first.
pub fn order(client: &Path, locale: &str) -> Vec<PathBuf> {
    let data = client.join("Data");
    let localised = data.join(locale);
    let lower = locale.to_ascii_lowercase();
    let mut chain = patches(&data, "patch-");
    chain.extend(patches(&localised, &format!("patch-{lower}-")));
    let path_key = |path: &PathBuf| key(&path.strip_prefix(client).unwrap_or(path).to_string_lossy());
    chain.sort_by_key(|path| std::cmp::Reverse(path_key(path)));
    chain.push(data.join("patch.mpq"));
    chain.push(localised.join(format!("patch-{lower}.mpq")));
    chain.extend(
        BASE_LOCALE
            .iter()
            .map(|name| localised.join(name.replace("{}", &lower))),
    );
    chain.extend(BASE.iter().map(|name| data.join(name)));
    chain.into_iter().filter(|path| path.exists()).collect()
}

/// The locale of the client: the one `WTF\Config.wtf` sets, else the one folder of `Data` holding
/// its `locale-<locale>.MPQ`.
pub fn locale(client: &Path) -> Result<String, String> {
    if let Ok(config) = std::fs::read_to_string(client.join("WTF").join("Config.wtf"))
        && let Some(locale) = config.lines().find_map(|line| {
            let value = line.trim().strip_prefix("SET locale ")?;
            Some(value.trim_matches('"').to_owned())
        })
        && client.join("Data").join(&locale).is_dir()
    {
        return Ok(locale);
    }
    let found: Vec<String> = std::fs::read_dir(client.join("Data"))
        .map_err(|e| format!("no folder Data: {e}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.len() == 4
                && client
                    .join("Data")
                    .join(name)
                    .join(format!("locale-{}.mpq", name.to_ascii_lowercase()))
                    .exists()
        })
        .collect();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err("no locale folder in Data".to_owned()),
        many => Err(format!(
            "several locales in Data ({}) and none set in WTF\\Config.wtf",
            many.join(", ")
        )),
    }
}

/// A folder mounted as an archive: its files by their path below it.
pub struct Folder {
    files: HashMap<String, (String, PathBuf)>,
}

impl Folder {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut files = HashMap::new();
        let mut folders = vec![path.to_owned()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
            {
                let found = entry.path();
                if found.is_dir() {
                    folders.push(found);
                } else if let Ok(relative) = found.strip_prefix(path) {
                    let name = relative.to_string_lossy().replace('/', "\\");
                    files.insert(key(&name), (name, found));
                }
            }
        }
        Ok(Self { files })
    }
}

/// An archive of the chain: an MPQ, or a folder mounted as one.
pub enum Source {
    Archive(Archive),
    Folder(Folder),
}

impl Source {
    pub fn open(path: &Path) -> Result<Self, String> {
        if path.is_dir() {
            Folder::open(path).map(Self::Folder)
        } else {
            Archive::open(path).map(Self::Archive)
        }
    }

    /// The names it lists.
    pub fn listed(&self) -> Result<Vec<String>, String> {
        match self {
            Self::Archive(archive) => archive.listed(),
            Self::Folder(folder) => Ok(folder.files.values().map(|(name, _)| name.clone()).collect()),
        }
    }
}

/// What the chain holds under a name.
enum Found<'a> {
    Archive(&'a Archive, usize),
    Loose(&'a Path),
    Deleted,
}

/// The archives of the client, the first read first, and the names their lists give.
pub struct Chain {
    sources: Vec<Source>,
    /// The files listed and not deleted, by key, with their path as listed.
    listed: BTreeMap<String, String>,
}

impl Chain {
    /// The chain of `sources`, the first read first, their lists merged: a name deleted by the
    /// first archive listing it is left out.
    pub fn new(sources: Vec<Source>) -> Self {
        let mut listed = BTreeMap::new();
        let mut seen = HashSet::new();
        for source in &sources {
            for name in source.listed().unwrap_or_default() {
                let key = key(&name);
                if !seen.insert(key.clone()) {
                    continue;
                }
                let deleted = match source {
                    Source::Archive(archive) => matches!(archive.find(&name), Some(Entry::Deleted) | None),
                    Source::Folder(_) => false,
                };
                if !deleted {
                    listed.insert(key, name);
                }
            }
        }
        Self { sources, listed }
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    pub fn listed_count(&self) -> usize {
        self.listed.len()
    }

    fn find(&self, path: &str) -> Option<Found<'_>> {
        for source in &self.sources {
            match source {
                Source::Archive(archive) => match archive.find(path) {
                    Some(Entry::File(index)) => return Some(Found::Archive(archive, index)),
                    Some(Entry::Deleted) => return Some(Found::Deleted),
                    None => {}
                },
                Source::Folder(folder) => {
                    if let Some((_, file)) = folder.files.get(&key(path)) {
                        return Some(Found::Loose(file));
                    }
                }
            }
        }
        None
    }

    /// The bytes of the file at `path`; none when no archive holds it or a patch deleted it.
    pub fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        match self.find(path) {
            None | Some(Found::Deleted) => Ok(None),
            Some(Found::Archive(archive, index)) => archive
                .read(index)
                .map(Some)
                .map_err(|e| format!("{path} in {}: {e}", archive.path().display())),
            Some(Found::Loose(file)) => std::fs::read(file)
                .map(Some)
                .map_err(|e| format!("{}: {e}", file.display())),
        }
    }

    pub fn exists(&self, path: &str) -> bool {
        matches!(self.find(path), Some(Found::Archive(..) | Found::Loose(_)))
    }

    /// The files listed under `folder` and the folders below it.
    pub fn files_under(&self, folder: &str) -> Vec<String> {
        let mut prefix = key(folder);
        if !prefix.is_empty() && !prefix.ends_with('\\') {
            prefix.push('\\');
        }
        self.listed
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix))
            .map(|(_, name)| name.clone())
            .collect()
    }
}
