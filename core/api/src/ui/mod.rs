//! Interface objects modelled on Qt (section 3): widgets, layouts, a graphics scene, painting
//! areas, trees, tables, sequences and their players. A module that is not written in Rust creates them through
//! handles and changes them from any thread; the core keeps them, draws them on the interface
//! thread, and sends their signals to the module's own thread. The object model is defined once
//! here, for every language.

pub mod data;
mod painter;

use std::collections::{BTreeSet, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

pub use painter::{MAX_TEXT, PaintCommand};

use crate::AppliedChange;
use crate::curve::ShownCurve;
use crate::egui;
use crate::sequence::{self, MAX_FRAME_RATE, MAX_LENGTH, Sequence, Track};
use data::{Row, Rows, SortJob, Table, TreeItem};

/// Identifies an object of one module.
pub type Handle = u64;

/// Reads the curves of a curve view from their JSON text (`ShownCurve::list_from_json`).
pub fn read_curves(text: &str) -> Result<Vec<ShownCurve>, String> {
    let value = serde_json::from_str(text).map_err(|error| format!("the curves are not JSON: {error}"))?;
    ShownCurve::list_from_json(&value)
}

/// Reads a JSON text.
fn json(text: &str, what: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(text).map_err(|error| format!("the {what} are not JSON: {error}"))
}

/// Reads the items of a tree view from their JSON text (`data::items_from_json`).
pub fn read_items(text: &str) -> Result<Vec<TreeItem>, String> {
    data::items_from_json(&json(text, "items")?)
}

/// Reads the rows of a table view from their JSON text (`data::rows_from_json`).
pub fn read_rows(text: &str) -> Result<Vec<Row>, String> {
    data::rows_from_json(&json(text, "rows")?)
}

/// Reads the headers of a table view's columns from their JSON text.
pub fn read_columns(text: &str) -> Result<Vec<String>, String> {
    data::columns_from_json(&json(text, "columns")?)
}

/// Reads the tracks of a sequence from their JSON text (`sequence::tracks_from_json`).
pub fn read_tracks(text: &str) -> Result<Vec<Track>, String> {
    let value = serde_json::from_str(text).map_err(|error| format!("the tracks are not JSON: {error}"))?;
    sequence::tracks_from_json(&value)
}

/// The highest row, column or span of a grid layout: a wrapped negative number would otherwise
/// ask for billions of cells.
pub const MAX_CELL: u32 = 10_000;

/// The fastest a player plays, in times the frame rate of its sequence.
pub const MAX_SPEED: f64 = 100.0;

/// Records, as an undo entry of the module owning the objects, a change already made to them: its
/// label, and how to undo and redo it. Set by the kernel, which refuses it when the module may not
/// record now.
pub type Recorder = Arc<dyn Fn(&str, Box<dyn AppliedChange>) -> Result<(), String> + Send + Sync>;

/// Runs a job on the module's own thread.
pub type Post = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

/// Runs long work off the interface's thread and the modules' threads.
pub type Background = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

static BACKGROUND: OnceLock<Background> = OnceLock::new();

/// Where the objects make their long work, such as the sort of a large table: the kernel's pool,
/// which the kernel sets when it starts. Before, each runs on a thread of its own.
pub fn set_background(run: Background) {
    let _ = BACKGROUND.set(run);
}

fn in_background(work: Box<dyn FnOnce() + Send>) {
    match BACKGROUND.get() {
        Some(run) => run(work),
        None => {
            if let Err(error) = std::thread::Builder::new()
                .name("uniwow-objects".to_owned())
                .spawn(work)
            {
                log::error!("no thread for the work of the objects: {error}");
            }
        }
    }
}

/// A function connected to a signal; it runs on the module's own thread.
pub type Slot = Arc<dyn Fn(&SignalData) + Send + Sync>;

/// The objects of one module, shared between its threads and the interface thread.
pub type SharedUi = Arc<Mutex<Ui>>;

pub use crate::numbers::{Kind, Property, Signal};

impl Kind {
    pub fn is_layout(self) -> bool {
        matches!(self, Kind::VBoxLayout | Kind::HBoxLayout | Kind::GridLayout)
    }

    pub fn is_item(self) -> bool {
        matches!(
            self,
            Kind::RectItem | Kind::LineItem | Kind::EllipseItem | Kind::TextItem | Kind::ItemGroup
        )
    }

    /// A widget that a layout can hold.
    pub fn is_widget(self) -> bool {
        !self.is_item()
            && !matches!(
                self,
                Kind::Panel | Kind::GraphicsScene | Kind::Dialog | Kind::Sequence | Kind::Player
            )
    }
}

/// What a signal carries; only the fields its signal uses are set.
#[derive(Clone, Debug, Default)]
pub struct SignalData {
    pub sender: Handle,
    pub signal: u32,
    /// The scene item concerned, for the signals of a scene.
    pub item: Handle,
    pub boolean: bool,
    pub integer: i64,
    pub number: f64,
    pub text: String,
    /// A position: in the area for a painting area, in the scene for a scene.
    pub x: f64,
    pub y: f64,
    /// A movement: the wheel, or how far an item moved.
    pub dx: f64,
    pub dy: f64,
    /// The size to paint, for `Paint`.
    pub width: f64,
    pub height: f64,
    /// 1 left, 2 right, 3 middle.
    pub button: u32,
    /// 1 Ctrl, 2 Shift, 4 Alt.
    pub modifiers: u32,
    /// The painter to paint with, for `Paint`.
    pub painter: Handle,
}

/// One interface object. Fields a kind does not use stay at their defaults.
#[derive(Clone, Debug)]
pub struct Object {
    pub kind: Kind,
    pub parent: Option<Handle>,
    /// A layout's widgets, a scene's or group's items, the layout of a panel or group box.
    pub children: Vec<Handle>,
    /// Row, column, row span and column span in a grid layout.
    pub cell: [u32; 4],
    pub text: String,
    pub tooltip: String,
    pub placeholder: String,
    pub title: String,
    pub enabled: bool,
    pub visible: bool,
    pub checked: bool,
    pub value: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub step: f64,
    pub decimals: u32,
    pub items: Vec<String>,
    pub current_index: i64,
    /// Position in the parent, for scene items.
    pub pos: [f64; 2],
    /// The rectangle of a rectangle or ellipse item, in its own coordinates.
    pub rect: [f64; 4],
    /// The two ends of a line item, in its own coordinates.
    pub line: [f64; 4],
    /// Colours as 0xRRGGBBAA.
    pub pen_color: u32,
    pub pen_width: f64,
    pub brush_color: u32,
    pub radius: f64,
    pub z: f64,
    /// 1 along x, 2 along y.
    pub movable: u32,
    pub selectable: bool,
    pub selected: bool,
    /// The positions a movable item may take: x, y, width, height.
    pub bounds: Option<[f64; 4]>,
    pub font_size: f64,
    pub minimum_height: f64,
    /// The scene a view shows.
    pub scene: Option<Handle>,
    pub view_scale: f64,
    pub view_center: [f64; 2],
    /// What a painting area last painted.
    pub picture: Arc<Vec<PaintCommand>>,
    /// The painting area asked to be painted again.
    pub repaint: bool,
    /// Changed at each change of the object, so that the scene draws it again.
    pub generation: u64,
    /// The curves of a curve view.
    pub curves: Vec<ShownCurve>,
    /// A sequence's frame rate, length and tracks, shared with the kernel playing it.
    pub sequence: Option<Arc<Sequence>>,
    /// The sequence a player plays, or a view shows.
    pub plays: Option<Handle>,
    /// The player whose time a view shows as the playhead.
    pub player: Option<Handle>,
    /// A tree view's items, shared with the kernel drawing them.
    pub tree: Option<Arc<Vec<TreeItem>>>,
    /// Changed when a tree view's items, or which are unfolded, change.
    pub tree_version: u64,
    /// A table view's columns and rows, shared with the kernel drawing them.
    pub table: Option<Arc<Table>>,
    /// The id of a tree view's current item or a table view's current row, 0 for none.
    pub current_item: u64,
    /// The column of a table view's current cell.
    pub current_column: usize,
    /// A player's time, in frames.
    pub time: f64,
    pub playing: bool,
    pub looping: bool,
    pub speed: f64,
}

impl Object {
    fn new(kind: Kind, parent: Option<Handle>) -> Self {
        Self {
            kind,
            parent,
            children: Vec::new(),
            cell: [0, 0, 1, 1],
            text: String::new(),
            tooltip: String::new(),
            placeholder: String::new(),
            title: String::new(),
            enabled: true,
            visible: true,
            checked: false,
            value: 0.0,
            minimum: 0.0,
            maximum: if kind == Kind::Slider || kind == Kind::SpinBox {
                100.0
            } else {
                0.0
            },
            step: 1.0,
            decimals: 0,
            items: Vec::new(),
            current_index: -1,
            pos: [0.0; 2],
            rect: [0.0; 4],
            line: [0.0; 4],
            pen_color: if kind == Kind::TextItem { 0xE6E6E6FF } else { 0x000000FF },
            pen_width: if matches!(kind, Kind::LineItem) { 1.0 } else { 0.0 },
            brush_color: 0x00000000,
            radius: 0.0,
            z: 0.0,
            movable: 0,
            selectable: false,
            selected: false,
            bounds: None,
            font_size: 13.0,
            minimum_height: if matches!(
                kind,
                Kind::GraphicsView
                    | Kind::PaintArea
                    | Kind::CurveView
                    | Kind::DopesheetView
                    | Kind::TreeView
                    | Kind::TableView
            ) {
                200.0
            } else {
                0.0
            },
            scene: None,
            view_scale: 1.0,
            view_center: [0.0; 2],
            picture: Arc::default(),
            repaint: true,
            generation: 0,
            curves: Vec::new(),
            sequence: (kind == Kind::Sequence).then(|| Arc::new(Sequence::default())),
            plays: None,
            player: None,
            tree: (kind == Kind::TreeView).then(Arc::default),
            tree_version: 0,
            table: (kind == Kind::TableView).then(Arc::default),
            current_item: 0,
            current_column: 0,
            time: 0.0,
            playing: false,
            looping: false,
            speed: 1.0,
        }
    }
}

/// A view showing a sequence: the view, the sequence and what it holds, and the player whose time
/// the view shows.
#[derive(Clone, Debug)]
pub struct SequenceShown {
    pub view: Handle,
    pub sequence: Handle,
    pub data: Arc<Sequence>,
    pub player: Option<Handle>,
    pub time: Option<f64>,
}

/// What a player shows at the end of a frame: its time in the sequence it plays.
#[derive(Clone, Debug)]
pub struct PlayerFrame {
    pub player: Handle,
    pub time: f64,
    pub sequence: Handle,
    /// Changed at each change of the sequence.
    pub generation: u64,
    pub data: Arc<Sequence>,
}

/// A change of a sequence's tracks, recorded for the module owning it: undoing and redoing set the
/// tracks back, recording nothing. The title of the sequence names its document.
struct TracksChange {
    objects: Weak<Mutex<Ui>>,
    sequence: Handle,
    before: Vec<Track>,
    after: Vec<Track>,
    document: Option<String>,
}

impl TracksChange {
    fn set(&self, tracks: &[Track]) {
        // A sequence destroyed since has nothing to set back.
        if let Some(shared) = self.objects.upgrade() {
            let _ = lock(&shared).replace_tracks(self.sequence, tracks.to_vec());
        }
    }
}

impl AppliedChange for TracksChange {
    fn undo(&mut self) {
        self.set(&self.before);
    }

    fn redo(&mut self) {
        self.set(&self.after);
    }

    fn document(&self) -> Option<String> {
        self.document.clone()
    }
}

struct Connection {
    id: u64,
    sender: Handle,
    signal: Signal,
    slot: Slot,
}

/// The objects of one module, its panels, its connections and the pictures being painted.
pub struct Ui {
    objects: HashMap<Handle, Object>,
    next: Handle,
    /// Panel id and its object, in the order the module declared them.
    panels: Vec<(String, Handle)>,
    connections: Vec<Connection>,
    next_connection: u64,
    /// Painter, and the area and commands it records.
    painting: HashMap<Handle, (Handle, Vec<PaintCommand>)>,
    /// The painting areas being painted, with the size asked meanwhile, painted next.
    in_flight: HashMap<Handle, Option<[f64; 2]>>,
    /// The last job posted, when it is a mouse move or a curves change still waiting: the same
    /// signal of the same object sent next replaces its data instead of queuing behind it.
    waiting: Option<(Handle, u32, Waiting)>,
    /// The `timeChanged` of each player last posted: while it waits, the next one replaces its
    /// time, whatever was posted since.
    times: HashMap<Handle, Waiting>,
    /// The players, which the kernel moves on at each frame.
    players: BTreeSet<Handle>,
    /// The dopesheet views and curve views, which may show a sequence.
    sequence_views: BTreeSet<Handle>,
    /// Changed at each change of a scene's set of items or their order, by scene.
    structure: HashMap<Handle, u64>,
    post: Post,
    /// Where jobs that record nothing go, if the module's thread tells them apart.
    uncounted: Option<Post>,
    /// Records the changes the kernel keeps track of, once the kernel has adopted the objects.
    recorder: Option<Recorder>,
    wake: Option<egui::Context>,
    /// The store itself, for its jobs on the module's thread to look at it.
    this: Weak<Mutex<Ui>>,
}

/// A signal waiting for the module's thread; taken when delivered.
type Waiting = Arc<Mutex<Option<SignalData>>>;

/// Ends a painting, even when a slot panicked: the picture replaces the area's, and a size asked
/// meanwhile is painted next.
struct PaintingDone {
    shared: SharedUi,
    area: Handle,
    painter: Handle,
}

impl Drop for PaintingDone {
    fn drop(&mut self) {
        let mut ui = lock(&self.shared);
        if let Some((area, commands)) = ui.painting.remove(&self.painter)
            && !std::thread::panicking()
            && let Some(object) = ui.objects.get_mut(&area)
        {
            object.picture = Arc::new(commands);
        }
        if let Some(Some(size)) = ui.in_flight.remove(&self.area) {
            ui.request_paint(&self.shared, self.area, size);
        }
        ui.wake();
    }
}

/// Calls, on the module's thread, the slots still connected when their turn comes: one
/// disconnected or destroyed before, from that thread or by an earlier slot, is no longer called.
fn deliver(ui: &Weak<Mutex<Ui>>, slots: Vec<(u64, Slot)>, data: &SignalData) {
    for (connection, slot) in slots {
        let Some(shared) = ui.upgrade() else { return };
        let connected = lock(&shared).connections.iter().any(|c| c.id == connection);
        if connected {
            slot(data);
        }
    }
}

/// Locks the objects of a module, even after a panic in another thread.
pub fn lock(ui: &SharedUi) -> MutexGuard<'_, Ui> {
    ui.lock().unwrap_or_else(|e| e.into_inner())
}

