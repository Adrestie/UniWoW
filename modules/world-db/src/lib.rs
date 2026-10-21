//! The world database of AzerothCore (step 15.11a), read only: its address, port, user and database
//! in the panel *World database*; its password asked in a window of its own, masked, and kept, when
//! the user leaves *Remember password* checked, in the Credential Manager of Windows once the
//! connection is made (never in the settings, the project, the log, the notices, nor a message of
//! failure). With *Connect automatically at startup* (checked by default) and a password kept, the
//! editor connects at its start without the window, which opens again if refused. The connection
//! lives on a thread of its own, its session read only; the service `world-db` and the command
//! `world-db.query` run one statement at a time beginning with `SELECT`, `SHOW`, `DESCRIBE` or
//! `EXPLAIN`.
//!
//! The changes for the server (step 16.1): the modules give those they hold; *Apply to the server…*
//! writes them all, once confirmed, through a connection that writes opened for them alone, one
//! transaction; each application is kept in the project with the rows it replaced, *Undo the last
//! application…* putting them back, and written as a script into the folder of SQL of a module of
//! the server when one is set, for a server installed again to have them.

mod bridge;
mod exchange;
mod live;
mod package;
mod sending;
mod servers;
#[cfg(test)]
mod tests;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use uniwow_api::db::secret::{self, Vault};
use uniwow_api::db::write::{self, Entry, RowChange};
use uniwow_api::db::{Connection, Rows, Settings, Writer};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, Pending, Reload, Server, State};
use uniwow_api::{Context, DockArea, JobId, JobOutcome, Module, Registrar, egui, journal, log, theme};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long a connection or a query waits at most.
const TIMEOUT: Duration = Duration::from_secs(10);

/// What the thread of the connection is asked.
enum Asked {
    Connect(Settings, String),
    Query(String, Vec<Value>, Sender<Result<Rows, String>>),
    Apply(Vec<RowChange>, Sender<Result<Vec<Entry>, String>>),
}

/// What the service shares with the thread of the connection.
#[derive(Default)]
struct Shared {
    state: Mutex<Option<State>>,
    asked: Mutex<Option<Sender<Asked>>>,
    /// The changes the modules hold for the server, by owner.
    pending: Mutex<std::collections::BTreeMap<String, Pending>>,
    /// The servers declared and the one chosen (step 16.7); a server chosen from the toolbar of the
    /// 3D view, and the window of the servers asked from there, for the module.
    servers: Mutex<Vec<Server>>,
    server: Mutex<Option<Server>>,
    chose: Mutex<Option<String>>,
    manage: std::sync::atomic::AtomicBool,
    /// The creatures dragged in the world, moved live by a thread of their own.
    moves: live::Moves,
}

impl Shared {
    fn set(&self, state: State) {
        *lock(&self.state) = Some(state);
    }
}

struct Service(Arc<Shared>);

impl world_db::WorldDb for Service {
    fn state(&self) -> State {
        lock(&self.0.state).clone().unwrap_or(State::Waiting)
    }

    fn query(&self, sql: &str, parameters: &[Value]) -> Result<Rows, String> {
        if journal::on_interface_thread() {
            return Err("a query waits for its rows: ask from a job, not from the interface thread".to_owned());
        }
        uniwow_api::db::allowed(sql)?;
        let (reply, answer) = mpsc::channel();
        lock(&self.0.asked)
            .as_ref()
            .ok_or("the world database is not connected")?
            .send(Asked::Query(sql.to_owned(), parameters.to_vec(), reply))
            .map_err(|_| "the world database is not connected".to_owned())?;
        answer
            .recv_timeout(TIMEOUT * 3)
            .map_err(|_| "the world database did not answer".to_owned())?
    }

    fn set_pending(&self, pending: Pending) {
        let mut held = lock(&self.0.pending);
        if pending.changes.is_empty() && pending.held.is_empty() {
            held.remove(&pending.owner);
        } else {
            held.insert(pending.owner.clone(), pending);
        }
    }

    fn pending(&self) -> Vec<Pending> {
        lock(&self.0.pending).values().cloned().collect()
    }

    fn server(&self) -> Option<Server> {
        lock(&self.0.server).clone()
    }

    fn servers(&self) -> Vec<Server> {
        lock(&self.0.servers).clone()
    }

    fn apply(&self, changes: &[RowChange]) -> Result<Vec<Entry>, String> {
        if journal::on_interface_thread() {
            return Err("an application waits for the database: make it from a job".to_owned());
        }
        for change in changes {
            write::check(change)?;
        }
        let (reply, answer) = mpsc::channel();
        lock(&self.0.asked)
            .as_ref()
            .ok_or("the world database is not connected")?
            .send(Asked::Apply(changes.to_vec(), reply))
            .map_err(|_| "the world database is not connected".to_owned())?;
        answer
            .recv_timeout(TIMEOUT * 30)
            .map_err(|_| "the world database did not answer".to_owned())?
    }
}

/// The thread of the connection: connects when asked, with the password given, kept here alone to
/// connect again; runs the queries one at a time.
fn serve(shared: Arc<Shared>, asked: Receiver<Asked>, cancelled: impl Fn() -> bool) {
    let mut connection: Option<Connection> = None;
    let mut account: Option<(Settings, String)> = None;
    while !cancelled() {
        let Ok(ask) = asked.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        match ask {
            Asked::Connect(settings, password) => {
                shared.set(State::Connecting);
                connection = None;
                match Connection::open(&settings, &password, TIMEOUT) {
                    Ok(mut opened) => {
                        // Its database said: another of the same MySQL is another server.
                        let version = opened.version().unwrap_or_default();
                        shared.set(State::Connected(format!("{version} · {}", settings.database)));
                        connection = Some(opened);
                        account = Some((settings, password));
                    }
                    Err(reason) => {
                        shared.set(State::Refused(reason));
                        account = None;
                    }
                }
            }
            Asked::Query(sql, parameters, reply) => {
                // Lost: connected again once, with the password of the session.
                if connection.is_none()
                    && let Some((settings, password)) = &account
                {
                    connection = Connection::open(settings, password, TIMEOUT).ok();
                }
                let result = match connection.as_mut() {
                    Some(open) => open.query(&sql, &parameters),
                    None => Err("the world database is not connected".to_owned()),
                };
                if result
                    .as_ref()
                    .is_err_and(|error| error.contains("broken pipe") || error.contains("Connection"))
                {
                    connection = None;
                }
                let _ = reply.send(result);
            }
            Asked::Apply(changes, reply) => {
                // A connection that writes, for these changes alone, with the account of the session.
                let result = match &account {
                    Some((settings, password)) => {
                        Writer::open(settings, password, TIMEOUT).and_then(|mut writer| writer.apply(&changes))
                    }
                    None => Err("the world database is not connected".to_owned()),
                };
                let _ = reply.send(result);
            }
        }
    }
}

/// Changes applied to the database: when, where, by whose changes, what each did and what the
/// server loads again.
#[derive(Clone, Debug, PartialEq)]
struct Application {
    when: String,
    /// The server it was made on, by its name; empty for one made before servers were.
    server: String,
    database: String,
    owners: Vec<String>,
    entries: Vec<Entry>,
    reloads: Vec<Reload>,
}

