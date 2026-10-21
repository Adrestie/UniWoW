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
#[cfg(test)]
mod tests;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use uniwow_api::db::secret::{self, Vault};
use uniwow_api::db::write::{self, Entry, RowChange};
use uniwow_api::db::{Connection, Rows, Settings, Writer};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, Pending, Reload, State};
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
                        let version = opened.version().unwrap_or_default();
                        shared.set(State::Connected(version));
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
    database: String,
    owners: Vec<String>,
    entries: Vec<Entry>,
    reloads: Vec<Reload>,
}

fn reload_json(reload: &Reload) -> Value {
    match reload {
        Reload::Quests => json!({ "quests": true }),
        Reload::Addons => json!({ "addons": true }),
        Reload::Path(id) => json!({ "path": id }),
        Reload::Formation(leader) => json!({ "formation": leader }),
        Reload::Creature(guid) => json!({ "creature": guid }),
        Reload::GameObject(guid) => json!({ "gameobject": guid }),
    }
}

fn reload_of(value: &Value) -> Option<Reload> {
    if value["quests"].as_bool() == Some(true) {
        return Some(Reload::Quests);
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
    if let Some(guid) = value["creature"].as_u64() {
        return Some(Reload::Creature(guid));
    }
    value["gameobject"].as_u64().map(Reload::GameObject)
}

impl Application {
    fn rows(&self) -> usize {
        self.entries.iter().filter(|entry| entry.before != entry.after).count()
    }

    fn to_json(&self) -> Value {
        json!({
            "when": self.when,
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

/// The setting of the folder of SQL of a module of the server, where each application is written.
const SQL_FOLDER: &str = "sql_folder";

/// The settings of the bridge of the server (`mod-uniwow-bridge`): its port and token.
const BRIDGE_PORT: &str = "bridge_port";
const BRIDGE_TOKEN: &str = "bridge_token";
const DEFAULT_BRIDGE_PORT: u16 = 8092;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirming {
    Apply,
    Undo,
}

/// The keys of the settings, kept by the module: never the password.
const KEYS: [&str; 4] = ["host", "port", "user", "database"];

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

fn settings_of(ctx: &Context) -> Settings {
    let defaults = Settings::default();
    let text = |key: &str, default: &str| {
        ctx.setting(key)
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| default.to_owned())
    };
    Settings {
        host: text("host", &defaults.host),
        port: ctx
            .setting("port")
            .and_then(|value| value.as_u64())
            .map_or(defaults.port, |port| port as u16),
        user: text("user", &defaults.user),
        database: text("database", &defaults.database),
    }
}

struct WorldDbModule {
    shared: Arc<Shared>,
    thread: Option<JobId>,
    /// The settings being edited in the panel; the password being typed, kept until *Connect*.
    editing: Option<Settings>,
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
    said: Option<(bool, String)>,
    /// The bridge of the server asked to load the changes again.
    bridging: Option<JobId>,
}

impl Default for WorldDbModule {
    fn default() -> Self {
        Self {
            shared: Arc::default(),
            thread: None,
            editing: None,
            password: String::new(),
            asking: false,
            remember: true,
            pending: None,
            vault: Arc::new(secret::System),
            kept: None,
            applications: Vec::new(),
            applying: None,
            confirming: None,
            said: None,
            bridging: None,
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
    fn undo(&mut self, ctx: &mut Context) {
        use world_db::WorldDb as _;
        let Some(last) = self.applications.last() else {
            return;
        };
        let changes = write::revert(&last.entries);
        let shared = self.shared.clone();
        let job = ctx.spawn("Undo the last application to the world database", move |_| {
            Service(shared).apply(&changes)
        });
        self.applying = Some(Applying::Undo { job });
    }

    /// Writes the script of `entries` into the folder of SQL set, if one is; what went wrong.
    fn write_script(&self, ctx: &Context, title: &str, when: &str, undone: bool, entries: &[Entry]) -> Option<String> {
        let folder = ctx
            .setting(SQL_FOLDER)
            .and_then(|value| value.as_str().map(str::to_owned))?;
        if folder.trim().is_empty() {
            return None;
        }
        let path = std::path::Path::new(folder.trim()).join(script_name(when, undone));
        let written =
            std::fs::create_dir_all(folder.trim()).and_then(|_| std::fs::write(&path, write::script(title, entries)));
        written
            .err()
            .map(|error| format!("the script {} is not written: {error}", path.display()))
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
        let (for_query, for_state) = (self.shared.clone(), self.shared.clone());
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
            );
    }

    fn init(&mut self, ctx: &mut Context) {
        let (asked, receiver) = mpsc::channel();
        *lock(&self.shared.asked) = Some(asked);
        let shared = self.shared.clone();
        self.thread = Some(ctx.spawn_thread("The connection to the world database", move |job| {
            serve(shared, receiver, || job.is_cancelled());
        }));
        self.shared.set(State::Waiting);
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
                ui.checkbox(&mut at_start, "Connect automatically at startup");
                ui.horizontal(|ui| {
                    connect |= ui.button("Connect").clicked();
                    later = ui.button("Not now").clicked();
                });
                ui.label(
                    egui::RichText::new("The address, the user and the database: in the panel World database.")
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
                format!("Connected, MySQL {version}, read only"),
            ),
            State::Refused(reason) => (theme::Theme::DARK.palette.error, format!("Refused: {reason}")),
        };
        ui.horizontal_wrapped(|ui| {
            theme::status_dot(ui, colour, matches!(state, State::Connecting));
            ui.label(text);
        });
        ui.separator();
        let current = settings_of(ctx);
        let editing = self.editing.get_or_insert_with(|| current.clone());
        egui::Grid::new("world-db settings").num_columns(2).show(ui, |ui| {
            ui.label("Address");
            ui.text_edit_singleline(&mut editing.host);
            ui.end_row();
            ui.label("Port");
            ui.add(egui::DragValue::new(&mut editing.port));
            ui.end_row();
            ui.label("User");
            ui.text_edit_singleline(&mut editing.user);
            ui.end_row();
            ui.label("Database");
            ui.text_edit_singleline(&mut editing.database);
            ui.end_row();
        });
        let changed = *editing != current;
        let edited = editing.clone();
        ui.horizontal(|ui| {
            if ui.add_enabled(changed, egui::Button::new("Apply")).clicked() {
                for (key, value) in KEYS.into_iter().zip([
                    json!(edited.host),
                    json!(edited.port),
                    json!(edited.user),
                    json!(edited.database),
                ]) {
                    ctx.set_setting(key, value);
                }
            }
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
        if self.bridging == Some(job) {
            self.bridging = None;
            self.bridged(outcome.take::<Result<Vec<(bool, String)>, String>>());
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
                let Some(undone) = self.applications.pop() else {
                    return;
                };
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

    /// Asks the bridge of the server to load again what `reloads` names, from a job.
    fn ask_bridge(&mut self, reloads: &[Reload], ctx: &mut Context) {
        let demands = bridge::demands(reloads);
        if demands.is_empty() {
            return;
        }
        let port = ctx
            .setting(BRIDGE_PORT)
            .and_then(|value| value.as_u64())
            .map_or(DEFAULT_BRIDGE_PORT, |port| port as u16);
        let token = ctx
            .setting(BRIDGE_TOKEN)
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
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
                (
                    false,
                    format!("not seen in the game until the server starts again: {reason}"),
                )
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
                        .on_hover_text("Writes every change above, one transaction: all or none")
                        .clicked()
                    {
                        self.confirming = Some(Confirming::Apply);
                    }
                    let last = self.applications.last().map(|last| last.when.clone());
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
                if let Some(last) = self.applications.last() {
                    ui.weak(format!(
                        "{} applications kept in the project; the last on {}, {} rows",
                        self.applications.len(),
                        last.when,
                        last.rows()
                    ));
                }
                let mut folder = ctx
                    .setting(SQL_FOLDER)
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                ui.label("Scripts written to");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut folder)
                            .hint_text(r"…\modules\mod-uniwow-content\data\sql\db-world")
                            .desired_width(f32::INFINITY),
                    )
                    .on_hover_text(
                        "The folder of SQL of a module of the server: each application written there too, \
                         for a server installed again to have it. None: not written.",
                    )
                    .changed()
                {
                    ctx.set_setting(SQL_FOLDER, json!(folder));
                }
                let mut port = ctx
                    .setting(BRIDGE_PORT)
                    .and_then(|value| value.as_u64())
                    .map_or(DEFAULT_BRIDGE_PORT, |port| port as u16);
                let mut token = ctx
                    .setting(BRIDGE_TOKEN)
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label("Bridge of the server").on_hover_text(
                        "mod-uniwow-bridge: the server loads again what was written, without starting again",
                    );
                    if ui.add(egui::DragValue::new(&mut port).prefix("port ")).changed() {
                        ctx.set_setting(BRIDGE_PORT, json!(port));
                    }
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut token)
                                .password(true)
                                .hint_text("token")
                                .desired_width(120.0),
                        )
                        .on_hover_text("UniwowBridge.Token of the server's configuration")
                        .changed()
                    {
                        ctx.set_setting(BRIDGE_TOKEN, json!(token));
                    }
                    if self.bridging.is_some() {
                        ui.spinner();
                    }
                });
            },
        );
        self.confirm_ui(ui.ctx(), ctx, &pending, settings);
    }

    /// The window confirming an application or its undoing.
    fn confirm_ui(&mut self, egui: &egui::Context, ctx: &mut Context, pending: &[Pending], settings: &Settings) {
        let Some(confirming) = self.confirming else {
            return;
        };
        let (title, text) = match confirming {
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
                match self.applications.last() {
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
                    yes = ui.button(title).clicked();
                    no = ui.button("Cancel").clicked();
                });
            });
        if yes {
            self.confirming = None;
            self.said = None;
            match confirming {
                Confirming::Apply => self.apply(ctx),
                Confirming::Undo => self.undo(ctx),
            }
        } else if no {
            self.confirming = None;
        }
    }
}

uniwow_api::export_module!(WorldDbModule::default());
