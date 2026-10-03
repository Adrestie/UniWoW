# UniWoW — Architecture and feature catalogue

Status: **validated**. Milestones 1 to 3 built and validated. Open questions in section 10.

UniWoW is a standalone desktop application (outside the game client) used to modify a
WoW 3.3.5a (build 12340) client and an AzerothCore server: maps, data, assets, interface,
scripts, packaging. Written in Rust, user interface with egui, 3D rendering with wgpu.

Players are expected to run the client with WarcraftXL (WXL).

---

## 1. Modularity requirements

| Id | Requirement |
|---|---|
| R1 | Every feature is separate from the core. The core contains no code specific to any feature and never names one. |
| R2 | Every feature is a DLL. The core detects the feature DLLs present at start and loads them. |
| R3 | A feature can be added, rebuilt or removed without recompiling the editor. |
| R4 | Removing a feature's DLL leaves the editor starting and working without it. Example: without the viewport DLL, the editor starts without any 3D window; without the terrain DLL, the 3D window shows no terrain. |
| R5 | Adding a feature is simple: one command creates it, one command builds it. |
| R6 | A feature that fails, or that was built for another version of the core, is refused or disabled and reported; the editor keeps running. |
| R7 | Speed: the editor uses every processor core. Long or heavy work (reading archives, parsing files, building meshes, extraction, database queries) never runs on the interface thread. |
| R8 | Features, scripts and native modules may use threads themselves, through what the kernel offers (section 5, threads). |

---

## 2. Layers

```
app            small executable: loads the runtime, then the features
core/api       contracts offered to features (traits, extension points, entry point macro)
core/kernel    feature loader and core services (section 5)
libs/*         shared libraries without user interface (section 6)
features/*     one crate per feature, built as one DLL (section 7)
client-bridge  C++ WXL module running inside the client (outside the Cargo workspace)
xtask          developer commands: new-feature, build, build-feature, run, check
```

Crates are prefixed `uniwow-` (`uniwow-api`, `uniwow-kernel`, `uniwow-feature-<id>`…).

`core/api`, `libs/*` and the shared dependencies (egui, wgpu…) are built into one shared DLL,
`uniwow_api.dll`; the Rust standard library is shared as `std-<hash>.dll`. Together they are the
**runtime**. Every feature DLL links to the runtime, so each of these exists once in memory.
`core/kernel` is linked into the executable: features never link to it, so changing the kernel
does not affect them.

Allowed dependencies (enforced by `xtask check`):