fn reload_json(reload: &Reload) -> Value {
    reload.to_json()
}

fn reload_of(value: &Value) -> Option<Reload> {
    Reload::of_json(value)
}

impl Application {
    fn rows(&self) -> usize {
        self.entries.iter().filter(|entry| entry.before != entry.after).count()
    }

    fn to_json(&self) -> Value {
        json!({
            "when": self.when,
            "server": self.server,
            "database": self.database,
            "owners": self.owners,
            "entries": self.entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
            "reloads": self.reloads.iter().map(reload_json).collect::<Vec<_>>(),
        })
    }

    fn of_json(value: &Value) -> Option<Self> {
        let list = |key: &str| value[key].as_array().cloned().unwrap_or_default();
        Some(Self {
            when: value["when"].as_str()?.to_owned(),
            server: value["server"].as_str().unwrap_or_default().to_owned(),
            database: value["database"].as_str().unwrap_or_default().to_owned(),
            owners: list("owners")
                .iter()
                .filter_map(|owner| owner.as_str().map(str::to_owned))
                .collect(),
            entries: list("entries").iter().map(Entry::of_json).collect::<Option<_>>()?,
            reloads: list("reloads").iter().filter_map(reload_of).collect(),
        })
    }
}

/// The date and time of the moment `since` seconds after 1970 in UTC, as `2026-10-21 07:48:00`.
fn when(since: u64) -> String {
    let (days, seconds) = (since / 86_400, since % 86_400);
    // From the days since 1970 to the civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 { shifted + 3 } else { shifted - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn now() -> String {
    when(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    )
}

/// The name of the script of an application made `when`: `uniwow_2026_10_21_074800.sql`, the
/// scripts of a folder ordered as they were made.
fn script_name(when: &str, undone: bool) -> String {
    let digits: String = when.chars().filter(char::is_ascii_digit).collect();
    let (date, time) = digits.split_at(digits.len().min(8));
    let (year, rest) = date.split_at(date.len().min(4));
    let (month, day) = rest.split_at(rest.len().min(2));
    format!(
        "uniwow_{year}_{month}_{day}_{time}{}.sql",
        if undone { "_undone" } else { "" }
    )
}

/// An application asked, waiting for its job: the changes applied, or the last application undone.
enum Applying {
    Apply {
        job: JobId,
        owners: Vec<String>,
        reloads: Vec<Reload>,
        database: String,
    },
    Undo {
        job: JobId,
    },
}

/// What the user is asked to confirm.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Confirming {
    Apply,
    Undo,
    /// Another server chosen while changes wait for this one.
    Switch(String),
}

/// Writes the script of `entries` into `folder`, if one is given: the row of `updates` telling the
/// server's updater it is applied already, once written; what went wrong.
fn write_script_to(
    folder: &str,
    title: &str,
    when: &str,
    undone: bool,
    entries: &[Entry],
) -> (Option<RowChange>, Option<String>) {
    if folder.trim().is_empty() {
        return (None, None);
    }
    // Two in the same second, as the changes applied at once are: the second numbered.
    let mut name = script_name(when, undone);
    let mut path = std::path::Path::new(folder.trim()).join(&name);
    let mut count = 1;
    while path.exists() {
        count += 1;
        name = script_name(when, undone).replace(".sql", &format!("_{count}.sql"));
        path = std::path::Path::new(folder.trim()).join(&name);
    }
    let content = write::script(title, entries);
    match std::fs::create_dir_all(folder.trim()).and_then(|_| std::fs::write(&path, &content)) {
        Ok(()) => (Some(write::applied_script(&name, &content)), None),
        Err(error) => (
            None,
            Some(format!("the script {} is not written: {error}", path.display())),
        ),
    }
}

/// The setting of the connection made at the start of the editor with the password kept.
const AT_START: &str = "connect_at_start";

/// What the module does at its start: connect with the password kept, or ask it.
#[derive(Debug, PartialEq)]
enum Start {
    Connect(String),
    Ask,
}

/// What the module does at its start, connecting at the start as `at_start` says, the password kept
/// for the connection being `kept`.
fn start(at_start: bool, kept: Result<Option<String>, String>) -> Start {
    match (at_start, kept) {
        (true, Ok(Some(password))) if !password.is_empty() => Start::Connect(password),
        (_, Err(reason)) => {
            log::warn!("{reason}");
            Start::Ask
        }
        _ => Start::Ask,
    }
}

/// A connection asked, waiting for its outcome: the password to keep once connected (none when
/// the user chose not to), and whether it was made at the start without the window.
struct Asking {
    keep: Option<(String, String, String)>,
    automatic: bool,
}

/// The servers declared in the settings, the module's settings of before made one at first.
fn servers_of(ctx: &Context) -> Vec<Server> {
    servers::read(ctx.setting(servers::SERVERS).as_ref(), |key| ctx.setting(key))
}

/// The server chosen: the one the settings name, else the first.
fn server_of(ctx: &Context) -> Server {
    let servers = servers_of(ctx);
    let chosen = ctx
        .setting(servers::CHOSEN)
        .and_then(|value| value.as_str().map(str::to_owned));
    servers
        .iter()
        .find(|server| Some(&server.name) == chosen.as_ref())
        .or(servers.first())
        .cloned()
        .unwrap_or_default()
}

fn settings_of(ctx: &Context) -> Settings {
    servers::settings(&server_of(ctx))
}

/// The window of the servers: the list being edited, the one shown.
struct Editing {
    servers: Vec<Server>,
    at: usize,
}

/// The tool of the 3D view whose toolbar chooses the server.
struct ServersTool(Arc<Shared>);

impl uniwow_api::viewport::Tool for ServersTool {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let servers = lock(&self.0.servers).clone();
        let chosen = lock(&self.0.server)
            .as_ref()
            .map(|server| server.name.clone())
            .unwrap_or_default();
        ui.separator();
        egui::ComboBox::from_id_salt("world-db server")
            .selected_text(format!("Server: {chosen}"))
            .show_ui(ui, |ui| {
                for server in &servers {
                    if ui
                        .selectable_label(server.name == chosen, &server.name)
                        .on_hover_text(format!(
                            "Read and write {}: its world database, its observer, its bridge. Writes nothing.",
                            server.name
                        ))
                        .clicked()
                        && server.name != chosen
                    {
                        *lock(&self.0.chose) = Some(server.name.clone());
                    }
                }
                ui.separator();
                if ui
                    .button("Servers…")
                    .on_hover_text("Declare the servers: their databases, observers and bridges")
                    .clicked()
                {
                    self.0.manage.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            })
            .response
            .on_hover_text("The server the editor reads and writes: its world database, observer and bridge");
    }
}

