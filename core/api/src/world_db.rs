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
    /// The quest of this id taken from every character of the server, deleted: out of the quest logs
    /// and the quests done (daily, weekly, monthly, seasonal too), for a quest of its id made again
    /// to be offered to them.
    ForgetQuest(u32),
    /// The addons of the creatures: their paths, mounts and auras.
    Addons,
    /// The path of waypoints of this id, the creatures walking it set on it again from its first
    /// point.
    Path(u32),
    /// The group of creatures led by the creature of this guid, its members gathered again.
    Formation(u64),
    /// The template of creature of this entry read again with its names in every language, its
    /// models, weapons, SmartAI and texts, its creatures running spawned again; before the
    /// creatures, for those made of a template new to the server.
    Template(u32),
    /// The creature of this guid spawned again where its row places it, or taken away.
    Creature(u64),
    /// The spawn point of the creature of this guid read again from its row: the creature keeps its
    /// place and goes home there as it comes back; spawned again by a bridge that cannot.
    Home(u64),
    /// The game object of this guid spawned again where its row places it, or taken away.
    GameObject(u64),
}

impl Reload {
    /// As JSON, as the applications and the packages keep it.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Reload::Quests => json!({ "quests": true }),
            Reload::ForgetQuest(id) => json!({ "forget_quest": id }),
            Reload::Addons => json!({ "addons": true }),
            Reload::Path(id) => json!({ "path": id }),
            Reload::Formation(leader) => json!({ "formation": leader }),
            Reload::Template(entry) => json!({ "template": entry }),
            Reload::Creature(guid) => json!({ "creature": guid }),
            Reload::Home(guid) => json!({ "home": guid }),
            Reload::GameObject(guid) => json!({ "gameobject": guid }),
        }
    }

    /// Read from its JSON.
    pub fn of_json(value: &serde_json::Value) -> Option<Reload> {
        if value["quests"].as_bool() == Some(true) {
            return Some(Reload::Quests);
        }
        if let Some(id) = value["forget_quest"].as_u64() {
            return Some(Reload::ForgetQuest(id as u32));
        }
        if value["addons"].as_bool() == Some(true) {
            return Some(Reload::Addons);
        }
        if let Some(id) = value["path"].as_u64() {
            return Some(Reload::Path(id as u32));
        }
        if let Some(leader) = value["formation"].as_u64() {
            return Some(Reload::Formation(leader));
        }
        if let Some(entry) = value["template"].as_u64() {
            return Some(Reload::Template(entry as u32));
        }
        if let Some(guid) = value["creature"].as_u64() {
            return Some(Reload::Creature(guid));
        }
        if let Some(guid) = value["home"].as_u64() {
            return Some(Reload::Home(guid));
        }
        value["gameobject"].as_u64().map(Reload::GameObject)
    }
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
    /// Each change listed for the user, which the command `<owner>.discard` takes away.
    pub items: Vec<Item>,
}

/// A change of a module waiting for the server, as the user reads it in the panel *World database*:
/// what changes, of which entity; before and after in short, its detail; the entity selected by a
/// click (an id of the scene as text); the changes taken away with it, which need it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    /// Its key, as `<owner>.discard` is given it.
    pub key: String,
    pub text: String,
    pub before: String,
    pub after: String,
    pub detail: String,
    pub select: Option<String>,
    /// The keys of the changes of its module taken away with it.
    pub with: Vec<String>,
}

/// Published once the server chosen is seen started again while it held changes set in its memory
/// alone (`world-db.set_live`), not applied: lost with its memory, they are set again by their
/// modules.
pub const SERVER_RESTARTED_TOPIC: &str = "world-db.server_restarted";

/// Published once the user chose another server (step 16.7), its name in `{ "server": .. }`: the
/// modules that read it, or follow it, read it again through `WorldDb::server`.
pub const SERVER_TOPIC: &str = "world-db.server";

/// A server the user declared (step 16.7): its name, its world database, its observer (the port and
/// token of `mod-uniwow-observer`, the port of its world), its bridge (`mod-uniwow-bridge`), the
/// folder of SQL its applications are written to. The password of its database is kept apart, in
/// the Credential Manager of Windows, never here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub observer_port: u16,
    pub observer_token: String,
    pub world_port: u16,
    pub bridge_port: u16,
    pub bridge_token: String,
    pub sql_folder: String,
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

    /// The server chosen, none before the module knows it.
    fn server(&self) -> Option<Server> {
        None
    }

    /// The servers declared, in their order.
    fn servers(&self) -> Vec<Server> {
        Vec::new()
    }
}
