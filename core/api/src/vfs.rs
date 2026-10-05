//! The contract of the client's files, offered by the module `assets`: the archives of the 3.3.5a
//! client in the order the client reads them, with the patches WarcraftXL adds, read from any thread
//! at once (T3).

use std::sync::Arc;

use crate::ServiceKey;

/// How far the client's files are.
#[derive(Clone, Debug, PartialEq)]
pub enum VfsState {
    /// No client folder is set, or it holds no archive of 3.3.5a: why.
    NoClient(String),
    /// The archives are being opened.
    Opening,
    /// Open: how many archives, and how many files their lists name.
    Ready { archives: usize, files: usize },
}

/// The client's files, offered by the module `assets`.
pub trait Vfs: Send + Sync {
    /// The bytes of the file at `path`, case and slashes ignored; none when no archive holds it or
    /// a patch deleted it. An error for a file that cannot be read, or before the archives are open.
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String>;

    /// Whether a file is at `path`.
    fn exists(&self, path: &str) -> bool;

    /// The files under `folder` and the folders below it, by the paths the archives' lists give.
    fn files_under(&self, folder: &str) -> Vec<String>;

    /// The path of the modern file of id `file_data_id`, as WarcraftXL finds it: through the tables
    /// `TextureFilePath.db2`, then `ModelFilePath.db2`, of the client; none when neither names it.
    fn path_of(&self, file_data_id: u32) -> Option<String>;

    fn state(&self) -> VfsState;
}

/// The service of the client's files.
pub const SERVICE: ServiceKey<Arc<dyn Vfs>> = ServiceKey::new("vfs");
