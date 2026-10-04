# UniWoW — Architecture and module catalogue

Status: **validated**. Milestones 1 to 7 built and validated; milestones 8 to 11 outlined. Open questions in section 10.

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
and Python receive theirs in milestones 9 and 10.

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
  catalogue of animatable properties, and only the values that changed.
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
  thread, where the module has no group open, and is an entry of its own, as the user's gesture. The author of a tool in any language has nothing to
  write for these; `record_change` stays for the module's own data. Rust modules using these
  objects hand them to the kernel with `Context::adopt_objects` and get the same.

### Capabilities and their unified form

What a built-in module can do, and how a module in any language does the same (R9). Every
milestone keeps this table up to date, and its review checks it. A dash is a gap: the step that
fills it, or *not planned* when no milestone does yet.

| Capability | Built-in (Rust) modules | Unified API (`uniwow.h`) | C++ and C# | Lua and Python |
|---|---|---|---|---|
| Named commands: offer, list, call | `Registrar::command`, `command_on_caller`; `Context::call`, `Editor::call` | `uniwow_module_info.commands`; `commands`, `call` | `uniwow::call`; `Editor.Call`, `Command` | Scripts: `uniwow.commands`, `uniwow.call`; modules: milestones 9 and 10 |
| Events | `Registrar::subscribe`, `Module::on_event`, `Context::publish_as`; `Editor::subscribe`, `next_event` | `publish`, `subscribe`, `next_event`, `unsubscribe` | The table (`uniwow::api()`, `Editor.Table`) | Scripts: `uniwow.publish`, `subscribe`, `next_event`, `unsubscribe` |
| Settings | `Context::setting`, `set_setting` | `setting`, `set_setting` | The table | Scripts: `uniwow.setting`, `set_setting` |
| Log | The `log` crate | `log` | `uniwow::log`; `Editor.Log` | Scripts: `uniwow.log`, `print` |
| Undo of the module's own data | `Command`, `Context::execute` | `record_change`, with `uniwow_module_info.apply_change` | `uniwow::recordChange`; `Editor.RecordChange` | Milestones 9 and 10 |
| Undo groups | The commands of one call | `begin_group`, `end_group` | The table; `Editor.BeginGroup`, `EndGroup` | Scripts: `uniwow.begin_group`, `end_group` |
| Panels | `Registrar::panel`, `Module::panel_ui` (egui) | `uniwow_module_info.panels`, `panel`, the objects | `uniwow::Panel`; `Panel` | Milestones 9 and 10 |
| Widgets, layouts, a scene, painting | egui | The objects of this section | The classes named as in Qt | Milestones 9 and 10 |
| Modal windows | The command `ui.dialog`; the object `Dialog` | `ui.dialog` through `call`; `Dialog` | `uniwow::Dialog`; `Dialog` | Scripts: `ui.dialog` through `uniwow.call` |
| The curve editor | The service `curve-editor` | `CurveView` | `uniwow::CurveView`; `CurveView` | Milestones 9 and 10 |
| Animatable properties: declare, list, read, write | `Registrar::animatable`; `Editor::properties`, `read_property`, `write_property` | `uniwow_module_info.properties` (`uniwow_property`); `properties`, `read_property`, `write_property`, `set_property` | `uniwow::Property`, `describeProperties`, `properties`, `readProperty`, `writeProperty`; `Editor.DeclareProperty`, `Properties`, `ReadProperty`, `WriteProperty`, `SetProperty` | — milestones 9 and 10 |
| The viewport's camera | Inside the module `viewport` (*View*, *Reset camera*) | The properties `viewport/camera_position`, `camera_target`, `camera_fov`; the commands `viewport.camera`, `viewport.look_at`, `viewport.frame` | The functions of properties; `call` | Scripts: the commands through `uniwow.call`; the properties: milestones 9 and 10 |
| Sequences and their playback | Inside the Timeline; `uniwow_api::sequence`, the objects `Sequence` and `Player` handed to the kernel with `Context::adopt_objects` | `Sequence`, `Player` | `uniwow::Sequence`, `uniwow::Player`; `Sequence`, `Player` | — milestones 9 and 10 |
| The dopesheet | Inside the Timeline's panel; the service `dopesheet` | `DopesheetView`; `CurveView` showing a `Sequence` | `uniwow::DopesheetView`; `DopesheetView` | — milestones 9 and 10 |
| Tree, table, property grid | egui | — step 8.7 | — step 8.7 | — milestones 9 and 10 |
| Drawing in the 3D view | The service `viewport` and its layers | — not planned (an other 3D access of step 8.3) | — | — |
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
| Jobs | Pool of worker threads, one per processor core: background jobs with progress and cancel (T2) |
| Log | Log panel shared by all modules. GPU errors captured by no module are logged instead of stopping the editor |
| Inspector host | Shows the selection with the inspector registered for its type |