struct WorldDbModule {
    shared: Arc<Shared>,
    thread: Option<JobId>,
    /// The password being typed, kept until *Connect*.
    password: String,
    asking: bool,
    /// Whether the password typed is kept once connected (*Remember password*).
    remember: bool,
    pending: Option<Asking>,
    /// Where passwords are kept, and whether one is for the connection of the settings.
    vault: Arc<dyn Vault>,
    kept: Option<bool>,
    /// The applications made, the last last, kept in the project; the one being made; what is
    /// asked to be confirmed; what the last one said.
    applications: Vec<Application>,
    applying: Option<Applying>,
    confirming: Option<Confirming>,
    /// The window of the servers, while open.
    editing_servers: Option<Editing>,
    /// The applications picked to be sent to another server, and the sending under way.
    picked: std::collections::BTreeSet<usize>,
    sending: Option<sending::Sending>,
    /// The export to a file, and the import of one, under way.
    exporting: Option<JobId>,
    importing: Option<exchange::Importing>,
    said: Option<(bool, String)>,
    /// The bridge of the server asked to load the changes again, and the quests it is asked to take
    /// from the characters.
    bridging: Option<JobId>,
    forgetting: Vec<u32>,
    /// The changes applied at once, waiting, and the one being written with the database it goes to.
    live: std::collections::VecDeque<LiveApply>,
    writing_live: Option<(JobId, LiveApply, String)>,
}

/// A change applied at once, as it is made (a spawn point dragged, a patrol changed): its owner, what
/// it is, the ticket its owner knows it by, its rows and what the server loads again.
struct LiveApply {
    owner: String,
    label: String,
    ticket: u64,
    changes: Vec<RowChange>,
    reloads: Vec<Reload>,
}

impl LiveApply {
    fn of(arguments: &Value) -> Result<Self, String> {
        let list = |key: &str| arguments[key].as_array().cloned().unwrap_or_default();
        Ok(Self {
            owner: arguments["owner"]
                .as_str()
                .ok_or("'owner' names the module")?
                .to_owned(),
            label: arguments["label"].as_str().unwrap_or("A change").to_owned(),
            ticket: arguments["ticket"].as_u64().ok_or("'ticket' is a number")?,
            changes: list("changes")
                .iter()
                .map(|change| RowChange::of_json(change).ok_or("'changes' are rows changed"))
                .collect::<Result<_, _>>()?,
            reloads: list("reloads").iter().filter_map(reload_of).collect(),
        })
    }
}

impl Default for WorldDbModule {
    fn default() -> Self {
        Self {
            shared: Arc::default(),
            thread: None,
            password: String::new(),
            asking: false,
            remember: true,
            pending: None,
            vault: Arc::new(secret::System),
            kept: None,
            applications: Vec::new(),
            applying: None,
            confirming: None,
            editing_servers: None,
            picked: std::collections::BTreeSet::new(),
            sending: None,
            exporting: None,
            importing: None,
            said: None,
            bridging: None,
            forgetting: Vec::new(),
            live: std::collections::VecDeque::new(),
            writing_live: None,
        }
    }
}

impl WorldDbModule {
    /// Connects with `password`, kept once connected when `keep`, made at the start when
    /// `automatic`.
    fn connect_with(&mut self, ctx: &mut Context, password: String, keep: bool, automatic: bool) {
        let settings = settings_of(ctx);
        self.pending = Some(Asking {
            keep: keep.then(|| (secret::target(&settings), settings.user.clone(), password.clone())),
            automatic,
        });
        if !keep && let Err(reason) = self.vault.forget(&secret::target(&settings)) {
            log::warn!("{reason}");
        }
        self.kept = None;
        // Connecting from now: a refusal said before is no outcome of this one.
        self.shared.set(State::Connecting);
        if let Some(asked) = lock(&self.shared.asked).as_ref() {
            let _ = asked.send(Asked::Connect(settings, password));
        }
        self.asking = false;
    }

    fn connect(&mut self, ctx: &mut Context) {
        let password = std::mem::take(&mut self.password);
        self.connect_with(ctx, password, self.remember, false);
    }

    /// The outcome of the connection asked: its password kept once connected; the window opened
    /// again when one made at the start is refused.
    fn settle(&mut self) {
        use world_db::WorldDb as _;
        let state = Service(self.shared.clone()).state();
        let Some(pending) = &mut self.pending else {
            return;
        };
        match state {
            State::Connected(_) => {
                if let Some((target, user, password)) = pending.keep.take()
                    && let Err(reason) = self.vault.set(&target, &user, &password)
                {
                    log::warn!("{reason}");
                }
                self.pending = None;
                self.kept = None;
            }
            State::Refused(_) => {
                self.asking |= pending.automatic;
                self.pending = None;
            }
            State::Waiting | State::Connecting => {}
        }
    }

    /// Applies the changes the modules hold, from a job.
    fn apply(&mut self, ctx: &mut Context) {
        use world_db::WorldDb as _;
        let service = Service(self.shared.clone());
        let pending = service.pending();
        let changes: Vec<RowChange> = pending.iter().flat_map(|held| held.changes.clone()).collect();
        if changes.is_empty() {
            return;
        }
        let mut reloads: Vec<Reload> = pending.iter().flat_map(|held| held.reloads.clone()).collect();
        reloads.sort();
        reloads.dedup();
        let owners = pending.iter().map(|held| held.owner.clone()).collect();
        let settings = settings_of(ctx);
        let shared = self.shared.clone();
        let job = ctx.spawn("Apply the changes to the world database", move |_| {
            Service(shared).apply(&changes)
        });
        self.applying = Some(Applying::Apply {
            job,
            owners,
            reloads,
            database: format!("{} on {}:{}", settings.database, settings.host, settings.port),
        });
    }

    /// Undoes the last application, from a job.
    /// The last application made on the server chosen, by its place.
    fn last_here(&self, ctx: &Context) -> Option<usize> {
        let here = server_of(ctx).name;
        self.applications
            .iter()
            .rposition(|application| application.server.is_empty() || application.server == here)
    }

    fn undo(&mut self, ctx: &mut Context) {
        use world_db::WorldDb as _;
        let Some(last) = self.last_here(ctx).map(|at| &self.applications[at]) else {
            return;
        };
        let changes = write::revert(&last.entries);
        let shared = self.shared.clone();
        let job = ctx.spawn("Undo the last application to the world database", move |_| {
            Service(shared).apply(&changes)
        });
        self.applying = Some(Applying::Undo { job });
    }

    /// Writes the script of `entries` into the folder of SQL of the server, if it has one; what went
    /// wrong.
    fn write_script(
        &mut self,
        ctx: &mut Context,
        title: &str,
        when: &str,
        undone: bool,
        entries: &[Entry],
    ) -> Option<String> {
        let (mark, failed) = write_script_to(&server_of(ctx).sql_folder, title, when, undone, entries);
        self.mark(mark, ctx);
        failed
    }

    /// The script written told applied to the updater of the server chosen, from a job: it never
    /// plays it again over what was changed in the game since.
    pub(crate) fn mark(&mut self, mark: Option<RowChange>, ctx: &mut Context) {
        let Some(mark) = mark else {
            return;
        };
        let shared = self.shared.clone();
        ctx.spawn("Tell the server's updater the script is applied", move |_| {
            use world_db::WorldDb as _;
            if let Err(reason) = Service(shared).apply(&[mark]) {
                log::warn!("the script is not told applied to the updater: {reason}");
            }
        });
    }

