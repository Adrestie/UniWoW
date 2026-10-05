//! The connection to the observer, on a thread of its own: it connects, says why when it cannot,
//! subscribes to the zone around the camera on the map the terrain shows, follows the camera, and
//! keeps the shared snapshot of the entities. Every wait is bounded, its cancellation checked in
//! between.

use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use uniwow_api::server_link::protocol::{FromObserver, State, ToObserver, Zone};
use uniwow_api::server_link::{Client, Error};

use crate::world::{Change, World};

/// How long a read waits, and so how often the camera is looked at.
pub const STEP: Duration = Duration::from_millis(100);
/// How long a connection or its welcome is waited for.
const CONNECT: Duration = Duration::from_secs(1);
/// Between two attempts to connect, and after a refusal.
const RETRY: Duration = Duration::from_secs(2);
const RETRY_REFUSED: Duration = Duration::from_secs(10);
/// Between two looks at the port of the worldserver, each of which costs it a query of its
/// database of logins.
const PROBE_WORLD: Duration = Duration::from_secs(30);
/// A message at least this often keeps the connection alive.
const HEARTBEAT: Duration = Duration::from_secs(2);
/// Two subscriptions that move the zone are this far apart in time at least.
pub const MOVE_EVERY: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub port: u16,
    /// The port of the worldserver for the game, which tells a server stopped from an observer
    /// missing.
    pub world_port: u16,
    pub token: String,
    pub radius: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: 8087,
            world_port: 8085,
            token: String::new(),
            radius: 300.0,
        }
    }
}

/// Where the connection stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    NoToken,
    Connecting,
    /// Nothing listens, neither the observer nor the worldserver.
    ServerStopped,
    /// The worldserver listens, the observer does not.
    ObserverMissing,
    Refused(String),
    Broken(String),
    Connected,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub connection: Connection,
    /// The version of the server and the commit of AzerothCore, once welcomed.
    pub server: String,
    pub commit: String,
    /// The zone subscribed to and where its subscription stands.
    pub zone: Option<Zone>,
    pub state: Option<State>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            connection: Connection::Connecting,
            server: String::new(),
            commit: String::new(),
            zone: None,
            state: None,
        }
    }
}

/// What the thread shares with the interface and the commands: each read under a brief lock.
#[derive(Default)]
pub struct Shared {
    world: Mutex<Arc<World>>,
    status: Mutex<Status>,
    /// Whether another connection took its place, after which it changes and tells nothing; held
    /// while the world is changed and told, so that nothing is told after the change that
    /// retires it.
    retired: Mutex<bool>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    pub fn world(&self) -> Arc<World> {
        lock(&self.world).clone()
    }

    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    /// The thread stopped for `why`, which nothing else will say.
    pub fn stopped(&self, why: String) {
        self.set_connection(Connection::Broken(why));
    }

    /// Puts `world` in place and tells `change`, unless it was retired.
    fn tell(&self, world: World, change: &Change, surroundings: &dyn Surroundings) {
        let retired = lock(&self.retired);
        if *retired {
            return;
        }
        *lock(&self.world) = Arc::new(world);
        if !change.is_empty() {
            surroundings.changed(change);
        }
    }

    /// Retires it, its entities gone: the change that says so, for the one taking its place to
    /// tell; nothing is told after it.
    pub fn retire(&self) -> Change {
        let mut retired = lock(&self.retired);
        *retired = true;
        let (world, change) = self.world().emptied();
        *lock(&self.world) = Arc::new(world);
        change
    }

    fn set_connection(&self, connection: Connection) {
        let mut status = lock(&self.status);
        if connection != Connection::Connected {
            status.zone = None;
            status.state = None;
        }
        status.connection = connection;
    }
}

/// What the thread reads of the editor, and tells it.
pub trait Surroundings {
    /// The position of the camera, in the yards of the game.
    fn camera(&self) -> Option<[f32; 3]>;
    /// The id of the map the terrain shows.
    fn map(&self) -> Option<u32>;
    /// What a message of the observer changed, never empty.
    fn changed(&self, change: &Change);
}

/// Whether to subscribe to `wanted`, `sent` being the zone last subscribed to, `since` ago: at once
/// on another map or instance; once its centre moved more than an eighth of its radius, or its
/// radius changed, after `MOVE_EVERY` at least.
pub fn should_subscribe(sent: Option<&Zone>, wanted: &Zone, since: Duration) -> bool {
    let Some(sent) = sent else {
        return true;
    };
    if sent.map != wanted.map || sent.instance != wanted.instance {
        return true;
    }
    let moved = (sent.centre[0] - wanted.centre[0]).hypot(sent.centre[1] - wanted.centre[1]);
    (moved > wanted.radius / 8.0 || sent.radius != wanted.radius) && since >= MOVE_EVERY
}