The 3D view is not a core service: it is the `viewport` module (section 7).

Threads:

| Id | Rule |
|---|---|
| T1 | The interface thread draws, applies the undoable commands and owns the state of each module. It never waits for slow work. |
| T2 | The kernel keeps a pool of worker threads, one per processor core, for computations. `Context::spawn` runs a job there, with progress and cancel; its result comes back to the module on the interface thread. Work that waits, such as a script, runs with `Context::spawn_thread` on a thread of its own, so that waiting never holds a thread of the pool; it is otherwise a job like the others. A job of the pool never waits without a time limit, for instance in `next_event` without a timeout. A Jobs panel lists the jobs running. |
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
| Worker of the pool | Nothing | Jobs of `Context::spawn` (T2) |
| Thread of its own | Nothing | Scripts and jobs that wait (`Context::spawn_thread`) |
| Thread of a compiled module | The module's data, by its own rules | Its slots, paintings and `apply_change`, and its commands called from the interface thread |
| Any thread | — | Commands running on the caller (T4), the reading and writing of animatable properties, the C functions (T7) |

Shared state and locks:

| What | Lock | Taken by |
|---|---|---|
| Catalogue of commands, running modules, properties, settings | Read-write locks of the bridge | Any thread, briefly. Only the set of running modules is held while another is read (the catalogue or the properties, to list them); only the interface thread writes them |
| Events and failures from other threads | Lock of the bridge | Pushed by any thread, taken by the interface thread at each frame |
| Subscriptions | Lock of the bridge, then one queue per subscription | Delivered by the interface thread, read by the subscriber's thread |
| Interface objects of a compiled module | One lock per module | The interface thread while it draws them, any thread in the C functions. Never held while module code runs: slots, paintings and replies are called once it is released. In C++ and C#, the lock of the classes' connections is taken before it, never after |
| Queue of the pool | One lock | Released before a job runs |

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

---

## 6. Libraries (libs/*)

| Library | Role |
|---|---|
| formats | Read and write MPQ, DBC, ADT, WDT, WDL, WMO, M2, BLP. Based on warcraft-rs (MIT/Apache) where its writing is verified, own code otherwise |
| defs | DBC layouts for build 12340 (WoWDBDefs) |
| vfs | Client archive chain in the 3.3.5a load order, plus the project's own files on top |
| gpu | Generic GPU helpers on wgpu (device, shaders, buffers, camera math). Drawing of each kind of object belongs to the module that owns it |
| db | MySQL access to the AzerothCore databases |
| server-link | SOAP client, server process control |
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
| assets | Browse the virtual file system (archives and project), search, preview |
| textures | BLP view, PNG to BLP and back, format and size checks |
| models | M2 and WMO viewer (animations, textures, attachments), import of new models |
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

### Milestone 8: parity of the languages (specified)

Whatever a built-in module can do, a module in any language can do (R9). This milestone completes
the unified API where it falls short, and splits the Timeline into an engine, widgets and a window
using them, so that a module of another language builds a timeline of its own on its own
properties. Lua and Python reach all of it in milestones 9 and 10, through the same API.

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

**Step 8.7, data widgets**, as in Qt:

- `TreeView` (as `QTreeWidget`), drawn by the kernel: items with a text and children, folded or
  unfolded, one selected; signals `itemClicked`, `currentItemChanged`, `itemExpanded`.
