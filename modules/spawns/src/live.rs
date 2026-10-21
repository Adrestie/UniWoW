//! The changes applied to the server at once, as they are made: the spawn point of a creature
//! dragged, once let go; a patrol changed. Each is one undo entry, and undone on the server too: its
//! rows written by the module `world-db` (`world-db.apply_live`), one transaction, kept as an
//! application with its script, the server's bridge asked to load them again; the rows they replaced
//! kept to put them back.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use uniwow_api::db::write::{self, Entry, RowChange};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, Reload};
use uniwow_api::{Command, Context, log, theme::Level};

use crate::data::{self, Changes};
use crate::{SpawnsModule, by_tile, paths, patrol_ui, patrols};

/// The rows a change applied at once replaced, once written.
type Replaced = Arc<Mutex<Option<Vec<Entry>>>>;

/// The spawns and patrols as the database had them, put back once a change applied at once is
/// undone; the reading they came from.
#[derive(Clone)]
pub struct Base {
    spawns: Arc<Vec<data::Spawn>>,
    patrols: Arc<patrols::Patrols>,
    paths: Arc<paths::Paths>,
    reading: u64,
}

/// What goes to the server, in order, one at a time: a change applied, or one undone.
pub enum Out {
    Apply {
        label: String,
        changes: Vec<RowChange>,
        reloads: Vec<Reload>,
        replaced: Replaced,
    },
    Undo {
        label: String,
        reloads: Vec<Reload>,
        replaced: Replaced,
    },
}

/// The changes applied at once waiting to go, and the one gone: its ticket, the rows it replaces
/// once told; whether the spawns are read again once it is.
#[derive(Default)]
pub struct Live {
    pub waiting: VecDeque<Out>,
    pub gone: Option<(u64, Option<Replaced>, bool)>,
    pub tickets: u64,
    /// The call that sent it, refused when no module `world-db` takes it.
    pub call: Option<uniwow_api::CallId>,
    /// Whether the spawns were read again since the base of a change undone: read again once its
    /// undoing is written.
    pub stale: bool,
    /// Whether a gizmo is being dragged: its changes applied once let go.
    pub held: bool,
}

/// A change applied to the server at once: the changes of the module before and after, the part
/// applied, the base it was laid over and what the server loads again for it.
pub struct Commit {
    pub label: String,
    pub before: Changes,
    pub after: Changes,
    pub live: Changes,
    pub base: Option<Base>,
    pub reloads: Vec<Reload>,
    pub replaced: Replaced,
}

impl Commit {
    fn new(label: String, before: Changes, after: Changes, live: Changes) -> Self {
        Self {
            label,
            before,
            after,
            live,
            base: None,
            reloads: Vec::new(),
            replaced: Arc::default(),
        }
    }
}

impl Command for Commit {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut SpawnsModule = module.downcast_mut().expect("the spawns");
        self.base = Some(module.base());
        let Some(entry_column) = module.entry_column else {
            return;
        };
        let pending = self
            .live
            .pending(&module.spawns, &module.index, entry_column, &module.patrols);
        self.reloads = pending.reloads.clone();
        module.lay(&self.live);
        module.set_changes(self.after.clone());
        module.at_once.waiting.push_back(Out::Apply {
            label: self.label.clone(),
            changes: pending.changes,
            reloads: pending.reloads,
            replaced: self.replaced.clone(),
        });
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut SpawnsModule = module.downcast_mut().expect("the spawns");
        let reloads = self.reloads.clone();
        if let Some(base) = self.base.take() {
            module.restore(base);
        }
        module.set_changes(self.before.clone());
        module.at_once.waiting.push_back(Out::Undo {
            label: format!("Undo: {}", self.label),
            reloads,
            replaced: self.replaced.clone(),
        });
    }
}

impl SpawnsModule {
    fn base(&self) -> Base {
        Base {
            spawns: self.spawns.clone(),
            patrols: self.patrols.clone(),
            paths: self.paths.clone(),
            reading: self.readings,
        }
    }

    /// The spawns and patrols as the database has them once `live` is written: the creatures moved
    /// there, the patrols changed.
    fn lay(&mut self, live: &Changes) {
        let mut tiles = BTreeSet::new();
        if !live.moved.is_empty() {
            let spawns = Arc::make_mut(&mut self.spawns);
            for (id, (position, orientation)) in &live.moved {
                if let Some(spawn) = self.index.get(id).and_then(|at| spawns.get_mut(*at)) {
                    tiles.extend(data::tile_of(spawn.position));
                    tiles.extend(data::tile_of(*position));
                    spawn.position = *position;
                    spawn.orientation = *orientation;
                }
            }
            self.by_tile = by_tile(&self.spawns);
        }
        if !live.patrols.is_empty() {
            let now = live.patrols.laid_over(&self.patrols);
            self.paths = Arc::new(patrol_ui::drawn_paths(&self.paths, &live.patrols, &now));
            self.patrols = Arc::new(now);
        }
        self.after_base(&tiles);
    }

