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
kind needs (its DLL and the DLL's hash, the runtime fingerprint, its entry file).

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
| Settings page | brush defaults |
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
| F6 | Every action a module offers to others is a named command. The kernel keeps their catalogue and routes the calls; the same catalogue serves every module and script (S1). A Rust module declares its commands itself; those of compiled, Lua and Python modules are declared on their behalf, by the kernel or by the module of their language: they are delegated. Once every module has registered, a name declared twice keeps the command declared directly over a delegated one, and the first registered between two of the same kind; each one set aside is logged with the one that wins. |

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
within bounds), selectable and selected or not. Colours are `0xRRGGBBAA`, sizes are in points.

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
- **Painting**: a painting area asks its module to paint when it is shown, resized, or after
  `update()`. The module paints in points from the top left corner of the area, a text placed by its
  top left corner; the picture stays until the next painting.
- **Dialogs**: a dialog is a floating window, created hidden. While it is shown, the rest of the
  editor takes neither clicks nor shortcuts. The user closing it, with Escape or its close button,
  hides it and sends `rejected`.
- **Sequences and players** (milestone 8): a sequence holds tracks of keys on animatable
  properties, with a frame rate and a length, in the JSON of the Timeline's files. While a player
  plays, the kernel moves it on at each frame by the time elapsed, times its speed; at the end it
  stops and sends `finished`, or starts again with *loop*. Whenever its time or its sequence
  changes, the kernel writes the value of each track at that time into its property, through the
  catalogue of animatable properties, and only the values that changed. The dopesheet views and the
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
| Drawing in the 3D view | The service `viewport`: its layers (`prepare`, a version to keep their bundle, their statistics with `stats`) and its frame signal (`wait_frame`) | — not planned (an other 3D access of step 8.3) | — | — |
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

The four gaps *not planned* are for the review to place in a milestone.

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

1. Scans `modules\*\module.toml`. A folder without a manifest, or without what its kind needs, is
   ignored and listed.
2. Compares the **runtime fingerprint** of each Rust module with its own. The fingerprint
   identifies the compiler version and the runtime build. A mismatch refuses the module with the
   reason ("built for another runtime, rebuild it") instead of loading it. A compiled module is
   checked against the version of `uniwow.h` (S10). A Lua or Python module is handed to the module
   of its language; without it, it is refused with the reason.
3. Copies each accepted DLL to a temporary folder and loads the copy, so that a module can be
   rebuilt while the editor is open.
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
| Animatable properties | Registry of the properties modules let be animated: path, type, range, reading and writing; written without history during playback (milestone 6). A property whose range is not two numbers, the lowest first, is refused with the reason |
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
| Interface objects of a compiled module | One lock per module | The interface thread while it draws them, any thread in the C functions. Never held while module code runs: slots, paintings and replies are called once it is released. In C++ and C#, the lock of the classes' connections is taken before it, never after |
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
| S4 | Every change one run of a script makes forms a single undo entry. The kernel learns to group commands. A group belongs to one caller on one thread and can be nested; a command delegated to another module runs inside its caller's group, as a change made on a thread where its caller has no group open enters the group opened last on that thread. A group closes at its outermost end, when the job that opened it ends, when its module fails, or from the Edit menu. While an open group already holds a change, Undo and Redo are refused, greyed with the reason; a group that changed nothing yet, such as a script waiting for events, blocks nothing. Changes made by hand meanwhile enter the history on their own: when they touch what the script changes, their order relative to the group can be imprecise, and so is the undo order of two runs in parallel that change the same thing. Indirect changes are not grouped: a command triggered by an event a script publishes is applied when the event is delivered, outside the group. |
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
                        xtask build, test-sdk and check
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
  the interface thread, a call to an interface-thread command is answered at the end of the frame.
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
  A script run is grouped automatically.
- **Lua in the runtime**: mlua's generic code is instantiated in the feature using it, which then
  calls the Lua C functions directly. The runtime exports the 122 functions of the Lua 5.1 C API
  (`core/api/build.rs`), so that every feature uses the one Lua compiled into it. Runtime: 18,261
  exported symbols, 28% of the limit.
- **Modules**: loaded once from `modules\` beside the executable and never unloaded; a module's
  calls go through an `Editor` named `native-modules#<module>`. `cargo xtask build` compiles each
  folder of `modules-src\` (`cl /LD /MD /O2 /std:c++17 /W4 /WX`, linked with `/Brepro` so that an
  unchanged module gives the same file) and copies `scripts\` beside the executable.
- **Lua**: the safe subset of the standard libraries; `require` finds Lua files in
  `scripts\lua-5.1\`. `uniwow.next_event(subscription, timeout_ms)` waits without a time limit when
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
  (`disabled_features`, `features`, a tab's `feature`) are still read.
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
    Dragging a diamond of the summary row moves every key of its frame. The wheel zooms the time,
    the middle button scrolls it.
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
    project model exists (section 10). The panel lists them, creates one with a name, and saves the
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
- **Limits**: the playhead moves and the values are written only while the *Timeline* panel is
  shown.

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
  asked for while one is shown wait their turn.
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
  only, with Shift the values only), the middle button scrolls, F frames the selected keys, or all
  of them. It tells its caller that the curves changed, while a drag goes on and when it ends, so
  that the caller records one undo entry per change. It is offered:
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
  - `set_property` refuses a count that is not the kind's, and numbers that are not finite.
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
    units from its target. The angle of view goes from 1 to 170 degrees (45 at start). The target
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
    the values at the playhead follow the keys, as in the Timeline. Only the values that changed
    since the player last wrote are written. A track whose property no running module declares, or
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
  (row, column, text), `currentCellChanged` and `sortChanged`. Only the rows in sight are drawn, so
  that 100,000 rows scroll smoothly. Besides the rows given at once, functions change one cell,
  insert rows and remove rows, without giving the whole table again. The kernel sorts, keeping the
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
  in Qt. The triangle before an item folds or unfolds it, which the tree keeps.
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
| `set_cell`, `insert_rows`, `remove_rows` | The module's | Yes: a binary search and one move per row, one pass to remove |
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
| The rows made and their values read: those in sight and 32 around, the first 64 of a grid not drawn yet | Interface, before drawing | No: reading a property runs its module's code, which may lock objects |
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
| 9.4 | Still M2 models, of 3.3.5a and modern: from the display id to the model, its skin, its textures and its scale |
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
  The entry of the neutral locale comes first. A delete marker hides the file of the archives read
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
- Measured on the user's client (frFR since 5 October), release: the four tables read in 7 ms,
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
  shown, and opens the client when a folder is chosen; cancelled, it changes nothing. The picker
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
  waits, a count read without the lock. In the test, a job of another module started while every
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
  0, 1, 4 or 8 bits), DXT1, DXT3 and DXT5, kept as BC or decoded, BGRA. A level cut short ends the
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
    the last reading put back in the update list from their GUIDs (`Map::GetCreature`,
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
