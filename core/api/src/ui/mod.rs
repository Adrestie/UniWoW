//! Interface objects modelled on Qt (section 3): widgets, layouts, a graphics scene and painting
//! areas. A module that is not written in Rust creates them through handles and changes them from
//! any thread; the core keeps them, draws them on the interface thread, and sends their signals to
//! the module's own thread. The object model is defined once here, for every language.

mod draw;
mod painter;
mod scene;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

pub use draw::PanelView;
pub use painter::PaintCommand;

use crate::curve::ShownCurve;
use crate::egui;

/// Identifies an object of one module.
pub type Handle = u64;

/// Reads the curves of a curve view from their JSON text (`ShownCurve::list_from_json`).
pub fn read_curves(text: &str) -> Result<Vec<ShownCurve>, String> {
    let value = serde_json::from_str(text).map_err(|error| format!("the curves are not JSON: {error}"))?;
    ShownCurve::list_from_json(&value)
}

/// The highest row, column or span of a grid layout: a wrapped negative number would otherwise
/// ask for billions of cells.
pub const MAX_CELL: u32 = 10_000;

/// Runs a job on the module's own thread.
pub type Post = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

/// A function connected to a signal; it runs on the module's own thread.
pub type Slot = Arc<dyn Fn(&SignalData) + Send + Sync>;

/// The objects of one module, shared between its threads and the interface thread.
pub type SharedUi = Arc<Mutex<Ui>>;