    /// `base` put back, once a change applied at once is undone; the spawns read again when they
    /// were read since.
    fn restore(&mut self, base: Base) {
        if base.reading != self.readings {
            self.at_once.stale = true;
            self.read_for = None;
            return;
        }
        let mut tiles = BTreeSet::new();
        for (before, now) in base.spawns.iter().zip(self.spawns.iter()) {
            if before.position != now.position {
                tiles.extend(data::tile_of(before.position));
                tiles.extend(data::tile_of(now.position));
            }
        }
        self.spawns = base.spawns;
        self.patrols = base.patrols;
        self.paths = base.paths;
        self.by_tile = by_tile(&self.spawns);
        self.after_base(&tiles);
    }

    fn after_base(&mut self, tiles: &BTreeSet<uniwow_api::formats::TileId>) {
        self.generation += 1;
        self.looks_wanted = true;
        self.refresh_patrols();
        self.refresh_tiles(Some(tiles));
    }

    /// Makes `after` the changes, one undo entry: their part applied at once to the server (the
    /// creatures moved, the patrols) unless a gizmo is being dragged, which applies it once let go.
    pub(crate) fn change(&mut self, label: String, after: Changes, ctx: &mut Context) {
        let before = self.changes.clone();
        if before == after {
            return;
        }
        let (live, rest) = after.split_live(&self.index);
        if self.at_once.held || live.is_empty() || self.entry_column.is_none() {
            ctx.execute(crate::SetChanges { label, before, after });
            return;
        }
        ctx.execute(Commit::new(label, before, rest, live));
    }

    /// The changes made while a gizmo was dragged applied at once, once let go (`spawns.commit`):
    /// in the gesture of the drag, one undo entry with it.
    pub(crate) fn commit(&mut self, label: &str, ctx: &mut Context) -> Value {
        self.at_once.held = false;
        let (live, rest) = self.changes.split_live(&self.index);
        if live.is_empty() || self.entry_column.is_none() {
            return json!({ "applied": false });
        }
        ctx.execute(Commit::new(label.to_owned(), self.changes.clone(), rest, live));
        json!({ "applied": true })
    }

    /// Sends the next change applied at once to the module `world-db`, once the one before is told.
    pub(crate) fn send_live(&mut self, ctx: &mut Context) {
        while self.at_once.gone.is_none() {
            let Some(out) = self.at_once.waiting.pop_front() else {
                return;
            };
            let (label, changes, reloads, replaced, undo) = match out {
                Out::Apply {
                    label,
                    changes,
                    reloads,
                    replaced,
                } => (label, changes, reloads, Some(replaced), false),
                Out::Undo {
                    label,
                    reloads,
                    replaced,
                } => {
                    // Never written (refused, or not told yet): nothing to put back.
                    let Some(entries) = replaced.lock().unwrap_or_else(|e| e.into_inner()).take() else {
                        continue;
                    };
                    (label, write::revert(&entries), reloads, None, true)
                }
            };
            if changes.is_empty() {
                continue;
            }
            self.at_once.tickets += 1;
            let ticket = self.at_once.tickets;
            self.at_once.call = Some(ctx.call(
                "world-db.apply_live",
                json!({
                    "owner": crate::MODULE,
                    "label": label,
                    "ticket": ticket,
                    "changes": changes.iter().map(RowChange::to_json).collect::<Vec<_>>(),
                    "reloads": reloads.iter().map(Reload::to_json).collect::<Vec<_>>(),
                }),
            ));
            // An undo whose base was read since: read again once written.
            let reread = undo && std::mem::take(&mut self.at_once.stale);
            self.at_once.gone = Some((ticket, replaced, reread));
        }
    }

    /// What the module `world-db` tells of a change applied at once: the rows it replaced kept, to
    /// put them back once undone; refused, the spawns read again as the database has them.
    pub(crate) fn told_live(&mut self, payload: &Value, ctx: &mut Context) {
        if payload["owner"].as_str() != Some(crate::MODULE) {
            return;
        }
        let Some((ticket, replaced, reread)) = self.at_once.gone.take() else {
            return;
        };
        if payload["ticket"].as_u64() != Some(ticket) {
            self.at_once.gone = Some((ticket, replaced, reread));
            return;
        }
        if let Some(reason) = payload["refused"].as_str() {
            log::warn!("a change is not applied to the server: {reason}");
            ctx.notify(Level::Warning, &format!("Not applied to the server: {reason}"));
            self.read_for = None;
            return;
        }
        let entries: Option<Vec<Entry>> = payload["entries"]
            .as_array()
            .map(|entries| entries.iter().filter_map(Entry::of_json).collect());
        if let (Some(replaced), Some(entries)) = (replaced, entries) {
            *replaced.lock().unwrap_or_else(|e| e.into_inner()) = Some(entries);
        }
        if reread {
            self.read_for = None;
        }
    }

    /// A call to `world-db.apply_live` answered: refused (no such module), the change is not
    /// applied. Whether the call was this one's.
    pub(crate) fn live_answered(
        &mut self,
        call: uniwow_api::CallId,
        result: &Result<Value, String>,
        ctx: &mut Context,
    ) -> bool {
        if self.at_once.call != Some(call) {
            return false;
        }
        self.at_once.call = None;
        if let Err(reason) = result
            && self.at_once.gone.take().is_some()
        {
            ctx.notify(Level::Warning, &format!("Not applied to the server: {reason}"));
            self.read_for = None;
        }
        true
    }
}

/// The topic `world-db` tells the changes applied at once by.
pub const TOLD: &str = world_db::APPLIED_LIVE_TOPIC;