| Layer | May depend on |
|---|---|
| features/* | core/api, libs/* |
| core/kernel | core/api, libs/* |
| core/api | libs/* (types only) |
| libs/* | other libs/* |

A feature never depends on another feature, nor on `core/kernel`. A library never depends on
the core or on a feature.

A feature depends on nothing else, not even through dev or build dependencies: enabling an option
of a crate the runtime also uses makes Cargo rebuild the runtime, which changes its fingerprint and
makes every feature incompatible. A crate a feature needs is added to the runtime (`core/api` or
`libs/*`). `cargo xtask check` names any other dependency.

Every crate of `libs/*` is a normal dependency of `uniwow-api`, so that it is compiled once, into
the runtime: a feature using it receives it through `uniwow_api.dll`, without its own copy of the
code or of its global state, and the library's own dependencies are those of the runtime.
`cargo xtask check` refuses a library that `uniwow-api` does not include, whether a feature uses
it or not.

---

## 3. Feature contract

A feature is one Rust crate in `features/<id>/` (crate type `cdylib`), deployed as `<id>.dll`.

What describes it is declared in its `Cargo.toml`, not in code: id, display name, category, and
the services it **requires** (cannot work without) or **uses** (when present) under
`[package.metadata.uniwow]`; version and description under `[package]`. `cargo xtask build`
copies them into `feature.toml`, which the kernel reads before loading the DLL.

Its behaviour is one type implementing the `Feature` trait of `core/api`, exported by an entry
point macro:

- **register**: declares what the feature contributes (list below). Called once at load.
- **init / shutdown**: start and stop, with access to core services.
- **panel_ui, on_event, on_menu**: draw its panels, receive its events, handle its menu items.

Extension points a feature may contribute to:

| Extension point | Example |
|---|---|
| Panels (dockable windows) | DBC table view, asset browser, 3D view |
| Menu entries and shortcuts | Map > New map |
| Named commands: called by name with JSON arguments and a JSON result, each declared with a description and the schema of its arguments and result. Not to be confused with the undoable `Command` of the history | "open creature <id>", "paint texture" |
| Inspectors, per selection type | creature inspector, doodad inspector |
| Asset handlers, per file type | open or preview `.blp`, `.m2` |
| Settings page | brush defaults |
| Services implementing an interface defined in core/api, under a typed `ServiceKey<T>` declared beside the interface, used both to provide and to ask, so that a type mismatch does not compile | "viewport", "creature lookup" |
| Viewport layers, through the viewport service | terrain, cube; each layer records into its own render bundle |
| Event subscriptions | "project saved", "tile changed" |
| Project data section owned by the feature | spawn edits not yet deployed |

Rules:

| Id | Rule |
|---|---|
| F1 | A feature talks to another only through the core: commands, events, services by interface. Example: the quest editor issues "open creature 1234"; the feature registered for creatures handles it. If no such feature is loaded, the link is shown disabled. |
| F2 | Every modification goes through an undoable command (single undo history). |
| F3 | A feature owns its project data section and its settings; no other feature reads them directly. |
| F4 | A missing required service: the feature is not loaded and the reason is shown. A missing used service: the feature loads without the parts that need it. A feature that fails withdraws its services; requirements are checked again just before each `init`, so a feature whose provider failed meanwhile is not initialised. |
| F5 | A feature that runs code on behalf of another, such as the viewport drawing a layer, catches its failures and reports the culprit with `Context::report_failure`. The kernel disables the culprit as if it had panicked, naming the reporter. |
| F6 | Every action a feature offers to others is a named command. The kernel keeps their catalogue and routes the calls; the same catalogue serves the features, the scripts and native modules (S1). |

---

## 4. Discovery and loading

### Retained mechanism: one DLL per feature, loaded at start

Output layout:

```
UniWoW.exe
uniwow_api.dll                 runtime: core/api, libs, egui, wgpu
std-<hash>.dll                 Rust standard library
features\<id>\<id>.dll
features\<id>\feature.toml     generated at build: id, name, version, runtime fingerprint
```

At start, the kernel:

1. Scans `features\*\feature.toml`. A folder without a manifest or without its DLL is ignored
   and listed.
2. Compares each manifest's **runtime fingerprint** with its own. The fingerprint identifies
   the compiler version and the runtime build. A mismatch refuses the feature with the reason
   ("built for another runtime, rebuild it") instead of loading it.
3. Copies each accepted DLL to a temporary folder and loads the copy, so that a feature can be
   rebuilt while the editor is open.
4. Calls the entry point, checks required services, orders initialization so that service
   providers start before their consumers, collects contributions, then calls `init`.
5. Catches any failure (error or panic) in `register`, `init` or while drawing a panel: that
   feature is disabled and reported in the Features panel and the log (R6).

Enabling or disabling a feature from the Features panel takes effect at the next start. A
loaded feature is never unloaded while the editor runs. Until the project model exists, this
choice and the panel layout are stored per user, in `%APPDATA%\UniWoW\settings.json`.

Developer commands:

```
cargo xtask new-feature <id>     creates features/<id>/ from a template (one empty panel)
cargo xtask build-feature <id>   builds that feature only, against the current runtime
cargo xtask build                builds the runtime, the executable and every feature
cargo xtask run                  builds what changed, then starts the editor
```

| Change | Rebuild |
|---|---|
| Code of one feature | That feature only (`build-feature`) |
| New feature | That feature only |
| `core/kernel` or `app` | The executable only; features stay compatible, as long as the change enables no option of a crate the runtime uses (below) |
| `core/api`, `libs/*`, shared dependency versions or compiler | Runtime and every feature (`build`), because the fingerprint changes |

The kernel and the app are built in the same `cargo build --workspace` as the runtime, and Cargo
merges the options each package asks of a shared crate. If the kernel or the app enabled an option
of a crate the runtime also uses, the runtime would be rebuilt and every feature refused.
`cargo xtask check` reads the resolved dependency graph and refuses a direct dependency of
`core/kernel` or `app` on a crate of the runtime's dependency tree, naming it; such a crate is
reached through `uniwow-api` instead. A crate reached only through another dependency is not seen
by the check: `build-feature` then detects the changed runtime and asks for a full build.

The fingerprint is the BLAKE3 hash of `uniwow_api.dll`. The MSVC linker runs with `/Brepro`,
which removes the link date and makes the PDB identifier depend on the content: relinking
unchanged code gives the same DLL, hence the same fingerprint (checked by rebuilding the runtime
after `cargo clean -p uniwow-api`, and after touching one of its sources without changing it).
A real change of the runtime still asks for every feature to be rebuilt; `cargo xtask build` does it.

`build-feature` compiles the whole workspace, not the feature alone, so that Cargo merges the
options of shared crates exactly as `build` does and leaves the runtime untouched. A compilation
error in another feature therefore blocks it too.

### Constraint accepted with this choice

Rust has no stable binary interface between separately compiled DLLs. Feature DLLs are
therefore only compatible with the runtime they were built against, with the same compiler.
The fingerprint (step 2) turns an incompatibility into a refusal with a message instead of a
crash. A feature DLL taken from another machine works only if built with the same compiler and
the same runtime.

### Alternatives not retained

| Alternative | Reason |
|---|---|
| Features compiled into the editor | Adding or removing a feature requires recompiling the editor (R3). |
| Stable C interface between DLLs (abi_stable, stabby) | egui and wgpu types cannot cross it: features could not draw their own panels or 3D. |
| One process per feature | Sharing the 3D view and the panels between processes is too heavy. |

---

## 5. Core services (core/kernel)

| Service | Role |
|---|---|
| Shell | Main window, menus, dockable layout (egui_dock), layouts saved per user. Panels of absent features leave the layout; a returning panel rejoins its area, or the default layout is rebuilt when its whole area had disappeared |
| Feature loader | Section 4 |
| Features panel | Lists features, version, state, refusal or failure reason; enable or disable |
| Commands and history | Undo, redo, unsaved-changes tracking. When a feature fails, all its entries leave the history, done and undone, with a warning in the log; the others stay valid since a command only changes its own feature's state (F3) |
| Events | Publish and subscribe, typed by serialisation: the topic is a string and the payload JSON, written with `Context::publish_as` and read with `Event::decode` into a type each feature declares on its own side. No Rust type is shared between features. Events may be published from any thread; they are delivered on the interface thread |
| Services | Registry of interface implementations provided by features |
| Selection | Current selection, any type |
| Project | Open, save; content defined in a later step |
| Settings | Global, per project, per feature. Written atomically (temporary file, then rename), at most once per second and at exit |
| Jobs | Pool of worker threads, one per processor core: background jobs with progress and cancel (T2) |
| Log | Log panel shared by all features. GPU errors captured by no feature are logged instead of stopping the editor |
| Inspector host | Shows the selection with the inspector registered for its type |

The 3D view is not a core service: it is the `viewport` feature (section 7).

Threads:

| Id | Rule |
|---|---|
| T1 | The interface thread draws, applies the undoable commands and owns the state of each feature. It never waits for slow work. |
| T2 | The kernel keeps a pool of worker threads, one per processor core, for computations. `Context::spawn` runs a job there, with progress and cancel; its result comes back to the feature on the interface thread. Work that waits, such as a script, runs with `Context::spawn_thread` on a thread of its own, so that waiting never holds a thread of the pool; it is otherwise a job like the others. A Jobs panel lists the jobs running. |
| T3 | Service interfaces are shared between threads (`Send + Sync`, held in an `Arc`), so that jobs, scripts and native modules call them directly. An interface tied to the interface thread says so explicitly. |
| T4 | Each named command declares where it runs: on the interface thread when it changes a feature's state (through an undoable command), or on the calling thread when it only reads or synchronises itself. The second kind answers at once, without waiting for a frame. |
| T5 | The GPU device and queue can be used from any thread: jobs create and upload buffers and textures; only drawing happens on the interface thread. |
| T6 | Each run of a script has a named thread of its own (T2), never the interface thread. Each run of a Lua script has its own Lua state, so several run in parallel. Python scripts run on worker threads too, but standard CPython lets one thread at a time execute Python code (the GIL): their parallel work comes from the commands they call. |
| T7 | Every function of the C interface can be called from any thread; native modules may create their own threads. |

---

## 6. Libraries (libs/*)

| Library | Role |
|---|---|
| formats | Read and write MPQ, DBC, ADT, WDT, WDL, WMO, M2, BLP. Based on warcraft-rs (MIT/Apache) where its writing is verified, own code otherwise |
| defs | DBC layouts for build 12340 (WoWDBDefs) |
| vfs | Client archive chain in the 3.3.5a load order, plus the project's own files on top |
| gpu | Generic GPU helpers on wgpu (device, shaders, buffers, camera math). Drawing of each kind of object belongs to the feature that owns it |
| db | MySQL access to the AzerothCore databases |
| server-link | SOAP client, server process control |
| client-link | Protocol with the WXL client module |
| ids | Id range allocation per module |

---

## 7. Feature catalogue

Each line is one feature, one DLL. The list is open: new features are added through section 4
without touching the core.

### World

| Id | Feature |
|---|---|
| viewport | 3D window: camera, picking, gizmos. Provides the "viewport" service to which the features below add their drawing and tools. Each layer records into its own render bundle, with the target's formats and sample count, inside a validation error scope; the viewport also catches the panic of `RenderBundleEncoder::finish`, which wgpu 30 raises on an invalid command instead of reporting it to the scope. A faulty layer is removed and its feature reported (F5); the grid and the other layers stay |
| maps | Map list (Map.dbc), WDT and WDL, create or duplicate a map, tile management, minimap tiles |
| terrain | Draws terrain in the viewport; height sculpting, texture painting (layers, alpha maps), holes, vertex shading, area painting, chunk flags |
| liquids | Draws and edits water, lava, slime: create, heights, types |
| placement | Draws M2 doodads and WMOs in tiles; place, move, rotate, scale, with gizmos and snapping |
| environment | Lighting and sky (Light tables, skyboxes), zone music and ambience |
| spawns | Server creatures and game objects placed in the viewport, waypoints, formations |
| server-map-data | Regenerate .map, vmaps and mmaps for changed tiles (AzerothCore extractors), in the background |

### Data

| Id | Feature |
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

| Id | Feature |
|---|---|
| assets | Browse the virtual file system (archives and project), search, preview |
| textures | BLP view, PNG to BLP and back, format and size checks |
| models | M2 and WMO viewer (animations, textures, attachments), import of new models |
| retail-import | Fetch modern assets through the wow.export bridge |

### Interface and scripts

| Id | Feature |
|---|---|
| interface | Client interface files (FrameXML, GlueXML, addons, AIO): tree, open in an external editor, reload in the client |
| scripts | ALE Lua scripts and C++ server modules: open in an external editor, build, errors in the log, reload or restart |

### Run

| Id | Feature |
|---|---|
| server | Start and stop authserver and worldserver, console, SOAP commands, server logs |
| play | One action: server and client started or reused, automatic login on the admin account, character moved to the viewport camera, changes pushed live |

### Delivery

| Id | Feature |
|---|---|
| package | Build an installable WoW-mods module (installer.json): files in the right patch archive, DBC copied from client to server, install and uninstall SQL, id ranges checked |

### Scripting and native modules

| Id | Feature |
|---|---|
| scripting-lua | Lua 5.1, the dialect of the 3.3.5 client and of ALE scripts, compiled into the runtime (mlua): console panel, script runner, the `uniwow` module |
| scripting-python | CPython, the latest stable version when the feature is built, embedded (PyO3) with the official embeddable distribution of Windows in `interpreters\python-3.xx\`: the same console, runner and `uniwow` module |
| native-modules | Loads the compiled modules of `modules\`: DLLs written in C++, in C# compiled with NativeAOT, or in any language able to export a C function. Each receives the C interface (S1) |

Rules:

| Id | Rule |
|---|---|
| S1 | One generic interface, the same for every language: list the named commands with their descriptions and schemas, call one by name, publish and receive events, read and write settings, log. Every value crosses it as JSON. It is defined once, independently of any language, and also offered as a C interface (`extern "C"` functions taking and returning UTF-8 JSON, header `uniwow.h`), so that compiled code reaches the same commands without depending on the Rust ABI. The Lua and Python `uniwow` modules only translate their values to and from JSON on top of this interface: they add no command of their own, so every language always has the same access. |
| S2 | Named commands (F6) must exist in the kernel first: they are what scripts and native modules mostly call. |
| S3 | Scripts never run on the interface thread (T6). A call that changes a feature's state is applied on the interface thread at the next frame; a call to a command running on the calling thread answers at once (T4). A running script can be stopped. |
| S4 | Every change one run of a script makes forms a single undo entry. The kernel learns to group commands. A group belongs to one caller on one thread and can be nested; it closes at its outermost end, when the job that opened it ends, when its feature fails, or from the Edit menu. While an open group already holds a change, Undo and Redo are refused, greyed with the reason; a group that changed nothing yet, such as a script waiting for events, blocks nothing. Changes made by hand meanwhile enter the history on their own: when they touch what the script changes, their order relative to the group can be imprecise. Indirect changes are not grouped: a command triggered by an event a script publishes is applied when the event is delivered, outside the group. |
| S5 | A Lua or Python error is shown in the console with its line; it does not make the feature fail. |
| S6 | Native code runs inside the editor and can end its process: a crash there is not an error that can be caught. Lua scripts cannot load C modules, nor precompiled Lua chunks, which the bytecode checks of Lua 5.1 cannot keep from corrupting the memory: `string.dump` is removed, and every way of loading Lua code (scripts, console, `load`, `loadstring`, `loadfile`, `dofile`, `require`) accepts source text only. The compiled packages a Python script imports and the native modules carry this risk, which is accepted. |
| S7 | Scripts and native modules have full access to the machine, like editor scripts in Unity: one received from someone else is checked before it is used. |
| S8 | Each language stays optional: without `scripting-python.dll`, or without the Python files, the editor starts with Lua only, and the other way round. In particular the runtime must not require the Python DLL to start. |
| S9 | One interpreter per language. Scripts are stored by language and version: `scripts\lua-5.1\…`, `scripts\python-3.xx\…`, the Python version being the one shipped. Native modules go in `modules\`. |
| S10 | A native module exports one C entry point. It receives the table of functions of the C interface and returns its description (name, version, the version of `uniwow.h` it was built with) and the named commands it offers, implemented in its own language with the same JSON form. |

Risks to verify first, before any other work on scripting: the runtime's exported symbol count with PyO3 and mlua inside it; starting the editor without the Python DLL while PyO3 is part of the runtime (delayed loading); the embeddable Python distribution beside the executable; a C++ module and a C# NativeAOT module calling the C interface from several threads.

---

## 8. Repository layout

```
E:\WoW-editor
  Cargo.toml            workspace: app, core/*, libs/*, features/*, xtask
  app/
  core/api/
  core/kernel/
  libs/<name>/
  features/<id>/
  sdk/uniwow.h          the C interface of native modules (S1)
  modules-src/<name>/   native modules built by `cargo xtask build` (C++, MSVC)
  scripts/lua-5.1/      sample scripts, copied beside the executable
  client-bridge/        C++ (WXL SDK), own build
  xtask/
  docs/
  .github/workflows/    CI on Windows: fmt, clippy -D warnings, cargo test, xtask build and check
```

---

## 9. Milestones

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
second to a command running on the interface thread. `Editor::call` refuses, with an error, to wait
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

### Milestone 4: Python and C# (outline)

`scripting-python` (CPython embedded through PyO3, the embeddable distribution in
`interpreters\python-3.xx\`, the runtime starting without the Python DLL, the same console, runner
and `uniwow` module as Lua, the GIL limit stated), and a sample native module in C# compiled with
NativeAOT. Specified in detail when milestone 3 is done.

---

## 10. Open questions

- Project model: what a project contains, where it is stored, how it maps to a WoW-mods module.
- Installations targeted: client with WXL, server, database connection.
- Order of the features after milestone 1.
