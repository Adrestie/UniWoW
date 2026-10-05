//! The world as it runs on the server: the creatures, game objects and players around the camera,
//! on the map the terrain shows, streamed by the observer `mod-uniwow-observer`. Read only: nothing
//! is changed on the server, nothing goes to the history. A thread of its own holds the connection
//! (`link`); the commands read the snapshot it leaves, from any thread; an event, batched, tells
//! what each message of the observer changed.

mod link;
mod looks;
mod markers;
mod world;

#[cfg(test)]
mod markers_tests;
#[cfg(test)]
mod models_tests;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use uniwow_api::serde_json::{Value, json};
use uniwow_api::server_link::protocol::{DEAD, Kind, MOVING, State, TEMPORARY};
use uniwow_api::{
    Context, DockArea, Editor, JobId, JobOutcome, Module, PropertyValue, Registrar, egui, formats, log, models,
    viewport,
};

use link::{Connection, Settings, Shared, Surroundings};
use looks::Looks;
use world::Change;

/// The topic of the event published for each message of the observer that changed something.
pub const CHANGED: &str = "live-world.changed";
const PORT: &str = "port";
const WORLD_PORT: &str = "world_port";
const TOKEN: &str = "token";
const RADIUS: &str = "radius";
/// The radius of a zone, in yards: the observer reads one grid at most.
const RADII: [f32; 2] = [10.0, 533.0];

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A GUID as text: JSON numbers do not hold 64 bits.
fn guid_text(guid: u64) -> String {
    format!("0x{guid:016X}")
}

fn kind_text(kind: Kind) -> &'static str {
    match kind {
        Kind::Creature => "creature",
        Kind::GameObject => "game object",
        Kind::Player => "player",
    }
}

fn change_json(change: &Change) -> Value {
    let guids = |list: &[u64]| list.iter().map(|guid| guid_text(*guid)).collect::<Vec<_>>();
    json!({
        "map": change.map,
        "instance": change.instance,
        "sequence": change.sequence,
        "appeared": guids(&change.appeared),
        "changed": guids(&change.changed),
        "left": guids(&change.left),
    })
}

/// What the connection thread reads of the editor: the camera, and the map of the terrain.
struct EditorSurroundings(Editor);

impl Surroundings for EditorSurroundings {
    fn camera(&self) -> Option<[f32; 3]> {
        match self.0.read_property("viewport/camera_position") {
            Ok(PropertyValue::Vector(position)) => Some(position.map(|value| value as f32)),
            _ => None,
        }
    }

    fn map(&self) -> Option<u32> {
        let map = self.0.call("terrain.map", json!({})).ok()?;
        map.get("id")?.as_u64().map(|id| id as u32)
    }

    fn changed(&self, change: &Change) {
        let _ = self.0.publish(CHANGED, change_json(change));
    }
}

/// What the connection stands at, in words, and whether it is a fault.
fn connection_text(status: &link::Status, settings: &Settings) -> (String, bool) {
    match &status.connection {
        Connection::NoToken => (
            "No token: set the token of UniwowObserver.Token (configs/modules/mod_uniwow_observer.conf of the worldserver)."
                .to_owned(),
            true,
        ),
        Connection::Connecting => ("Connecting to the observer…".to_owned(), false),
        Connection::ServerStopped => (
            format!(
                "Server stopped: nothing listens on port {} (the worldserver) nor on port {} (its observer).",
                settings.world_port, settings.port
            ),
            true,
        ),
        Connection::ObserverMissing => (
            format!(
                "Observer missing: the worldserver listens on port {}, but nothing on port {}. Is mod-uniwow-observer built and enabled?",
                settings.world_port, settings.port
            ),
            true,
        ),
        Connection::Refused(reason) => (format!("Refused by the observer: {reason}."), true),
        Connection::Broken(problem) => (format!("Connection broken: {problem}."), true),
        Connection::Connected => (format!("Connected to {}.", status.server), false),
    }
}