/// Runs until `cancelled` says so.
pub fn run(settings: &Settings, shared: &Shared, surroundings: &dyn Surroundings, cancelled: &dyn Fn() -> bool) {
    let address = SocketAddr::from(([127, 0, 0, 1], settings.port));
    // What the port of the worldserver said, and when: looked at again 30 seconds later at most.
    let mut probed: Option<(Instant, Connection)> = None;
    while !cancelled() {
        if settings.token.is_empty() {
            shared.set_connection(Connection::NoToken);
            wait(RETRY, cancelled);
            continue;
        }
        let pause = match Client::connect(address, &settings.token, CONNECT) {
            Ok(client) => {
                probed = None;
                let ended = session(client, settings, shared, surroundings, cancelled);
                empty(shared, surroundings);
                shared.set_connection(ended);
                RETRY
            }
            Err(Error::Refused(reason)) => {
                shared.set_connection(Connection::Refused(reason));
                RETRY_REFUSED
            }
            Err(Error::Io(_)) => {
                let connection = match &probed {
                    Some((when, connection)) if when.elapsed() < PROBE_WORLD => connection.clone(),
                    _ => {
                        let world = SocketAddr::from(([127, 0, 0, 1], settings.world_port));
                        let connection = match TcpStream::connect_timeout(&world, CONNECT) {
                            Ok(_) => Connection::ObserverMissing,
                            Err(_) => Connection::ServerStopped,
                        };
                        probed = Some((Instant::now(), connection.clone()));
                        connection
                    }
                };
                shared.set_connection(connection);
                RETRY
            }
            Err(error) => {
                shared.set_connection(Connection::Broken(error.to_string()));
                RETRY
            }
        };
        wait(pause, cancelled);
    }
    empty(shared, surroundings);
}

/// Waits `time`, looking at `cancelled` every `STEP`.
fn wait(time: Duration, cancelled: &dyn Fn() -> bool) {
    let end = Instant::now() + time;
    while !cancelled() && Instant::now() < end {
        std::thread::sleep(STEP.min(end.saturating_duration_since(Instant::now())));
    }
}

/// The entities gone, said once.
fn empty(shared: &Shared, surroundings: &dyn Surroundings) {
    let (world, change) = shared.world().emptied();
    shared.tell(world, &change, surroundings);
}

/// A connection welcomed, until it breaks or `cancelled`: what it ended with.
fn session(
    mut client: Client,
    settings: &Settings,
    shared: &Shared,
    surroundings: &dyn Surroundings,
    cancelled: &dyn Fn() -> bool,
) -> Connection {
    {
        let mut status = lock(&shared.status);
        status.connection = Connection::Connected;
        status.server = client.welcome().server.clone();
        status.commit = client.welcome().commit.clone();
    }
    let radius = settings.radius.min(client.welcome().max_radius);
    let mut sent: Option<Zone> = None;
    let mut subscribed = Instant::now();
    let mut last_sent = Instant::now();
    while !cancelled() {
        // Follow the camera on the map the terrain shows.
        let wanted = surroundings.map().zip(surroundings.camera()).map(|(map, centre)| Zone {
            map,
            instance: 0,
            centre,
            radius,
        });
        let message = match (&wanted, &sent) {
            (Some(wanted), _) if should_subscribe(sent.as_ref(), wanted, subscribed.elapsed()) => {
                Some(ToObserver::Subscribe(*wanted))
            }
            (None, Some(_)) => Some(ToObserver::Unsubscribe),
            _ if last_sent.elapsed() >= HEARTBEAT => Some(ToObserver::Heartbeat),
            _ => None,
        };
        if let Some(message) = message {
            if let Err(error) = client.send(&message) {
                return lost(error);
            }
            last_sent = Instant::now();
            match message {
                ToObserver::Subscribe(zone) => {
                    sent = Some(zone);
                    subscribed = Instant::now();
                    lock(&shared.status).zone = Some(zone);
                }
                ToObserver::Unsubscribe => {
                    sent = None;
                    lock(&shared.status).zone = None;
                    lock(&shared.status).state = None;
                    empty(shared, surroundings);
                }
                _ => {}
            }
        }

        let received = match client.receive(STEP) {
            Ok(Some(received)) => received,
            Ok(None) => continue,
            Err(error) => return lost(error),
        };
        let now = Instant::now();
        let (world, change) = match received {
            FromObserver::Status { state, .. } => {
                lock(&shared.status).state = Some(state);
                continue;
            }
            FromObserver::Snapshot {
                map,
                instance,
                sequence,
                entities,
            } => shared.world().snapshot(map, instance, sequence, entities, now),
            FromObserver::Changes {
                sequence,
                entities,
                left,
                ..
            } => shared.world().changes(sequence, entities, &left, now),
            FromObserver::Welcome(_) | FromObserver::Refused(_) => {
                return Connection::Broken("a WELCOME or REFUSED after the welcome".to_owned());
            }
        };
        shared.tell(world, &change, surroundings);
    }
    Connection::Connecting
}

/// What a connection lost to `error` ended with.
fn lost(error: Error) -> Connection {
    match error {
        Error::Closed | Error::Io(_) => Connection::Connecting,
        error => Connection::Broken(error.to_string()),
    }
}