    /// The servers of the settings given to the service; the modules told once another is chosen.
    fn sync_servers(&mut self, ctx: &mut Context) {
        let (servers, server) = (servers_of(ctx), server_of(ctx));
        let before = lock(&self.shared.server).clone();
        *lock(&self.shared.servers) = servers;
        *lock(&self.shared.server) = Some(server.clone());
        if before.as_ref().is_some_and(|before| *before != server) {
            ctx.publish(world_db::SERVER_TOPIC, json!({ "server": server.name }));
        }
    }

    /// Chooses the server `name`: once confirmed when changes wait for this one; connected with
    /// its password kept, or the password asked.
    fn choose(&mut self, name: &str, confirmed: bool, ctx: &mut Context) {
        use world_db::WorldDb as _;
        if !confirmed && !Service(self.shared.clone()).pending().is_empty() {
            self.confirming = Some(Confirming::Switch(name.to_owned()));
            return;
        }
        ctx.set_setting(servers::CHOSEN, json!(name));
        self.kept = None;
        self.sync_servers(ctx);
        log::info!("the server chosen: {name}");
        match start(true, self.vault.get(&secret::target(&settings_of(ctx)))) {
            Start::Connect(password) => self.connect_with(ctx, password, true, true),
            Start::Ask => {
                self.shared.set(State::Waiting);
                self.asking = true;
            }
        }
    }

    /// The window of the servers: each declared, its database, observer and bridge; added,
    /// duplicated, removed; written to the settings once saved.
    fn servers_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        if self.shared.manage.swap(false, std::sync::atomic::Ordering::Relaxed) && self.editing_servers.is_none() {
            let servers = servers_of(ctx);
            let chosen = server_of(ctx).name;
            let at = servers.iter().position(|server| server.name == chosen).unwrap_or(0);
            self.editing_servers = Some(Editing { servers, at });
        }
        let Some(editing) = &mut self.editing_servers else {
            return;
        };
        let (mut open, mut save, mut close) = (true, false, false);
        // Of a size of its own: a window that fits its content grows with a vertical line or a text
        // that wraps.
        egui::Window::new("Servers")
            .open(&mut open)
            .resizable(false)
            .fixed_size([600.0, 430.0])
            .show(egui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        for (at, server) in editing.servers.iter().enumerate() {
                            if ui
                                .selectable_label(editing.at == at, &server.name)
                                .on_hover_text("Shows its settings on the right")
                                .clicked()
                            {
                                editing.at = at;
                            }
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button("Add")
                                .on_hover_text("A new server to fill in; saved with Save")
                                .clicked()
                            {
                                let name = servers::new_name(&editing.servers, "Server");
                                let mut server = servers::of_json(&json!({ "name": name })).unwrap_or_default();
                                server.name = name;
                                editing.servers.push(server);
                                editing.at = editing.servers.len() - 1;
                            }
                            if ui
                                .button("Duplicate")
                                .on_hover_text("A copy of the server shown, to change; saved with Save")
                                .clicked()
                                && let Some(server) = editing.servers.get(editing.at).cloned()
                            {
                                let name = servers::new_name(&editing.servers, &server.name);
                                editing.servers.push(Server { name, ..server });
                                editing.at = editing.servers.len() - 1;
                            }
                            if ui
                                .add_enabled(editing.servers.len() > 1, egui::Button::new("Remove"))
                                .on_hover_text("Takes the server shown out of the list once saved; its database is not touched")
                                .clicked()
                            {
                                editing.servers.remove(editing.at);
                                editing.at = editing.at.min(editing.servers.len() - 1);
                            }
                        });
                    });
                    ui.add_space(12.0);
                    let Some(server) = editing.servers.get_mut(editing.at) else {
                        return;
                    };
                    egui::Grid::new("world-db server fields")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            let text = |ui: &mut egui::Ui, label: &str, value: &mut String, secret: bool, hint: &str| {
                                ui.label(egui::RichText::new(label).weak()).on_hover_text(hint);
                                ui.add(egui::TextEdit::singleline(value).password(secret).desired_width(240.0))
                                    .on_hover_text(hint);
                                ui.end_row();
                            };
                            let port = |ui: &mut egui::Ui, label: &str, value: &mut u16, hint: &str| {
                                ui.label(egui::RichText::new(label).weak()).on_hover_text(hint);
                                ui.add(egui::DragValue::new(value)).on_hover_text(hint);
                                ui.end_row();
                            };
                            let heading = |ui: &mut egui::Ui, label: &str, hint: &str| {
                                ui.strong(label).on_hover_text(hint);
                                ui.end_row();
                            };
                            text(
                                ui,
                                "Name",
                                &mut server.name,
                                false,
                                "The name shown in the lists of servers (the toolbar of the 3D view, the panel World database)",
                            );
                            heading(
                                ui,
                                "World database",
                                "The MySQL database of the world of this server (acore_world…): its spawns, quests and patrols are read from it, and every Apply writes into it",
                            );
                            text(ui, "Address", &mut server.host, false, "The machine of MySQL: 127.0.0.1 for this one");
                            port(ui, "Port", &mut server.port, "The port of MySQL, 3306 by default");
                            text(
                                ui,
                                "User",
                                &mut server.user,
                                false,
                                "The account of MySQL; its password is asked once, then kept in the Credential Manager",
                            );
                            text(
                                ui,
                                "Database",
                                &mut server.database,
                                false,
                                "The name of the world database: acore_world, acore_world_test…",
                            );
                            heading(
                                ui,
                                "Observer",
                                "mod-uniwow-observer, the module of the server that shows the world as it runs (the panel Live world): its creatures, players, hour and weather",
                            );
                            port(
                                ui,
                                "Port",
                                &mut server.observer_port,
                                "UniwowObserver.Port of mod_uniwow_observer.conf: 8087 for Nibelung, 8088 for the tests",
                            );
                            text(
                                ui,
                                "Token",
                                &mut server.observer_token,
                                true,
                                "UniwowObserver.Token of mod_uniwow_observer.conf: the observer refuses any other",
                            );
                            port(
                                ui,
                                "Port of its world",
                                &mut server.world_port,
                                "WorldServerPort of worldserver.conf: the port the game connects to, to know the server runs",
                            );
                            heading(
                                ui,
                                "Bridge",
                                "mod-uniwow-bridge, the module of the server that loads again what an Apply wrote, for it to be seen in the game at once, without starting the server again",
                            );
                            port(
                                ui,
                                "Port",
                                &mut server.bridge_port,
                                "UniwowBridge.Port of mod_uniwow_bridge.conf: 8092 for the tests, 8093 for Nibelung",
                            );
                            text(
                                ui,
                                "Token",
                                &mut server.bridge_token,
                                true,
                                "UniwowBridge.Token of mod_uniwow_bridge.conf: the bridge refuses any other",
                            );
                            text(
                                ui,
                                "Scripts written to",
                                &mut server.sql_folder,
                                false,
                                r"Optional. A folder each Apply is also written to as an SQL file (uniwow_<date>_<time>.sql), which can be played again as many times as wanted: the folder data\sql\db-world of a module of the server, which the server plays again when installed. Empty: the database alone.",
                            );
                        });
                });
                ui.label(
                    egui::RichText::new(
                        "The password of each database is asked when it is chosen, and kept in the Credential \
                         Manager of Windows.",
                    )
                    .weak()
                    .small(),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    save = ui
                        .button("Save")
                        .on_hover_text("Keeps these servers in the settings of the editor; the server chosen connected again if it changed")
                        .clicked();
                    close = ui
                        .button("Close")
                        .on_hover_text("Closes the window; what was not saved is lost")
                        .clicked();
                });
            });
        if save {
            let mut names = std::collections::BTreeSet::new();
            let unique = editing
                .servers
                .iter()
                .all(|server| !server.name.trim().is_empty() && names.insert(server.name.trim().to_owned()));
            if unique {
                let list: Vec<Value> = editing.servers.iter().map(servers::to_json).collect();
                let chosen = server_of(ctx).name;
                let kept = editing.servers.iter().any(|server| server.name == chosen);
                let first = editing
                    .servers
                    .first()
                    .map(|server| server.name.clone())
                    .unwrap_or_default();
                ctx.set_setting(servers::SERVERS, json!(list));
                if !kept {
                    ctx.set_setting(servers::CHOSEN, json!(first));
                }
                let before = lock(&self.shared.server).clone();
                self.sync_servers(ctx);
                // The server chosen changed: connected again.
                if before != lock(&self.shared.server).clone() {
                    let name = server_of(ctx).name;
                    self.choose(&name, true, ctx);
                }
            } else {
                self.said = Some((false, "Each server has a name of its own".to_owned()));
            }
        }
        if close || !open {
            self.editing_servers = None;
        }
    }

    /// Whether a password is kept for the connection of the settings, asked of the vault once.
    fn is_kept(&mut self, ctx: &Context) -> bool {
        *self.kept.get_or_insert_with(|| {
            self.vault
                .get(&secret::target(&settings_of(ctx)))
                .is_ok_and(|kept| kept.is_some())
        })
    }
}