fn state_text(state: Option<State>) -> Option<&'static str> {
    state.map(|state| match state {
        State::Waiting => "waiting",
        State::Active => "active",
        State::NotFound => "not found",
    })
}

/// The command `live-world.state`.
fn state(shared: &Shared, settings: &Settings) -> Value {
    let status = shared.status();
    let world = shared.world();
    let (message, fault) = connection_text(&status, settings);
    let connection = match status.connection {
        Connection::NoToken => "no token",
        Connection::Connecting => "connecting",
        Connection::ServerStopped => "server stopped",
        Connection::ObserverMissing => "observer missing",
        Connection::Refused(_) => "refused",
        Connection::Broken(_) => "broken",
        Connection::Connected => "connected",
    };
    json!({
        "connection": connection,
        "message": message,
        "fault": fault,
        "server": status.server,
        "commit": status.commit,
        "zone": status.zone.map(|zone| json!({
            "map": zone.map, "instance": zone.instance, "centre": zone.centre, "radius": zone.radius,
        })),
        "subscription": state_text(status.state),
        "sequence": world.sequence,
        "entities": {
            "creatures": world.count(Kind::Creature),
            "game_objects": world.count(Kind::GameObject),
            "players": world.count(Kind::Player),
        },
    })
}

/// The command `live-world.entities`: the entities within `radius` of `x`, `y`, the nearest first,
/// at most `limit`, of the `kinds` asked for; around the zone subscribed to by default.
fn entities(shared: &Shared, arguments: &Value) -> Result<Value, String> {
    let world = shared.world();
    let zone = shared.status().zone;
    let number = |key: &str| arguments.get(key).and_then(Value::as_f64).map(|value| value as f32);
    let (Some(x), Some(y)) = (
        number("x").or(zone.map(|zone| zone.centre[0])),
        number("y").or(zone.map(|zone| zone.centre[1])),
    ) else {
        return Ok(json!({ "map": world.map, "instance": world.instance, "sequence": world.sequence, "entities": [] }));
    };
    let radius = number("radius")
        .or(zone.map(|zone| zone.radius))
        .unwrap_or(f32::INFINITY);
    let kinds: Option<Vec<String>> = match arguments.get("kinds") {
        None | Some(Value::Null) => None,
        Some(Value::Array(kinds)) => Some(
            kinds
                .iter()
                .map(|kind| kind.as_str().map(str::to_owned).ok_or("kinds: a list of names"))
                .collect::<Result<_, _>>()?,
        ),
        Some(_) => return Err("kinds: a list of names, among creature, game object and player".to_owned()),
    };
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(500)
        .min(10_000) as usize;
    let now = Instant::now();
    let mut found: Vec<(f32, Value)> = world
        .entities
        .values()
        .filter(|tracked| {
            kinds
                .as_ref()
                .is_none_or(|kinds| kinds.iter().any(|kind| kind == kind_text(tracked.entity.kind)))
        })
        .filter_map(|tracked| {
            let position = tracked.position_at(now);
            let distance = (position[0] - x).hypot(position[1] - y);
            let entity = &tracked.entity;
            (distance <= radius).then(|| {
                (
                    distance,
                    json!({
                        "guid": guid_text(entity.guid),
                        "kind": kind_text(entity.kind),
                        "entry": entity.entry,
                        "spawn": entity.spawn,
                        "name": entity.name,
                        "display": entity.display,
                        "position": position,
                        "orientation": entity.orientation,
                        "scale": entity.scale,
                        "phase": entity.phase,
                        "pool": entity.pool,
                        "event": entity.event,
                        "temporary": entity.flags & TEMPORARY != 0,
                        "dead": entity.flags & DEAD != 0,
                        "moving": entity.flags & MOVING != 0,
                    }),
                )
            })
        })
        .collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.truncate(limit);
    Ok(json!({
        "map": world.map,
        "instance": world.instance,
        "sequence": world.sequence,
        "entities": found.into_iter().map(|(_, entity)| entity).collect::<Vec<_>>(),
    }))
}

