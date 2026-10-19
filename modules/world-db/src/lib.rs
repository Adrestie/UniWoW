//! The world database of AzerothCore (step 15.11a), read only: its address, port, user and database
//! in the panel *World database*; its password asked at each start in a window of its own, masked,
//! kept in memory for the session only to connect again, never written (not in the settings, the
//! project, the log, the notices, nor a message of failure). The connection lives on a thread of its
//! own, its session read only; the service `world-db` and the command `world-db.query` run one
//! statement at a time beginning with `SELECT`, `SHOW`, `DESCRIBE` or `EXPLAIN`.

#[cfg(test)]
mod tests;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use uniwow_api::db::{Connection, Rows, Settings};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, State};
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
}

/// What the service shares with the thread of the connection.
#[derive(Default)]
struct Shared {
    state: Mutex<Option<State>>,
    asked: Mutex<Option<Sender<Asked>>>,
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
        }
    }
}

/// The keys of the settings, kept by the module: never the password.
const KEYS: [&str; 4] = ["host", "port", "user", "database"];

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

#[derive(Default)]
struct WorldDbModule {
    shared: Arc<Shared>,
    thread: Option<JobId>,
    /// The settings being edited in the panel; the password being typed, kept until *Connect*.
    editing: Option<Settings>,
    password: String,
    asking: bool,
}

impl WorldDbModule {
    fn connect(&mut self, ctx: &mut Context) {
        let settings = settings_of(ctx);
        let password = std::mem::take(&mut self.password);
        if let Some(asked) = lock(&self.shared.asked).as_ref() {
            let _ = asked.send(Asked::Connect(settings, password));
        }
        self.asking = false;
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
        // Asked at each start.
        self.asking = true;
    }

    fn shutdown(&mut self) {
        *lock(&self.shared.asked) = None;
        self.password.clear();
    }

    fn windows_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        if !self.asking {
            return;
        }
        let settings = settings_of(ctx);
        let mut connect = false;
        let mut later = false;
        egui::Window::new("World database")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(egui, |ui| {
                ui.label(format!(
                    "The password of {} for {} on {}:{}, read only; kept for this session alone, never written.",
                    settings.user, settings.database, settings.host, settings.port
                ));
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .hint_text("Password"),
                );
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    connect = true;
                }
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
        });
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if self.thread == Some(job) {
            self.thread = None;
            *lock(&self.shared.asked) = None;
            if let JobOutcome::Panicked(message) = outcome {
                log::error!("the connection to the world database stopped: {message}");
            }
        }
    }
}

uniwow_api::export_module!(WorldDbModule::default());