impl Ui {
    /// The objects of a module, whose signals run through `post`.
    pub fn new(post: Post) -> SharedUi {
        Arc::new_cyclic(|this| {
            Mutex::new(Ui {
                objects: HashMap::new(),
                next: 1,
                panels: Vec::new(),
                connections: Vec::new(),
                next_connection: 1,
                painting: HashMap::new(),
                in_flight: HashMap::new(),
                waiting: None,
                times: HashMap::new(),
                players: BTreeSet::new(),
                sequence_views: BTreeSet::new(),
                structure: HashMap::new(),
                post,
                uncounted: None,
                recorder: None,
                wake: None,
                this: this.clone(),
            })
        })
    }

    fn insert(&mut self, object: Object) -> Handle {
        let handle = self.next;
        self.next += 1;
        self.objects.insert(handle, object);
        handle
    }

    pub fn object(&self, handle: Handle) -> Option<&Object> {
        self.objects.get(&handle)
    }

    pub fn object_mut(&mut self, handle: Handle) -> Option<&mut Object> {
        self.objects.get_mut(&handle)
    }

    fn get(&self, handle: Handle) -> Result<&Object, String> {
        self.objects.get(&handle).ok_or_else(|| format!("no object {handle}"))
    }

    fn get_mut(&mut self, handle: Handle) -> Result<&mut Object, String> {
        self.objects
            .get_mut(&handle)
            .ok_or_else(|| format!("no object {handle}"))
    }

    /// The object of a panel, created the first time it is asked for, so that a module can fill
    /// its panels while it starts, before it has declared them.
    pub fn panel(&mut self, id: &str) -> Handle {
        if let Some(handle) = self.find_panel(id) {
            return handle;
        }
        let handle = self.insert(Object::new(Kind::Panel, None));
        self.panels.push((id.to_owned(), handle));
        handle
    }

    pub fn find_panel(&self, id: &str) -> Option<Handle> {
        self.panels
            .iter()
            .find(|(panel, _)| panel == id)
            .map(|(_, handle)| *handle)
    }

