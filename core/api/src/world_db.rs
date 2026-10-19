//! Interface of the "world-db" service (step 15.11a): the world database of AzerothCore, read only,
//! through the connection the module `world-db` keeps on a thread of its own. Ask from a job: a query
//! waits for its rows.

use std::sync::Arc;

use crate::ServiceKey;
pub use crate::db::Rows;

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("world-db");

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

/// Shared between threads (rule T3).
pub trait WorldDb: Send + Sync {
    fn state(&self) -> State;

    /// The rows of `sql`, one statement beginning with `SELECT`, `SHOW`, `DESCRIBE` or `EXPLAIN`,
    /// its `?` given `parameters`. From a job or a thread of its own: it waits for the rows.
    fn query(&self, sql: &str, parameters: &[serde_json::Value]) -> Result<Rows, String>;
}