#[derive(Default)]
struct LiveWorld {
    /// What the connection thread running now shares; replaced whole when it starts again.
    current: Arc<Mutex<Arc<Shared>>>,
    /// The settings in use, shared with the commands, and those being edited in the panel.
    settings: Arc<Mutex<Settings>>,
    editing: Settings,
    thread: Option<JobId>,
    /// What the thread placing the markers shares with their layer, and that thread.
    drawing: Arc<markers::Drawing>,
    animating: Option<JobId>,
    /// The service drawing the models, the looks of the displays, and the job reading them.
    models: Option<models::Handle>,
    looks: Arc<Looks>,
    reading: Option<JobId>,
}

impl LiveWorld {
    fn shared(&self) -> Arc<Shared> {
        lock(&self.current).clone()
    }

    /// Starts the connection thread again with the settings in use. The entities of the one
    /// before are said to leave first, through the same queue of events its thread told them
    /// through, and it tells nothing more.
    fn start(&mut self, ctx: &mut Context) {
        let change = self.shared().retire();
        if !change.is_empty() {
            let _ = ctx.editor().publish(CHANGED, change_json(&change));
        }
        if let Some(job) = self.thread.take() {
            ctx.cancel(job);
        }
        let shared = Arc::new(Shared::default());
        *lock(&self.current) = shared.clone();
        let settings = lock(&self.settings).clone();
        let surroundings = EditorSurroundings(ctx.editor());
        self.thread = Some(
            ctx.spawn_thread("Follow the world of the server through its observer", move |job| {
                link::run(&settings, &shared, &surroundings, &|| job.is_cancelled());
            }),
        );
    }
}

