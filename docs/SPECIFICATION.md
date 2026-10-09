# UniWoW — Architecture and module catalogue

Status: **validated**. Milestones 1 to 7 built and validated; milestone 8 built, its acceptance on the user's machine to come; milestone 9 validated; milestones 10 to 14 outlined. Open questions in section 10.

UniWoW is a standalone desktop application (outside the game client) used to modify a
WoW 3.3.5a (build 12340) client and an AzerothCore server: maps, data, assets, interface,
scripts, packaging. Written in Rust, user interface with egui, 3D rendering with wgpu.

Players are expected to run the client with WarcraftXL (WXL).

**Vocabulary.** The **core** is what UniWoW cannot work without: the executable, the runtime and
the kernel (loading, undo and redo, saving, the interface, the log, the commands, the threads).
Everything else is a **module**: one functionality, such as the 3D view or a timeline. A module is
written in Rust in this project, or compiled by its author as a DLL from C, C++, C# or any language
able to export a C function, or written in Lua or Python. A **script** is a one-off task in Lua or
Python, run from the panel of its language, whose **console** also runs commands on the fly.
Milestones 1 to 3 (section 9) were written with earlier words: a *feature* was a Rust module, a
*native module* a compiled module.

---

## 1. Modularity requirements

| Id | Requirement |
|---|---|
| R1 | Every module is separate from the core. The core contains no code specific to any module and never names one. |
| R2 | Every module has its own folder and manifest. The core detects the modules present at start and loads them: Rust and compiled modules are DLLs, Lua and Python modules are source files run by the module of their language. |
| R3 | A module can be added, rebuilt or removed without recompiling the editor. |
| R4 | Removing a module's folder leaves the editor starting and working without it. Example: without the viewport module, the editor starts without any 3D window; without the terrain module, the 3D window shows no terrain. |
| R5 | Adding a module is simple: one command creates it, one command builds it. |
| R6 | A module that fails, or that was built for another version of the core, is refused or disabled and reported; the editor keeps running. |
| R7 | Speed: the editor uses every processor core. Long or heavy work (reading archives, parsing files, building meshes, extraction, database queries) never runs on the interface thread. |
| R8 | Modules and scripts may use threads themselves, through what the kernel offers (section 5, threads). |
| R9 | Parity: whatever a built-in module can do, a module in any language can do through the unified API (S1). Built-in modules may keep egui for their own windows, but every capability and every widget they offer exists in the unified API: an engine, an object of the interface, a property, a command. New built-in modules are designed that way from the start. |

---

## 2. Layers

```
app            small executable: loads the runtime, then the modules
core/api       contracts offered to modules (traits, extension points, entry point macro)
core/kernel    module loader and core services (section 5)
libs/*         shared libraries without user interface (section 6)
modules/*      one crate per Rust module, built as one DLL (section 7)
examples/*     sample modules in C++, C#, Lua and Python, built and installed as an author would
client-bridge  C++ WXL module running inside the client (outside the Cargo workspace)
xtask          developer commands: new-module, build, build-module, run, check, test-sdk, bindings
```

Crates are prefixed `uniwow-` (`uniwow-api`, `uniwow-kernel`, `uniwow-module-<id>`…).

`core/api`, `libs/*` and the shared dependencies (egui, wgpu…) are built into one shared DLL,
`uniwow_api.dll`; the Rust standard library is shared as `std-<hash>.dll`. Together they are the
**runtime**. Every Rust module links to the runtime, so each of these exists once in memory.
`core/kernel` is linked into the executable: modules never link to it, so changing the kernel
does not affect them. The runtime keeps the contracts shared with modules (the model of the
interface objects, curves, properties); the C interface of compiled modules and the drawing of the
interface objects are in the kernel, which draws a module's objects through
`Context::draw_objects` and `Context::draw_dialogs`.

Allowed dependencies (enforced by `xtask check`):

| Layer | May depend on |
|---|---|
| app | core/api, core/kernel |
| modules/* | core/api, libs/* |
| core/kernel | core/api, libs/* |
| core/api | libs/* (types only) |
| libs/* | other libs/* |

A module never depends on another module, nor on `core/kernel`. A library never depends on
the core or on a module.

A Rust module depends on nothing else, not even through dev or build dependencies: enabling an option
of a crate the runtime also uses makes Cargo rebuild the runtime, which changes its fingerprint and
makes every module incompatible. A crate a module needs is added to the runtime (`core/api` or
`libs/*`). `cargo xtask check` names any other dependency.

Every crate of `libs/*` is a normal dependency of `uniwow-api`, so that it is compiled once, into
the runtime: a module using it receives it through `uniwow_api.dll`, without its own copy of the
code or of its global state, and the library's own dependencies are those of the runtime.
`cargo xtask check` refuses a library that `uniwow-api` does not include, whether a module uses
it or not.

No crate sets a global allocator (`#[global_allocator]`): the Rust standard library, shared as a
DLL with the runtime, keeps the allocator of Windows, and what one side allocated, the other would
free. Tried in step 9.1 with mimalloc in the runtime: every program crashed as it started.

---

## 3. Module contract

Every module has a folder `modules\<id>\` beside the executable, with a manifest `module.toml`:
id, name, version, category, description, kind, the services it requires or uses, and what its
kind needs (its DLL and the DLL's hash, the runtime fingerprint, its entry file). The manifest of a
Rust module also names its Cargo package, which the loader checks against the DLL, and the one
`cargo xtask build` writes says `origin = "workspace"`: the next build removes such a folder once
its source is gone, and leaves the folders of other origins alone.

| Kind | Written as | Loaded by | Panels |
|---|---|---|---|
| Rust | A crate of this project, in `modules/<id>/` | The kernel, against the runtime fingerprint | Drawn with egui |
| Compiled | A DLL exporting the C entry point of `uniwow.h` (S10), from C, C++, C# compiled with NativeAOT, or any language able to export a C function; added by its author | The kernel, through the C interface | Interface objects |
| Lua, Python | Source files, loaded at start, keeping their state while the editor runs | The module of their language (`scripting-lua`, `scripting-python`), which hosts them | Interface objects |

Every kind offers named commands, publishes and receives events, has settings, records undoable
changes and has panels. The panels of a module not written in Rust are made of **interface
objects** modelled on Qt: widgets, layouts, a graphics scene and painting areas, created through
handles, changed property by property from any thread, their signals connected to the module's
functions (milestone 5). The core keeps the objects and draws them; signals reach the module on a
thread of its own: the interface thread never waits for a module's code (T1, T6).

Rules F1 to F6 apply to every kind. The rest of this section is the contract of Rust modules;
section 7 gives that of the others (S1 to S10).

### Rust modules

A Rust module is one crate in `modules/<id>/` (crate type `cdylib`), deployed as `<id>.dll`.

What describes it is declared in its `Cargo.toml`, not in code: id, display name, category, and
the services it **requires** (cannot work without) or **uses** (when present) under
`[package.metadata.uniwow]`; version and description under `[package]`. `cargo xtask build`
copies them into `module.toml`, which the kernel reads before loading the DLL.

Its behaviour is one type implementing the `Module` trait of `core/api`, exported by an entry
point macro:

- **register**: declares what the module contributes (list below). Called once at load.
- **init / shutdown**: start and stop, with access to core services.
- **panel_ui, on_event, on_menu**: draw its panels, receive its events, handle its menu items.

Extension points a module may contribute to:

| Extension point | Example |
|---|---|
| Panels (dockable windows) | DBC table view, asset browser, 3D view |
| Menu entries and shortcuts | Map > New map |
| Named commands: called by name with JSON arguments and a JSON result, each declared with a description and the schema of its arguments and result. Not to be confused with the undoable `Command` of the history | "open creature <id>", "paint texture" |
| Inspectors, per selection type | creature inspector, doodad inspector |
| Asset handlers, per file type | open or preview `.blp`, `.m2` |
| A category of the window *Settings*, declared by the module | the distances of the view |
| Services implementing an interface defined in core/api, under a typed `ServiceKey<T>` declared beside the interface, used both to provide and to ask, so that a type mismatch does not compile | "viewport", "creature lookup" |
| Viewport layers, through the viewport service | terrain, cube; each layer records into its own render bundle, with pipelines matching the view's `Target`: its colour and depth formats, its sample count and its depth comparison (reverse Z: nearer is greater) |
| Event subscriptions | "project saved", "tile changed" |
| Project data section owned by the module | spawn edits not yet deployed |

Rules:

| Id | Rule |
|---|---|
| F1 | A module talks to another only through the core: commands, events, services by interface. Example: the quest editor issues "open creature 1234"; the module registered for creatures handles it. If no such module is loaded, the link is shown disabled. |
| F2 | Every modification goes through an undoable command (single undo history). |
| F3 | A module owns its project data section and its settings; no other module reads them directly. |
| F4 | A missing required service: the module is not loaded and the reason is shown. A missing used service: the module loads without the parts that need it. A module that fails withdraws its services; requirements are checked again just before each `init`, so a module whose provider failed meanwhile is not initialised. |
| F5 | A module that runs code on behalf of another, such as the viewport drawing a layer, catches its failures and reports the culprit with `Context::report_failure`. The kernel disables the culprit as if it had panicked, naming the reporter. |
| F6 | Every action a module offers to others is a named command. The kernel keeps their catalogue and routes the calls; the same catalogue serves every module and script (S1). A Rust module declares its commands itself; those of compiled, Lua and Python modules are declared on their behalf, by the kernel or by the module of their language: they are delegated. Once every module has registered, a name declared twice keeps the command declared directly over a delegated one, and the first registered between two of the same kind; each one set aside is logged with the one that wins. The choice is made among the modules running: the catalogue is made again whenever a module is blocked or fails, so that a command set aside comes back once the one that won stops, which the log tells; a command no running module offers is refused with the reason, its module not running. |

### Interface objects

The panels of compiled, Lua and Python modules, written once for every language. In C they are the
typed functions of `uniwow.h`, which numbers the objects, properties and signals below; C++ has the
classes of `sdk/uniwow.hpp` (`uniwow::PushButton`, `button.clicked.connect(...)`), C# those of
`sdk/UniWoW.cs` with the names of C# (`button.Clicked.Connect(...)`, `UiObject` for `QObject`); Lua
and Python receive theirs in milestones 10 and 11.

| Object | As in Qt | Properties | Signals |
|---|---|---|---|
| `Panel` | a dock widget | the one layout it holds | |
| `Label` | `QLabel` | text | |
| `PushButton` | `QPushButton` | text | `clicked` |
| `CheckBox` | `QCheckBox` | text, checked | `toggled` (checked) |
| `Slider` | `QSlider` | value, minimum, maximum, step | `valueChanged`, `sliderPressed`, `sliderReleased` (value) |
| `SpinBox` | `QDoubleSpinBox` | value, minimum, maximum, step, decimals | `valueChanged`, `editingFinished` (value) |
| `LineEdit` | `QLineEdit` | text, placeholder | `textChanged`, `editingFinished` (text) |
| `ComboBox` | `QComboBox` | entries, current index, count | `currentIndexChanged` (index) |
| `Separator` | a line | | |
| `GroupBox` | `QGroupBox` | title, the one layout it holds | |
| `VBoxLayout`, `HBoxLayout`, `GridLayout` | the same | children; row, column and spans in a grid | |
| `GraphicsView` | `QGraphicsView` | scene, zoom, centre, minimum height | |
| `GraphicsScene` | `QGraphicsScene` | items | `itemPressed`, `itemMoved`, `itemDoubleClicked` (item, x, y, dx, dy, button, keys), `selectionChanged` |
| `RectItem`, `EllipseItem` | `QGraphicsRectItem`, `QGraphicsEllipseItem` | rectangle; corner radius of a rectangle | |
| `LineItem` | `QGraphicsLineItem` | line | |
| `TextItem` | `QGraphicsSimpleTextItem` | text, font size, colour | |
| `ItemGroup` | `QGraphicsItemGroup` | items | |
| `PaintArea` | a `QWidget` and its `paintEvent` | minimum height | `paint` (painter, width, height), `mousePress`, `mouseMove`, `mouseRelease`, `wheel` (x, y, dx, dy, button, keys) |
| `Dialog` | `QDialog` | title, the one layout it holds, shown or hidden | `rejected` |
| `CurveView` | a `QWidget` drawn by the module `curves` | curves (JSON), minimum height; or a sequence and a player | `curvesChanged` (curves, finished); `keysChanged` (tracks, finished) when it shows a sequence |
| `DopesheetView` | a `QWidget` drawn by the module `dopesheet` | sequence, player, title, minimum height | `keysChanged` (tracks, finished), `playheadMoved` (frame) |
| `TreeView` | `QTreeWidget` | items (JSON), current item, minimum height | `itemClicked`, `currentItemChanged` (item), `itemExpanded` (item, unfolded) |
| `TableView` | `QTableWidget` | columns, rows (JSON), current row, sort column and direction, minimum height | `cellChanged` (row, column, text), `currentCellChanged` (row, column), `sortChanged` (column, from the highest) |
| `PropertyGrid` | a `QWidget` drawn by the module `properties`, as the Inspector of Unity | paths of the properties shown (JSON), minimum height | |
| `Sequence` | the data a `QTimeLine` plays; not drawn | tracks (JSON), frame rate, length | |
| `Player` | `QTimeLine`; not drawn | sequence, time, playing, loop, speed | `timeChanged` (time), `finished` |

Every widget is enabled or not, visible or not, and has a tooltip. Every item has a position in its
parent, a pen, a brush, a stacking order, a tooltip, and is visible, movable (along x, y or both,
within bounds), selectable and selected or not. Colours are `0xRRGGBBAA`, sizes are in points. A
range changed holds the value in it, as in Qt.

- **Handles**: an object is a number, 0 being none. A module creates its objects, except its
  panels, which it declares when it starts and finds by their id; destroying an object destroys
  its children. Every function can be called from any thread, and a change shows at the next frame.
- **Signals** tell what the user did, never a change the module made. A widget shows what the user
  did at once, then tells the module. The functions connected to a signal run in order on the
  module's own thread, never on the interface thread; one that takes long only delays the next.
- **Layouts**: in a box layout, a graphics view, a painting area, or a layout or group box holding
  one, share the room the other children leave; the others take their own size.
- **Graphics scene**: an item's position is relative to its parent. A movable item follows the
  pointer without waiting for the module, which learns where it was dropped and how far it moved.
  Items are drawn by stacking order, then in the order they were created; texts above the shapes.
  The view scrolls with the middle or right button and zooms with the wheel, outside the history.
  The tooltip of the item under the pointer shows once the pointer rests on it; an item without
  one shows that of its nearest group that has one.
- **Painting**: a painting area asks its module to paint when it is shown, resized, or after
  `update()`. The module paints in points from the top left corner of the area, a text placed by its
  top left corner; the picture stays until the next painting. As in Qt, a painting asked while no
  function is connected to `paint` is not kept: a module connecting it once the area is shown calls
  `update()`.
- **Dialogs**: a dialog is a floating window, created hidden. While it is shown, the rest of the
  editor takes neither clicks nor shortcuts. The user closing it, with Escape or its close button,
  hides it and sends `rejected`.
- **Sequences and players** (milestone 8): a sequence holds tracks of keys on animatable
  properties, with a frame rate and a length, in the JSON of the Timeline's files. While a player
  plays, the kernel moves it on at each frame by the time elapsed, times its speed; at the end it
  stops and sends `finished`, or starts again with *loop*. Whenever its time or its sequence
  changes, the kernel writes the value of each track at that time into its property, through the
  catalogue of animatable properties; when only its keys change, only the values that changed. The dopesheet views and the
  curve views showing the same sequence with the same player share their time axis, zoom and
  scrolling; a sequence no view shows with that player any more is forgotten, and fitted again as
  when first shown once a view shows it again.
- **Undo**: a module records a change it has made with a label and two JSON values, one undoing it
  and one redoing it. Undo and Redo hand the matching value to the function the module declared,
  on its thread; when that function fails, the module fails and its changes leave the history.
  While a compiled module's thread still has signals, changes or commands to handle, Undo and
  Redo are refused with the reason; the writes of its properties and the `timeChanged` of its
  players, which record nothing, do not count. One job running for more than 3 seconds shows the module as not responding in the Modules
  panel (a write only when other work waits behind it), where it can be disabled: its changes leave the history and
  Undo comes back. The Commands panel never waits for an answer: another call replaces the one
  awaited, or the wait is given up.
- **Undo of the state the kernel keeps** (milestone 8): for the objects whose state the kernel
  keeps, it knows the value before and after a change, and records the undo entry itself: keys
  moved in a `DopesheetView` or a `CurveView` showing a `Sequence`, a value changed in a
  `PropertyGrid`, the `TRACKS` of a `Sequence` set by its module, the value of a property of a
  module of another language changed by hand. The entry belongs to the module owning the object or
  the property. A change the module makes, such as its `TRACKS`, joins the group the module has
  open on the thread making it (S4); a change made by hand in a view is recorded from the interface
  thread, where the module has no group open, and is an entry of its own, as the user's gesture.
  The title of a `Sequence` names the document its changes belong to: `Context::forget_document`
  forgets them with the module's own commands. The author of a tool in any language has nothing to
  write for these; `record_change` stays for the module's own data. Rust modules using these
  objects hand them to the kernel with `Context::adopt_objects` and get the same.

### Capabilities and their unified form

What a built-in module can do, and how a module in any language does the same (R9). Every
milestone keeps this table up to date, and its review checks it. A dash is a gap: the step that
fills it, or *not planned* when no milestone does yet.

| Capability | Built-in (Rust) modules | Unified API (`uniwow.h`) | C++ and C# | Lua and Python |
|---|---|---|---|---|
| Named commands: offer, list, call | `Registrar::command`, `command_on_caller`; `Context::call`, `Editor::call` | `uniwow_module_info.commands`; `commands`, `call` | `uniwow::call`; `Editor.Call`, `Command` | Scripts: `uniwow.commands`, `uniwow.call`; modules: milestones 10 and 11 |
| Events | `Registrar::subscribe`, `Module::on_event`, `Context::publish_as`; `Editor::subscribe`, `next_event` | `publish`, `subscribe`, `next_event`, `unsubscribe` | The table (`uniwow::api()`, `Editor.Table`) | Scripts: `uniwow.publish`, `subscribe`, `next_event`, `unsubscribe` |
| Settings | `Context::setting`, `set_setting` | `setting`, `set_setting` | The table | Scripts: `uniwow.setting`, `set_setting` |
| A category of the window *Settings* | `Registrar::settings`, `SettingSpec` | — not planned yet | — not planned yet | — milestones 10 and 11 |
| Log | The `log` crate | `log` | `uniwow::log`; `Editor.Log` | Scripts: `uniwow.log`, `print` |
| Undo of the module's own data | `Command`, `Context::execute` | `record_change`, with `uniwow_module_info.apply_change` | `uniwow::recordChange`; `Editor.RecordChange` | Milestones 10 and 11 |
| Undo groups | The commands of one call | `begin_group`, `end_group` | The table; `Editor.BeginGroup`, `EndGroup` | Scripts: `uniwow.begin_group`, `end_group` |
| Panels | `Registrar::panel`, `Module::panel_ui` (egui) | `uniwow_module_info.panels`, `panel`, the objects | `uniwow::Panel`; `Panel` | Milestones 10 and 11 |
| Widgets, layouts, a scene, painting | egui | The objects of this section | The classes named as in Qt | Milestones 10 and 11 |
| Modal windows | The command `ui.dialog`; the object `Dialog` | `ui.dialog` through `call`; `Dialog` | `uniwow::Dialog`; `Dialog` | Scripts: `ui.dialog` through `uniwow.call` |
| The curve editor | The service `curve-editor` | `CurveView` | `uniwow::CurveView`; `CurveView` | Milestones 10 and 11 |
| Animatable properties: declare, list, read, write | `Registrar::animatable`; `Editor::properties`, `read_property`, `write_property` | `uniwow_module_info.properties` (`uniwow_property`); `properties`, `read_property`, `write_property`, `set_property` | `uniwow::Property`, `describeProperties`, `properties`, `readProperty`, `writeProperty`; `Editor.DeclareProperty`, `Properties`, `ReadProperty`, `WriteProperty`, `SetProperty` | — milestones 10 and 11 |
| The viewport's camera | Inside the module `viewport` (*View*, *Reset camera*) | The properties `viewport/camera_position`, `camera_target`, `camera_fov`; the commands `viewport.camera`, `viewport.look_at`, `viewport.frame` | The functions of properties; `call` | Scripts: the commands through `uniwow.call`; the properties: milestones 10 and 11 |
| Sequences and their playback | `uniwow_api::sequence`; the objects `Sequence` and `Player` handed to the kernel with `Context::adopt_objects`, as the Timeline does | `Sequence`, `Player` | `uniwow::Sequence`, `uniwow::Player`; `Sequence`, `Player` | — milestones 10 and 11 |
| The dopesheet | The service `dopesheet`; the objects `DopesheetView` and `CurveView`, as the Timeline does | `DopesheetView`; `CurveView` showing a `Sequence` | `uniwow::DopesheetView`; `DopesheetView` | — milestones 10 and 11 |
| Tree, table, property grid | egui; the service `property-grid` | `TreeView`, `TableView`, with `set_cell`, `insert_rows`, `remove_rows`; `PropertyGrid` | `uniwow::TreeView`, `uniwow::TableView`, `uniwow::PropertyGrid`; `TreeView`, `TableView`, `PropertyGrid` | — milestones 10 and 11 |
| Drawing in the 3D view | The service `viewport`: its layers (`prepare`, a version to keep their bundle, their statistics with `stats`, the texts they write over the view with `labels`) and its frame signal (`wait_frame`) | — not planned (an other 3D access of step 8.3) | — | — |
| The client's files, read from its archives, and their formats | The services `vfs` and `formats` of the module `assets` — step 9.1 | — not planned yet | — | — |
| The live world: entities of the server around a point, their moves | The commands `live-world.state` and `live-world.entities`, the event `live-world.changed` (step 9.3b); the client of the observer, `uniwow_api::server_link` | The same commands and event, through `call` and `subscribe` | The same, through `call` and `subscribe` | Lua: the same, through `uniwow.call` and `uniwow.subscribe` (checked in step 9.3b); Python: milestone 11 |
| Splitting work over the cores, `parallel_for` | `uniwow_api::parallel_for` (step 9.2a) | — not planned; compiled modules run threads of their own (T7) | — | — |
| Picking in the 3D view, the selection shown in 3D | — designed in milestone 9, not built | — | — | — |
| The terrain of a map shown in the 3D view, with its horizon | The module `terrain` (steps 9.2c, 9.2e, 9.2f) | — not planned yet | — | — |
| The statistics of the 3D view: the times of the frames, of the interface thread and of the GPU, what each layer drew | `Layer::stats`; shown over the view (step 9.2e) | — not planned yet | — | — |
| Hotkeys: declared, read, bound to other keys by the user in *Edit > Hotkey* | `Registrar::hotkey`, `Hotkey::pressed`, `held` (step 9.2d) | — not planned yet | — | — |
| The memory of the GPU the editor draws with | `Context::gpu_memory` (step 9.2d) | — not planned yet | — | — |
| Editing the world: terrain, painting, objects and creatures placed | — designed in milestone 9, not built | — | — | — |
| Unsaved changes, asked about when the editor closes | `Module::unsaved`, `save_unsaved` | — not planned | — | — |
| Menu items | `Registrar::menu_item`, `Module::on_menu` | — not planned | — | — |
| Jobs of the kernel's pool, with progress and *Cancel* in the Jobs panel | `Context::spawn`, `spawn_thread` | — not planned; compiled modules run threads of their own (T7) | — | Scripts run on threads of their own (T6) |
| Reporting another module as the culprit of a failure (F5) | `Context::report_failure` | — not planned | — | — |
| Services between modules | `Registrar::provide`, `Context::service` | Commands, events and the objects are their form for every language (S1) | | |

The six gaps *not planned* and the five *not planned yet* are for the review to place in a
milestone.

---

## 4. Discovery and loading

### Retained mechanism: one folder per module, loaded at start

Output layout:

```
UniWoW.exe
uniwow_api.dll                 runtime: core/api, libs, egui, wgpu
std-<hash>.dll                 Rust standard library
modules\<id>\module.toml       manifest: id, name, version, kind, runtime fingerprint for a Rust module
modules\<id>\<id>.dll          a Rust or compiled module
modules\<id>\*.lua, *.py       a Lua or Python module
scripts\<language>-<version>\<tool>\
interpreters\python-3.xx\
```

At start, the kernel:

1. Scans `modules\*\module.toml`. A folder without a manifest whose subfolders hold manifests is a
   **group folder**, such as `modules\UI\`: its subfolders are scanned instead, whatever its name.
   A folder without a manifest, or without what its kind needs, is ignored and listed. An id is
   kept by the first folder, in the order of their paths, whose module is loaded; a later folder
   giving the same id is ignored and listed. A folder ignored, refused or disabled keeps no id.
2. Compares the **runtime fingerprint** of each Rust module with its own. The fingerprint
   identifies the compiler version and the runtime build. A mismatch refuses the module with the
   reason ("built for another runtime, rebuild it") instead of loading it. A compiled module is
   checked against the version of `uniwow.h` (S10). A Lua or Python module is to be handed to the
   module of its language, and refused with the reason without it: the target of milestones 10 and
   11. Until then, a manifest of kind `lua` or `python` is not valid, and its folder is ignored and
   listed.
3. Copies the DLL of each accepted Rust module to a temporary folder and loads the copy, so that
   the module can be rebuilt while the editor is open. A compiled module's DLL is loaded where it
   is, so that the DLLs it needs are found in its folder: the editor holds it, and it is rebuilt
   with the editor closed.
4. Calls the entry point, checks required services, orders initialization so that service
   providers start before their consumers, collects contributions, then calls `init`.
5. Catches any failure (error or panic) in `register`, `init` or while drawing a panel: that
   module is disabled and reported in the Modules panel and the log (R6).

Enabling or disabling a module from the Modules panel takes effect at the next start. A
loaded module is never unloaded while the editor runs. Until the project model exists, this
choice and the panel layout are stored per user, in `%APPDATA%\UniWoW\settings.json`.

Developer commands:

```
cargo xtask new-module <id>      creates the Rust module modules/<id>/ from a template (one empty panel)
cargo xtask build-module <id>    builds that Rust module only, against the current runtime
cargo xtask build                builds the runtime, the executable, every Rust module and the examples
cargo xtask run                  builds what changed, then starts the editor
```

| Change | Rebuild |
|---|---|
| Code of one module | That module only (`build-module`) |
| New module | That module only |
| `core/kernel` or `app` | The executable only; modules stay compatible, as long as the change enables no option of a crate the runtime uses (below) |
| `core/api`, `libs/*`, shared dependency versions or compiler | Runtime and every module (`build`), because the fingerprint changes |

The kernel and the app are built in the same `cargo build --workspace` as the runtime, and Cargo
merges the options each package asks of a shared crate. If the kernel or the app enabled an option
of a crate the runtime also uses, the runtime would be rebuilt and every module refused.
`cargo xtask check` reads the resolved dependency graph and refuses a direct dependency of
`core/kernel` or `app` on a crate of the runtime's dependency tree, naming it; such a crate is
reached through `uniwow-api` instead. A crate reached only through another dependency is not seen
by the check: `build-module` then detects the changed runtime and asks for a full build.

The fingerprint is the BLAKE3 hash of `uniwow_api.dll`. The MSVC linker runs with `/Brepro`,
which removes the link date and makes the PDB identifier depend on the content: relinking
unchanged code gives the same DLL, hence the same fingerprint (checked by rebuilding the runtime
after `cargo clean -p uniwow-api`, and after touching one of its sources without changing it).
A real change of the runtime still asks for every module to be rebuilt; `cargo xtask build` does it.

`build-module` compiles the whole workspace, not the module alone, so that Cargo merges the
options of shared crates exactly as `build` does and leaves the runtime untouched. A compilation
error in another module therefore blocks it too.

### Constraint accepted with this choice

Rust has no stable binary interface between separately compiled DLLs. Rust modules are
therefore only compatible with the runtime they were built against, with the same compiler.
The fingerprint (step 2) turns an incompatibility into a refusal with a message instead of a
crash. A Rust module taken from another machine works only if built with the same compiler and
the same runtime.

### Alternatives not retained

| Alternative | Reason |
|---|---|
| Modules compiled into the editor | Adding or removing a module requires recompiling the editor (R3). |
| A stable C interface for every module (abi_stable, stabby) | egui and wgpu types cannot cross it: Rust modules keep the Rust interface, with egui and wgpu. Compiled modules use the C interface, with interface objects and no 3D drawing. |
| One process per module | Sharing the 3D view and the panels between processes is too heavy. |

---

## 5. Core services (core/kernel)

| Service | Role |
|---|---|
| Shell | Main window, menus, dockable layout (egui_dock), layouts saved per user. Panels of absent modules leave the layout; a returning panel rejoins its area, or the default layout is rebuilt when its whole area had disappeared |
| Module loader | Section 4 |
| Modules panel | Lists the modules, their kind, version, state, refusal or failure reason, and their commands; enable or disable |
| Commands and history | Undo, redo, unsaved-changes tracking. When a module fails, all its entries leave the history, done and undone, with a warning in the log; the others stay valid since a command only changes its own module's state (F3). A document closed without saving takes its changes out of the history (`Context::forget_document`) |
| Events | Publish and subscribe, typed by serialisation: the topic is a string and the payload JSON, written with `Context::publish_as` and read with `Event::decode` into a type each module declares on its own side. No Rust type is shared between modules. Events may be published from any thread; they are delivered on the interface thread |
| Services | Registry of interface implementations provided by modules |
| Selection | Current selection, any type |
| Animatable properties | Registry of the properties modules let be animated: path, type, range, reading and writing; written without history during playback (milestone 6). A property whose range is not two numbers, the lowest first, or whose name is empty or holds a `/` or a space (its path is `<module>/<name>`), is refused with the reason |
| Project | Open, save; content defined in a later step |
| Settings | Global, per project, per module. Written atomically (temporary file, then rename), at most once per second and at exit |
| Hotkeys | Registry of the keys the kernel and the modules act on (`Registrar::hotkey`), with their keys by default; *Edit > Hotkey* binds them to others, kept in the settings (step 9.2d) |
| Jobs | Pool of worker threads, one per processor core: background jobs with progress and cancel (T2) |
| Log | Log panel shared by all modules. GPU errors captured by no module are logged instead of stopping the editor |
| Inspector host | Shows the selection with the inspector registered for its type |

The 3D view is not a core service: it is the `viewport` module (section 7).

Threads:

| Id | Rule |
|---|---|
| T1 | The interface thread draws, applies the undoable commands and owns the state of each module. It never waits for slow work. |
| T2 | The kernel keeps a pool of worker threads, one per processor core, for computations, below the normal priority, so that the interface thread is never kept waiting for a core while every worker computes. `Context::spawn` runs a job there, with progress and cancel; its result comes back to the module on the interface thread. Work that waits, such as a script, runs with `Context::spawn_thread` on a thread of its own, so that waiting never holds a thread of the pool; it is otherwise a job like the others. A job of the pool never waits without a time limit, for instance in `next_event` without a timeout; waiting in `parallel_for` for its slices, which the workers of the pool share with it, is not waiting without a limit. A Jobs panel lists the jobs running. |
| T3 | Service interfaces are shared between threads (`Send + Sync`, held in an `Arc`), so that jobs, scripts and compiled modules call them directly. An interface tied to the interface thread says so explicitly. |
| T4 | Each named command declares where it runs: on the interface thread when it changes a module's state (through an undoable command), or on the calling thread when it only reads or synchronises itself. The second kind answers at once, without waiting for a frame; called from the interface thread, a compiled module's command runs on the module's own thread and answers later. |
| T5 | The GPU device and queue can be used from any thread: jobs create and upload buffers and textures; only drawing happens on the interface thread. |
| T6 | Each run of a script has a named thread of its own (T2), never the interface thread. Each run of a Lua script has its own Lua state, so several run in parallel. Python scripts run on worker threads too, but standard CPython lets one thread at a time execute Python code (the GIL): their parallel work comes from the commands they call. |
| T7 | Every function of the C interface can be called from any thread; compiled modules may create their own threads. |

### Concurrency model

Who runs what, and how the threads reach each other:

```
 other threads: scripts (T6), jobs (T2), the threads of compiled modules (T7), any thread
 holding an Editor
   |  requests, in one queue (mpsc): Call, Answer, BeginGroup, EndGroup, RecordChange,
   |  ThreadEnded; events and failures in lists under a lock; ended jobs in a list
   v
 interface thread (T1): owns each module's state, the history and the open groups, the views of
 the interface objects, the pending commands and events (KernelHost)
   logic: collect events and failures -> apply commands -> serve requests for a time budget
          -> ended jobs -> events -> closing
   ui:    panels, modules' windows (modal windows), menus, Undo and Redo
   |  jobs posted (mpsc, one queue per compiled module)
   v
 thread of a compiled module (one each): its slots, paintings, undo and redo values, and its
 commands called from the interface thread
```

| Thread | Owns | Runs |
|---|---|---|
| Interface | Every module's state, the history and the open groups, the views of the interface objects, the kernel's state | The `Module` methods of every module, the undoable commands, Undo and Redo, the requests of the queue, the drawing |
| Worker of the pool | Nothing | Jobs of `Context::spawn` (T2); the sorts of large tables of the interface objects; the slices of `parallel_for`, after any job waiting |
| Thread of its own | Nothing | Scripts and jobs that wait (`Context::spawn_thread`), among them the threads of modules waiting for the frame signal of the viewport |
| Thread of a compiled module | The module's data, by its own rules | Its slots, paintings and `apply_change`, and its commands called from the interface thread |
| Any thread | — | Commands running on the caller (T4), the reading and writing of animatable properties, the C functions (T7) |

Shared state and locks:

| What | Lock | Taken by |
|---|---|---|
| Catalogue of commands, running modules, properties, settings | Read-write locks of the bridge | Any thread, briefly. Only the set of running modules is held while another is read (the catalogue or the properties, to list them); only the interface thread writes them |
| Events and failures from other threads | Lock of the bridge | Pushed by any thread, taken by the interface thread at each frame |
| Subscriptions | Lock of the bridge, then one queue per subscription | Delivered by the interface thread, read by the subscriber's thread |
| Interface objects of a compiled module | One lock per module | The interface thread while it draws them, any thread in the C functions. Never held while the code of the module owning them runs: slots, paintings and replies are called once it is released. The services drawing curve views, dopesheets and property grids, code of other modules, run while it is held: they touch no module's objects, and the properties they show are read before it is taken. In C++ and C#, the lock of the classes' connections is taken before it, never after |
| Queues of the pool: the jobs and the kernel's work, then the slices | One lock | Released before a job or a slice runs; the count of jobs waiting is read without it between two slices |

No lock is held while the kernel calls a module.

Orders guaranteed:

- The requests of one thread are served in the order sent; `ThreadEnded` comes after every request of the job that ended.
- The events a thread published before a call are delivered before those the call causes.
- A module's queued commands are applied once its call returns, in the order queued, into its caller's group when one is open on the calling thread (S4).
- On a compiled module's thread, jobs run in the order posted. A mouse move, or a curves change still under way, replaces the same signal of the same object still waiting, never across another job.
- After `disconnect` on the module's thread, or the destruction of the sender, the slot is not called again.
- A painting area is painted once at a time; a size asked meanwhile is painted next.
- The answer of a command a compiled module's thread ran comes through the queue (`Answer`), after the requests sent before it.
- Undo and Redo run only when no open group holds a change and no compiled module's thread has work; they first serve the requests waiting, so a change a compiled module recorded is in the history before them.

Not guaranteed:

- An order between the requests of different threads, beyond their arrival.
- The order, relative to an open group, of a change made by hand meanwhile (S4).
- `disconnect` from another thread does not wait for a call already under way.
- An event published during a frame is delivered at the next one.
- The order, within one frame, between a module's undo entries recorded through its interface
  objects, such as the keys of a sequence, served with the requests of the threads, and those of
  its commands (`Context::execute`), applied at the end of the kernel's pass.
- Every event reaching a subscription: one whose reader leaves 4,096 events unread is closed, with
  a warning in the log, and its reader then gets the error of a closed subscription.

Stress tests with random interleavings, seeded so that a failure can be replayed: threads calling and grouping at once through the router, each getting its answers in its order; the undo groups against a model of rule S4; Undo at random moments while a compiled module's thread records changes, never before the module's last change reaches the history.

Points to revisit: work of milestones 1 to 7 still done on the interface thread, small today;
milestone 9 takes up those that concern it.

- Loading the modules: at start, on the interface thread, one module after another (the hash of
  the runtime, the copy and hash of each DLL, its loading, then each `init`); the window waits for
  all of it, and the start grows with the number of modules.
- The files of the Timeline: listed, read, parsed and written on the interface thread when a
  sequence is opened or saved; a large sequence would freeze the editor while it loads or saves.
- The 3D view: a layer without a version records its render bundle again at every frame on the
  interface thread (since step 9.2a, a layer with one keeps it), and creates its GPU resources
  there when it first draws; large data (terrain, models) uploaded that way would stall frames.

---

## 6. Libraries (libs/*)

| Library | Role |
|---|---|
| formats | Read and write MPQ, DBC, ADT, WDT, WDL, WMO, M2, BLP. Based on warcraft-rs (MIT/Apache) where its writing is verified, own code otherwise. Milestone 9 proposes to keep it out of the runtime, inside the module `assets`, behind services, with the modern formats WarcraftXL loads, translated from wow.export (see milestone 9) |
| defs | DBC layouts for build 12340 (WoWDBDefs). Milestone 9 proposes it inside the module `assets` too |
| vfs | Client archive chain in the 3.3.5a load order, plus the project's own files on top. Milestone 9 proposes it inside the module `assets` too |
| gpu | Generic GPU helpers on wgpu (device, shaders, buffers, camera math). Drawing of each kind of object belongs to the module that owns it |
| db | MySQL access to the AzerothCore databases |
| server-link | The client of the observer `mod-uniwow-observer` and a fake observer for the tests (step 9.3b); the SOAP client and the control of the server process: later |
| client-link | Protocol with the WXL client module |
| ids | Id range allocation per module |

---

## 7. Module catalogue

Each line is one Rust module of this project. The list is open: new modules, of any kind, are
added through section 4 without touching the core.

### World

| Id | Module |
|---|---|
| viewport | 3D window: camera, picking, gizmos. Provides the "viewport" service to which the modules below add their drawing and tools. Each layer records into its own render bundle, with the target's formats and sample count, inside a validation error scope; the viewport also catches the panic of `RenderBundleEncoder::finish`, which wgpu 30 raises on an invalid command instead of reporting it to the scope. A faulty layer is removed and its module reported (F5); the grid and the other layers stay |
| maps | Map list (Map.dbc), WDT and WDL, create or duplicate a map, tile management, minimap tiles |
| terrain | Draws terrain in the viewport; height sculpting, texture painting (layers, alpha maps), holes, vertex shading, area painting, chunk flags |
| liquids | Draws and edits water, lava, slime: create, heights, types |
| placement | Draws M2 doodads and WMOs in tiles; place, move, rotate, scale, with gizmos and snapping |
| environment | Lighting and sky (Light tables, skyboxes), zone music and ambience |
| spawns | Server creatures and game objects placed in the viewport, waypoints, formations |
| server-map-data | Regenerate .map, vmaps and mmaps for changed tiles (AzerothCore extractors), in the background |
| live-world | The world as it runs on the server, in the viewport: creatures, NPCs, game objects and players where they are, in real time, from the observer `mod-uniwow-observer` (milestone 9) |

### Data

| Id | Module |
|---|---|
| dbc | Generic editor for every client DBC, links between tables. The client copy is the source of truth; the server copy is byte-identical |
| world-db | Generic editor for the AzerothCore world tables, navigation along references |
| spells | Spell editor: DBC rows and server tables of a spell together |
| items | Item editor: Item.dbc and item_template together |
| creatures | Creature editor: template, models, equipment |
| quests | Quest editor: template, givers and enders, objectives, rewards |
| loot | Every loot table |
| gossip | Gossip menus, vendors, trainers |
| localization | Every player-facing text and its locales in one place |

### Assets

| Id | Module |
|---|---|
| assets | Browse the virtual file system (archives and project), search, preview. Holds the reading of the formats and archives of WoW and offers it through the services `vfs` and `formats` (milestone 9) |
| textures | BLP view, PNG to BLP and back, format and size checks |
| models | M2 and WMO viewer (animations, textures, attachments), import of new models. Provides the `models` service: M2 and WMO loaded in jobs, kept on the GPU, and instances drawn for the modules giving them (milestone 9) |
| retail-import | Fetch modern assets through the wow.export bridge |

### Interface and scripts

| Id | Module |
|---|---|
| interface | Client interface files (FrameXML, GlueXML, addons, AIO): tree, open in an external editor, reload in the client |
| scripts | ALE Lua scripts and C++ server modules: open in an external editor, build, errors in the log, reload or restart |

### Animation

| Id | Module |
|---|---|
| timeline | Sequences in two modes, as the Timeline and Animation windows of Unity. Montage: tracks of clips moved, trimmed, cut and snapped. Animation: keys on the animatable properties declared by the modules, dopesheet, curves with tangents, playback and recording. Sequences saved as files until the project model exists |

### Run

| Id | Module |
|---|---|
| server | Start and stop authserver and worldserver, console, SOAP commands, server logs |
| play | One action: server and client started or reused, automatic login on the admin account, character moved to the viewport camera, changes pushed live |

### Delivery

| Id | Module |
|---|---|
| package | Build an installable WoW-mods module (installer.json): files in the right patch archive, DBC copied from client to server, install and uninstall SQL, id ranges checked |

### Scripting

| Id | Module |
|---|---|
| scripting-lua | Lua 5.1, the dialect of the 3.3.5 client and of ALE scripts, compiled into the runtime (mlua): console panel, script runner, the `uniwow` module, and the host of Lua modules |
| scripting-python | CPython, the latest stable version when the module is built, with the official embeddable distribution of Windows in `interpreters\python-3.xx\`, embedded by a host DLL built apart (PyO3) that reaches the editor through `uniwow.h`: the same console, runner and `uniwow` module, and the host of Python modules |

Rules:

| Id | Rule |
|---|---|
| S1 | One generic interface, the same for every language: list the named commands with their descriptions and schemas, call one by name, publish and receive events, read and write settings, log, and, for modules, offer commands, build panels of interface objects and record undoable changes. Commands, events and settings carry their values as JSON; the interface objects are reached through typed functions (handles, texts, numbers). It is defined once, independently of any language, and also offered as a C interface (`extern "C"` functions taking and returning UTF-8 JSON, header `uniwow.h`), so that compiled code reaches the same commands without depending on the Rust ABI. The Lua and Python `uniwow` modules only translate their values to and from JSON on top of this interface: they add no command of their own, so every language always has the same access. Each language has classes over the interface objects, named as in Qt. |
| S2 | Named commands (F6) must exist in the kernel first: they are what scripts and compiled modules mostly call. |
| S3 | Scripts never run on the interface thread (T6). A call that changes a module's state is applied on the interface thread at the next frame; a call to a command running on the calling thread answers at once (T4). A running script can be stopped. |
| S4 | Every change one run of a script makes forms a single undo entry. The kernel learns to group commands. A group belongs to one caller on one thread and can be nested; a command delegated to another module runs inside its caller's group, as a change made on a thread where its caller has no group open enters the group opened last on that thread. A group closes at its outermost end, when the job that opened it ends, when its module fails, or from the Edit menu. While an open group already holds a change, Undo and Redo are refused, greyed with the reason; a group that changed nothing yet, such as a script waiting for events, blocks nothing. Changes made by hand meanwhile enter the history on their own: when they touch what the script changes, their order relative to the group can be imprecise, and so is the undo order of two runs in parallel that change the same thing. Indirect changes are not grouped: a command triggered by an event a script publishes is applied when the event is delivered, outside the group. A change a slice of `parallel_for` records is made on its worker's thread, outside the group of the job that called it: slices compute, and the job records. |
| S5 | A Lua or Python error is shown in the console with its line; it does not make the module fail. |
| S6 | Compiled code runs inside the editor and can end its process: a crash there is not an error that can be caught. Lua scripts cannot load C modules, nor precompiled Lua chunks, which the bytecode checks of Lua 5.1 cannot keep from corrupting the memory: `string.dump` is removed, and every way of loading Lua code (scripts, console, `load`, `loadstring`, `loadfile`, `dofile`, `require`) accepts source text only. The compiled packages a Python script imports and the compiled modules carry this risk, which is accepted. |
| S7 | Scripts and compiled modules have full access to the machine, like editor scripts in Unity: one received from someone else is checked before it is used. |
| S8 | Each language stays optional: without `scripting-python.dll`, or without the Python files, the editor starts with Lua only, and the other way round. In particular the runtime must not require the Python DLL to start. |
| S9 | One interpreter per language. Scripts are stored by language and version, then by tool: `scripts\lua-5.1\<tool>\<script>.lua`, `scripts\python-3.xx\<tool>\<script>.py`, the Python version being the one shipped. A script may also sit directly in the folder of its language. A script loads the other files of its tool. Every module has its folder in `modules\` (section 3). |
| S10 | A compiled module is a folder `modules\<id>\` with its manifest and a DLL exporting one C entry point. It receives the table of functions of the C interface and returns its description (name, version, the version of `uniwow.h` it was built with), the named commands it offers, implemented in its own language with the same JSON form, the panels it fills with interface objects, and the function that applies its undo and redo values. They join the catalogue as delegated commands (F6): a Rust module's command of the same name keeps its name, and the Modules panel shows the compiled module's one as refused. |

Risks to verify first, before any other work on scripting: the runtime's exported symbol count with PyO3 and mlua inside it; starting the editor without the Python DLL while PyO3 is part of the runtime (delayed loading); the embeddable Python distribution beside the executable; a C++ module and a C# NativeAOT module calling the C interface from several threads. Verified for milestone 4: see section 9.

---

## 8. Repository layout

```
E:\WoW-editor
  Cargo.toml            workspace: app, core/*, libs/*, modules/*, xtask
  app/
  core/api/
  core/kernel/
  libs/<name>/
  modules/<id>/         Rust modules
  modules/UI/<id>/      Rust modules of the interface, deployed in modules\UI\<id>\
  examples/modules/<id>/  sample modules in C++, C#, Lua and Python, built and installed by
                        `cargo xtask build` as their author would
  sdk/uniwow.h          the C interface of compiled modules (S1)
  sdk/uniwow.hpp, sdk/UniWoW.cs  its classes for C++ and C#
  sdk/bindings.toml     the numbers of the kinds, properties and signals of the interface objects,
                        written into every language by `cargo xtask bindings`
  sdk/tests/            tests of the C++ and C# classes, run by `cargo xtask test-sdk`
  scripts/<language>-<version>/<tool>/  sample scripts, copied beside the executable
  client-bridge/        C++ (WXL SDK), own build
  server/mod-uniwow-observer/  C++ module of AzerothCore, built with the AzerothCore source tree
  xtask/
  docs/
  .github/workflows/    CI on Windows with .NET 10: fmt, bindings --check, clippy -D warnings, cargo test,
                        xtask build, the C# sample module built, test-sdk and check
```

---

## 9. Milestones

A milestone is marked done only after its external review; milestones are kept small.

### Milestone 1: proof of the architecture (done)

Content:

- Workspace, `xtask` (new-feature, build, build-feature, run, check).
- Runtime built as shared DLLs; executable loading feature DLLs as in section 4.
- Kernel: shell with dockable panels, feature loader, Features panel, commands and undo,
  events, services, settings, log panel.
- `viewport` feature: empty 3D window drawn by wgpu inside egui (camera and grid), providing
  the viewport service.
- Two sample features created from the template: one draws a cube through the viewport
  service, both communicate only through an event.

No WoW file is handled in this milestone.

Acceptance:

| Check | Expected result |
|---|---|
| Remove `features\viewport\` from the output, start the editor | The editor starts without any 3D window; the cube feature is listed as running without its 3D part |
| Put it back, start the editor | The 3D window is back with the grid and the cube |
| `cargo xtask new-feature third`, `cargo xtask build-feature third`, start the editor | A third panel appears; `UniWoW.exe` and the runtime DLLs are unchanged (same hash); only `features/third/` and `Cargo.lock` (generated list of the workspace members) change |
| Rebuild a feature while the editor is open | The build succeeds; the new version is loaded at the next start |
| Change `core/api`, rebuild the runtime only, start the editor | Every feature is refused with "built for another runtime"; no crash |
| A sample feature panics while drawing | That feature is shown as failed in the Features panel; the rest keeps working |
| A sample feature depends on the other in its Cargo.toml | `cargo xtask check` fails and names the offending dependency |
| `sample-faulty` draws without its bind group, or with a pipeline of the wrong colour format | The viewport and the grid stay; only `sample-faulty` is marked as failed, reported by `viewport` |
| `sample-faulty` panics while drawing (button "Panic while drawing") | The viewport and the grid stay; only `sample-faulty` is marked as failed, reported by `viewport` |

Risk verified first: a Windows DLL exports at most 65,535 symbols. Unoptimised, the runtime
exceeds it (LNK1189). The `dev` profile is therefore built with `opt-level = 2`, which stops the
sharing of generic instantiations: 17,764 exported symbols, 27% of the limit. `cargo xtask check`
reports the count and fails from 50,000; the runtime would then have to be split.

### Milestone 2: threads, jobs and named commands (done)

The common base of speed (R7, R8) and of scripting (S1, S2): the threading model and the catalogue
of commands, before any other feature is written on the milestone 1 contract.

Content:

- **Contract**: service interfaces become shared between threads (`Arc`, `Send + Sync`, T3); the
  viewport service and its layers follow, and so do the samples. Features themselves stay on the
  interface thread (T1).
- **Jobs (T2)**: a pool of worker threads, one per processor core. `Context::spawn` runs a job with
  progress and cancel; its result, or the message of its panic, comes back to the feature on the
  interface thread. A panic in a job does not make the feature fail; the feature decides. A Jobs
  panel lists the running jobs, with their progress and a Cancel button.
- **Named commands (F6, T4)**: a feature declares each command with its name, description, the
  schema of its arguments and result, and where it runs: on the interface thread (it may change
  state, through an undoable command) or on the calling thread (a `Send + Sync` handler). A panic
  in a command handler makes its feature fail.
- **Editor handle**: the generic interface of S1 in Rust, a `Send + Sync` handle any thread can hold:
  list the commands, call one by name (JSON in, JSON out), publish events, log. From a worker
  thread, a call blocks until its answer: at once for a calling-thread command; for an
  interface-thread command, the interface thread serves the pending calls during a time budget of
  a few milliseconds each frame, so that successive calls are not limited to one per frame. From
  the interface thread, `Context::call` queues the call, whatever command it names: it is served in
  the kernel's next logic pass, and its answer handed to the module's `on_reply` right after, at
  the next frame for a call made while drawing or while taking jobs and events back.
  The C interface over this handle comes with scripting (milestone 3).
- **Events from any thread (T3)**, delivered on the interface thread.
- **Commands panel**: the catalogue with descriptions and schemas, and a field to call a command
  with JSON arguments and see its answer.
- **Samples**: `sample-cube` offers `cube.paint` (interface thread, undoable) and `cube.color`
  (calling thread, reading only) and builds its GPU buffers in a job (T5); `sample-notes` starts a
  long computation job and a measuring job that calls both commands in a loop and shows how many
  calls per second each kind reaches.

Acceptance:

| Check | Expected result |
|---|---|
| Start the long computation job | The 3D view stays fluid (the cube keeps turning); the Jobs panel shows its progress |
| Cancel it from the Jobs panel | It stops; the feature receives its cancellation |
| Start as many computation jobs as there are cores | Every core works (Task Manager); the interface stays fluid |
| A job panics | The feature receives the panic message as the job's result; it keeps running |
| Commands panel: call `cube.paint` with a colour | The cube is painted; Ctrl+Z undoes it |
| Commands panel: call an unknown command, or with invalid arguments | A clear error is shown; nothing else happens |
| The measuring job | Calls per second for both kinds are shown; calling-thread calls are not bounded by the frame rate, interface-thread calls exceed one per frame |
| A job publishes an event | It is delivered on the interface thread and listed by `sample-notes` |
| The cube's GPU buffers are built in a job | The cube is drawn as before |
| `cargo test`, `cargo xtask check`, CI | Green; new tests cover the pool, the cancellation and the routing of commands |

As built: the interface thread serves the calls of other threads for 4 ms per frame, waiting up
to 0.5 ms after each call for the next one of a thread calling in a loop. Measured on a 32-thread
processor: 3.7 million calls per second to a command running on the calling thread, 24,000 per
second to a command running on the interface thread. While the window is minimised, eframe wakes the editor at most every 100 ms and nothing is drawn: the interface thread then serves calls for 80 ms per wake. Measured from Lua (fourth and fifth reviews): about 52,500 calls per second to `cube.paint` while minimised, against 5,400 with the 4 ms budget and 26,000 to 39,000 with the window shown. `Editor::call` refuses, with an error, to wait
on the interface thread for a command of the interface thread, which would wait for itself.

### Milestone 3: C interface, native modules and Lua scripts (done)

The generic interface of S1 complete and offered in C, the first native module, and Lua 5.1.
Python, whose risks are of another kind (embedding, delayed loading of its DLL, distribution,
supported versions), comes in milestone 4.

Content:

- **Generic interface complete (S1)**: the `Editor` handle gains settings, logging and event
  subscriptions usable from any thread (subscribe to a topic, then wait for the next event with a
  time limit).
- **One undo entry per run (S4)**: the kernel groups the commands a script run or a module call
  applies into a single history entry.
- **C interface (S1, S10, T7)**: `sdk/uniwow.h`, a table of C functions (list the commands, call,
  publish, subscribe and wait for an event, settings, log, undo groups) exchanging UTF-8 JSON,
  callable from any thread. The editor hands each text it produces to a reply function given by
  the caller, valid during that call: no string crosses to be freed. A module exports one entry
  point that receives this table and returns its name, version and commands.
- **`native-modules` feature**: loads the DLLs of `modules\`, adds their commands to the catalogue
  (running on the calling thread), lists the modules and their state in a panel. A module without
  the entry point, or whose entry point reports an error, is refused with the reason.
- **Sample C++ module** in `modules-src\sample-cpp\`, compiled by `cargo xtask build` with the MSVC
  compiler found on the machine: one command of its own, and a thread of its own calling
  `cube.paint` through the C interface.
- **`scripting-lua` feature**: Lua 5.1 compiled into the runtime (`libs/lua`, mlua). A console panel
  (one line to evaluate, the output below), the list of `scripts\lua-5.1\`, Run and Stop. Each run
  on a worker thread with its own Lua state (T6). The `uniwow` Lua module translates tables to and
  from JSON over the generic interface and adds nothing of its own (S1). Errors shown with their
  line (S5); C modules cannot be loaded (S6); Stop interrupts a running script through a hook
  checking its cancellation (S3).
- **Sample scripts**: paint the cube, measure calls, print the events of a topic.

Acceptance:

| Check | Expected result |
|---|---|
| Start the editor | The C++ module is listed as running; its command appears in the Commands panel and answers |
| The C++ module's thread calls `cube.paint` | The cube is painted; Ctrl+Z undoes it |
| Remove `modules\sample-cpp.dll`, start | The editor starts without it |
| A DLL without the entry point in `modules\` | Refused with the reason; the rest runs |
| Lua console: `uniwow.call("cube.paint", {color = {1, 0, 0}})` | The cube turns red |
| A Lua script painting the cube three times | One undo entry; Ctrl+Z restores the colour before the script |
| A Lua script with an error | The message and the line are shown; the feature keeps running |
| A script looping forever, then Stop | It ends; the interface stays fluid throughout |
| Two scripts at once | Both run in parallel |
| `require` of a C module in Lua | Refused |
| A script waiting for the events of a topic | It prints them as they are published |
| Calls per second from Lua, for both kinds of command | Measured and recorded here |
| Runtime size, tests, `cargo xtask check`, CI | Below the symbol limit, green |

As built:

- **C interface**: a deviation from the content first validated, which had a function freeing the
  strings returned by the editor. Texts are given to a reply function instead, so no memory
  crosses between the editor and a module, and a module that forgets to free leaks nothing. The
  table also has `begin_group` and `end_group`: a module's command often returns before the work
  it starts ends (the sample's thread), so a module groups its own changes into one undo entry.
  A script run is grouped automatically. A command whose answer is not JSON, against `uniwow.h`,
  has it handed on as a JSON string, with a warning under its module's name the first time; one
  that fails without a message gets the editor's ("the command failed without a message"). A
  panic in the editor's side of a C function, or on a module's thread, is logged with the
  module's name.
- **Lua in the runtime**: mlua's generic code is instantiated in the feature using it, which then
  calls the Lua C functions directly. The runtime exports the 122 functions of the Lua 5.1 C API
  (`core/api/build.rs`), so that every feature uses the one Lua compiled into it. Runtime: 18,261
  exported symbols, 28% of the limit.
- **Modules**: loaded once from `modules\` beside the executable and never unloaded; a module's
  calls go through an `Editor` named `native-modules#<module>`. `cargo xtask build` compiles each
  folder of `modules-src\` (`cl /LD /MD /O2 /std:c++17 /W4 /WX`, linked with `/Brepro` so that an
  unchanged module gives the same file) and copies `scripts\` beside the executable.
- **Lua**: the standard libraries mlua deems safe: `debug` is left out and no C code can be loaded,
  but `os` and `io` stay whole (S7: `os.exit` ends the editor without the question about unsaved
  changes). A run may take 1 GB of memory; beyond it, the script gets "not enough memory" and the
  editor goes on. A table crossing to JSON is a list or a map: one holding a list, keys 1 to its
  length, and other keys besides is refused with an error rather than losing keys; NaN becomes
  null. `uniwow.end_group` ends only a group the script opened with `begin_group`, never the group
  of the run. `require` finds Lua files in `scripts\lua-5.1\`. `uniwow.next_event(subscription, timeout_ms)` waits without a time limit when
  `timeout_ms` is omitted, until an event arrives or the run is stopped; the subscriptions a run
  leaves open end with it. Stop is checked every 1,000 Lua instructions and before each call.
  Each console line is a run of its own, in a new Lua state: a global set on one line is gone on
  the next. The console shows a table returned by an expression as JSON.
- **Calls per second**, measured on a 32-thread processor in one session, `measure.lua` (Lua) and
  the measuring job of `sample-notes` (Rust):

  | Command | From Lua | From Rust |
  |---|---|---|
  | `cube.color`, calling thread | 1.06 to 1.09 million | 3.6 million |
  | `cube.paint`, interface thread | 30,000 to 39,000 | 24,000 |

### Milestone 4: modules of every kind (done)

The model of section 3 put in place with what exists: one folder and manifest per module, the
words of the vocabulary, compiled modules loaded by the kernel, scripts by tool. Compiled, Lua and
Python modules gain no new capability yet: panels and undo come with milestone 5.

Content:

- **Words**: everything named *feature* is named *module*: the `Module` trait and its entry point
  macro, `modules/` in the repository and beside the executable, the crates `uniwow-module-<id>`,
  `module.toml`, the Modules panel, `cargo xtask new-module` and `build-module`, the rules and the
  messages. Settings saved under the former names are still read.
- **One folder per module**: `modules\<id>\module.toml` gives its `kind`: `rust` for the modules
  of this project, `compiled` for a compiled module, whose author writes the manifest (id, name,
  version, kind, the name of the DLL) beside the DLL.
- **Compiled modules loaded by the kernel**: what `native-modules` does (loading, the C interface
  over an `Editor`, refusals with their reason) moves into the core, which declares their commands
  on their behalf (delegated, F6). `native-modules` disappears: the Modules panel lists every
  module with its kind, state and commands, refused ones with their reason, and a compiled module
  is enabled or disabled there like any other.
- **Examples**: the sample C++ module moves to `examples/modules/sample-cpp/` with its manifest;
  `cargo xtask build` builds it and installs it in `modules\sample-cpp\`, as its author would.
- **Scripts by tool (S9)**: the Lua panel lists the scripts of `scripts\lua-5.1\` as a tree, by tool
  folder; `require` looks in the folder of the script's tool first; the samples move into
  `scripts\lua-5.1\samples\`.

Acceptance:

| Check | Expected result |
|---|---|
| Start the editor | The Modules panel lists every module with its kind: the Rust modules, and `sample-cpp` as compiled, with its commands |
| Remove `modules\viewport\`, start | No 3D window; the rest works (R4) |
| Remove `modules\sample-cpp\`, start | The editor starts without it; its commands are gone |
| A folder of `modules\` without manifest, and a compiled module whose DLL has no entry point | One ignored, the other refused, each with its reason; the rest runs |
| A compiled module offering `cube.paint` | The cube keeps it; the Modules panel shows the module's one as refused |
| Disable `sample-cpp` in the Modules panel, restart; enable it, restart | Not loaded, then back |
| `cargo xtask new-module third`, then `cargo xtask build-module third` | A third module with its panel; the runtime and the executable unchanged |
| The Lua panel | Scripts listed under their tool, the examples under `samples`; a script loads another file of its tool with `require` |
| The checks of milestones 1 to 3 | Same results under the new names: C++ thread painting, Lua console and scripts, undo groups, Stop |
| Tests, `cargo xtask check`, CI | Green |

As built:

- **Compiled modules in the kernel**: the C interface over an `Editor` was in the runtime
  (`uniwow_api::capi`), in the kernel since the sixth review; the kernel loads a compiled module's DLL where it is, so that the DLLs it
  needs are found in its folder, and refuses one named like a DLL already in the process. Each
  compiled module is a module of its own in the catalogue, with its own `Editor`, settings and log
  name, and its commands declared on its behalf as delegated (F6).
- **Manifest**: `module.toml` has a `kind` (`rust` or `compiled`); `package`, `dll_hash` and
  `runtime` concern Rust modules only. Settings and layouts saved with the former names
  (`disabled_features`, `features`, a tab's `feature`) were read until the full verification of
  the milestones, which removed them: none was left in use.
- **Modules panel**: kind, state and commands of every module; a command set aside shows the module
  that offers it.
- **Scripts**: the panel lists the scripts placed directly in `scripts\lua-5.1\`, then each tool
  folder as a group that opens and closes; `require` looks in the script's tool folder first, then
  in the language folder.

### Milestone 5: panels and undo for compiled modules (done)

An interface API modelled on Qt, defined once in the core for every module that is not written in
Rust, offered here to compiled modules with classes for C++ and C#; Lua and Python modules receive
the same objects, with classes in their language, in milestones 10 and 11.

Measures taken first, with egui 0.36.2 as the editor uses it:

| Measure | Result |
|---|---|
| Drawing shapes again at every frame | 0.9 ms for 1,000 shapes, 9.4 ms for 10,000: too slow beyond a few thousand |
| Shapes turned into a mesh once, drawn at every frame | 0.35 ms for 1,000 shapes, 2.5 ms for 10,000; building the mesh: 0.6 ms and 7.8 ms |
| Moving that mesh (scroll, zoom) on the processor | 5.6 ms for 10,000 shapes at every frame: a view is moved on the GPU instead |
| .NET 10 in the CI | GitHub's runners install it with `actions/setup-dotnet` (version 10.0.x) |

Content:

- **Objects, as in Qt**: a module creates objects and receives a handle for each, sets their
  properties one by one, places widgets in layouts and connects signals to its functions. The core
  keeps the tree of objects and draws it: a call from any thread changes it and shows at the next
  frame, and the interface thread never waits for a module (T1). The functions connected to a
  signal run in order on a thread the core keeps for the module, as a queued connection does in Qt,
  never on the interface thread. A widget shows what the user did to it at once (a ticked box, a
  moved slider), then tells the module. Destroying an object destroys its children.
- **Base widgets**: `Label`, `PushButton`, `CheckBox`, `Slider`, `SpinBox`, `LineEdit`, `ComboBox`,
  a separator line, `GroupBox`, and the layouts `VBoxLayout`, `HBoxLayout`, `GridLayout`; each with
  its usual properties (text, checked, value and range, items and current index, enabled, visible,
  tooltip) and signals (`clicked`, `toggled`, `valueChanged`, `textChanged`, `editingFinished`,
  `currentIndexChanged`). A module declares its panels when it starts (id, title, area of the dock),
  each holding one layout. Lists, trees, tables, tabs and menus come in a later milestone.
- **Graphics scene, as `QGraphicsScene` and `QGraphicsView`**: a `GraphicsView` widget shows a
  `GraphicsScene` of items: `RectItem`, `LineItem`, `EllipseItem`, `TextItem` and `ItemGroup`, with
  position, size, pen, brush, stacking order, tooltip, and the flags `ItemIsMovable` (along x, y or
  both, within bounds) and `ItemIsSelectable`. A movable item follows the pointer in the core,
  without waiting for the module, which receives where it was dropped; the scene also tells
  presses, double clicks and selection changes. The view scrolls and zooms with the mouse and can be
  set by the module. The core keeps the items on the GPU and draws them through the view's
  transform: scrolling and zooming cost nothing per item; an item being dragged is drawn on its own
  until it is dropped; texts are drawn for the visible items only.
- **Painting, as `paintEvent` and `QPainter`**: a `PaintArea` widget asks its module to paint when
  it is shown, resized, or after `update()`; the module paints on its thread with a painter
  (`setPen`, `setBrush`, `drawLine`, `drawRect`, `drawEllipse`, `drawText`, `translate`, `scale`,
  `save`, `restore`), and the picture shows at the next frame. Mouse presses, moves, releases and
  the wheel reach the module as signals, with the position and the keys held.
- **Undoable changes (F2, S4)**: a module changes its own state, then records the change with a
  label and two JSON values, the one that undoes it and the one that redoes it. The change enters
  the history, or the open undo group of the calling thread, like any other. Undo and Redo hand the
  matching value to the module, on its thread, which applies it; a module that fails to apply it
  fails, and its changes leave the history.
- **`uniwow.h` version 3**: the C functions of the objects, typed (handles, texts, numbers):
  commands, events and settings keep their JSON (S1 changes accordingly). A module built with
  version 2 is refused with the reason; `sample-cpp` moves to version 3. Beside it,
  `sdk/uniwow.hpp`, header-only C++ classes (`uniwow::PushButton`, signals connected to lambdas),
  and `sdk/UniWoW.cs`, the same classes for C#.
- **Sample C++ module on the graphics scene** in `examples/modules/sample-scene/`: a board of
  coloured cards, each card dragged as one undo entry, a click on a card painting the cube in its
  colour through `cube.paint`, zoom and scroll outside the history, Add card, the selected card
  shown under the view; commands `scene.cards`, `scene.add_card` and `scene.fill` (many cards at
  once, to measure). The Timeline is a Rust module of its own (milestones 6 to 9).
- **Sample C# module** in `examples/modules/sample-csharp/` (.NET 10, NativeAOT): `cs.sum`,
  `cs.paint_from_threads` (each thread's paints as one undo entry), and a panel: a counter changed
  by buttons, a slider and a spin box, each change undoable, with a `PaintArea` drawing its history
  as bars. Built by `cargo xtask build` with `dotnet publish`; without the .NET SDK the build says
  so and goes on without it; the CI installs .NET 10 and builds it.
- **Section 3** describes the objects, their properties and signals, written once for every
  language.

Acceptance:

| Check | Expected result |
|---|---|
| Start the editor | `sample-scene` and `sample-csharp` listed as compiled and running; their panels in the dock and in the Window menu |
| Drag a card | It follows the pointer; one undo entry when dropped; Ctrl+Z puts it back, Ctrl+Y moves it again |
| Click a card | The cube takes its colour |
| Scroll and zoom the board | The view moves; nothing enters the history |
| Add card, then Ctrl+Z | A card appears, then goes |
| `scene.fill` with 10,000 cards | The interface stays fluid while a card is dragged and while zooming; the frame time is measured and recorded here |
| The C# panel: buttons, slider, spin box | The counter changes, the widgets agree, each change is one undo entry; the bars are painted again |
| Resize the C# panel | The bars are painted again at the new size |
| A slot of a module waiting two seconds | The interface stays fluid meanwhile; the next signals wait their turn |
| `cs.paint_from_threads` | Each thread's paints form one undo entry |
| A Lua script adding three cards | One undo entry |
| A module built with `uniwow.h` version 2 | Refused with the reason; the rest runs |
| A module failing to apply an undo value | It fails, its changes leave the history, its panel says so; the rest runs |
| Remove `modules\sample-scene\`, start | The editor starts without it; its panel is gone |
| Tests, `cargo xtask check`, CI with the C# module built | Green |

As built:

- **Objects and panels** as in section 3. A module's objects are kept in the runtime
  (`uniwow_api::ui`) and drawn by the kernel in the module's panels. The scene is drawn by the GPU
  into a texture of its own, its colours taken as given (no conversion to linear light); each
  item keeps its mesh until it changes. The slider shows no number beside it, as `QSlider`; the
  spans of a grid layout are kept but not drawn yet.
- **Signals**: `sliderReleased` is also sent after a click or a key on the slider, so that the
  module always learns the final value. A double click on a card of `sample-scene` paints the cube
  (a click selects the card and starts dragging it).
- **Undo groups (S4)**: a change recorded by a command delegated to another module, such as
  `scene.add_card` called by a Lua script, enters the script's group.
- **C#**: `sdk/UniWoW.cs` is compiled into the module; `sample-csharp` also has two trial
  controls for the acceptance, *Fail to apply undo and redo* and *Wait 2 seconds*. `cargo xtask
  build` publishes it when `dotnet --list-sdks` lists a 10.x SDK, and otherwise says that it is left
  out; the CI checks that its DLL was built.
- **Lua**: `scripts\lua-5.1\samples\cards.lua` adds three cards to the board, as one undo entry.
- **Measured** on the development machine, editor built with the debug profile (optimised), screen
  at 60 Hz:

  | Board | Interface work per frame | Frames |
  |---|---|---|
  | 6 cards, dragging one | 0.4 ms | 60 per second |
  | 10,006 cards (30,018 objects), dragging one | 4.5 ms | 60 per second |
  | 10,006 cards, zooming | 4.3 to 4.9 ms | 51 to 60 per second |

  `scene.fill` with 10,000 cards took 0.66 s; the first frame after it, which builds the meshes,
  88 ms.

### Milestone 6: animatable properties and the Timeline in Animation mode (done)

The Animation mode comes first, as the user asked: dragging the playhead shows the cube's position,
rotation, scale and colour change in the 3D view as it moves. The Montage mode follows in
milestone 7, curves and recording in milestone 8.

Content:

- **Animatable properties**, a contract of the core, as Unity animates any field of a component. A
  Rust module declares, when it registers, each property it lets be animated: its path
  (`<module>/<name>`), its label, its type (number, vector of three numbers, colour, boolean), its
  range, and two functions that read and write its value, callable from any thread. The kernel
  keeps their catalogue as it keeps the commands; the properties of a module that stops or fails
  leave it, and a write to them is refused. A module lists the properties and reads and writes them
  through its `Context`. A write is not an undoable change: it enters no history. Compiled, Lua and
  Python modules declare theirs in a later milestone, through the C interface.
- **The sample cube** declares `position` (vector, its centre, at (0, 0, 1) as now), `rotation`
  (vector, Euler angles in degrees, applied around x, then y, then z), `scale` (vector, from 0.01 to
  100 on each axis, 1 by default) and `colour`. Its rotation by itself disappears, with its speed:
  the slider of its panel, the command that changed it, and `speed` in the result of `cube.color`.
- **`timeline` module** (Rust, egui), a panel *Timeline* in the bottom area, as the Animation window
  of Unity:
  - **Sequence**: frame rate (30 by default), length in frames (120 by default), and one track per
    animated property, holding keys. A key is a whole frame and a value of the property's type.
  - **Properties** on the left: the tracks, grouped by module, each with its value at the playhead.
    *Add property* lists the declared properties not yet in the sequence; a track can be removed.
    Editing a value sets the key at the playhead, adding it when there is none. The key button adds
    a key at the playhead with the property's current value, as its module holds it.
  - **Dopesheet** on the right: a ruler in seconds and frames (`1:15`), a summary row of all the
    keys, then one row per track with its keys as diamonds. A click selects a key, Ctrl+click adds
    or removes one, a box drawn on the rows selects the keys inside; dragging moves the selected
    keys by whole frames, a key landing on another of its track replacing it; Delete removes them.
    Dragging a diamond of the summary row moves every key of its frame. The wheel over the keys
    zooms the time, the middle button scrolls it; the rows scroll by their bar.
  - **Playhead**: clicked or dragged in the ruler. Each time it moves, every track is evaluated and
    its value written to its property, so the 3D view follows it while it is dragged. Between two
    keys the values change smoothly without overshooting them (as the *Clamped Auto* keys of Unity),
    each number of a vector or colour on its own; a boolean keeps the value of the previous key;
    before the first key and after the last, the value of that key.
  - **Playback**: play and pause (also Space over the panel), first frame, previous key, next key,
    last frame, loop, the current frame typed in a field. Playing follows the clock at the
    sequence's frame rate and writes the values at each frame.
  - **Undo (F2)**: each change of the sequence is one undo entry (a key added, moved, set or
    removed, a track added or removed, the frame rate, the length); after Undo or Redo, the values
    at the playhead are written again. Moving the playhead and playing enter no history. The
    timeline writes only when the playhead or the keys change: a value changed elsewhere meanwhile,
    in the cube's panel or by `cube.paint`, stays until then.
  - **Files**: one sequence per file, readable JSON, in `sequences\` beside the executable until the
    project model exists (section 10). The panel lists them, read again each time the list opens,
    creates one with a name, and saves the
    one shown, marked while it has unsaved changes; changing sequence with unsaved changes asks
    whether to save them. A track whose property is not declared (its module absent or stopped) is
    shown greyed, its keys kept and saved.

Acceptance:

| Check | Expected result |
|---|---|
| Start the editor | The *Timeline* panel in the bottom area; the cube still, without its speed slider |
| *Add property* | Lists the position, rotation, scale and colour of the cube |
| Add the four tracks, a key on each at frame 0, the playhead at frame 60, other values typed | Keys at frames 0 and 60 on each track |
| Drag the playhead between 0 and 60 | The cube moves, turns, grows or shrinks and changes colour in the 3D view while the playhead is dragged |
| Play, with and without loop; Pause | The animation plays at the frame rate, starts again or stops at the end; Pause stops it |
| Move a key, delete a key, select keys with a box and move them | Each action one undo entry; Ctrl+Z puts it back and the cube shows the value at the playhead |
| Move the playhead, play | Nothing enters the history |
| Paint the cube from its panel, without moving the playhead | The colour stays until the playhead moves |
| Save, restart, choose the sequence | Keys, frame rate and length as saved; the file is readable JSON in `sequences\` |
| Change sequence with unsaved changes | The panel asks whether to save them |
| Disable `sample-cube`, restart | The timeline starts; the cube's tracks greyed, their keys kept when saving |
| Tests, `cargo xtask check`, CI | Green |

As built:

- **Contract**: `Registrar::animatable` declares a property; `Context` and `Editor` list, read and
  write them (`properties`, `read_property`, `write_property`), with typed values
  (`PropertyValue`); a value of the wrong type is refused, and each number is kept within the
  range. A panic in a property's function makes its module fail, as in a command.
- **Cube**: position from -100 to 100, rotation from -3600 to 3600 degrees on each axis. Its normals
  go through the inverse transpose of its transform, so that its lighting stays right when its scale
  differs between axes. Its numbers are read back as they were written (0.15, not
  0.15000000596046448).
- **Interpolation**: cubic between two keys with the slopes of Fritsch and Butland, flat at the
  first and last keys and at each peak or trough.
- **Panel**: a track's label is the property's; its value fields show the value at the playhead,
  or the property's current value while the track has no keys. The playhead shows its frame in the
  ruler, and the frame and time (`2:15`) beside it.
- **File**: JSON with each track's property first and one key per line.
- **Limits**: until step 8.6b, the playhead moved and the values were written only while the
  *Timeline* panel was shown; since then, the kernel moves the players adopted at each frame, the
  panel shown or not.

Complement asked before validation (done): a module of the interface shows modal windows for
the other modules, as `QDialog`. The module that opens one gives its text and its buttons, and does
what the button chosen calls for. Changes that would be lost are asked about this way.

- **`Dialog` in the Qt of the core**, as `QDialog`, for every language: a floating window, modal:
  while it is shown, the rest of the editor takes neither clicks nor shortcuts (Ctrl+Z included).
  It has a title and holds one layout; it is created hidden, shown and hidden by its module
  (`show`, `hide`). Its signal `rejected` tells that the user closed it, with Escape or its close
  button, which hides it. `uniwow.h` keeps version 3: it gains the kind `UNIWOW_DIALOG` and the
  signal `UNIWOW_SIGNAL_REJECTED`; `uniwow.hpp` and `UniWoW.cs` gain the class `Dialog`.
- **`modules/UI/`**: the modules of the interface, now and to come, in `modules/UI/<id>/` in the
  repository and in `modules\UI\<id>\` beside the executable, loaded by the kernel as the others.
  An id stays unique across the folders.
- **Module `dialogs`** in `modules/UI/dialogs/` (Rust), built with the Qt objects of the core, for
  any module or script: the named command `ui.dialog` opens a modal window with the title, the
  text and the buttons its caller gives (each with an id), and the button that Escape stands for;
  it answers with the window's number. When the user clicks a button, or presses Escape, the window
  closes and the module publishes the event `ui.dialog_answered`, with the window's number and the
  button's id: the module that opened the window does what it decided for that button. Windows
  asked for while one is shown wait their turn. A window that cannot be built is answered at once
  with the button Escape stands for, so that its module waits no more.
- **Unsaved changes, a contract of the core**: a Rust module tells which of its documents have
  unsaved changes and saves them on request (compiled, Lua and Python modules in a later
  milestone). When the editor is closed while modules have some, the kernel opens a window through
  `ui.dialog` that lists them, with *Save*, *Don't save* and *Cancel*: *Save* has each module save,
  then closes, unless a save fails, which is shown; *Don't save* closes; *Cancel*, or Escape, keeps
  the editor open. Without the module `dialogs`, the editor closes without a question and the
  changes are lost.
- **Timeline**: changing sequence with unsaved changes opens a window through `ui.dialog`
  (*Save*, *Don't save*, *Cancel*) and does what the answer says, or changes without asking when
  the command does not exist; the question inside the panel goes. Its unsaved documents are its
  sequences with unsaved changes.
- **Sample script** `scripts\lua-5.1\samples\dialog.lua`: opens a window with a text and buttons of
  its own, then prints the button chosen.

Acceptance of the complement:

| Check | Expected result |
|---|---|
| Run `samples/dialog.lua` | The window shows the script's title, text and buttons; nothing else in the editor answers until one is chosen; the script prints that button |
| Change sequence with unsaved changes | A window lists the sequence; *Save*, *Don't save* and *Cancel* do what they say |
| Close the editor with unsaved changes | A window lists them: *Save* saves then closes, *Don't save* closes, *Cancel* keeps the editor open |
| Escape in a window | The button its caller chose for Escape (*Cancel* in both cases above) |
| Ctrl+Z while a window is shown | Nothing is undone |
| Remove `modules\UI\dialogs`, restart | Changing sequence and closing the editor no longer ask: unsaved changes are lost |
| Tests, `cargo xtask check`, CI | Green |

As built:

- **Contract**: `Module::windows_ui` draws a module's floating windows at every frame;
  `Module::unsaved` and `Module::save_unsaved` tell and save its unsaved changes. The names
  `DIALOG_COMMAND` and `DIALOG_ANSWERED_TOPIC` are in `uniwow_api`.
- **Dialog**: drawn as a modal window of egui, its title and a close button above its layout;
  Escape closes the window on top only. A Rust module may build with the objects of the core: the
  module `dialogs` runs their slots once its windows are drawn, on the interface thread.
- **Closing**: without the module `dialogs`, the log says which unsaved changes were lost.
- **`modules/UI/`**: the workspace takes `modules/[!U]*` and `modules/UI/*` (module ids are
  lowercase); the kernel loads the modules of the group folder `modules\UI\`; `cargo xtask build`
  deploys there, and removes a deployed module whose folder no longer matches its source.

### Milestone 7: the Curves view of the Timeline (done)

As the Curves view of the Animation window of Unity: every number of an animated property is a
curve of its own, whose keys and tangents are edited by hand. A module of the interface draws and
edits the curves; the Timeline uses it, and every language can use it through the Qt of the core.
The Montage mode follows in milestone 8.

Content:

- **Curves, in the core** (`uniwow_api`), shared by the module that edits them and by the modules
  that evaluate them. A curve is a list of keys, each with a time, a value and, on each side, a
  tangent (a slope) and a weight. The tangent mode of a key, as in Unity:

  | Mode | Tangents |
  |---|---|
  | Clamped Auto (by default) | Smooth, never overshooting the neighbouring keys (the slopes of milestone 6) |
  | Auto | Smooth; may overshoot |
  | Free Smooth | Set by hand, the same slope on both sides |
  | Flat | Horizontal |
  | Broken | Each side on its own: Free (set by hand), Linear (pointing at the neighbouring key), Constant (the value holds until the next key) |

  Each side may also be weighted: the length of its handle, a share of the time to the
  neighbouring key (a third by default), shapes the curve. The core computes the automatic
  tangents again after each change, and evaluates a curve at any time: a cubic between two keys,
  in time and value when a side is weighted.
- **Keys of their own for each number**: each number of a property (x, y and z of a position; red,
  green and blue of a colour) has its own curve and its own keys, as in Unity. In the dopesheet, a
  property's row shows a diamond wherever one of its numbers has a key, and unfolds into one row
  per number; moving or deleting a diamond of the property's row moves or deletes the keys of
  every number at that frame. Editing a number at the playhead sets a key on its curve only; the
  key button sets one on each number. Sequence files gain a version: version 2 holds the curves
  with their tangents; the files of milestone 6 are still read.
- **Module `curves`** in `modules/UI/curves/` (Rust), the curve editor: a graph of times and
  values, with its grid and rulers, the curves in their colours and the playhead. Keys are
  selected by clicking or with a box, moved in time (by whole frames when its caller asks) and in
  value; the handles of the selected keys' tangents are dragged; a right click on a key chooses
  its mode (the modes above, then Left, Right or Both tangents: Free, Linear, Constant, Weighted);
  Delete removes keys; a double click on a curve adds a key; the wheel zooms (with Ctrl the time
  only, through the zoom egui makes of Ctrl and the wheel; with Shift the values only), the middle
  button scrolls, F frames the selected keys, or all of them. Keys changed elsewhere end the
  gesture and the menu under way; the keys selected are found again by their time, and let go
  when they are no more. It tells its caller that the curves changed, while a drag goes on and
  when it ends, so that the caller records one undo entry per change; told that an editor's view
  is gone, it forgets what it kept of it. It is offered:
  - to Rust modules, as a service the Timeline uses in its panel;
  - to every language, as an object of the Qt of the core, `CurveView`, as a `QWidget`: its curves
    are set and read as JSON (the property `CURVES` through `set_text` and `text`), and its signal
    `curvesChanged` gives them after each change the user makes, with whether the change is
    finished. The core has the module `curves` draw it.

  Without the module `curves`, a `CurveView` and the Curves view of the Timeline say that the
  module is not running; the dopesheet works as before.
- **Timeline**: the buttons *Dopesheet* and *Curves* switch the right side of the panel; on the
  left, each number of a property has its colour and can be shown or hidden in the Curves view.
  Each change of the curves is one undo entry; the cube follows the curves at the playhead.
- **Sample**: the C# panel *Counter* gains a `CurveView` with a curve of its own, each change one
  undo entry, so that a module of another language uses the curve editor.

Acceptance:

| Check | Expected result |
|---|---|
| *Curves*, with the cube's tracks of milestone 6 | The curves of x, y and z of the position, rotation and scale, and of red, green and blue of the colour, in their colours, through their keys |
| Drag a key in value, then in time | The curve follows; the cube shows the value at the playhead; one undo entry for each drag; the keys stay on whole frames |
| Right click a key: Auto, Clamped Auto, Flat, Free Smooth, Broken | The curve changes as in Unity: Auto may overshoot, Clamped Auto never does; the handles of Free Smooth move together, those of Broken each on its own |
| Both tangents: Constant, then Linear; then Weighted, a handle lengthened | Steps; straight segments; a curve shaped by the handle's length |
| Double click on the curve of x | A key on x only: the dopesheet shows it on the row of x and on the position's row |
| Delete on the position's row of the dopesheet | The keys of x, y and z at that frame go |
| Open a sequence saved by milestone 6, then save it | Same animation; written in version 2 |
| The curve of the C# panel: drag a key, then Ctrl+Z | The curve changes, then comes back |
| Remove `modules\UI\curves`, restart | The Curves view and the curve of the C# panel say that the module is not running; the dopesheet works |
| Tests, `cargo xtask check`, CI | Green |

As built:

- **Curves** are in `uniwow_api::curve`: a handle that is not weighted has a third of the time to
  the neighbouring key; Auto and Clamped Auto are flat at the first and last keys. A segment
  without weights is the cubic of Hermite; with a weight, a Bézier curve whose time is found by
  bisection.
- **Contract of the curve editor**: the trait `CurveEditor` and the service `curve-editor`. Its
  caller keeps the time axis, so that the graph of the Timeline lines up with its ruler, and gives
  the snap, the playhead and the span to shade; the editor answers whether a change goes on or is
  done.
- **Module `curves`**: the handles of the selected keys are drawn, at the length of their weight,
  or 40 pixels from a key without neighbour on that side; dragging the handle of a key set
  automatically makes it Free Smooth; a dragged key cannot pass a key that does not move; the
  values are written inside the graph, along its left edge.
- **Timeline**: a property's row has a button unfolding the rows of its numbers; in the Curves
  view each row has a box showing or hiding its curves; a boolean has no curve there, its value
  holding from key to key. Delete in the Curves view is the curve editor's.
- **`CurveView`**: drawn by the core through the service; `curvesChanged` is sent while a key or a
  handle is dragged, with the change not finished, then once it is.

### Sixth review (milestones 5 to 7)

- **Lighter runtime**: the C interface (`capi`) and the drawing of the interface objects moved
  from `core/api` to `core/kernel`; the runtime went from about 5,800 to 2,900 lines, and changing
  either no longer changes its fingerprint. Exported symbols: 18,364 before, 18,350 after in debug;
  16,559 before, 16,546 after in release. Nearly all of them come from the crates the runtime
  re-exports (egui, wgpu…), not from its own code.
- **Checks without a window**: the tests run the shell as eframe does, `logic` then `ui`, or
  `logic` alone while minimised, on input they make up and with modules they define
  (`core/kernel/src/shell/tests.rs`): Undo and Redo from the menu and the keyboard, a group as one
  entry, closing with unsaved changes shown and minimised, without a window to ask in, a module
  failing, a modal window, a compiled module's command and its work on its thread, a document
  closed without saving. They found that a modal window was known one frame late: the kernel now
  tells it from the frame it draws it in.
- **One source for the numbers**: `sdk/bindings.toml` (section 8).

### Milestone 8: parity of the languages (built)

Whatever a built-in module can do, a module in any language can do (R9). This milestone completes
the unified API where it falls short, and splits the Timeline into an engine, widgets and a window
using them, so that a module of another language builds a timeline of its own on its own
properties. Lua and Python reach all of it in milestones 10 and 11, through the same API.

What the unified API lacks today:

| Capability | Built-in (Rust) modules | Unified API |
|---|---|---|
| Animatable properties: declare, list, read, write | `Registrar::animatable`, `Editor` | Nothing |
| Sequences, their evaluation, their playback | Inside the Timeline module | Nothing |
| Dopesheet and tracks | Inside the Timeline's panel | Nothing; only `CurveView` |
| Tree, table, property grid | egui | Nothing |
| The viewport's camera | Inside the viewport module | Nothing |

Specified and validated by the user, with the adjustments of the seventh review. It is built in
seven small steps, each checked and reviewed before the next one.

**Step 8.1, the rule.** R9 in section 1, and in section 3 a table *Capabilities and their unified
form*: each capability a built-in module offers, with the object, property, command or function
that gives it to every language, and the classes of each language. Every later milestone keeps the
table up to date, and its review checks it.

**Step 8.2, animatable properties for every language** (`uniwow.h` version 4):

- A module declares its properties in `uniwow_module_info`, in a table as for its commands: name,
  label, kind (number, vector, colour, boolean; their numbers in `sdk/bindings.toml`), range,
  initial value, and a `write` function called on the module's thread.
- The kernel keeps the value of each declared property: a read never waits for the module, so that
  the Timeline, a property grid or another module reads it from any thread without holding the
  interface. The module tells its own changes with `set_property`, and readers see them. A write
  from elsewhere (playback, a property grid, another module) is kept within the range, stored, then
  handed to `write` on the module's thread, in order. Otherwise as in Rust: the path is
  `<module>/<name>`, a property is listed only while its module runs, and a failing `write` makes
  the module fail.
- When `write` refuses or adjusts a value, it gives back the value it kept, and the kernel's copy
  follows; a module may also tell it later with `set_property`.
- The writes of one property still waiting for the module's thread are merged, the last one
  winning: playback writing at each frame never fills the queue of a slow module.
- A value changed by hand (a property grid) is one undo entry the kernel records, owned by the
  property's module (section 3, undo of the state the kernel keeps); a write by playback is none.
- Functions: `properties` lists them as JSON (path, owner, label, kind, range); `read_property` and
  `write_property` read and write the numbers of one. Classes: C++ `uniwow::Property` and
  `uniwow::properties()`, C# `Editor.DeclareProperty`, `Editor.Properties`, `ReadProperty`,
  `WriteProperty`.
- A module built with version 3 of the header is refused with the reason, as for every version.
- Samples: the value of the C# *Counter* and the position of the first card of the C++ scene become
  animatable; the Timeline animates them as it animates the cube.

  As built:

  - `uniwow_property` holds name, label, kind (`UNIWOW_VALUE_NUMBER`, `VECTOR`, `COLOUR`,
    `BOOLEAN`, generated from `sdk/bindings.toml` with `PropertyKind` and C#'s `ValueKind`), range,
    initial value, `write` and its `user`; `uniwow_module_info` ends with `properties`,
    `property_count` and `property_size`.
  - `write` receives the numbers written in a buffer it may change: what it leaves there is the value
    the module keeps, which the kernel's copy then takes, unless a newer write already waits; a value
    kept that is not finite is refused with a warning, the one before staying. Its failure is
    reported with the property's name and the module's message.
  - A write records nothing: it neither blocks Undo nor counts as the module's work, and
    `record_change`, `begin_group` and `end_group` are refused while the write function runs (*a
    property's write records nothing: the kernel records a value changed by hand*). Undo, which does
    not wait for writes, would otherwise cross a change recorded there. Since step 8.4, the module's
    thread marks every job that records nothing, the slots of `timeChanged` as well (*nothing is
    recorded in a property's write or a player's timeChanged, which Undo does not wait for*).
  - `set_property` refuses a count that is not the kind's, and numbers that are not finite. Since
    the full verification of the milestones, a property whose name is empty or holds a '/' or a
    space, or whose range goes down, is refused as soon as the module declares it, as in Rust, the
    reason in the log: `set_property` refuses it too, rather than keep a value nobody reads.
  - C++ also has `describeProperties(info)`, `readProperty` and `writeProperty`, and
    `Property::set`; C# also has `Editor.SetProperty`.
  - The C# *Counter* keeps whole values from 0 to 100, and has *Fail to write the value* to check
    a failing `write`; the scene's first card keeps a z of 0.
  - The undo of a value changed by hand comes with the property grid of step 8.7, the first place
    where a value is changed by hand: in the Timeline, the fields set keys, which the Timeline's own
    history keeps.

**Step 8.3, the viewport's camera:**

- Animatable properties `viewport/camera_position` and `viewport/camera_target` (vectors), and
  `viewport/camera_fov` (a number, in degrees): the Timeline animates the camera as it animates the
  cube.
- Commands `viewport.camera` (its position, target and angle), `viewport.look_at` (a position, a
  target, an angle if given) and `viewport.frame` (fits a box).
- Other 3D access, proposed for later milestones and not built here: picking
  (`viewport.pick { x, y }`, the object and point hit), layers drawn by other languages (meshes
  given through the API), the selection shown in 3D, handles to move objects.

  As built:

  - The camera is the orbit of the 3D view, shared by its panel, its properties and its commands,
    which may come from any thread. A position or a target given keeps the other one; the camera
    then stays within its orbit: at most 1.5 radians above or below the ground, from 0.5 to 100,000
    units from its target. The angle of view goes from 1 to 170 degrees (45 at start), one given
    beyond held at the nearest end, as an orbit is, where a position or a target out of reach is
    refused. The target
    stays within 100,000 of the origin on each axis and the eye within 200,000, as far again as its
    distance to the target: a position read can always be written back.
  - The commands run on the calling thread and give the camera as `{ "position", "target", "fov" }`;
    `viewport.look_at` and `viewport.frame` refuse what is not three finite numbers within reach, and
    a box upside down. `viewport.frame` fits the box within the narrower of the two angles of view,
    the vertical one or the horizontal one, from the width over height of the view last drawn.
  - Reverse Z with no far plane: the depth is 1 at the near plane (0.1) and falls towards 0 at
    infinity, where the view clears it; the layers compare depths with `Target::depth_compare`
    (`Greater`). A point 50,000 units away is drawn, and the precision stays near the eye. The
    review of this step found the former far plane, at 5,000 units, cutting what the camera frames.
  - The recette found the 3D view locking its camera twice in one frame, which froze the editor: it
    is locked once, and a test keeps it so.

**Step 8.4, the engine of sequences, in the API:**

- `uniwow_api::sequence`: the model of sequences, today inside the Timeline module, moves to the
  runtime beside the curves. A sequence has a frame rate, a length and tracks; a track animates one
  property with one curve per number; evaluating a sequence at a time gives each track's value. Its
  JSON is that of the version 2 files.
- Two kinds of objects of the Qt of the core, for every language:
  - `Sequence`: its tracks as JSON (the property `TRACKS`, through `set_text` and `text`), its
    frame rate and length;
  - `Player`, as `QTimeLine`: the sequence it plays (`SEQUENCE`, a handle), its time in frames
    (`TIME`, fractional), `PLAYING`, `LOOP`, `SPEED`; signals `timeChanged` and `finished`. While it
    plays, the kernel moves it on at each frame by the time elapsed. Whenever its time changes,
    played or set by its module, the kernel writes the value of each track at that time into its
    property, through the catalogue of step 8.2, and nothing where a value did not change.
- Like every object, a sequence and its player belong to the module that created them; a module may
  animate any property of the catalogue: its own, the cube's, the camera's. The kernel records the
  undo entry of each change of a sequence's `TRACKS`, set by its module or made in a widget, owned
  by that module and joining its open group (section 3): the module records nothing itself.
- Sample: the C++ scene plays a sequence of its own on its first card, which moves; pause and loop
  from its panel.

  As built:

  - `uniwow_api::sequence` holds the model the Timeline used, which the Timeline now reads from
    there. `tracks_to_json` and `tracks_from_json` give the tracks of a sequence alone, with the
    rules of a file: keys at whole frames from 0, one track per property.
  - Kinds 24 and 25 (`SEQUENCE`, `PLAYER`), properties 32 to 39 (`TRACKS`, `FRAME_RATE`,
    `LENGTH`, `SEQUENCE`, `TIME`, `PLAYING`, `LOOP`, `SPEED`) and signals 20 and 21
    (`TIME_CHANGED`, `FINISHED`) in `sdk/bindings.toml`; `uniwow.h` keeps version 4 and its table.
    A frame rate is a whole number from 1 to 240, a length from 1 to 1,000,000 frames; a player's
    time stays within the length of its sequence and its speed from 0 to 100; a player plays a
    sequence of its own module, and a destroyed sequence leaves its players without one. Playing
    from the end of the sequence starts from 0.
  - The kernel moves on the players of every module whose objects it adopted: a compiled module's
    when it starts, a Rust module's through `Context::adopt_objects`. The first frame a player plays
    in counts no time, so that a player started while the editor is idle does not jump, and a player
    at speed 0 asks for no frame.
  - `timeChanged` records nothing: it neither blocks Undo nor shows the module busy, and
    `record_change`, the undo groups and the changes of tracks are refused in its slots, as in a
    property's write. While the `timeChanged` of a player still waits for the module's thread, the
    next one only changes its time, whatever was posted since: one at most waits per player.
    `finished` is counted, as the other signals are. The review of this step found `timeChanged`
    counted, which greyed out Undo while a player played, and merged only when nothing else was
    posted in between.
  - The values are written whenever the player's time changes, and also when its sequence changes:
    the values at the playhead follow the keys, as in the Timeline. Where its time or its sequence
    changed, every track is written, so that a value changed elsewhere gives way to the track once
    the playhead moves; where only the keys changed, the playhead still, only the values that
    changed since the player last wrote. A track whose property no running module declares, or
    of another kind, is told once per player in the log.
  - Each change of `TRACKS` is one undo entry, *edit a sequence*, owned by the module and in its
    open group; Undo and Redo set the tracks back without recording. Tracks set while the module
    starts, before the editor is ready, and tracks unchanged, are not recorded. While a write
    function of a property runs, a change of tracks is refused and not made (the rule of step 8.2).
  - The C++ scene's panel has *Play*, *Pause*, *Loop* and the frame played, then *Finished*; its
    first card goes down, right and back in 3 seconds.

**Step 8.5, the dopesheet as an object:**

- `DopesheetView`, as a `QWidget`, drawn by a new module of the interface, `modules/UI/dopesheet/`,
  through a service, as `CurveView` is drawn by `curves`: the rows of a `Sequence` (one per track,
  unfolding into one per number), the ruler, the keys as diamonds and the playhead of a `Player`;
  keys selected, moved and deleted, the playhead moved; on the left, each track's label and value
  at the playhead. Signals `keysChanged` (the tracks as JSON, with whether the change is finished,
  as `curvesChanged`) and `playheadMoved`. Without the module `dopesheet`, a `DopesheetView` says
  that it is not running.
- Shown with a `Sequence`, a `DopesheetView` or a `CurveView` changes it directly, and the kernel
  records each finished change as one undo entry, owned by the sequence's module.
- The widgets drawn by modules of the interface through a service, `DopesheetView` and
  `PropertyGrid` as `CurveView`, have the protections of `CurveView` from the start: their data
  checked when given (finite numbers within 1e9, keys in time order and at times of their own),
  every loop bounded, the drawing inside `catch_unwind` with the provider of the service reported
  as the culprit (F5), and a gesture ended when its data change outside it.
- Sample: the C# *Counter* shows a dopesheet of its own sequence on its value; each change of keys
  is one undo entry, without the module recording anything.

  As built:

  - Kind 26 (`DOPESHEET_VIEW`), property 40 (`PLAYER`, the player whose time a view shows as its
    playhead) and signals 22 and 23 (`KEYS_CHANGED`, `PLAYHEAD_MOVED`); `SEQUENCE` also names the
    sequence a dopesheet view or a curve view shows, and `TITLE` the row of every key.
  - The module `modules/UI/dopesheet` offers the service `dopesheet` (`uniwow_api::dopesheet`),
    taken from the Timeline's dopesheet: a row of every key, one per module, one per track unfolding
    into one per number; on the left, each track's label and its values at the playhead, read only;
    keys selected by a click, Ctrl+click or a box, moved by dragging, deleted with Delete; the wheel
    zooms the time, the middle button scrolls it; a press on the ruler moves the playhead to a whole
    frame. Rust modules may use the service as they use the curve editor's.
  - The kernel draws a `DopesheetView` with the service, the sequence and the player's time it is
    shown with, and the labels and values of the catalogue. A drag shows the keys moved while it
    goes on (`keysChanged`, not done) and changes the sequence when it ends; Delete at once. The
    kernel makes each change done to the sequence as one undo entry, *move keys* or *delete keys*,
    owned by the sequence's module, which records nothing; tracks that break the rules of a file are
    refused. The playhead moved pauses the player at that frame, whose values the kernel then
    writes; it is no change of the history.
  - A `CurveView` shown with a sequence shows one curve per number of its tracks, keys at whole
    frames within the sequence, the player's time as its playhead. A change under way is drawn from
    a copy; the sequence changes, as one undo entry *edit curves*, when it is done. Its own `CURVES`
    are then not shown, and it sends `keysChanged` instead of `curvesChanged`.
  - A gesture of the dopesheet ends when the tracks change outside it, without changing a key: a
    key removed by the module during a drag ends the drag, and the module's change stands. A panic
    of the dopesheet or of the curve editor while drawing makes its provider fail (F5); the labels
    of the ruler are bounded.
  - The C# *Counter* shows a dopesheet of a sequence of its own on its value, with a player whose
    playhead, moved on the ruler, sets the counter; it records nothing for its keys.
  - After the review: the labels and values of the properties a view shows are read before the
    objects are locked, reading a property running its module's code; the tracks of `keysChanged`
    are made only for a slot connected; the dopesheet forgets a view once it is gone.
  - Left for step 8.6, which the Timeline has today and its views do not: the values on the left
    edited by hand to set keys, a key added at the playhead, a track added or removed, the curves of
    the Curves view hidden one by one.

**Step 8.6, the Timeline, a client:** the Timeline module keeps its egui window (sequence and
playback bars, *Add property*), but its sequences are `Sequence` objects, its playback a `Player`,
its dopesheet a `DopesheetView` and its Curves view a `CurveView`; no sequence logic remains in the
module. For the user nothing changes: files, undo, unsaved changes, as in milestones 6 and 7.

Built in two parts, each reviewed. The user chose that the Curves view be a `CurveView` with, on
its left, the rows of the dopesheet.

As built, first part (8.6a), the views doing what the Timeline does by hand:

- The left of a dopesheet's rows, taken from the Timeline: each track's value at the playhead in a
  field per number (a box for a boolean, a swatch for a colour), kept within the property's range;
  a value set is a key at the playhead, which pauses there, one undo entry *set a key of <label>*
  for a value dragged or typed; a button adds a key at the playhead with the property's value,
  *add a key to <label>*; another removes the track, *remove <label>*. The fields of a property no
  running module declares are greyed. `RowProperty` gives the property's current value and range.
- A `CurveView` showing a sequence has the rows of the dopesheet on its left, through
  `Dopesheet::curve_properties`, the kernel drawing both services side by side; a box on each row
  shows or hides the curves of its numbers. The curves of a boolean, which holds its value from key
  to key, are not shown.
- A change under way now goes to the sequence at once, as in the Timeline, so that what the
  sequence animates follows it: the kernel sets the tracks without recording
  (`Ui::set_tracks_under_way`), then records one entry when the change is done, from the tracks
  before it began (`Ui::finish_tracks`). A change dropped, nothing being done by the user any more,
  is undone; one the tracks changed under, elsewhere, starts again from them. Step 8.5 showed a
  change under way in the view alone.
- After the review: a change under way is put back when its view goes or is drawn without its
  service; the tracks the view set, not the version of the sequence, tell a change made elsewhere,
  so that a change of the length during a drag leaves where it began; the properties the views
  show are read once a frame for a module, whatever panels and dialogs are drawn.

As built, second part (8.6b), the Timeline a client:

- The Timeline keeps its files, its bars and its question about unsaved changes. Each sequence
  opened is a `Sequence` object, made with its content (`Ui::create_sequence`, which records
  nothing) and titled with its name; the playback bar drives a `Player`; the dopesheet and the
  Curves view are a `DopesheetView` and a `CurveView`, which the Timeline hands to the kernel
  (`Context::adopt_objects`) and draws with `Context::draw_objects`. Its own dopesheet, curves and
  playback are gone, and the module holds no sequence logic.
- A sequence is unsaved while its object differs from what its file holds: undoing back to it
  marks it saved again.
- The changes of keys and tracks are recorded by the kernel; *Add property* is one, *add <label>*.
  The frame rate and the length, which the kernel does not record, stay undoable by a command of
  the Timeline, one entry for a field dragged. The undo entries lose their prefix *timeline:*.
- The title of a sequence names the document its changes belong to (`AppliedChange::document`): a
  sequence left without saving is destroyed, and `Context::forget_document` forgets the kernel's
  entries of its tracks with the Timeline's own.
- After the review, for nothing to change for the user: the curve editor, shown with a playhead,
  has a ruler above the curves, whose press moves the playhead (`CurveOutput::playhead`), the kernel
  pausing the player there and sending `playheadMoved`, for every curve view showing a sequence and
  a player; the views showing the same sequence with the same player share one time axis in the
  kernel, fitted again for a sequence shown again; the button adding a key is greyed, with *a value beyond 1e9 cannot be
  keyed*, when the property's value is beyond the limit of the curves, as is a field showing such a
  value, and a value typed beyond is brought back to the limit.
- At the user's request, as in Unity: a number whose curve has no key, given a value by hand away
  from frame 0, also gets a key at frame 0 holding the value it had before; a number that has a
  key already behaves as before.

**Step 8.7, data widgets**, as in Qt:

- `TreeView` (as `QTreeWidget`), drawn by the kernel: items with a text and children, folded or
  unfolded, one selected; signals `itemClicked`, `currentItemChanged`, `itemExpanded`.
- `TableView` (as `QTableWidget`), drawn by the kernel: columns with headers, rows of cells, cells
  edited in place, sorted by a column when the user clicks its header; signals `cellChanged`
  (row, column, text), `currentCellChanged` and `sortChanged`. Only the rows and the columns in
  sight are drawn, so that 100,000 rows scroll smoothly; columns beyond the width of the view are
  reached by scrolling sideways, the headers with them. A cell being edited that scrolls out of
  sight keeps its text, as leaving its field does; a cell given back with the text it had tells
  nothing. Since the full verification of the milestones, a column to sort by that the table does
  not have is refused, its sort staying, and `SORT_COLUMN` holds the order too, as a second number:
  the classes of C++ and C# set both at once and sort once; the column alone keeps the order.
  Besides the rows given at once, functions change one cell, insert rows and remove rows, without
  giving the whole table again; a removal naming a row the table does not hold is refused, none
  removed. The kernel sorts, keeping the
  id of each row, which the signals and these functions use, whatever the order shown. The SQL
  tool of milestone 12 relies on it.
- `PropertyGrid`, drawn by a new module of the interface, `modules/UI/properties/`, through a
  service: the properties of the catalogue whose paths it is given, each with its label and an
  editor for its kind (numbers dragged or typed, a colour, a box); a change by hand writes the
  property and is one undo entry, *Set <label>*, which the kernel records and which belongs to the
  property's module.
- The rows of a tree or a table are given as JSON (`ITEMS`, `ROWS`), each with an id the signals
  give back.
- Sample: a C# panel with a table of 100,000 rows, a tree, and a property grid of the cube.

Built in two parts, each reviewed: the tree and the table, then the property grid.

As built, first part (8.7a), trees and tables:

- `uniwow.h` goes to version 5: `set_cell`, `insert_rows` and `remove_rows` at the end of the
  table of functions. `ITEMS`, `COLUMNS` and `ROWS` are JSON; the ids of items and rows are whole
  numbers from 1 to 2^53, each of its own, so that every language holds them exactly; a tree holds
  1,000,000 items over 64 levels at most, a table 1,000,000 rows and 1,000 columns at most. A cell
  given as a number is taken as its text.
- The current item or row (`CURRENT_ITEM`) is one the view holds, 0 for none; an item or a row
  removed is no longer current.
- A click on a header sorts by its column from the lowest, a second click from the highest, and so
  on (`SORT_COLUMN`, `SORT_DESCENDING`, `sortChanged`); the module sets them too, -1 for its own
  order, which the table keeps: `ROWS`, and the place `insert_rows` takes, are in the module's
  order. Cells sort as numbers first, by value, then as texts, ignoring their case, rows of equal
  cells keeping the module's order; a cell changed in the column sorted by sorts the rows again.
- A click makes a cell current, a double click edits it in place: Enter or leaving the field keeps
  the text, which the table holds and `cellChanged` gives, Escape drops it; nothing is recorded, as
  in Qt. The field left without a key, its row scrolled away or its panel hidden, keeps the text
  too. The triangle before an item folds or unfolds it, which the tree keeps.
- Trees and tables draw only the rows in sight. What the user did is applied once the kernel lets
  go of its copy of the object drawn, so that a table of 100,000 rows is changed in place rather
  than copied.
- The SDK has `TreeView` and `TableView` in C++ and C#, a `CellEvent` giving a cell's row, column
  and text. The C# sample has a panel *Data*: a table of 100,000 rows (Id, Name, Value), a button
  inserting a row at the top and one removing the current row, a tree, and the last signal received
  above them.
- After the review, each change of rows made where it falls: the rows keep their places, the index
  of the ids is kept up to date rather than made again, a row inserted or a cell changed in the
  column sorted by goes to its place in the order shown by binary search (many rows inserted at
  once are sorted and merged in one pass), and rows are removed in one pass; no change of rows
  sorts every row. Rows of equal cells keep the module's order through ranks with room between
  them, ranked again only when no room is left.
- A table of 50,000 rows or more is sorted whole off the lock and off the interface's thread, on
  the kernel's pool: its former order stays shown, the header saying *sorting…*, until the sort is
  made. A sort whose rows changed meanwhile, or which another sort replaced, is dropped and made
  again. The kernel starts it as it draws the table, so that the changes of the sort between two
  frames make one sort. Limit: a module changing such a table more often than a sort takes keeps
  it in its former order.
- A tree view's rows are made again only when its items, or which are unfolded, change.

Where the work of trees and tables is done:

| Work | Thread | Under the lock of the module's objects |
|---|---|---|
| `ITEMS` and `ROWS` read from JSON, rows made ready (places, ranks, index) | The module's, in `set_text` | No |
| Items or rows set; those replaced freed | The module's, in `set_text` | Set: yes, by exchange; freed: no |
| `set_cell`, `insert_rows`, `remove_rows` | The module's | Yes: a binary search and one move per row, one pass to remove; the first change while a sort of the background runs copies the places it shares with it, once a sort |
| `ROWS` read | The module's, in `text` | Only to take the rows, shared, not copied; the JSON is made after |
| What the user did: a sort asked, a cell kept, an item folded | Interface | Yes, once the kernel let go of its copy of the object |
| A sort of fewer than 50,000 rows | The thread asking for it | Yes |
| A sort of 50,000 rows or more | A worker of the pool | No: the rows are shared with it under the lock, its order taken under it |
| The rows of a tree view made again | Interface, when they change | Yes |
| Drawing | Interface | Yes; only the rows in sight |

As built, second part (8.7b), the property grid:

- `PropertyGrid` (kind 29) shows the properties whose paths `PATHS` gives (property 47, a JSON
  list of 100,000 paths at most), one row each: its label, then a field for its kind, numbers
  dragged or typed, a colour picked, a box ticked; a property no running module declares is
  greyed. `uniwow.h` stays at version 5: no function is added.
- It is drawn by a new module of the interface, `modules/UI/properties/`, through the service
  `property-grid` (`uniwow_api::property_grid`), as the dopesheet is: without the module, the grid
  says that it is not running; a panic of the service is its provider's failure (F5).
- A value being changed, such as a number dragged, is written at once and shown by the grid until
  the change is done; done, it is written and recorded by the kernel as one undo entry, *Set
  <label>*, of the property's module, from the value before the change began; a value left as it
  was records nothing. A change under way is put back when its grid goes, is drawn without its
  service, or when the user does nothing any more without the grid saying it is done; the grid
  says it is done once the user does nothing any more, even when its row left the sight. A change
  of another value ends the one under way. A value that is not of the property's kind, or not
  finite, is not written. The colour picker gives a colour back a little changed even when only
  looked at: only a number it changes by more than 1e-4 counts, the others kept whole.
- The module records nothing, and the grid sends no signal: the property's module receives the
  value in its `write`.
- The SDK has `PropertyGrid` in C++ and C#. The C# sample's panel *Data* shows the cube's position,
  rotation, scale and colour in a grid.

Where the work of the property grid is done:

| Work | Thread | Under the lock of the module's objects |
|---|---|---|
| `PATHS` read from JSON | The module's, in `set_text` | No |
| The rows made and their values read: those in sight and 32 around, the first 64 of a grid not drawn yet, for every grid of the module once any of its panels or dialogs is drawn, its own panel closed or not | Interface, before drawing | No: reading a property runs its module's code, which may lock objects |
| Drawing, and what the user did turned into a change | Interface | Yes; only the rows in sight |
| The values changed written, a change done recorded | Interface, once the drawing is done | No: writing runs the module's code, or hands the value to the module's thread |

Choices confirmed by the review:

- The properties of the modules of other languages are read from the kernel's copy, never by
  calling the module, so that reading never waits on a module's thread.
- The kernel records the undo entries of the state it keeps (section 3), owned by the module of the
  object or the property.
- Players are moved on by the kernel, as part of the objects of the core, so that a module plays its
  sequences without the Timeline module; the Timeline module itself becomes removable without
  taking the engine away.
- `uniwow.h` goes to version 4: the table of functions and `uniwow_module_info` grow at their end.
- Trees and tables receive their rows as JSON rather than as an object per row, for the speed of
  large tables.

Acceptance, automated where possible (the shell run without a window, the tests of the SDK):

| Step | Check | Expected result |
|---|---|---|
| 8.2 | *Add property* in the Timeline | The value of the C# *Counter* and the card's position are listed beside the cube's properties |
| 8.2 | Keys on the Counter's value, then the playhead moved | The Counter's panel shows the value at the playhead; no undo entry while the playhead moves |
| 8.2 | A C# write function that throws | The module fails, with its name and the reason; the editor goes on |
| 8.3 | Keys on the camera's position, then play | The 3D view follows the camera's path |
| 8.3 | `viewport.look_at` from the Commands panel | The camera moves there |
| 8.4 | *Play* in the C++ scene's panel | The first card moves along its sequence; *Pause* stops it; with *Loop*, it starts again |
| 8.5 | A key dragged in the Counter's dopesheet | The key moves; one undo entry, though the module records nothing; Ctrl+Z puts it back |
| 8.5 | A key removed by the module while the user drags it | The drag ends; no key changes but those the user moved |
| 8.6 | The checks of milestones 6 and 7 | The same results |
| 8.7 | The table of 100,000 rows scrolled, sorted, a cell edited | Smooth; sorted by the column clicked; `cellChanged` received with the row's id |
| 8.7 | A row inserted, then one removed, by the module | The table changes there only, its order kept |
| 8.7 | The cube's colour changed in the property grid | The cube changes; one undo entry |
| All | Tests, `cargo xtask check`, the tests of the SDK, CI | Green |

### Milestone 9: live view of the world (validated)

*Written by the external review from the user's request, brought up to date with milestone 8 as
built, then completed and corrected against the code by the instance; validated by the user as
merged (5a77086).*

The world of WoW as it runs now on the AzerothCore server, shown in the editor: a map with its
terrain, buildings, doodads and water, and the creatures, NPCs, game objects and players where they
are, in real time. The editor is not a game client: there is **no action of the game**, and no
character connected. In this milestone the only thing the user does is move the camera.

The editor will later edit this world: sculpt the terrain, select objects and creatures, add,
remove and move them, paint vertex colours, areas, holes and textures. This milestone builds none of
it, but its design must not stand in the way: *Preparing the editing to come*, below, records the
choices made here for that.

Decisions of the user:

- **The live state comes from an observer module of the server**, `mod-uniwow-observer`, written in
  C++ for AzerothCore. Not a ghost client speaking the protocol of the game, not the real client
  shown in the editor: both would need a connected character.
- **The first delivery is the whole scene**: terrain, animated models, buildings, doodads and water.
  It is built in small steps, each reviewed before the next one.
- **It comes after milestone 8**; the outlined milestones that followed move one number on.
- **The server runs on the same machine as the editor**: the observer listens on 127.0.0.1 only,
  and asks for a token.
- **Only WotLK 3.3.5a is targeted, with WarcraftXL**: the player's client loads, through the
  extensions of WarcraftXL, the modern files exported from the retail game with wow.export, without
  conversion. The view shows them **from this milestone on**, as the client does.
- **The readers of the files**: those of 3.3.5a copied from warcraft-rs, the modern ones translated
  from wow.export, inside the module `assets` (*Components*).
- **The editor keeps the allocator of Windows, and `assets` reads economically** (after the
  verifications of step 9.1): with the allocator of Windows, reading the archives from more than 8
  threads at once gets slower, where mimalloc kept scaling (*Results of the verifications*, below);
  but mimalloc cannot be the allocator of the editor (section 2). `assets` allocates the data of a
  file once, at its size, and reuses its buffers thread by thread. Measured in step 9.1, it scales
  to 32 threads (*As built*, below): mimalloc is not needed.

Rules:

| Id | Rule |
|---|---|
| L1 | No action of the game, ever: the editor never plays, and never connects as a player. |
| L2 | Nothing is changed in this milestone: the observer offers no write; the database and the files of the client are not touched; the live view records no undo entry. The only interaction is the camera. The observer does have one effect on the running server, which is no change of its data: it keeps the zone looked at alive (below), which costs the server some work and lets its creatures move where no player is. |
| L3 | The work is spread over the threads of the editor as *Threads* below sets out: reading, decoding, building meshes and uploading them run on every core, the network and the animation on threads of their own; the interface thread only hands over what is ready and draws it (R7, T1 to T5). |
| L4 | The live entities are offered to every language (R9): commands that read, such as *the entities around a point*, run on the calling thread from a snapshot shared between threads and answer at once (T4); events are batched, one per update of the server listing what appeared, left or moved, never one per entity. The table of capabilities is kept up to date. |
| L5 | The core names none of these modules (R1). |

#### Components

**The formats and archives of WoW, behind the module `assets`** (decision of the user): their code
will change often for a long time, and in the runtime each fix would change its fingerprint and
make every Rust module be rebuilt, while making the runtime export more. So they live in a module:

- `assets` reads two families of files (decisions of the user):
  - **the files of the 3.3.5a client**: the chain of archives in the order of the patches (MPQ), the
    DBC with the layouts of build 12340 (from WoWDBDefs, its licence to check), WDT, ADT, WMO, M2
    and BLP. They are read with the parts of warcraft-rs (MIT/Apache) the milestone uses;
  - **the modern files WarcraftXL makes the client load directly**, without conversion: the M2 of
    recent versions (chunked, with their `.skin`, `.anim` and `.bone`), the modern WMO, the split
    ADT, and two DB2 tables that turn a FileDataID into the path of its file. The user exports them
    from the retail game with wow.export and the client loads them through the extensions of
    WarcraftXL (`wxl-modern-m2`, `wxl-modern-wmo`, `wxl-modern-adt`, `wxl-db2`), installed with
    wxl-hub. The editor reads the same files from the same places, so that what the client shows,
    the view shows. Their readers are written from those of wow.export (MIT, JavaScript),
    translated with its notice. The BLP did not change and keep one reader.
  - Read only in this milestone. What can also be **written** correctly is recorded format by
    format: the terrain and the placement will need it.
- WarcraftXL is GPL-3: nothing of its code is copied into UniWoW, whose own licence is still to
  choose (section 10). It stays a dependency of the player's client, not of the editor's code.
- It offers the services `vfs` (the bytes of a file, whether it exists, the files under a folder,
  and a modern file by its FileDataID, through the same two tables as WarcraftXL) and `formats` (a tile, a model, a texture, the rows of a DBC, parsed), shared between threads
  (T3). Their interfaces, and the plain data they return (heights, layers and alpha maps of a chunk,
  vertices, bones and keyframes of a model, the levels of a texture), are declared in `core/api`:
  they change far less often than the code reading the files.
- The rule of section 2 stays as the user chose it: a module depends on `uniwow-api` and `libs/*`
  only. `assets` therefore holds its own copy of the parts of warcraft-rs it uses and its
  translation of the readers of wow.export, each with its licence, its authors and the version or
  commit it comes from (for instance in `modules/assets/THIRD_PARTY.md`); their updates are carried
  over by hand. The copy drops rayon, a second pool of threads that wow-mpq brings without a way to
  avoid it: the parallel work goes through the kernel's pool (`Context::spawn`, `parallel_for`), and
  the copied code uses the crates `uniwow-api` re-exports (`log`, `glam`, `serde`, `serde_json`,
  `bytemuck`) rather than versions of its own (decision of the user, after the review found that
  every crate of warcraft-rs shares crates with the runtime). A crate the copy needs that the runtime
  does not offer, such as the decompressions of the archives (zlib and bzip2 at least), is added to
  the runtime as section 2 says, in step 9.1: the runtime's fingerprint changes once, then.
- `libs/server-link` (in the runtime) holds the client of the observer: its protocol is small and
  changes with its version only.

**Installations** (part of the open question of section 10): the folder of the client, and the
observer's port and token, in the settings; a clear message when one is missing or wrong.

**The observer, `server/mod-uniwow-observer/`** (C++, AzerothCore, outside the Cargo workspace,
built with the AzerothCore source tree; how to build it is documented):

- **Handshake**: a version of the protocol, the token, and a list of capabilities. This milestone
  offers reading only; the list lets a protected capability of editing be added later without
  breaking the protocol.
- **Subscription**: a map, the id of its instance for a dungeon or a battleground, and a zone (a
  centre and a radius), moved with the camera.
- **Snapshot, then changes** 10 to 20 times a second. For each object:
  - its GUID at run time and its type: creature, game object, player;
  - its entry, and **the id of its spawn in the database** (the guid of the tables `creature` and
    `gameobject`), with whether it is permanent or temporary (summoned, spawned by a script), and
    the pool or the game event the spawn belongs to, if any;
  - its phase mask: two objects may stand at the same place in different phases;
  - its display id, position, orientation and scale;
  - its movement: standing, walking, running, flying, following a path (the spline when there is
    one);
  - its name.
- **The zone looked at stays alive without a player**: AzerothCore updates an object only near a
  player. As proposed with step 9.3, from the sources of AzerothCore (*Step 9.3, proposed*, below),
  the observer spawns nothing: on the map's own update, it loads the grids of the zone and keeps
  its creatures and game objects in the map's list of objects to update, as the sight of a player
  does. An active invisible object, first chosen here, would keep only itself updated. The zone is
  kept no longer once the editor unsubscribes or disconnects, or when nothing comes from the
  editor for 10 seconds (a heartbeat), so that an editor that crashed leaves no zone alive behind
  it.
- **Supported AzerothCore**: the module is built against a given commit of AzerothCore, written in
  its documentation and in the handshake; moving to a newer one is a change of the module, reviewed
  as such. The CI cannot build AzerothCore: the protocol is tested against a fake server.
- **Bounded load**: a maximum radius, a maximum number of objects, changes only.

**Modules of the editor:**

- `viewport`: a free camera beside the orbit of step 8.3 (fly with the keyboard, look with the
  right or middle button, the wheel for speed). Its position, target and angle stay the
  properties and commands of step 8.3 (`viewport/camera_position`, `viewport.look_at`, ...), so
  that the Timeline animates a flight over a map; their reach, 100,000 units from the origin,
  covers a map, 64 × 64 tiles of 533.33 yards centred on the origin. The axes and units of WoW
  are matched to those of the view. The view draws in reverse Z with no far plane since step 8.3
  (`Target::depth_compare`): the terrain, the models and the buildings take it, so that a whole
  map is seen with its depth precise near the eye.
- `terrain`: WDT and ADT, those of 3.3.5a and the split tiles WarcraftXL loads, heights, textures
  and their layers, loaded and unloaded around the camera.
- `models` and its service `models`: M2 (model, skin, textures, the animations *Stand*, *Walk*,
  *Run* chosen by the movement received) and WMO, those of 3.3.5a and the modern ones WarcraftXL
  loads, the same instances whatever their version, loaded in jobs, kept on the GPU, and the
  instances other modules give drawn in the viewport, each with its transform, its animation and
  its id.
- `placement`: the doodads and buildings of the tiles, given to `models` as instances; `liquids`:
  the water of the tiles.
- `live-world`: the connection to the observer, the entities given to `models` as instances, their
  moves interpolated between two updates, reconnection, and a visible state *server stopped* or
  *observer missing*.

#### Threads

The live view is mostly reading and decoding, which spread over every core, and a little drawing,
which stays on the interface thread.

| Work | Thread | How |
|---|---|---|
| Reading the archives (MPQ) | Any thread | The `vfs` service of `assets` is `Send + Sync` and reads with positional reads, without a global lock, so that every job reads at once; a small cache of decompressed blocks is shared between threads |
| A terrain tile: the ADT parsed, the meshes and alpha maps of its chunks built, its textures decoded, its GPU buffers and textures created and filled | A job of the pool (`Context::spawn`) per tile; its 256 chunks split with `parallel_for` (below) | Created and uploaded in the job (T5), which submits its own uploads: wgpu starts a transfer only at the next `submit`, and the viewport submits only when it draws, so uploads left to it would pile up in memory while the view is hidden and all leave with one frame, making it late. The GPU resources, ready to draw, come back through `on_job` |
| An M2 or a WMO: parsed, its skins built, its textures decoded, uploaded; the groups of a WMO in parallel | Jobs of the pool | The `models` service loads a file once even when many ask for it at once: a load in flight is shared |
| BLP textures | Inside the jobs that need them | A texture cache shared between threads (`libs/gpu`), by path, a load in flight shared |
| The order of loading | The module, on the interface thread | What is wanted is queued by distance to the camera and size on screen; at most *cores − 1* loading jobs run at once; a job whose tile left the zone is cancelled (`is_cancelled`). Loading pauses while the view is not drawn (no frame signal: minimised, or its tab hidden) and goes on when it is |
| Handing ready resources to the drawing | The interface thread | A bounded amount per frame, within a time budget (2 ms to start with), so that a burst of loads never makes a frame late |
| The connection to the observer: reading, decoding a binary protocol, keeping the state of every entity | A thread of its own (`Context::spawn_thread`: work that waits) | Writes a new snapshot of the entities, shared between threads (an `Arc` swapped under a brief lock); its readers never wait for the network |
| Interpolating the moves of the live entities between two updates | A thread of `live-world`, woken at each frame by the viewport's frame signal (below) | Reads the snapshot and gives `models` the transform and movement of each entity for frame N+1 while frame N is drawn |
| Choosing the animations and computing the bones of every animated instance: creatures, and the doodads that move (torches, trees) | A thread of the `models` service, woken at each frame | The bones of hundreds of instances split with `parallel_for`; the data of the frame to come written to the GPU from that thread (T5) with `Queue::write_buffer`, one call per buffer, the instances and the bones they use in the same buffer: wgpu applies a write whole at the next submission, so that a frame draws the old data or the new, never half of it, and the bundles kept go on drawing the same buffers. Written after the frame signal, which comes once a frame is submitted, it is drawn by the next frame |
| Culling and recording the draws | The interface thread | Culling by tile and by group of instances, not by object: the bundle of a layer is recorded again only when the set of tiles or groups in sight, or what is loaded, changes, which flying does far less often than once per frame. Instanced draws, one per model and material, so that a frame's work follows the number of models in sight, not of objects; the camera and the animated instances change through buffers, not through the draws |
| Commands that read the live world | The calling thread (T4) | Read the shared snapshot and answer at once, from any thread |
| Events of the live world | Published by the connection thread, delivered on the interface thread | Batched, as L4 says |
| Rebuilding one terrain chunk (the editing to come) | A job | Its new GPU resources take the place of the old ones at the next frame |

**Drawing many models (9.4 to 9.6)**, from what step 9.2e measured on the terrain, where a draw per
chunk made the frames late and a draw per tile left the interface thread nearly idle:

- The draws follow the models and the materials in sight, never the objects: instanced, a draw per
  model and material, the instances and their bones in buffers (*Threads* above).
- Each object is drawn up to a distance by its size, as the client does: the small doodads cut near,
  the large ones and the buildings far, the distance of the view scaling them all.
- The M2 have levels of detail by distance, their skin profiles (the files `00.skin` to `03.skin`
  of 3.3.5a, fewer triangles each), chosen for each group of instances as the terrain chooses a
  level for each tile, a level changing only past its limit by a margin.
- The buildings are culled by their portals, as the client does: from the group the camera is in,
  through the portals in sight, the groups seen; from outside, each group by its bounds.
- Then, if the cities ask for it once measured, an occlusion on the GPU: the depth of the frame
  before reduced to a pyramid (Hi-Z), the instances tested against it by a compute pass that writes
  the indirect draws.
- The statistics of the view count their draws, triangles and times, a line per layer, and the
  targets of step 9.2e hold for them: 60 frames a second flying fast, the interface thread under 4
  ms a frame for a layer, a few hundred draws for a layer.
- No job writes with `Queue::write_texture` to a texture a frame may be drawing, nor submits while
  holding a lock the interface thread takes: wgpu-core 30 holds the state of initialisation of the
  texture written while it takes the trackers of the device, a submission takes them the other way
  round, and the two lock each other (found in step 9.2f). A job fills a shared texture, an array
  of textures of the models for instance, by a copy from a buffer of its own that it submits. The
  lock is checked again in the sources of wgpu at each new version, and the work-around removed
  once it is fixed.

Additions to the core this milestone needs, specified and reviewed with step 9.2a:

- **Fork-join on the pool, `parallel_for`**: a job, or a thread of its own, splits a slice of work
  over the threads of the pool and waits for them, working on its own slices while it waits, never on
  other jobs. The pool stays the only set of worker threads: a second pool, inside a library, would
  fight it for the cores. The slices are short (under a millisecond as a goal), and the pool runs an
  ordinary job, or the kernel's own work (the sorts of large tables of step 8.7, `Pool::background`),
  before any slice, so that a job of another module waits at most for one slice: that is
  what keeps a thread for the others, more than the limit of *cores − 1* loading jobs, whose slices
  use every thread. T2 says that waiting in `parallel_for` is not waiting without a limit.
  - The pool has one queue today, a channel its workers read in order; it gets two: the jobs and the
    kernel's work in one, the slices in the other, a worker taking from the first whenever it can.
  - It is offered to the Rust modules and services as `uniwow_api::parallel_for`, a function of the
    runtime the kernel sets when it starts, as it sets `ui::set_background`; before that, in the
    tests of a module for one, the slices run on the calling thread. It needs neither a job nor a
    `Context`: a service such as `formats` uses it inside the job that calls it.
  - The work may borrow the caller's data: `parallel_for` returns only once every slice has ended,
    even when one panicked, and the first panic is then resumed in the caller, whose module it is.
    A slice may call `parallel_for` in turn.
  - The other languages: not planned in this milestone; their modules run threads of their own (T7).
    The table of capabilities says so.
- **Bundles kept in the viewport**: a layer may keep its recorded render bundle from one frame to
  the next and record it again only when what it draws changes; the viewport validates a bundle
  once, when it is recorded, as today. A layer gains a step run at each frame before the drawing,
  `prepare`, with the view of that frame, which writes its buffers (the camera, the instances) without
  recording anything: today `draw` receives the view only while it records.
  - The trait `Layer` gains `prepare` and a version of what it draws, none by default: a layer
    without a version is recorded at every frame as today, as the cube's and the faulty sample's
    layers are, unchanged. A layer with one is recorded again when it changes.
  - `prepare` runs, as `draw` does, inside an error scope and `catch_unwind`: a layer failing there
    is removed and its module reported (F5).
  - Every bundle is recorded again when the device is created again (device lost, below).
- **A frame signal in the viewport service**: threads of modules wait on it to prepare the next
  frame while the current one is drawn, with the time of the frame to come; shared between threads
  (T3).
  - It is given once the viewport has submitted a frame, so that what a thread writes then is drawn
    by the next frame, with the number of that frame and its time, estimated from the frames
    before. It is not given while the view is not drawn.
  - A thread waits at most 100 ms at a time, then checks its cancellation: the viewport does not
    know which module a waiting thread belongs to.
- The table *Who runs what* of section 5 gains the slices of `parallel_for` on the workers of the
  pool, and the threads of modules waiting for the frame signal.

Of the points to revisit of section 5, this milestone takes up the 3D view: bundles kept from one
frame to the next, and GPU resources created and uploaded by the jobs rather than on the interface
thread (steps 9.2a and 9.2). The loading of the modules and the files of the Timeline are not its
concern.

When a module fails or the editor closes:

- Its threads of their own end: the connection reads the network with a short time limit (100 ms)
  and checks its cancellation between reads; a thread waiting for the frame signal waits 100 ms at
  most at a time and checks its cancellation as well. None keeps writing after its module failed.
- The `models` service removes the instances of a module that failed, as the viewport removes its
  layers, and the loads only it had asked for are cancelled.

GPU memory:

- One budget for the whole view, shared by the terrain, the M2 and the WMO (designed with step
  9.2f, built with 9.4); the terrain keeps its own until then:
  - **Who decides**: the `viewport` service, on the interface thread, once a frame. The budget is
    its setting, half the memory of the GPU's own by default, as the terrain's is today; the
    terrain's `gpu_budget_mb` becomes it.
  - **What the layers tell it**: at each frame, while steering, each layer that keeps resources on
    the GPU gives what it takes outside its items (the arrays of textures, the horizon), and the
    bytes of the items it holds and of those it wants, counted by their distance from the eye in
    quarters of a tile up to 64 tiles: 256 sums, whatever the number of items, merged in a
    microsecond. An item whose cost is not known yet counts the mean of those of its kind held, as
    the terrain's tiles do. The models are counted at the distance their size lets them be drawn
    from (*Drawing many models* above), so that a small doodad never claims room far away.
  - **The priority, near first, whatever the layer**: from the bands, the service finds the farthest
    distance whose items, of every layer, fit 90 % of the budget, where the layers load, and the
    farthest that fits all of it, where they keep what they hold; between the two, nothing is
    loaded or released, as the terrain does since step 9.2f. Both are given back to every layer at
    the next frame, with the reach the budget leaves when it holds fewer than wanted, which the
    panel of the view and the statistics show.
  - **How each layer gives memory back**: it starts no load beyond the first distance, nor one
    that its cost would take beyond the budget; it releases what lies beyond the second, the
    farthest first, never an item whose model holds changes; its GPU resources go when the last
    frame using them ends (wgpu keeps them until then), its models in memory are freed by a job;
    what its items share (the textures of the M2, the layers of the terrain's arrays) goes when its
    last holder goes, purged by a job.
  - The interface thread never waits for it: the layers and the service run on that thread, and
    the jobs only read the distances given.
- A device lost (driver reset, memory exhausted) stops the drawing of the view with a message; the
  rest of the editor goes on, and the view loads again what it shows once a device is back.

On the server, in `mod-uniwow-observer` (AzerothCore updates its maps on several threads):

- The objects of a map are read only during that map's update, by a hook of the map's scripts, on
  its thread, and copied into the outgoing state of each subscription. The network runs on a thread
  of its own and never touches an object of the game.
- A subscription the network moves is applied at the next update of its map, through a queue; the
  grids of its zone are loaded and its objects kept updated there, never from the network thread.

#### Preparing the editing to come (designed here, not built)

1. **A terrain model that can be edited, not only drawn.** The ADT are loaded into a model in
   memory that keeps, chunk by chunk, everything that will be edited: heights, normals, vertex
   colours (MCCV), the area of each chunk (`AreaTable`), holes, texture layers and alpha maps,
   liquids, the references of doodads and buildings. Drawing is built from this model **chunk by
   chunk**, one mesh and its alpha maps per chunk, so that a changed chunk will be rebuilt alone, in
   a job, without reloading its tile. Each chunk and tile can be marked changed; nothing is written yet.
2. **Stable ids for whatever will be selectable**: a terrain chunk by its tile and chunk; a doodad
   or a building of a tile by its `uniqueId`; a live entity by its GUID at run time **and** the id of
   its spawn in the database, so that a later selection finds the row to edit.
3. **Picking**: each model (bounds of the M2 and WMO) and each terrain chunk keeps a bounding
   volume. The viewport's interface plans, without building it, *what is under this point* (a ray
   against the terrain, the objects and the entities), the picking of step 8.3; the drawing plans
   how a selection is highlighted, and `models` already takes a highlight flag per instance.
4. **Controls**: the left button is kept for the tools to come (selection, handles, brushes); the
   camera uses the right or middle button, the wheel and the flying keys. In this milestone, a left
   click does nothing.
5. **Overlays**: the drawing of the terrain can lay information over it, off in this milestone:
   vertex colours, areas in colours, holes, the grid of chunks. The brushes will need these views.
6. **Two sources of entities**: the editor will show the spawns as they are in the database (what is
   edited) and the live entities (what runs). The model of entities tells the two apart from now
   on, even though this milestone shows the live ones only, and keeps for each its map, instance,
   phase mask, and the pool or game event of its spawn: editing a spawn will depend on them.
7. **The way of writing to come**, described, not built; every change will go through the
   kernel's undoable commands (F2, S4):
   - terrain: the ADT written by `libs/formats`, then the server's data rebuilt (`.map`, vmaps,
     mmaps, the module `server-map-data`);
   - spawns: written in the database (`libs/db`), and the server updated while it runs, through a
     protected capability of editing of the observer or another way chosen later (SOAP, GM
     commands).
   Nothing in this milestone's design may stand in the way of either.
8. **Parity from the design**: selection, picking, and later the editing of the terrain and of the
   spawns, are offered to every language through the unified API, as the other capabilities (R9).

#### Steps

Each step is reviewed before the next one; the milestone is delivered once all are done.

| Step | Content |
|---|---|
| 9.1 | Installations; the module `assets` and its services `vfs` and `formats`, with their interfaces in `core/api`, read from any thread at once: the archives in the order of the client and of the patcher of WarcraftXL, folders mounted as archives, the delete markers of the patches; read economically (one allocation for the data of a file, buffers reused by each thread); the FileDataIDs turned into paths through `TextureFilePath.db2` and `ModelFilePath.db2`, of the versions WarcraftXL reads for them (WDC1 to WDC3), as WarcraftXL does; the DBC `Map`, `AreaTable`, `CreatureDisplayInfo`, `CreatureModelData`, and those the next steps need |
| 9.2a | The additions to the core: `parallel_for`, bundles kept in the viewport with `prepare`, its frame signal |
| 9.2 | The terrain model that can be edited (point 1 above), from the ADT of 3.3.5a and the split tiles, loaded in jobs in the order of *Threads*, its uploads submitted by the jobs, the GPU memory budget, drawn chunk by chunk; the free camera |
| 9.2e | The performance of the terrain, asked by the review of 9.2c and 9.2d: measured first (the statistics of the view); a draw per tile, its textures in arrays; levels of detail by distance; the horizon of the WDL with a fog; the targets measured on the user's machine; what will hold for the doodads and the creatures (*Drawing many models*, below) |
| 9.2f | The terrain at a distance of 64, asked by the review of 9.2e: the budget kept without loading and releasing in turn; light tiles beyond 7 tiles; the limits of the device; measured on three maps; the budget shared by the view for 9.4 (*GPU memory*, above) |
| 9.3 | The observer and its threads, on both sides; the entities as markers (a coloured shape and the name) moving in real time; the commands and events of L4. In three parts (*Step 9.3, proposed*, below): |
| 9.3a | The protocol, written down; the observer on the server, built in the user's AzerothCore and checked there with a small probe: the zone kept alive, its cost, the bytes a second in a city |
| 9.3b | The editor's side: the client of the protocol in `libs/server-link`, tested against a fake server; the module `live-world`, its connection thread, its snapshot, its settings and states; the commands and events of L4 |
| 9.3c | The entities as markers moving in real time, interpolated on the frame signal; accepted on the user's machine with the server running |
| 9.4 | Still M2 models, of 3.3.5a and modern: from the display id to the model, its skin, its textures and its scale. In four parts (*Step 9.4, proposed*, below): |
| 9.4a | The GPU budget of the view, shared, as designed in step 9.2f; the terrain on it |
| 9.4b | Reading M2 and skins, of 3.3.5a and modern, in `assets`; the tables of the displays of creatures and game objects; checked over every model of the client |
| 9.4c | The module `models` and its service: instances given by the modules, models loaded once by jobs, drawn instanced by model and batch, their levels of detail and their reach by size, on the shared budget |
| 9.4d | The live world as models: creatures and game objects by their display, the markers kept for what has no model; accepted on the user's machine in a city |
| 9.5 | M2 animations, of 3.3.5a and modern (`.anim` files): *Stand*, *Walk*, *Run* chosen by the movement received, on the animation thread |
| 9.6 | Buildings (WMO, of 3.3.5a and modern), doodads and water |
| 9.7 | Optional, proposed apart: light and sky (`Light.dbc`), the server's time of day |

#### Risks verified first

| Risk | Result |
|---|---|
| Does the active invisible object keep the zone looked at alive without any player: creatures moving, paths followed, respawns? | Read in the sources of AzerothCore for the proposal of step 9.3: no, an active object keeps only itself updated; the observer keeps the objects of the zone in the map's update list instead (*Step 9.3, proposed*). On the server: to verify in step 9.3a |
| The volume of data in a crowded city at the rate chosen | To verify |
| The work of the M2 animations (bones, interpolation) | To estimate |
| Speed of the terrain and the models in a city (the goal to fix), and the cost of rebuilding one terrain chunk alone, for the editing to come | The terrain measured in step 9.2e (below); the models and a chunk rebuilt alone: to measure |
| Reading the archives from many threads at once: does it scale with the cores, or does the disk or a lock limit it? | Measured in step 9.1: the archives scale with the cores, the allocator of Windows does not beyond 8 threads; `assets` reads economically, which scales to 32 threads (step 9.1) |
| The time the interface thread spends per frame while flying fast over a city: handing over, culling, recording | The terrain measured in step 9.2e, flying fast over Azeroth: 0.1 ms a frame on average, 0.3 ms at most; at a distance of 64 (step 9.2f), 0.4 ms at most on average, 0.73 at most; the models: to measure |
| The bytes of animation (instances and bones) written to the GPU per frame in a crowded city | To measure |
| What warcraft-rs reads and writes correctly in 3.3.5a, format by format; what `assets` copies of it, without `rayon` | Reading verified in step 9.1 (below): archives, DBC, WDT, ADT and WMO groups read; M2, skins, WMO roots and BLP have faults to correct in the copy. Writing not verified yet |
| Which versions of the modern formats the extensions of WarcraftXL load, and where they find the files (their folders, loose files, FileDataIDs and listfile): the editor must read the same files from the same places | Verified in step 9.1 in their sources (below) |
| What wow.export reads of those formats, and how much of it the translation takes | DB2 verified in step 9.1: wow.export reads WDC2, `1SLC`, WDC3, WDC4 and WDC5, not WDC1; the translation takes WDC2, `1SLC` and WDC3, for the tables of paths, which read their strings, ids and rows as wxl-db2 does. M2, WMO, ADT and BLP: at their steps |
| Does the active invisible object stay out of the game (no aggro, no AI, not seen by game masters), and is it always removed (unsubscription, disconnection, heartbeat lost)? | No object is spawned any more (proposal of step 9.3); that the zone is kept no longer after an unsubscription, a disconnection or a heartbeat lost: to verify in step 9.3a |

#### Results of the verifications (step 9.1)

**warcraft-rs 0.7.0 on the 3.3.5a client of the user** (`E:\world of warcraft 3.3.5a hd`, enUS),
read only, by a probe outside the repository:

- The 35 archives of the chain open, in the order of Wow.exe 12340, `patch-Z.MPQ` included (format
  1, 3.4 GB, its tables beyond 2 GB): 235,111 names. 4,911 of them are deleted by a **delete marker**
  of a patch (flag `0x02000000`, size 0), which wow-mpq lists and reads as an empty file: the `vfs`
  service takes them as deleted.
- Read and checked: every DBC (245, their header against their size), every WDT (107), 400 ADT of
  3.3.5a (256 chunks each), 199 WMO groups.
- Faults the copy corrects, each with its test on the client's files:
  - **M2**: 273 of 500 models read, their bones and vertices as their header counts them; not one
    model with particles, ribbons, cameras or lights (0 of 183), and few with events: those
    structures are not read as 3.3.5a (version 264) lays them out, and they are those of the torches
    and braziers of the scene.
  - **Skins**: 480 of 500; an empty skin (48 bytes, valid) is refused.
  - **WMO roots**: 199 of 200; a name that is not UTF-8 is refused.
  - **BLP**: 597 of 600 decoded; three DXT5 textures whose last levels are empty are refused.
- Writing, needed by later milestones, is not verified in this step.

**Reading the archives from several threads at once** (1,500 files, about 220 MB, a run; each
thread with handles of its own, opened beforehand; the client on a SATA SSD; 32 threads):

| Threads | Files never read (Windows allocator) | Files cached (Windows allocator) | Files cached (mimalloc) |
|---|---|---|---|
| 1 | 104 MB/s | 260 MB/s | 250 MB/s |
| 8 | 325 MB/s | 1,450 MB/s | 1,950 MB/s |
| 16 | 500 MB/s | 990 to 1,180 MB/s | 2,700 MB/s |
| 32 | 545 to 620 MB/s | 630 to 760 MB/s | 3,600 MB/s |

The archives scale with the cores; the allocator of Windows does not, beyond 8 threads, as three
runs showed. With mimalloc, files never read reach 763 MB/s at 32 threads. Hence the decision of the
user above, after mimalloc failed as the allocator of the editor (section 2). Opening an archive reads its tables, up to 60 ms for `common.mpq`: the `vfs` service
reads them once and shares them.

**WarcraftXL**, read in its sources (wxl-core 60033ab, wxl-modern-m2 feba6b3, wxl-modern-wmo e56fa7c,
wxl-modern-adt 5f2726c, wxl-db2 30e4f2c, wxl-hub 884a0da, DB2Gen 7e86924), nothing copied. wxl-hub
installs the last release of each extension, v1.0.0 of August 2026; the M2 and ADT ones have later
commits, which may change what follows:

- **Where the files are**: through the client's own reading, in its archives. The patcher of
  WarcraftXL widens `patch-?.MPQ` and `patch-<locale>-?.MPQ` to any name, `Data\Patch-<name>.MPQ`,
  which may also be a **folder** of that name mounted as an archive. No CASC, no listfile at run
  time.
- **FileDataIDs**: turned into the path of the file through `TextureFilePath.db2` (`.blp`) and
  `ModelFilePath.db2` (models, `.skin`, `.anim`...), WDC1 tables that DB2Gen builds from the
  community listfile, read loose from `DBFilesClient\` beside the client, then from the archives;
  the file must then exist at that path. wxl-hub does not install these tables.
- **M2**: only the `MD21` container, of versions 272 to 274; the skins by name, `<model>NN.skin`,
  and `<model>_lodNN.skin`; the `.anim` by name, unwrapped from `AFM2`; the textures of `TXID`
  through the tables; `.bone` and `.phys` left aside; a model with `.skel` refused; with more than 4
  skin profiles, the first only.
- **WMO**: modern when the root has no `MOTX` or has `GFID`; the textures of `MOMT` through the
  tables; the groups by name, `<root>_NNN.wmo`; the doodads of a modern WMO are not placed.
- **ADT**: split when `<tile>_tex0.adt` exists; the root, `_tex0` and `_obj0` only; the doodads and
  buildings flagged as FileDataIDs (`MDDF` 0x40, `MODF` 0x8) through the tables; at most 4 texture
  layers.
- **DB2**: WDC1 to WDC3 for the two tables of paths (corrected in 9.1b, below), WDC5 for its other
  tables; no DBC of 3.3.5a is replaced or extended.
- The view shows what the client shows: a modern WMO without its doodads, for one.

**The client of the user**: the extensions of WarcraftXL are installed (since 2 October), but its
archives hold no `.db2`, no split tile, and 2 `MD21` models of 23,155, one of version 274, the other
an `MD21` around a model of version 264, which WarcraftXL does not take. The acceptance of the
modern files needs files exported with wow.export and the two tables of DB2Gen installed.

#### As built

Step 9.1 is built in two parts, each reviewed: the archives and the service `vfs` (9.1a), then the
FileDataIDs, the DB2 and the service `formats` (9.1b).

First part (9.1a), the archives:

- `uniwow_api::vfs`: the trait `Vfs`, shared between threads (T3): the bytes of a file by its path,
  case and slashes ignored, or none when no archive holds it or a patch deleted it; whether a file
  exists; the files listed under a folder; the state, no client (with why), opening, or ready with
  its archives and files. The runtime re-exports `miniz_oxide`, for zlib: it already held it.
- The module `assets` offers it as the service `vfs`. Its panel *Assets* sets the folder of the
  client, kept in its settings (`client_folder`), and says how far its files are and which
  archives were left out. The locale is the one `WTF\Config.wtf` sets, else the one folder of
  `Data` holding its `locale-<locale>.MPQ`.
- The order is that of Wow.exe 12340 for the patches, widened to any name as the patcher of
  WarcraftXL does, a folder of that name mounted as an archive; the base archives follow in the
  order the community documents, not checked against Wow.exe (checked and corrected in 9.1b).
- An archive of format 1 or 2, its positions unsigned (`patch-Z.MPQ`, past 2 GB), has its tables
  read once, then its files read by position from any thread, without a lock: the packed bytes in a
  buffer each thread reuses (kept up to 64 MB, 8 MB since 9.1b), the file in one allocation of its
  size; stored, compressed with zlib in one unit or in sectors, with or without their checksums
  (not checked).
  The entry of the neutral locale comes first; without one, the first entry of the name is taken,
  whatever its locale, where the client would prefer its own: the archives of 3.3.5a give every
  file a neutral entry. A damaged archive is refused, or its file: sectors of more than 512 << 23
  bytes, data past the end of the archive, a file of more than 1 GB or larger than its stored
  block. A folder mounted is read once by its real path, a junction back up making no loop. A
  delete marker hides the file of the archives read
  after it. The encryption of files, PKWare's implode, the other compressions and the incremental
  patches are refused by name: the archives of 3.3.5a hold none.
- The lists of the archives are merged, a name deleted by the first archive listing it left out; a
  file no list names is read, but not listed.
- `modules/assets/THIRD_PARTY.md` names what comes from wow-mpq, its commit, its authors and its
  licence.
- Measured on the user's client, the reader of `assets` with the allocator of Windows (1,500 files
  a run):

  | Threads | Files cached | Files read for the first time |
  |---|---|---|
  | 1 | 330 MB/s | 230 MB/s |
  | 8 | 2,400 to 2,600 MB/s | 770 MB/s |
  | 32 | 4,100 to 4,300 MB/s | 860 MB/s |

  More than wow-mpq with mimalloc (3,600 MB/s at 32 threads): the bytes it allocates per file, not
  the allocator, held the reading back.
- Tests: archives the tests write (the formats, the ways of storing a file, a delete marker, the
  order of the patches and a folder mounted, the files refused, 16 threads reading at once); the
  client's own archives, their DBC checked and a sample read, and the reading measured, when
  `UNIWOW_CLIENT` names its folder, skipped otherwise.
- Not in this part: the FileDataIDs, the DB2 and the service `formats` (9.1b); the observer's
  address, port and token, with the observer (9.3). Other languages do not reach the client's files
  yet: the table of capabilities says so.

Where the work of the archives is done:

| Work | Thread | Lock |
|---|---|---|
| The locale and the order of the archives | Interface, at start or when the folder changes: a few listings of folders | — |
| An archive opened: its header and tables, or a folder walked | A job of the pool, one per archive | None |
| The lists merged into the chain | A job of the pool | None |
| The chain handed to the service | Interface, when that job ends | The state of the service, the time of an exchange |
| A file read | The thread calling `read`, any | The state of the service, to take the chain; none while reading |

Second part (9.1b), the FileDataIDs, the DB2 and the service `formats`:

- Corrected after the review of the first part: the base archives follow the order of Wow.exe
  12340, read in its table at 0xAB6168, which 0x405DD0 opens with priorities falling from 0x3F,
  the patches above 0x40: `expansion`, `lichking`, `common`, `common-2`, then those of the locale
  (`locale`, `speech`, `expansion-locale`, `lichking-locale`, `expansion-speech`,
  `lichking-speech`), then `development`; `Data\alternate.MPQ` comes before every patch;
  `base-<locale>.MPQ`, which the game does not open, is left out. No file of the user's client
  lies in two base archives: what it reads does not change. A thread keeps up to 8 MB of packed
  bytes; a larger file is read into a buffer of its own.
- Every file the client lists read once, by a test run on demand
  (`every_file_the_client_lists_is_read_and_none_is_refused`): 230,200 files, 17,157 of them
  sounds, 36.6 GB in 57 s, none refused; again on the client as it is since 5 October: 232,042
  files, 22,176 of them sounds, 30.9 GB in 58 s, none refused.
- `uniwow_api::vfs`: `path_of`, the path of a modern file by its FileDataID, through
  `TextureFilePath.db2`, then `ModelFilePath.db2`, as WarcraftXL does; none when neither names it.
- `uniwow_api::formats`: the trait `Formats`, shared between threads (T3), and its rows, plain
  data: the maps (`Map.dbc`), the areas (`AreaTable.dbc`), the looks of creatures
  (`CreatureDisplayInfo.dbc`) and their models (`CreatureModelData.dbc`), by increasing id, their
  texts in the locale of the client (enGB reads those of enUS). Each table is read once, by the
  first thread asking for it, the others asking meanwhile waiting for it; another client folder
  starts them again. In debug, a table asked from the interface thread is said once in the log.
  The module `assets` offers it as the service `formats`.
- The tables of paths: read loose from `DBFilesClient` in the client folder first, then from the
  archives, both at once, each on a thread of its own, inside the job that merges the lists. A
  table missing is not an error; one that cannot be read is said in the panel, which also says
  how many FileDataIDs the tables name. A table keeps its bytes and, for each FileDataID, where
  its path starts: a million rows read in about 10 ms (release), the path made only when asked.
- DB2: WDC1, as DB2Gen writes it, read from the public description of the format; WDC2, `1SLC`
  and WDC3 translated from wow.export (MIT), each section found by the offset its header gives.
  The rest as wxl-db2 30e4f2c reads these two tables (`FdidResolver.cpp`, `Db2Decode.cpp`,
  `DB2File.cpp`, read after the review, nothing copied), which corrected the first build:
  - the offset of a path counts from the start of the strings of all the sections, end to end, in
    every version: wxl-db2 names no column of strings for these tables, so it never takes the
    offset from its field; an offset out of the strings names nothing;
  - WDC4 and WDC5 are refused: wxl-db2 reads WDC5 only for its other tables (`Wdc5.cpp`);
  - a record takes its id from the list of ids of its section as it stands, else from the column
    of ids, plain, packed, signed or in a pallet, else from its place;
  - an id named twice keeps its last row, copies included, which come after the rows; a last row
    with an empty path names nothing, and the lookup goes on to `ModelFilePath.db2`;
  - the records of variable size are refused, as a column of relations: wxl-db2 does not read a
    path from the first, and refuses the second by the size of its rows;
  - FileDataID 0 names no file; an empty loose table leaves that of the archives.

  A table holds its paths and, with no list of ids, its ids, nothing else. A section encrypted
  with a key the client lacks holds zeros, which name no file: it needs no reading of its own.
  wxl-db2 also reads `TextureFileData.db2` (in WDC5), to turn a material into a FileDataID, which
  milestone 9 does not take yet.
- DBC: the header checked against the columns WoWDBDefs gives for 3.3.5.12340 (CC BY-SA 4.0); a
  table of another layout, or damaged, is refused with its name.
- `modules/assets/THIRD_PARTY.md` names wow.export, its commit, its authors and its licence,
  WoWDBDefs, and the sources of wxl-db2 read; nothing comes from DB2Gen or WarcraftXL (GPL-3).
- Measured on the user's client (frFR from 5 October; esES on 8 October, as Config.wtf says),
  release: the four tables read in 7 ms,
  135 maps, 2,307 areas, 24,262 looks of creatures and 1,537 models, 1,527 of them found in the
  archives by their `.m2`. The client holds no table of paths: no FileDataID named.
- Tests: DB2 the tests write, of each version (sections, strings, ids of every kind, the last
  row kept, copies, an encrypted section; WDC4, WDC5, records of variable size and relations
  refused; every file cut short refused, every byte damaged without a panic); the tables of paths
  found loose then in the archives, textures before models; DBC the tests write (the locale, the
  order by id, a table read once, another layout and damaged files refused); the services before
  and after the client opens, and the warning of debug; the client's own tables when
  `UNIWOW_CLIENT` names it. Each change made on purpose to the readers and the services made a
  test fail, but the thread of the picker, which no test opens: the recette checks it.
- At the user's request, *Open* shows the folder picker of Windows, which starts in the folder
  shown, and opens the client when a folder is chosen; cancelled, it changes nothing. A folder
  typed in the field is opened once the field is left, Enter or not, unless it is the one open; one
  that is no folder is said under the field. The picker
  waits for the user on a thread of its own (T2), the interface going on meanwhile, *Open* greyed
  until it closes.
  The runtime re-exports `rfd` for it, which adds no other crate to the runtime.
- Kept for later, from the review of the first part: an index of the files by folder, for
  `files_under`, when the browser of files needs it. The DBC the next steps need come with them.

Where the work of the second part is done:

| Work | Thread | Lock |
|---|---|---|
| The tables of paths read | The job merging the lists, a thread of its own per table | None |
| A FileDataID turned into a path | The thread calling `path_of`, any | The state of the service, to take the client; none while reading |
| A DBC read and its rows made | The first thread asking for it; the others asking meanwhile wait | The cell of the table (`OnceLock`), until it is read |
| The folder picker shown | A thread of its own (T2), while the user chooses | None |

Step 9.2a, the additions to the core:

- `uniwow_api::parallel_for(count, slice, work)`: `work` over `0..count` in slices of `slice`
  indices, on the workers of the pool and the calling thread, which takes slices while it waits
  and nothing else; `uniwow_api::parallel::Workers` gives the same on a pool of one's own, as the
  kernel's tests do. The kernel sets the workers when it starts (`parallel::set_workers`); before,
  the slices run on the calling thread. The work borrows the caller's data; once a slice panicked,
  the slices left are skipped, and the first panic is resumed in the caller once every slice has
  ended. A slice may call it in turn.
- The pool has two queues under one lock: the jobs and the kernel's work, then the helpers of the
  slices. A worker takes a job before any helper; a helper leaves between two slices when a job
  waits, a count read without the lock, and is queued again when it left slices to take, so that
  a worker comes back to them once the job is done. A helper runs on behalf of the module that
  called `parallel_for`: a panic in a slice is logged under its name. In the test, a job of another module started while every
  thread ran slices of 10 ms ended within 100 ms.
- The layers of the viewport: `Layer::prepare(gpu, view)`, run at each frame before any bundle is
  drawn, and `Layer::version()`, none by default. A layer with a version keeps its bundle while the
  version stays the same; it is recorded again when the version changes or when the device
  changes, the device of the bundles kept being compared at each frame. `prepare` and `version`
  run inside a validation error scope and `catch_unwind`: a layer failing there is removed and its
  module reported. The cube and the faulty sample have no version: recorded at every frame, as
  before.
- The frame signal: `Viewport::wait_frame(after, timeout)` gives the frame to come (`Frame`: its
  number, from 1, and its time, estimated from an average of the frames before, a pause of more
  than a second not counted) once its number is past `after`, waiting `MAX_FRAME_WAIT` (100 ms) at
  most at a time. It is given after each submission of the view, so not while it is not drawn.
- Tests: the slices cover every index once on several threads, and nest; a panic comes back once
  every slice has ended, the slices left skipped; a job started while every thread runs slices
  waits for about one slice; a bundle kept while its version stays, recorded again once it changes
  or for a device created again, and a layer failing in `prepare` removed, on the software adapter
  of Windows (WARP), skipped where there is none; the frame signal waking a waiting thread, never
  holding it more than 100 ms, and the time of the frame to come. Each of 13 changes made on
  purpose to `parallel_for`, the queues of the pool, the bundles kept and the frame signal made a
  test fail; the comparison of devices in the drawing of the view, which no test draws, is checked
  by reading.
- Not in this step: the jobs that create and upload GPU resources (9.2). No layer uses a version
  or the frame signal yet.

Step 9.2 is built in parts, each reviewed: the readers of the terrain and of the textures (9.2b);
the module `terrain`, its model chunk by chunk, loaded and drawn, with the GPU memory budget and a
map to choose (9.2c); the free camera, with the hotkeys and the GPU budget by default (9.2d).

First part (9.2b), the readers of the terrain and of the textures:

- `uniwow_api::formats` gains the plain data of the terrain and of the textures (`FileRef`, `Wdt`,
  `Tile`, `Chunk`, `Layer`, `Doodad`, `Building`, `Texture`, `TextureFormat`) and four reads:
  `wdt` (read once per map), `tile`, `texture` (the levels as stored, DXT kept) and
  `texture_rgba` (every level decoded); its documentation sets the world coordinates. In debug, a
  format asked from the interface thread is said once in the log.
- WDT (`modules/assets/src/terrain.rs`): the flags of `MPHD`, and the tiles of `MAIN`, the tile
  `<map>_<x>_<y>` at `y * 64 + x`: so for the 753 tiles of Azeroth the client lists. `tile`
  gives none for a tile its WDT does not name, as the client loads none.
- A tile of 3.3.5a finds the parts of each chunk by the offsets of its header, as the client
  does, and only those its flags and counts ask for: some offsets point at nothing else (an offset
  of shadow in a chunk without the flag of shadow, in Azeroth). A part whose size goes past its
  chunk reaches its end; the alpha maps always do, as the client reads them by offset: some
  compressed ones of Northrend go past the size of their part.
- A split tile, as WarcraftXL loads it when its `_tex0` exists: its root, `_tex0` and `_obj0`,
  their chunks walked as wow.export walks them; its textures by FileDataID when it has `MDID`, its
  doodads and buildings by FileDataID by their flags (0x40, 0x8), its holes of high resolution
  (flag 0x10000).
- The alpha maps: compressed in runs, of 8 bits when the WDT has the flag 0x4 or 0x80, else of 4
  bits, with their last row and column copied from the ones before when the chunk lacks the flag
  0x8000, as the description of the format says and wow.export does not. The shadows baked in,
  the vertex colours (stored blue first, given red first), the holes of 3.3.5a widened to the 8 × 8
  quads, the references of doodads and buildings.
- Positions: a chunk's as the file gives it. The client places a chunk by its tile and index,
  which the 17 tiles of the row 60 of Azeroth contradict, all at [3200, 1066.67, 0]. The doodads
  and buildings keep the axes of the file, world = (17066⅔ − z, 17066⅔ − x, y), checked on the
  doodads of `Azeroth_32_48`. The components of the normals are those of the world: checked against
  the slopes of the heights on every tile read.
- BLP (`modules/assets/src/blp.rs`), translated from wow.export: version 2, its palette (alphas of
  0, 1, 4 or 8 bits, an alpha of 4 bits widened times 17 as DXT3 and the alpha maps are, where
  wow.export shifts it and leaves 0xF short of opaque), DXT1, DXT3 and DXT5, kept as BC or
  decoded, BGRA. A level cut short ends the
  levels kept. Versions 0 and 1 are refused by name: Wow.exe 12340 holds the mark `BLP2` seven
  times and never `BLP1`, so the client reads no other. One texture of the terrain of the user's
  client of 2,379 is a BLP1 (`Tileset\Aerie Peaks\AeriePeaksWebs.blp`).
- The device of the editor has no compression BC (the features egui-wgpu asks by default):
  `texture_rgba` serves it until 9.2c chooses.
- warcraft-rs (`wow-adt`, `wow-wdt`, `wow-blp`) could not be copied: it reads through `binrw`, and
  decodes through `image` and `texpresso`, which the runtime does not offer. Its layouts were
  consulted with the public description of the formats; `THIRD_PARTY.md` says so.
- Measured on the user's client, release: the 3,672 tiles of Azeroth, Kalimdor, Outland and
  Northrend read in 1.3 s on 16 threads; the 205 textures of the terrain of Azeroth read and
  decoded in 0.64 s.
- Tests: files the tests write (a WDT and the bytes after its last chunk; tiles of 3.3.5a in every
  way of storing alpha, alpha maps longer than their part, an offset of shadow without its flag;
  split tiles, by names and by FileDataID, holes of high resolution, the flag 0x80; damaged files
  without a panic; BLP of each palette, DXT1 of four and of three colours, DXT3, DXT5 in its two
  modes, levels cut short, BGRA, BLP1 refused); the service (a tile split when its `_tex0` exists,
  none outside its WDT, the WDT read once, a FileDataID no table names refused); the client's own
  maps when `UNIWOW_CLIENT` names it. Each of 22 changes made on purpose to the readers made a test
  fail.
- Not in this part: the liquids (`MH2O`, `MCLQ`) and the building of a map made of one, with the
  water and the buildings (9.6); the map of low quality textures, the sound emitters, `MTXF`.

Second part (9.2c), the module `terrain`, with the two points of the review of 9.2b:

- The device asks for the block compressions BC1 to BC3 when the adapter offers them: the kernel
  wraps the description of the device egui-wgpu asks by default (`core/kernel/src/lib.rs`). The
  textures then go to the GPU as their BLP stores them (`Formats::texture`). Without it, they go
  decoded (`Formats::texture_rgba`), four to eight times larger on the GPU and in the transfer, so
  that the budget fills that much sooner; the panel says which. The software adapter of Windows
  offers BC, and so does the user's GPU.
- `modules/terrain`, its panel *Terrain* on the right: the maps that have terrain (from `Map.dbc`
  and their WDT, named in the locale of the client), the map chosen kept in its settings (`map`)
  and shown again at start; the camera placed over the middle of the map, 1,200 yards high and 800
  aside, through the properties of the viewport, without history.
- The model (`model`): a tile as read, its chunks able to be marked changed, a bit each, which
  nothing marks yet; a chunk known by its tile and index. `model`, `mesh` and `gpu` are public, for
  the editing to come.
- A chunk is placed by its tile and its index, as the client does, `Chunk::position` giving only
  the base of its heights: so are the 17 tiles of the row 60 of Azeroth, whose positions are wrong.
- Loading, steered at each frame from `windows_ui`, the one call of a module at every frame: the
  tiles whose centre lies within `view_distance` tiles of the camera (3 by default), nearest first;
  at most a load per worker but one; a load whose tile left the zone cancelled; nothing started
  while the view is not drawn (no frame signal). A tile refused is said in the panel and not read
  again.
- A job per tile reads it into its model, takes its textures from a cache the tiles share (each
  read once, a load in flight shared), creates its buffers and its texture of blending (the 256
  chunks of the tile, 64 × 64: three alpha maps and the shadow), writes its chunks over the threads
  of the pool (`write_chunk`, which a chunk changed takes alone), its triangles (the quads of its
  holes left out), and submits its uploads.
- The tiles ready are handed to the drawing within 2 ms a frame, one at least.
- The GPU budget, `gpu_budget_mb` in the settings (1,024 by default): beyond it, the tiles out of
  sight are released, the longest unseen and then the farthest first, and the textures no tile
  holds are forgotten; the tiles in sight are kept, even beyond it; a tile released is loaded again
  once wanted.
- The layer keeps its bundle while the tiles drawn and those in sight stay the same, a tile in
  sight when its bounds may meet the view; the camera written in `prepare`; a draw a chunk, with its
  four textures. Its shader blends the layers by their alpha maps (the first by what the others
  leave), repeats a texture eight times across a chunk, multiplies by twice the vertex colour,
  darkens by the shadow baked in, and lights by a fixed sun until the lights of the map (9.7). The
  triangles face up, counter-clockwise; their backs are not drawn.
- On the user's client (deDE since 5 October), Azeroth chosen: 39 tiles drawn and 223 MB on the GPU
  at start, BC; 62 tiles and 353 MB once the camera moved; Dun Morogh as the client shows its
  ground; no entry in the history.
- Tests: a chunk placed by its tile and index, its position wrong; the triangles facing up, the
  holes left out; the texels of blending; the tiles wanted, nearest first; the loads within their
  slots and those left cancelled; the tiles released beyond the budget and loaded again; a box in
  sight, behind, aside, behind and wider than the view; on the software adapter, skipped where there
  is none: a tile built and uploaded while no view draws, read back from the GPU, a chunk written
  again alone, a load cancelled, the textures as BC or decoded, read once for two tiles and
  forgotten once no tile holds them; the device asking for BC when the adapter offers it. Each of 17
  changes made on purpose to the module made a test fail. Its steering and handing over, which no
  test runs without a context, are checked in the recette.
- Not in this part: the free camera (9.2d), so that a left click still orbits; the water, the
  doodads and the buildings (9.6); the overlays; the lights of the map (9.7); the device lost; the
  cache of textures of `libs/gpu` shared with the models, the terrain keeping its own until then.

Where the work of the terrain is done:

| Work | Thread | Lock |
|---|---|---|
| Steering the loads, handing over within 2 ms, keeping to the budget | Interface, at each frame (`windows_ui`) | The scene shared with the layer, briefly |
| A tile read, its model, its resources created and uploaded | A job of the pool per tile, its chunks written over `parallel_for` | The cache of textures, briefly; a texture read once (`OnceLock`) |
| The tiles in sight, the camera written, the bundle recorded when they change | Interface, the layer's `prepare` and `draw` | The scene, in `prepare` |

Third part (9.2d), the free camera, with the hotkeys the user asked for and the GPU budget by
default:

- The hotkeys (`uniwow_api::hotkey`): a module declares each key it acts on with
  `Registrar::hotkey(name, label, kind, keys)`, as `<module>/<name>`, and keeps the handle returned,
  `Hotkey`. It tells whether its keys were pressed this frame (`pressed`: its modifiers exactly,
  the press then taken) or are held now (`held`: other modifiers allowed, but Ctrl). A hotkey of
  `HotkeyKind::Hold` may be modifiers alone; one of `HotkeyKind::Press` may not. No hotkey acts
  while a text field has the keyboard, nor while the user chooses new keys.
- The kernel keeps the registry: its own hotkeys, Undo (Ctrl+Z) and Redo (Ctrl+Y), whose keys the
  Edit menu shows, and those of the running modules.
- *Edit > Hotkey* lists them by module:
  - a click on the keys of one, then the new keys pressed, binds it to them: a key with the
    modifiers held, or, for a hotkey held, modifiers alone once released; Escape cancels;
    *Default* gives its own keys back;
  - two hotkeys bound to the same keys are said when they may act at once: of the same module, or
    one of them the kernel's, which acts anywhere.
- The settings keep, in `hotkeys`, the keys of the hotkeys bound to other keys than their own, by
  `<owner>/<name>`. Keys saved that cannot be read, or modifiers alone for a hotkey pressed, are
  said in the log and its own keys kept; a hotkey declared twice is left without keys.
- The keys of the modules became hotkeys: Space in the Timeline (play or pause), Delete and F
  (frame) in the Curves view, Delete in the dopesheet. Escape, which closes a dialog, and Enter,
  which runs the line of the Lua console, stay as they are: they belong to the dialog and to the
  field.
- The camera of `viewport`:
  - it flies with its hotkeys while the pointer is over the view: Z forward, S back, Q left,
    D right, E up, A down by default, for a French keyboard, egui naming a key by what it types;
    four times faster while Shift is held. Forward is where the view looks, up or down included;
    up and down follow Z;
  - the right or middle drag looks around, the eye staying; with Alt held, it turns around the
    target, as the orbit of step 8.3;
  - the wheel sets the speed, from 1 to 2,000 yards a second, 30 at start; the caption gives it
    with the keys bound;
  - a left click does nothing: it is kept for the tools to come;
  - the properties and commands of step 8.3 are unchanged.
- The memory of the GPU: wgpu does not tell it. The kernel reads it through DXGI, from the adapter
  of the same vendor and device as the one wgpu draws with, whatever its backend, logs it at start,
  and gives it to the modules (`Context::gpu_memory`). The crate `windows`, already in the runtime
  for wgpu with the same options, is re-exported by `uniwow_api`, through which the kernel reaches
  it (`cargo xtask check`).
- The terrain's GPU budget by default is half of it, or 1,024 MB when it is not told (the
  software adapter has no memory of its own). `gpu_budget_mb`, set by the panel from 64 MB to
  64 GB, still wins.
- On the user's machine (an RTX 3080 Ti, 12,084 MB of its own, drawn with through Vulkan): the
  budget by default 6,042 MB. The camera read in the Lua console (`viewport.camera`): Z held a
  second flew 31 yards along the view, Shift and Z 124, D 31.6 to the right, E 31 up; the right
  drag turned the view, the eye staying, and with Alt the eye around the target; a left drag, and Z
  held while the console's field had the keyboard, changed nothing. In *Edit > Hotkey*, *Fly
  forward* bound to W, which the caption said, then to S, said on both lines to be the keys of the
  other; W kept in the settings and read again at the next start; *Default* gave Z back and took it
  out of the settings. No entry in the history.
- Tests:
  - the keys written and read back; a hotkey pressed by exactly its keys, the press taken; held
    with Shift, not with Ctrl; none acting while suspended, while a field has the keyboard, nor
    without keys;
  - the keys saved applied, and only those changed saved; a hotkey declared twice, or pressed and
    bound to modifiers alone, left without keys; the conflicts, within a module or with the
    kernel, among the modules running;
  - in frames run as eframe runs them: a hotkey bound in its window, whose new keys act and are
    saved while the keys awaited act on nothing; Escape cancelling; Ctrl+Z awaited not undoing,
    said to be Undo's keys; a hotkey held bound to Shift alone, nothing held meanwhile, a hotkey
    pressed taking no modifiers alone; Undo and Redo on other keys;
  - the camera flying forward, right and up as seen, the eye and the target together, within
    reach; looking keeping the eye and turning the way of the drag, the orbit keeping the target;
    in frames: Z, Q, S, D, E and A flying their ways, Ctrl+Z not flying, the forward hotkey bound to
    W; flying only with the pointer over the view; a left drag doing nothing, a right drag looking,
    a middle drag with Alt turning around the target; the wheel setting the speed within its range,
    Shift flying four times faster;
  - each adapter of DXGI found by its ids and none by others, the software adapter without memory
    of its own; the budget by default half of it, 1,024 MB when not told, within its range.
  Each of 28 changes made on purpose to the hotkeys, their window, the camera and the budget made
  a test fail.
- Not in this part: the hotkeys of the compiled modules and of the scripts (the table of the
  capabilities).

Step 9.2e, the performance of the terrain, asked by the review of 9.2c and 9.2d: the user saw the
frames fall with the terrain alone, on an RTX 3080 Ti. A draw and a bind group a chunk made 256 draws
a tile, up to some 74,000 a frame at a distance of 8 tiles, and the bundle was recorded again at each
change of the tiles in sight; wgpu's work for each draw, on the interface thread, outweighed all.

- Measured first, the statistics of the view (*View > Statistics*, kept in the settings of
  `viewport`), over the last second: the frames a second and the time of a frame, with the longest;
  the time of the view on the interface thread, preparing the layers, recording their bundles and
  submitting; the time of the GPU on the pass of the view, by its timestamps when the device has
  them, read back some frames later without waiting (the kernel asks for them as for BC); for each
  layer, from `Layer::stats`, its draws, triangles, bytes on the GPU and what it counts its own way,
  and its time on the interface thread, steering, preparing and recording, the mean and the longest
  of each. The models to come will be measured with it.
- A draw per tile:
  - the textures of the terrain are layers of arrays, an array for each class of texture (format,
    size, levels), 12 of them bound at once (`textures`). An array grows by a copy on the GPU, from 4
    layers, doubling to 32, then 32 more, up to the limit of the device; a layer no tile holds is
    given to the next texture; an array that holds none is dropped;
  - the textures of each chunk, four codes of an array and a layer, are in a uniform buffer of its
    tile; the shader takes the array by a `switch`, sampling with the gradients taken before the
    branch, where only a gradient given allows it;
  - one bind group for the arrays, for all the terrain, one for each tile (its blending and its
    codes), one draw a tile: the 37,696 vertices of a tile with its skirts fit indices of 16 bits.
    Neither `binding_array` nor indirect draws: not needed for so few draws, and some adapters lack
    them.
- The interface thread never waits for the jobs placing textures: it reads the views of the arrays,
  their generation and their bytes as the jobs last published them, and a job of its own purges
  them. The workers of the pool run below the normal priority (T2): flying fast, the 16 workers and
  the interface thread shared the 16 cores of the user's machine, and the interface thread lost up
  to 8 ms at a time to them.
- Levels of detail, for each tile by the distance from the eye to its bounds: all the vertices under
  1.5 tiles; the outer ones, two triangles a quad, under 3; one outer vertex in two under 6; the
  corners of each chunk beyond. A level changes only 0.15 tile past its limit. The holes are left out
  at the finer levels; a coarser block only when all its quads are holes.
- Skirts, 40 yards deep under the four sides of each tile, at every level, facing out of it, close
  what opens between two tiles at two levels; `write_chunk` writes those of its chunk.
- The horizon:
  - the WDL of the map (`Formats::wdl`, read from the public description of the format; its rows
    checked against the tiles of the client: 0.5 yard apart on average, 51 yards taken the other way
    round), its 17 × 17 heights a tile in one mesh, built by a job when the map is shown;
  - one draw; the tiles drawn in detail are left out by a bit each, written when they change;
  - it fades into the fog within 1.5 tiles of a tile without heights, where it would end on the sky.
- The fog and the sky: the fog by the distance on the ground, half of it at the reach of the tiles
  loaded, where the horizon starts, all of it at the farthest of the map; behind everything, the sky
  in its colour; fixed colours until the lights of the map (9.7). The horizon ends in haze, as in the
  client, and `view_distance` can stay moderate, 3 by default.
- The bundle is recorded again when the tiles in sight, their levels or the arrays change: a few
  dozen draws, recorded in a few hundredths of a millisecond. No culling on the GPU, then.
- Measured on the user's machine (RTX 3080 Ti through Vulkan, 16 threads, 60 Hz with the vertical
  sync), on Azeroth at a distance of 8 tiles, flying at 888 yards a second (Shift held), the horizon
  to the edge of the map:

  | Target | Measured |
  |---|---|
  | At least 60 frames a second | 60, the vertical sync; the longest frame 20 to 22.5 ms |
  | The interface thread under 4 ms a frame for the terrain | 0.10 ms on average, 0.30 ms at most (steering 0.11, preparing 0.18, recording 0.13 at most) |
  | A few hundred draws at most for the terrain | 35 to 55 (the tiles in sight, the horizon, the sky), 0.6 to 1.1 M triangles |
  | — | The view on the interface thread 0.25 ms (preparing, recording, submitting), 1.4 ms at most; the GPU 0.6 to 1.7 ms a frame; 242 to 415 tiles loaded during the flight, 1.4 to 2.5 GB on the GPU of 6 GB |

  The same views drawn a chunk at a time would have made 9,000 to 14,000 draws: the tiles in sight
  times 256, which the statistics, new with this step, did not measure.
- At the edge of a map, the tiles of the ocean end on the sky, where the client draws its water: it
  comes with the water (9.6).
- Tests:
  - the levels of detail facing up, their triangles counted, the holes left out; the skirts upright,
    facing out, under the vertices they copy; a level by the distance, changing past its margin; the
    distance from an eye to a box;
  - the horizon a mesh of the heights of the WDL, facing up, placed as the tiles, its normals by the
    slopes, fading near the tiles without heights; the bits of the tiles drawn in detail;
  - the WDL read, a tile without heights left none, one whose heights cannot be found refused; on the
    client, its rows checked against those of the tiles;
  - on the software adapter, skipped where there is none: the textures in arrays by class, read once
    for every tile, dropped once no tile holds them; an array grown keeping its layers; the slots
    full; a tile drawn in one draw, its chunks textured from two arrays, read back from the GPU, the
    horizon beyond it, its own tile left out, the sky in the colour of the fog; the bundle recorded
    again when the level of a tile changes, kept otherwise; the GPU timed by its timestamps;
  - the statistics over a second; what each layer cost and drew, a bundle kept not recorded; the
    workers below the normal priority; the device asking for timestamps.
  Each of 21 changes made on purpose to the arrays, the meshes, the horizon, the layer, the
  statistics, the priority of the workers and the reading of the WDL made a test fail.
- After the step, at the user's request, `view_distance` goes up to 64 tiles, the side of a map,
  all of which it reaches from its middle; 8 at most before.
- Not in this part: the doodads, the buildings and the water (9.6); the lights of the map (9.7);
  editing the terrain, for which a chunk changed writes its vertices, its skirts and its blending,
  and the codes of its textures would be written again.

Step 9.2f, the terrain at a distance of 64, asked by the review of step 9.2e and of the distance of
64 (17f6ec0, 38d81dc): beyond its budget the terrain loaded and released the same tiles in turn, and
all of Kalimdor at 64 did not fit 1.5 GB.

- The budget kept without going back and forth, planned by `loading::plan`, a function of what it
  reads alone, on the interface thread:
  - the tiles wanted, the nearest first, each with its kind; the loads fill 90 % of the budget, the
    tiles held are kept up to all of it: between the two, a tile at the edge is neither loaded nor
    released;
  - no load is started that would take the terrain beyond its budget, its cost the mean of the
    tiles of its kind held, or 6 MB a full tile and 0.3 MB a light one before any;
  - when what is held, loading and missing goes beyond the budget, released are the tiles no longer
    wanted, the farthest first; then those the budget no longer keeps; then those beyond the share
    of the loads. Never a tile the loads want, nor one whose model holds changes;
  - a tile the budget leaves out is not wanted while the budget stays: the reach left is in the
    panel, in orange, and in the statistics (*reach limited by the budget: N tiles*), and the fog
    follows it;
  - the plan is made again only when what it reads changes: the camera by eighths of a tile, the
    distance, the budget, the tiles, the loads, the textures. Turning the camera changes none.
- Light tiles beyond 7 tiles from the camera, full again within 7 once beyond 8: the corners of
  their chunks only (1,152 vertices and 640 triangles with the skirts), their blending reduced to 16
  × 16 a chunk by the means of 4 × 4 texels, their textures those of the arrays; built by a job, no
  model kept: about 0.3 MB, against 6 MB a full tile. A tile changes kind by a job, the old one
  drawn until the new one is handed over. A tile whose model holds changes stays full. The models
  of the full tiles are counted in memory, and those left are freed by a job.
- Each tile goes to the GPU at once: its chunks built over `parallel_for`, then its vertices with
  their buffer and the 256 layers of its blending in one write, two operations of the queue where
  there were some 800.
- A deadlock in wgpu-core 30.0.1, found by the stacks of the threads of the window that stopped
  responding at start: `Queue::write_texture` holds the state of initialisation of the texture
  written while it takes the trackers of the device, `Queue::submit` takes them the other way round
  for the textures its frame draws. A job writing a layer of an array of textures while the
  interface thread submitted a frame drawing that array locked both, and every job waiting for the
  arrays behind it: one start in two at a distance of 64 once the tiles loaded faster. A texture is
  now placed by a copy from a buffer of its own that the job submits, under the lock of the arrays,
  before any array grows from it; a submission takes the locks in the order of the frames.
  `write_texture` writes only the blending of a tile no frame draws yet, and `write_chunk` is for
  the interface thread. The rule for the layers to come is in *Drawing many models*.
- The limits of the device: the kernel asks for as many layers in an array of textures as the
  adapter takes (256 by default in wgpu); the terrain is not drawn, the reason in the log, on a
  device that binds fewer than 13 textures at once (its 12 arrays and the blending). A
  texture refused for want of room is tried again when a tile asks for it once room is made; the
  statistics count the textures placed, unreadable and waiting for room, and the arrays used.
- The statistics of the view show the memory of the process, in memory and private
  (`GetProcessMemoryInfo`).
- The sky is drawn last, where neither the tiles nor the horizon drew; the tiles in sight are tested
  without allocating.
- Measured on the user's machine as in step 9.2e (its table above, at a distance of 8), at a
  distance of 64 from the middle of each map, all its tiles loaded: still, then flying at 888 yards
  a second for 8 seconds, the statistics over the last second of each of three captures:

  | Measured | Kalimdor | Azeroth | Northrend |
  |---|---|---|---|
  | Tiles loaded, all of the map, in | 988 (154 to 167 full, 821 to 834 light), 1.1 s | 753 (147 to 163 full, 590 to 606 light), 0.8 s | 1,131 (154 to 167 full, 964 to 977 light), 1.2 s |
  | Frames a second; the longest frame | 60; 18.5 to 20.4 ms | 60; 18.5 to 20.6 ms | 60; 18.0 to 20.4 ms |
  | The view on the interface thread | 0.22 to 0.72 ms, 1.01 at most | 0.20 to 0.51 ms, 0.76 at most | 0.24 to 0.46 ms, 1.69 at most |
  | The terrain on the interface thread | 0.06 to 0.38 ms, 0.72 at most | 0.04 to 0.26 ms, 0.65 at most | 0.06 to 0.30 ms, 0.73 at most |
  | The GPU a frame | 0.40 to 0.81 ms, 1.46 at most | 0.16 to 1.08 ms, 1.22 at most | 0.34 to 0.94 ms, 1.24 at most |
  | Draws of the terrain; triangles | 23 to 294; 0.99 to 1.34 M | 24 to 183; 1.05 to 1.16 M | 31 to 113; 1.03 to 1.20 M |
  | The terrain on the GPU (target: under 1.5 GB) | 1,269 to 1,340 MB | 1,116 to 1,203 MB | 1,328 to 1,400 MB |
  | The models of the full tiles in memory | 556 to 599 MB | 393 to 493 MB | 325 to 523 MB |
  | The process, in memory; private | 1,198 to 1,230 MB; 2,826 to 3,119 MB | 1,034 to 1,135 MB; 2,407 to 2,766 MB | 1,014 to 1,208 MB; 2,848 to 3,108 MB |
  | Textures placed; refused; arrays | 208; none; 9 of 12 | 205; one unreadable, a BLP1 the client does not read either; 9 of 12 | 320; none; 10 of 12 |

  With a budget of 600 MB on Kalimdor at 64: 84 full tiles, 535 MB, the reach limited to 5 tiles;
  nothing loaded nor released afterwards, still or turning (0.34 s of the processor in 5 s still).
  18 starts at 64, six on each map, all responding (one in two stopped before the deadlock was
  avoided).
- Tests:
  - the kind of a tile by its distance, changing past its margin, full while changed; the loads
    started nearest first, full near and light beyond, within their slots, those left cancelled;
    beyond the budget, the tiles not wanted released first, then the farthest, never those the loads
    want nor a changed one, no load started beyond it, the reach left told; the costs expected; a
    world of tiles under a budget smaller than they want, settling, then nothing loaded nor released
    for a hundred frames still or turning, and no tile released twice moving slowly;
  - the light mesh, its corners, skirts and blending reduced; on the software adapter, a light tile
    a fifteenth of a full one at most, drawn with its textures; a texture refused for want of room
    placed once room is made, one unreadable never read again; the device asking for the layers the
    adapter takes; the memory of the process in the statistics.
  Each of 11 changes made on purpose to the planning, the kinds, the costs, the textures waiting for
  room and the light blending made a test fail. The deadlock has no test: it was checked by the 18
  starts above.
- Not in this part: an occlusion on the GPU, kept for the models (*Drawing many models*); the
  budget shared by the view, designed above and built with 9.4.
- After the review of step 9.2f:
  - a load the plan no longer waits for, cancelled once done, of a kind no longer wanted or for a
    map left, is dropped when it ends, its model freed by a job; only the load waited for is handed
    over;
  - the private memory of the process, measured at a distance of 64, the maps changed by the
    panel, each measure once its tiles are loaded:

    | Shown | Private | In memory |
    |---|---|---|
    | No map | 663 MB | 441 MB |
    | Kalimdor, Northrend, Azeroth | 2,894, 3,179, 2,841 MB | 1,253, 1,258, 1,170 MB |
    | Kalimdor, Northrend, Azeroth again | 2,982, 3,202, 2,871 MB | 1,315, 1,274, 1,193 MB |
    | Kalimdor a third time | 2,984 MB | 1,310 MB |

    It does not grow at each change of map: no leak. What it holds on Kalimdor, from the regions of
    the process and the report of wgpu's allocator: 1,602 MB of pages write-combined, 158 MB of them
    in memory, which is the memory of the GPU Windows charges to the process, the 1,600 MB of the
    blocks of wgpu's allocator (8 blocks of 64 to 256 MB, 1,288 MB used in them; 1,614 MB dedicated
    by the counters of Windows); then 1,242 MB of the heap, 1,150 MB in memory, the models of the
    full tiles (556 MB) and the rest of the editor (366 MB before a map). The blocks, partly used,
    are kept when the map changes: that part stays as the first map left it;
  - left as the review allows: a texture waiting for room leaves the chunks of a tile already built
    white until the tile is loaded again (none measured).

Where the work of the terrain is done, since step 9.2f:

| Work | Thread | Lock |
|---|---|---|
| Steering the loads, handing over within 2 ms, keeping to the budget | Interface, at each frame (`windows_ui`), planned again only when what it reads changes | The scene shared with the layer, briefly; never that of the arrays of textures |
| A tile read, its model, its resources created and uploaded at once, its textures placed; a tile changing kind | A job of the pool per tile, its chunks built over `parallel_for` | The arrays of textures, while a texture is copied in, submitted, or an array grows; a texture read once (`OnceLock`) |
| The horizon of the map read and built | A job, its tiles over `parallel_for` | None |
| The textures no tile holds forgotten; the models of the tiles left freed | A job each | The arrays of textures; none |
| The tiles in sight and their levels, the camera and the bits of the horizon written, the bundle recorded when they change | Interface, the layer's `prepare` and `draw` | The scene, in `prepare`; the views of the arrays as published, briefly |

#### Step 9.3, proposed

The server of the user (decision of the user): the AzerothCore of `E:\azerothcore-wotlk`, at commit
bc9198ce7, built in `E:\Serveur`. The module stays in this repository, `server/mod-uniwow-observer/`,
and is seen by AzerothCore through a junction of its folder of modules; the worldserver is linked
again while stopped, and started and stopped for the checks once no real player is connected. Its
settings: `MapUpdate.Threads = 1`, `MapUpdateInterval = 10`, `PreloadAllNonInstancedMapGrids = 0`.

What the sources of AzerothCore at that commit say (read only, the lines are theirs):

- **The map's own update**: `AllMapScript::OnMapUpdate(Map*, uint32)` (`AllMapScript.h:97`) is
  called at the end of `Map::Update` (`Map.cpp:529`), on the worker updating that map
  (`MapUpdater.cpp:43-47`); the instances of a map are updated in parallel with each other
  (`MapInstanced.cpp:64-67`). The hook is skipped by the ticks without time (`Map.cpp:460-464`).
  The observer reads there, and only there: a map's objects are touched by its own thread.
- **Listing a zone**: `Cell::VisitObjects(x, y, map, visitor, radius)` (`CellImpl.h:171`), its radius
  capped to a grid, 533 yards (`CellImpl.h:79-80`), the grids not loaded skipped (`Map.h:783-785`).
  A visitor of its own for creatures, game objects and players needs no object to search from,
  whose phase would filter what it sees (`GridNotifiersImpl.h:469`).
- **Keeping a zone alive**: a grid once loaded stays loaded (`Map::UnloadAll` only, `Map.cpp:1026`);
  `Map::LoadGrid` is public (`Map.h:224`). But an object is updated only while it is in the map's
  update list, and is taken out of it every 30 seconds unless it is active, near a player, in
  combat, summoned, on waypoints or in a formation (`Creature.cpp:3940-3966`,
  `Map.cpp:546-568`); the cells near an object are marked only for players (`Map.cpp:491-502`). An
  active object keeps only itself updated (`Object.cpp:1086-1104`). `Map::AddObjectToPendingUpdateList`
  (`Map.h:553`), which the sight of a player calls (`PlayerUpdates.cpp:1710`), puts an object back
  at the next update (`Map.cpp:540-544, 583-593`). Respawns run over the whole map every 5
  seconds where its grids are loaded (`Map.cpp:467-476, 2817-2822`).
- **What each object gives**: its spawn id (`Creature::GetSpawnId`), summoned or not
  (`Unit::IsSummon`), its pool (`PoolMgr::IsPartOfAPool`), its game event (the public
  `GameEventMgr::GameEventCreatureGuids`), phase mask, display id, scale, walking or flying, its
  spline (`movespline->_Spline().getPoints()`: `MoveSpline::getPath` is protected), its name; for a
  game object its spawn id, display id and rotation.
- **The network**: Boost.Asio is in the tree; the main `io_context` is local to `main` and out of a
  module's reach, `AsyncAcceptor` is a header (`AsyncAcceptor.h`). A module runs its own
  `io_context` on a thread of its own, started at `WorldScript::OnStartup` (`Main.cpp:395`) and
  stopped at `OnShutdown` (`Main.cpp:419`), as SOAP runs on a thread of its own
  (`Main.cpp:337-345`). Its settings come from `configs/modules/mod_uniwow_observer.conf`, found by
  its `.conf.dist` (`modules/CMakeLists.txt:342-351`).
- **Instances**: `MapMgr::FindMap` reads without a lock, and an instance is destroyed by a worker
  (`MapInstanced.cpp:263-281`): the network thread never looks a map up; the subscription waits for
  the next update of the map that matches it. An empty instance unloads (`Map.h:178-188`).

Changes from the design above, proposed:

1. **No object spawned**: the observer loads the grids of the zone and keeps its creatures and
   game objects in the map's update list at each of its updates, as the sight of a player does.
   The creatures of the zone then move, follow their paths and respawn as near a player; nothing
   is created, nothing can be seen, selected or saved.
2. **A zone of one grid at most in this milestone**, 533 yards around the centre, the cap of
   `Cell::VisitObjects`; more would need several visits. Every grid the circle touches is loaded,
   up to 4 when the centre is near a corner of grids, by `Map::LoadGridsInRange(center, radius)`
   (`Map.h:226`).
3. **A dungeon or a battleground is shown while its instance exists**: the observer does not keep
   an instance alive.

Added by the review of the proposal:

- **The licence of the module**: AzerothCore is under the GPL 2.0 or later (the headers of its
  sources); a module compiled into the worldserver with its headers takes a licence compatible with
  it. `server/mod-uniwow-observer/` is under the GPL 2.0 or later, with its file `LICENSE`, and so
  are `PROTOCOL.md` and `tools/probe.py`, which go with it. The editor speaks to it only through
  the network protocol: two separate programs, and the licence of UniWoW stays the open question of
  section 10. Nothing of the code of AzerothCore goes into the editor.
- **The worldserver never falls nor waits because of the module**, which runs in the user's
  server:
  - no exception leaves the network thread nor the hook;
  - the size of a message, the connections and the subscriptions are bounded; a message malformed
    closes its connection, nothing more;
  - the thread of a map takes only the lock of its subscription, briefly, and never waits for the
    network;
  - a setting `Enable` in its `.conf` turns it off.
- **The cost of each update**: reading the zone, at the pace of the protocol (10 times a second),
  is apart from putting its objects back in the update list. If visiting the cells costs at each
  full update, the objects are put back from the GUIDs of the last reading (`Map::GetCreature`,
  `Map::GetGameObject`), not by a new visit. Both are measured.
- **Care on the user's server**:
  - `worldserver.exe` and its configuration are saved before the module is linked, and how to go
    back is written;
  - a check with `MapUpdate.Threads = 4` tries the lock of each subscription while maps update in
    parallel, then the user's setting is put back;
  - the documentation of the module says that the grids it loads stay loaded until the server
    starts again, as those a player crossed (`UnloadAll` only): flying over the whole world in the
    editor loads all it flew over.

The parts:

- **9.3a, the observer.** The protocol, written in `server/mod-uniwow-observer/PROTOCOL.md`: a
  little-endian binary framing, each message its length then its kind; the handshake (version of
  the protocol, token, capabilities, the commit of AzerothCore, the limits), the subscription (map,
  instance, centre, radius), the heartbeat, the snapshot then the changes 10 times a second (the
  setting), the fields of the components above. The module: its network thread, 127.0.0.1 only,
  a few connections at most; the subscriptions queued to the updates of their maps; at each update
  of a map subscribed, its zone read into plain records under a lock per subscription, no pointer
  of the game leaving the map's thread; the changes sent by the network thread. How to build it is
  written beside it. Checked on the server, with `tools/probe.py` beside the module (a client of
  the protocol in Python, printing what it receives): a zone without a player kept alive (creatures
  moving, respawning) and no longer once unsubscribed, disconnected or silent for 10 seconds; the
  time the observer takes in the update of a map, reading and keeping apart, on average and at
  most, in a city; the bytes a second in a city; a game master sees nothing new; messages corrupted
  and cut short, a client cut in the middle of a message, 100 connections and disconnections in a
  row, a subscription to a map or an instance that does not exist, the worldserver going on
  without an error; the same with `MapUpdate.Threads = 4`. The CI cannot build AzerothCore: the
  module is built on the user's machine only.
- **9.3b, the editor's side.** `libs/server-link` in the runtime (its fingerprint changes once): the
  client of the protocol, tested against a fake server in Rust that the CI runs. The module
  `live-world`: the connection on a thread of its own, the snapshot of the entities shared between
  threads, the subscription following the camera, the heartbeat, reconnection, the address, port
  and token in its settings, the states *server stopped* and *observer missing* shown; the commands
  that read (*the entities around a point*) and the batched events of L4; the table of
  capabilities.
- **9.3c, the markers.** The entities as markers, a coloured shape by kind and the name, drawn
  instanced in one layer; their moves interpolated between two updates by a thread of `live-world`
  woken by the frame signal; accepted on the user's machine with the server running.

#### Step 9.3a, as built

- `server/mod-uniwow-observer/`, under the GPL 2.0 or later (`LICENSE`, the text of AzerothCore's):
  `PROTOCOL.md` (version 1), `README.md` (what it does to the server, building, going back,
  checking), `conf/mod_uniwow_observer.conf.dist`, `src/` and `tools/probe.py`.
- The protocol: little-endian framing, each message its length, its kind and its body; HELLO,
  SUBSCRIBE, UNSUBSCRIBE and HEARTBEAT from the editor; WELCOME, REFUSED, STATUS, SNAPSHOT and
  CHANGES from the observer; each entity with the fields of *Components* above, the quaternion and
  state of a game object, and the spline it follows, 32 points at most, with its duration and the
  time already gone. A subscription unknown after 2 seconds is told so (*not found*).
- The module:
  - `Observer`, on the threads of the maps: at the end of each update of a map
    (`AllMapScript::OnMapUpdate`), for each subscription to that map and instance, the objects of
    the last reading, those sent and no more, put back in the update list from their GUIDs (`Map::GetCreature`,
    `Map::GetGameObject`, `Map::AddObjectToPendingUpdateList`); 10 times a second, the zone read by
    a visitor of the cells (`Cell::VisitObjects`), the circle applied, the nearest 2,000 kept, into
    plain records; the grids of the zone loaded when it changes (`Map::LoadGridsInRange`). The
    subscriptions are read from a list the network thread replaces whole
    (`std::atomic<std::shared_ptr>`); each is under its own lock, held to copy a few values.
  - `Server`, on a thread of its own: a Boost.Asio `io_context` on 127.0.0.1, started at
    `WorldScript::OnStartup` and stopped at `OnShutdown`. A message from the editor is 1,024 bytes at
    most, 4 editors by default (16 at most), 32 MiB waiting to be written at most; 10 seconds of
    silence close a connection. It encodes each entity and sends only those that changed since the
    last reading, and those that left.
  - No exception leaves the hook nor the network thread (each caught and logged); the token, empty
    by default, refuses every connection until it is set; `UniwowObserver.Enable` and
    `UniwowObserver.KeepAlive` in its settings; `UniwowObserver.StatsInterval` writes, per
    connection, the time of reading and of keeping, on average and at most, and the bytes sent.
- On the user's server: `worldserver.exe`, its `.pdb` and `configs` saved in
  `E:\Serveur\backup_before_uniwow_observer_20261005` with how to go back; the junction
  `E:\azerothcore-wotlk\modules\mod-uniwow-observer`; `cmake .`, the targets `modules` and
  `worldserver` built; `configs\modules\mod_uniwow_observer.conf` with a token. Stopped and started
  through SOAP once no player was connected; `MapUpdate.Threads` put back to 1 after its check
  (`worldserver.conf` the same as saved); the worldserver left stopped, as found.
- Measured with `tools/probe.py`, no player connected, the time in the update of a map from the log
  of the module:

  | Zone, radius, threads of the maps | Entities | Reading, 10 a second | Keeping, each update | Sent |
  |---|---|---|---|---|
  | Orgrimmar, 300 yards, 1 | 578: 331 creatures, 247 game objects | 0.25 to 0.28 ms, 0.52 at most (the first, loading the grids: 110 ms) | 0.14 to 0.16 ms, 0.92 at most | 37 to 40 KB a second |
  | Elwynn by Goldshire, 400 yards, 4 | 463 | 0.39 ms, 3.6 at most | 0.16 ms, 2.9 at most | 156 to 159 KB a second |
  | Shattrath, 400 yards, 4 | 887 | 0.61 to 0.72 ms, 3.5 at most | 0.25 to 0.35 ms, 4.1 at most | 40 to 45 KB a second |
  | Dalaran, 400 yards, 4 | 1,252 | 0.58 to 0.67 ms, 0.98 at most | 0.37 to 0.41 ms, 0.83 at most | 97 to 103 KB a second |

  The four zones were watched at once with 4 threads. The update of the world stayed at 9 ms on
  average, 27 ms at most with 1 thread and 21 ms with 4 (`server info`). Keeping runs at each update
  of the map, about 30 a second, from the GUIDs of the last reading: it costs about as much as
  reading. The bytes follow the creatures walking: each one moving is sent again at each reading
  with its spline.
- The zone kept alive: Elwynn without a player, two minutes. Kept, about 32,000 changes every 30
  seconds all along, 241 of its 330 creatures moved. With `UniwowObserver.KeepAlive = 0`, 25,800
  changes in the first 30 seconds, then 8,400 in the next 90: the creatures stop in the middle of
  their paths once the map's check of every 30 seconds takes them out of its update list.
- Survived, with 1 thread and with 4, the worldserver going on without an error in its logs nor a
  report of a crash (21 cases of `probe.py abuse`): a wrong token; a version not served; SUBSCRIBE
  before HELLO; random bytes before HELLO; a length of zero; a length too long; an unknown kind; a
  SUBSCRIBE too short and too long; a HEARTBEAT with a body; a second HELLO; a centre outside the
  world; a radius not a number; a negative radius; a client gone in the middle of a header and of a
  body; a map and an instance that do not exist (*not found*); 100 connections in a row, each
  welcomed and subscribed; one connection more than allowed (refused); a connection afterwards
  (welcomed). A silent connection was closed after 10.1 seconds; nothing came after UNSUBSCRIBE.
- Not checked: a game master in the client (nothing is spawned, nothing can be seen); a respawn
  seen (they run over the whole map every 5 seconds where its grids are loaded, *Step 9.3,
  proposed*). The CI does not build the module.
- After the review of step 9.3a, the protocol changed, still version 1 (no client used it yet):
  - **The zone follows the camera.** A SUBSCRIBE to the map and instance already subscribed to moves
    the zone: what was sent and what is kept stay, the grids are loaded when needed, and the next
    reading sends a CHANGES of what entered, left or changed. Only another map or instance starts
    again with a SNAPSHOT. However often SUBSCRIBE comes, the zone is read at most once in half a
    period, the first reading of a new map aside. The rule for the editor, in `PROTOCOL.md`: subscribe
    again once the centre moved more than an eighth of the radius, twice a second at most.
  - **A spline is sent when it starts.** An entity gives the id of its spline, Catmull-Rom or linear,
    cyclic and falling as flags, the time gone along it at the reading, and only the points it goes
    through, `first()` to `last()`, each with its time from the start (`length(i) - length(first)`,
    in milliseconds in `Movement::Spline<int32>`); beyond 32 points, a window from the segment it is
    on (`_currentSplineIdx()`). It is sent again only when its spline changes or another field does:
    its position, orientation and time along the spline are left out of the comparison, the editor
    places it from the times.
  - An error of `accept` that lasts waits 100 ms before the next one.
- Measured again on the user's server, no player connected:

  | Zone, radius | Sent before | Sent after, once read the first time |
  |---|---|---|
  | Elwynn by Goldshire, 400 yards (about 130 creatures moving, 1,250 splines started every 30 seconds) | 156 to 159 KB a second | 6.5 to 6.8 KB a second |
  | Dalaran, 400 yards | 97 to 103 KB a second | 3.7 KB a second |
  | Orgrimmar, 300 yards | 37 to 40 KB a second | 1.4 to 1.5 KB a second |
  | Shattrath, 400 yards | 40 to 45 KB a second | 1.6 to 1.8 KB a second |

  A zone of 300 yards moved at 100 yards a second for 30 seconds over Orgrimmar and Durotar (`probe.py
  move`, 3,003 yards): 57 SUBSCRIBE, 1 SNAPSHOT, then 148 CHANGES; 383 entities entered and 926 left;
  3.9 KB a second. Loading the grids it reaches costs up to 55 to 102 ms in the update of its map,
  as for a player crossing them. The time of reading and keeping in the update of a map is the
  same as before. The 21 hostile cases passed again, with 1 thread and with 4.

  The update of the world (`server info`, the last 500 updates, three readings 10 seconds apart,
  each line the mean, then the maximum), the same server with the module off for reference:

  | Threads of the maps | Module off | Module on, the four zones above watched and kept alive |
  |---|---|---|
  | 1 | 9 to 10 ms; 20 to 37 ms | 12 to 15 ms; 41 to 117 ms |
  | 4 | 9 to 10 ms; 17 to 36 ms | 8 to 11 ms; 31 to 44 ms |

  With one thread, four zones of some 1,700 creatures kept alive add 3 to 5 ms to an update of the
  world on average, and its longest updates; the creatures moving where no player is are most of it,
  which the module asks for. With four threads, the difference is within what the readings vary.
- Point to revisit, for the dungeons and battlegrounds: the editor cannot know the ids of the
  instances. A message listing the maps and instances updated in the last seconds (id, instance,
  players), filled in `OnMapUpdate` once a second for each map, would give them. Not needed while
  only the continents are shown in this milestone.

#### Step 9.3b, as built

- `libs/server-link`, the crate `uniwow-server-link`, part of the runtime as `uniwow_api::server_link`
  (its fingerprint changed once):
  - `protocol`: the messages of `PROTOCOL.md` both ways, encoded and decoded; a body read within
    its bounds, so that a message malformed is an error, never a panic, and a count that lies
    cannot ask for more memory than the body holds;
  - `Client`: blocking, every wait bounded (the connection, WELCOME, each read); a message cut
    anywhere is gathered; refused, closed, broken and the errors of the network told apart;
  - `fake::FakeObserver`: an observer on 127.0.0.1 that welcomes a token, keeps what it receives
    and sends what a test queues, whole or in pieces; the tests of the client and of `live-world`
    run against it, in the CI.
- The module `terrain` gives the map it shows, from any thread: the command `terrain.map` (its id in
  `Map.dbc`, its folder, its name; null while none is shown).
- The module `live-world`:
  - a thread of its own (`Context::spawn_thread`) holds the connection. Every 100 ms it reads the
    camera (`viewport/camera_position`) and the map of the terrain, and subscribes as `PROTOCOL.md`
    says: at once on another map, once the centre moved more than an eighth of the radius, twice a
    second at most; no map, it unsubscribes. A HEARTBEAT every 2 seconds when nothing else went;
    connected again 2 seconds after a loss, 10 after a refusal;
  - the snapshot of the entities is shared between threads, an `Arc` replaced whole under a brief
    lock, each entity shared between snapshots; an entity on a spline stands where the times of
    its spline say, counted on from when it was received (linearly between its points; the markers
    of step 9.3c follow the curves of the client);
  - the states are told apart and shown in its panel, *Live world*: no token; connecting; *server
    stopped*, when nothing listens on the port of the worldserver (8085) nor of the observer;
    *observer missing*, when the worldserver listens and the observer does not; refused, with the
    reason of the observer; broken; connected, with the version of the server, the zone and its
    entities counted by kind. Its settings: the port of the observer, its token, the port of the
    worldserver, the radius of the zone (300 yards by default, 10 to 533);
  - the commands of L4, on the calling thread from the snapshot: `live-world.state` (the
    connection, its message, the version of the server, the zone, the state of the subscription,
    the entities counted by kind) and `live-world.entities` (the entities within a radius of a
    point, the nearest first, of the kinds asked for, at most a limit, around the zone by default;
    each with its GUID as text, since JSON numbers do not hold 64 bits, its entry, its spawn, its
    name, its display, its position now, its orientation, its scale, its phase, its pool, its game
    event and whether it is temporary, dead or moving);
  - the event of L4, `live-world.changed`, one for each message of the observer that changed
    something: the map, the instance, the sequence and the GUIDs that appeared, changed and left.
    The entities going with a connection lost are said to leave.
  - Read only: nothing is written to the server, nothing goes to the history.
- Tests: the messages both ways read back as written; a body cut at every byte, too long, of an
  unknown kind, an entity of an unknown kind, a string not UTF-8, a count of four billion entities,
  each an error; the client welcomed, subscribing, reading a message cut in pieces of 7 bytes and
  two messages in one write; a wrong token, a connection closed and a message too long told apart;
  nothing listening. The world replaced, changed and emptied, each GUID told; the position along a
  spline, cyclic or not; the rule of subscribing again; the link against the fake observer
  following the camera and the map, keeping the entities, answering the commands, unsubscribing
  without a map, connecting again after a loss; no token, a wrong one, a server stopped and an
  observer missing told apart. Each of 12 changes made on purpose to the protocol, the client, the
  rule of subscribing again, the world, the commands and the states made a test fail.
- Accepted on the user's machine, with the worldserver of `E:` started and stopped as for step
  9.3a, no player connected: the panel connected, *active*, the entities counted; a script of Lua,
  not kept, put the camera over Orgrimmar: 315 creatures and 226 game objects within 300 yards, the
  five nearest given by `live-world.entities`; the camera moved 600 yards east, the zone followed
  (68 entities entered, 541 left); the worldserver stopped, *server stopped* and the entities gone;
  started with `UniwowObserver.Enable = 0`, *observer missing*.
- After the review of step 9.3b:
  - the port of the worldserver is looked at on the first failure to connect to the observer, then
    30 seconds later at most: each connection to it costs the worldserver a query of its database of
    logins (`WorldSocket::Start`); the observer is still tried every 2 seconds;
  - *Apply* retires the connection before: its entities are said to leave (`live-world.changed`) by
    the interface thread, through the queue of events of the module's `Editor`, the one its thread
    told them through, so that they come in order, before the new connection's; a connection retired
    changes and tells nothing more, the check and the telling under one lock;
  - the client keeps the buffer it reads into;
  - measured for the user's setting, `MapUpdate.Threads = 1`, one zone of 300 yards kept alive, no
    player connected, `server info` read six times 10 seconds apart (the mean; the longest):

    | | Mean | Longest |
    |---|---|---|
    | Module off | 9 to 14 ms | 22 to 51 ms |
    | Orgrimmar kept alive | 9 to 14 ms | 18 to 49 ms, one reading 85 ms |
    | Elwynn by Goldshire kept alive | 13 to 14 ms | 19 to 43 ms |

    The longest updates stay within those of the server without the module, one reading aside;
    the mean rises by some 3 ms in Elwynn, whose creatures walk at random. The module's own time,
    reading and keeping, is 0.55 ms at most in an update of the map: the rest is the updates of the
    creatures it keeps alive, what a player standing there would cost. The peaks of up to 117 ms
    measured with four zones at once (step 9.3a) come with four zones kept alive on one thread;
  - with step 9.3c: the time along a cyclic spline sent by a window of 32 points, which does not
    start at 0, wrapped as its curves are.

#### Step 9.3c, as built

- The service `viewport` gains `Layer::labels`, none by default: texts a layer writes over the view,
  each above a point of the world, with its colour, a few dozen at most; called after `prepare` and
  `draw`, inside `catch_unwind` as `stats` is. The view projects each through the transform of the
  frame, leaves out those behind the eye or out of the view, and writes them over its image on a
  dark ground.
- A thread of `live-world` placing the entities, or of `models` animating them, that panics is
  said in its panel until the user starts it again (*Apply*, *Animate again*), the entities still
  and the models at rest meanwhile; neither starts again by itself, which a panic at each start
  would make a loop of.
- The markers of `live-world`:
  - an octahedron each, standing on the position of its entity, its height that of a person by the
    scale of the entity, never smaller than a few pixels however far; orange a creature, blue a game
    object, green a player, violet a game master, grey the dead;
  - drawn in one instanced draw by a layer of `live-world`, from a buffer of the markers; its bundle
    is kept while the buffer stays, and recorded again when the buffer grows (256 markers at least,
    by powers of two). Its camera is written in `prepare`;
  - a thread of `live-world`, woken by the frame signal, places them where they stand when the next
    frame is shown, about a frame later by the time between two signals, and writes them itself with
    `Queue::write_buffer`, out of every lock the layer takes: drawn by the next frame. Those drawn
    before and not now are given a size of 0;
  - the names of the 48 nearest to the eye within 120 yards, by `Layer::labels`.
- An entity on a spline stands where the client puts it: linearly or by Catmull-Rom between its
  points, with the weights of AzerothCore (`s_catmullRomCoeffs`), and the points of control beyond
  each end as `InitCatmullRom` makes them: a cyclic spline sent whole goes on round; else a yard
  back from the first point the way it heads (AzerothCore takes the orientation the entity had,
  which is not sent), and the last point again. A spline sent by a window, which does not start at
  0, is not wrapped: it stops at the end of the window, which the observer sends again as the
  entity moves on (the point of the review of step 9.3b). The command `live-world.entities` gives
  the same positions.
- Tests: the weights against a straight line and a corner computed by hand; a cyclic spline round
  its loop, a window not wrapped; the markers standing on their entities, their colours, their
  scale, the nearest named, at most 48, none without an eye; on the software adapter, a marker
  drawn orange where it stands, in one draw, its buffer grown and recorded again, the markers no
  longer drawn given no size; the labels of the layers collected with the bundles; a label at the
  centre of the view for the point looked at, none behind the eye nor out of the view. Each of 10
  changes made on purpose to the curves, the markers, their buffer and the labels made a test fail.
- Accepted on the user's machine, the worldserver of `E:` started and stopped as for step 9.3a, no
  player connected: over Orgrimmar, 548 to 567 markers and 48 names, the creatures walking from one
  capture to the next; 60 frames a second still and flying; the layer of `live-world` 0.02 ms on the
  interface thread, 0.07 at most, the view 0.21 to 0.23 ms; one draw.
- After the review of step 9.3c (step 9.3 validated):
  - a buffer of the markers grown is made with its markers, then put in place, so that no frame
    draws it empty (by construction: the buffer is published only once filled; the test checks what
    it holds once grown);
  - point to revisit, when the protocol changes next: the observer could send the points of control
    beyond each end of a spline, `getPoint(first() - 1)` and `getPoint(last() + 1)`, and for a
    window the neighbours of its ends, two points more a spline, rather than the editor estimating
    a yard back the way the entity heads (`initialOrientation` is not sent): the curve would be
    exact, the edges of a window included.

#### Step 9.4, proposed

What is there: `formats` reads `CreatureDisplayInfo` and `CreatureModelData` (step 9.1b), not yet
the models; the verifications of step 9.1 found the reading of M2 of warcraft-rs wrong for 3.3.5a
(the particles, ribbons, cameras and lights of version 264 described wrongly, the skins read empty)
and two modern models (MD21) among the 23,155 of the user's client. The terrain keeps its own
budget (step 9.2f), and *GPU memory* above designs the one the view will share. The markers of
step 9.3c stand where the models will.

- **9.4a, the shared budget.** The service `viewport` holds the one budget of the view, its setting
  half the memory of the GPU's own by default (the terrain's `gpu_budget_mb` becomes it, read once
  from the terrain's settings when the view's is not set). At each frame, on the interface thread,
  each layer keeping resources on the GPU tells it what it takes outside its items and the bytes of
  its items held and wanted by distance, in quarters of a tile up to 64 tiles; the service gives
  back, at the next frame, the distance the loads fill 90 % of the budget to and the one all of it
  is kept to, and the reach the budget leaves. The planner of the terrain (`loading::plan`) takes
  those distances instead of computing them from its own budget; its tests and its behaviour at 64
  (step 9.2f) are checked again, alone on the budget. The statistics of the view give the budget,
  what each layer takes of it, and the reach left.
- **9.4b, the models read.** In `assets`, the M2 of 3.3.5a (version 264) and their skins
  (`00.skin` to `03.skin`): the parts of warcraft-rs copied and corrected where step 9.1 found
  them wrong; the modern ones WarcraftXL loads (`MD21` with `SFID`, `TXID`), translated from
  wow.export. `formats` gains a model as plain data: its vertices in the pose at rest, its skins
  (indices, submeshes, batches with their texture and blending), its textures (a file, or a type
  the display fills: the skins of a creature), its bounds; and the tables `GameObjectDisplayInfo`
  and `CreatureDisplayInfoExtra` (the baked skin of a character's look). Bones and animations come
  with step 9.5. Checked over every M2 of the client, as the terrain was over its tiles: how many
  read, how many refused and why, the time; and over the two modern ones.
- **9.4c, the module `models`.** Its service, in `core/api`: a module gives its instances (an id,
  a model by its file, the textures a display fills, a transform) and takes them back; nothing of
  the live world in it. A model is loaded once, by a job, whoever asks (a load in flight shared),
  its textures by a cache shared between threads, its resources created and uploaded by the job
  (T5), by copies a job submits where a texture may be drawn (the rule of step 9.2f). Drawn by the
  rules of *Drawing many models*: a draw per model and batch for all its instances, the instances
  in buffers written from a thread at the frame signal as the markers are; a skin by the distance,
  changing past a margin; each instance drawn up to a distance by its size; opaque and alpha-tested
  batches first, blended ones after, nearest last. On the shared budget: the models nearest kept,
  the farthest given back, the statistics of the layer (draws, triangles, models, instances).
- **9.4d, the live world as models.** `live-world` gives its creatures and game objects to `models`:
  a creature by `CreatureDisplayInfo` (its model of `CreatureModelData`, its three skins, its scale
  by both tables and by the entity); a character's look (`CreatureDisplayInfoExtra`) by its race's
  model and its baked skin, without its equipment; a game object by `GameObjectDisplayInfo` when
  its model is an M2 (a WMO waits for step 9.6). What has no model loaded, and the players, stay
  markers; the names stay over all of them. Accepted on the user's machine, the worldserver of `E:`
  started and stopped as for step 9.3a: Orgrimmar and Dalaran, the frames a second, the interface
  thread, the draws, what the budget gives the terrain and the models.

Risks, verified at the start of their part: how many M2 of the client the copy reads right (9.4b);
how many distinct models and batches a city draws, and the draws it makes (9.4c, 9.4d); whether the
terrain keeps its behaviour at 64 on the shared budget (9.4a).

Added by the review of the proposal, before 9.4b and 9.4c. The rules below were read in the
description of the formats (wowdev.wiki, its pages *M2*, *M2/.skin*, *M2/Rendering*, *DB/...*,
*Character Customization*), in wow.export (MIT, its bundled `app.js`), in AzerothCore at
bc9198ce7, and checked on the skins and tables of the user's client; WoW Model Viewer and Noggit
(GPL) were read for facts only, nothing of their code is taken.

- **Which submeshes (geosets) a display draws**: an M2 holds all its variants, which drawn at once
  overlap.
  - A creature: `CreatureDisplayInfo.CreatureGeosetData` (field 14 of 3.3.5a) chooses, when it is
    not 0, a variant in each of the groups 100 to 800: every submesh 1 to 899 is hidden, then the
    nibble *n* from the lowest, of value *v*, shows the submesh (*n* + 1) × 100 + *v* (the client's
    `ApplyMonsterGeosets`, decompiled on wowdev). In the user's client, 39 displays have it, all of
    `IronDwarf.mdx`, and every submesh they name is in its skin. When it is 0, every submesh is
    drawn: to check on the models of the client with several submeshes in one group, and with the
    user in the client.
  - A character's look (`CreatureDisplayInfoExtra`: race, sex, skin, face, hair style and colour,
    facial hair, eleven items, flags, baked skin): the hair by `CharHairGeosets` (race, sex,
    variation = the hair style), its `GeosetID` the submesh of group 0, 0 with `Showscalp` meaning
    bald; the facial hair by `CharacterFacialHairStyles` (race, sex, variation = the facial hair, no
    id column in 3.3.5a), its five values the submeshes of the groups 100, 300, 200, 1600 and 1700
    (group + value, 0 for none), which the user's 20 skins of race and sex confirm for 17 of them
    where the other orders and the + 1 of some viewers leave submeshes missing; the other groups at
    their bare variant, the hands (401), the feet (501) and the legs (1301), no sleeves, chest,
    pants, tabard, belt nor cape (their x01 does not exist, or 1501 would draw a cape). Its items
    are not drawn in this milestone. Open, to check in 9.4b and with the user: the ears (701 or
    702), the scalp of a bald look (submesh 1).
  - Anything else, game objects among them: the submesh 0 and the first variant of each group (an
    id ending in 01), as wow.export does.
  - Tested in 9.4b on known models: a human NPC by its look, and a creature with variants
    (`IronDwarf`), their submeshes chosen as the client's rule says.
- **The materials 9.4 renders, and what waits for step 9.5**:
  - each batch of a skin with its texture: the first of its texture unit, and the second where the
    client combines two, by its shader chosen at load as WotLK does (`shader_id` is 0 on disk and
    computed from the blending, the environment mapping and the second set of UV, or from the
    combiner combos when the model's flag 0x08 is set), its eight combiners (opaque, mod, decal,
    add, mod2x, fade, mod2x without alpha, add without alpha);
  - the eight blending modes of 3.3.5a: opaque; alpha key, tested at 224/255 of the alpha of the
    batch in WotLK; alpha; add without alpha; add; mod; mod2x; blend add, each with its blending
    and whether it writes the depth;
  - the render flags: unlit (0x01), unfogged (0x02), two-sided (0x04), without depth test (0x08),
    without depth write (0x10) (warcraft-rs names the last two wrongly: corrected in the copy); the
    fog of an added batch black, of a mod one white, of a mod2x one grey, mod and mod2x unlit;
  - the textures wrapped or clamped on each axis by their flags; a texture of type 11 to 13 is the
    skin of the display (`CreatureDisplayInfo` textures, in the folder of the model);
  - the colour and transparency of a batch at their value at rest, the first sequence at its
    start; a batch whose alpha is 0 is not drawn; `CreatureDisplayInfo.CreatureModelAlpha` (field
    5, 0 to 255) multiplies the alpha of every batch of the display;
  - with step 9.5: the animations of the textures, of their coordinates, of colours and
    transparency, and the bones.
- **Fog and light, the view's**: the colour and distances of the fog and the sun (direction,
  colour, ambient), constants of the terrain's layer today, become the view's: `View` gives them to
  every layer, the terrain and the models alike, so that a model far away fades into the same fog
  as the ground. The terrain sets the distances of the fog from its reach, as it does now; step 9.7
  sets the colours and the sun from `Light.dbc`, in the same place.
- **Instances that move**: the service `models` takes the transforms of a module's instances by a
  slice at a time, once a frame from the thread of the module (the thread animating `live-world`),
  and writes the buffers of the instances from that thread, as the markers are written: no call
  nor lock for each instance and each frame.
- **The scale**: an instance is drawn at the scale the server sends (`OBJECT_FIELD_SCALE_X`), times
  `CreatureDisplayInfo.CreatureModelScale` (field 4), times `CreatureModelData.ModelScale` (field
  4) for a creature. The server's scale never includes that of the display: AzerothCore sets it
  from `creature_template_model.DisplayScale` (`ObjectMgr.cpp:717-751`, `Unit.cpp:13166-13174`),
  a game object's from `gameobject_template.size`, and uses the same product for the size of a
  creature's collision (`Unit.cpp:17136`, `17174`); wowdev says the scales stack by multiplying.
  1,328 of the 1,331 models of the client have a model scale of 1. Checked in 9.4d on two or three
  creatures whose size in the client is known.
- **The modern models**: two in the user's client; the user prepares models of retail exported with
  wow.export (a creature, a humanoid NPC, an object, a model of many textures): where they are is
  asked at the start of 9.4b, and they are checked there.

#### Step 9.4a, as built

- `uniwow_api::viewport` holds the budget shared by the view:
  - `Demand`: what a layer takes outside its items (`fixed`), and the bytes of its items held and
    wanted, in 256 bands of a quarter of a tile (`BAND`, 133.3 yards) from the eye, the last band
    holding all beyond 64 tiles;
  - `Allowance`: the budget, what every layer holds, the distance the loads fill `LOAD_SHARE`
    (90 %) of the budget to, the one what is held is kept to, and the reach left when the budget
    holds fewer items than wanted;
  - `allow(budget, demands)`, pure: the fixed costs of every layer first, then the bands of all of
    them, the nearest first, whatever layer they belong to;
  - the service: `tell_budget(owner, demand)` answers at once with what the budget allows, with
    what every other layer told last, so that a layer never plans on an allowance that misses its
    own demand; `allowance()`; `set_budget(bytes)`. A layer removed, or whose module failed, is no
    longer counted.
- The module `viewport` keeps the budget, `gpu_budget_mb` in its settings, half the memory of the
  GPU's own by default (1,024 MB when not told), from 64 MB to 64 GB. The statistics of the view
  give *GPU budget of the view: N of M MB*, and the reach left when limited.
- The terrain is on it: its planner (`loading::plan`) loads the tiles wanted closer than the
  distance allowed for the loads, keeps those closer than the one allowed for what is held, and
  never starts a load beyond the budget less what the other layers hold; `loading::demand` tells
  the tiles held by their distance and the tiles wanted at the kind each wants, at the bytes they
  take when held so, at the cost expected otherwise. It tells when its demand changes, and plans
  again when what is allowed changes (the reaches by eighths of a tile, what the others leave by
  16 MB). Its panel sets the budget of the view; the terrain's own `gpu_budget_mb`, of before, is
  given to the view once, then cleared.
- Tests: the budget given by bands, the nearest first, shared by two layers, without margin
  between loads and keeps once nothing more fits, the fixed costs first, all fitting without a
  reach; the band of a distance; the service answering at once, shared, given back when a layer is
  gone, kept in the settings; half the GPU by default; the terrain telling its tiles held and
  wanted, refused ones aside; its loads within the reach allowed for the loads and not as far as
  the one for what is kept; the world of tiles of step 9.2f under a budget settling, still,
  turning and moving, now on the budget of the view. Each of 8 changes made on purpose to the
  budget, the service and the terrain's demand made a test fail.
- Measured again on the user's machine, as in step 9.2f: Kalimdor at 64, its 988 tiles in 1.1 s,
  1,269 to 1,340 MB of a budget of 6,042 MB, 60 frames a second; with 600 MB given by the terrain's
  setting of before, 500 MB of 600 held, the reach limited to 5.0 tiles, 78 tiles (84 before: the
  budget now counts by quarters of a tile), nothing loaded nor released afterwards, still or
  turning.
- After the review of step 9.4a, two rules for the layers to come (9.4c, 9.6):
  - an item counts in the band of its point nearest to the eye, by its bounds, as the levels of
    detail of the terrain do, not of its centre: a city (WMO) or a large model around the camera
    would otherwise count far away and be the first given up;
  - every layer whose resources on the GPU can pass a few MB tells its demand to the budget: the
    models (9.4c), then the buildings, the doodads and the water (9.6), and later a layer of a
    compiled module. The markers of `live-world` and the grid of the view, a few KB each, need not.

#### Step 9.4b, as built

- `uniwow_api::formats` holds a model as plain data, at rest: `Model` (its version and flags, its
  vertices, its textures, a file, a kind its display fills, or named by no file, with their flags
  of wrapping, its materials, the combos its batches index, its colours and weights at rest, its
  bounds, its skins, the finest first, and its faults), `Skin` (its triangles as indices of the
  vertices of the model, its submeshes, its batches), `Submesh`, `Batch`. The tables:
  `CreatureLook` (`CreatureDisplayInfoExtra`), `HairGeoset` (`CharHairGeosets`), `FacialHair`
  (`CharacterFacialHairStyles`), `GameObjectDisplay` (`GameObjectDisplayInfo`); `CreatureDisplay`
  gains its alpha (field 5) and its variants (field 14). `Formats` gains `creature_looks`,
  `hair_geosets`, `facial_hairs`, `game_object_displays` and `model`. The rules of the submeshes,
  pure: `creature_geosets`, `look_geosets`, `default_geosets`.
- The module `assets` reads them (`src/m2.rs`):
  - a model of 3.3.5a (`MD20`, version 264) or modern (`MD21`, versions up to 274), a path a table
    names `.mdx` or `.mdl` read as `.m2`; its skins, of 3.3.5a its views beside it (`00.skin` to
    `03.skin`), modern by the FileDataIDs of `SFID`, its views then its levels of detail (`_lod01`
    to `_lod03`), by the name of the model when no table of paths names them; its textures by the
    FileDataIDs of `TXID`;
  - at rest: a colour or a weight is the first key of its first sequence when the model holds its
    keys (the flag 0x20 of the sequence), or of its global sequence; white and opaque otherwise,
    the keys of an `.anim` file waiting for step 9.5;
  - what a batch refers to is checked once, so that its readers index without a check: a batch
    referring to what its model lacks is left out; a skin after the first that is missing or does
    not hold together is left out with the next ones; both are said in `Model::faults`. A model
    without its first skin is refused. A model without combos of coordinates, every modern one
    (unused since Cataclysm), takes its coordinates by its shader: its batches are not checked
    on them;
  - corrected from warcraft-rs: the triangles of a skin go through its list of vertices, a submesh
    starts at its `level` × 65,536 more, the render flags 0x08 and 0x10 are without depth test and
    without depth write (credited in `modules/assets/THIRD_PARTY.md`).
- Checked over every model of the user's client (`every_model_of_the_client_is_read_whole`,
  ignored, 16 threads): 23,190 models, 23,184 read, in 6.3 s the first time and 0.9 s once the
  system holds the archives in its cache; versions 264 (23,181), 272 (2) and 274 (1); 25,409
  skins, 102,652 batches kept, 5,437 models with combiners; 2,425 models whose triangles would be
  wrong without the skin's list of vertices; none past 65,536 indices. Refused 6, all for their
  first skin: 5 not in the client (`arthaslichking_unarmed2`, `druidcat`, `saberon_noweapon`...),
  and `FelBeastMount`, whose skin names its vertex 1,866 of 1,846. Said in their faults, with the
  checks added by the review (below): 39 batches left out, 27 to a weight (the monocles of helms),
  8 to texture combos (`KristallSpider`), 2 to combiners (`moltengiant`), 1 to a material
  (`westfallcabbage`), 1 to a colour (`jadeforestsky01`); 5 later skins missing (`ghoul`,
  `Jain3`...); the coordinates of 35 models completed. The first report counted 96 faults, 54 of
  them combos of coordinates of models that have none (`Varimathras`, `drakeold`), measured before
  such models were no longer checked on them. 39 models draw a texture named by no file: what the
  client shows there is to see with the user in 9.4c.
- Checked over the user's modern models, exported by wow.export (`UNIWOW_MODERN`): the 15 read
  whole, without a fault, in 17 ms; versions 272 (5) and 274 (10); their skins by `SFID` as
  wow.export named them in its manifest, 4 for those with levels of detail; their textures by
  `TXID`, each in the manifest: two creatures (`owldragonmount`, 33,813 vertices and 19 textures;
  `moirathaurissan`), a helm and two weapons, doodads, spells, a fire of particles only (no
  vertex).
- The tables of the user's client: 15,477 looks of characters, 370 hairs, 272 facial hairs and
  3,791 looks of game objects, read with the others in 14 ms; 9 of the 15,433 looks of characters
  the displays name are missing; every look of a game object an M2 or a WMO. 267 displays of 14
  models have variants, where 39 were counted above in the tables of the server: the patches of
  the client change `CreatureDisplayInfo`.
- The submeshes, on known models:
  - `IronDwarf`: its 39 displays all have variants, 6 distinct; each shows exactly the variant its
    nibble chooses in each group, nothing else below 900;
  - Marshal Dughan (display 1985 of `creature_template_model`, look 634: hair style 2, facial hair
    1): `HumanMale` shows 0, 3 (his hair), 302 (the facial value 2 of the group 300), 401, 501,
    702, 1301, 1801 and 2001; his values 1 of the groups 100 and 200 name 101 and 201, which the
    `HumanMale` of the user's client lacks (its facial hairs are 102, 202 and 302): checked after
    the review, below, and to compare with the client in 9.4c;
  - settled, as WoW Model Viewer dresses a character without items: the ears at 702, which the 20
    bodies have (701 only five men); a hair of no submesh shows the scalp, 1 (that `HumanMale`
    lacks: nothing is drawn); every other group at its first variant (x01), which keeps the
    sleeves, chest, pants, tabard and cape bare as said above, and draws the belt 1801 and the
    feet 2001 of the bodies that have them.
- Tests: on files the tests write, a model of 3.3.5a at rest (vertices, textures of the three
  sources, materials, combos, colours and weights at rest, bounds, combiners only with their
  flag), the keys of an `.anim` sequence not read, triangles through the skin's list, a submesh's
  level, each kind of batch left out and said, a model without its first skin refused and a later
  one left out, a modern model's skins and textures by FileDataID, what is not a model or a skin
  refused, a model or a skin cut anywhere refused without a panic, the paths of the tables; the
  tables of the four kinds; the rules of the submeshes; and, given the client and the folder of
  wow.export, the iron dwarves, Marshal Dughan, the tables of the client and the modern models.
  Each of 14 changes made on purpose to the reader, the tables and the rules made a test fail.

#### Step 9.4b, after its review

- Three more references of a batch checked, the batch left out and said otherwise, a test each:
  - its transform combos, one a texture and at least one, within `transform_combos` (no model of
    the client lacks them);
  - with combiners (the model's flag 0x08), a shader under 0x8000 is where the combiners of its
    textures start, within `combiner_combos`, as the client's choice of shader reads them (wowdev,
    *M2/.skin*, `sub_836980`); a shader from 0x8000 is already chosen, as a later client does, and
    no place. The 2 batches of `moltengiant` whose shader 16 points past its 2 combiners are left
    out;
  - its combo of coordinates within `uv_combos`, when the model has some.
- The values of the combos of coordinates: 1 the second set, 0xFFFF the environment, any other the
  first, as that choice of shader tests only for 1 and -1. Batches of 63 models of the client use
  another value, 2 in those looked at, many of 3.3.5a itself (the weapons of Drak'Tharon, the water
  of the Black Temple, the falls of Coilfang): kept.
- The coordinates of the next textures of a batch, past the combos of its model, are completed
  with the first set, as the client's choice takes one it does not find, and said in the faults:
  35 models brought back from later clients (`Skeleton`, `Voidlord`, skies, artifact weapons) give
  one combo for two textures; leaving those batches out would have cost 191 of them.
- The sweep of the client and the test of the modern models index every reference of every batch
  kept, as a reader does without a check: none is missing. Each of 5 changes made on purpose to
  these checks and to the completion made a test fail.
- The facial hair, checked against the files of 3.3.5a. The `HumanMale` the client reads is that
  of an HD pack (`patch-a.mpq`: 8,522,096 bytes, version 264, 16,271 vertices, 50 submeshes,
  without 1, 18, 101, 201, 301 nor 1501, with 1801 and 2001); that of 3.3.5a is in `patch-3.mpq`
  and `patch-2.mpq` (1,585,376 bytes, version 264, 5,264 vertices, 54 submeshes, 101, 201 and 301
  among them). The pack replaces `CharacterFacialHairStyles` too (`patch-frfr-u.mpq`, 272 rows for
  222), and other bodies (`patch-u.mpq` for `ScourgeMale`). The user confirms the pack.
  - With the table and the bodies of 3.3.5a, group + value finds every facial submesh of 17 bodies
    of 20, `HumanMale` 27 of 27 (+ 1: 15 of 27), Marshal Dughan's three (101, 302 and 201)
    included; `DwarfMale` 20 of 33, `NightElfMale` 15 of 24 (+ 1: 17) and `TaurenMale` 13 of 18 in
    part either way.
  - With the table and the bodies of the pack, group + value finds every one of 14 bodies of 20;
    `HumanMale` 12 of 27 (+ 1: 15), `ScourgeFemale` 17 of 18 (+ 1: 8), and four others in part.
  - Both readings stay open until the user's captures in 9.4c (Marshal Dughan from the front at
    Goldshire, an iron dwarf): the code reads group + value meanwhile, which the files of 3.3.5a
    confirm; the other reading is one line of `look_geosets`. The result is written here then.

#### Step 9.4c, proposed

What is there: `formats` gives a model at rest whose readers index without a check (step 9.4b),
the tables of the displays, and the rules of the submeshes; the view has its shared budget (9.4a),
its frame signal and its bundles kept (9.2a); `live-world` writes its markers from its own thread
at the frame signal (9.3c). Nothing draws a model yet, and nothing can show one display alone,
which the captures of the user need to settle the facial hair (*Step 9.4b, after its review*).

In two parts, each reviewed before the next:

- **9.4c1, the models drawn**, everything below but the combination of two textures.
- **9.4c2, the second texture**: the combiners of WotLK (the shader chosen at load from the
  blending, the coordinates and the combiner combos, as `sub_836980` and `sub_837680` do), the
  environment mapping, and the second set of coordinates; checked on the models of the client that
  use them (`FrostLord`, the weapons of Drak'Tharon, the falls of Coilfang) against captures.

**The service `models`**, in `core/api` (`uniwow_api::models`), shared between threads (T3):

- `Look`: a model by its file, the textures its display fills (a kind, 11 to 13 for a creature or
  1 for a character's baked skin, and a file), and the submeshes it shows: all, the default ones,
  a creature's variants (`CreatureGeosetData`), or a character's hair and facial hair (the
  numbers of `CharHairGeosets` and `CharacterFacialHairStyles`), so that a look is plain data,
  compared and hashed.
- `look(look) -> LookId`, from any thread: the same look the same id; nothing is loaded until an
  instance of it is placed.
- `display(id) -> (Look, scale)`, from a job: the look of a creature's display and the scale of its
  display and its model (`CreatureDisplayInfo`, `CreatureModelData`); for a display with a
  character's look (`CreatureDisplayInfoExtra`), the model of its race and sex (`ChrRaces`, its
  male and female displays), its baked skin (`Textures\BakedNpcTextures\`), its hair and facial
  hair. Its equipment is not drawn in this milestone. Step 9.4d gives the live world by it.
- `place(owner, instances)`: the whole set of `owner`'s instances for the next frame, each a look,
  a transform (scale included) and an alpha, from the thread of the module at the frame signal, as
  the markers are written: the service groups them by look and by tile, writes their buffer from
  that thread, and hands the groups to the layer under a brief lock; no call nor lock for each
  instance. `clear(owner)`; the instances of a module that fails are cleared, as the viewport
  removes its layers.
- `state(look)`: waiting, loading, drawn, or refused with why, so that 9.4d keeps a marker for
  what has no model.

**Loading**, by jobs of the pool, steered on the interface thread at each frame as the terrain is:

- A model is read once whoever asks, a load in flight shared; its skins built (a vertex buffer for
  the model, an index buffer for each skin, 16 bits when its vertices fit), its textures decoded
  through a cache shared between threads by file, a load in flight shared, those of 2048 texels
  and more reduced as the terrain does; all created and uploaded by the job (T5), the textures by
  copies the job submits (the rule of step 9.2f). The submeshes a look shows are chosen in the
  job, so that a look's batches are only those drawn.
- Wanted by the distance of the nearest group of its instances, at most *cores − 1* loads at once,
  paused while the view is not drawn; on the shared budget as *GPU memory* says, a look counted in
  the band of the nearest point of its groups' bounds (rule of step 9.4a); released beyond the
  distance kept, the farthest first, its textures when their last holder goes.

**Drawing**, the layer of the module, its bundle kept while what it draws stays:

- A group is drawn when in sight (its bounds, from its instances and the model's radius), at the
  skin its distance chooses, a level changing only past its limit by a margin, as the terrain's
  tiles; an instance only up to a distance by its size (its radius times its scale), the
  distance of the view scaling them all: the vertex shader drops those beyond, so that the
  bundle does not follow each instance. A draw per group and batch, all the group's instances.
- The materials of step 9.4 (*The materials 9.4 renders*): the first texture of each batch,
  wrapped or clamped by its flags; the eight blending modes, the alpha key at 224/255; the render
  flags unlit, unfogged, two-sided, without depth test, without depth write; the colour and
  transparency at rest, the weight at rest, the alpha of the display; a batch of alpha 0 not
  drawn. Opaque and alpha-keyed batches first, then the blended ones by group, the farthest first,
  and within a model by the priority and layer of its batches.
- **Fog and light, the view's**: `View` gains the fog (its colour, the distance it starts and the
  one it covers all) and the sun (its direction, colour and ambient); the viewport holds them, the
  terrain sets the distances from its reach (`Viewport::set_fog`), and both layers read them:
  the constants of the terrain move to the viewport, the terrain drawing as before.
- The statistics of the view: draws, triangles, looks drawn, loading, refused and their MB,
  instances and groups in sight, levels used.

**The preview**, for the acceptance and the captures: the panel of `models` takes a display id,
shows it before the camera, its look and scale as `display` gives them, and clears it; the command
`models.preview` does the same for a script, with a count to place a grid of that many for the
measures. Its owner is `models` itself.

**Accepted on the user's machine** (9.4c1): Marshal Dughan (display 1985) and an iron dwarf
previewed beside the user's captures in the client, the facial hair settled then; a grid of 1,000
of a creature, the frames a second, the draws and the interface thread; the terrain looking as
before under the view's fog. **Tests**: the grouping of instances by look and tile, the levels
and their margin, the reach by size, the order of the batches, the materials' states, the looks
of displays (a creature, a character, a variant), the cache of textures shared, a load in flight
shared, a module's instances cleared when it fails; on the software adapter, models drawn and read
back as the markers were.

Added by the review of the proposal, before the service is written:

- **A set kept**: `place(owner, instances)` keeps `owner`'s set until it gives another, not for
  the next frame only. An owner that moves its instances (`live-world`) gives them again at each
  frame signal; one that does not (the doodads of the terrain in 9.6, tens of thousands) gives
  them once, then when they change. **A partial update**, `change(owner, changed, removed)`: each
  instance carries an id of its owner's; those given replace the ones of the same id or join the
  set, the ids removed leave it.
- **Groups per owner**: a group is an owner, a look and a tile, and its buffers are written by the
  thread of its owner only: two modules placing the same look from two threads never write the
  same buffer. Each owner has its buffer of instances, its groups ranges in it.
- **A buffer never drawn half written**: a set whose groups keep their places in the buffer, as
  moving instances do, is written in place, out of the lock; one whose groups change is written
  into a new buffer created filled, then published with its groups under the lock, so that no
  frame draws new groups over an old layout or an empty buffer (the lesson of step 9.3c). A
  buffer grown is such a new buffer.
- **The blended batches**: their order, the farthest first, changes as the camera moves; it is
  sorted again only when two of them cross by more than a margin, and the statistics count the
  bundles recorded a second.
- Also needed by `display`, found while writing it: `CharSections`, for the texture of a
  character's hair (kind 6), by its race, sex, style and colour.

#### Step 9.4c1, as built

- **The service `models`**, `uniwow_api::models`: `Look` (a model by its file, the textures its
  display fills by kind, the submeshes it shows: all, the default ones, a creature's variants, a
  character's hair and facial hair), `LookId`, `Instance` (an id of its owner's, a look, a
  transform with its scale, an alpha), `LookState` (waiting, loading, drawn, refused with why),
  and `Models`: `look`, `display`, `place`, `change`, `clear`, `state`, as proposed with the
  additions of its review.
- **`display`**, in the module (`src/display.rs`), from a job: a creature's look, its skins in
  the folder of its model, its variants or all its submeshes, its scale that of the display times
  that of its model, a scale of 0 in a table read as 1 (the client's table has some); a
  character's look, whose display names the model of its race and sex itself, as all the 15,451
  of the client do (`ChrRaces`, proposed, is not needed), its baked skin in
  `Textures\BakedNpcTextures`, the texture of its hair from `CharSections` (its section 3 of its
  style and colour, read in `formats` for this), its hair and facial hair from their tables.
- **Loading** (`src/cache.rs`, `src/loading.rs`, `src/gpu.rs`), by jobs of the pool:
  - the models and the textures in two caches shared between threads, by their file (a path in
    lower case, `.mdx` and `.mdl` as `.m2`; a FileDataID by its number): a load in flight shared,
    a value kept while a look holds it, read again once none does, one refused not read again, a
    load that fails letting those waiting for it go;
  - a model's vertices in a buffer (position, normal, first coordinates, 32 bytes), each skin's
    indices in one of 16 bits when its vertices fit, 32 otherwise, made filled; a texture as BC
    when the device takes it, filled by a copy the job submits, its levels larger than the device
    takes left out (the terrain reduces none, against what the proposal said);
  - a look's batches: those of the submeshes it shows (`formats`' rules) and seen at rest (alpha
    times weight above 0), the planes of priority first, then their order in the skin; each with
    its first texture (a file, the one its display fills, or white; one unreadable drawn white
    and said once), its sampler by the wrapping of its texture, its colour at rest, its flags and
    the radius of its model in a uniform; the pipelines of the states of the materials (blending,
    two-sided, depth test, depth write) made by the first job needing each.
- **The owners' sets** (`src/groups.rs`), as the additions say: kept until replaced; `change`
  merges by id and groups the whole set again on the caller's thread, its cost growing with the
  set, to measure with the doodads of step 9.6; written in place when the groups keep their
  places, into a new buffer made filled and published with its groups under the lock otherwise;
  none before the view has its device, the set kept and written by its next change.
- **Steering**, on the interface thread at each frame while the view is drawn: the nearest group
  of each look placed, by its bounds; the looks wanted within the distance the budget gives for
  the loads, the nearest first, at most *cores − 1* loads at once; a load no longer wanted
  cancelled; a look released, by a job that drops it and purges the caches, beyond the distance
  kept or once no instance of it is placed. The demand told to the budget counts each model and
  texture once, in the band of the nearest look holding it, a look wanted at the mean of those
  held (1 MB before any).
- **Drawing** (`src/layer.rs`, `src/models.wgsl`): a group drawn when in sight and within its
  reach, at the skin of its distance in radii (limits 40, 80 and 160, kept 10 % past them); an
  instance drawn up to `reach` times its radius times its scale, at least 1 yard (`reach`, the
  setting of the panel, 100 by default, from 10 to 1,000), the vertex shader dropping those
  beyond and those of alpha 0; the materials of step 9.4 but the second texture: the eight
  blendings, the alpha key at 224/255 of the texel, unlit, unfogged, two-sided, without depth
  test or write, a blended batch never writing the depth, mod and mod2x unlit, the fog black,
  white or grey for the added, mod and mod2x ones; opaque and alpha-keyed batches first, then the
  groups with blended ones, the farthest first, sorted again only when two cross by 2 yards or
  5 % of their distance. Its statistics: draws, triangles, looks on the GPU and their MB,
  loading, waiting, the models and textures held, those unreadable, the instances and groups in
  sight, the levels, the bundles recorded in the last second.
- **Fog and sun**: `View` has `fog` (its colour, start, middle and end) and `sun` (direction,
  colour, ambient, those of the terrain before); the viewport holds the fog, the terrain sets it
  by `Viewport::set_fog` at each frame from its reach as before (more than half at the reach of
  its tiles, all at the farthest of the map); the terrain's shaders and the models' read them.
- **The preview**: the panel takes a display and a count, *Show* and *Clear*, and the command
  `models.preview` (`display`, `count`, 0 to clear); a job reads the display and its model, then
  places the instances as the owner `models`, the first twice its height before the camera, its
  middle at the height of the eye, facing it, a grid of its size between them, the alpha of the
  display applied. Its size is that of its vertices at rest: the bounds of a model hold its
  animations too (5.5 yards high for `HumanMale`).
- **Measured on the user's machine** (RTX 3080 Ti): Marshal Dughan (display 1985) drawn at
  Goldshire with his baked skin and tabard, his hair 3 and the moustache 302 of the rule group +
  value, a goatee in his baked skin (captures `c1_dughan6_view.png`, `c1_dughan6_face.png` in the
  work folder); an iron dwarf (display 25748) with its variants and its runes; a grid of 1,000
  Marshal Dughans: 60 frames a second, 14 draws, 7.28 M triangles, the GPU 2.8 to 3.0 ms a frame,
  the interface thread 0.15 to 0.23 ms, still or flying, the bundle recorded 0 times a second while
  flying over them. The terrain looks as before under the fog of the view.
- **Tests**: the cache (a load in flight shared by eight threads, a value read again once
  released, one refused not read again, a failed load letting its waiter go); the grouping, the
  partial update, the places kept; the service (ids, states, a set kept, an owner cleared, the
  instances of a failed module cleared); the levels and their margin, the blended order and its
  margin, the nearest point and the sight; the states and flags of the materials; the looks of a
  creature, a variant, a character; the submeshes shown; the fog set given with the view; the
  table `CharSections`, written by the test and the client's (10,060 rows, a hair for each of the
  20 bodies); and on
  the software adapter, a model drawn where its instance stands, lit and one-sided, from behind
  when two-sided, an instance beyond its reach not drawn nor one beyond its own reach in a group
  drawn, an alpha-keyed texel under 224 not drawn, a blended instance letting what is behind
  through, the batches a look keeps, the buffers written in place or made new. Each of 16 changes
  made on purpose to the module made a test fail.
- Open: the facial hair, settled with the user's captures; a group's skin follows its nearest
  instance, so that the far instances of a near group are drawn at the finest (7.28 M triangles
  for the grid), to revisit with the doodads; the second texture and the combiners in 9.4c2.

#### Step 9.4c1, after its review

- **Point to revisit before step 9.6, chosen before it is built and measured with its doodads:
  the level of detail and the reach of each instance.** Today the skin of a group follows its
  nearest instance, so that the far instances of a near group are drawn at the finest (7.28 M
  triangles for the grid of 1,000), and the reach by size is tested in the vertex shader, so that
  an instance beyond it still runs all its vertices: with tens of thousands of doodads, neither
  holds. Two ways, to choose between then:
  - at each frame, on the thread of the owner at the frame signal or on a thread of `models`, the
    instances of each group in sight sorted by level, those in sight and within their reach only,
    into a list of indices (a `u32` an instance seen, not its 64 bytes) the shader reads from a
    storage buffer; a draw per group, level and batch;
  - or a compute pass culling the instances and writing indirect draws, with a way back for a
    device without them.
- **The alpha, as WotLK tests it** (wowdev, *M2/Rendering*, *Alpha Testing* and *Element
  Alpha*): the alpha of a pixel is the alpha of the batch at rest (its colour and its weight)
  times that of the instance, times that of the texel unless the batch is opaque (the shader of
  an opaque batch of WotLK takes the alpha of the vertex only); it is drawn when that alpha is
  at least the reference: 224/255 times the alpha of the batch and the instance for an
  alpha-keyed batch (128/255 from Cataclysm on, not taken), 1/255 for every other blending. The
  key of before, on the texel alone, was the same for an alpha-keyed batch; a texel of alpha 0 of
  an added or modulated batch is now left out, as the client leaves it. Tested: an opaque batch
  drawn whatever the alpha of its texel, an added one without alpha not for a texel of alpha 0,
  an alpha-keyed one at 230 and not at 200.
- **A device lost**: the device of the view is taken once (`Service::gpu`); a device made again
  after a loss is not taken, and the models ask for a restart of the editor then, as the terrain
  and the markers do (only the bundles of the view are recorded again on a new device, and eframe
  makes none after a loss today).
- **The facial hair, without the captures** (the user goes on without them): the rule group +
  value is kept. The rule is in Wow.exe and applies to the files of the HD pack as they are; it
  finds every facial submesh of 17 bodies of 20 with the table and the bodies of 3.3.5a, among
  them Marshal Dughan's three (101, 302, 201); what it misses with the pack (101 and 201 of
  Marshal Dughan in its `HumanMale`) the client misses too. The preview of display 1985 shows his
  moustache 302 and a goatee of his baked skin. Should a capture of the client show a beard where
  the rule finds none, the rule changes, not the files.

#### Step 9.4c2, as built

- **The shader of each batch** (`src/shaders.rs`), chosen when its look loads, as WotLK chooses
  it: translated from the implementation of the Wowser project published on wowdev (*M2/.skin/WotLK
  shader selection*, MIT, its notice in `modules/models/THIRD_PARTY.md`):
  - `sub_836980`: a model without combiners (its flag 0x08) takes the shader of a batch from its
    blending and its first coordinates (a blended batch on the environment marked so, the second
    set flagged); a model with them takes the combiner of each of its one or two textures at its
    shader in `combiner_combos`, the first of an opaque batch opaque, one on the environment
    marked; a shader from 0x8000 is kept;
  - `sub_837680`, on a skin of more than one layer: the layers of a submesh merged into its first
    where WotLK draws them in one, each of the same weight, the alpha layers of the same unlit
    flag: after an opaque layer of two textures whose second is mod2x on the environment, an alpha
    layer of its first texture (`Opaque_Mod2xNA_Alpha`); after an opaque layer of one texture on
    the first set, an added or mod2x layer on the environment (`Opaque_AddAlpha`, or the shader
    0xE for mod2x, `Opaque_Mod2xNA`), then an alpha layer of the first texture
    (`Opaque_AddAlpha_Alpha`). A layer merged is not drawn; the batches sharing the material of
    the one before take what it became;
  - the names of its shaders as a pixel shader, one of the 23 of WotLK the models use, and the
    coordinates of each texture (the first set, the second, the environment); a pair of
    combiners WotLK has no shader for takes that of 0x11, as it does. For one texture, the
    coordinates 0 are the first set and any other the second, as the tables of the names read
    them; step 9.4b said "any other the first", from `sub_836980` alone, and the documentation of
    `Batch` says so now.
  - Only for the models of 3.3.5a (version 264): a batch of a later version (the models brought
    back from later clients) is drawn with its first texture alone, opaque or modulated by its
    blending, as is one WotLK names no shader for. A model of 3.3.5a without combos of coordinates
    takes the first set.
- **The vertices** carry both sets of coordinates (40 bytes).
- **The pixel shader** (`src/models.wgsl`): its two textures sampled on the coordinates chosen and
  combined by the formulas of wowdev (*M2/Rendering*) in gamma, as WotLK combines them (mod2x
  doubles around 0.5): the textures are read as they are stored (`Unorm`, `UnormSrgb` before);
  the alpha tested on the combined alpha as step 9.4c1 says; then the colour made linear for the
  target of the view, lit and fogged in linear as the terrain. A batch of one texture draws what
  9.4c1 drew. A batch of mod2x blending writes its colour such that the doubling the target makes
  in linear gives the doubling in gamma, `(2c)^2.2 / 2`, as far as a power of 2.2 follows the
  curve of sRGB (62 for 64 in the test), grey 0.5 in the fog.
- **The environment** (wowdev, *M2/.skin*): the position and the normal in the space of the
  camera of the client (across, up, away from the eye), the direction from the eye reflected on
  the normal, its depth plus 1, normalized, its first two halved and moved by 0.5, at each vertex;
  the axes of the camera come from the rows of `view_proj` (the first two, minus the fourth), in
  the camera of the shader (52 floats).
- **The depth tested or equal**, against 9.4c1: a layer of a submesh lies on its first, at the
  same depth, and failed the test (`Greater`), as the test of a mod2x layer shows; the glow of
  the iron dwarf, an added layer on the submeshes of its skin, is such a layer. The client draws
  them: with the reversed depth of the view, `Greater` is taken as `GreaterEqual`.
- **The preview** takes the file of a model too: the field "or the model" of the panel and `model`
  in the command `models.preview` (a path), its default submeshes, without the textures a display
  fills (drawn white).
- **Seen in the editor**, the user going on without captures of the client:
  - `FrostLord` (display 23344): its two textures and its glows on the environment;
  - Marshal Dughan (display 1985): as in 9.4c1;
  - the iron dwarf (display 25748): its alpha layer merged into its first
    (`Opaque_Mod2xNA_Alpha`, the reflection `ORBREFLECT` where the alpha of its skin lets it),
    dark as its skin `IronDwarf_Dark` is (mean 36, 37, 45 of 255) and as 9.4c1 draws it: e014979
    built again and drawn at the same place gives the same. The silver of a capture taken during
    the work on 9.4c1 (`c1_irondwarf_view.png`, before aaaf1f0) came from a state not kept;
  - the mace of Drak'Tharon (`Mace_1H_DrakTharon_D_01`): its first texture is filled by
    `ItemDisplayInfo`, not read yet; drawn white, its reflection on it;
  - the falls of Coilfang (`Coilfang_waterfall_Type1`): loaded (15 textures, 6 MB) but not seen
    at rest: their waters are dark blue with little alpha (a mean alpha of 33 to 84 of 255), which
    the client moves and stacks by their texture transforms, not drawn yet.
- **Tests**: the shader of a batch alone from its blending and its coordinates; the combiners of
  a model, the first of an opaque batch opaque, a pair without a shader taking that of 0x11; an
  added or mod2x layer on the environment merged, then an alpha layer, one of its own weight or
  on the first set apart; an alpha layer of the first texture of two merged; a shader already
  chosen kept, a later model neither chosen nor merged, a shared material copied, a skin of one
  layer neither merged nor copied; and on the software adapter, unlit and unfogged so that a
  pixel is the colour combined: opaque then mod2x (128 and 64 give 64) and opaque then added
  (192), a mod2x layer drawn on its first (62), the second texture on the second set, the
  coordinates of the environment those of the sphere map at the vertices (within 6 of 255), the
  merged layer of the iron dwarf (128 where its skin is opaque, 64 where not). Each of 21 changes
  made on purpose to the selection, the shader, the loading and the camera made a test fail.
- Open: the texture transforms and the animated colours (the falls of Coilfang), with the
  animations; the textures of items (`ItemDisplayInfo`); the logic of `sub_876530`, which Wowser
  leaves to do (a shader thrown back to the table), not taken.

#### Step 9.4c2, after its review

- **The axes of the camera given by the view**: `View::view`, the view alone (from the world to
  the camera), whose first three rows are the axes across, up and back; `models` reads them there.
  They were taken from the rows of `view_proj`, right only for a symmetric perspective: the fourth
  row of an orthographic projection, as a view from above for editing will be, is (0, 0, 0, 1),
  and the axis back came out null. Tested: the same axes under a perspective and an orthographic
  projection.
- **Gamma and linear**: the textures are combined in gamma, as WotLK combines them, but the light
  and the fog are applied in linear, as the terrain applies them since 9.2c, where the client
  lights in gamma. A slight difference of tint with the client would come from there; to look at
  again with the lights of the map (9.7).

#### Step 9.4d, proposed

What is there: `live-world` keeps the entities of its zone (step 9.3b), each with its kind, its
display, its position or its spline, its orientation (0 while it moves: the spline gives it), the
scale the server sends (`OBJECT_FIELD_SCALE_X`), a game object's rotation (`GetWorldRotation`, its
orientation included) and its name; a thread of its own, woken by the frame signal, writes their
markers and names (9.3c). `models` gives the look of a creature's display and its scale, loads a
look once an instance of it is placed, within the distance the budget gives, and draws each
instance up to its reach (9.4c). In Orgrimmar, 315 to 331 creatures and 226 to 247 game objects
stand within 300 yards (9.3a, 9.3b).

- **A look for each entity**, by its display, read again when the display changes (a morph):
  - a creature by `Models::display`: its look and the scale of its display and model, times the
    scale the server sends;
  - a game object by `GameObjectDisplayInfo`, through a new `Models::object(display) ->
    Result<Option<Look>, String>`: the look of its M2 (the default submeshes, its textures named by
    the model), none for a WMO, which waits for step 9.6; drawn at the scale the server sends;
  - a player keeps its marker: its look needs its customization and its equipment, which the
    observer does not send;
  - the tables are read by jobs of `live-world`, a display once (its look and scale kept, or why it
    failed), the displays seen for the first time gathered into one job.
- **The instances**, owned by `live-world` and given by `Models::place` at each frame signal from
  the thread that writes the markers, as the service asks (one thread an owner): an entity's id its
  GUID; its transform its position along its spline at the time of the next frame (as the
  markers), turned by its orientation about Z for a creature, by its quaternion for a game object,
  at its scale. A creature moving faces the way it goes along its spline, as the client turns it;
  its orientation is taken again once it stops. The instance of an entity whose look is not drawn
  is placed all the same: that is what makes `models` load it.
- **The markers kept** for what is not seen as a model: the players; an entity without a look (a
  WMO, a display the tables do not have, a model refused); one whose look is waiting or loading
  (beyond the distance of the budget, or its turn to come); one beyond the reach of its model; one
  whose model draws nothing at rest (the invisible models of the triggers, which the client does
  not show and an editor must). For this the service gives, once a look is read, a new
  `Models::extent(look) -> Option<Extent>`: the bounds of its vertices at rest, the count of its
  batches seen at rest, and how far an instance of scale 1 is drawn (the setting `reach` times its
  radius).
- **The names** stay over every entity: over a model at the top of its vertices at rest, times its
  scale; over a marker as today.
- **Not in 9.4d**: the animations (a creature drawn at rest, a dead one standing, its name grey as
  today; the state of a door), the equipment and mounts, the players' looks, the WMO (9.6), the
  transports moving with what they carry.
- **Checked at the start**: in Orgrimmar and in Dalaran, with the worldserver of `E:` as for step
  9.3a, the displays the observer sends: how many distinct, how many the tables have, how many game
  objects are a WMO, how many creatures draw nothing at rest; that `Models::place` of the whole set
  at each frame keeps the instances written in place while none crosses a tile, and what it costs
  the thread of `live-world`.
- **Accepted on the user's machine**, the worldserver of `E:` started and stopped as for step 9.3a
  (no player connected, checked by SOAP; the server left as found): Orgrimmar and Dalaran (its city
  is a WMO, not drawn before 9.6: the entities stand in the air over Crystalsong), the creatures
  and game objects as models, the markers left; the frames a second, the interface thread, the
  thread of `live-world`, the draws and triangles, what the budget gives the terrain and the
  models; the scale checked on two or three creatures whose size in the client is known, against
  the captures the user may give.
- **Tests**: the look of each kind of entity and the scale; the transform of a creature, a moving
  one and a game object; which entities keep a marker; the name over a model; a display read once,
  a failed one not read again; a morph; against a fake service `models`, and the extent of a look
  in `models`.

#### Step 9.4d, added by the review of the proposal

- **The draws of a city.** Each look of a character (`CreatureDisplayInfoExtra`) has its own baked
  skin, so that each humanoid NPC is a look of its own, without instancing: a hundred distinct NPCs
  in Orgrimmar, at 8 to 15 batches a body once its submeshes are chosen, can make more than 1,000
  draws for the layer of the models alone, and wgpu replays a bundle command by command in the pass
  at every frame on Vulkan (the submitting of the view shows it).
  - Checked at the start, in Orgrimmar and in Dalaran: the distinct looks, the looks of
    characters, the batches drawn and the draws.
  - Accepted: the draws, the time of recording the bundle and the time of the view on the
    interface thread (preparing, recording, submitting), flying over the city. The goal of step
    9.2e holds: a few hundred draws a layer, the interface thread under 4 ms.
- **Point to revisit, should the measure go past that goal** (built now only if it does): the
  baked skins of the characters of one race and sex in arrays of textures, an index of layer for
  each instance, so that every NPC of one body is drawn as one group; the submeshes that differ
  (hair, beard) by a mask of submeshes for each instance, or by groups of the looks of the same
  submeshes.

#### Step 9.4d, as built

- **Checked at the start**, by a probe not kept (a test of `assets` subscribing to the observer of
  the worldserver of `E:`, already running, for 300 yards, and reading the tables and models of
  the client); the draws counted as if every look were loaded and drawn, skin 0, a group a look and
  tile:

  | City | Entities | Distinct displays | Looks (of characters) | Models | Groups | Draws |
  |---|---|---|---|---|---|---|
  | Orgrimmar (1629, -4373) | 576: 329 creatures, 247 game objects | 190, all in the tables | 190 (144) | 56 | 232 | 2,237 |
  | Dalaran (5804, 624) | 1,206: 508 creatures, 698 game objects | 492, 4 not in the tables | 481 (287) | 192 | 551 | 4,683 |

  No game object of either is a WMO. Drawing nothing at rest: `InvisibleStalker`,
  `InvisibleStalkerNoName` (the triggers) and `KristallSpider` (its batches transparent at rest).
- **`Models::object`** (`display.rs`): the M2 of a `GameObjectDisplayInfo` row with its default
  submeshes, none for a `.wmo`, an error for a row missing or without a model. **`Models::extent`**:
  `Extent` (the bounds of the vertices at rest, kept with the model when it is read, its radius,
  its batches seen at rest at the finest skin, the setting `reach`) while the look is drawn, given
  by the module when a load ends and taken back when the look is released; `Extent::distance`, how
  far an instance at a scale is drawn, as the vertex shader tests it. `models` takes the tables at
  its start, not at its first frame, for the modules asking for looks before.
- **`live-world`** uses `models` and `formats`:
  - the displays are read by a job of the module, started at each frame from `windows_ui` when the
    thread placing the entities asked for some, one job at a time (`looks.rs`): a creature's look
    by `Models::display` with the alpha of its display, a game object's by `Models::object`; a
    display read once, a failure kept (a marker, and why);
  - the thread placing the markers builds at each frame signal the markers, the names and the
    instances (`markers.rs`): an instance for each creature and game object whose look is read, its
    id its GUID, its transform its position along its spline, its rotation (`Tracked::rotation_at`:
    a game object's quaternion; a creature moving the way it goes, taken 50 ms ahead along its
    spline, the way it came at its end; its orientation otherwise), its scale the display's times
    the server's; given whole to `Models::place` as the owner `live-world`;
  - a marker is kept for a player, an entity without a look, one whose look is not drawn, one
    beyond `Extent::distance` at its scale, and one whose look draws nothing at rest; the name of
    an entity seen as its model at the top of its vertices at rest times its scale;
  - the statistics of the markers give the models placed and the time the thread takes for a frame,
    and its longest within the last second.
- **Accepted on the user's machine** (RTX 3080 Ti), the worldserver of `E:` started by the user
  and left running, observed only; the user saw the models in the world, without animations
  (step 9.5). A script of Lua, not kept, run from the panel of scripts: the camera still over the
  city for 25 seconds, then flying 400 yards across it in 20 seconds. Read from the statistics of
  the view:

  | | Orgrimmar | Dalaran |
  |---|---|---|
  | Models: draws | 1,085 still; 376 to 882 flying | 2,743 to 3,124 still; 1,472 to 2,789 flying over the city, 14 past it |
  | Looks on the GPU, instances in sight | 180 to 192 looks (53 to 63 MB), 231 to 305 instances | 425 to 482 looks (144 to 158 MB), 578 to 688 instances |
  | The view on the interface thread (preparing, recording, submitting) | 0.51 to 0.74 ms (submitting 0.37 to 0.60), 1.23 at most | 1.46 to 2.12 ms over the city (submitting 0.99 to 1.62), 3.10 at most, once 7.25 |
  | The bundle of the models recorded | once a second still, 1 to 10 times flying | 1 to 22 times a second still, 14 to 21 flying |
  | The GPU | 0.8 to 1.5 ms a frame, 2.2 at most | 1.5 to 1.9 ms, 3.7 at most |
  | The thread of `live-world` | 0.17 to 0.20 ms a frame, 0.34 at most; 540 to 584 models, 89 to 134 markers | 0.30 to 0.36 ms, 0.51 at most; 1,102 to 1,210 models, 462 to 542 markers |
  | The budget of the view | 534 to 569 MB of 6,042 | 482 to 510 MB of 6,042 |

  Close by, in Orgrimmar: an Orgrimmar Grunt with his baked skin, a banner of Brewfest, the
  invisible "[DND] Brewfest Barker Bunny" kept as a marker, the names over the models. In Dalaran,
  the entities stand in the air over Crystalsong; the game objects farther than the reach of
  their small models are markers.
- **The goal of step 9.2e is not met by the draws**: 1,085 in Orgrimmar, over 3,000 in Dalaran,
  where a few hundred were wanted; the interface thread stays under 4 ms on average (2.1 ms in
  Dalaran) but its submitting grows with the draws (about 0.5 µs a draw) and went once to 7.25 ms;
  and the bundle is recorded again about 20 times a second in Dalaran, its blended groups crossing
  as their creatures walk. The point to revisit above (the baked skins in arrays of textures, a
  group a body) is what the measure calls for: to decide by the review, as a step of its own.
- **Tests**: a display read once with its look, scale and alpha, a failure kept, a WMO and a row
  missing as markers; an entity seen as its model without a marker and its name over it, its
  instance placed before its look is drawn, beyond its reach and drawing nothing as a marker, a
  player never placed, an unread display asked for; a creature turned the way it moves and at the
  end of its spline, standing by its orientation, a game object by its quaternion; a morph taking
  its new look; in `models`, the look of a game object and the extent of a look, told and taken
  back, on the GPU from its vertices at rest. Each of 15 changes made on purpose to `live-world`
  and `models` made a test fail; the call to `Models::place` from the thread and the extent given
  when a load ends are checked by the acceptance only.

#### Step 9.4e, proposed: the drawing of the models led by the GPU

Decided by the user after the review of 9.4d: the draws of a city are settled now, before step
9.5, whose animation is built on this. Each look today has its own buffers, textures and bind
groups, a draw per group, look and batch, chosen on the CPU and kept in a bundle recorded again
when the order of the blended groups changes.

**Checked before proposing**, on the user's machine (RTX 3080 Ti) and the client:

- What the adapter offers, through Vulkan, wgpu 30: `INDIRECT_FIRST_INSTANCE`,
  `MULTI_DRAW_INDIRECT_COUNT`, `TIMESTAMP_QUERY`, `TIMESTAMP_QUERY_INSIDE_ENCODERS` and
  `_INSIDE_PASSES`, `TEXTURE_BINDING_ARRAY` with non-uniform indexing; `VERTEX_STORAGE`,
  `INDIRECT_EXECUTION`, compute; 2,048 layers an array, 524,288 sampled textures a stage, a
  storage binding of 2 GB. The software adapter of the tests (Microsoft Basic Render Driver)
  offers the same features.
- In wgpu 30, `RenderPass` has `multi_draw_indexed_indirect` and its count variant, issued as one
  `vkCmdDrawIndexedIndirect` with its count when the device has `multiDrawIndirect` (else a loop
  in wgpu-hal); `RenderBundleEncoder` has neither, only `draw_indexed_indirect`. A layer that
  draws by multi-draw draws in the pass of the view, not in a bundle.
- The textures of a city, by class (format, size, levels), read by a probe not kept: Orgrimmar
  266 textures in 25 classes (138 of them BC1 of 512 × 512), Dalaran 762 in 36; 25 and 34
  classes by format and size alone. The terrain binds 12 arrays at once.
- The pairs (instance, batch) at rest, every submesh counted: Orgrimmar 20,183, of them 1,856
  alpha-keyed and 894 blended; Dalaran 21,424, 2,878 and 1,336. The vertices and indices of the
  models of a city, every skin: 179 K and 2.6 M (Orgrimmar), 499 K and 6.1 M (Dalaran).

**In three parts, each reviewed before the next:**

- **9.4e1, the core and the arrays of textures.**
  - `Layer` gains a step of computation before the pass: `compute(&mut self, gpu, view, encoder:
    &mut CommandEncoder)`, nothing by default, run after `prepare`, in the same scope of
    validation, its failure removing the layer as `prepare`'s does; and a way to draw in the pass
    itself: a layer says it draws in the pass (`Drawing::Pass`, `Drawing::Bundle` by default) and
    is then given `draw_pass(&mut self, gpu, target, view, pass: &mut RenderPass)` at each frame,
    the layers drawn in their order, bundles and passes mixed. Tested in `core/api` and
    `viewport` as the additions of 9.2a: the order kept, a compute writing what the pass reads in
    the same frame, a failure of each kind reported.
  - The statistics time each layer's computation and drawing on the GPU apart, by timestamps
    around its compute pass and its draws (inside the pass where the adapter allows it, the
    layer's total otherwise).
  - The kernel asks, when the adapter offers them, for `INDIRECT_FIRST_INSTANCE`,
    `MULTI_DRAW_INDIRECT_COUNT` and `TIMESTAMP_QUERY_INSIDE_PASSES`, and for 64 sampled textures a
    stage at least.
  - The arrays of textures of the terrain (`terrain/src/textures.rs`) move into `core/api`,
    generalised: as many slots as the user asks, the class by format and size, a texture of fewer
    levels than its class taking the chain of its class, its own levels filled and its level of
    detail held to them by its shader (its gradients clamped). The terrain uses them unchanged,
    its tests kept; measured as in 9.2f.
- **9.4e2, the shared resources of the models, drawn in the pass by multi-draw.**
  - The vertices of every model read in one arena, the indices (32 bits) in another: a buffer
    each, a list of holes, grown by a buffer twice as large filled by a copy the job submits (the
    rule of 9.2f), a model's ranges given back when no look holds it; counted in the shared
    budget with the arrays.
  - A record of draw for each (look, level of skin, batch): its range of indices, its base vertex,
    its material, its state of pipeline. A table of materials in a storage buffer: the colour and
    weight at rest, the flags, the shader of 9.4c2 and its coordinates, its two textures as
    (array, layer, levels, wrapping); the textures of the looks in the arrays of `core/api`, 64
    slots read by a `switch` as the terrain does (no `binding_array`: 36 classes in Dalaran fit),
    a texture clamped or wrapped by the shader from its wrapping, one sampler. The baked skins
    are textures like the others: the NPCs of one body share its vertices and differ by their
    materials.
  - Drawn in the pass, by state of pipeline (blending, two sides, depth: a dozen at most), by one
    `multi_draw_indexed_indirect` over the records of that state, their arguments written by the
    CPU at each frame for the groups in sight (as 9.4c chooses them), `first_instance` pointing
    into a list of the instances seen; the blended ones sorted by the CPU, the farthest first,
    within each blended state, the states in a fixed order (alpha and blend add sorted, then add,
    mod and mod2x, whose order changes nothing within themselves). The bundle is gone: the layer
    records a few dozen commands each frame, whatever moves.
  - Kept: the path of 9.4c, for an adapter without the features or limits above, chosen at start
    and tested by forcing it; the API of the service `models`, unchanged for its users.
- **9.4e3, the choice by the GPU.**
  - A pass of computation at each frame, over the instances of each owner: in sight (the bounds of
    its look, turned and scaled, against the planes of the view), within the reach of its size, its
    level of skin by its distance in radii with the margin of 9.4c1, the level before kept in a
    buffer for each instance (begun again when its owner's set is regrouped); for each record of
    its look at that level, its instance appended to the record's list by an atomic add in a range
    reserved as many as the look's instances (reserved by the CPU when the sets change), the
    arguments of the draws copied from their templates first. What a seen instance needs (its rows,
    alpha, record) is copied into the list, the vertex shader reading it there by
    `instance_index`: every owner's instances drawn by the same commands.
  - The blended pairs stay sorted by the CPU as in 9.4e2, now by instance, its arguments written
    for the draws; a sort on the GPU only if the measure asks for it.
  - Measured against the table of 9.4d, in Orgrimmar and Dalaran, still then flying: the commands
    of the layer, the interface thread (preparing, recording, submitting), the GPU (computation
    and drawing apart), the bundles recorded a second. Goals: a few dozen commands for the layer,
    the interface thread under 1 ms on average in Dalaran, no recording while flying but when the
    looks change, the GPU no worse than in 9.4d.

**What this settles too**: the point to revisit of 9.4c1 (the level and the reach of each instance,
for the doodads of 9.6) by 9.4e3; and the bundle recorded again 20 times a second in Dalaran, gone
in 9.4e2.

**Made ready for step 9.5, not built**: the bones of an instance as an offset into a storage
buffer of matrices, carried with the instance into the list the vertex shader reads; the
vertices in the arena gaining their four bone indices and weights (8 bytes) then.

**Threads**: the choice moves from the CPU to the GPU; the thread of `live-world` keeps writing its
instances by `place`; the loads stay jobs writing into the arena and the arrays by copies they
submit.

#### Step 9.4e, added by the review of the proposal

- **A texture without room (9.4e2).** Sixty-four slots hold a city (36 classes in Dalaran), but a
  continent, and the modern models of WarcraftXL (1024, 2048, other formats), can fill them. A
  texture finding no room is never drawn white: its look falls back on the path of 9.4c (its own
  textures, a draw a group), and the statistics count the looks on that path. Measured in Dalaran:
  what the arrays reserve (their capacity) against what they hold (the layers used). Kept in
  reserve, built only if that measure or a city's classes ask for it: `binding_array` for the
  models alone (the adapter offers it, with non-uniform indexing), a texture an entry, without
  classes nor copies to grow, the path of 9.4c the fallback in the same way.
- **The lists of the instances seen (9.4e3)**, without reserving a range for each (look, level,
  batch) as large as the look's instances nor copying 64 bytes into it, which multiplies the memory
  by the levels and the batches (80,000 entries for a city, but 50,000 doodads of 5 batches and 4
  levels in 9.6 would reserve 1,000,000 entries, 64 MB):
  - at each frame, the instances of every owner are first copied by the GPU into one buffer of the
    frame, each owner at its base, so that an instance is one index whoever owns it;
  - the compute counts the instances of each record, a prefix sum gives each record its offset,
    then each instance writes 8 bytes (its index in the buffer of the frame, its record) at its
    record's place: the lists hold exactly the pairs seen, the vertex shader following the index to
    the instance's data and the record to its material;
  - reserved for a zone of 50,000 doodads of 5 batches, all seen at worst: 250,000 entries, 2 MB,
    and, for its records (say 500 looks × 4 levels × 5 batches: 10,000), their counts, offsets and
    arguments, 0.3 MB; the buffer of the frame 3.2 MB.
- **The draws without instances (9.4e3).** About 15,000 records in Dalaran (looks × 4 levels ×
  batches), most without an instance once chosen: the pass giving the offsets also appends the
  records that have instances to a list of each state of pipeline, counted, and each state is
  drawn by `multi_draw_indexed_indirect_count` over its list (the adapter offers
  `MULTI_DRAW_INDIRECT_COUNT`); without it, `multi_draw_indexed_indirect` over every record of the
  state, the time of the empty draws measured on the GPU.
- **Layers drawn in the pass and in bundles, mixed (9.4e1).** `execute_bundles` resets the state of
  the pass after the bundles run: a layer drawn in the pass sets all its state (pipeline, bind
  groups, vertex and index buffers) without assuming anything of what came before, and a layer
  drawn in a bundle never sees the state a layer drawn in the pass left. Tested with a layer of
  each kind, in both orders.

#### Step 9.4e1, as built

- **`Layer`** (`core/api`, `viewport`): `Drawing` (`Bundle` by default, `Pass`); `compute(gpu,
  view, encoder)`, nothing by default; `drawing()`, read at each frame; `draw_pass(gpu, target,
  view, pass)`, nothing by default; `draw` now nothing by default too, for a layer drawn in the
  pass. Their documentation says what the state of the pass is when `draw_pass` begins.
- **A frame of the view** (`draw_frame`), in three times: each layer prepared, then its computing
  recorded into an encoder of its own, for a layer whose `computes` says so (none by default: no
  encoder for a layer computing nothing, its timestamps of computing read as nothing),
  finished inside its scope of validation (a panic, invalid
  commands or a GPU error there remove that layer alone), then its bundle kept or recorded, or none
  for a layer drawn in the pass; then the pass: the grid, then the layers in their order, a bundle
  run or `draw_pass` called; then each layer's statistics and labels. Everything is submitted in
  one call, the encoders of the computing first. A GPU error in the pass is known only when its
  encoder is finished: it removes the layers drawn in the pass that frame, and the frame is not
  drawn (the computing is submitted all the same). The time of recording a layer drawn in the pass
  is its recording, at each frame; the submitting of the view leaves it out.
- **The GPU timed for each layer** (`stats.rs`): 2 timestamps for the pass and 4 for each of the
  first 16 layers (its computing begun and ended in its encoder, its drawing begun and ended in the
  pass, a bundle as a layer drawn in the pass), where the device offers timestamps inside encoders
  and passes; resolved with the frame, read back some frames later. The statistics give for each
  layer `GPU: computing x ms (longest), drawing y (longest)`. A frame whose pass failed is not
  timed.
- **The device** (`kernel`): asks, when the adapter offers them, for the timestamps inside encoders
  and passes, `INDIRECT_FIRST_INSTANCE` and `MULTI_DRAW_INDIRECT_COUNT`, and for up to 128 sampled
  textures a stage (16 by default).
- **The arrays of textures** (`uniwow_api::texture_arrays`): those of the terrain moved and
  generalised: their owner (for the log and the labels of the GPU), their count of slots, and
  whether the textures are read as sRGB are given to `TextureArrays::new`; the counts give the
  layers holding a texture and all the layers of the arrays (their capacity), counted where a layer
  is taken and given back. The terrain keeps its 12 slots, read as sRGB, and its tests.
  - Against the proposal, the class stayed (format, size, levels): by format and size alone,
    Dalaran has 34 classes instead of 36 and Orgrimmar 25 as before, which did not pay for a
    level of detail held by the gradients in every shader reading the arrays. Changed by the full
    verification of the milestones, at the user's choice: a texture whose levels are cut short (3
    of the client) or exported with fewer took a class, and a slot, of its own, which a map holding
    several such could run out of. The class is (format, size); an array has every level of its
    size, down to 1 texel (BC: down to 4), and those a texture lacks are made from its last one
    when it is placed, its texels averaged 2 × 2, or for BC one block of each 2 × 2 taken, which
    needs no encoder and no change of the shaders.
- **Tests**: in `viewport`, on the software adapter: layers drawn in the pass and in bundles in
  their order, each with its own state, in three orders (the pass then a bundle, a bundle then the
  pass, the pass, a bundle and the pass again), the pixel read back; what a layer computes drawn
  in the same frame; a layer failing in each of four ways (a panic or a GPU error while computing,
  a panic or a GPU error in the pass) removed and reported alone, the next frame drawn; the GPU
  timed for each layer, computing and drawing, in their order; the statistics of a layer timed.
  In `kernel`, the features and the textures asked and the device made with them. In `terrain`,
  its tests through the arrays of `core/api`, the layers and the capacity counted, taken in a new
  array or in one holding the class, and given back. Each of 11 changes made on purpose to the
  frame of the view, its timing of the GPU, the device and the arrays made a test fail.
- **Accepted on the user's machine** (RTX 3080 Ti), the worldserver of `E:` running, observed only,
  over Orgrimmar with the script of 9.4d (still 25 seconds, then flying): the terrain, the models
  and the markers drawn as in 9.4d; the view on the interface thread 0.59 ms still (submitting
  0.44), 0.64 to 0.70 flying, 1.63 at most; the models 707 draws still, 542 to 844 flying; the GPU
  timed for each layer for the first time: the models drawing 0.69 ms still (0.75 at most), 0.37
  to 0.54 flying; the terrain 1.16 ms still, 0.66 to 1.04 flying, 1.58 at most; the other layers
  under 0.02 ms; nothing computed yet. The same as 9.4d (0.51 to 0.74 ms): nothing lost by the
  new frame.

#### Step 9.4e1, after its review

- **A pass that fails without a layer drawn in it** (the grid, the timestamps, or a bundle that got
  through) was not drawn nor said, at every frame: the view froze without a word. The error of the
  pass is now given with the frame (`Drawn::pass_error`) to a watch of the view (`PassWatch`): said
  in the log at the first frame missed in a row only, shown in the statistics (`the pass failed,
  N frames in a row not drawn: …`) while it lasts, forgotten at the first frame drawn. The layers
  drawn in the pass are still removed at the first frame; with none, after 30 frames missed in a
  row the viewport reports itself failed. Tested: a bundle recorded for the view's multisampling,
  run in a pass of one sample, refused at every frame, said once, given up at the thirtieth, no
  layer removed; the watch begun again by a frame drawn; the line of the statistics.

#### Step 9.4e2, as built

- **The pool** (`models/src/pool.rs`, `arena.rs`), made when the module starts if the device offers
  the first instance of an indirect draw, storage buffers read by the vertices (three a stage) and
  64 sampled textures a stage; otherwise the whole module draws on the path of 9.4c, as the tests
  force it:
  - three arenas: the vertices of every model (40 bytes each), their indices (32 bits), the
    materials of every look (96 bytes each). A range is taken from the first hole holding it and
    given back merged with its neighbours. A buffer too small is replaced by one twice as large,
    the one before copied into it, so that every range keeps its place. Each range is written by a
    copy the job submits under the lock of the arena, after any copy into a larger buffer;
  - the textures of the looks in the arrays of `core/api`, 64 slots, read as stored, as in 9.4c;
    `TextureArrays::fetch` tells an unreadable texture from one without room;
  - a pipeline for each state (blending, two sides, depth), made by the job loading the first look
    that needs it, on one shader: `common.wgsl` (shared with `models.wgsl` of 9.4c: the placing of
    an instance, the combiners, the alpha test, the light and the fog) and `pool.wgsl`, its 64
    arrays read by a `switch` written with them;
  - one sampler, repeating. A texture is held to its edge by the shader on the axes its flags do
    not wrap: the point read is kept inside by half of what the filter reads around it, the texel
    of the coarsest level its gradients can choose and their spread for the anisotropy. Found by
    the acceptance: held by half a texel of the first level only, the coarse levels of the hide of
    the Orc Tent (opaque at its top, clear at its bottom) mixed its bottom into its top edge, and
    the alpha test left dark dots along it. Tried and not kept: four samplers, chosen by the shader
    by how the texture wraps, exact but four times the reads in the shader (the tests on the
    software adapter from 5 to 29 seconds); their cost on the GPU of the user could not be told
    from a change of its load meanwhile.
- **A look from the pool** (`pooled.rs`):
  - its model in the arenas once for all its looks (`Caches::pooled`): held while a look holds it,
    its ranges given back with its last look;
  - for each batch of each level of skin, a record: its first index in the arena, its count, its
    first vertex, its material, its state;
  - its materials written to their arena and given back when the look is released. A material
    holds what a batch gives the shader in 9.4c, then its two textures (their slot and layer), how
    each wraps, and the sizes of their classes;
  - a texture without room: the look falls back on the path of 9.4c (`Ready::Own`), said in the
    log; an unreadable texture is drawn white, as before.
- **Drawn in the pass** (`layer.rs`), with the pool:
  - in `compute`, the instances of every owner are copied by the GPU into one buffer of the frame,
    each owner at its base;
  - for each batch of each group in sight (chosen as in 9.4c), the CPU writes its entries (an
    instance of the buffer of the frame and its material, 8 bytes each) and its arguments. They are
    gathered by state: the opaque ones, then the blended ones, the blended groups the farthest first
    within each state, the states in a fixed order (alpha, blend add, add without alpha, add, mod,
    mod2x);
  - in the pass, one `multi_draw_indexed_indirect` a state, `first_instance` pointing at the batch's
    first entry;
  - the looks on the path of 9.4c are drawn in the same pass after those of the pool, opaque then
    blended, as their bundle drew them;
  - the buffers of the frame grow to twice what they need; the bind group is made again when they,
    the arrays or the buffer of the materials change. No bundle is recorded with the pool; a
    device without it keeps the bundle of 9.4c.
- **The statistics**: `N commands in the pass` replaces the bundle recorded; the summary gives the
  looks on the path of 9.4c and, for the pool, its models, the bytes of its arenas and those used,
  its textures in how many arrays, the layers held of their capacity, the textures waiting for room
  and the unreadable ones. The budget counts a model or a texture once, whichever path holds it.
- **Tests**, on the software adapter: the holes of an arena taken, given back and merged, grown;
  an arena grown keeping its ranges, a range given back taken again; three looks of one model,
  each with its own skin as a layer of one array, their instances from two owners, drawn by one
  command, each where it stands and in its own colour; two models sharing the arenas, the second
  drawn from its own vertices and triangle; a look of a new class made after the first frame, its
  array bound by a new bind group, then instances enough to grow the buffer of the frame; a look
  without room drawn on the path of 9.4c in the same pass (two commands); released looks giving
  back their materials, the model with its last look, their textures when the arrays are purged,
  the pipeline of a look made by its load. Every test of the shaders draws its model on both paths
  and asks for the same pixel within 2; three of them are new: a texture held to its edge unless
  its flags wrap it, one held to its edge never reading the other at a coarse level (the hide of
  the Orc Tent), and an alpha layer drawn before a mod layer. The tests of 9.4c draw with the pool,
  as the editor does. Of 33 changes made on purpose to the arenas, the pool, its shader, the looks
  from it and the layer, 31 made a test fail; the two others are checked by the acceptance only:
  the module set up with the pool, and the purge of `Caches::pooled` when looks are released
  (nearly without effect: the cache holds its models weakly, given back with their last look).
- **Accepted on the user's machine** (RTX 3080 Ti), the worldserver of `E:` running, observed only,
  with the script of 9.4d over Orgrimmar and Dalaran (still 25 seconds, then flying 400 yards in
  20 seconds), every look drawn from the pool, none on the path of 9.4c nor waiting for room. Read
  from the statistics of the view, against 9.4d:

  | | Orgrimmar | Dalaran |
  |---|---|---|
  | Models: draws, commands | 992 to 1,013 still; 375 to 813 flying; 6 commands | 2,961 to 2,985 still; 1,536 to 2,765 flying over the city, 14 to 177 past it; 6 to 9 commands |
  | The view on the interface thread | 0.43 to 0.44 ms still (submitting 0.18), 0.33 to 0.42 flying, 0.76 at most (9.4d: 0.51 to 0.74, 1.23 at most) | 0.78 to 0.79 ms still (submitting 0.21 to 0.22), 0.58 to 0.71 flying over the city, 0.94 at most (9.4d: 1.46 to 2.12, 3.10 at most, once 7.25) |
  | The models on the interface thread | 0.19 to 0.28 ms | 0.52 to 0.69 ms; the steering of the module once 3.53 ms |
  | The bundle of the models recorded | never (9.4d: up to 10 times a second) | never (9.4d: 14 to 22 times a second) |
  | The GPU | 1.61 to 2.21 ms a frame; the models drawing 0.52 to 0.96 ms | 1.43 to 3.10 ms; the models drawing 0.56 to 1.03 ms |
  | The pool | 47 to 50 models in arenas of 28 MB (18 used); 189 to 224 textures in 22 to 26 arrays, 189 to 224 layers held of 284 to 324 | 184 models in arenas of 55 to 75 MB (43 to 46 used); 599 to 608 textures in 33 arrays, 599 to 608 layers held of 756 to 760 |

  The GPU stays at a low clock while it draws a view this light (P8 mostly, 270 to 430 MHz on
  average of 2,145, read by `nvidia-smi` during the runs), and its times follow that clock from
  one run to the next: the terrain, unchanged, drew the same tiles in 0.82 to 2.26 ms. Compared
  in the same conditions, run after run (the pool, the path of 9.4c forced, the pool again; the
  same clocks on average within each city), against the path of 9.4c:

  | | Orgrimmar | Dalaran |
  |---|---|---|
  | The models on the GPU, still | 0.78 to 0.83 ms; 0.83 to 0.84 | 0.75 to 0.84 ms; 0.86 |
  | The models on the GPU, flying | 0.48 to 0.95 ms; 0.47 to 0.93 | 0.22 to 0.89 ms; 0.16 to 0.95, the same share of the terrain's time |
  | The view on the interface thread | 0.34 to 0.44 ms, 0.62 at most; 0.51 to 0.70, 1.15 at most | 0.61 to 0.83 ms, 1.34 at most; 1.48 to 2.16, 3.08 at most, once 9.13 |

  The GPU draws the models from the pool as fast as on the path of 9.4c, a few hundredths of a
  millisecond slower with few draws; the interface thread spends half as long. The arrays reserve
  a quarter more layers than they hold in
  Dalaran (608 of 756): no `binding_array` needed. Seen the same on both paths, drawn from the
  pool and forced on that of 9.4c: the Orc Tent once fixed, and the white parts of some guards of
  Orgrimmar, which are not of this step.

#### Step 9.4e2, after its review

- **The white parts of the guards of Orgrimmar**, read by a probe not kept over the displays the
  observer sends there (the Orgrimmar Grunts 4259, 4260, 4601, 4602, the Forsaken refugees, Kaja,
  a tauren): every one is a batch of the kind 8, the extra of a character's skin, on the body
  (submesh 0), the ears (702) and some of the face (1xx, 2xx, 3xx) of the orc, undead and tauren
  models of the client. The client takes it from `CharSections`: the section 0 (the skin) of the
  look's race and sex, of variation 0 and of the look's skin colour, its second texture
  (`…Skin00_NN_Extra.blp`; 318 of them in the client, 2 missing, of the gnome female). `display()`
  reads that table already for the hair: it gives that texture now, with the kind 8.
- **Points to revisit, still drawn white** (over every display of the client, by the same probe):
  - 4 character looks without a baked skin: their skin (kind 1) is made by the client of the
    sections of `CharSections` (skin, face, facial hair, underwear) and of the textures of their
    items by region of the body (`ItemDisplayInfo`); with the equipment of a look (its items'
    models, textures and submeshes), which no step of milestone 9 draws: to place as a step of its
    own when the user decides it, after 9.5;
  - 12 character looks whose skin colour has no extra in `CharSections` (the kind 8, as a male
    draenei): what the client draws there is not established, to see in game;
  - 66 creature displays on a character's model without a look (kinds 1, 2, 6 and 8), and 97
    displays leaving empty a skin their model uses (the kinds 11 to 13: 25, 65 and 7 displays):
    what the client draws there is not established either; none of them was seen in Orgrimmar or
    Dalaran.
- **The steering of `models` once at 3.53 ms** in Dalaran: timed by parts over three flights
  (instrumentation not kept), every frame over a millisecond spent its time reading the bytes of
  the arenas for the statistics, waiting for the lock a job held while it filled its staging
  buffer, grew an arena and submitted its copy (up to 3.18 ms of 3.50). Not the handing of looks.
  An arena now counts its bytes in atomics and keeps its buffer under a lock of its own, held only
  to read or replace it; the job fills its staging buffer before taking the lock of the holes.
  Tested: the bytes and the buffer read while a job holds the holes.

#### Step 9.4e3, as built

- **Checked at the start**, by a probe not kept over the displays the observer sends: a third of
  the instances have a look with blended batches (167 of 531 in Orgrimmar, 336 of 1,014 in
  Dalaran). Chosen by the CPU, as the proposal had them sorted, their opaque batches could take
  another level than their blended ones; so the GPU chooses every instance, and the CPU only orders
  the blended draws.
- **The choice by the GPU** (`models/src/choice.rs`, `choice.wgsl`), with the pool:
  - the tables the GPU chooses from, made by a job of the module when its looks change and handed
    with them (a look loaded is drawn a frame or two later, when its tables come), one job at a
    time: looks published while it runs make it start once more when it ends, from the looks it then
    finds, rather than one job each, of which only the newest was kept (the full verification of
    the milestones). A job of the pipelines that fails is said in the panel, no model drawn; one of
    the preview, in its line. The counts of the choice are read back without waiting, by
    `device.poll` without blocking on the interface thread at each frame, which the callbacks of
    `map_async` need. The pooled looks
    by slot, their radius and levels; their opaque records, gathered by state in the order drawn
    (then by look, level and batch), and the regions of those states;
  - at each frame, the CPU finds the groups in sight as in 9.4c and writes them for the GPU; it
    sorts the instances of looks with blended batches the farthest first, one by one, the order
    kept until two cross by the margin of 9.4c, and writes for each a template of draw at each
    level of its look, for each blended batch, gathered by blended state in the fixed order;
  - `choose`, a workgroup a group: each instance in sight (its box against the sides of the view),
    within the reach of its size, at the level its distance chooses with the margin of 9.4c1 from
    its level of the frame before; the instances of each record counted. The levels are kept for
    each owner: copied where the owner moved in the buffer of the frame, chosen afresh when it
    regroups, forgotten while out of sight;
  - a prefix sum over the records by blocks of 256 (`blocks`, `tops`, `place`): the place of each
    record's instances among the entries, and of its draw among those of its state; `pack`: the
    arguments of the records with instances, packed in the order of their state's records, and the
    count of each state's draws; `tops` keeps the templates whose level is the one chosen for their
    instance, packed in their order; `scatter`: each instance's entries at its records' places;
  - drawn: one `multi_draw_indexed_indirect_count` a state, the opaque ones then the blended ones.
    On Direct3D 12, wgpu 30 does not give a draw counted by the GPU its first instance (its command
    signature for `draw_indexed_indirect_count` leaves out the special constants the other draws are
    given): there, one `multi_draw_indexed_indirect` a state over all its records, those without
    instances drawing none;
  - what was drawn read back two frames later: the draws, the pairs of an instance and a batch, the
    triangles, the instances at each level, in the statistics of the layer.
- **The pool** now asks for compute shaders, indirect execution and eight storage buffers a stage;
  the instances of every owner publish their origins, for the order of the blended ones. The looks
  of their own (no room in the arrays) are chosen by the CPU as in 9.4c.
- **Found by the acceptance and fixed**: the prefix sum in one workgroup cost a fixed 0.7 to 1.7 ms
  of the GPU at its low clock, over about 20,000 records in Dalaran (most without instances); the
  tables, made on the interface thread, took 2.2 to 3.4 ms 46 times in 50 seconds over Dalaran,
  as its looks came and went (frames of 2.7 to 4.2 ms). Both as above now.
- **Tests**, on the software adapter (Direct3D 12: every record drawn there, the packing read back):
  an instance of a group in sight, one beside the view and one beyond its reach, one drawn; the
  level kept going up and down within the margin, then changed past it; kept where its owner moves
  in the buffer of the frame, chosen afresh when it regroups, forgotten while out of sight; blended
  instances of two owners drawn one by one the farthest first (four halves of red and blue), each
  where it stands, their order kept until two cross by the margin; a template drawn only at the
  level chosen; the draws of a state packed in their order and counted, across two blocks of
  records (360). Of 29 changes made on purpose to the choice, its shader and the layer, 28 made a
  test fail; the job making the tables is checked by the acceptance only.
- **Accepted on the user's machine** (RTX 3080 Ti, Vulkan: the draws packed and counted by the
  GPU), the worldserver of `E:` running, observed only, with the script of 9.4d, run after run as in
  9.4e2 against the path of 9.4c forced, at the same clocks (about 300 MHz in Orgrimmar, 420 to 490
  in Dalaran):

  | | Orgrimmar: 9.4e3; 9.4c | Dalaran: 9.4e3; 9.4c |
  |---|---|---|
  | Draws | 874 to 882 still, 418 to 733 flying; 1,010, 367 to 810 | 2,948 to 2,973 still, 1,502 to 2,809 flying; 2,954 to 2,971, 1,556 to 2,813 |
  | Triangles | 0.72 to 0.73 M still, 0.29 to 0.58 flying; 1.10 M, 0.69 to 1.19 | 1.13 to 1.14 M still; 1.25 to 1.26 M |
  | Instances at each level, still | 129, 33, 0, 0 | 187, 120, 6, 0 |
  | Commands of the layer | 6 to 7 | 8 to 9 |
  | The view on the interface thread | 0.31 to 0.38 ms, 1.21 at most; 0.52 to 0.73, 1.10 at most | 0.33 to 0.45 ms, 1.39 at most, once 3.41; 1.56 to 2.29, 3.51 at most, once 12.46 |
  | The models on the GPU | computing 0.35 to 0.40 ms, drawing 0.36 to 0.60; drawing 0.49 to 0.83 | computing 0.26 to 0.35 ms, drawing 0.42 to 0.77; drawing 0.39 to 0.89 |

  No bundle recorded. Against the goals of 9.4e: a few dozen commands, met (9 at most); the
  interface thread under 1 ms on average in Dalaran, met; no recording while flying, met; the GPU
  no worse than 9.4d, not quite: the levels chosen for each instance take a tenth to a third of
  the triangles off and the drawing is faster, but with the computing the models cost the GPU,
  at these clocks, as much as the path of 9.4c to 0.3 ms more. Seen from five yards: an Orgrimmar
  Grunt and Kaja with their skins whole (the extra of 9.4e2, after its review).

#### Step 9.4e3, after its review

Step 9.4e is done. **Points to revisit with step 9.6**, its doodads adding instances and looks:

- **The cost of the choice on the GPU** is mostly fixed: the prefix sum goes over every record
  of every look held, about 20,000 in Dalaran, most of them without instances. It was measured at
  the low clock the GPU keeps for a view this light (300 to 490 MHz of 2,145); measure it once at a
  high clock (the RTX 3080 Ti held at its highest performance, or a scene that loads it), to know
  its true cost.
- **Should it still weigh** with the doodads: go over the records of the looks with instances in
  sight only (an indirect dispatch over a list of the looks seen), or skip the blocks of the prefix
  sum without instances.
- **Two phases of drawing** (the review of 9.5c), before the water and the buildings, which will
  need them: first what every layer draws opaque, then the sky; then what every layer blends, so
  that a blended batch of a layer is drawn over the opaque ones of all.
- **The writing of the bones** (the review of 9.5b): about 40 % of the thread (0.35 to 0.54 ms
  in Orgrimmar), after the computing, in one piece: the bones kept from a frame to the next, then
  copied into the view of `Queue::write_buffer_with`. Each slice could write its own into its part
  of the view: one copy less, the writing spread. In wgpu 30, `WriteOnly<[u8]>` is not `Send` (its
  implementation asks for a type of known size): the view is to be shared among the slices another
  way, without `unsafe`.

#### Step 9.5, proposed: the animations of the models

Asked after the review of 9.4e: the M2 models animated, *Stand*, *Walk* and *Run* chosen by the
movement received, on the base step 9.4e laid (*Made ready for step 9.5*, above): the bones of an
instance in a storage buffer, their place carried with the instance.

**Checked before proposing**, by probes not kept, over the client of `E:` and the models of the
user's wow.export:

- 23,186 models of 3.3.5a: 12,893 have one bone or none, 8,036 up to 16, 1,214 up to 64, 751 up
  to 128, 282 up to 256 and 10 more (315 at most; the characters of the HD pack 245 to 262). The
  vertices of 5,394 are weighted to more than their first bone; a vertex names its bones among the
  model's directly (wowdev).
- Of 138,843 sequences, 10,602 have their keys in `.anim` files, in 666 models; none of them is a
  *Stand* (id 0, in 23,025 models), a *Walk* (4, in 1,849) or a *Run* (5, in 1,756): every
  sequence this step plays is in its model. The same in the two modern creatures (`MD21`, 274):
  their `.anim` files (chunked, `AFM2`, named by `AFID`) hold other sequences; their `.bone` files
  (`BFID`) are for the customisation of faces.
- The keys of the bones: 130 million in the client, 14 million of them for *Stand*, *Walk* and
  *Run*. 4,020 models have bones on global sequences, which loop whatever is played: doodads
  mostly.
- In the cities of 9.4d, every instance animated: Orgrimmar 531 instances and 74,787 bones (54,557
  of them with keys), 3.6 MB a frame as matrices of 3 × 4 floats; Dalaran 1,014 instances and
  83,976 bones, 4.0 MB.
- The observer sends whether an entity moves along a spline (0x10) and whether it walks (0x04),
  and the points of the spline with their times, so its speed.

**In three parts, each reviewed before the next:**

- **9.5a, the animations read.** In `assets`, `formats::Model` gains, as plain data:
  - its bones: parent, flags, pivot, and their tracks of translation, rotation (the compressed
    quaternions of 3.3.5a and of the modern models) and scale; their order, the parents first;
  - its sequences (id, variation, duration, speed of movement, flags, time of blending, next
    variation, alias), its global sequences, and the fallbacks of its playable animations (3.3.5a);
  - the tracks of its colours, transparencies and texture transforms;
  - the keys kept only for the sequences played (*Stand*, *Walk*, *Run*, their variations and
    aliases) and the global sequences; the others named but not read;
  - the interpolations of WotLK: none, linear (normalised for rotations), Bézier and Hermite;
  - `.anim` files not read: no sequence played needs them (above); their reading, of 3.3.5a and
    modern (`AFID`, `AFM2`), comes with the first sequence played that does.
  Checked over every model of the client (read, refused and why, keys kept), tested on files the
  tests write.
- **9.5b, the bones on the GPU.**
  - The service `models`: an instance gains its motion, standing, walking or running, and its
    speed in yards a second; `live-world` gives them from the flags and the spline of each entity.
  - A thread of the module, woken at each frame by the frame signal (*Threads*, above), keeps for
    each instance (by owner and id) its sequence, variation and time, and the one before while
    they blend. It chooses *Stand*, *Walk* or *Run* by the motion, through the fallbacks of the
    model where it lacks one; plays it at the speed of the instance over that of the sequence, so
    that the feet do not slide; each instance from its own moment (by its id), so that the guards
    do not breathe together.
  - It computes the bones of the instances of the groups in sight, the parents first, with their
    pivots, the billboards facing the camera of the frame before, split with `parallel_for`. It
    writes in one buffer of the frame the instances of every owner, each with the place of its
    bones, and the bones (`Queue::write_buffer`: a frame draws the old or the new whole, as
    *Threads* says). The layer takes that buffer and its layout instead of copying the owners'
    instances (9.4e); the choice by the GPU stays as it is.
  - The vertices in the arena gain their four bones and weights (8 bytes, 48 a vertex); the vertex
    shaders of the pool and of the path of 9.4c skin them. A model without bones, or whose bones
    have no keys, is drawn at rest as today, and so is every model on a device without the pool.
  - An instance is chosen by the bounds of its sequences, which the animation may pass.
  - Measured in Orgrimmar and Dalaran against 9.4e3: the time of the thread on average and at
    most, the bytes written a frame, the interface thread, the GPU; the walk and run of guards and
    creatures seen.
- **9.5c, the materials animated.** The colours, transparencies and texture transforms of each
  batch, at the time of its instance's sequence or of a global sequence, written with its bones (a
  few floats for each batch animated), read by the shaders at the place the instance gives: the
  falls of Coilfang flowing (9.4c2), colours that pulse.

**Not in 9.5**: the dead (lying as *Death* leaves them), emotes, fights and spells, mounts and
what they carry, the attachments (weapons in hands) and the equipment, particles and ribbons, the
sounds and events of an animation, the faces of the modern customisation. The doodads that move
(torches, trees) take this thread with step 9.6.

**To decide with the review:**

- An instance out of sight keeps its pose and its time goes on: the work follows what is seen, and
  an instance coming back in sight takes up its animation where its time is.
- If the measure finds the bones of a city costing the thread more than about 2 ms a frame, or the
  bytes weighing: the instances far away animated less often (every other frame beyond a distance
  in radii), or the bones computed by the GPU from keys uploaded once, the instances giving only
  their sequence and time. Built only if the measure asks for it.

#### Step 9.5, added by the review of the proposal

The proposal is validated, with these additions, for 9.5b:

- **The instances that do not move are not written again by the CPU at each frame.** As
  proposed, the thread of the animations would have written every instance of every owner at each
  frame, and the static doodads of 9.6 (tens of thousands) with them, which the set kept of 9.4c1
  and the copy by the GPU of 9.4e avoid. The copy of the owners' instances into the buffer of the
  frame by the GPU stays. The thread of the animations writes only:
  - the bones of the animated instances in sight;
  - for each owner, a table of a `u32` for each instance, in the order of the owner's set: the
    place of its bones, or none; copied by the GPU at the same base as its instances.
  An instance whose bones have no keys, or a static one, costs nothing a frame. The bones and the
  tables are published together (a buffer grown is made filled, the lesson of 9.3c), so that a
  frame reads a whole set even when the thread is late.
- **The priority of the slices of the animations.** In `Queue::take` (`core/kernel/src/jobs.rs`),
  a worker takes a job before a helper of `parallel_for`. During a large load (the terrain at a
  distance of 64 and the looks of a city, up to *cores − 1* loads each), the helpers of the thread
  of the animations would wait behind the jobs, and the thread would compute its slices alone
  (about 80,000 bones). Its time is measured while the map changes at a distance of 64 and while
  flying fast over a city; should it pass the frame, the helpers of a work due at a frame take
  their turn before the jobs (a queue of their own, or a flag of `parallel_for`), without starving
  the jobs, and tested.
- **The two points to decide**, agreed: an instance out of sight keeps its pose and its time goes
  on; beyond 2 ms of the thread a frame (measured, loads included), the instances far away
  animated less often or the bones computed by the GPU from keys uploaded once, on the measure
  only.

#### Step 9.5a, as built

- **`formats`** (`core/api`): `Model::animation`, an `Animation` as plain data:
  - its sequences: the animation played, its variation, its length, the speed it moves at, its
    flags, how often among its variations, its replays, its times of blending in and out (one
    field in 3.3.5a, given twice), the bounds and radius it moves in, its next variation, the
    sequence it is an alias of, and whether its keys are kept;
  - the lengths of its global sequences;
  - its bones (key bone, flags, parent, pivot, and the tracks of their translation, rotation and
    scale, the compressed quaternions of 3.3.5a made whole) and the order they are computed in,
    each after its parent;
  - the tracks of its colours and alphas, of its weights and of its texture transforms;
  - a `Track`: how it goes from a key to the next (in steps, in a line, along a Bézier or a Hermite
    curve), the global sequence it loops on, and its keys in each sequence (times, values, and the
    tangents in and out of each on a curve).
  `Formats::animations` gives `AnimationData.dbc`: each animation's id, name and fallback.
- **Read** (`assets/src/animation.rs`), the same in 3.3.5a and in the modern models: the keys kept
  for *Stand*, *Walk* and *Run*, through their aliases, when held in the model, and for the global
  sequences; the other sequences named, their keys left; the `.anim` files not read. A track whose
  times and values do not match, or of an interpolation unknown, is left at rest; a parent past
  the bones is left out; the parent closing a loop of parents is cut; each said in the faults of
  the model (`its animation: …`). What lies out of the file refuses the model.
- **Against the proposal**: the fallbacks of the playable animations are not in the models of
  3.3.5a; their table ends with Burning Crusade (wowdev: the header of version 264 has no such
  array). The client takes them from `AnimationData.dbc`, its sixth field, read by
  `Formats::animations`: *Walk* and *Run* fall back on *Stand*, *Stand* on *Closed* (147), the
  state of a door.
- **Checked over the client** (`every_animation_of_the_client_is_read_its_played_keys_kept`,
  ignored, run on `E:`): 23,190 models read in 0.9 s (1.0 s before, without them), 10,292 with
  bones that move, 315 at most; 30,409 sequences kept and 14.2 million keys of bones; their tracks
  linear (141,694) or in steps (7,518), none on a curve; 3 tracks left at rest in 2 models
  (`ValkierDark`, a scale of 0 times and 1 value; `10hgl_tundrasky04`, 1 time and 10 values);
  `AnimationData.dbc`, 506 animations. The two modern creatures of wow.export keep their *Stand*.
- **Tests**, on models the tests write: the sequences kept (*Stand*; what *Walk* is an alias of;
  not an emote, nor *Run* in an `.anim` file), their fields, the global sequence; three bones
  given a child before its parent, computed in the order of their parents; the keys of a kept
  sequence read, the others left; a rotation made whole; a Bézier track with its tangents, on a
  global sequence; a colour in a sequence not kept left; a weight on a global sequence; a texture
  transform; a track whose times and values differ left at rest, a parent past the bones left
  out, a loop of parents cut, each said; an animated model cut anywhere refused. Of 13 changes
  made on purpose to the reading, every one made a test fail, the field of the fallback that over
  the client.

#### Step 9.5b, as built

- **The motion** (`core/api`): `models::Motion`, carried by each `Instance`: standing; walking at
  a speed in yards a second; or moving at a speed without saying how. `live-world` gives it from
  the spline of each entity: along it, at the speed of the stretch it is on (its length over its
  time), walking when its flag 0x04 says so, moving otherwise; standing without a spline, or at its
  end unless the spline is cyclic.
- **Against the proposal, found while preparing the acceptance**: the observer sends no creature
  walking. AzerothCore gives a creature walking along its waypoints the speed of walking alone,
  not the flag of the unit (`MoveSplineInit::Launch`), and the spline of a monster in 3.3.5a has no
  flag of walking (`MoveSplineFlag`): of 19 creatures moving in Orgrimmar, none flagged, all at 2.2
  to 2.5 yards a second, the speed of walking. So a creature moving without the flag plays *Walk*
  or *Run*, whichever of their speeds is nearer its own, in yards of its model (its speed over its
  scale); *Walk* when the model has no *Run*.
- **The thread of the animations** (`models/src/animator.rs`), started once the pool is made and
  woken at each frame by the frame signal (*Threads*, above):
  - it keeps, for each instance of a look whose bones move (keys in a sequence kept or a global
    one), by its owner and id: the animation its motion asks for (*Stand*, *Walk*, *Run*); a
    variation picked by their frequencies, by its id and its loop, the same at every run; the
    sequence its alias leads to, whose keys are kept; through the fallbacks of `AnimationData.dbc`
    when the model has none (*Walk* and *Run* to *Stand*, *Stand* to *Closed*), at rest when none
    at all. It plays it at the speed of the instance over that of the sequence at the scale of the
    instance, so that the feet do not slide; standing, at the pace of its sequence; each instance
    from a moment of its own, by its id; a new animation from its start, the one before blended out
    over the time of blending of the new; a variation picked again at each loop;
  - it computes the bones of the instances of the groups in sight of the camera of the frame
    before, handed by the layer, their box widened by 8 yards and the view by a quarter on each
    side, so that what the camera turns or moves into sight between two frames is posed; within the
    reach of their size. Each bone turns about its pivot (moved to it, translated, rotated, scaled,
    moved back), placed by its parent, the parents first; two sequences mixed while one blends into
    the next; a billboard (flags 0x08 to 0x40, all taken as facing the camera whole) turned to the
    axes of the camera brought into the space of its instance, its scale kept. Split with
    `parallel_for`, four instances a slice, each into its own part of the bones kept from a frame
    to the next (their memory not taken again at each frame);
  - **the rotations by a slerp**, by the shorter arc, between two keys and between two sequences
    blending: the client of 3.3.5a interpolates its quaternions so, and a normalised lerp bends the
    arc between keys far apart (at a quarter of the way between keys 160 degrees apart, it turns
    by 31.6 degrees instead of 40, the test). The curves of rotation, which no bone of the client
    has (9.5a), are taken as slerps between their keys;
  - an instance out of sight goes on in time, its bones not computed: not drawn, it needs none;
    back in sight, it is posed at its time;
  - it writes one buffer: for each owner with an instance animated in sight, a table of a `u32` a
    instance in the order of its set, the first bone of the instance plus one, 0 for none; from the
    next multiple of 256 bytes, the bones, three rows of four floats each. Three buffers written in
    turn, straight into the staging memory of the queue (`Queue::write_buffer_with`); one grown is
    made mapped and filled, with room for a quarter more. Published at once with, for each owner,
    what it had published when its bones were computed (`Animated`). An instance whose bones have
    no keys, or a static one, costs nothing.
- **Drawn** (`layer.rs`, `choice.rs`, `skin.wgsl`):
  - the owners animated are drawn as the thread saw them, so that the table of their bones fits
    their instances when an owner regroups between the thread and the frame (an instance added is
    drawn a frame later); the others as they published last; the owners in the order of their
    numbers;
  - the GPU clears the table of the bones of the frame, then copies the table of each owner at the
    base of its instances, beside their copy of 9.4e;
  - the vertices gain their four bones and weights (48 bytes); a bone past those of its model
    weighs nothing. The vertex shaders of the pool and of the looks of their own pose each vertex
    by its four bones, each by its share of their weights, and leave it at rest for an instance
    without bones written. The looks of their own, in the pass, read their instances from the
    buffer of the frame, where the table finds them. On a device without the pool, their shader is
    made without storage buffers, every model at rest, as proposed;
  - an instance is chosen, drawn and reached by the larger of the radius of its model and those of
    its sequences kept.
- **Tests**, on the software adapter for what is drawn: the sequence of a motion through its
  aliases, its variations by their frequencies and the fallbacks, a loop of fallbacks ended; *Walk*
  or *Run* by the nearer speed at the scale of the instance; the time at the speed of the instance,
  a variation picked again at each loop; a new animation from its start, the one before blended
  out for its time; instances started apart, the same at every run; the tables, the bones aligned
  and their rows written for the instances in sight only, one past the side of the view within the
  view widened, the time of one out of sight going on, a buffer written again through the queue at
  the fourth step; vertices posed on the GPU from two owners, each from its base, a still one where
  it stands, at rest again once out of the thread's sight; an owner regrouping drawn as the thread
  saw it until its next step; a look of its own posed from the instances of the frame; a vertex
  moved by two bones by its share of their weights whatever their sum; a bone past the model
  weighing nothing, the radius of the sequences kept; a billboard facing the camera whatever the
  turn of its instance; and, in `pose`, the interpolations, a global sequence, the slerp, a child
  turning with its parent, two sequences blending. In `live-world`: the motion along a spline, and
  placed with its instance. Of 41 changes made on purpose to the thread, the shaders, the layer, the
  loading and `live-world`, every one made a test fail.
- **Accepted on the user's machine** (RTX 3080 Ti, Vulkan), the worldserver of `E:` running,
  observed only, with the script of 9.4d, run after run against 9.4e3 (the commit before, built
  apart) at the same clocks; the thread measured again once its memory was kept from a frame to the
  next (*final*):

  | | Orgrimmar: 9.5b; 9.4e3 | Dalaran: 9.5b; 9.4e3 |
  |---|---|---|
  | Animated, still | 236 to 241 instances, 41,200 to 42,400 bones, 1.9 MB a frame | 390 to 394 instances, 61,000 to 61,800 bones, 2.8 MB a frame |
  | Animated, flying | 187 to 288 instances, 23,600 to 52,300 bones, 1.1 to 2.4 MB | 21 to 348 instances, 2,100 to 53,200 bones, 0.1 to 2.4 MB |
  | The thread, final, on average over a second | 1.15 ms still, 0.94 to 1.36 flying; 1.39 at most still, 3.59 flying | 1.58 to 1.62 ms still, 1.19 to 1.53 flying; 1.97 at most still, 9.07 flying while loading |
  | The thread before | 1.42 to 1.86 ms still | 2.03 to 2.24 ms still |
  | Draws, still | 865 to 907; 902 to 910 | 2,942 to 2,990; 2,949 to 2,981 |
  | The view on the interface thread | 0.33 to 0.39 ms; 0.31 to 0.33 | 0.36 to 0.53 ms; 0.34 to 0.44 |
  | The models on the GPU, still | computing 0.22 to 0.32 ms, drawing 0.38 to 1.24; 0.28 to 0.29, 0.51 to 0.80 | computing 0.17 to 0.27 ms, drawing 0.59 to 1.37; 0.24 to 0.25, 0.49 to 0.60 |

  The thread split by a probe not kept, in Orgrimmar: 0.06 ms finding the instances, 0.55 to 0.88
  computing (6 to 10 of the 32 workers taking slices, the others coming after the last), 0.35 to
  0.54 writing. At a distance of 64 over Orgrimmar, the 988 tiles of Kalimdor loaded in 1.5 s, then
  flying 2,000 yards across the city in 20 seconds and back: the thread 0.34 to 1.78 ms a second
  on average, 4.84 at most; once 33.8 ms, its first step with instances (their states started, its
  buffers made). It never passed a frame: the helpers of `parallel_for` keep their turn after the
  jobs, as the review allowed; under 2 ms on average in both cities, the fallbacks are not built.
  Seen from four yards: Kruban Darkblade and a Troll Roof Stalker walking along their waypoints,
  their legs at another stride from a capture to the next.

#### Step 9.5b, after its review

- An instance keeps the look whose sequences it counts: when its look changes under the same id
  (a mount, a morph), it starts again in the model of its new look. Before, a blend toward a model
  of fewer sequences stopped the thread, and a sequence past them left the instance at rest.
- When the thread ends, whatever the cause, the bones it published last are forgotten: each owner
  is drawn from its last publication, at rest, not frozen as the thread last saw it.
- The loops a step goes over are counted at once, a variation picked once for the last; a
  sequence of no length, or the speed of a stretch of a millisecond, made hundreds a frame. A time
  beyond counting starts the sequence again.
- Tests: an instance changing look while it blends, toward a model without its run; one back to
  its look after one playing nothing, started afresh; an owner drawn from its last publication
  once the thread ends; a hundred million loops counted at once. Of 5 changes made on purpose,
  every one made a test fail.

#### Step 9.5c, as built

- **What moves in a material** (`models/src/dress.rs`): the colour of its batch (red, green, blue
  and alpha), its weight, and the transform of the coordinates of each texture it reads. A batch
  moves when one of its colour tracks or its weight takes more than one value through the keys
  kept, or when one of its textures has a transform, even holding one value (9.4 drew them
  unmoved). Each combination that moves is a slot of its look, the same for the batch at every
  level; the material of each batch points to its slot (the fourth number of its combination, 0
  for none). A batch unseen at rest (no alpha) is kept when it moves, and a material of no alpha
  draws nothing, as WotLK leaves it out.
- **A slot at a moment**: the colour of its track, else of the model at rest; its alpha times its
  weight; the rows of each transform: translated, then turned and scaled about the middle of the
  texture, as WMV reads them; at the moment of the instance's sequence, or of the global sequence
  of the track. While two sequences blend, the materials follow the sequence played.
- **Written with the bones**: a look whose bones or materials move is animated. The thread writes
  for each instance in sight its slots, four vectors each (its colour, then the two rows of each
  transform), just before its bones, its first slot nearest; the table of an owner now gives where
  the first bone of each instance begins, in vectors of four floats, plus one.
- **Read by the shaders** (`skin.wgsl`, `common.wgsl`): the vertex shader of the pool and of the
  looks of their own reads the slot of its material before the first bone of its instance; it
  passes on the colour and the coordinates of each texture, chosen (first set, second, or the
  environment) and moved by their transform, at the vertex rather than at the pixel. At rest,
  without a slot or without bones written, the colour of the material and the coordinates
  unmoved, as before; on a device without the pool, every material at rest.
- **Checked over the client**, by probes not kept: of 23,190 models, 1,144 have a texture
  transform, 796 a colour that moves and 937 a weight that moves. The falls of Coilfang
  (`Coilfang_waterfall_Type1`, 88 yards high) have 13 transforms, translations each on a global
  sequence of 0.7 to 3.8 seconds, and weights holding 0, 1, 0.59 and 0.25.
- **Tests**: a material moving by its colour, its weight or a transform, even holding one value,
  for the textures it reads only, and not when its tracks hold one value; a slot at a moment: its
  colour halfway between two keys, its alpha times its weight, a translation, a quarter turn and a
  scale about the middle, a sequence without keys at rest, a global sequence by the clock; the
  batches of two levels pointing to one slot, one unseen at rest kept while its weight moves; and
  on the software adapter: a colour moving drawn from its slot, in the pool and on the path of
  9.4c; the coordinates of a texture moved half across by its transform; a batch unseen at rest
  drawn once its alpha rises, opaque and alpha-keyed, in the pool and of its own; two slots of a
  look each read by its own batches. Of 20 changes made on purpose to `dress`, the plan, the
  thread and the shaders, every one made a test fail.
- **Accepted on the user's machine**, the worldserver of `E:` running, observed only:
  - a lava fall of Dragonblight (`BD_Lavafall01`) previewed against the sky: its lava flows, its
    pattern another at each capture;
  - the falls of Coilfang previewed against the sky: their instance placed, in sight and animated
    (11 slots), their 13 batches drawn, but no pixel seen, still or moving, as at rest in 9.4c2:
    the cause, found by the review, is the order of the layers (below);
  - the cities with the script of 9.4d, the GPU at 780 MHz (other clocks than the measures of
    9.5b, not compared): Orgrimmar 236 instances animated still, 41,225 bones and 16 slots, 1.9 MB
    a frame, the thread 0.76 to 0.90 ms on average, 1.11 at most; Dalaran 392 instances, 61,456
    bones and 164 slots, 2.8 MB, the thread 1.20 to 1.21 ms, 1.36 at most, 4.59 at most flying;
    the view on the interface thread 0.37 to 0.46 ms in Orgrimmar, 0.47 to 0.53 in Dalaran.

#### Step 9.5c, after its review

- **Found by the review**: the layers were drawn in the order they were added, which followed
  the alphabetical order of the folders of the modules starting without depending on each other:
  the models before the terrain. A blended batch writes no depth, so the tiles of the terrain drawn
  after it covered it, and so did the sky, drawn where the depth is still cleared: every blended
  batch in front of the ground or the sky was lost, the falls of Coilfang whole.
- **The order of the layers** (`core/api`, `viewport`): a layer gives its stage, `Stage::Ground`
  for the terrain and its sky, `Stage::Scene` by default; the view draws them by their stage, those
  of one stage in the order they were added, whatever the order the modules started in.
- **Tests**, on the software adapter: half red blended in front without writing the depth, added
  first, over the ground (green, writing its depth) and over the sky (blue, where the depth is
  cleared): both seen once the frame is drawn; the terrain drawn at the stage of the ground. Taking
  out the sort makes the first fail.
- **Accepted on the user's machine**: the falls of Coilfang previewed 400 yards above Elwynn, the
  statistics hidden: seen whole against the sky and in front of the ground, their water blue and
  streaked, their splash at their foot; flowing, 5,400 to 5,900 pixels changing from a capture to
  the next seen from their side, 1,400 to 2,400 seen from their edge, against the sky and the ground.

#### Step 9.6, proposed: the doodads, the buildings and the water

Asked after the review of 9.5c: the world around the terrain, as the client draws it, on the
base of 9.4e (the pool, the choice by the GPU) and of the order of the layers.

**Checked before proposing**, by probes not kept, over the client of `E:` and the exports of the
user's wow.export:

- **The doodads of the tiles** (`MDDF`, already read with the tiles): Azeroth 167,168 on 753
  tiles (2,991 on one tile at most, 2,269 models), Kalimdor 175,348 on 988 (2,781; 2,193), Outland
  209,924 on 800 (2,790; 2,677), Northrend 302,497 on 1,131 (3,576; 3,102). Within 300 yards of
  the middle of a city: Orgrimmar 26 (its doodads stand in its building), Stormwind 205 (15 of
  models that move), Dalaran 1,587 (47 models, 9 that move, 6 yards of radius on average),
  Shattrath 240.
- **The buildings** (`MODF`, already read): 1,882 to 2,285 a map, 39 to 63 on one tile at most,
  375 to 520 files of WMO a map. In the client, 1,986 WMO of version 17, 9,347 groups (306 for one
  at most), 7,548 portals and 259,338 doodads placed inside them; none modern. The cities:
  - Stormwind: 286 groups (278 indoor), 319 portals, 606 lights, 6,803 doodads in one set,
    727,741 triangles, 2,754 batches, 192 groups of vertex colours, 5 of liquid;
  - Dalaran: 91 groups, 102 portals, 4,957 doodads, 480,450 triangles, 1,619 batches, 7 of liquid;
  - Orgrimmar: 144 groups, 157 portals, 2,373 doodads, 379,079 triangles, 1,294 batches;
  - Shattrath: 29 groups, 39 portals, 3,510 doodads in 5 sets, 193,249 triangles.
- **The modern WMO** exported by wow.export (31 files): version 17 as well, with the files of their
  groups and of their doodads by FileDataID (`GFID`, `MODI`) and groups at levels of detail
  (`_lod1` to `_lod3`); new chunks of lights and of their own (`MAVG`, `MNLD`, `MFED`, `MGI2`).
- **The water**: `MH2O` in 529 tiles of Azeroth, 746 of Kalimdor, 1,022 of Northrend (92,206,
  129,596 and 226,762 chunks with liquid, two layers at most); Outland has only the older `MCLQ`
  in its chunks (61,874), Azeroth 505 more. The types of `LiquidType.dbc` (26) fall into water,
  ocean, magma and slime; in Azeroth and Northrend, the oceans are most of it (308,256 layers),
  then slow water, magma, the lake of Wintergrasp, an orange slime. The groups of a building hold
  their own liquid (`MLIQ`).

**In six parts, each reviewed before the next:**

- **9.6a, the two phases of drawing** (asked by the review of 9.5c). A layer draws what it draws
  opaque, then what it blends, in two calls; the view draws the opaque of every layer, then the
  sky (the terrain's, still where nothing is drawn), then the blended of every layer, the order of
  the stages kept within each phase. `Layer::draw` and `draw_pass` take the phase; a layer of one
  phase only draws nothing in the other. The terrain and the models split as they are: the models
  already draw their opaque states, then their blended ones. Tested on the software adapter: a
  blended batch of a layer drawn before an opaque one of a later layer, behind it, hidden; in
  front, seen over it.
- **9.6b, the doodads of the tiles**, in a module `doodads` of their own (decision of the user):
  - the service `formats` gains a light call, the placements of a tile (its doodads and its
    buildings) without its terrain: the file read, its chunks of terrain not parsed;
  - the module follows the map the terrain shows by the command `terrain.map`, as `live-world`
    does, and chooses its tiles itself: those around the camera within a distance of its own, a
    setting of its panel, by jobs, the nearest first;
  - it places the doodads of each tile through the service `models`, an owner a tile
    (`doodads/<map>/<x>_<y>`), given once and cleared when the tile is left: no set merged again at
    each change. Each at the transform of its file (its position, its rotation in degrees, its
    scale in 1024ths), its look the model of its file with its own textures. The models choose them
    by the GPU (9.4e3), cut by their size and their reach, posed by the thread when they move
    (torches, banners);
  - without the module, the terrain is drawn without its doodads, as the architecture wants of a
    module taken out;
  - measured in the four cities and on the tile of most doodads, against 9.5: the frame, the choice
    on the GPU (at a high clock, as asked for 9.6), the interface thread, the memory; the points
    left for 9.6 (records without instances, the writing of the bones) built only if the measure
    asks.
- **9.6c, the WMO read**, in `assets`: `formats::Wmo` as plain data: its materials (shader,
  blending, flags, textures, colours), its groups (flags, bounds, vertices, normals, one or two
  sets of coordinates, vertex colours, triangles with their materials, batches, the doodads they
  hold, their liquid), its portals and their references, its doodad sets and doodads, its fog and
  its lights; of version 17 and modern (`GFID`, `MODI`, the groups at their finest level). Checked
  over every WMO of the client and the exports of wow.export.
- **9.6d, the buildings drawn**: a module `buildings` and its layer, at the stage of the scene,
  built as `doodads` (decision of the user): it follows the map by `terrain.map`, chooses its tiles
  within a distance of its own and reads their buildings by the placements of `formats`. The
  groups go to arenas on the GPU and their textures to arrays (`core/api`), drawn in the pass by a
  draw per state over the groups seen: from outside, each group by its bounds; from inside, the
  groups seen through the portals in sight from the group of the camera, as the client does, on
  the CPU; their shaders of WotLK (diffuse, specular, metal, environment, the two layers), their
  vertex colours inside. The doodads of the set a building names placed through `models`, an owner
  a building.
- **9.6e, the water**: the liquids of the tiles (`MH2O`, and `MCLQ` for Outland) and of the
  groups of the buildings (`MLIQ`), meshes by layer and by group, drawn blended in the second
  phase, their textures turning through their frames as `LiquidType.dbc` names them; the oceans
  of a map to its edges.
- **9.6f, the occlusion on the GPU**, beside the portals (asked by the user): a culling of
  today's engines, in two passes at each frame: first what was seen at the frame before is drawn;
  the depth it leaves is reduced to a pyramid of its farthest values (Hi-Z) by a compute pass;
  then the rest is tested against the pyramid, each instance of `models` and each group of
  `buildings` by its bounds, and those found in sight drawn, so that nothing appears a frame late.
  Its test joins the choice by the GPU (9.4e3) and the draws of the groups; measured in the four
  cities with and without it.

**Not in 9.6**: the lights of the buildings and of the map (9.7), the shadows, the reflections and
the refraction of the water, the levels of detail of the buildings (their finest only), the doodads
of detail of the ground (`MCLY` ground effects), the destructible buildings.

**Decided by the user**, after the proposal:

- The doodads in a module `doodads` of their own, to keep to the design of the modules: it reads
  the placements of its tiles itself, through the light call of `formats`, so that a tile both it
  and the terrain need is read twice from the archives but its terrain parsed once.
- The buildings in a module of their own, with their layer, rather than in `models`, built the
  same way.
- The culling by portals on the CPU, and the occlusion on the GPU of today's engines beside it
  (9.6f).
- The two phases (9.6a) before anything else, so that the water and the glass of the buildings are
  drawn over what stands behind them whatever the layer.

#### Step 9.6, added by the review of the proposal

The plan and the decisions are validated, with these additions:

- **9.6b: the cost of a frame follows all that is loaded, not only what is in sight.** On the
  interface thread, `ModelsLayer::prepare` goes at each frame over every group (owner, look, tile)
  of every owner: the look looked up, the box, the test of the view; the looks with blended
  batches add work for each instance (its distance, the candidates, a map, the order). On the
  thread of the animations, `Animator::step` advances every animated instance loaded, in sight or
  not, and inserts it into the set of those seen. With the doodads, a group is nearly a doodad: up
  to 3,576 doodads of 3,102 models on a tile of Northrend, so that a few tiles loaded make tens of
  thousands of groups a frame on the interface thread. Measured besides the cities and one tile:
  the densest area of Northrend at the greatest distance of the module, its interface thread, its
  thread of the animations, the groups and the animated instances loaded. Remedies ready should
  the measure ask:
  - the bounds of each owner (a tile) tested before its groups;
  - an animated instance out of sight not advanced at each frame: it catches up its time by the
    clock when it comes back in sight.
- **9.6a: no call of its own for the sky.** The ground comes first in each phase, so that the
  terrain draws its sky at the beginning of its blended phase, where nothing opaque is drawn.
- **9.6e: the water is of the ground, drawn before all the blended of the scene**, so that a
  blended batch under its surface would be drawn over it. The rule is fixed with the part, whether
  the water writes the depth or not, and tested.
- **9.6f: the pyramid of depth cuts the pass.** The target is multisampled four times
  (`Depth32Float`, a `RENDER_ATTACHMENT` only): the depth needs `TEXTURE_BINDING` and a reading as
  `texture_depth_multisampled_2d`; the colour and the depth stored, then loaded again, between the
  two passes; and a point of computing between the passes in the interface of the layers. The cost
  of that cut alone is measured, so that the occlusion gains more than it costs.

#### Step 9.6a, as built

- **The phases** (`core/api`): `viewport::Phase`, the opaque then the blended. `Layer::draw` and
  `Layer::draw_pass` take the phase: a layer draws, in each call, what it draws in that phase.
- **The view** (`viewport`): it records two bundles for a layer drawing in bundles, one a phase,
  kept together while its version stays; then in the pass, the grid, the opaque phase of every
  layer, then the blended phase of every layer, by their stage within each, in the order they were
  added within one. A layer that panics drawing in the first phase is not drawn in the second. The
  GPU times each layer's drawing in both phases, six timestamps a layer.
- **The layers**: the terrain draws its tiles and its horizon in the opaque phase, and its sky at
  the beginning of the blended one, the ground drawn first there, where nothing opaque was drawn
  (as the review of the proposal said, without a call of its own); the models their opaque states
  then, in the blended phase, their blended ones, in the pool and on the path of 9.4c alike; the
  markers of `live-world`, the cube and the faulty layer draw in the opaque phase only.
- **Tests**, on the software adapter: half red blended, of a layer added first, in front of the
  green opaque of a layer added after it, seen over it; behind it, hidden; the blended of the scene
  over the ground and over the sky as in the review of 9.5c, the sky in the blended phase; the
  terrain's sky absent from its opaque phase; a layer panicking in the first phase drawn once; a
  layer timed drawing in both phases. Of 9 changes made on purpose to the view, its timer, the
  terrain and the models, every one made a test fail.
- **Accepted on the user's machine**, the worldserver of `E:` running, observed only, with the
  script of 9.4d, run after run against the commit before (built apart): in Orgrimmar, the GPU at
  210 to 225 MHz, the view on the interface thread 0.35 to 0.41 ms both, the frame on the GPU within
  0.1 ms of the commit before (1.56 to 2.49 ms; the second run of 9.6a equal to it), the models
  computing 0.40 to 0.44 ms and drawing 0.35 to 0.79 alike; in Dalaran, the GPU at 390 to 435 MHz,
  the times as close within their noise. The two phases cost nothing that shows.

#### Step 9.6b, as built

- **The placements of a tile** (`core/api`, `assets`): `Formats::placements`, its doodads and its
  buildings (`formats::Placements`), by default from the whole tile. `assets` reads them alone: the
  `_obj0` of a tile split, as the whole tile is read (when its `_tex0` exists), else its root; of
  its chunks, those of the names and the placements only. Over the four continents of the client,
  the 3,672 tiles give the same doodads and buildings as the whole tiles, read in 4.2 ms on average
  instead of 6.4 (16 threads at once).
- **Checked before building**, by probes not kept: a doodad near the border of its tile is listed
  by the tiles its bounds reach, under the same unique id, its data the same (7,593 in Azeroth,
  11,715 in Kalimdor, 9,344 in Outland, 10,637 in Northrend, up to 16 tiles for one); in Kalimdor,
  the birds of the tile 39_23 list again, under the ids of their neighbours', other positions, and
  33 doodads in all are listed only by a tile they do not stand in. A doodad is therefore kept by
  its unique id, as the client keeps it, not by the tile it stands in.
- **The module `doodads`** (category World, using `viewport`, `models` and `formats`): at each frame
  it reads the map the terrain shows by the command `terrain.map`, and the WDT of that map by a
  job; at each frame signal, the tiles whose centre lies within its distance and half a tile of the
  camera, as the terrain chooses its own, those placed kept within a whole tile more. It starts the
  reads of the nearest first, as many at once as the workers but one: a job reads the placements
  of a tile, makes its instances and places them. Its panel sets the distance (2 tiles by default,
  1 to 4) and tells the tiles placed, read, refused, the doodads placed and listed, and how long
  all took once the map or the distance changed (said in the log too).
- **An instance a doodad**: its look the model of its file with its own textures and all its
  submeshes; its id its unique id, the first of an id a tile lists twice; its transform from the
  axes of its file (the world's X `32·TILE` less its Z, its Y `32·TILE` less its X, its Z its Y), its
  rotation in degrees as Noggit applies it, `Rz(Y − 90°)·Rx(X)·Ry(−Z)·Rz(−90°)` in the world's axes,
  and its scale.
- **The owners** (`doodads/<map>/<x>_<y>`): what the jobs share is kept under a lock, so that a
  tile is placed only while wanted, and taken away by a job once it is not. A doodad listed by
  several tiles is placed by the first of them placed; when that tile is left, it goes to the
  first by its place of the tiles placed that list it, by `Models::change`. Another map takes the
  tiles of the one before away at once.
- **A module that fails** (`models`): the owners it names `<id>/…` are cleared with the owner its id
  names.
- **Tests**: the placements of a tile of 3.3.5a and of a split one, the same as the doodads and
  buildings of the whole tile; the service reading them from the `_obj0` of a split tile, an
  `_obj0` without its `_tex0` left unread as the whole tile leaves it, none for a tile its WDT does
  not name; over the client, when `UNIWOW_CLIENT` names it, those of every tile of the four
  continents the same as the whole tile's. The tiles wanted around the camera (their axes, the
  nearest first, those held within a tile more, none the WDT does not name); the transform against
  Noggit's, built in its axes, for seven rotations, and the position; the instances of a tile once
  each by their id, with their look; a doodad listed by three tiles placed once and handed on; a
  tile placed only while wanted, another map taking all away; the owners of a failed module cleared,
  not those of a module whose id begins the same. Of 18 changes made on purpose to the doodads,
  the models and the reading of the placements, every one made a test fail.
- **Accepted on the user's machine**, the worldserver of `E:` running, observed only, with the
  script of 9.4d, run after run with the module, without it, with it again (the commit before
  without its doodads): the doodads turned as the client turns them, two lines of fences of Elwynn
  joined along their path and the rows of the vines of Brackwell along the fence of their field. At
  2 tiles, with the doodads against without, the view on the interface thread 0.46 to 0.53 ms
  against 0.37 to 0.40 in Orgrimmar, 0.54 to 0.61 against 0.40 to 0.47 in Dalaran, 0.42 to 0.45
  against 0.34 to 0.35 in Stormwind, 0.53 to 0.62 against 0.36 to 0.38 in Shattrath, 0.50 to 0.53
  against 0.33 to 0.35 on the tile of most doodads (Northrend 23_21); the models on that thread
  0.37 to 0.86 ms against 0.14 to 0.42; their groups 1,081 to 2,224 against 32 to 515, their looks
  on the GPU 429 to 1,126 against 26 to 461; the frame on the GPU alike within its noise (0.7 to
  2.2 ms both), the choice by the GPU 0.03 to 0.3 ms more; the memory of the process 0.1 to 0.5 GB
  more. In the densest area of Northrend within 4 tiles (around 21_24, 68 tiles placed, 73,061
  doodads of 75,103 listed): the view on the interface thread 0.73 to 0.81 ms against 0.35, the
  models there 1.04 to 1.13 ms (their steering 0.57 to 0.63) against 0.15, 4,297 to 4,832 groups
  against 66 to 77, 1,361 to 1,450 looks on the GPU (396 to 413 MB) against 52 to 67; the thread of
  the animations 1.93 to 2.31 ms on average and 3.13 at most, over 2,125 to 3,711 instances, against
  0.44 to 0.60; the choice by the GPU 0.60 to 0.70 ms against 0.21 to 0.27; the memory of the
  process 3.5 to 4.1 GB against 2.6. The 18 to 61 tiles around the middle of a map placed in 0.1
  to 0.2 s. Every cost stays far within a frame: the remedies of the review of the proposal are not
  built. The GPU stayed at its low clock throughout (425 to 555 MHz on average, of 2,145), the
  densest area not loading it enough: the choice by the GPU at a high clock is still to measure.

#### Step 9.6b, after its review

Validated, nothing to correct. The measure in the densest area of Northrend at 4 tiles (0.8 ms on
the interface thread, 2.3 ms on the thread of the animations, about 4,800 groups) justifies leaving
the remedies unbuilt. Kept for 9.6d:

- **The memory of the process**: at 4 tiles, 0.9 to 1.5 GB more, for about 400 MB of looks on the
  GPU. When the buildings come, the memory on the CPU is given by its parts (the models kept, the
  animations, the caches), to know where the surplus comes from.
- **The choice by the GPU at a high clock** is still to measure.

#### Step 9.6c, as built

- **The building as plain data** (`core/api`): `formats::Wmo`, its flags, ambient colour, id,
  bounds and sky; its materials (`WmoMaterial`: flags, shader, blending, three textures, emissive,
  diffuse and third colours, ground); its groups (`WmoGroup`: name, flags, bounds, portals, batches
  by kind, fogs, type of liquid, id; vertices, normals, every set of coordinates and of vertex
  colours, triangles, the flags and material of each, batches, doodads held, liquid); its portals
  and their references, lights, doodad sets, doodads (a quaternion each) and fogs; its faults. Its
  colours red, green, blue and alpha. `Formats::wmo` reads it with its groups at their finest
  level.
- **The reader** (`assets`, `wmo.rs`), from the public description of the format: the groups of
  3.3.5a beside the root by its name and their index (`_000`), the modern ones by the first entries
  of `GFID`, the finest level; the doodads by their place in `MODI` and the textures by FileDataID
  when the root has no `MOTX`, as the loader of wow.export reads them; the triangles of `MPY2`; the
  material of a batch past 255 in the last of its bounds when its flag 0x2 says so (wow.export reads
  only its byte). The groups are read on the workers of the pool, a group a slice.
- **Checked before building**, over the client: no `MOTX` begins with an empty name, so that the
  offset 0 names the first texture (2,047 materials); a texture left out names an empty name; the
  field of the third texture holds other data in 3.3.5a (0.1 in 14,459 materials), and its shaders
  go to 6 only: a third texture is read only for the shaders of later clients. The groups of
  Darnassus and of the World Trees declare their `MOGP` 44 to 128 bytes longer than their file,
  the chunks inside ending with it: read to the end of the file, as the client reads it. The
  headers count 259,338 doodads, the count of the proposal; their chunks hold 250,296, which the
  sets fit in, and the chunks are read.
- **What a building lacks**, said in its faults and kept in its places: a batch out of its
  triangles or vertices or of a material it does not have drawn as nothing, a triangle of such a
  material only collided with, a doodad held that it does not have left out, a set past its
  doodads cut, a group missing kept empty with the flags and bounds of its root; a portal reference
  or a range of portals past what it has said.
- **Tests**, on buildings the tests write: one of 3.3.5a read whole, field by field (its second
  texture an empty name, its third field 0.1, its liquid, both sets of coordinates and of colours);
  a modern one, its groups asked by their finest FileDataIDs, its doodads and textures by
  FileDataID, the third texture of a later shader, the large material of a batch, its empty sky;
  what a building lacks, eight faults; a group longer in its header than its file; damaged at
  random, refused or said and never a panic; read through an archive by the service. Over the
  client, when `UNIWOW_CLIENT` names it: the 1,986 buildings without a fault, 9,347 groups,
  31,712,510 triangles, 7,548 portals, 406 liquids; Stormwind as the proposal counted it (286
  groups, 278 inside, 319 portals, 606 lights, 727,741 triangles, 2,754 batches, 192 groups
  coloured, 5 liquids), its doodads 6,157. Over the exports of wow.export, when `UNIWOW_MODERN`
  names them: the 7 modern buildings without a fault, their groups whole at their finest level,
  their textures those wow.export wrote. Of 26 changes made on purpose to the reader, every one
  made a test fail.
- **Measured** on the user's machine: the 1,986 buildings in 0.65 s on 16 threads, the archives
  read once already; Stormwind alone in 174 ms, its groups on one thread.

#### Step 9.6c, after its review

Validated, nothing to correct: the offsets checked against the public description of the format,
the colours read as `0xAARRGGBB` and given red first, as Noggit reads the ambient colour. Two rules
of the client, to be written into the proposal of 9.6d:

- **The doodads of a building**: its set 0 (`Set_$DefaultGlobal`) is always placed, and besides it
  the set its placement (`MODF`) names when that is another.
- **The vertex colours of the insides**: the client of 3.3.5a fixes them when it loads a group
  (`FixColorVertexAlpha`, by its batches of transition, inside and outside, and the ambient colour),
  unless the flag 0x8 of `MOHD` forbids it. Without it, the insides are too dark or too light.

#### Step 9.6d, proposed: the buildings drawn

Asked by the review of 9.6c, on the plan of 9.6 (a module `buildings` and its layer, built as
`doodads`; the groups in arenas, their textures in arrays; seen by their bounds from outside and
through the portals from inside, on the CPU; the shaders of WotLK; the doodads of their sets through
`models`), with the two rules of that review and the points kept since 9.6b.

**Checked before proposing**, by probes not kept, over the client of `E:`:

- **The buildings placed** (`MODF`, by their unique id): 1,673 in Azeroth, 1,430 in Kalimdor, 1,659
  in Outland, 1,528 in Northrend; 292, 142, 83 and 83 of them name a doodad set other than 0. Their
  flags are 0 but for 57 of Northrend (0x1, the destructible ones of Wintergrasp). Every file they
  name is read by 9.6c.
- **Around the cities**, the buildings whose position lies within 2.5 and 4.5 tiles:

  | Place | Buildings (files) | Vertices | Triangles | Textures (MB) | Doodads of their sets |
  |---|---|---|---|---|---|
  | Stormwind, 2 tiles | 62 (36) | 1,012,906 | 980,521 | 309 (93) | 8,831 |
  | Stormwind, 4 tiles | 137 (77) | 1,418,461 | 1,378,327 | 540 (168) | 13,498 |
  | Ironforge, 2 tiles | 56 (37) | 522,251 | 542,674 | 277 (67) | 7,021 |
  | Orgrimmar, 2 tiles | 38 (27) | 390,220 | 521,696 | 186 (57) | 2,766 |
  | Shattrath, 2 tiles | 140 (65) | 381,330 | 427,219 | 264 (77) | 4,619 |
  | Dalaran, 2 tiles | 108 (47) | 621,782 | 673,833 | 340 (126) | 4,800 |
  | Dalaran, 4 tiles | 345 (150) | 1,789,460 | 1,853,866 | 800 (295) | 11,331 |

  Their textures are of 512 × 512 mostly, some of 1,024, one of 2,048 near Dalaran.
- **The groups**: all 9,347 have a BSP tree (`MOBN`, `MOBR`), which 9.6c does not read; 6,222 are
  inside, 3,125 outside; 6,224 have vertex colours, 31 a second set of colours and of coordinates;
  at most 32,761 vertices (16-bit indices hold them) and 1,453 batches a group, 306 groups and 342
  portals a building.
- **The materials**: shaders 0 to 6 (22,836 diffuse, 1,273 environment metal, 517 specular, 192
  environment, 90 metal, 73 opaque, 52 of two layers); blendings opaque (23,911), alpha key (1,105),
  alpha (13), add (3), mod2x (1); flags unlit 495, unfogged 6, two-sided 1,022, lit as outside 96,
  lit at night 323, window 102, clamped 660 across and 689 up and down.
- **The roots**: their flag 0x8 (the vertex colours not fixed) in 124 buildings, 0x2 (lit as one,
  the ambient colour taken as none) in 162.
- **The fix of the vertex colours** (`FixColorVertexAlpha`), as the public description and Noggit
  give it, to be checked against the client: the vertices of the batches of transition, up to the
  last vertex of the last of them, lose the ambient colour and are darkened by their alpha; the
  others lose the ambient colour and are brightened by their alpha, their alpha then 255 in a group
  outside, 0 inside; the colours halved, the shader doubling them. With the flag 0x8, only that
  alpha of the vertices after those of transition is set.

**In two parts, each reviewed before the next:**

- **9.6d1, the buildings seen from outside.**
  - The module `buildings` (category World, using `viewport`, `models` and `formats`), steered as
    `doodads`: the map of `terrain.map`, the tiles around the camera within a distance of its own,
    their placements read by jobs, the nearest first, a building kept by its unique id while a tile
    listing it is held. The choice of the tiles and that keeping are those of `doodads`, moved to
    `core/api` and shared, as the transform of a placement.
  - A file of a building read once by a job for all its placements, its vertex colours fixed there
    as the client fixes them; its groups put in two arenas of the GPU, vertices (about 40 bytes
    each: position, normal, both sets of coordinates and of colours) and 16-bit indices, the arena
    of `models` moved to `core/api` and shared; its textures in the arrays of `core/api`. The
    memory told to the budget of the view, as the terrain and the models tell it.
  - Its layer at the stage of the scene: each placement tested by its bounds, then each of its
    groups by its own, against the view; the batches of the groups seen drawn by
    `multi_draw_indexed_indirect`, a draw a pipeline (shader, blending, two-sided), the transform
    and material of each batch read by its instance index. The opaque and alpha-keyed batches in the
    opaque phase, the others in the blended phase, the farthest group first.
  - The shaders of 3.3.5a: diffuse, specular, metal, environment, opaque, environment metal, two
    layers (the second texture blended by the alpha of the second colours); lit as the models are,
    by the sun of the view and its ambient light, the insides by their vertex colours and the
    ambient colour of the building, the vertices of transition blending the two by their alpha;
    unlit and unfogged as their flags say, clamped as their flags say.
  - The doodads of each building through `models`, an owner a building
    (`buildings/<map>/<unique id>`): its set 0 always, and the set its placement names when that is
    another; each at the transform of the building times its own (its position, its quaternion, its
    scale).
  - Measured in Stormwind, Ironforge, Orgrimmar, Shattrath and Dalaran at 2 and 4 tiles, against the
    commit before: the frame, the GPU of the layer, the interface thread, the thread of the
    animations; the memory of the process given by its parts (the models kept, the animations, the
    caches, the buildings kept), as the review of 9.6b asked; and, once, the choice of `models` on
    the GPU at a high clock, as asked for 9.6.
- **9.6d2, the buildings seen from inside.** The BSP of each group read (`formats`); the group the
  camera is in found as the client finds it, by the BSP below the camera among the groups inside
  whose bounds hold it; from it, the groups seen through the portals in sight, each portal clipping
  the view it lets through, the side of the camera tested by the plane of the portal; the doodads
  of a group drawn only when it is seen. Measured inside Stormwind, Ironforge and Dalaran.

**Not in 9.6d**: the water of the buildings (`MLIQ`, 9.6e); their lights, their fogs, their sky
(`MOLT`, `MFOG`, `MOSB`) and the glow of their materials at night, with the lights of the map (9.7);
the colour of a doodad inside a building, which its lighting needs (9.7); the destructible
buildings in any state but whole; the occlusion on the GPU (9.6f). The blended batches of the
buildings and of the models are sorted within each layer only.

**To decide:**

1. The two parts, the portals in their own (recommended), or one step.
2. The choice of the tiles, the keeping by unique id, the transform of a placement and the arena
   moved to `core/api` and shared between `doodads`, `buildings` and `models` (recommended), or
   copied into `buildings`.
3. The distance of the buildings: 1 to 8 tiles, 3 by default, as the terrain's.
4. The high clock of the GPU for its measure: the profile of the driver set to its highest
   performance for `UniWoW.exe` during the measure, then set back, which changes a setting of the
   system and needs your leave.

#### Step 9.6d1, as built: the buildings seen from outside

Built without a review between the proposal and the buildings drawn, as the user asked: the
questions met on the way are noted at the end, with their possibilities, for the review.

- **Shared in `core/api`**: the arena of a buffer of the GPU (`arena`, moved from `models`, its
  tests with it); the tile of a map (`formats::TileId`, `TILE`, `ORIGIN`), the tiles around the
  camera (`formats::tiles_around`) and the transform of a placement of a tile
  (`formats::placement`), moved from `doodads`, which uses them, their tests with them.
- **The module `buildings`** (category World, using `viewport`, `models` and `formats`), steered as
  `doodads`: the map of `terrain.map`, its WDT by a job, the tiles around the camera within its
  distance (1 to 8 tiles, 3 by default) read by jobs, the nearest first, as many at once as the
  workers but one; a building kept by its unique id while a tile listing it is held (`keeping`);
  each file read once by a job for all its placements, dropped when no building kept has it.
- **A file on the GPU** (`gpu`): its vertex colours fixed as the client fixes them (`colours`), its
  groups one after the other in the arena of the vertices (40 bytes each: position, normal, both
  sets of coordinates and of colours) and their indices in that of the indices (32 bits, after the
  vertices before them), its materials in a table, their textures in arrays of `core/api` (64 a
  stage, their own); its ranges given back when it is dropped. What the buildings hold on the GPU
  told to the budget of the view, as a fixed cost.
- **The layer**, at the stage of the scene, drawn in the pass: at each frame, each placement tested
  by its bounds against the sides of the view, then each of its groups by its own; the batches of
  the groups in sight listed as indirect draws, their entry (the placement, the material, the
  flags of the group and the kind of the batch) read by their instance index; the opaque and
  alpha-keyed ones in the opaque phase by state (blending, two sides), the others in the blended
  phase from the farthest group; a `multi_draw_indexed_indirect` for each run of one state. Drawn
  only on a device offering the first instance of an indirect draw, storage buffers read by
  vertices and 64 textures a stage.
- **The shading** (`buildings.wgsl`): the pixel shaders of 3.3.5a combined in gamma (diffuse,
  specular and metal as diffuse, environment and environment metal adding the second texture
  mapped on the environment by the alpha of the first, opaque, two layers blending the second
  texture by the alpha of the second colours); held to their edges as their flags say. A batch is
  lit by its kind, as the three counts of its group give it: outside by the sun and the ambient
  light of the view, inside by its vertex colours doubled back and the ambient colour of its
  building, of a transition by both, blended by the alpha the fix leaves; a material lit as
  outside wholly outside, an unlit one not lit; every batch of a building lit as one (its flag 0x2)
  lit as outside. An opaque batch keeps every pixel, the alpha of its textures a mask of their own;
  an alpha-keyed one those over its key, both written opaque. Fogged as the models are, but where
  unfogged.
- **Their doodads** (`doodads`), through `models`, an owner a building
  (`buildings/<map>/<unique id>`): its set 0 and the set its placement names when another, each at
  the transform of the building times its own (its position, its quaternion as the file gives it,
  which Noggit converts then inverts to the same, its scale), placed by a job and taken away as
  `doodads` takes its tiles away, under a lock.
- **The memory on the CPU** (asked by the review of 9.6b): `models` says on a line of its own what
  the models it holds keep there, each once (their skins and the rest, their animations, counted
  from their lists); `buildings` what its files keep.
- **Tests**: a building listed by several tiles kept once, and let go when no tile lists it; the
  vertex colours fixed as the client fixes them (the transition darkened, the others brightened,
  the flags 0x2 and 0x8, held to a byte); the groups of a file one after the other; a box in sight
  of the view, across it and along it; the doodads of the set 0 and of the set named, at the
  building's transform times their own, placed while the building is kept and taken away after;
  the CPU a model keeps. Drawn on the software adapter: a square seen from its front, its back
  culled but where two-sided; lit inside by its vertex colours, outside by the sun; a batch outside
  of a group inside lit by the sun, a transition by its colours; a building lit as one lit as
  outside; an opaque batch keeping the pixels its texture's alpha leaves out, an alpha-keyed one
  not; the opaque batches by state, the blended from the farthest. Of 31 changes made on purpose
  to the module and to the count of `models`, every one made a test fail; one more, on a test of
  the doubles of a tile, showed the code it changed to change nothing, and that code was taken
  out.
- **Seen on the user's machine**: Goldshire, its inn and its smithy; Stormwind from above its gates,
  its walls, bridge, statues and cathedral, its trade and mage quarters near, their roofs, walls
  and streets; the doodads of the tiles and of the buildings among them.
- **Measured on the user's machine**, the worldserver of `E:` running, observed only, with the
  script of 9.4d, run after run with the module, without it, with it again, the buildings and the
  terrain at 3 tiles, the doodads at 2, the GPU at its low clock (354 to 586 MHz on average):

  | Place | View, interface (ms) | Buildings: in sight of kept, groups, batches | Their GPU (MB, ms) | `models` on the interface (ms) | Private memory (GB) |
  |---|---|---|---|---|---|
  | Stormwind | 0.73 to 0.75 against 0.39 to 0.40 | 13 to 16 of 90, 72 to 161 groups, 826 to 1,527 | 290 to 301, 0.60 to 0.84 | 0.80 to 0.86 against 0.38 | 3.4 to 3.7 against 2.2 |
  | Orgrimmar | 0.63 to 0.70 against 0.43 to 0.45 | 1 of 98 to 147, 24 to 73, 416 to 792 | 220 to 226, 0.26 to 0.65 | 0.71 to 0.89 against 0.46 to 0.51 | 3.1 to 3.2 against 2.5 to 2.6 |
  | Shattrath | 0.79 to 0.82 against 0.47 to 0.49 | 1 of 281 to 308, 6 to 10, 176 to 254 | 204, 0.27 to 0.43 | 1.14 to 1.23 against 0.65 to 0.68 | 3.8 to 4.1 against 2.4 to 2.8 |
  | Dalaran | 0.91 to 1.07 against 0.48 to 0.54 | 15 to 16 of 226 to 241, 45 to 98, 602 to 1,518 | 405 to 416, 0.23 to 0.84 | 1.49 to 1.70 against 0.72 to 0.79 | 5.3 to 5.9 against 3.1 |
  | Dalaran, 4 tiles | 1.09 to 1.22 against 0.51 to 0.55 | 18 of 406, 50 to 105, 671 to 1,595 | 591 to 601, 0.64 to 0.90 | 1.79 to 1.97 against 0.71 to 0.79 | 6.3 to 8.2 against 3.7 |

  The layer of the buildings takes 0.08 to 0.14 ms of the interface thread; the rest of what it
  adds is in `models`, whose groups the doodads of the buildings make 2.5 times as many (5,007 to
  5,373 against 1,886 to 1,934 in Dalaran at 4 tiles): the remedy of the review of 9.6b, the bounds
  of an owner tested before its groups, is now worth weighing. The frame on the GPU alike within
  its noise. The memory on the CPU by its parts (the review of 9.6b): the models held 12 to 35 MB,
  their animations 56 to 126 MB, the models of the tiles of the terrain 142 to 286 MB, the files
  of the buildings 0.6 to 2 MB; the private memory of the process grows by 0.5 to 4.5 GB while
  its working set grows by 0.1 GB: the surplus is not the data the modules keep but memory
  committed and not touched, likely the buffers each upload makes (`create_buffer_init` for each
  range of an arena and each texture) kept by the driver; to measure apart, a belt of staging
  reused being the remedy. The buildings of the 15 to 61 tiles around the middle of a map drawn
  in 0.2 to 0.6 s.
- **Decided while building, without a review** (the user asked to go on to the buildings drawn):
  the four points of the proposal taken as recommended: 9.6d1 first, the portals in 9.6d2; the
  transform of a placement, the choice of the tiles and the arena moved to `core/api` (the keeping
  by unique id written apart in `buildings`, simpler than that of `doodads`: an owner a building,
  no doodad handed on); the distance 1 to 8 tiles, 3 by default; no setting of the system changed
  for the clock of the GPU.
- **Found by the first sight of the buildings**, and corrected:
  - the outer walls of the rooms of a building are batches outside in groups inside (the hall of
    the inn of Goldshire: no batch of transition, 6 inside, 1 outside): lit as inside by their
    vertex colours brightened, they were white. A batch is lit by its kind, as the three counts of
    its group give it: outside by the sun, inside by the vertex colours, of a transition by both;
  - the alpha of the textures of the buildings is a mask (a third of the texels of the walls and
    roofs of Stormwind at 0): an opaque batch tested by it showed the sky through its holes, white
    and speckled. An opaque batch keeps every pixel, an alpha-keyed one tests its key, both written
    opaque;
  - the 162 buildings lit as one (0x2), Stormwind among them (its flags 0xF), have vertex colours
    nearly black (1 to 8 of 255 on average) and not fixed: their insides were black from outside.
    Every batch of such a building is lit as outside.
- **Questions noted while building, with their possibilities:**
  1. The light inside: the vertex colours (fixed, doubled back) and the ambient colour of the
     building, the light outside the sun and the ambient light of the view, a transition blended by
     the alpha the fix leaves, a material lit as outside (0x8) wholly outside; after the public
     description and the shaders of later clients. To compare with the client in the same places,
     by captures of the game (`jeu.ps1`) if you allow it.
  2. The buildings lit as one (0x2): lit as outside here; what the client of 3.3.5a does with them
     is to check (they may come from a later client, through the HD files of `E:`).
  3. The models test the alpha of their opaque batches against 1/255, as the buildings did: a
     model whose texture's alpha is a mask may show holes too; to check on the doodads.
  4. The shaders specular (1) and metal (2) drawn as diffuse, their highlight left out: it needs
     the light of the map, step 9.7.
  5. Environment (3) and environment metal (5): the second texture, mapped on the environment as
     the models map it, added by the alpha of the first; the exact weights of 3.3.5a to check.
  6. Two layers (6): the second texture under the first by the alpha of the second colours; the
     order to check on one of the 31 groups that have them.
  7. The alpha key at 224/255, as the models test it; the client's for the buildings to check.
  8. The faces culled as those of the models; the diffuse colour, the glow at night (`sidn`) and
     the flag of a window not used.
  9. The budget of the view: the buildings tell what they hold, a fixed cost; they do not limit
     their loads by what the budget allows (possibility: by bands of distance, as the terrain).
  10. The device: drawn only where the models draw by their pool (the first instance of an
      indirect draw, storage buffers read by vertices, 64 textures a stage); no path for the
      others.
  11. Their own arrays of textures: a texture both a model and a building use is held twice
      (possibility: arrays shared in `core/api`).
  12. The vertices 40 bytes, the indices 32 bits (16 would halve them, a base vertex a group).
  13. The doodads of the sets placed whatever group is seen (the portals of 9.6d2 to show those of
      the groups seen only); their colour (`MODD`) not used, with the lights (9.7).
  14. Where the terrain has holes under a building and a group of it is not drawn, the clear colour
      of the view shows, black, below the horizon of the sky.

#### Step 9.6d1, after its review

The buildings seen from outside validated: the fix of the vertex colours as `FixColorVertexAlpha`,
the planes of the view in reverse Z. Four points before 9.6d2:

1. **The private memory** (+0.5 to 4.5 GB for 0.1 GB of working set): the report of the allocator
   of the device (`Device::generate_allocator_report`) in the statistics, and whether the memory
   falls after 30 s without loads. An arena writes its range by `Queue::write_buffer` under its
   lock instead of a buffer of its own copied (two allocations and two copies a range); the
   textures as sparingly.
2. **The budget**: the buildings told by bands of distance, as the terrain, their files loaded
   within what it allows, and read as many at once as the workers but one.
3. **The interface thread**: the remedy of 9.6b built, the bounds of each owner of `models` tested
   before its groups.
4. **The transition**: the fix already darkens its colours by `1 − a`, so that blending inside and
   outside by `a` darkens the inside twice; probably the inside plus the outside times `a`, to
   decide by captures of the game.

Answered: the captures of the game allowed by the user (`jeu.ps1`), in the same places (the inn of
Goldshire inside and outside, a door, Stormwind, Dalaran); a building lit as one (0x2, the
unified render path of 3.3.5a) kept lit as outside, its ambient colour added at the drawing; the
opaque batches of the models whose textures have texels of alpha 0 probed, and given the rule of
the buildings if they show holes; no path without the pool, said in the panel. Later: the
specular and metal shaders and the glow at night (9.7), the doodads of the groups seen (9.6d2),
the arrays of textures shared and 16-bit indices, the holes of the terrain under a building.

#### Step 9.6d1, its review points as built

- **The private memory.** The statistics give the report of the allocator of the device
  (`Device::generate_allocator_report`, made again once a second): its bytes allocated, those
  reserved and its blocks. An arena writes a range by `Queue::write_buffer` under the lock of its
  holes, without a buffer of its own; a texture is written into a buffer mapped where the GPU reads
  it from (`MAP_WRITE`), without a buffer of wgpu's own copied into it. Measured in Dalaran at 4
  tiles with the script of 9.4d: the private memory 7.0 to 7.6 GB against 6.3 to 8.2 GB before,
  within its noise; the allocator 2.1 to 2.2 GB allocated of 5.6 to 6.2 GB reserved, in 24 to 26
  blocks; the process 1.2 to 1.3 GB in memory. The private memory is the memory in use plus what
  the allocator reserves (1.2 + 6.2 against 7.5 GB): the surplus is the blocks of the allocator,
  which the system commits, not buffers the driver keeps; two thirds of what they reserve is not
  allocated. Held still 31 s without loads, nothing falls: 7,532 then 7,533 MB private, 6,208 MB
  reserved both times.
- **The budget.** Each file of a building told to the budget by the distance of the nearest of its
  buildings, held or wanted in that band, as the terrain tells its tiles; a file not yet read
  wanted at what those on the GPU take on average (4 MB while none is). A file waits until the
  budget lets it load; the nearest read first, as many at once as the workers but one, those
  reading counted; a file beyond the reach the budget lets keep let go, waiting again. The panel
  counts the files waiting for the budget or their turn.
- **The interface thread.** The bounds of each owner of `models`, the box of its groups drawn
  grown by their largest radius at their scales, kept until its publication or the looks change,
  tested before its groups; an owner out of reach or out of sight not given to the frame, neither
  its instances nor its bones copied. Measured in Dalaran at 4 tiles before that last change:
  `models` 1.81 to 1.92 ms on the interface thread against 1.79 to 1.97 ms, nothing gained there.
  The city of Dalaran is one building (`ND_Dalaran.wmo`: 91 groups, 4,633 doodads in its set 0),
  one owner whose bounds hold the camera; the steering of `models` (1.13 to 1.16 ms) still walks
  every group to tell the budget. The doodads of the groups seen (9.6d2) are the remedy there.
- **Found by the mutations**: where the device has no count of indirect draws (the software
  adapter of the tests), a frame where the GPU chose nothing drew the draws of the frame before;
  hidden while every owner's instances were copied anew, shown once an owner out of sight was not.
  Nothing is drawn then.
- **The transition**: lit by the inside plus the outside times the alpha of its vertex colours, as
  the review proposed; its test checks that the colour the fix left is kept whole under the light
  outside added. To compare with the client by its captures.
- **A building lit as one (0x2)**: lit as outside, its ambient colour added.
- **The opaque batches of the models**: of the 13,624 opaque batches with a texture of the models
  under `World\`, 1,288 have a texture with more than 5 % of its texels of alpha 0; the models
  already take the first texture of an opaque batch as opaque whatever its combiner (a test checks
  it), and the stable of Duskwood shows no hole: no change.
- **The device**: the panel says there is no other way to draw the buildings where the pool is
  not offered.
- **Seen on the user's machine**: the inn of Goldshire, its door, walls, roof and chimney; a gate of
  wood; the stable of Duskwood; Dalaran.
- **Tested** by 25 changes made on purpose: 23 made a test fail; one, the files let go put back to
  waiting by the steering, is two lines of the module not tested apart; one changed code since
  taken out (the bones of an owner skipped passed over, now never given to the frame).
- **The captures of the game** (`jeu.ps1`, the user's client connected, its character put back
  where it stood after), at the same places as the view of the editor: the inn of Goldshire from
  outside, its door from the porch and from inside, its hall; the trade district of Stormwind;
  Dalaran, in a hall of the city and at Krasus' Landing. Two differences:
  1. The light of the view: the editor's is still the light set before the lights of the maps (a
     sun of 0.55, an ambient light of 0.45, a grey sky), the game's that of its hour (11 h, a blue
     sky): what is outside is about twice as dark in the editor. To compare again once the lights
     of the maps are drawn (9.7).
  2. The insides, much darker in the editor at the same places, which that light does not touch:
     the floor of the hall of the inn 4 of 255 against 43 in the game, a wall 28 against 92; the
     hall of Dalaran brown where the game shows it violet. They are lit here by their vertex colours
     as the fix leaves them, doubled, and the ambient colour of the building, which gives about the
     colours as read (0.43 for the walls of the hall of the inn): the client lights them more.
  The transition and the buildings lit as one cannot be told apart through those two differences:
  kept as built.
- **Questions:**
  1. The blocks the allocator reserves and does not allocate (about 4 GB in Dalaran at 4 tiles):
     to measure first, from the same report, the size and the allocations of each block, to know
     what holds them; then either left as they are, or fewer and longer-lived allocations (the
     arrays of textures and the arenas grown by larger steps).
  2. The steering of `models` walks every group at each frame to tell the budget (1.1 ms in
     Dalaran): the distance of an owner far beyond the reach to keep taken for all its groups, or
     walked again only when the camera moved by a margin.
  3. The light of the insides: the vertex colours not halved back (twice as bright), the light of
     the map mixed in as later clients do it, or the lights of the building (`MOLT`, 10 in the inn);
     to decide with the lights of 9.7, the light of the view then that of the game's hour.

#### Step 9.6d2, as built: the buildings seen from inside

- **The BSP trees read** (`formats`): each group's tree (`MOBN`, `BspNode`: its axis and whether a
  leaf, its two children, the triangles of a leaf, where its plane cuts its axis) and the triangles
  its leaves hold (`MOBR`); a tree that does not hold together (a child before its parent or past
  the nodes, a leaf past the triangles of the leaves, a triangle past those of the group) left out
  and said. The 1,986 buildings of the client read with their trees whole.
- **Checked over the client** before building on it, by probes not kept: the plane of a portal is
  `n·p + d = 0` (over the 30,439 vertices of the portals, `|n·p + d|` is 0.0000 on average; one
  portal has a plane that is not finite, never passed); the side of a portal reference is that of
  the group listing it; every group is outside (0x8) or inside (0x2000), never both nor neither;
  62 buildings have groups no portal joins to a group outside (909 groups, 12 of Stormwind), 32 have
  no group outside, most of them dungeons. Seen from outside, the groups of a building are tested
  by their bounds, as in 9.6d1, so that a dungeon looked at from outside shows.
- **The group the camera is in** (`buildings/src/cells.rs`): among the groups inside whose bounds
  hold it, those with a triangle of their tree under it facing up (by the normals of its vertices),
  the one whose floor is the nearest under it; the tree walked along the line down from the camera:
  on the side of the camera of a plane across X or Y, both sides where it lies on it, under a plane
  across Z, and over it too where the camera is. As the public implementation WebWowViewerCpp,
  read for its facts, finds it, but without its test of the side of each of the group's portals:
  the streets of Stormwind are groups inside, open to the sky and concave, which the planes of four
  of their portals cross where the camera stands.
- **The portals**: from that group, each portal it lists passed when the camera is on the side of
  the group (within 0.5 yards of its plane, passed unclipped), its polygon clipped by what the
  camera sees through the portals before it; past it, the camera sees between the planes through
  itself and the sides of the polygon, and past the portal's own plane. Eight portals at most one
  after the other, never back through one already passed. A group outside reached: every group
  outside is seen, and the groups inside their portals as the camera sees them. In the layer, the
  groups of a placement whose bounds hold the camera are first tested this way, in the axes of the
  building: the eye moved by the inverse of its transform, the planes of the view by its transpose.
- **The doodads of the groups seen**: an owner of `models` for the doodads the same groups hold
  (`buildings/<map>/<unique id>/<groups>`, `-` for those no group holds), placed again in other
  owners taking those before away. The service gives each owner a flag
  (`Models::shown(owner)`, true until set), which the layer of `models` reads before giving the
  frame an owner; the layer of the buildings sets it at each frame: shown where a group holding
  the doodads is seen, and every doodad of a building not seen from inside. The layer of `models`
  is prepared before that of the buildings: the flags it reads are those of the frame before.
- **Compared with the game** (the client of the user, its character put back where it stood after):
  in the trade district of Stormwind, a spire of the cathedral that the editor drew over the
  rampart without the portals is not drawn with them, and the game does not show it either.
  Seen in the editor with and without the portals, the same picture but for that spire and the
  creatures walking: the inn of Goldshire (its hall; its door from inside, the street through it),
  the trade district of Stormwind, Ironforge, a hall of Dalaran.
- **Measured** on the user's machine, each place held still, the same build with the portals and
  without (the camera never taken as inside), the game of the user running:

  | Place | Groups seen through the portals | Buildings: groups, batches, triangles | `models`: instances, in groups | View, interface (ms) |
  |---|---|---|---|---|
  | Stormwind, trade district | 7 | 31 against 120, 307 against 1,204, 0.08 M against 0.34 M | 708 in 152 groups against 1,302 in 289 | 0.67 to 0.68 against 0.84 |
  | Ironforge | 4 | 94 against 138, 808 against 1,369, 0.13 M against 0.28 M | 708 in 67 against 837 in 145 | 0.63 to 0.64 against 0.74 |
  | Dalaran, a hall | 35 | 161 against 187, 2,000 against 2,403, 0.78 M against 0.89 M | 1,017 in 344 against 1,398 in 511 | 1.00 against 1.09 to 1.10 |

  `models` on the interface thread 0.94 to 0.96 ms against 1.05 to 1.08 in Stormwind, 0.91 to
  0.93 against 0.96 in Ironforge, 1.73 to 1.75 against 1.79 to 1.81 in Dalaran: its steering, which
  walks every group to tell the budget, unchanged. The layer of the buildings 0.12 to 0.22 ms. The
  cells kept on the CPU, the trees with the vertices, normals and triangles of the groups inside,
  take 28 to 46 MB for the buildings held, against 1.8 MB before.
- **Tested** by 36 changes made on purpose to the reading of the trees, the cells, the layer, the
  doodads and `models`: 33 made a test fail, two of them once a test was added (a point beside a
  triangle taken in it; the rooms past a door aside drawn); one changes the work only, not what is
  seen (passing back through the portal come by, which the limit of eight bounds); two are the
  lines of the module handing the flags to its layer, not tested apart, their working shown by the
  measure (fewer instances drawn by `models` with the portals).
- **Questions:**
  1. The group of the camera without the test of the portals' sides: the open streets of Stormwind
     pass it; to compare with the client in more places, or that test kept for the groups that
     are not open to the sky.
  2. The doodads of a group a frame late, the layer of `models` prepared before that of the
     buildings (not seen at 60 frames a second): an order of the layers within the scene, or kept.
  3. The memory of the cells (28 to 46 MB): the vertices and triangles only of the leaves' triangles,
     or read again from the arenas; or kept.
  4. Seen from outside, a building's groups are drawn by their bounds; the client draws its insides
     through the portals of its groups outside. The same picture, the walls hiding them; the cost
     only. Kept for the dungeons seen from outside.
  5. The insides still dark (question 3 of the review points of 9.6d1).

#### Step 9.6d2, after its review

The points of 9.6d1 validated, and 9.6d2: the recursion through the portals and their clipping, the
planes moved into the axes of the building by the transpose, `Models::shown` read again after each
place. Two points before 9.6e: the camera over a roof taken for inside a room, and the insides too
dark, the chain of the colour of a vertex of the floor of the inn traced to the pixel. Minor: the
owners present kept in a set. Answered: the blocks of the allocator detailed, those visible by the
CPU told apart; the groups of `models` walked again only when the camera moved by a margin or the
owners changed; the doodads a frame late, the memory of the cells and the insides drawn by their
bounds from outside kept.

#### Step 9.6d2, its review points as built

- **The insides too dark: an error found in the chain, after the pixel.** For a vertex of the floor
  of the hall of the inn of Goldshire, under the camera of the captures (group 5, flags 0x2805, its
  vertex colours set, 0x4): the colours read, red first, (60, 47, 34), (57, 44, 28), (86, 68, 46),
  their alpha 0; at the camera (79.0, 62.2, 41.9); the ambient colour of `MOHD` (19, 16, 14); fixed
  (30.4, 23.1, 13.9); the light inside, in gamma `c·2 + ambient` (0.313, 0.244, 0.164), linear
  (0.080, 0.049, 0.023); the texel of its texture (101, 79, 51): the pixel expected (26, 12, 3), the
  pixel stored by the frame (22, 8, 1), as expected. Shown (2, 1, 0): egui read the texture of the
  view through its view of sRGB, decoded to linear when sampled, then took the value for one in
  gamma and decoded it again, which darkened the whole view, terrain, sky and models as the
  buildings. The view now gives egui a second view of the same texture, `Rgba8Unorm`, which reads
  its bytes as stored (`resolve_target`); a test writes a grey of 0.5 through each view and reads
  the byte back, 188 through the view of the pass, 128 through the one egui shows. The other texture
  egui shows, the scene of the kernel, was already `Rgba8Unorm`. Compared again with the captures
  of the game at the same places: a plaster panel by the door 86, 72, 53 against 92, 74, 53; an
  outer wall 76, 54, 35 against 71, 53, 37; the floor by the door 34, 20, 5 against 43, 30, 14. The
  outside now as the game shows it but for the sky; the insides about 0.8 times as bright and less
  blue: left to the lights of 9.7. Every view of the editor is brighter than when its steps were
  validated: they were seen through that darkening.
- **The camera over a roof.** Flown low over the inn and the roofs of Dalaran, the fault was worse
  than holes: in the attic of the inn and in a hall of Dalaran, groups outside by their flags
  (0x8, `upstairs` and `interior_petshop`), the camera was taken for inside a room under it whose
  bounds hold it, its floor 16 to 19 yards lower and nothing of it over the camera; drawn through
  that room's portals, the building disappeared. The group the camera is in now needs, besides a
  triangle of its tree under the camera facing up, one over it facing down, a ceiling; or, for a
  group lit as outside (0x40), inside but open to the sky, its floor within 8 yards under the
  camera. Of the 350 groups lit as outside, in 37 buildings, are the streets of Stormwind and the
  Valley of Honor of Orgrimmar; no room of the inn or of Dalaran is. The 8 yards keep the camera
  among the houses of a street, whose portals reach 15 to 19 yards over it: over Stormwind's roofs,
  at 18 and 31 yards over the street, the camera was still inside before and the buildings past
  the portals, a dome of the mage quarter and ramparts, were missing; it is outside now. The test of
  the sides of the portals of the public implementation (WebWowViewerCpp, read for its facts)
  rejects only a camera on a portal's plane, within 0.01 yards and its box: it answers neither case.
- **Seen again**, each view with the portals and without, in the editor: the attic of the inn three
  times and Dalaran three times, outside now, the same picture but for the creatures walking; the
  hall of the inn, its door from inside, the street of Stormwind, Ironforge and a hall of Dalaran
  still inside (2, 4, 7, 4 and 35 groups seen through their portals), the second spire of
  Stormwind's cathedral hidden in the street as the game hides it.
- **The blocks of the allocator**, in Dalaran with the terrain at 3 tiles and the buildings at 4: 23
  blocks, 5,440 MB reserved, 1,956 MB allocated. The report gives no memory type; told apart by the
  allocations they hold, the memory of a block being of one type: visible by the CPU, the block of
  the staging of wgpu (`(wgpu internal) Staging`, 128 MB, empty then) and one of the readbacks
  (statistics and timestamps, 64 MB), 192 MB; in the memory of the device, the 21 others, 5,248 MB,
  of which 11 of 256 MB use 0 to 12 MB each, held by small allocations that live long: the buffers
  of instances of the owners of `models` (478 of them, one each), the frame's buffers of the
  buildings, small arrays of textures. The private memory, 6,682 MB, is about what the process
  holds in memory and all the blocks reserved (1,129 + 5,440): on this machine (Vulkan, NVIDIA,
  Windows) the blocks of the device count in it, not only those visible by the CPU.
- **The walk of `models`**: the distance of the nearest group of each look, walked whole only when
  the camera moved past a sixteenth of a band of the budget (8.3 yards), the looks held were
  published, an owner came or went, or an owner that did not move published again; the owners that
  move, seen to publish again at the last whole walk (the creatures, which publish at each frame),
  walked again at each frame over the distances of the others kept. In Dalaran, the steering of
  `models` on the interface thread 0.91 to 0.93 ms held still against 1.13, 0.93 to 0.95 in flight
  against 1.22 to 1.24. Of what remains, by a timing not kept: what the models tell the budget,
  0.53 ms, and their summary, 0.13 ms.
- **Minor**: the owners present kept in a set, the bounds of those gone let go in a time linear in
  their number.
- **Tested** by 13 changes made on purpose: all made a test fail, two of them once two cases were
  added to the test of the walk (an owner that did not move gone; a creature farther than at the
  whole walk).
- **Questions:**
  1. The 8 yards over the floor of a street: from the portals of Stormwind's streets; to keep, or
     taken from the portals of each street (their lowest top over its floor).
  2. The insides 0.8 times as bright as the game's and less blue: with the lights of 9.7.
  3. The demand of the models told to the budget at each frame (0.53 ms): made again only when the
     distances are walked whole or the looks held change.
  4. The blocks of the device held by small buffers: the buffers of instances of the owners in an
     arena, as the vertices are, so that the blocks emptied are given back; to measure first.

#### Step 9.6e, proposed: the water

Asked by the review of 9.6d2, on the plan of 9.6 (the liquids of the tiles and of the buildings,
meshes by layer and by group, drawn blended in the second phase, their textures turning through
their frames; the oceans to the edges of a map) and the rule its review added (the water of the
ground, drawn before all the blended of the scene, whether it writes the depth decided and tested).

**Checked before proposing**, by probes not kept, over the client of `E:`:

- **The water of the tiles** (`MH2O`): in 529 tiles of Azeroth, 746 of Kalimdor, 1,022 of
  Northrend, two layers a chunk at most. Most of it is ocean (`LiquidType` 2): 86,222 layers of
  92,219 in Azeroth, 124,647 of 129,611 in Kalimdor, 222,034 of 226,825 in Northrend, of the
  vertex format 2 (depths only), and most layers have no vertices at all (70,528, 94,944 and
  201,204 layers), flat at their least height. Then slow water (5, format 0: heights and depths),
  slow magma (7, format 1: heights and coordinates), the lake of Wintergrasp (81), an orange slime
  (181) and water (1). 6,073 to 8,516 layers a map name the tiles they cover by a bitmap.
- **The older water** (`MCLQ`, in the chunks): all of Outland's (61,874 chunks, none of `MH2O`),
  and 505 chunks of Azeroth: a grid of 9 × 9 vertices, 8 × 8 tiles whose flags say where the
  liquid lies, its kind by the flags of the chunk (river 0x4, ocean 0x8, magma 0x10, slime 0x20).
- **`LiquidType.dbc`**: 26 types of 45 fields. Its field 3 is 1 for the waters and oceans of the
  world, 0 for three waters of instances and the orange slime, 2 for the magmas, 3 for the slimes;
  their material (`LiquidMaterial.dbc`: 3 records, a vertex format and flags each) 3 for the waters
  and oceans (format 0), 2 for magma and slime (format 1), 1 for the four others (format 0). The
  first texture of a water names a file of frames (`lake_a.%d`), that of an ocean a reflection map;
  the sixth of both the frames of the ocean (`ocean_h.%d`); magma `lava.%d` or one texture, slime
  `slime.%d`; 30 frames each. The others are those of the detailed procedural water of the client,
  a setting (`basicReflectionMap`, `basicWaterHeightTex_%d`, `proceduralRiverDepthTex`). The colours
  of the water are in none of these tables: the client takes them from the light of the map
  (`LightIntBand`: river and ocean, near and far; `LightFloatBand`: their alpha shallow and deep),
  which comes with 9.7.
- **The water of the buildings** (`MLIQ`, read in 9.6c): 406 groups; in 200, the flag 0x4 of the
  building makes the type of the group a `LiquidType` (1, 3, 4, 5, 7, 8, 14); in the 206 others,
  the group names 15 and the low nibble of its tiles gives the kind (0, 2, 3, 4, 6, 7), its
  `LiquidType` to be derived as the public description of the format gives it and checked against
  the client. 255 vertices a liquid at the median, 29,673 at most.

**In two parts, each reviewed before the next:**

- **9.6e1, the water of the tiles.**
  - `formats`: the liquids of a tile by a light call, as the placements of `doodads`: `MH2O`, or
    `MCLQ` in the chunks without it; each layer its `LiquidType`, the tiles it covers in its chunk,
    its heights (its least height where it has none), its depths and coordinates where its format
    has them. `LiquidType.dbc` and `LiquidMaterial.dbc` in the tables.
  - The module `liquids` (World, as this specification names it), steered as `doodads`: the tiles
    around the camera of the map of `terrain.map` within the distance of the terrain, read by jobs,
    the nearest first, let go once left; their meshes, by tile and by type, in an arena of
    `core/api`, told to the budget of the view by bands.
  - Its layer at a new stage between the ground and the scene (`Stage::Water`), so that the water is
    drawn after the sky of the terrain and before every blended batch of the scene, whatever the
    order the modules started in: in the blended phase, tested against the depth and not writing
    it, so that what lies under its surface, opaque, drawn before it, shows through it.
  - Its look before the lights of 9.7: the frames of its type's texture turning (at the rate of the
    client, to measure by captures of the game); water and ocean coloured near and far by the depth
    of each vertex, their alpha shallow and deep, by fixed values taken from captures of the game at
    noon, the light of the map to replace them in 9.7; magma and slime by their texture and their
    coordinates.
  - Measured over the coasts of Azeroth and of Northrend, Outland and a lake of magma: the frame,
    the GPU of the layer, the interface thread, the memory.
- **9.6e2, the water of the buildings**: the liquid of each group by its `LiquidType`, by the rule of
  the flag 0x4 and of the tiles; handed by `buildings` to `liquids` through a service, an owner a
  building, as the doodads are handed to `models`; shown only while its group is seen, through the
  portals from inside. Measured in Stormwind (its canals are of its building) and Dalaran.

**Not in 9.6e**: the reflections, the refraction, the detailed procedural water of the client, the
waves and flows, the particles and sounds of the liquids, the colours of the light of the map
(9.7), the editing of the liquids.

**To decide:**

1. The two parts (recommended), or one step.
2. `liquids` reading the liquids of its tiles itself by a light call (recommended, as `doodads`),
   or the terrain reading them and sharing them.
3. A stage of its own for the water between the ground and the scene (recommended), or the water
   drawn by the layer of the terrain after its sky.
4. The water not writing the depth (recommended: what lies under it shows; a blended batch under
   its surface is drawn over it), or writing it.
5. Magma and slime: blended as the water, or opaque in the opaque phase; to choose from captures of
   the game (a lake of magma of the Searing Gorge, the slime of Naxxramas).
6. The ocean past the last tiles of a map: none, the sky shows (recommended for 9.6e), or a plane at
   the level of the sea to the horizon of the view.
7. The liquids of the buildings handed to `liquids` (recommended), or drawn by the layer of the
   buildings in its blended phase, after the blended of the models it follows.

#### Step 9.6e, after its review

The two parts, `liquids` reading the liquids of its tiles itself, a stage of its own for the water,
and the liquids of the buildings handed to `liquids` (1, 2, 3 and 7, as recommended). Magma and
slime by captures of the game: opaque (5). Nothing past the last tiles of a map (6). Added to 4,
already in 9.6e1: the blended batches under the water seen through it. The water writes no depth;
the blended are drawn in the order sky, the scene blended beyond the surface, the water, the scene
blended on the eye's side, the order inverted when the camera is under the water; `liquids` offers
the height of the surface over a point and whether a point is under it; `models` places each blended
instance by its origin, `buildings` each group by its bounds; the API of the layers is given a phase
more; tested on the software adapter. With 9.6e, the memory (question 4 of 9.6d2): the buffers of
instances of the owners of `models` in an arena, and the hints of memory of the device
(`wgpu::MemoryHints`), measured apart then together; the budget counts what is allocated, not what
is reserved. The 8 yards over a street kept (question 1), the light of the insides with 9.7 (2), the
demand of `models` made again only when its distances are walked whole or its looks held change (3).
For 9.6e1: instances of `MH2O` of 24 bytes; the format of the vertices by the `LiquidType` through
`LiquidMaterial`; a layer without vertices flat at its least height; the bitmap of its tiles; a
small cache of the files read last in `assets`, optional.

#### Step 9.6e1, as built: the water of the tiles

- **Read** (`formats`, `assets/src/liquid.rs`): `Formats::liquids(directory, x, y)` reads the root
  of a tile only, as the placements are read: its `MH2O`, a header of 12 bytes for each of its 256
  chunks, then instances of 24 bytes (the `LiquidType`, the format of the vertices, the least and
  greatest heights, the rectangle of tiles, the offsets of the bitmap and of the vertices); the
  bitmap read from its low bit, row by row over the rectangle, every tile of it where none is given;
  a layer without vertices flat at its least height, its depth 255; heights held within the two.
  Then `MCLQ` in the chunks `MH2O` leaves without liquid: its offset in the header of the chunk
  (0x60), which points at the header of its part; a layer for each kind the flags of the chunk name
  (0x4 water, 0x8 ocean, 0x10 magma, 0x20 slime, the `LiquidType` 1 to 4), 804 bytes each, a tile
  dry by its flag 0x8; the depths of the water, the coordinates of magma and slime. `LiquidType.dbc`
  (26 records of 45 fields: id, name, kind, material, six textures, the two numbers of its
  animation) and `LiquidMaterial.dbc` (3 records) in the tables (`Formats::liquid_types`).
- **The format of the vertices: the layer's own, not its type's.** The review gave it by the
  `LiquidType` through `LiquidMaterial`. Over the client, 22,264 layers of ocean (type 2, whose
  material 3 has the format 0: heights and depths) hold the bytes of the format their own field
  names, 2 (depths only), and not those of 0: read by their material, their vertices would run past
  their data. The layer's field is read; the material's format is kept in the record of the type
  (`vertex_format`), read by nothing.
- **Over the client** (a test, when `UNIWOW_CLIENT` names it): 26 types; every tile of Azeroth,
  Kalimdor, Outland and Northrend read without a fault, their layers by type: Azeroth 1: 44, 2:
  86,683, 5: 5,874, 7: 123; Kalimdor 2: 124,647, 5: 4,960, 7: 4; Outland, all of `MCLQ`, 1: 6,946,
  2: 54,477, 3: 461; Northrend 1: 68, 2: 222,034, 5: 4,036, 81: 638, 181: 49. The axes: the corner
  of a chunk at X `ORIGIN - y·TILE - row·CHUNK`, Y `ORIGIN - x·TILE - column·CHUNK` of its tile
  `<x>_<y>`, the vertex of a row and a column an eighth of a chunk further down each: of the 975
  tiles of water whose ground lies under them, 765 have the ground under the surface so placed, 554
  with the axes swapped; the test asks three quarters at least, and a tenth more than swapped.
- **The phases** (`core/api`): `Phase::Opaque`, `Beyond`, `Water` and `Near`, drawn in that order
  (`Phase::ALL`), each but the first blended; `Stage::Water` between the ground and the scene. The
  view records a bundle for each of the four, and the statistics time each. The sky of the terrain
  is drawn at the start of `Beyond`, where the depth is still at infinity. The service `liquids`
  (`core/api/src/liquids.rs`): `Surfaces`, the height of the water over each of its tiles (an eighth
  of a chunk), the highest where layers overlap; `surface(x, y)`, `under(point)`, and `phase(eye,
  point)`: `Beyond` when one of the two lies under the water and the other not, `Near` otherwise.
  Magma and slime have no surface there.
- **The others' blended**: `buildings` lists each blended group of a placement by the centre of its
  bounds; `models` each blended instance chosen by the GPU by its origin (the regions of the frame,
  those beyond first), and each group of a look of its own (the path of 9.4c) by its centre. Their
  opaque batches unchanged.
- **The module `liquids`** (World), steered as `doodads`: the map and the distance of `terrain.map`
  (its answer now gives the distance), the WDT read by a job, the tiles within that distance of the
  camera read by jobs, the nearest first, as many at once as the workers but one, let go once left,
  told to the budget of the view by bands. A tile read: a mesh of 81 vertices a layer (its place,
  its coordinates, a quarter of a repeat a tile where it has none, its depth over 255, the slot of
  its type), two triangles a tile covered, the water apart from the magma and slime; the tiles of
  water at the mean height of their four corners, given to `Surfaces`. Vertices and indices in two
  arenas of `core/api`, the frames of the types in arrays of `core/api` (16 slots), a table of 64
  types for the shader (the codes of their frames, their count, water or not, the material, the
  animation). Its layer draws in the pass: the magma and slime in `Opaque`, writing the depth; the
  water in `Water`, blended, not writing it, both faces drawn. Its panel: the tiles read, with
  liquids, reading and refused, the tiles of water given.
- **Its look**, before the lights of 9.7: a frame every 60 ms (`ceil(ms / 60)` modulo the count, as
  Noggit, read for the facts only: 30 frames in 1.8 seconds); the procedural water (material 3)
  takes the frames of `XTextures\river\lake_a`, turning without moving; the water its coordinates
  scaled by the first number of its animation and turned by the second, in degrees, its texel added
  to a colour from shallow to deep by its depth, its alpha so; magma and slime their coordinates
  running by their animation, `ms / 2,880`, their texel brightened, opaque and unlit. Fogged as the
  terrain.
- **Its colours, by captures of the game at noon** (the pond of Goldshire, the Searing Gorge), in
  gamma: shallow (0.24, 0.35, 0.31), alpha 0.65; deep (0.18, 0.28, 0.27), alpha 0.9; the magma 1.35
  times its texture. The pond seen near, the editor 68 to 86, 85 to 90, 68 to 70 against the game's
  63 to 81, 79 to 92, 63 to 72; seen low, 78 to 81, 87 to 88, 69 against 51 to 53, 71 to 76, 66 to
  68: the game's water darker and more opaque at a grazing angle, which these values do not depend
  on. The magma 249, 52 at the median against 255, 43 to 62. The first values, (0.16, 0.25, 0.23,
  0.45) and (0.11, 0.18, 0.18, 0.85), showed the pond brown (67 to 91, 68 to 77, 45 to 50), the
  ground under it too visible, and the magma darker (185, 42).
- **Seen in the editor**, one launch a map: the pond of Goldshire near, low and from under its
  surface (what is over the water tinted through it); the magma of the Searing Gorge, running; the
  sea off Booty Bay; the lake of Zangarmarsh and the magma of Shadowmoon Valley, of `MCLQ`.
- **Measured** on the user's machine, the frame at 16.7 ms everywhere: the pond, 28 tiles, 0.19 M
  triangles of water, 25 MB, the GPU 0.05 ms (0.08 at most), the interface 0.03 ms; the Searing
  Gorge, 5 tiles, 5,364 triangles of water and 8,346 of magma, the GPU under 0.01 ms; the sea off
  Booty Bay, 35 tiles, 0.96 M triangles of water, 47 MB, the GPU 0.15 ms (0.16).
- **The memory**, in Dalaran held still, the same build, an environment variable choosing the
  configuration for this measure only (one capture each, 34 seconds after the start):

  | Configuration | Allocated | Reserved | Blocks | Private memory |
  |---|---|---|---|---|
  | A buffer an owner, `Performance` (before) | 1,957 MB | 5,120 MB | 22 | 6,344 MB |
  | The arena, `Performance` | 1,960 MB | 4,928 MB | 21 | 6,181 MB |
  | A buffer an owner, `Manual` 32 to 128 MB | 1,962 MB | 2,815 MB | 22 | 4,074 MB |
  | The arena, `Manual` 32 to 128 MB (kept) | 1,966 MB | 2,943 MB | 23 | 4,190 MB |
  | The arena, `MemoryUsage` (8 to 64 MB) | 1,957 MB | 2,401 MB | 30 | 3,651 MB |

  The arena alone gives back a block of 192 MB; the hints, 2.2 GB of what is reserved and as much of
  the private memory. Kept: the arena, and `Manual` from 32 to 128 MB (`kernel`), as the review
  proposed; `MemoryUsage` reserves 0.5 GB less in blocks of 8 to 64 MB, more blocks to allocate
  while tiles stream. The owners' instances (`models/src/groups.rs`): a range each in one arena,
  written in place while its groups keep their places, a new range written whole when they change,
  the range before given back once nothing published names it (the layer holds what a frame draws
  until the next); the arena read after what the owners published, so that it holds every range they
  name; `Arena::write` writes a range held again under the lock of the holes.
- **The demand of `models`** (question 3 of 9.6d2): made again and told to the budget only when the
  distances were walked whole, which a publication of the looks held also makes. In Dalaran held
  still, the same build otherwise: the steering of `models` 0.36 ms (0.42 at most) against 0.91
  (1.59), the module on the interface thread 1.05 ms against 1.66.
- **Tested**, besides the tests named above (the layers and tiles read from written files, a damaged
  one refused without a panic, the client's when given; the meshes and surfaces; the frames of a
  type; on the software adapter, the magma alone red and opaque, the water over it tinting it, a
  blended green beyond the surface drawn before the water and on the eye's side after it, from over
  the water and from under it, one behind the surface but before the magma seen, one behind the
  magma hidden; the buildings' group under the water drawn in `Beyond` only; the order of the
  blended of `models` with the pond, on both its paths; the arena's range written in place, a new
  one for new groups, the old given back once nothing names it; the hints of the device), by 29
  changes made on purpose: 21 made a test fail at once; the 8 others once the tests were completed
  (the ocean without vertices given two heights; slime no water; the painter drawn first in its
  phase, as a layer of another stage; one behind the magma; the buildings' phases drawn apart; the
  tests of the water and of the owners' places run on the path of 9.4c too) or run with the crates
  holding them (the order of the phases, caught by the test of the liquids).
- **Not done**: the cache of the files read last in `assets`, optional.
- **Questions:**
  1. The water seen low, brighter and less opaque than the game's, which darkens it at a grazing
     angle: with the alphas of the light of 9.7, or a term of the angle of view now.
  2. At sea, past the three tiles of liquids, the ground of the WDL under the sea shows as a dark
     band at the horizon (decision 6): kept until the ocean to the edges, or a plane at the level of
     the sea to the horizon.
  3. The magma 1.35 times its texture, and under the water no fog of its own: with the lights of
     9.7.
  4. The hints of memory: `Manual` from 32 to 128 MB kept; `MemoryUsage` reserves 0.5 GB less in
     Dalaran, in more blocks.

#### Step 9.6e1, after its review

Validated: the reading of `MH2O` and `MCLQ`, the format of the vertices by the layer, the phases
around the water and their inversion under it, the arena of the owners' instances and the hints of
memory. To fix before 9.6e2, the user seeing refusals in the game (water and buildings missing): the
device asking for the buffers the adapter takes; an arena growing up to that limit; a refusal for
want of room tried again, the demand told to the budget bounded by the room of the arenas; flat
layers by quads, and the water to the distance of the terrain by bands of the budget. Then the
surfaces made by tile in the job reading it, and a texture missing said once with how it is drawn.
Answered: the water darker at a grazing angle in the game is the reflection of the client's
procedural water, kept for later; the band at the horizon answered inside the map by the water to
the distance of the terrain, nothing past its edge (decision 6 kept); the gain of the magma and the
fog under the water with 9.7; `Manual` from 32 to 128 MB kept.

#### Step 9.6e1, its review points as built

- **The buffers of the device** (`kernel`): `max_buffer_size` and `max_storage_buffer_binding_size`
  asked for up to what the adapter takes, 2 GB at most (`LARGEST_BUFFER`), against the 256 and 128
  MB of wgpu's defaults. Of the arenas, those of the materials of `models` and of `buildings` are
  bound for storage; the vertices, the indices and the owners' instances are not. An arena takes for
  its limit the largest buffer of its use: for storage, the smaller of the two.
- **The arenas** (`core/api/src/arena.rs`): grown twice as large or to their limit, `min(max(2 ×
  capacity, needed, least), most)`, the hole that ends the arena counted in what is needed; refused
  only when that passes the limit, as `NoRoom` (the bytes needed and the most), told apart from the
  other refusals by `Refusal::{NoRoom, Failed}`. Each counts the ranges it gave back
  (`Arena::given`). `arena::room` gives how far from the eye what is wanted fits in arenas filled to
  a share of them, the nearest first, each by what it takes there or what is expected;
  `arena::room_made`, whether room may have been made since a refusal: a range given back, or the
  camera moved by a chunk (`MOVED`).
- **Refused for want of room, tried again**: a file of `buildings` (`FileState::NoRoom`) or a tile
  of `liquids` refused for want of room stays wanted and waits until room may have been made, then
  is read again, the nearest first. Both read only what is nearer than the room of their arenas
  filled to 90 %, and leave the rest out of the demand told to the budget; they let go what passes
  the room of their arenas whole (`budget::room` and `budget::plan` of `buildings`, `steps` of
  `liquids`, each tested apart).
- **Flat layers** (`liquids/src/mesh.rs`): a layer whose vertices are all at one height and one
  depth, its coordinates those by default, those without vertices among them, is drawn by a quad for
  each rectangle of the cells it covers, the runs of a row joined to the same run of the row before.
  The triangles of the water, measured over the client: off Booty Bay within 3 tiles 1,216,514
  before and 415,154 now, 2.9 times fewer; all of Azeroth 11,356,280 and 2,442,892, 4.6 times; all
  of Northrend 28,567,954 and 3,177,722, 9 times. Not the 60 times of the review: 70,744 of the
  92,724 layers of Azeroth are flat, but the 21,980 others, whose depths differ from a vertex to the
  next (the ocean of the format 2 along the coasts among them), keep their 9 × 9 vertices and now
  hold most of the triangles (question 1).
- **The water to the distance of the terrain**: the tiles of `liquids` within the distance of the
  terrain, read within the reach the budget lets load, told by bands, let go beyond the reach it
  lets keep.
- **The surfaces by tile** (`core/api/src/liquids.rs`): each tile of the map a grid of its 128 × 128
  cells, NaN where it has no water, or one height when the water covers it all at one (`Grid::Flat`,
  the open sea), made by the job reading the tile; `Surfaces` maps each tile to its grid, shared; a
  publication shares them; a height is found by the tile, then an index.
- **A texture missing** (`bearrug.blp`, named by a model, absent from the client): drawn white by
  the three layers (`NONE`, or white on the path of 9.4c); said once in the log, "left out, drawn
  white", by the arrays for the pool, and by `models` once for each message on the path of 9.4c.
- **Measured** on the user's machine, the terrain at 64 tiles, the buildings at 8, the models at a
  reach of 1,000, each flight at 150 yards a second; no refusal in the log, but the texture missing,
  said once:

  | Flight | Arenas (used) | Memory of the device | Private |
  |---|---|---|---|
  | Zul'Drak to the Storm Peaks | buildings: vertices 320 MB (147), indices 64 (45), materials under 1; liquids 112 and 64 MB (76 and 36); the pool of `models` 130 MB (48), the owners' instances 8 (3.8); 2,047 MB at most each | 3,674 MB allocated of 5,046 reserved, 36 blocks | 7,036 MB |
  | Off Booty Bay | liquids 56 and 32 MB (55 and 27) | 1,970 MB of 2,877, 24 blocks | 4,152 MB |
  | Off Booty Bay, the terrain at 3 tiles | buildings 160, 32 and under 1 MB (62, 18); liquids 14 and 8 (6 and 3); owners' instances 8 (3.7) | 1,626 MB of 1,853, 16 blocks | 3,120 MB |

  The arena of the vertices of `buildings` at 320 MB is the size refused before. The water:
  Northrend 1,022 tiles, 3.18 M triangles, 180 MB, the GPU 1.2 ms; at sea 544 tiles, 2.44 M
  triangles, 91 MB, the GPU 1.26 ms. The frame at 16.7 ms, the longest 17.3 ms, but 34.5 ms once at
  sea while the terrain streamed at 64 tiles.
- **The tiles of water in sight**, after the full verification of the milestones: each tile kept
  on the GPU has the box of its vertices, tested against the view before it is drawn, as the
  liquids the buildings place were; the statistics say how many of those held are drawn. The
  measure above, every tile held drawn, is to be made again where it was.
- **Publishing** the tiles of liquids: the longest 0.26 to 0.36 ms with 544 to 1,022 tiles held,
  while they arrive one by one; flying over the sea with the terrain at 3 tiles, 41 tiles held and
  read as the camera passes, 0.03 ms.
- **Tested** by 24 changes made on purpose, to the arenas, the limits of the device, the grids, the
  meshes of flat layers, the steering of `liquids`, the budget of `buildings` and the refusals said:
  all made a test fail. Added: an arena grown to its limit and refused past it, the hole that ends
  it counted, a range given back making room; the reach of what fits in arenas; when room may have
  been made; a tile and a file refused for want of room, put on the GPU once a range is given back
  (buffers of 32 KB); a tile read placing its water by its place in the world; the grids of the
  water; the quads of flat layers; the tiles read and let go by the budget and the room; a refusal
  said once.
- **Questions:**
  1. The layers whose depths differ now hold most of the triangles of the water: a coarser mesh for
     those of one height (the depth, and so the colour, less exact), or kept.
  2. At 64 tiles, the water costs the GPU 1.2 ms (a draw for each of 1,022 tiles): one draw for all
     of them, or kept.

#### Step 9.6e2, as built: the water of the buildings

- **Checked over the client** before building on it, by probes not kept: 406 groups have a liquid,
  255 vertices at the median, 29,673 at most. 200 are of buildings whose flag 0x4 names the types of
  `LiquidType.dbc`, their types 1, 3, 4, 5, 7, 8 and 14; the 206 others all name the type 15, the
  low nibbles of their cells 0, 2, 3, 4, 6 and 7. No group has cells of two basic kinds, none the
  flag 0x80000 of the ocean. The rows of a grid go up the Y of the building: of 340,938 vertices,
  325,968 lie within the bounds of their group so, 50,633 down Y.
- **The type of a liquid** (`buildings/src/liquid.rs`), as the client takes it, read for the facts
  in the public description of the format and in Noggit: of a building of the flag 0x4, the group's
  own type from 21, below it the basic kind of the type before; otherwise the basic kind of the
  group's type below 20, of each cell's for the type 15, the type after from 20. A basic kind, the
  two low bits, is the water of the buildings (13), their ocean (14) in a group of the flag 0x80000,
  their magma (19) or their slime (20). Those of the client come to 13, 19, 20 and, for one group,
  14; the water of the procedural material (`lake_a`), the magma `magma0`, the slime `slime.%d`.
- **The meshes**: each cell drawn (not of the flag 0x8) two triangles over its four vertices, a
  liquid for each type of a group's cells; the water its depth from the first byte of each vertex,
  its flow, over 255, and two repeats of its texture a chunk as the tiles; magma and slime their
  coordinates from their vertices, two numbers of 16 bits over 255. Made once by the job reading the
  file (`WmoFile::liquids`), in the axes of the building.
- **Handed to `liquids`** (`core/api/src/liquids.rs`): `Liquids::place(owner, liquids)` places
  liquids in the world, those of the owner before taken away, and gives a flag for each, drawn while
  true; `Liquids::clear(owner)` takes them away. `buildings` places those of each building drawn,
  moved by its placement, its owner `buildings/<map>/<unique id>`; it clears them once the building
  is let go or the map changes.
- **Shown** as the doodads: the layer of the buildings sets the flag of each liquid, shown when its
  group is seen through the portals from inside, all of them seen from outside or out of sight; the
  layer of `liquids` draws one while its flag is set and its bounds are in sight, the frame after,
  as `models` the doodads.
- **In `liquids`**: the liquids placed put on the GPU by jobs, their types and textures as the
  tiles', a refusal for want of room tried again once room may have been made; magma and slime
  opaque, the water in its phase. The surfaces of their water, the cells under the middle of each
  triangle and of its sides, made by the job for each owner; merged into those of the tiles when
  given, a tile of an owner's alone shared, one of the ground too made one, copied once. What they
  take on the GPU counted in the fixed part of the demand.
- **Seen in the editor**: the canals of Stormwind by the trade district and the mage quarter, and
  from over the city; the fountain of Dalaran.
- **Measured** on the user's machine, the buildings at 8 tiles: Stormwind, 50 liquids placed by the
  buildings, 2 or 3 drawn; Dalaran, 29 placed, 4 drawn. Giving the liquids held took 0.34 ms at the
  longest in Stormwind and 0.14 ms in Dalaran, against 6.06 and 1.01 ms when the first build added
  the cells of the owners one by one on the interface thread; the frame at 16.7 ms. Once, while the
  window of the editor was widened by the recette, the kernel said a validation error of wgpu
  (`Surface::configure`, the GPU not idle in time); not seen again in the next launches.
- **Not done**: the fog under the water and the light of the insides (9.7); the material the liquid
  of a group names, which Noggit does not read either.
- **Tested** by 19 changes made on purpose to the type, the meshes, the flags, the merging of the
  surfaces and the liquids placed: all made a test fail, one once a case was added (a type from 15
  to 19 of a building without the flag 0x4, of its basic kind). Added: the types of the table and of
  the cells; a group's liquid, a cell of water and one of magma, a cell not drawn; the flags of the
  liquids of the groups seen from inside, out of sight and from outside; the surfaces merged, a tile
  shared, two flat; a liquid placed meshed as given, its water under the surfaces; placed through
  the service, a flag each, taken away; put on the GPU and drawn while shown and in sight.
- **Questions:**
  1. The surfaces are one height a cell: the water of a building over a floor, a pool upstairs,
     makes what lies under it on the floor below beyond the surface for the order of the blended.
     Kept, or the water of each building kept apart and tested against its bounds.
  2. A building seen from outside draws its groups by their bounds, the walls hiding its insides;
     their liquids are shown with them, as the doodads. Seen from under Dalaran, its sewers show
     through the faces turned away. Kept.

#### The frames measured in flight, before any correction

Asked by the user's flights in Northrend at 30 to 40 frames a second, from the Howling Fjord to
Dalaran by the Grizzly Hills, the view drawn at each vertical blank of 60 Hz (a frame past 16.7 ms
waits for the next, 33 ms).

- **The journal of the frames** (`core/api/src/journal.rs`, `kernel/src/shell/frames.rs`): what each
  part spent on the interface thread in a frame (the kernel's steps; each module in all its calls,
  in `windows_ui` and in `on_job` apart; each layer preparing and recording, the submission, the
  report of the allocator), the bytes sent to the GPU from any thread (the arenas, the arrays of
  textures, the tiles of the terrain, the models of their own, the per-frame tables of `models` and
  `buildings`), the arenas grown and what they copied, the waits of the interface thread for the
  locks of the arenas and of `models`, `liquids` and `terrain`, what the GPU spent by layer on the
  frame timed last, and the jobs started and not yet handed back, waiting, running or ended. Kept
  cheap whether written or not; written a line a frame where `UNIWOW_SLOW_FRAMES` names a file, the
  frames past 17.5 ms marked slow. The parts nest: the kernel's, named `kernel: …`, follow one
  another, and those of the modules (each in all its calls, then its windows and its jobs apart)
  and of the views lie within them; eframe's runs from the window drawn to the next frame, its
  waiting for the vertical blank and any idleness included. The work of the interface thread is the
  kernel's parts summed, each once, which the line gives last since the full verification of the
  milestones; the frames of a window minimised are written too.
- **Flown by the user**, the terrain at 64 tiles, the buildings at 8, the doodads at 4, the models
  at a reach of 1,000, 300 then 600 yards a second:

  | Flight | Frames a second | Slow | The longest | Interface work, slow / others | Sent a slow frame | Arenas grown |
  |---|---|---|---|---|---|---|
  | Statistics shown | 46.4 | 33 % | 112 ms | 27.0 / 6.0 ms | 26.6 MB | 4, 89 MB |
  | Statistics hidden | 43.8 | 34 % | 133 ms | 28.9 / 6.5 ms | 31.6 MB | 4, 89 MB |
  | Terrain 16, buildings 3 | 28.0 | 53 % | 138 ms | 38.9 / 4.5 ms | 42.1 MB | none |

  The work of the interface thread, above and below, is the kernel's parts summed (eframe's aside):
  of the slow frames, 421 of 464, 381 of 433 and 296 of 325 took more than 16.7 ms of it; the GPU spent 3 to 4.4 ms on a frame. By part, a slow frame of the second
  flight: the jobs handed back to `models` 7.3 ms (46 at most), its windows 2.7, those of
  `buildings` 1.9, `liquids` 1.5 and `terrain` 1.5; recording the bundle of the terrain 3.9 ms,
  preparing `models` 3.1, `buildings` 1.1, submitting 1.1. The waits for a lock 0.1 ms a slow frame
  on average, 3 ms at the longest (10 in the third flight); the report of the allocator, made once a
  second while the statistics are shown, 2.9 ms at the longest, and the second flight, without it,
  as slow. Up to 298 jobs were started and not yet handed back at once, waiting, running or ended:
  94 on average in the slow frames of the second flight, 11 in the others.
- **The same flight scripted** (`viewport.look_at` 60 times a second along the same way): 60 frames
  a second, 4 to 5 % slow, 34 ms at the longest; the flights of the user loaded many more models at
  once.

#### Step 9.6f, proposed: the occlusion on the GPU

Asked by the user once the terrain, the doodads and the buildings of a whole map were drawn (a
branch without the distance of the view: 43 frames a second over Northrend, 49 over the Eastern
Kingdoms, 58 over Kalimdor, the whole map in sight). In three parts, each measured and reviewed:

1. **9.6f1, the two passes and the pyramid**: the pass of the view cut in two, the pyramid of the
   depth built between them, and a point of computing for the layers there; nothing culled yet, so
   that the cost of the cut alone is measured.
2. **9.6f2, the instances of `models`**: the choice by the GPU (9.4e3) made twice. Before the first
   pass, the instances in sight that were drawn at the frame before; between the passes, every
   instance in sight tested against the pyramid by its box, those found in sight and not drawn yet
   drawn in the second pass, and those found in sight kept as drawn for the next frame. The
   blended are drawn only when found in sight.
3. **9.6f3, the groups of `buildings`**: the groups the CPU lists by the portals and the view tested
   on the GPU the same way, in two phases by what was seen at the frame before.

Measured where the user flew, with and without, as 9.6f says: the four cities and the whole maps.

#### Step 9.6f1, as built: the two passes and the pyramid

- **The interface** (`core/api`): `Phase::Revealed`, between `Opaque` and the blended phases, for
  the opaque a layer finds in sight against the pyramid; `Phase::first_pass`, true for `Opaque`
  only. `viewport::Pyramid`: the view of every level, the size, the levels and a generation counting
  the pyramids made, so that a layer keeps its bind group while the view keeps its size.
  `Layer::occlude` and `Layer::occludes`, as `compute` and `computes`: an encoder of the layer's own,
  submitted between the passes, inside a validation error scope; a layer that panics or fails there
  is removed and its module reported.
- **The frame** (`viewport`), one submission: the computing of the layers; the first pass, the grid
  and the opaque phase, its colour and depth stored, not resolved, then the pyramid in the same
  encoder; what the layers compute against it; the second pass, loading both, then `Revealed`, the
  blended phases, and the resolve.
- **The pyramid** (`viewport/src/pyramid.rs`): an `R32Float` texture of the size of the view, its
  levels those of a texture, halved and rounded down to one texel; its first level the least of the
  four samples of each pixel, read as a `texture_depth_multisampled_2d` (the depth target is now
  `TEXTURE_BINDING` too); each next level the least of the 2 × 2 texels under each texel, the last
  of a row or a column reading on to the end of the level under it, three texels where that side
  is odd, so that every texel is covered. A compute pass, a dispatch a level, of 8 × 8 threads.
  Made again with the targets, when the view changes size.
- **Its time**: the GPU timer writes the beginning of the first pass and the end of the second, the
  pyramid apart, and each layer's computing against the pyramid with its computing; the
  statistics say *the pyramid of the depth N ms*.
- **The layers**: none occludes yet, and every one draws nothing in `Revealed`.
- **Tested**: the pyramid of a depth of 5 × 3, with four samples and with one, each sample of a
  pixel at another depth, read back level by level against the rule; a layer reading the last level
  between the passes reads the depth the first pass of the same frame left, and what the second
  draws is not in it; what a layer reveals nearer than the ground is drawn over it, what lies behind
  the ground hidden by the depth the first pass left; a layer panicking or failing on the GPU while
  testing against the depth is removed and reported; the timer, the pyramid apart. 10 changes made
  on purpose (the least of the samples, every sample, the least when reduced, the last texel of an
  odd side, the pyramid built before the first pass, the colour and the depth of the first pass
  kept, `Revealed` in the second pass, the layers asked to occlude, the pyramid timed) all made a
  test fail, two once the samples of a pixel were set at depths in no order.
- **To measure by the user**: the frames a second and the line *GPU* of the statistics, before and
  after this step, on the whole maps and in the cities.

#### Step 9.6f1, measured by the user

The pyramid of the depth takes 0.10 to 0.15 ms of the GPU a frame on the user's machine.

#### Step 9.6f2, as built: the instances of `models`

- **The choice by the GPU in two phases**, in the submission of the frame:
  - before the first pass (`Layer::compute`), `choose_first` chooses, among the instances of the
    groups in sight, those drawn at the frame before (their level then not 0), in sight and within
    the reach of their size, at their level; their opaque batches are drawn in `Phase::Opaque`;
  - between the passes (`Layer::occlude`), the work is cleared but its statistics, and
    `choose_second` tests every instance in sight against the pyramid: the eight corners of its box
    projected, its nearest depth (reverse Z: the greatest) against the least of the pyramid over
    the rectangle the box covers, read at the level where that rectangle spans two texels at most;
    a box reaching behind the eye is seen. Those found in sight that the first phase did not draw
    are drawn in `Phase::Revealed`; the templates of the blended are kept only for those found in
    sight; the levels written are those found in sight, which the next frame draws first.
  - An instance coming into sight is drawn in that frame, by the second phase; one hidden since
    the frame before is drawn one frame more, by the first.
- **The pipelines**: the choosing, the templates and the scattering of each phase apart
  (`choose_first` and `choose_second`, `tops_first` and `tops_second`, `scatter_first` and
  `scatter_second`), the others shared; the second's choosing reads the pyramid from a group of
  its own, whose bind group is made again with the generation of the pyramid. `Params` holds the
  view's matrix.
- **The statistics**: both phases summed, and the instances the depth hid, in the word of the
  statistics left unused before: *N hidden by the depth*.
- **Not tested against the depth**: the looks of their own, those the pool had no room for and
  all of them without the pool, drawn in the first pass as before.
- **Tested**: the bench of the module draws as the view does, its two passes around the layer's
  computing against a pyramid it makes of a wall at a depth by quarter of the view. An instance
  behind the wall is not drawn; drawn once the wall is farther; drawn by the first phase at the
  next frame though hidden, then no longer, its count of the hidden said; a blended one likewise,
  counted once; two looks, one seen at the frame before and one new, each drawn once at its place;
  the wall over the left, then the upper half of the view hides the instances there only. The
  tests of the packing read the draws of a frame where every instance is new. 16 changes made on
  purpose (every box seen, the farthest corner for the nearest, the axes of the view, the first
  phase drawing what was not seen, the second drawing or scattering again what the first drew, a
  hidden instance kept, the hidden not counted, the templates kept in the first phase, `Revealed`
  not drawn, the work not cleared or its statistics cleared, the pyramid of an earlier frame kept,
  the layer not testing, the statistics not read) all made a test fail, one once the blended were
  counted.
- **To measure by the user**: the frames a second over the whole maps and in the cities, and the
  count of the hidden in the statistics of the layer.

#### The frames measured while the camera moves, after 9.6f2

The user saw no gain from 9.6f2, and the frames falling while the camera moves, back up once it
stops, as before 9.6f. The journal of the frames, the whole map of the branch without the distance
of the view loaded, 112 seconds and 6,225 frames, flown and still at several places:

| Part, on the interface thread | Still | Moving, a crowded view |
|---|---|---|
| `view: models prepare` | 1.4 ms, up to 10 in a crowded view | 9 to 10 ms |
| `windows of models`: the distances of every look placed walked whole for the budget | 1 ms | 3.5 to 4.5 ms |
| `view: buildings prepare` | 0.2 ms, up to 2.4 | 2.2 to 2.4 ms |
| `view: submit` | 0.4 ms, up to 2.4 | 2 ms |
| The GPU, the frame timed last | 3.2 ms | 4 to 6 ms |

The GPU is not what the frames wait for, so that the occlusion could not show. The distances of
the looks were walked whole again once the camera moved by 8 yards, at each frame of a flight;
their milliseconds take the interface thread past 16.7 and the frames miss the vertical blank.
Decided by the user: the walk first, then `models prepare` measured and lightened, then the
buildings and the submission; 9.6f3 after them.

#### The walk of the distances off the interface thread, as built

- `Walking` in `models`: a whole walk of the distances is made by a job of the pool from what is
  placed then (the owners' publications, the radii of the looks held, the eye), one at a time;
  the frames meanwhile keep the distances walked before. Its outcome is kept once it comes back,
  and the next frame tells the budget, the loads and the releases following it. The owners that
  move, whose groups alone are walked again when nothing else changed, are still walked on the
  interface thread, which is quick.
- `Walked::whole` says, apart from `Walked::update`, whether a walk would be whole.
- **Tested**: a job a whole walk, not a frame; nothing until the first comes back; another job's
  outcome given back; the distances come back told to the budget once; the owners that move walked
  on the interface thread without a job; the camera past the margin keeping those before while one
  job runs; a walk that panicked followed by another. 9 changes made on purpose: 8 made a test
  fail; the ninth, the budget never told, is the line of the module's steering, which no test
  drives, the module having no harness of its own.

#### The window *Settings* and the distances of the view, as built

Asked by the user after the walk: the distances back, the branch drawing the whole map left, in a
window *Settings* with a category for each module, which the modules declare, since they may be
disabled; by default at half the most that shows everything at all times, from any point of a map.

- **The core**: `Registrar::settings(title, settings)` declares the module's category, the last
  declared kept; `SettingSpec::integer(key, label, [least, most], default)`, a setting of whole
  values, its key among the module's settings; `SettingSpec::value` reads the value stored, kept
  within its range, or the default where none is stored or it is not a whole number.
- **The kernel**: *Edit > Settings* opens the window: the categories of the running modules, by
  their titles, on the left, the one chosen on the right, its settings each a slider with its field
  and a button *Default*; a value chosen is written to the module's settings, which the module
  reads. A module stopped or failed shows no category.
- **The distances**: `formats::WHOLE_MAP`, 90 tiles, at which every tile of a map is taken from
  any point of it (from a corner, 63.5 tiles along the diagonal times the square root of 2, less the
  half tile of `tiles_around`, rounded up); `formats::distance_setting`, from 1 to 90, 45 by default,
  for the terrain (`view_distance`), the doodads and the buildings (`distance`). The models
  (`reach`, in radii): from 10 to 48,300, the diagonal of a map in yards rounded up to the hundred,
  an instance of a radius of 1 or less drawn from any point of a map to its farthest; 24,150 by
  default. Each module reads its setting at each frame; their panels no longer hold them, the GPU
  budget of the view staying in the terrain's. A value stored before is kept, *Default* bringing
  back the new one.
- **Tested**: the value of a setting within its range, its default; every tile of a map around any
  point of it at 90 and not at 89; the window: the categories of the running modules by their
  titles, none for a module declaring none, a value stored shown, *Default* and a value typed in
  the field written to the module's settings, another category chosen, a module failed showing
  none. Not tested: each module reading its setting at each frame, which no test drives, the
  modules having no harness of their own.
- **Not offered yet**: the categories of the compiled modules, of C++ and C#, and of Lua and
  Python.
- **Then, asked by the user**: the categories of several modules under one title shown as one, the
  settings of each module in the order of their ids; a module stopped takes its settings away, the
  category staying while another of its modules runs. The GPU budget of the view in the category
  *View* the module `viewport` declares (`gpu_budget_mb`, from 64 to 65,536 MB, half the memory of
  the GPU's own by default, 1,024 MB when not told), taken at each frame, the terrain's panel no
  longer holding it; a budget set through the service, as the terrain gives its own of before once,
  kept in the setting. `Registrar::gpu_memory`, given by the kernel, tells the modules the memory of
  the GPU when they register, for the defaults following it. Tested: the merge, its order, a module
  of the category failed; a module told the memory of the GPU; the budget declared, its default,
  taken from the setting at each frame, kept when set through the service. 8 changes made on
  purpose, all caught, one once the order of the titles differed from that of the ids.

#### The preparation of the models, measured then lightened

The journal of the frames, its parts of `models prepare` and of the steering of `models` apart, in
the crowded views the user flew (961 frames; the interface thread at 15.8 ms a frame on average):

| Part | ms |
|---|---|
| `models prepare` | 6.0 |
| of which every group (a look on a tile) tested against the view | 2.7 |
| of which the choice recorded (each owner's instances copied), the part left | about 1.5 |
| of which the camera, the owners and the tables | 0.7 |
| of which the frame of the choice written | 0.7 |
| of which the order of the blended | 0.4 |
| The steering of `models` | 2.2 |

Decided by the user, after the walk: the groups and the steering first.

- **The groups**: an owner in sight by its bounds gives the GPU every group of the pool it holds,
  untested here, the GPU testing each instance against the view, its reach and the pyramid
  already. What an owner's groups give is planned once (`layer::OwnerPlan`, `plan_of`) for its
  publication, the generation of the looks held and that of the tables, and made again once one
  of them changes: its pooled groups, among them those of looks with blended batches, still tested
  here before giving their instances to the order of the blended, and those of looks of their own,
  tested and drawn by group as before. The panel says the groups given.
- **The dispatches of a workgroup a group** go over rows of 65,535 workgroups, the most a side of a
  dispatch takes, the shader reading the width of a row from `num_workgroups`; a workgroup past the
  groups of the frame takes the last with no instance, reaching the barriers with the others.
- **The steering**: the loads, the releases and the loads cancelled are decided again only once
  what they follow changes (`Decision`: a whole walk come back, the looks held and their
  generation, the loads running, the reaches of the budget, the formats known), or, the owners
  that move walked again alone, at most every 100 ms. The owners that move are walked again only
  once one of them published again or the eye moved. The summary of the panel is made at most
  twice a second.
- **Reviewed** by another instance before being committed, as the user asked from then on: no
  serious point; two of middling weight corrected (the steering decided at every frame while an
  owner moved, the live world republishing at each frame; a change of the shader the test of the
  rows let through), and some light ones (a plan let go with its publication, the formats in what
  is decided from, the width of a row read by the shader). A second review, of those corrections:
  nothing serious or middling; light ones taken: the owners that move walked again from where the
  eye stands once a whole walk, made from where it stood, comes back; the owners published since
  taken once a frame rather than four times; comments set right.
- **Tested**: the plan of an owner, made again once the tables or the looks held change; more
  groups than a side of a dispatch, the last in sight chosen once; the walks of the owners that
  move counted only when walked, and walked again once a whole walk comes back; what the loads and
  the releases are decided again on. 23 changes made on purpose: 22 made a test fail, the last, a
  width of the rows other than the dispatch's, being the same as it in the test.

#### The instances read where they are in the arena

Asked by the user after the groups and the steering: the copies of each owner's instances into a
buffer of the frame, a command a copy at each frame (about 1.7 ms of the interface thread in the
crowded views), made no more.

- The choice and the shaders of the pool read each instance where its owner's are in the arena of
  the instances (`STORAGE` too now), the groups given and the blended instances by those places:
  nothing is copied, whatever the owners in sight. The levels of the frame before and the table of
  the bones are of the size of the arena, the tables of the bones of the animated owners copied
  where their instances are there; once the arena grows, the places kept, the levels before are
  carried over into the new buffers. A range given back is placed again only once the frames that
  drew it no longer hold its publication, its places read as 0 by then. The arena of the instances
  is now as large as a binding of storage takes at most (`max_storage_buffer_binding_size`, up to
  2 GB asked of the device by the kernel).
- The looks of their own read their instances from the arena as before, posed by the same table.
- **Tested**: an owner, then another whose instances make the arena grow, each drawn where it
  stands; a level kept across the growth of the arena; the tests of the groups, of the blended, of the bones and of the levels, as before. 8
  changes made on purpose: 6 made a test fail; of the 2 left, the arena kept from an earlier frame
  now fails, and the one saying a new buffer of the arena, the table of the bones growing with it,
  is the same as the code in every case the arena gives; the levels not carried over, added after
  the review, fails. Reviewed by another instance: nothing serious or middling; light points
  taken: the levels carried over once the arena grows, comments and this section set right.

#### Tests

The protocol of the observer against a fake server; the interpolation; the loading of tiles around
the camera, its order and the cancelling of what left the zone; rebuilding one terrain chunk from
the model; reading the formats, of 3.3.5a and modern, on small sample files that the tests write themselves (never files
taken from the client: they are Blizzard's and the repository is public); `parallel_for`, and a job
of another module started while every thread runs slices; the
archives read from many threads at once; the snapshot of the entities read from many threads while
the connection thread replaces it; a bundle kept while unchanged and recorded again once changed;
uploads while the view is hidden; the threads of a module ending when it fails; releasing beyond the
GPU budget and loading again. The tests on the real files of the client run when their folder is given by
an environment variable, and are skipped with a message otherwise: the CI does not have the client.

#### Acceptance

On the user's machine, with the client and the server on it:

| Check | Expected result |
|---|---|
| A map chosen, then flown over | Terrain, buildings, doodads and water right |
| Creatures and NPCs | At their place, animated, moving as on the server |
| A creature, a building or a terrain tile exported from the retail game and loaded by WarcraftXL; to prepare: files exported with wow.export, and `TextureFilePath.db2` and `ModelFilePath.db2` of DB2Gen installed in the client | Shown as the client shows it, animated for a creature |
| A player connected meanwhile with the real client | Seen moving |
| The server stopped, then started again | The view says so, then reconnects |
| Anything changed? | Nothing: no undo entry, no write in the database or the files |
| A left click in the view | Does nothing: it is kept for the tools to come |
| The interface during all of it | Fluid: flying fast over a city, no frame of the interface thread over 33 ms |
| The loading of a city (Task Manager, Jobs panel) | Every core works; the jobs of other modules still start |
| The editor minimised, or the 3D tab hidden, for a minute while tiles were loading | Memory stays flat; the view picks up where it was |
| The editor killed while it watched a zone | On the server, the invisible object is gone within 10 seconds |
| A dungeon, and a zone with phases | Its instance's entities only; entities of other phases told apart |
| Tests, `cargo xtask check`, CI | Green |

### Milestone 10: Lua modules (outline)

Lua modules in `modules\<id>\` (manifest and `main.lua`), loaded at start by `scripting-lua`, which
hosts them through a contract of the core open to the module of any language: their own Lua state
kept while the editor runs, commands, events, settings, the interface objects with Lua classes
(those of milestone 8 included), animatable properties, sequences and players, undo. Specified in
detail when milestone 9 is done.

### Milestone 11: Python (outline)

Python scripts, console and modules, with the behaviour of Lua: the host built apart and reaching
the editor through `uniwow.h`, the embeddable distribution in `interpreters\python-3.14\`, scripts
by tool in `scripts\python-3.14\` (the tool folder is a package), Stop even when a script catches
exceptions, the editor starting without Python, the interface objects with Python classes.
Specified in detail when milestone 10 is done.

Risks verified first:

| Risk | Result |
|---|---|
| PyO3 in the runtime | Impossible: `uniwow_api.dll` imports `python314.dll`, so the editor cannot start without Python (S8). Delayed loading is refused by the linker (LNK1194): the C API of Python exports data, such as `PyExc_ValueError` |
| PyO3 in the `scripting-python` module | Loads, and the editor starts without Python with that module refused. But the dependencies of PyO3 change, through Cargo's feature unification, the options of crates the runtime shares (`once_cell`, `syn`): the runtime is built differently and every Rust module must be rebuilt. It would also need an exception to the dependency rules of section 2 |
| The embeddable distribution beside the executable | The official Python 3.14.8 package (SHA-256 checked) runs from `interpreters\python-3.14\` with an isolated `sys.path`; the C extensions of the standard library load |
| C# module calling the C interface from several threads | A NativeAOT module (.NET 10) has four threads call the C interface at once; the group of each thread is one undo entry. `dotnet publish` needs the folder of `vswhere.exe` on the path, and `ProgramFiles(x86)` set |

Decisions: Python is embedded by a host built apart; Python 3.14, with its GIL; the .NET 10 SDK
builds the C# module.

### Milestone 12: acceptance of the extensibility (outline)

Three tools written only with the unified API, without touching the core:

- a SQL tool in Python;
- the equipment of a creature in Lua, which reads and writes the AzerothCore database (`libs/db`
  and a module of the database offering its commands);
- a timeline of particles in Lua: a module of particles, built in and written in Rust, draws them in
  the 3D view and offers their settings as animatable properties; the Lua tool has its own
  timeline (`Sequence`, `Player`, `DopesheetView`), which animates them. Drawing in 3D from the
  other languages stays out of this milestone: it is one of the other 3D accesses of step 8.3.

Specifying it needs the project model of section 10 decided first.

### Milestone 13: the Timeline in Montage mode (outline)

Sequences of tracks holding clips; a clip moved along its track or to another one, trimmed at
either end, cut in two at the playhead; edges snapping to the playhead, to the other clips and to
the frames; each change one undo entry. Built on the engine and widgets of milestone 8.

### Milestone 14: recording and copied keys in the Timeline (outline)

- **Recording**: while recording, changing a property by hand sets a key at the playhead.
- **Keys copied and pasted** in the dopesheet and in the Curves view.

---

## 10. Open questions

- Project model: what a project contains, where it is stored, how it maps to a WoW-mods module.
- Installations targeted: client with WXL, server, database connection. Milestone 9 settles part of it:
  the client's folder, and the observer on the same machine (address, port, token).
- The licence of UniWoW, none today. warcraft-rs and wow.export, whose code `assets` copies or
  translates, are MIT or Apache; WarcraftXL is GPL-3 and nothing of its code is copied.
- Order of the features after milestone 1.