impl Module for WorldDbModule {
    fn register(&mut self, reg: &mut Registrar) {
        let service: world_db::Handle = Arc::new(Service(self.shared.clone()));
        let (for_query, for_state, for_moves) = (self.shared.clone(), self.shared.clone(), self.shared.clone());
        reg.panel("world-db", "World database", DockArea::Right)
            .provide(world_db::SERVICE, service)
            .command_on_caller(
                "world-db.query",
                "The rows of 'sql', one statement beginning with SELECT, SHOW, DESCRIBE or EXPLAIN, its ? given \
                 'parameters', from the world database, read only: its columns and its rows. From a thread other \
                 than the interface's: it waits for the rows.",
                json!({
                    "type": "object",
                    "properties": { "sql": { "type": "string" }, "parameters": { "type": "array" } },
                    "required": ["sql"]
                }),
                json!({ "type": "object", "properties": { "columns": { "type": "array" }, "rows": { "type": "array" } } }),
                Arc::new(move |arguments| {
                    use world_db::WorldDb as _;
                    let sql = arguments["sql"].as_str().ok_or("'sql' is a statement")?;
                    let parameters = arguments["parameters"].as_array().cloned().unwrap_or_default();
                    let rows = Service(for_query.clone()).query(sql, &parameters)?;
                    Ok(json!({ "columns": rows.columns, "rows": rows.rows }))
                }),
            )
            .command_on_caller(
                "world-db.state",
                "Where the connection to the world database stands: waiting (its password asked or not now), \
                 connecting, connected (and the version of the server), refused (and why).",
                json!({ "type": "object" }),
                json!({ "type": "object" }),
                Arc::new(move |_| {
                    use world_db::WorldDb as _;
                    Ok(match Service(for_state.clone()).state() {
                        State::Waiting => json!({ "state": "waiting" }),
                        State::Connecting => json!({ "state": "connecting" }),
                        State::Connected(version) => json!({ "state": "connected", "version": version }),
                        State::Refused(reason) => json!({ "state": "refused", "reason": reason }),
                    })
                }),
            )
            .command_on_caller(
                "world-db.move_live",
                "Moves the creature 'guid' (its GUID at run time, 0x… text) of 'map' and 'instance' in the world \
                 of the server chosen to 'position' facing 'orientation', live, through its bridge, nothing \
                 written: held still while dragged, moving again once 'let_go'. Sent ten times a second at \
                 most, the last of each creature. What went wrong with the moves sent last, in 'said'.",
                json!({
                    "type": "object",
                    "properties": {
                        "map": { "type": "integer" }, "instance": { "type": "integer" },
                        "guid": { "type": "string" }, "position": { "type": "array" },
                        "orientation": { "type": "number" }, "let_go": { "type": "boolean" },
                    },
                    "required": ["map", "guid", "position"]
                }),
                json!({ "type": "object" }),
                Arc::new(move |arguments| {
                    for_moves.moves.push(live::move_of(&arguments)?);
                    Ok(live::answer(&for_moves.moves))
                }),
            )
            .command(
                "world-db.apply_live",
                "Applies 'changes' (rows changed) of the module 'owner' at once, as they are made, one transaction \
                 of their own, kept as an application with its script, 'reloads' asked of the server's bridge. \
                 Told by the topic world-db.applied_live with the 'ticket' given: the rows replaced, or why not.",
                json!({
                    "type": "object",
                    "properties": {
                        "owner": { "type": "string" }, "label": { "type": "string" }, "ticket": { "type": "integer" },
                        "changes": { "type": "array" }, "reloads": { "type": "array" },
                    },
                    "required": ["owner", "ticket", "changes"]
                }),
                json!({ "type": "object" }),
            );
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        match name {
            "world-db.apply_live" => {
                let live = LiveApply::of(&arguments)?;
                let waiting = self.live.len() + usize::from(self.writing_live.is_some());
                self.live.push_back(live);
                self.write_live(ctx);
                Ok(json!({ "waiting": waiting }))
            }
            other => Err(format!("'{other}' is not handled")),
        }
    }

    fn init(&mut self, ctx: &mut Context) {
        let (asked, receiver) = mpsc::channel();
        *lock(&self.shared.asked) = Some(asked);
        let shared = self.shared.clone();
        self.thread = Some(ctx.spawn_thread("The connection to the world database", move |job| {
            serve(shared, receiver, || job.is_cancelled());
        }));
        let shared = self.shared.clone();
        ctx.spawn_thread("Move the creatures dragged in the world, live", move |job| {
            let server = || {
                lock(&shared.server)
                    .as_ref()
                    .map(|server| (server.bridge_port, server.bridge_token.clone()))
            };
            live::run(&shared.moves, server, || job.is_cancelled());
        });
        self.shared.set(State::Waiting);
        self.sync_servers(ctx);
        // For the modules started before it, which follow the server chosen.
        ctx.publish(world_db::SERVER_TOPIC, json!({ "server": server_of(ctx).name }));
        if let Some(view) = ctx.service(uniwow_api::viewport::SERVICE) {
            view.add_tool(ctx.module_id(), Box::new(ServersTool(self.shared.clone())));
        }
        let at_start = ctx.setting(AT_START).and_then(|value| value.as_bool()).unwrap_or(true);
        match start(at_start, self.vault.get(&secret::target(&settings_of(ctx)))) {
            Start::Connect(password) => self.connect_with(ctx, password, true, true),
            Start::Ask => self.asking = true,
        }
    }