- `TableView` (as `QTableWidget`), drawn by the kernel: columns with headers, rows of cells, cells
  edited in place, sorted by a column when the user clicks its header; signals `cellChanged`
  (row, column, text), `currentCellChanged` and `sortChanged`. Only the rows in sight are drawn, so
  that 100,000 rows scroll smoothly. Besides the rows given at once, functions change one cell,
  insert rows and remove rows, without giving the whole table again. The kernel sorts, keeping the
  id of each row, which the signals and these functions use, whatever the order shown. The SQL
  tool of milestone 11 relies on it.
- `PropertyGrid`, drawn by a new module of the interface, `modules/UI/properties/`, through a
  service: the properties of the catalogue whose paths it is given, each with its label and an
  editor for its kind (numbers dragged or typed, a colour, a box); a change by hand writes the
  property and is one undo entry, *Set <label>*, which the kernel records and which belongs to the
  property's module.
- The rows of a tree or a table are given as JSON (`ITEMS`, `ROWS`), each with an id the signals
  give back.
- Sample: a C# panel with a table of 100,000 rows, a tree, and a property grid of the cube.

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

### Milestone 9: Lua modules (outline)

Lua modules in `modules\<id>\` (manifest and `main.lua`), loaded at start by `scripting-lua`, which
hosts them through a contract of the core open to the module of any language: their own Lua state
kept while the editor runs, commands, events, settings, the interface objects with Lua classes
(those of milestone 8 included), animatable properties, sequences and players, undo. Specified in
detail when milestone 8 is done.

### Milestone 10: Python (outline)

Python scripts, console and modules, with the behaviour of Lua: the host built apart and reaching
the editor through `uniwow.h`, the embeddable distribution in `interpreters\python-3.14\`, scripts
by tool in `scripts\python-3.14\` (the tool folder is a package), Stop even when a script catches
exceptions, the editor starting without Python, the interface objects with Python classes.
Specified in detail when milestone 9 is done.

Risks verified first:

| Risk | Result |
|---|---|
| PyO3 in the runtime | Impossible: `uniwow_api.dll` imports `python314.dll`, so the editor cannot start without Python (S8). Delayed loading is refused by the linker (LNK1194): the C API of Python exports data, such as `PyExc_ValueError` |
| PyO3 in the `scripting-python` module | Loads, and the editor starts without Python with that module refused. But the dependencies of PyO3 change, through Cargo's feature unification, the options of crates the runtime shares (`once_cell`, `syn`): the runtime is built differently and every Rust module must be rebuilt. It would also need an exception to the dependency rules of section 2 |
| The embeddable distribution beside the executable | The official Python 3.14.8 package (SHA-256 checked) runs from `interpreters\python-3.14\` with an isolated `sys.path`; the C extensions of the standard library load |
| C# module calling the C interface from several threads | A NativeAOT module (.NET 10) has four threads call the C interface at once; the group of each thread is one undo entry. `dotnet publish` needs the folder of `vswhere.exe` on the path, and `ProgramFiles(x86)` set |

Decisions: Python is embedded by a host built apart; Python 3.14, with its GIL; the .NET 10 SDK
builds the C# module.

### Milestone 11: acceptance of the extensibility (outline)

Three tools written only with the unified API, without touching the core:

- a SQL tool in Python;
- the equipment of a creature in Lua, which reads and writes the AzerothCore database (`libs/db`
  and a module of the database offering its commands);
- a timeline of particles in Lua: a module of particles, built in and written in Rust, draws them in
  the 3D view and offers their settings as animatable properties; the Lua tool has its own
  timeline (`Sequence`, `Player`, `DopesheetView`), which animates them. Drawing in 3D from the
  other languages stays out of this milestone: it is one of the other 3D accesses of step 8.3.

Specifying it needs the project model of section 10 decided first.

### Milestone 12: the Timeline in Montage mode (outline)

Sequences of tracks holding clips; a clip moved along its track or to another one, trimmed at
either end, cut in two at the playhead; edges snapping to the playhead, to the other clips and to
the frames; each change one undo entry. Built on the engine and widgets of milestone 8.

### Milestone 13: recording and copied keys in the Timeline (outline)

- **Recording**: while recording, changing a property by hand sets a key at the playhead.
- **Keys copied and pasted** in the dopesheet and in the Curves view.

---

## 10. Open questions

- Project model: what a project contains, where it is stored, how it maps to a WoW-mods module.
- Installations targeted: client with WXL, server, database connection.
- Order of the features after milestone 1.