impl Module for LiveWorld {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("live-world", "Live world", DockArea::Right);
        let (current, settings) = (self.current.clone(), self.settings.clone());
        reg.command_on_caller(
            "live-world.state",
            "Where the connection to the observer of the server stands, the zone subscribed to, and how many entities it holds.",
            json!({ "type": "object" }),
            json!({ "type": "object" }),
            Arc::new(move |_| Ok(state(&lock(&current).clone(), &lock(&settings).clone()))),
        );
        let current = self.current.clone();
        reg.command_on_caller(
            "live-world.entities",
            "The creatures, game objects and players of the server within radius yards of x, y, the nearest first: at most limit (500), of the kinds given (creature, game object, player); around the zone subscribed to when x and y are not given.",
            json!({
                "type": "object",
                "properties": {
                    "x": { "type": "number" }, "y": { "type": "number" }, "radius": { "type": "number" },
                    "kinds": { "type": "array", "items": { "type": "string", "enum": ["creature", "game object", "player"] } },
                    "limit": { "type": "integer", "minimum": 0 },
                },
            }),
            json!({ "type": "object" }),
            Arc::new(move |arguments| entities(&lock(&current).clone(), &arguments)),
        );
    }

    fn init(&mut self, ctx: &mut Context) {
        let defaults = Settings::default();
        let port = |key: &str, default: u16| {
            ctx.setting(key)
                .and_then(|value| value.as_u64())
                .and_then(|port| u16::try_from(port).ok())
                .unwrap_or(default)
        };
        let settings = Settings {
            port: port(PORT, defaults.port),
            world_port: port(WORLD_PORT, defaults.world_port),
            token: ctx
                .setting(TOKEN)
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            radius: ctx
                .setting(RADIUS)
                .and_then(|value| value.as_f64())
                .map_or(defaults.radius, |radius| (radius as f32).clamp(RADII[0], RADII[1])),
        };
        self.editing = settings.clone();
        *lock(&self.settings) = settings;
        self.start(ctx);

        // The markers, where there is a 3D view.
        let (Some(view), Some(gpu)) = (ctx.service(viewport::SERVICE), ctx.gpu().cloned()) else {
            return;
        };
        view.add_layer(ctx.module_id(), Box::new(markers::Markers::new(self.drawing.clone())));
        self.models = ctx.service(models::SERVICE);
        let (drawing, current) = (self.drawing.clone(), self.current.clone());
        let (service, looks) = (self.models.clone(), self.looks.clone());
        self.animating = Some(
            ctx.spawn_thread("Place the entities of the live world at each frame", move |job| {
                let models = service.as_deref().map(|service| (&*looks, service));
                markers::animate(&view, &gpu, &drawing, &|| lock(&current).world(), models, &|| {
                    job.is_cancelled()
                });
            }),
        );
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        let shared = self.shared();
        let status = shared.status();
        let settings = lock(&self.settings).clone();
        let (message, fault) = connection_text(&status, &settings);
        if fault {
            ui.colored_label(ui.visuals().warn_fg_color, message);
        } else {
            ui.label(message);
        }
        if let Some(zone) = status.zone {
            let world = shared.world();
            ui.label(format!(
                "Map {}, instance {}, {:.0} yards around {:.0}, {:.0}: {}.",
                zone.map,
                zone.instance,
                zone.radius,
                zone.centre[0],
                zone.centre[1],
                state_text(status.state).unwrap_or("waiting"),
            ));
            let counted = |kind: Kind, one: &str, many: &str| {
                let count = world.count(kind);
                format!("{count} {}", if count == 1 { one } else { many })
            };
            ui.label(format!(
                "{}, {}, {}.",
                counted(Kind::Creature, "creature", "creatures"),
                counted(Kind::GameObject, "game object", "game objects"),
                counted(Kind::Player, "player", "players")
            ));
        } else if status.connection == Connection::Connected {
            ui.label("No zone: the terrain shows no map.");
        }
        ui.separator();
        egui::Grid::new("live-world settings").num_columns(2).show(ui, |ui| {
            ui.label("Observer port");
            ui.add(egui::DragValue::new(&mut self.editing.port));
            ui.end_row();
            ui.label("Token");
            ui.add(egui::TextEdit::singleline(&mut self.editing.token).password(true));
            ui.end_row();
            ui.label("Worldserver port");
            ui.add(egui::DragValue::new(&mut self.editing.world_port));
            ui.end_row();
            ui.label("Radius (yards)");
            ui.add(egui::DragValue::new(&mut self.editing.radius).range(RADII[0]..=RADII[1]));
            ui.end_row();
        });
        // Also when the thread stopped, to start it again.
        if ui
            .add_enabled(
                self.editing != settings || self.thread.is_none(),
                egui::Button::new("Apply"),
            )
            .clicked()
        {
            ctx.set_setting(PORT, json!(self.editing.port));
            ctx.set_setting(WORLD_PORT, json!(self.editing.world_port));
            ctx.set_setting(TOKEN, json!(self.editing.token));
            ctx.set_setting(RADIUS, json!(self.editing.radius));
            *lock(&self.settings) = self.editing.clone();
            self.start(ctx);
        }
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // Called at every frame: the displays the entities wanted are read here, one job at a time.
        if self.reading.is_some() {
            return;
        }
        let (Some(service), Some(tables)) = (self.models.clone(), ctx.service(formats::SERVICE)) else {
            return;
        };
        let displays = self.looks.take_wanted();
        if displays.is_empty() {
            return;
        }
        let looks = self.looks.clone();
        self.reading = Some(ctx.spawn("Read the looks of the live world", move |_| {
            looks.insert(looks::read(&*service, &*tables, &displays));
        }));
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if self.reading == Some(job) {
            self.reading = None;
            if let JobOutcome::Panicked(message) = outcome {
                log::error!("the looks of the live world could not be read: {message}");
            }
            return;
        }
        if self.animating == Some(job) {
            self.animating = None;
            if let JobOutcome::Panicked(message) = outcome {
                log::error!("the markers of the live world stopped: {message}");
            }
            return;
        }
        if self.thread != Some(job) {
            return;
        }
        self.thread = None;
        if let JobOutcome::Panicked(message) = outcome {
            log::error!("the connection to the observer stopped: {message}");
            self.shared()
                .stopped(format!("its thread stopped, {message}; Apply starts it again"));
        }
    }
}

uniwow_api::export_module!(LiveWorld::default());