    fn shutdown(&mut self) {
        *lock(&self.shared.asked) = None;
        self.password.clear();
    }

    fn windows_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        self.settle();
        let chose = lock(&self.shared.chose).take();
        if let Some(name) = chose {
            self.choose(&name, false, ctx);
        }
        self.servers_ui(egui, ctx);
        self.sending_ui(egui, ctx);
        self.importing_ui(egui, ctx);
        if self.confirming.is_some() && !self.asking {
            let pending = {
                use world_db::WorldDb as _;
                Service(self.shared.clone()).pending()
            };
            let settings = settings_of(ctx);
            self.confirm_ui(egui, ctx, &pending, &settings);
        }
        if !self.asking {
            return;
        }
        let mut at_start = ctx.setting(AT_START).and_then(|value| value.as_bool()).unwrap_or(true);
        let refused = match lock(&self.shared.state).clone() {
            Some(State::Refused(reason)) => Some(reason),
            _ => None,
        };
        let settings = settings_of(ctx);
        let mut connect = false;
        let mut later = false;
        egui::Window::new("World database")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(egui, |ui| {
                ui.label(format!(
                    "The password of {} for {} on {}:{}, read only.",
                    settings.user, settings.database, settings.host, settings.port
                ));
                if let Some(reason) = &refused {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("Refused: {reason}"));
                }
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .hint_text("Password"),
                );
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    connect = true;
                }
                ui.checkbox(&mut self.remember, "Remember password")
                    .on_hover_text("Kept in the Credential Manager of Windows once connected, for your account alone");
                ui.checkbox(&mut at_start, "Connect automatically at startup")
                    .on_hover_text("At each start of the editor, with the password kept, without this window");
                ui.horizontal(|ui| {
                    connect |= ui
                        .button("Connect")
                        .on_hover_text("Connects to the world database of the server chosen, read only; writes nothing")
                        .clicked();
                    later = ui
                        .button("Not now")
                        .on_hover_text("Leaves the editor without the database: no spawns, quests nor patrols")
                        .clicked();
                });
                ui.label(
                    egui::RichText::new(
                        "The address, the user and the database: in the servers of the panel World database.",
                    )
                    .weak()
                    .small(),
                );
            });
        if at_start != ctx.setting(AT_START).and_then(|value| value.as_bool()).unwrap_or(true) {
            ctx.set_setting(AT_START, json!(at_start));
        }
        if connect {
            self.connect(ctx);
        } else if later {
            self.password.clear();
            self.asking = false;
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        use world_db::WorldDb as _;
        let state = Service(self.shared.clone()).state();
        let (colour, text) = match &state {
            State::Waiting => (ui.visuals().weak_text_color(), "Not connected".to_owned()),
            State::Connecting => (theme::Theme::DARK.palette.warning, "Connecting…".to_owned()),
            State::Connected(version) => (
                theme::Theme::DARK.palette.success,
                format!("Connected, MySQL {version}"),
            ),
            State::Refused(reason) => (theme::Theme::DARK.palette.error, format!("Refused: {reason}")),
        };
        ui.horizontal_wrapped(|ui| {
            theme::status_dot(ui, colour, matches!(state, State::Connecting));
            ui.label(text);
        });
        ui.separator();
        let current = settings_of(ctx);
        let (servers, server) = (servers_of(ctx), server_of(ctx));
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.label("Server");
            egui::ComboBox::from_id_salt("world-db panel server")
                .selected_text(&server.name)
                .show_ui(ui, |ui| {
                    for other in &servers {
                        if ui
                            .selectable_label(other.name == server.name, &other.name)
                            .on_hover_text(format!(
                                "Read and write {}: its world database, its observer, its bridge. Writes nothing.",
                                other.name
                            ))
                            .clicked()
                            && other.name != server.name
                        {
                            chosen = Some(other.name.clone());
                        }
                    }
                });
            if ui
                .button("Servers…")
                .on_hover_text("Declare the servers, their databases, observers and bridges")
                .clicked()
            {
                self.shared.manage.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        if let Some(name) = chosen {
            self.choose(&name, false, ctx);
        }
        ui.label(
            egui::RichText::new(format!(
                "{} on {}:{} as {}; observer {}, bridge {}",
                server.database, server.host, server.port, server.user, server.observer_port, server.bridge_port
            ))
            .weak()
            .small(),
        );
        ui.horizontal(|ui| {
            if ui
                .button("Connect…")
                .on_hover_text("Asks the password, then connects")
                .clicked()
            {
                self.asking = true;
            }
            let kept = self.is_kept(ctx);
            if ui
                .add_enabled(kept, egui::Button::new("Forget password"))
                .on_hover_text("Takes the password kept out of the Credential Manager of Windows")
                .clicked()
            {
                if let Err(reason) = self.vault.forget(&secret::target(&settings_of(ctx))) {
                    log::warn!("{reason}");
                }
                self.kept = None;
            }
        });
        let mut at_start = ctx.setting(AT_START).and_then(|value| value.as_bool()).unwrap_or(true);
        if ui
            .checkbox(&mut at_start, "Connect automatically at startup")
            .on_hover_text("With the password kept; the window asks it otherwise")
            .changed()
        {
            ctx.set_setting(AT_START, json!(at_start));
        }
        ui.add_space(8.0);
        self.changes_ui(ui, ctx, &state, &current);
    }

    fn project_section(&self) -> Option<Value> {
        Some(json!({ "applications": self.applications.iter().map(Application::to_json).collect::<Vec<_>>() }))
    }

    fn open_project(&mut self, section: Option<Value>, _ctx: &mut Context) {
        self.applications = section
            .as_ref()
            .and_then(|section| section["applications"].as_array())
            .map(|applications| applications.iter().filter_map(Application::of_json).collect())
            .unwrap_or_default();
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        let ours = match &self.applying {
            Some(Applying::Apply { job: made, .. } | Applying::Undo { job: made }) => *made == job,
            None => false,
        };
        if self.exporting == Some(job) {
            self.exporting = None;
            self.exported(outcome.take::<Result<Option<std::path::PathBuf>, String>>());
            return;
        }
        match &self.importing {
            Some(exchange::Importing::Reading(reading)) if *reading == job => {
                self.import_read(outcome.take::<exchange::Read>());
                return;
            }
            Some(exchange::Importing::Writing { job: writing, .. }) if *writing == job => {
                self.imported(outcome.take::<Result<Vec<Entry>, String>>(), ctx);
                return;
            }
            _ => {}
        }
        match &self.sending {
            Some(sending::Sending::Reading { job: reading, .. }) if *reading == job => {
                self.target_read(
                    outcome.take::<Result<Vec<Vec<uniwow_api::db::write::Row>>, String>>(),
                    ctx,
                );
                return;
            }
            Some(sending::Sending::Writing { job: writing, .. }) if *writing == job => {
                self.target_written(outcome.take::<Result<Vec<Entry>, String>>(), ctx);
                return;
            }
            _ => {}
        }
        if self.bridging == Some(job) {
            self.bridging = None;
            self.bridged(outcome.take::<Result<Vec<(bool, String)>, String>>());
            return;
        }
        if let Some((_, live, database)) = self.writing_live.take_if(|(writing, _, _)| *writing == job) {
            self.written_live(live, database, outcome.take::<Result<Vec<Entry>, String>>(), ctx);
            self.write_live(ctx);
            return;
        }
        if ours && let Some(applying) = self.applying.take() {
            self.applied(applying, outcome.take::<Result<Vec<Entry>, String>>(), ctx);
            return;
        }
        if self.thread == Some(job) {
            self.thread = None;
            *lock(&self.shared.asked) = None;
            if let JobOutcome::Panicked(message) = outcome {
                log::error!("the connection to the world database stopped: {message}");
            }
        }
    }
}