macro_rules! numbered {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $value:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u32)]
        pub enum $name { $($variant = $value),* }

        impl $name {
            pub fn from_u32(value: u32) -> Option<Self> {
                match value {
                    $($value => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

numbered!(
    /// The kinds of objects, named as in Qt. The numbers are those of `uniwow.h`.
    Kind {
        Panel = 1, Label = 2, PushButton = 3, CheckBox = 4, Slider = 5, SpinBox = 6, LineEdit = 7,
        ComboBox = 8, Separator = 9, GroupBox = 10, VBoxLayout = 11, HBoxLayout = 12,
        GridLayout = 13, GraphicsView = 14, GraphicsScene = 15, RectItem = 16, LineItem = 17,
        EllipseItem = 18, TextItem = 19, ItemGroup = 20, PaintArea = 21, Dialog = 22, CurveView = 23,
    }
);

numbered!(
    /// The properties of the objects. The numbers are those of `uniwow.h`.
    Property {
        Text = 1, ToolTip = 2, Enabled = 3, Visible = 4, Checked = 5, Value = 6, Minimum = 7,
        Maximum = 8, Step = 9, Decimals = 10, Placeholder = 11, CurrentIndex = 12, Title = 13,
        Pos = 14, Rect = 15, Line = 16, PenColor = 17, PenWidth = 18, BrushColor = 19,
        Radius = 20, ZValue = 21, Movable = 22, Selectable = 23, Selected = 24, MoveBounds = 25,
        FontSize = 26, MinimumHeight = 27, ViewScale = 28, ViewCenter = 29, Count = 30, Curves = 31,
    }
);

numbered!(
    /// The signals, named as in Qt. Each tells what the user did, never a change the module
    /// made itself. The numbers are those of `uniwow.h`.
    Signal {
        Clicked = 1, Toggled = 2, ValueChanged = 3, SliderPressed = 4, SliderReleased = 5,
        TextChanged = 6, EditingFinished = 7, CurrentIndexChanged = 8, ItemPressed = 9,
        ItemMoved = 10, ItemDoubleClicked = 11, SelectionChanged = 12, Paint = 13,
        MousePress = 14, MouseMove = 15, MouseRelease = 16, Wheel = 17, Rejected = 18, CurvesChanged = 19,
    }
);

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
        !self.is_item() && !matches!(self, Kind::Panel | Kind::GraphicsScene | Kind::Dialog)
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
            minimum_height: if matches!(kind, Kind::GraphicsView | Kind::PaintArea | Kind::CurveView) {
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
        }
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
    /// Changed at each change of a scene's set of items or their order, by scene.
    structure: HashMap<Handle, u64>,
    post: Post,
    wake: Option<egui::Context>,
}

/// Locks the objects of a module, even after a panic in another thread.
pub fn lock(ui: &SharedUi) -> MutexGuard<'_, Ui> {
    ui.lock().unwrap_or_else(|e| e.into_inner())
}

impl Ui {
    /// The objects of a module, whose signals run through `post`.
    pub fn new(post: Post) -> SharedUi {
        let ui = Ui {
            objects: HashMap::new(),
            next: 1,
            panels: Vec::new(),
            connections: Vec::new(),
            next_connection: 1,
            painting: HashMap::new(),
            structure: HashMap::new(),
            post,
            wake: None,
        };
        Arc::new(Mutex::new(ui))
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

    pub(crate) fn object_mut(&mut self, handle: Handle) -> Option<&mut Object> {
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

    pub(crate) fn set_wake(&mut self, wake: &egui::Context) {
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
        }
        // Views showing a destroyed scene show nothing.
        let alive: Vec<Handle> = self.objects.keys().copied().collect();
        for object in self.objects.values_mut() {
            if object.scene.is_some_and(|scene| !alive.contains(&scene)) {
                object.scene = None;
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

    pub fn text(&self, handle: Handle, property: Property) -> Result<String, String> {
        let object = self.get(handle)?;
        Ok(match property {
            Property::Text => object.text.clone(),
            Property::ToolTip => object.tooltip.clone(),
            Property::Placeholder => object.placeholder.clone(),
            Property::Title => object.title.clone(),
            Property::Curves => ShownCurve::list_to_json(&object.curves).to_string(),
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
        let object = self.get(handle)?;
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

    /// Sends a signal to the slots connected to it, on the module's thread.
    pub(crate) fn emit(&self, data: SignalData) {
        let slots: Vec<Slot> = self
            .connections
            .iter()
            .filter(|c| c.sender == data.sender && c.signal as u32 == data.signal)
            .map(|c| c.slot.clone())
            .collect();
        if slots.is_empty() {
            return;
        }
        (self.post)(Box::new(move || {
            for slot in slots {
                slot(&data);
            }
        }));
    }

    /// Asks the module to paint an area of `size`; the picture replaces the area's once painted.
    pub(crate) fn request_paint(&mut self, shared: &SharedUi, area: Handle, size: [f64; 2]) {
        let slots: Vec<Slot> = self
            .connections
            .iter()
            .filter(|c| c.sender == area && c.signal == Signal::Paint)
            .map(|c| c.slot.clone())
            .collect();
        if slots.is_empty() {
            return;
        }
        let painter = self.next;
        self.next += 1;
        self.painting.insert(painter, (area, Vec::new()));
        let shared = shared.clone();
        (self.post)(Box::new(move || {
            let data = SignalData {
                sender: area,
                signal: Signal::Paint as u32,
                width: size[0],
                height: size[1],
                painter,
                ..Default::default()
            };
            for slot in slots {
                slot(&data);
            }
            let mut ui = lock(&shared);
            if let Some((area, commands)) = ui.painting.remove(&painter)
                && let Some(object) = ui.objects.get_mut(&area)
            {
                object.picture = Arc::new(commands);
            }
            ui.wake();
        }));
    }

    /// The scene an item belongs to, through its groups.
    pub(crate) fn scene_of(&self, mut handle: Handle) -> Option<Handle> {
        loop {
            let object = self.objects.get(&handle)?;
            if object.kind == Kind::GraphicsScene {
                return Some(handle);
            }
            handle = object.parent?;
        }
    }

    /// The version of a scene's set of items and their order.
    pub(crate) fn structure(&self, scene: Handle) -> u64 {
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
    pub(crate) fn moved(&mut self, handle: Handle) {
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

    use super::{Kind, PanelView, Property, Signal, SignalData, Ui, lock};
    use crate::curve::{CurveChange, CurveEditor, CurveOptions, ShownCurve, TimeAxis};
    use crate::egui;

    /// A Ui whose posted jobs run at once on the calling thread.
    fn ui() -> super::SharedUi {
        Ui::new(Arc::new(|job| job()))
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
        let shared = ui();
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
        ui.disconnect(connection);
        ui.emit(click(first));
        assert_eq!(*received.lock().unwrap(), vec![first]);
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

    struct Broken;

    impl CurveEditor for Broken {
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _id: egui::Id,
            _curves: &mut [ShownCurve],
            _time: &mut TimeAxis,
            _options: &CurveOptions,
        ) -> CurveChange {
            panic!("broken editor")
        }
    }

    #[test]
    fn a_panic_of_the_curve_editor_is_kept_for_its_provider_to_be_reported() {
        let shared = ui();
        {
            let mut store = lock(&shared);
            let panel = store.panel("p");
            let layout = store.create(Kind::VBoxLayout, None).unwrap();
            store.add_to(panel, layout, [0, 0, 1, 1]).unwrap();
            let view = store.create(Kind::CurveView, None).unwrap();
            store.add_to(layout, view, [0, 0, 1, 1]).unwrap();
        }
        let mut panels = PanelView::default();
        panels.set_curve_editor(Some(Arc::new(Broken)));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| panels.show(&shared, "p", ui, None));
        output.textures_delta.clear();
        assert!(
            panels
                .take_editor_failure()
                .is_some_and(|m| m.contains("broken editor"))
        );
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
}
