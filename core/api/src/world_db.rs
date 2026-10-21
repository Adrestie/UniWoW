//! Interface of the "world-db" service (step 15.11a): the world database of AzerothCore, read only,
//! through the connection the module `world-db` keeps on a thread of its own. Ask from a job: a query
//! waits for its rows. The changes for the server (step 16.1): each module gives those it holds
//! (`set_pending`), the user applies them all at once from the panel *World database*, one
//! transaction; the topic `APPLIED_TOPIC` then tells the modules, which read their rows again.

use std::sync::Arc;

use crate::ServiceKey;
pub use crate::db::Rows;
pub use crate::db::write::{Entry, Row, RowChange};

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("world-db");

/// Published once changes are applied to the database or undone there, with the owners whose
/// changes it held (`{ "owners": [..] }`): they read their rows again.
pub const APPLIED_TOPIC: &str = "world-db.applied";

pub type Handle = Arc<dyn WorldDb>;

/// Where the connection stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// The password is asked, or the user said *Not now*.
    Waiting,
    Connecting,
    /// Connected to the server of this version.
    Connected(String),
    /// Refused, and why, the password never said.
    Refused(String),
}

/// What the server running is asked to load again once changes are applied, for them to be seen
/// in the game without starting it again (step 16.2); in the order it is made, what the creatures
/// read first.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reload {
    /// The quests, their givers and enders, their texts.
    Quests,
    /// The addons of the creatures: their paths, mounts and auras.
    Addons,
    /// The path of waypoints of this id, the creatures following it set on it again.
    Path(u32),
    /// The group of creatures led by the creature of this guid, its members gathered again.
    Formation(u64),
    /// The creature of this guid spawned again where its row places it, or taken away.
    Creature(u64),
    /// The game object of this guid spawned again where its row places it, or taken away.
    GameObject(u64),
}

/// The changes a module holds for the server: its rows changed, what the server loads again, and
/// what it holds that is not written yet, said for the user.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    /// The module, as `APPLIED_TOPIC` names it back.
    pub owner: String,
    /// What the changes are, for the user: "Spawns".
    pub label: String,
    pub changes: Vec<RowChange>,
    pub reloads: Vec<Reload>,
    pub held: Vec<String>,
}

/// Shared between threads (rule T3).
pub trait WorldDb: Send + Sync {
    fn state(&self) -> State;

    /// The rows of `sql`, one statement beginning with `SELECT`, `SHOW`, `DESCRIBE` or `EXPLAIN`,
    /// its `?` given `parameters`. From a job or a thread of its own: it waits for the rows.
    fn query(&self, sql: &str, parameters: &[serde_json::Value]) -> Result<Rows, String>;

    /// The changes of `pending.owner` for the server, in place of those it gave before; none once
    /// it holds none.
    fn set_pending(&self, pending: Pending);

    /// The changes the modules hold for the server, by owner.
    fn pending(&self) -> Vec<Pending>;

    /// Makes `changes` in the database as one transaction, through a connection that writes opened
    /// for them: what each did, or nothing made. From a job: it waits for the database.
    fn apply(&self, changes: &[RowChange]) -> Result<Vec<Entry>, String>;
}