impl WorldDbModule {
    /// The outcome of an application: kept, or taken off once undone; its script written; the
    /// owners told.
    fn applied(&mut self, applying: Applying, outcome: Option<Result<Vec<Entry>, String>>, ctx: &mut Context) {
        let entries = match outcome {
            Some(Ok(entries)) => entries,
            Some(Err(reason)) => {
                log::warn!("the changes are not applied: {reason}");
                self.said = Some((false, format!("Nothing written: {reason}")));
                return;
            }
            None => {
                self.said = Some((false, "Nothing written: the application stopped".to_owned()));
                return;
            }
        };
        let when = now();
        let (owners, message, failed, reloads) = match applying {
            Applying::Apply {
                owners,
                reloads,
                database,
                ..
            } => {
                let application = Application {
                    when: when.clone(),
                    server: server_of(ctx).name,
                    database: database.clone(),
                    owners: owners.clone(),
                    entries,
                    reloads,
                };
                let rows = application.rows();
                let title = format!("Applied by UniWoW on {when} to {database}: {rows} rows");
                let failed = self.write_script(ctx, &title, &when, false, &application.entries);
                let reloads = application.reloads.clone();
                self.applications.push(application);
                (owners, format!("{rows} rows written to {database}"), failed, reloads)
            }
            Applying::Undo { .. } => {
                let Some(undone) = self.last_here(ctx).map(|at| self.applications.remove(at)) else {
                    return;
                };
                self.picked.clear();
                let title = format!("Undone by UniWoW on {when}: the application of {}", undone.when);
                let failed = self.write_script(ctx, &title, &when, true, &entries);
                let message = format!(
                    "The application of {} undone: {} rows put back",
                    undone.when,
                    undone.rows()
                );
                (undone.owners, message, failed, undone.reloads)
            }
        };
        log::info!("{message}");
        if let Some(failed) = &failed {
            log::warn!("{failed}");
        }
        self.said = Some((
            failed.is_none(),
            failed.map_or(message.clone(), |failed| format!("{message}; {failed}")),
        ));
        ctx.publish(world_db::APPLIED_TOPIC, json!({ "owners": owners }));
        self.ask_bridge(&reloads, ctx);
    }

    /// Writes the next change applied at once, from a job, once the one before is written.
    fn write_live(&mut self, ctx: &mut Context) {
        use world_db::WorldDb as _;
        if self.writing_live.is_some() {
            return;
        }
        let Some(live) = self.live.pop_front() else {
            return;
        };
        let settings = settings_of(ctx);
        let (shared, changes) = (self.shared.clone(), live.changes.clone());
        let job = ctx.spawn("Apply a change to the world database at once", move |_| {
            Service(shared).apply(&changes)
        });
        let database = format!("{} on {}:{}", settings.database, settings.host, settings.port);
        self.writing_live = Some((job, live, database));
    }

    /// The outcome of a change applied at once: kept as an application, its script written, the
    /// server asked to load it again; its owner told, with the rows it replaced.
    fn written_live(
        &mut self,
        live: LiveApply,
        database: String,
        outcome: Option<Result<Vec<Entry>, String>>,
        ctx: &mut Context,
    ) {
        let entries = match outcome {
            Some(Ok(entries)) => entries,
            refused => {
                let reason = match refused {
                    Some(Err(reason)) => reason,
                    _ => "the application stopped".to_owned(),
                };
                log::warn!("{} is not applied: {reason}", live.label);
                self.said = Some((false, format!("{}: nothing written, {reason}", live.label)));
                ctx.publish(
                    world_db::APPLIED_LIVE_TOPIC,
                    json!({ "owner": live.owner, "ticket": live.ticket, "refused": reason }),
                );
                return;
            }
        };
        let when = now();
        let application = Application {
            when: when.clone(),
            server: server_of(ctx).name,
            database: database.clone(),
            owners: vec![live.owner.clone()],
            entries,
            reloads: live.reloads.clone(),
        };
        let rows = application.rows();
        let title = format!("Applied at once by UniWoW on {when} to {database}: {}", live.label);
        let failed = self.write_script(ctx, &title, &when, false, &application.entries);
        let message = format!("{}: {rows} rows written to {database} at once", live.label);
        log::info!("{message}");
        self.said = Some((
            failed.is_none(),
            failed.map_or(message.clone(), |failed| format!("{message}; {failed}")),
        ));
        ctx.publish(
            world_db::APPLIED_LIVE_TOPIC,
            json!({
                "owner": live.owner,
                "ticket": live.ticket,
                "entries": application.entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
            }),
        );
        self.applications.push(application);
        self.ask_bridge(&live.reloads, ctx);
    }

    /// Asks the bridge of the server to load again what `reloads` names, from a job.
    fn ask_bridge(&mut self, reloads: &[Reload], ctx: &mut Context) {
        let server = server_of(ctx);
        self.ask_bridge_at(server.bridge_port, server.bridge_token, reloads, ctx);
    }

    /// Asks the bridge on `port` with `token` to load again what `reloads` names, from a job; the
    /// quests to take from the characters kept, to say they are not if it does not answer.
    pub(crate) fn ask_bridge_at(&mut self, port: u16, token: String, reloads: &[Reload], ctx: &mut Context) {
        let demands = bridge::demands(reloads);
        if demands.is_empty() {
            return;
        }
        self.forgetting = reloads
            .iter()
            .filter_map(|reload| match reload {
                Reload::ForgetQuest(id) => Some(*id),
                _ => None,
            })
            .collect();
        let count = demands.len();
        self.bridging = Some(ctx.spawn("Ask the server to load the changes again", move |_| {
            bridge::reload(port, &token, &demands)
        }));
        log::info!("{count} loads asked of the server's bridge on 127.0.0.1:{port}");
    }