    /// The editor's window, repainted when the objects change from another thread.
    pub fn set_wake(&mut self, wake: &egui::Context) {
        if self.wake.is_none() {
            self.wake = Some(wake.clone());
        }
    }

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake.request_repaint();
        }
    }

    /// Creates an object. An item's parent is a scene or a group; a widget or layout has no parent
    /// until it is placed.
    pub fn create(&mut self, kind: Kind, parent: Option<Handle>) -> Result<Handle, String> {
        match (kind.is_item(), parent) {
            (true, Some(parent)) => {
                let parent_kind = self.get(parent)?.kind;
                if !matches!(parent_kind, Kind::GraphicsScene | Kind::ItemGroup) {
                    return Err(format!("an item's parent is a scene or a group, not a {parent_kind:?}"));
                }
            }
            (true, None) => return Err("an item needs a scene or a group as parent".to_owned()),
            (false, Some(_)) => return Err(format!("a {kind:?} gets no parent when created: place it")),
            (false, None) if kind == Kind::Panel => return Err("panels are declared by the module".to_owned()),
            (false, None) => {}
        }
        let mut object = Object::new(kind, parent);
        // A dialog shows when its module asks for it.
        object.visible = kind != Kind::Dialog;
        let handle = self.insert(object);
        if kind == Kind::Player {
            self.players.insert(handle);
        }
        if matches!(kind, Kind::DopesheetView | Kind::CurveView) {
            self.sequence_views.insert(handle);
        }
        if let Some(parent) = parent {
            self.get_mut(parent)?.children.push(handle);
            self.changed_structure(parent);
        }
        Ok(handle)
    }

    /// Destroys an object and its children; it leaves its parent.
    pub fn destroy(&mut self, handle: Handle) -> Result<(), String> {
        let object = self.get(handle)?;
        if object.kind == Kind::Panel {
            return Err("a panel cannot be destroyed".to_owned());
        }
        if let Some(parent) = object.parent {
            if let Some(parent) = self.objects.get_mut(&parent) {
                parent.children.retain(|child| *child != handle);
            }
            self.changed_structure(parent);
        }
        let mut doomed = vec![handle];
        while let Some(next) = doomed.pop() {
            if let Some(object) = self.objects.remove(&next) {
                doomed.extend(object.children);
            }
            self.connections.retain(|c| c.sender != next);
            self.structure.remove(&next);
            self.times.remove(&next);
            self.players.remove(&next);
            self.sequence_views.remove(&next);
        }
        // Views showing a destroyed scene show nothing; players of a destroyed sequence play
        // nothing.
        let alive: Vec<Handle> = self.objects.keys().copied().collect();
        for object in self.objects.values_mut() {
            if object.scene.is_some_and(|scene| !alive.contains(&scene)) {
                object.scene = None;
            }
            if object.plays.is_some_and(|sequence| !alive.contains(&sequence)) {
                object.plays = None;
            }
            if object.player.is_some_and(|player| !alive.contains(&player)) {
                object.player = None;
            }
        }
        Ok(())
    }

    /// The dialogs shown, in the order they were created.
    pub fn dialogs(&self) -> Vec<Handle> {
        let mut shown: Vec<Handle> = self
            .objects
            .iter()
            .filter(|(_, object)| object.kind == Kind::Dialog && object.visible)
            .map(|(handle, _)| *handle)
            .collect();
        shown.sort_unstable();
        shown
    }

    /// Places a widget or layout in a layout, or the layout of a panel, group box or dialog.
    pub fn add_to(&mut self, container: Handle, child: Handle, cell: [u32; 4]) -> Result<(), String> {
        if cell.iter().any(|number| *number > MAX_CELL) {
            return Err(format!("a grid's rows, columns and spans go up to {MAX_CELL}"));
        }
        let container_kind = self.get(container)?.kind;
        let child_kind = self.get(child)?.kind;
        match container_kind {
            Kind::Panel | Kind::GroupBox | Kind::Dialog if child_kind.is_layout() => {
                for old in std::mem::take(&mut self.get_mut(container)?.children) {
                    if let Some(object) = self.objects.get_mut(&old) {
                        object.parent = None;
                    }
                }
            }
            Kind::Panel | Kind::GroupBox | Kind::Dialog => {
                return Err(format!("a {container_kind:?} holds one layout, not a {child_kind:?}"));
            }
            kind if kind.is_layout() && child_kind.is_widget() => {}
            kind => return Err(format!("a {kind:?} cannot hold a {child_kind:?}")),
        }
        if child == container || self.is_ancestor(child, container) {
            return Err("an object cannot hold itself".to_owned());
        }
        if let Some(old) = self.get(child)?.parent {
            self.get_mut(old)?.children.retain(|c| *c != child);
        }
        let object = self.get_mut(child)?;
        object.parent = Some(container);
        object.cell = cell;
        self.get_mut(container)?.children.push(child);
        self.wake();
        Ok(())
    }

    fn is_ancestor(&self, ancestor: Handle, mut handle: Handle) -> bool {
        while let Some(parent) = self.objects.get(&handle).and_then(|o| o.parent) {
            if parent == ancestor {
                return true;
            }
            handle = parent;
        }
        false
    }

    /// Sets a text property.
    pub fn set_text(&mut self, handle: Handle, property: Property, text: &str) -> Result<(), String> {
        if property == Property::Curves {
            return self.set_curves(handle, read_curves(text)?);
        }
        match property {
            Property::Tracks => return self.set_tracks(handle, read_tracks(text)?),
            Property::Items => return self.set_items(handle, read_items(text)?).map(drop),
            Property::Rows => return self.set_rows(handle, Rows::new(read_rows(text)?)?).map(drop),
            Property::Columns => return self.set_columns(handle, read_columns(text)?),
            _ => {}
        }
        let object = self.get_mut(handle)?;
        let field = match property {
            Property::Text => &mut object.text,
            Property::ToolTip => &mut object.tooltip,
            Property::Placeholder => &mut object.placeholder,
            Property::Title => &mut object.title,
            other => return Err(format!("{other:?} is not a text")),
        };
        text.clone_into(field);
        self.changed(handle);
        Ok(())
    }

    /// Sets the curves a curve view shows.
    pub fn set_curves(&mut self, handle: Handle, curves: Vec<ShownCurve>) -> Result<(), String> {
        self.get_mut(handle)?.curves = curves;
        self.changed(handle);
        Ok(())
    }

    /// A copy of the curves a curve view shows.
    pub fn curves(&self, handle: Handle) -> Result<Vec<ShownCurve>, String> {
        Ok(self.get(handle)?.curves.clone())
    }

    /// The items of a tree view.
    pub fn items(&self, handle: Handle) -> Result<Arc<Vec<TreeItem>>, String> {
        let object = self.get(handle)?;
        object
            .tree
            .clone()
            .ok_or_else(|| format!("a {:?} is not a tree view", object.kind))
    }

    /// The columns and rows of a table view.
    pub fn table(&self, handle: Handle) -> Result<Arc<Table>, String> {
        let object = self.get(handle)?;
        object
            .table
            .clone()
            .ok_or_else(|| format!("a {:?} is not a table view", object.kind))
    }

    /// The items of a tree view; gives back those replaced, to be freed after the lock.
    pub fn set_items(&mut self, handle: Handle, items: Vec<TreeItem>) -> Result<Arc<Vec<TreeItem>>, String> {
        let object = self.get_mut(handle)?;
        let kind = object.kind;
        let tree = object
            .tree
            .as_mut()
            .ok_or_else(|| format!("a {kind:?} is not a tree view"))?;
        // An item gone is no longer current.
        if !data::has_item(&items, object.current_item) {
            object.current_item = 0;
        }
        let replaced = std::mem::replace(tree, Arc::new(items));
        object.tree_version += 1;
        self.changed(handle);
        Ok(replaced)
    }

    /// An item of a tree view folded or unfolded; returns whether the tree holds it.
    pub fn set_item_expanded(&mut self, handle: Handle, item: u64, expanded: bool) -> Result<bool, String> {
        let object = self.get_mut(handle)?;
        let kind = object.kind;
        let tree = object
            .tree
            .as_mut()
            .ok_or_else(|| format!("a {kind:?} is not a tree view"))?;
        let Some(found) = data::find_item(Arc::make_mut(tree).as_mut_slice(), item) else {
            return Ok(false);
        };
        found.expanded = expanded;
        object.tree_version += 1;
        self.changed(handle);
        Ok(true)
    }

    /// Changes the table of a table view.
    fn change_table<R>(
        &mut self,
        handle: Handle,
        change: impl FnOnce(&mut Table) -> Result<R, String>,
    ) -> Result<R, String> {
        let object = self.get_mut(handle)?;
        let kind = object.kind;
        let table = object
            .table
            .as_mut()
            .ok_or_else(|| format!("a {kind:?} is not a table view"))?;
        let result = change(Arc::make_mut(table))?;
        // A row gone is no longer current.
        if table.row(object.current_item).is_none() {
            object.current_item = 0;
        }
        self.changed(handle);
        Ok(result)
    }

    pub fn set_columns(&mut self, handle: Handle, columns: Vec<String>) -> Result<(), String> {
        self.change_table(handle, |table| {
            table.set_columns(columns);
            Ok(())
        })
    }

    /// The rows of a table view, in the module's order, made beforehand off the lock; gives back
    /// those replaced, to be freed after it.
    pub fn set_rows(&mut self, handle: Handle, rows: Rows) -> Result<Rows, String> {
        self.change_table(handle, |table| Ok(table.set_rows(rows)))
    }

    /// The sort of a table view: a column and whether from the highest, or the module's order.
    pub fn set_sort(&mut self, handle: Handle, sort: Option<(usize, bool)>) -> Result<(), String> {
        self.change_table(handle, |table| {
            table.set_sort(sort);
            Ok(())
        })
    }

    /// Makes off the lock and off the interface's thread the sort a large table view asks for, when
    /// it is neither made nor being made. The kernel calls it as it draws the view, so that the
    /// changes of the sort between two frames make one.
    pub fn start_sort(&mut self, handle: Handle) {
        let Some(table) = self.objects.get_mut(&handle).and_then(|object| object.table.as_mut()) else {
            return;
        };
        let Some(job) = Arc::make_mut(table).sort_job() else {
            return;
        };
        let this = self.this.clone();
        in_background(Box::new(move || {
            let order = std::panic::catch_unwind(AssertUnwindSafe(|| job.run())).ok();
            if let Some(shared) = this.upgrade() {
                lock(&shared).sorted(handle, &job, order);
            }
        }));
    }

    /// The order a sort made off the lock, or none when it failed: shown if its sort is still the
    /// one asked for and the rows have not changed, otherwise made again when next drawn.
    fn sorted(&mut self, handle: Handle, job: &SortJob, order: Option<Vec<usize>>) {
        let Some(table) = self.objects.get_mut(&handle).and_then(|object| object.table.as_mut()) else {
            return;
        };
        let table = Arc::make_mut(table);
        match order {
            Some(order) => {
                table.finish_sort(job, order);
            }
            None => {
                log::error!("the sort of a table failed; its order stays");
                table.abandon_sort();
            }
        }
        self.changed(handle);
    }

    /// One cell of a table view, of the row `row`.
    pub fn set_cell(&mut self, handle: Handle, row: u64, column: usize, text: &str) -> Result<(), String> {
        self.change_table(handle, |table| table.set_cell(row, column, text))
    }

    /// Rows inserted at `at` in the module's order of a table view, without giving the others.
    pub fn insert_rows(&mut self, handle: Handle, at: usize, rows: Vec<Row>) -> Result<(), String> {
        self.change_table(handle, |table| table.insert_rows(at, rows))
    }

    /// The rows of these ids removed from a table view; returns how many there were.
    pub fn remove_rows(&mut self, handle: Handle, ids: &[u64]) -> Result<usize, String> {
        self.change_table(handle, |table| Ok(table.remove_rows(ids)))
    }

    /// The frame rate, length and tracks of a sequence.
    pub fn sequence(&self, handle: Handle) -> Result<Arc<Sequence>, String> {
        let object = self.get(handle)?;
        object
            .sequence
            .clone()
            .ok_or_else(|| format!("a {:?} is not a sequence", object.kind))
    }

    /// Creates a sequence holding `sequence`, which is no change to undo: one read from a file, for
    /// one. It follows the rules of a file.
    pub fn create_sequence(&mut self, sequence: Sequence) -> Result<Handle, String> {
        sequence.check()?;
        let handle = self.create(Kind::Sequence, None)?;
        self.get_mut(handle)?.sequence = Some(Arc::new(sequence));
        Ok(handle)
    }

    /// Sets the tracks of a sequence. A change is one undo entry, which the kernel records once it
    /// has adopted the objects; a change it refuses is not made.
    pub fn set_tracks(&mut self, handle: Handle, tracks: Vec<Track>) -> Result<(), String> {
        self.change_tracks(handle, tracks, "edit a sequence")
    }

    /// `set_tracks`, the undo entry named `label`.
    pub fn change_tracks(&mut self, handle: Handle, tracks: Vec<Track>, label: &str) -> Result<(), String> {
        self.finish_tracks(handle, None, tracks, label)
    }

    /// Sets the tracks of a sequence while a change of them goes on, recording nothing: the kernel
    /// records the change once done, with `finish_tracks`.
    pub fn set_tracks_under_way(&mut self, handle: Handle, tracks: Vec<Track>) -> Result<(), String> {
        self.replace_tracks(handle, tracks)
    }

    /// Sets the tracks of a sequence at the end of a change, which is one undo entry named `label`
    /// from the tracks `before` it began, or from those it has when none are given.
    pub fn finish_tracks(
        &mut self,
        handle: Handle,
        before: Option<Vec<Track>>,
        tracks: Vec<Track>,
        label: &str,
    ) -> Result<(), String> {
        let current = self.sequence(handle)?.tracks.clone();
        let unchanged = current == tracks;
        let before = before.unwrap_or(current);
        // Back where it began: nothing to undo.
        if before == tracks {
            return if unchanged {
                Ok(())
            } else {
                self.replace_tracks(handle, tracks)
            };
        }
        if let Some(record) = &self.recorder {
            let title = &self.get(handle)?.title;
            let change = TracksChange {
                objects: self.this.clone(),
                sequence: handle,
                before,
                after: tracks.clone(),
                document: (!title.is_empty()).then(|| title.clone()),
            };
            record(label, Box::new(change))?;
        }
        self.replace_tracks(handle, tracks)
    }

    /// Sets the tracks of a sequence, recording nothing: for its undo and redo.
    fn replace_tracks(&mut self, handle: Handle, tracks: Vec<Track>) -> Result<(), String> {
        let object = self.get_mut(handle)?;
        let kind = object.kind;
        let sequence = object
            .sequence
            .as_mut()
            .ok_or_else(|| format!("a {kind:?} is not a sequence"))?;
        Arc::make_mut(sequence).tracks = tracks;
        self.changed(handle);
        Ok(())
    }

    /// The kernel records the changes it keeps track of through `recorder` (section 3).
    pub fn set_recorder(&mut self, recorder: Recorder) {
        self.recorder = Some(recorder);
    }

    pub fn text(&self, handle: Handle, property: Property) -> Result<String, String> {
        let object = self.get(handle)?;
        Ok(match property {
            Property::Text => object.text.clone(),
            Property::ToolTip => object.tooltip.clone(),
            Property::Placeholder => object.placeholder.clone(),
            Property::Title => object.title.clone(),
            Property::Curves => ShownCurve::list_to_json(&object.curves).to_string(),
            Property::Tracks => sequence::tracks_to_json(&self.sequence(handle)?.tracks).to_string(),
            Property::Items => data::items_to_json(&self.items(handle)?).to_string(),
            Property::Rows => data::rows_to_json(self.table(handle)?.rows()).to_string(),
            Property::Columns => serde_json::json!(self.table(handle)?.columns()).to_string(),
            other => return Err(format!("{other:?} is not a text")),
        })
    }

    /// Sets a property of one or more numbers: a value, a flag (0 or 1), a colour (0xRRGGBBAA), a
    /// position (2), a rectangle or a line (4).
    pub fn set_numbers(&mut self, handle: Handle, property: Property, values: &[f64]) -> Result<(), String> {
        let expected = Self::count(property);
        if values.len() != expected {
            return Err(format!("{property:?} takes {expected} numbers, not {}", values.len()));
        }
        let first = values[0];
        if Self::is_playback(property) {
            return self.set_playback(handle, property, first);
        }
        if matches!(
            property,
            Property::CurrentItem | Property::SortColumn | Property::SortDescending
        ) {
            return self.set_data_number(handle, property, first);
        }
        let object = self.get_mut(handle)?;
        // Which items a scene draws, and in which order.
        let structural = matches!(property, Property::ZValue | Property::Visible);
        match property {
            Property::Enabled => object.enabled = first != 0.0,
            Property::Visible => object.visible = first != 0.0,
            Property::Checked => object.checked = first != 0.0,
            Property::Value => {
                object.value = first.clamp(object.minimum.min(object.maximum), object.maximum.max(object.minimum))
            }
            Property::Minimum => object.minimum = first,
            Property::Maximum => object.maximum = first,
            Property::Step => object.step = first.max(0.0),
            Property::Decimals => object.decimals = first.clamp(0.0, 10.0) as u32,
            Property::CurrentIndex => object.current_index = first as i64,
            Property::Pos => object.pos = [values[0], values[1]],
            Property::Rect => object.rect = [values[0], values[1], values[2].max(0.0), values[3].max(0.0)],
            Property::Line => object.line = [values[0], values[1], values[2], values[3]],
            Property::PenColor => object.pen_color = first as u32,
            Property::PenWidth => object.pen_width = first.max(0.0),
            Property::BrushColor => object.brush_color = first as u32,
            Property::Radius => object.radius = first.max(0.0),
            Property::ZValue => object.z = first,
            Property::Movable => object.movable = first as u32 & 3,
            Property::Selectable => object.selectable = first != 0.0,
            Property::Selected => object.selected = first != 0.0,
            Property::MoveBounds => object.bounds = Some([values[0], values[1], values[2], values[3]]),
            Property::FontSize if !first.is_finite() => return Err("a font size is a finite number".to_owned()),
            Property::FontSize => object.font_size = first.clamp(1.0, f64::from(painter::MAX_TEXT)),
            Property::MinimumHeight => object.minimum_height = first.max(0.0),
            Property::ViewScale => object.view_scale = first.clamp(0.01, 100.0),
            Property::ViewCenter => object.view_center = [values[0], values[1]],
            other => return Err(format!("{other:?} is not a number")),
        }
        if property == Property::Pos {
            self.moved(handle);
        } else {
            self.changed(handle);
        }
        if structural && let Some(parent) = self.get(handle)?.parent {
            self.changed_structure(parent);
        }
        Ok(())
    }

    /// How many numbers a property takes.
    pub fn count(property: Property) -> usize {
        match property {
            Property::Pos | Property::ViewCenter => 2,
            Property::Rect | Property::Line | Property::MoveBounds => 4,
            _ => 1,
        }
    }

    pub fn numbers(&self, handle: Handle, property: Property) -> Result<Vec<f64>, String> {
        if Self::is_playback(property) {
            return self.playback(handle, property).map(|number| vec![number]);
        }
        let object = self.get(handle)?;
        let sort = object.table.as_ref().and_then(|table| table.sort());
        match property {
            Property::CurrentItem if matches!(object.kind, Kind::TreeView | Kind::TableView) => {
                return Ok(vec![object.current_item as f64]);
            }
            Property::SortColumn if object.kind == Kind::TableView => {
                return Ok(vec![sort.map_or(-1.0, |(column, _)| column as f64)]);
            }
            Property::SortDescending if object.kind == Kind::TableView => {
                return Ok(vec![if sort.is_some_and(|(_, descending)| descending) {
                    1.0
                } else {
                    0.0
                }]);
            }
            _ => {}
        }
        let flag = |value: bool| if value { 1.0 } else { 0.0 };
        Ok(match property {
            Property::Enabled => vec![flag(object.enabled)],
            Property::Visible => vec![flag(object.visible)],
            Property::Checked => vec![flag(object.checked)],
            Property::Value => vec![object.value],
            Property::Minimum => vec![object.minimum],
            Property::Maximum => vec![object.maximum],
            Property::Step => vec![object.step],
            Property::Decimals => vec![f64::from(object.decimals)],
            Property::CurrentIndex => vec![object.current_index as f64],
            Property::Pos => object.pos.to_vec(),
            Property::Rect => object.rect.to_vec(),
            Property::Line => object.line.to_vec(),
            Property::PenColor => vec![f64::from(object.pen_color)],
            Property::PenWidth => vec![object.pen_width],
            Property::BrushColor => vec![f64::from(object.brush_color)],
            Property::Radius => vec![object.radius],
            Property::ZValue => vec![object.z],
            Property::Movable => vec![f64::from(object.movable)],
            Property::Selectable => vec![flag(object.selectable)],
            Property::Selected => vec![flag(object.selected)],
            Property::MoveBounds => object.bounds.map_or_else(Vec::new, |b| b.to_vec()),
            Property::FontSize => vec![object.font_size],
            Property::MinimumHeight => vec![object.minimum_height],
            Property::ViewScale => vec![object.view_scale],
            Property::ViewCenter => object.view_center.to_vec(),
            Property::Count => vec![(object.items.len().max(object.children.len())) as f64],
            other => return Err(format!("{other:?} is not a number")),
        })
    }

    /// The current item of a tree view or a table view, or the sort of a table view.
    fn set_data_number(&mut self, handle: Handle, property: Property, value: f64) -> Result<(), String> {
        let object = self.get(handle)?;
        let kind = object.kind;
        let fits = match property {
            Property::CurrentItem => matches!(kind, Kind::TreeView | Kind::TableView),
            _ => kind == Kind::TableView,
        };
        if !fits {
            return Err(format!("a {kind:?} has no {property:?}"));
        }
        if !value.is_finite() || value.fract() != 0.0 {
            return Err(format!("{property:?} is a whole number"));
        }
        let sort = object.table.as_ref().and_then(|table| table.sort());
        match property {
            Property::CurrentItem => {
                let id = value as u64;
                let known = value == 0.0
                    || (value > 0.0
                        && value <= data::MAX_ID as f64
                        && (object.tree.as_ref().is_some_and(|items| data::has_item(items, id))
                            || object.table.as_ref().is_some_and(|table| table.row(id).is_some())));
                if !known {
                    return Err(format!("no item {value}"));
                }
                self.get_mut(handle)?.current_item = id;
                self.changed(handle);
                Ok(())
            }
            Property::SortColumn => {
                let descending = sort.is_some_and(|(_, descending)| descending);
                self.set_sort(handle, (value >= 0.0).then_some((value as usize, descending)))
            }
            _ => self.set_sort(handle, sort.map(|(column, _)| (column, value != 0.0))),
        }
    }

    /// A number of a sequence or of a player.
    fn is_playback(property: Property) -> bool {
        matches!(
            property,
            Property::FrameRate
                | Property::Length
                | Property::Sequence
                | Property::Time
                | Property::Playing
                | Property::Loop
                | Property::Speed
                | Property::Player
        )
    }

    /// The object `handle` if it has `property`: a sequence its frame rate and length, a player
    /// its sequence and playback, a dopesheet view or a curve view its sequence and player.
    fn playback_object(&self, handle: Handle, property: Property) -> Result<&Object, String> {
        let object = self.get(handle)?;
        let view = matches!(object.kind, Kind::DopesheetView | Kind::CurveView);
        let has = match property {
            Property::FrameRate | Property::Length => object.kind == Kind::Sequence,
            Property::Sequence => object.kind == Kind::Player || view,
            Property::Player => view,
            _ => object.kind == Kind::Player,
        };
        if !has {
            return Err(format!("a {:?} has no {property:?}", object.kind));
        }
        Ok(object)
    }

    /// The object `value` names, of `kind`, among this module's: none for 0.
    fn handle_of(&self, value: f64, kind: Kind) -> Result<Option<Handle>, String> {
        if value == 0.0 {
            return Ok(None);
        }
        let handle = value as Handle;
        if value.fract() != 0.0 || value < 0.0 || self.objects.get(&handle).is_none_or(|o| o.kind != kind) {
            return Err(format!("no {kind:?} {value} among the module's objects"));
        }
        Ok(Some(handle))
    }

    /// The length of the sequence a player plays, if it plays one.
    fn length_played(&self, player: &Object) -> Option<f64> {
        let sequence = self.objects.get(&player.plays?)?.sequence.as_ref()?;
        Some(f64::from(sequence.length))
    }

    fn set_playback(&mut self, handle: Handle, property: Property, value: f64) -> Result<(), String> {
        let object = self.playback_object(handle, property)?;
        if !value.is_finite() {
            return Err(format!("{property:?} is a finite number"));
        }
        let length = self.length_played(object);
        match property {
            Property::FrameRate | Property::Length => {
                let high = if property == Property::FrameRate {
                    MAX_FRAME_RATE
                } else {
                    MAX_LENGTH
                };
                if value.fract() != 0.0 || !(1.0..=f64::from(high)).contains(&value) {
                    return Err(format!("{property:?} is a whole number from 1 to {high}"));
                }
                let sequence = self.get_mut(handle)?.sequence.as_mut().expect("checked a sequence");
                let sequence = Arc::make_mut(sequence);
                if property == Property::FrameRate {
                    sequence.frame_rate = value as u32;
                } else {
                    sequence.length = value as u32;
                }
            }
            Property::Sequence => {
                let plays = self.handle_of(value, Kind::Sequence)?;
                self.get_mut(handle)?.plays = plays;
            }
            Property::Player => {
                let player = self.handle_of(value, Kind::Player)?;
                self.get_mut(handle)?.player = player;
            }
            Property::Time => {
                self.get_mut(handle)?.time = value.clamp(0.0, length.unwrap_or(f64::MAX));
            }
            Property::Playing => {
                let player = self.get_mut(handle)?;
                let playing = value != 0.0;
                if playing && !player.playing && length.is_some_and(|length| player.time >= length) {
                    player.time = 0.0;
                }
                player.playing = playing;
            }
            Property::Loop => self.get_mut(handle)?.looping = value != 0.0,
            Property::Speed => self.get_mut(handle)?.speed = value.clamp(0.0, MAX_SPEED),
            _ => unreachable!("only the numbers of a sequence or a player come here"),
        }
        self.changed(handle);
        Ok(())
    }

    fn playback(&self, handle: Handle, property: Property) -> Result<f64, String> {
        let object = self.playback_object(handle, property)?;
        let flag = |value: bool| if value { 1.0 } else { 0.0 };
        Ok(match property {
            Property::FrameRate => f64::from(object.sequence.as_ref().map_or(0, |s| s.frame_rate)),
            Property::Length => f64::from(object.sequence.as_ref().map_or(0, |s| s.length)),
            Property::Sequence => object.plays.map_or(0.0, |sequence| sequence as f64),
            Property::Player => object.player.map_or(0.0, |player| player as f64),
            Property::Time => object.time,
            Property::Playing => flag(object.playing),
            Property::Loop => flag(object.looping),
            Property::Speed => object.speed,
            _ => unreachable!("only the numbers of a sequence or a player come here"),
        })
    }

    /// Moves the players that play on by `seconds`, sending `timeChanged` and, at the end of a
    /// sequence played without loop, `finished`. Returns each player that has a sequence, with its
    /// time, and whether one moves on.
    pub fn advance_players(&mut self, seconds: f64) -> (Vec<PlayerFrame>, bool) {
        let (mut frames, mut signals, mut playing) = (Vec::new(), Vec::new(), false);
        let players: Vec<Handle> = self.players.iter().copied().collect();
        for player in players {
            let Some((sequence, generation, data)) = self.objects[&player].plays.and_then(|sequence| {
                let object = self.objects.get(&sequence)?;
                Some((sequence, object.generation, object.sequence.clone()?))
            }) else {
                continue;
            };
            let length = f64::from(data.length);
            let object = self.objects.get_mut(&player).expect("listed above");
            if object.playing {
                let mut time = object.time + seconds * f64::from(data.frame_rate) * object.speed;
                if time >= length {
                    if object.looping {
                        time %= length;
                    } else {
                        time = length;
                        object.playing = false;
                    }
                }
                if time != object.time {
                    object.time = time;
                    signals.push((player, Signal::TimeChanged, time));
                }
                if !object.playing {
                    signals.push((player, Signal::Finished, time));
                } else if object.speed > 0.0 {
                    // At speed 0 nothing moves until the speed changes, which wakes the interface.
                    playing = true;
                }
            } else {
                // A sequence made shorter.
                object.time = object.time.min(length);
            }
            frames.push(PlayerFrame {
                player,
                time: object.time,
                sequence,
                generation,
                data,
            });
        }
        for (sender, signal, time) in signals {
            if signal == Signal::TimeChanged {
                self.emit_time(sender, time);
            } else {
                self.emit(SignalData {
                    sender,
                    signal: signal as u32,
                    number: time,
                    ..Default::default()
                });
            }
        }
        (frames, playing)
    }

    /// Sends a player's `timeChanged` as a job that records nothing: it neither blocks Undo nor
    /// counts as the module's work. While the one sent before still waits, it takes the new time.
    fn emit_time(&mut self, player: Handle, time: f64) {
        let data = SignalData {
            sender: player,
            signal: Signal::TimeChanged as u32,
            number: time,
            ..Default::default()
        };
        if let Some(waiting) = self.times.get(&player) {
            let mut waiting = waiting.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pending) = waiting.as_mut() {
                *pending = data;
                return;
            }
        }
        let slots = self.slots(player, data.signal);
        if slots.is_empty() {
            return;
        }
        let this = self.this.clone();
        let waiting: Waiting = Arc::new(Mutex::new(Some(data)));
        let taken = waiting.clone();
        self.post_uncounted_job(Box::new(move || {
            let data = taken.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(data) = data {
                deliver(&this, slots, &data);
            }
        }));
        self.times.insert(player, waiting);
    }

    /// Adds an entry to a combo box.
    pub fn add_entry(&mut self, handle: Handle, text: &str) -> Result<(), String> {
        let object = self.get_mut(handle)?;
        if object.kind != Kind::ComboBox {
            return Err(format!("a {:?} has no entries", object.kind));
        }
        object.items.push(text.to_owned());
        if object.current_index < 0 {
            object.current_index = 0;
        }
        self.changed(handle);
        Ok(())
    }

    pub fn clear_entries(&mut self, handle: Handle) -> Result<(), String> {
        let object = self.get_mut(handle)?;
        object.items.clear();
        object.current_index = -1;
        self.changed(handle);
        Ok(())
    }

    /// Shows a scene in a view.
    pub fn set_scene(&mut self, view: Handle, scene: Handle) -> Result<(), String> {
        if self.get(scene)?.kind != Kind::GraphicsScene {
            return Err("a view shows a scene".to_owned());
        }
        let object = self.get_mut(view)?;
        if object.kind != Kind::GraphicsView {
            return Err(format!("a {:?} shows no scene", object.kind));
        }
        object.scene = Some(scene);
        self.wake();
        Ok(())
    }

    /// Asks a painting area to be painted again.
    pub fn update(&mut self, area: Handle) -> Result<(), String> {
        let object = self.get_mut(area)?;
        if object.kind != Kind::PaintArea {
            return Err(format!("a {:?} is not painted", object.kind));
        }
        object.repaint = true;
        self.wake();
        Ok(())
    }

    /// Connects a signal of an object to a slot; returns the connection.
    pub fn connect(&mut self, sender: Handle, signal: Signal, slot: Slot) -> Result<u64, String> {
        self.get(sender)?;
        let id = self.next_connection;
        self.next_connection += 1;
        self.connections.push(Connection {
            id,
            sender,
            signal,
            slot,
        });
        Ok(id)
    }

    pub fn disconnect(&mut self, connection: u64) {
        self.connections.retain(|c| c.id != connection);
    }

    /// Records a command of a painter while its module paints.
    pub fn paint(&mut self, painter: Handle, command: PaintCommand) -> Result<(), String> {
        let (_, commands) = self
            .painting
            .get_mut(&painter)
            .ok_or_else(|| format!("no painter {painter}: paint only inside a Paint slot"))?;
        commands.push(command);
        Ok(())
    }

    /// Whether a slot is connected to `signal` of `sender`: what it would carry need not be made
    /// otherwise.
    pub fn has_slots(&self, sender: Handle, signal: Signal) -> bool {
        self.connections
            .iter()
            .any(|c| c.sender == sender && c.signal == signal)
    }

    /// The views showing a sequence, with that sequence and the player they show.
    pub fn shown_sequences(&self) -> Vec<SequenceShown> {
        self.sequence_views
            .iter()
            .filter_map(|handle| {
                let view = self.objects.get(handle)?;
                let sequence = view.plays?;
                let data = self.objects.get(&sequence)?.sequence.clone()?;
                let player = view
                    .player
                    .filter(|player| self.objects.get(player).is_some_and(|o| o.kind == Kind::Player));
                let time = player
                    .and_then(|player| self.objects.get(&player))
                    .map(|player| player.time);
                Some(SequenceShown {
                    view: *handle,
                    sequence,
                    data,
                    player,
                    time,
                })
            })
            .collect()
    }

    /// The slots connected to a signal of `sender`, with their connections.
    fn slots(&self, sender: Handle, signal: u32) -> Vec<(u64, Slot)> {
        self.connections
            .iter()
            .filter(|c| c.sender == sender && c.signal as u32 == signal)
            .map(|c| (c.id, c.slot.clone()))
            .collect()
    }

    /// Where to post the jobs that record nothing in the history, such as the writes of a property:
    /// they then neither block Undo nor count as the module's work.
    pub fn set_uncounted_post(&mut self, post: Post) {
        self.uncounted = Some(post);
    }

    /// Runs on the module's thread, after the jobs posted before, a job that records nothing in the
    /// history.
    pub fn post_uncounted_job(&mut self, job: Box<dyn FnOnce() + Send>) {
        self.waiting = None;
        (self.uncounted.as_ref().unwrap_or(&self.post))(job);
    }

    /// Runs a job on the module's thread, after those posted before.
    pub fn post_job(&mut self, job: Box<dyn FnOnce() + Send>) {
        // A signal waiting behind another job is no longer the last one: nothing merges into it.
        self.waiting = None;
        (self.post)(job);
    }

    /// Sends a signal to the slots connected to it, on the module's thread. A mouse move, or a
    /// change of curves or keys still under way, replaces the same one of the same object still
    /// waiting.
    pub fn emit(&mut self, data: SignalData) {
        let under_way = matches!(
            Signal::from_u32(data.signal),
            Some(Signal::CurvesChanged | Signal::KeysChanged)
        );
        let mergeable = data.signal == Signal::MouseMove as u32 || (under_way && !data.boolean);
        let (sender, signal) = (data.sender, data.signal);
        if mergeable
            && let Some((waiting_sender, waiting_signal, waiting)) = &self.waiting
            && (*waiting_sender, *waiting_signal) == (sender, signal)
        {
            let mut waiting = waiting.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pending) = waiting.as_mut() {
                *pending = data;
                return;
            }
        }
        let slots = self.slots(sender, signal);
        if slots.is_empty() {
            return;
        }
        let this = self.this.clone();
        let waiting: Waiting = Arc::new(Mutex::new(Some(data)));
        let taken = waiting.clone();
        self.post_job(Box::new(move || {
            let data = taken.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(data) = data {
                deliver(&this, slots, &data);
            }
        }));
        if mergeable {
            self.waiting = Some((sender, signal, waiting));
        }
    }

    /// Asks the module to paint an area of `size`; the picture replaces the area's once painted.
    /// One painting at a time per area: a size asked meanwhile is painted when it ends.
    pub fn request_paint(&mut self, shared: &SharedUi, area: Handle, size: [f64; 2]) {
        let slots = self.slots(area, Signal::Paint as u32);
        if slots.is_empty() {
            return;
        }
        if let Some(next) = self.in_flight.get_mut(&area) {
            *next = Some(size);
            return;
        }
        self.in_flight.insert(area, None);
        let painter = self.next;
        self.next += 1;
        self.painting.insert(painter, (area, Vec::new()));
        let done = PaintingDone {
            shared: shared.clone(),
            area,
            painter,
        };
        let this = self.this.clone();
        self.post_job(Box::new(move || {
            let _done = done;
            let data = SignalData {
                sender: area,
                signal: Signal::Paint as u32,
                width: size[0],
                height: size[1],
                painter,
                ..Default::default()
            };
            deliver(&this, slots, &data);
        }));
    }

    /// The scene an item belongs to, through its groups.
    pub fn scene_of(&self, mut handle: Handle) -> Option<Handle> {
        loop {
            let object = self.objects.get(&handle)?;
            if object.kind == Kind::GraphicsScene {
                return Some(handle);
            }
            handle = object.parent?;
        }
    }

    /// The version of a scene's set of items and their order.
    pub fn structure(&self, scene: Handle) -> u64 {
        self.structure.get(&scene).copied().unwrap_or(0)
    }

    fn changed_structure(&mut self, handle: Handle) {
        if let Some(scene) = self.scene_of(handle) {
            *self.structure.entry(scene).or_default() += 1;
        }
        self.wake();
    }

    fn changed(&mut self, handle: Handle) {
        if let Some(object) = self.objects.get_mut(&handle) {
            object.generation += 1;
        }
        self.wake();
    }

    /// A moved item, and every item of a moved group, are drawn again.
    pub fn moved(&mut self, handle: Handle) {
        let mut stack = vec![handle];
        while let Some(next) = stack.pop() {
            if let Some(object) = self.objects.get_mut(&next) {
                object.generation += 1;
                stack.extend(object.children.iter().copied());
            }
        }
        self.wake();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{Kind, Property, Signal, SignalData, Ui, lock};
    use crate::AppliedChange;

    type Jobs = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

    /// A Ui whose posted jobs run at once on the calling thread.
    fn ui() -> super::SharedUi {
        Ui::new(Arc::new(|job| job()))
    }

    /// A Ui whose posted jobs wait for `run`, as on the module's thread.
    fn queued() -> (super::SharedUi, Jobs) {
        let jobs: Jobs = Arc::default();
        let queue = jobs.clone();
        (Ui::new(Arc::new(move |job| queue.lock().unwrap().push(job))), jobs)
    }

    fn run(jobs: &Jobs) {
        let waiting = std::mem::take(&mut *jobs.lock().unwrap());
        for job in waiting {
            job();
        }
    }

    #[test]
    fn widgets_go_in_layouts_and_layouts_in_panels() {
        let shared = ui();
        let mut ui = lock(&shared);
        let panel = ui.panel("main");
        assert_eq!(ui.panel("main"), panel, "the same panel the second time");
        let layout = ui.create(Kind::VBoxLayout, None).unwrap();
        let button = ui.create(Kind::PushButton, None).unwrap();
        ui.add_to(layout, button, [0, 0, 1, 1]).unwrap();
        ui.add_to(panel, layout, [0, 0, 1, 1]).unwrap();
        assert_eq!(ui.object(panel).unwrap().children, vec![layout]);
        assert!(
            ui.add_to(panel, button, [0, 0, 1, 1]).is_err(),
            "a panel holds a layout"
        );
        assert!(
            ui.add_to(button, layout, [0, 0, 1, 1]).is_err(),
            "a button holds nothing"
        );
        assert!(ui.create(Kind::RectItem, None).is_err(), "an item needs a scene");
    }

    #[test]
    fn destroying_a_layout_destroys_its_widgets_and_connections() {
        let shared = ui();
        let mut ui = lock(&shared);
        let layout = ui.create(Kind::HBoxLayout, None).unwrap();
        let button = ui.create(Kind::PushButton, None).unwrap();
        ui.add_to(layout, button, [0, 0, 1, 1]).unwrap();
        ui.connect(button, Signal::Clicked, Arc::new(|_| {})).unwrap();
        ui.destroy(layout).unwrap();
        assert!(ui.object(button).is_none());
        assert!(ui.connections.is_empty());
    }

    #[test]
    fn properties_are_checked_and_read_back() {
        let shared = ui();
        let mut ui = lock(&shared);
        let slider = ui.create(Kind::Slider, None).unwrap();
        ui.set_numbers(slider, Property::Maximum, &[10.0]).unwrap();
        ui.set_numbers(slider, Property::Value, &[42.0]).unwrap();
        assert_eq!(
            ui.numbers(slider, Property::Value).unwrap(),
            vec![10.0],
            "clamped to the range"
        );
        assert!(
            ui.set_numbers(slider, Property::Rect, &[1.0]).is_err(),
            "a rectangle takes four numbers"
        );
        assert!(
            ui.set_text(slider, Property::Value, "x").is_err(),
            "a value is not a text"
        );
        ui.set_text(slider, Property::ToolTip, "speed").unwrap();
        assert_eq!(ui.text(slider, Property::ToolTip).unwrap(), "speed");
    }

    #[test]
    fn a_signal_reaches_only_its_slots() {
        let (shared, jobs) = queued();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut ui = lock(&shared);
        let first = ui.create(Kind::PushButton, None).unwrap();
        let second = ui.create(Kind::PushButton, None).unwrap();
        let seen = received.clone();
        let connection = ui
            .connect(
                first,
                Signal::Clicked,
                Arc::new(move |data: &SignalData| seen.lock().unwrap().push(data.sender)),
            )
            .unwrap();
        let click = |sender| SignalData {
            sender,
            signal: Signal::Clicked as u32,
            ..Default::default()
        };
        ui.emit(click(first));
        ui.emit(click(second));
        drop(ui);
        run(&jobs);
        let mut ui = lock(&shared);
        ui.disconnect(connection);
        ui.emit(click(first));
        drop(ui);
        run(&jobs);
        assert_eq!(*received.lock().unwrap(), vec![first]);
    }

    #[test]
    fn an_area_is_painted_once_at_a_time_then_at_the_last_size_asked() {
        let (shared, jobs) = queued();
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let area = {
            let mut store = lock(&shared);
            let area = store.create(Kind::PaintArea, None).unwrap();
            let seen = sizes.clone();
            store
                .connect(
                    area,
                    Signal::Paint,
                    Arc::new(move |data: &SignalData| seen.lock().unwrap().push(data.width)),
                )
                .unwrap();
            for width in [100.0, 200.0, 300.0] {
                store.request_paint(&shared, area, [width, 50.0]);
            }
            area
        };
        assert_eq!(jobs.lock().unwrap().len(), 1, "one painting at a time");
        run(&jobs);
        run(&jobs);
        assert_eq!(*sizes.lock().unwrap(), vec![100.0, 300.0]);
        assert!(!lock(&shared).in_flight.contains_key(&area));
    }

    #[test]
    fn mouse_moves_still_waiting_merge_but_never_across_another_signal() {
        let (shared, jobs) = queued();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut store = lock(&shared);
        let area = store.create(Kind::PaintArea, None).unwrap();
        for signal in [Signal::MouseMove, Signal::MouseRelease] {
            let seen = seen.clone();
            store
                .connect(
                    area,
                    signal,
                    Arc::new(move |data: &SignalData| seen.lock().unwrap().push((data.signal, data.x))),
                )
                .unwrap();
        }
        let event = |signal: Signal, x| SignalData {
            sender: area,
            signal: signal as u32,
            x,
            ..Default::default()
        };
        store.emit(event(Signal::MouseMove, 1.0));
        store.emit(event(Signal::MouseMove, 2.0));
        store.emit(event(Signal::MouseRelease, 2.0));
        store.emit(event(Signal::MouseMove, 3.0));
        drop(store);
        assert_eq!(jobs.lock().unwrap().len(), 3);
        run(&jobs);
        let (moved, released) = (Signal::MouseMove as u32, Signal::MouseRelease as u32);
        assert_eq!(*seen.lock().unwrap(), vec![(moved, 2.0), (released, 2.0), (moved, 3.0)]);
    }

    #[test]
    fn a_slot_disconnected_or_destroyed_before_its_turn_is_not_called() {
        let (shared, jobs) = queued();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut store = lock(&shared);
        let button = store.create(Kind::PushButton, None).unwrap();
        let other = store.create(Kind::PushButton, None).unwrap();
        let second = Arc::new(Mutex::new(0));
        // The first slot disconnects the second, as a module does from its own thread.
        let (ui_of_first, second_of_first, seen) = (shared.clone(), second.clone(), calls.clone());
        store
            .connect(
                button,
                Signal::Clicked,
                Arc::new(move |_: &SignalData| {
                    seen.lock().unwrap().push("first");
                    lock(&ui_of_first).disconnect(*second_of_first.lock().unwrap());
                }),
            )
            .unwrap();
        let seen = calls.clone();
        let id = store
            .connect(
                button,
                Signal::Clicked,
                Arc::new(move |_: &SignalData| seen.lock().unwrap().push("second")),
            )
            .unwrap();
        *second.lock().unwrap() = id;
        let seen = calls.clone();
        store
            .connect(
                other,
                Signal::Clicked,
                Arc::new(move |_: &SignalData| seen.lock().unwrap().push("other")),
            )
            .unwrap();
        let click = |sender| SignalData {
            sender,
            signal: Signal::Clicked as u32,
            ..Default::default()
        };
        store.emit(click(button));
        store.emit(click(other));
        // Destroyed after its signal was sent, before it was delivered.
        store.destroy(other).unwrap();
        drop(store);
        run(&jobs);
        assert_eq!(*calls.lock().unwrap(), vec!["first"]);
    }

    #[test]
    fn moving_a_group_draws_its_items_again() {
        let shared = ui();
        let mut ui = lock(&shared);
        let scene = ui.create(Kind::GraphicsScene, None).unwrap();
        let group = ui.create(Kind::ItemGroup, Some(scene)).unwrap();
        let rect = ui.create(Kind::RectItem, Some(group)).unwrap();
        let before = ui.object(rect).unwrap().generation;
        let structure = ui.structure(scene);
        ui.set_numbers(group, Property::Pos, &[10.0, 0.0]).unwrap();
        assert!(ui.object(rect).unwrap().generation > before);
        assert_eq!(ui.scene_of(rect), Some(scene));
        ui.create(Kind::LineItem, Some(scene)).unwrap();
        assert!(
            ui.structure(scene) > structure,
            "a new item changes the scene's structure"
        );
    }

    #[test]
    fn hiding_a_group_changes_what_its_scene_draws() {
        let shared = ui();
        let mut ui = lock(&shared);
        let scene = ui.create(Kind::GraphicsScene, None).unwrap();
        let group = ui.create(Kind::ItemGroup, Some(scene)).unwrap();
        ui.create(Kind::RectItem, Some(group)).unwrap();
        let structure = ui.structure(scene);
        ui.set_numbers(group, Property::Visible, &[0.0]).unwrap();
        assert!(ui.structure(scene) > structure);
    }

    #[test]
    fn a_text_item_refuses_an_infinite_font_size() {
        let shared = ui();
        let mut ui = lock(&shared);
        let scene = ui.create(Kind::GraphicsScene, None).unwrap();
        let text = ui.create(Kind::TextItem, Some(scene)).unwrap();
        assert!(ui.set_numbers(text, Property::FontSize, &[f64::INFINITY]).is_err());
        assert!(ui.set_numbers(text, Property::FontSize, &[f64::NAN]).is_err());
        ui.set_numbers(text, Property::FontSize, &[1e12]).unwrap();
        assert_eq!(ui.numbers(text, Property::FontSize).unwrap(), vec![512.0]);
    }

    #[test]
    fn a_grid_refuses_cells_beyond_its_limit() {
        let shared = ui();
        let mut ui = lock(&shared);
        let grid = ui.create(Kind::GridLayout, None).unwrap();
        let label = ui.create(Kind::Label, None).unwrap();
        assert!(
            ui.add_to(grid, label, [0, u32::MAX, 1, 1]).is_err(),
            "a column of -1 from C#"
        );
        assert!(ui.add_to(grid, label, [3, 4, 1, 2]).is_ok());
    }

    #[test]
    fn a_dialog_starts_hidden_and_holds_one_layout() {
        let shared = ui();
        let mut ui = lock(&shared);
        let dialog = ui.create(Kind::Dialog, None).unwrap();
        assert!(ui.dialogs().is_empty(), "created hidden");
        let layout = ui.create(Kind::VBoxLayout, None).unwrap();
        ui.add_to(dialog, layout, [0, 0, 1, 1]).unwrap();
        let button = ui.create(Kind::PushButton, None).unwrap();
        assert!(ui.add_to(dialog, button, [0, 0, 1, 1]).is_err(), "only a layout");
        assert!(
            ui.add_to(layout, dialog, [0, 0, 1, 1]).is_err(),
            "never inside a layout"
        );
        ui.set_numbers(dialog, Property::Visible, &[1.0]).unwrap();
        assert_eq!(ui.dialogs(), vec![dialog]);
    }

    /// One track on `cube/opacity`, with a key of `value` at frame 0.
    fn tracks(value: f64) -> String {
        format!(
            "[{{\"property\":\"cube/opacity\",\"kind\":\"number\",\"curves\":[{{\"keys\":[{{\"time\":0,\"value\":{value}}}]}}]}}]"
        )
    }

    #[test]
    fn a_sequence_s_tracks_cross_their_json_and_wrong_ones_are_refused() {
        let shared = ui();
        let mut ui = lock(&shared);
        let sequence = ui.create(Kind::Sequence, None).unwrap();
        ui.set_text(sequence, Property::Tracks, &tracks(0.5)).unwrap();
        let text = ui.text(sequence, Property::Tracks).unwrap();
        assert!(text.contains("\"property\":\"cube/opacity\""), "{text}");
        assert_eq!(
            super::read_tracks(&text).unwrap(),
            ui.sequence(sequence).unwrap().tracks
        );
        let wrong = [
            "[{\"property\":\"cube/opacity\",\"kind\":\"number\",\"curves\":[{\"keys\":[{\"time\":2.5,\"value\":1}]}]}]",
            "[{\"property\":\"cube/position\",\"kind\":\"vector\",\"curves\":[{\"keys\":[]}]}]",
            "{",
        ];
        for text in wrong {
            assert!(ui.set_text(sequence, Property::Tracks, text).is_err(), "{text}");
        }
        assert_eq!(ui.sequence(sequence).unwrap().tracks[0].curves[0].keys[0].value, 0.5);
        let label = ui.create(Kind::Label, None).unwrap();
        assert!(
            ui.set_text(label, Property::Tracks, &tracks(0.5)).is_err(),
            "only a sequence"
        );
    }

    #[test]
    fn a_sequence_and_a_player_keep_their_numbers_within_their_rules() {
        let shared = ui();
        let mut ui = lock(&shared);
        let sequence = ui.create(Kind::Sequence, None).unwrap();
        let player = ui.create(Kind::Player, None).unwrap();
        for (property, wrong) in [
            (Property::FrameRate, 0.0),
            (Property::FrameRate, 241.0),
            (Property::Length, 2.5),
        ] {
            assert!(
                ui.set_numbers(sequence, property, &[wrong]).is_err(),
                "{property:?} {wrong}"
            );
        }
        ui.set_numbers(sequence, Property::FrameRate, &[24.0]).unwrap();
        ui.set_numbers(sequence, Property::Length, &[48.0]).unwrap();
        assert_eq!(ui.numbers(sequence, Property::Length).unwrap(), vec![48.0]);
        assert!(
            ui.set_numbers(player, Property::FrameRate, &[24.0]).is_err(),
            "a player has no frame rate"
        );
        assert!(
            ui.set_numbers(player, Property::Sequence, &[player as f64]).is_err(),
            "not a sequence"
        );
        ui.set_numbers(player, Property::Sequence, &[sequence as f64]).unwrap();
        ui.set_numbers(player, Property::Time, &[100.0]).unwrap();
        assert_eq!(
            ui.numbers(player, Property::Time).unwrap(),
            vec![48.0],
            "within the length"
        );
        ui.set_numbers(player, Property::Speed, &[1e6]).unwrap();
        assert_eq!(ui.numbers(player, Property::Speed).unwrap(), vec![super::MAX_SPEED]);
        assert!(ui.set_numbers(player, Property::Time, &[f64::NAN]).is_err());
        let layout = ui.create(Kind::VBoxLayout, None).unwrap();
        assert!(
            ui.add_to(layout, sequence, [0, 0, 1, 1]).is_err(),
            "never placed in a layout"
        );
        ui.destroy(sequence).unwrap();
        assert_eq!(
            ui.numbers(player, Property::Sequence).unwrap(),
            vec![0.0],
            "its sequence gone"
        );
    }

    #[test]
    fn a_player_plays_loops_and_finishes() {
        let (shared, jobs) = queued();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut store = lock(&shared);
        let sequence = store.create(Kind::Sequence, None).unwrap();
        store.set_numbers(sequence, Property::Length, &[60.0]).unwrap();
        let player = store.create(Kind::Player, None).unwrap();
        store
            .set_numbers(player, Property::Sequence, &[sequence as f64])
            .unwrap();
        for signal in [Signal::TimeChanged, Signal::Finished] {
            let seen = seen.clone();
            store
                .connect(
                    player,
                    signal,
                    Arc::new(move |data: &SignalData| seen.lock().unwrap().push((data.signal, data.number))),
                )
                .unwrap();
        }
        let (moved, finished) = (Signal::TimeChanged as u32, Signal::Finished as u32);
        let (frames, playing) = store.advance_players(1.0);
        assert!(!playing && frames[0].time == 0.0, "paused, it stays");
        store.set_numbers(player, Property::Playing, &[1.0]).unwrap();
        store.advance_players(0.5);
        store.advance_players(0.5);
        drop(store);
        run(&jobs);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(moved, 30.0)],
            "two frames, one signal: the last time"
        );
        let mut store = lock(&shared);
        let (frames, playing) = store.advance_players(2.0);
        assert!(!playing && frames[0].time == 60.0, "stopped at the end");
        drop(store);
        run(&jobs);
        assert_eq!(seen.lock().unwrap()[1..], [(moved, 60.0), (finished, 60.0)]);
        let mut store = lock(&shared);
        store.set_numbers(player, Property::Loop, &[1.0]).unwrap();
        store.set_numbers(player, Property::Playing, &[1.0]).unwrap();
        assert_eq!(
            store.numbers(player, Property::Time).unwrap(),
            vec![0.0],
            "from the start"
        );
        let (frames, playing) = store.advance_players(2.5);
        assert!(playing && frames[0].time == 15.0, "looped: {}", frames[0].time);
        store.set_numbers(player, Property::Speed, &[0.0]).unwrap();
        let (frames, playing) = store.advance_players(1.0);
        assert!(!playing && frames[0].time == 15.0, "at speed 0 it asks for no frame");
    }

    #[test]
    fn a_change_of_tracks_is_recorded_once_and_undone_without_recording() {
        let shared = ui();
        let recorded: Arc<Mutex<Vec<Box<dyn AppliedChange>>>> = Arc::default();
        let refuse = Arc::new(Mutex::new(false));
        let (kept, refusing) = (recorded.clone(), refuse.clone());
        let sequence = {
            let mut ui = lock(&shared);
            ui.set_recorder(Arc::new(move |label, change| {
                assert_eq!(label, "edit a sequence");
                if *refusing.lock().unwrap() {
                    return Err("not now".to_owned());
                }
                kept.lock().unwrap().push(change);
                Ok(())
            }));
            let sequence = ui.create(Kind::Sequence, None).unwrap();
            ui.set_text(sequence, Property::Tracks, &tracks(1.0)).unwrap();
            ui.set_text(sequence, Property::Tracks, &tracks(1.0)).unwrap();
            ui.set_text(sequence, Property::Tracks, &tracks(2.0)).unwrap();
            sequence
        };
        assert_eq!(
            recorded.lock().unwrap().len(),
            2,
            "the same tracks again change nothing"
        );
        let value = || lock(&shared).sequence(sequence).unwrap().tracks[0].curves[0].keys[0].value;
        let mut last = recorded.lock().unwrap().pop().unwrap();
        last.undo();
        assert_eq!(value(), 1.0);
        last.redo();
        assert_eq!(value(), 2.0);
        assert_eq!(recorded.lock().unwrap().len(), 1, "undo and redo record nothing");
        *refuse.lock().unwrap() = true;
        assert!(
            lock(&shared)
                .set_text(sequence, Property::Tracks, &tracks(3.0))
                .is_err()
        );
        assert_eq!(value(), 2.0, "a change that cannot be recorded is not made");
    }

    #[test]
    fn a_view_shows_a_sequence_and_a_player_of_its_module() {
        let shared = ui();
        let mut ui = lock(&shared);
        let sequence = ui.create(Kind::Sequence, None).unwrap();
        let player = ui.create(Kind::Player, None).unwrap();
        for kind in [Kind::DopesheetView, Kind::CurveView] {
            let view = ui.create(kind, None).unwrap();
            assert!(
                ui.set_numbers(view, Property::Sequence, &[player as f64]).is_err(),
                "not a sequence"
            );
            assert!(
                ui.set_numbers(view, Property::Player, &[sequence as f64]).is_err(),
                "not a player"
            );
            ui.set_numbers(view, Property::Sequence, &[sequence as f64]).unwrap();
            ui.set_numbers(view, Property::Player, &[player as f64]).unwrap();
            assert_eq!(ui.numbers(view, Property::Player).unwrap(), vec![player as f64]);
        }
        let label = ui.create(Kind::Label, None).unwrap();
        assert!(
            ui.set_numbers(label, Property::Sequence, &[sequence as f64]).is_err(),
            "a label shows none"
        );
        assert!(ui.set_numbers(player, Property::Player, &[player as f64]).is_err());
        let layout = ui.create(Kind::VBoxLayout, None).unwrap();
        let view = ui.create(Kind::DopesheetView, None).unwrap();
        ui.add_to(layout, view, [0, 0, 1, 1]).unwrap();
        ui.set_numbers(view, Property::Player, &[player as f64]).unwrap();
        ui.destroy(player).unwrap();
        assert_eq!(
            ui.numbers(view, Property::Player).unwrap(),
            vec![0.0],
            "its player gone"
        );
    }

    #[test]
    fn a_change_of_keys_under_way_still_waiting_takes_the_next_one() {
        let (shared, jobs) = queued();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut store = lock(&shared);
        let view = store.create(Kind::DopesheetView, None).unwrap();
        let kept = seen.clone();
        store
            .connect(
                view,
                Signal::KeysChanged,
                Arc::new(move |data: &SignalData| kept.lock().unwrap().push((data.text.clone(), data.boolean))),
            )
            .unwrap();
        let change = |text: &str, done| SignalData {
            sender: view,
            signal: Signal::KeysChanged as u32,
            text: text.to_owned(),
            boolean: done,
            ..Default::default()
        };
        store.emit(change("a", false));
        store.emit(change("b", false));
        store.emit(change("c", true));
        drop(store);
        run(&jobs);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![("b".to_owned(), false), ("c".to_owned(), true)]
        );
    }

    #[test]
    fn a_current_item_is_one_the_view_holds_and_goes_with_it() {
        let shared = ui();
        let mut ui = lock(&shared);
        let tree = ui.create(Kind::TreeView, None).unwrap();
        ui.set_text(
            tree,
            Property::Items,
            r#"[{"id":1,"text":"a","children":[{"id":2,"text":"b"}]}]"#,
        )
        .unwrap();
        ui.set_numbers(tree, Property::CurrentItem, &[2.0]).unwrap();
        assert!(
            ui.set_numbers(tree, Property::CurrentItem, &[9.0]).is_err(),
            "an item it does not hold"
        );
        assert_eq!(ui.numbers(tree, Property::CurrentItem).unwrap(), vec![2.0]);
        ui.set_text(tree, Property::Items, r#"[{"id":1,"text":"a"}]"#).unwrap();
        assert_eq!(
            ui.numbers(tree, Property::CurrentItem).unwrap(),
            vec![0.0],
            "its item gone"
        );
        assert!(ui.text(tree, Property::Items).unwrap().contains(r#""text":"a""#));

        let table = ui.create(Kind::TableView, None).unwrap();
        ui.set_text(table, Property::Columns, r#"["a","b"]"#).unwrap();
        ui.set_text(
            table,
            Property::Rows,
            r#"[{"id":1,"cells":["x","1"]},{"id":2,"cells":["y","2"]}]"#,
        )
        .unwrap();
        ui.set_numbers(table, Property::CurrentItem, &[2.0]).unwrap();
        ui.remove_rows(table, &[2]).unwrap();
        assert_eq!(
            ui.numbers(table, Property::CurrentItem).unwrap(),
            vec![0.0],
            "its row gone"
        );
        assert!(
            ui.set_numbers(tree, Property::SortColumn, &[0.0]).is_err(),
            "a tree is not sorted"
        );
    }

    #[test]
    fn a_large_table_view_is_sorted_off_the_lock_once_drawn() {
        let shared = ui();
        let count = super::data::BACKGROUND_SORT_ROWS as u64;
        let table = {
            let mut ui = lock(&shared);
            let table = ui.create(Kind::TableView, None).unwrap();
            ui.set_text(table, Property::Columns, r#"["value"]"#).unwrap();
            let many = (1..=count)
                .map(|id| super::Row {
                    id,
                    cells: vec![(count - id).to_string()],
                })
                .collect();
            ui.set_rows(table, super::Rows::new(many).unwrap()).unwrap();
            ui.set_numbers(table, Property::SortColumn, &[0.0]).unwrap();
            assert_eq!(
                ui.numbers(table, Property::SortColumn).unwrap(),
                vec![0.0],
                "the sort asked"
            );
            assert!(ui.table(table).unwrap().is_sorting());
            ui.start_sort(table);
            table
        };
        let started = std::time::Instant::now();
        while lock(&shared).table(table).unwrap().is_sorting() {
            assert!(started.elapsed() < std::time::Duration::from_secs(10), "never sorted");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let ui = lock(&shared);
        let sorted = ui.table(table).unwrap();
        assert_eq!(sorted.shown_sort(), Some((0, false)));
        assert_eq!(sorted.shown(0).unwrap().id, count, "the lowest first");
    }

    #[test]
    fn a_tree_view_s_version_changes_with_its_items_and_their_folding() {
        let shared = ui();
        let mut ui = lock(&shared);
        let tree = ui.create(Kind::TreeView, None).unwrap();
        let version = |ui: &Ui| ui.object(tree).unwrap().tree_version;
        ui.set_text(
            tree,
            Property::Items,
            r#"[{"id":1,"text":"a","children":[{"id":2,"text":"b"}]}]"#,
        )
        .unwrap();
        let set = version(&ui);
        assert_eq!(ui.set_item_expanded(tree, 1, true), Ok(true));
        assert!(ui.items(tree).unwrap()[0].expanded);
        assert!(version(&ui) > set);
        let unfolded = version(&ui);
        assert_eq!(
            ui.set_item_expanded(tree, 9, true),
            Ok(false),
            "an item it does not hold"
        );
        assert_eq!(version(&ui), unfolded);
    }

    #[test]
    fn a_table_is_sorted_by_a_column_from_the_lowest_or_the_highest() {
        let shared = ui();
        let mut ui = lock(&shared);
        let table = ui.create(Kind::TableView, None).unwrap();
        ui.set_text(table, Property::Columns, r#"["a","b"]"#).unwrap();
        let sort = |ui: &Ui| {
            (
                ui.numbers(table, Property::SortColumn).unwrap()[0],
                ui.numbers(table, Property::SortDescending).unwrap()[0],
            )
        };
        assert_eq!(sort(&ui), (-1.0, 0.0), "the module's order");
        ui.set_numbers(table, Property::SortDescending, &[1.0]).unwrap();
        assert_eq!(sort(&ui), (-1.0, 0.0), "no order without a column");
        ui.set_numbers(table, Property::SortColumn, &[1.0]).unwrap();
        ui.set_numbers(table, Property::SortDescending, &[1.0]).unwrap();
        assert_eq!(sort(&ui), (1.0, 1.0));
        ui.set_numbers(table, Property::SortColumn, &[0.0]).unwrap();
        assert_eq!(sort(&ui), (0.0, 1.0), "another column keeps the direction");
        assert!(ui.set_numbers(table, Property::SortColumn, &[0.5]).is_err());
        ui.set_text(table, Property::Columns, r#"["a"]"#).unwrap();
        ui.set_numbers(table, Property::SortColumn, &[1.0]).unwrap();
        assert_eq!(sort(&ui), (-1.0, 0.0), "a column it does not have");
        ui.set_numbers(table, Property::SortColumn, &[0.0]).unwrap();
        ui.set_numbers(table, Property::SortColumn, &[-1.0]).unwrap();
        assert_eq!(sort(&ui), (-1.0, 0.0));
    }

    #[test]
    fn a_sequence_created_with_its_content_records_nothing() {
        let shared = ui();
        let mut ui = lock(&shared);
        let recorded = Arc::new(Mutex::new(0));
        let count = recorded.clone();
        ui.set_recorder(Arc::new(move |_, _| {
            *count.lock().unwrap() += 1;
            Ok(())
        }));
        let mut sequence = crate::sequence::Sequence {
            tracks: super::read_tracks(&tracks(0.5)).unwrap(),
            ..Default::default()
        };
        let handle = ui.create_sequence(sequence.clone()).unwrap();
        assert_eq!(*ui.sequence(handle).unwrap(), sequence);
        assert_eq!(*recorded.lock().unwrap(), 0);
        sequence.tracks[0].curves[0].keys[0].time = 2.5;
        assert!(ui.create_sequence(sequence).is_err(), "a key between frames");
    }
}
