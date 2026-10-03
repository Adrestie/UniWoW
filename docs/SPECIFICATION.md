# UniWoW — Architecture and feature catalogue

Status: **validated**. Milestone 1 built and validated. Open questions in section 10.

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
| Commands | "open creature <id>", "paint texture" |
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
| `core/kernel` | The executable only; features stay compatible |
| `core/api`, `libs/*`, shared dependency versions or compiler | Runtime and every feature (`build`), because the fingerprint changes |

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
| Events | Publish and subscribe, typed by serialisation: the topic is a string and the payload JSON, written with `Context::publish_as` and read with `Event::decode` into a type each feature declares on its own side. No Rust type is shared between features |
| Services | Registry of interface implementations provided by features |
| Selection | Current selection, any type |
| Project | Open, save; content defined in a later step |
| Settings | Global, per project, per feature. Written atomically (temporary file, then rename), at most once per second and at exit |
| Jobs | Background tasks with progress and cancel |
| Log | Log panel shared by all features. GPU errors captured by no feature are logged instead of stopping the editor |
| Inspector host | Shows the selection with the inspector registered for its type |

The 3D view is not a core service: it is the `viewport` feature (section 7).

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
  client-bridge/        C++ (WXL SDK), own build
  xtask/
  docs/
  .github/workflows/    CI on Windows: fmt, clippy -D warnings, cargo test, xtask build and check
```

---

## 9. Milestone 1: proof of the architecture

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

Risk verified first: a Windows DLL exports at most 65,535 symbols. Unoptimised, the runtime
exceeds it (LNK1189). The `dev` profile is therefore built with `opt-level = 2`, which stops the
sharing of generic instantiations: 17,764 exported symbols, 27% of the limit. `cargo xtask check`
reports the count and fails from 50,000; the runtime would then have to be split.

---

## 10. Open questions

- Project model: what a project contains, where it is stored, how it maps to a WoW-mods module.
- Installations targeted: client with WXL, server, database connection.
- Order of the features after milestone 1.