    /// What the bridge made, told after what was written.
    fn bridged(&mut self, outcome: Option<Result<Vec<(bool, String)>, String>>) {
        let told = match outcome {
            Some(Ok(made)) => {
                let failed: Vec<&String> = made.iter().filter(|(done, _)| !done).map(|(_, said)| said).collect();
                for (done, said) in &made {
                    if *done {
                        log::info!("the server: {said}");
                    } else {
                        log::warn!("the server: {said}");
                    }
                }
                if failed.is_empty() {
                    (
                        true,
                        format!("seen in the game: the server loaded the {} changes again", made.len()),
                    )
                } else {
                    let failed: Vec<&str> = failed.iter().map(|said| said.as_str()).collect();
                    (false, format!("in the game, not all: {}", failed.join("; ")))
                }
            }
            Some(Err(reason)) => {
                log::warn!("the server is not asked to load the changes again: {reason}");
                let mut said = format!("not seen in the game until the server starts again: {reason}");
                // A restart does not take a deleted quest from the characters: the bridge alone.
                if !self.forgetting.is_empty() {
                    let ids: Vec<String> = self.forgetting.iter().map(u32::to_string).collect();
                    said.push_str(&format!(
                        ". The characters keep the deleted quests {} until the bridge takes them: its id made again                          would not be offered to those who took them",
                        ids.join(", ")
                    ));
                }
                (false, said)
            }
            None => (false, "the server's bridge stopped".to_owned()),
        };
        let (fine, said) = self.said.take().unwrap_or((true, String::new()));
        self.said = Some((fine && told.0, format!("{said}; {}", told.1)));
    }

    /// The changes the modules hold for the server: what each holds, where they are written, the
    /// application and its undoing, each confirmed; the folder of SQL the scripts go to.
    fn changes_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context, state: &State, settings: &Settings) {
        use world_db::WorldDb as _;
        let pending = Service(self.shared.clone()).pending();
        let rows: usize = pending.iter().map(|held| held.changes.len()).sum();
        let note = (rows > 0).then(|| format!("{rows} rows"));
        theme::section(
            ui,
            egui::Id::new("world-db changes"),
            theme::icon::CONNECTED,
            "Changes for the server",
            note.as_deref(),
            true,
            |ui| {
                if pending.is_empty() {
                    ui.weak("No change waits for the server.");
                }
                for held in &pending {
                    ui.horizontal(|ui| {
                        ui.strong(&held.label);
                        ui.weak(format!("{} rows", held.changes.len()));
                    });
                    for note in &held.held {
                        ui.label(egui::RichText::new(format!("Not written yet: {note}")).weak().small());
                    }
                }
                let connected = matches!(state, State::Connected(_));
                let busy = self.applying.is_some();
                ui.label(
                    egui::RichText::new(format!(
                        "Written to {} on {}:{} by {}",
                        settings.database, settings.host, settings.port, settings.user
                    ))
                    .weak()
                    .small(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            connected && rows > 0 && !busy,
                            egui::Button::new("Apply to the server…"),
                        )
                        .on_hover_text(
                            "Writes every change above into the world database of the server chosen, one transaction: \
                             all or none; then its bridge loads them again in the game. Asks first.",
                        )
                        .clicked()
                    {
                        self.confirming = Some(Confirming::Apply);
                    }
                    let last = self.last_here(ctx).map(|at| self.applications[at].when.clone());
                    if ui
                        .add_enabled(
                            connected && last.is_some() && !busy,
                            egui::Button::new("Undo the last application…"),
                        )
                        .on_hover_text(last.map_or("Nothing applied".to_owned(), |when| {
                            format!("Puts back the rows the application of {when} replaced")
                        }))
                        .clicked()
                    {
                        self.confirming = Some(Confirming::Undo);
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                if let Some((fine, said)) = &self.said {
                    let colour = if *fine {
                        theme::Theme::DARK.palette.success
                    } else {
                        theme::Theme::DARK.palette.error
                    };
                    ui.colored_label(colour, said);
                }
                self.applications_ui(ui, ctx);
                ui.horizontal(|ui| {
                    let label = if self.picked.is_empty() {
                        "Export the changes waiting…"
                    } else {
                        "Export the applications picked…"
                    };
                    if ui
                        .add_enabled(connected && self.exporting.is_none(), egui::Button::new(label))
                        .on_hover_text("A .uniwow file: the rows, what they name, what belongs to what they make")
                        .clicked()
                    {
                        self.export(ctx);
                    }
                    if ui
                        .add_enabled(connected && self.importing.is_none(), egui::Button::new("Import…"))
                        .on_hover_text("A .uniwow file into this server: its problems shown first")
                        .clicked()
                    {
                        self.import(ctx);
                    }
                });
                let server = server_of(ctx);
                ui.label(
                    egui::RichText::new(if server.sql_folder.trim().is_empty() {
                        "No folder of scripts for this server (Servers…)".to_owned()
                    } else {
                        format!("Each application also written to {}", server.sql_folder)
                    })
                    .weak()
                    .small(),
                );
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Loaded again in the game by the bridge on 127.0.0.1:{}",
                            server.bridge_port
                        ))
                        .weak()
                        .small(),
                    );
                    if self.bridging.is_some() {
                        ui.spinner();
                    }
                });
            },
        );
    }

    /// The window confirming an application, its undoing, or another server chosen while changes
    /// wait.
    fn confirm_ui(&mut self, egui: &egui::Context, ctx: &mut Context, pending: &[Pending], settings: &Settings) {
        let Some(confirming) = self.confirming.clone() else {
            return;
        };
        let (title, text) = match &confirming {
            Confirming::Apply => {
                let rows: usize = pending.iter().map(|held| held.changes.len()).sum();
                let mut tables: Vec<&str> = pending
                    .iter()
                    .flat_map(|held| held.changes.iter().map(RowChange::table))
                    .collect();
                tables.sort_unstable();
                tables.dedup();
                (
                    "Apply to the server",
                    format!(
                        "Write {rows} changes of rows into {} on {}:{} ({})? One transaction: all or none. \
                         Undone from the panel with Undo the last application.",
                        settings.database,
                        settings.host,
                        settings.port,
                        tables.join(", ")
                    ),
                )
            }
            Confirming::Undo => (
                "Undo the last application",
                match self.last_here(ctx).map(|at| &self.applications[at]) {
                    Some(last) => format!(
                        "Put back the {} rows the application of {} replaced in {}? Changes made to them since, \
                         in the game or elsewhere, are lost.",
                        last.rows(),
                        last.when,
                        last.database
                    ),
                    None => "Nothing applied.".to_owned(),
                },
            ),
            Confirming::Switch(name) => {
                let rows: usize = pending.iter().map(|held| held.changes.len()).sum();
                (
                    "Choose another server",
                    format!(
                        "{rows} changes of rows wait for {}. Choose {name} all the same? They stay in the project and are written to the server chosen when applied: apply them first to write them here.",
                        settings.database
                    ),
                )
            }
        };
        let (mut yes, mut no) = (false, false);
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(egui, |ui| {
                ui.set_max_width(420.0);
                ui.label(text);
                ui.horizontal(|ui| {
                    yes = ui.button(title).on_hover_text("Goes on, as said above").clicked();
                    no = ui
                        .button("Cancel")
                        .on_hover_text("Nothing done, nothing written")
                        .clicked();
                });
            });
        if yes {
            self.confirming = None;
            self.said = None;
            match confirming {
                Confirming::Apply => self.apply(ctx),
                Confirming::Undo => self.undo(ctx),
                Confirming::Switch(name) => self.choose(&name, true, ctx),
            }
        } else if no {
            self.confirming = None;
        }
    }
}

uniwow_api::export_module!(WorldDbModule::default());
