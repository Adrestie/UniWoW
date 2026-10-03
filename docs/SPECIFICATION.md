# WoW Editor — Architecture and feature catalogue

Status: **draft, awaiting validation**. No code yet.

The editor is a standalone desktop application (outside the game client) used to modify a
WoW 3.3.5a (build 12340) client and an AzerothCore server: maps, data, assets, interface,
scripts, packaging. Written in Rust, user interface with egui, 3D rendering with wgpu.

Players are expected to run the client with WarcraftXL (WXL).

---

## 1. Modularity requirements

| Id | Requirement |
|---|---|
| R1 | Every feature is separate from the core. The core contains no code specific to any feature and never names one. |
| R2 | Adding a feature is simple: one command creates it, nothing in the core is edited by hand. |
| R3 | The core detects the features present and loads them. |
| R4 | Removing a feature (deleting its folder) leaves the core and every other feature building and running. |
| R5 | A feature that fails is disabled and reported; the editor keeps running. |

---

## 2. Layers

```
app            thin executable: starts the core
core/api       contracts offered to features (traits, extension points, registration macro)
core/kernel    feature loader and core services (section 5)
libs/*         shared libraries without user interface (section 6)
features/*     one folder per feature (section 7)
client-bridge  C++ WXL module running inside the client (outside the Cargo workspace)
xtask          developer commands: new-feature, build, run, check
```

Allowed dependencies (enforced by `xtask check`):

| Layer | May depend on |
|---|---|
| features/* | core/api, libs/* |
| core/kernel | core/api, libs/* |
| core/api | libs/* (types only) |
| libs/* | other libs/* |

A feature never depends on another feature, nor on `core/kernel`. A library never depends on
the core or on a feature.

---

## 3. Feature contract

A feature is one Rust crate in `features/<id>/`. It exposes one type implementing the `Feature`
trait of `core/api`, declared with a registration macro:

- **info**: id, display name, version, category, description, API version it was built for.
- **register**: declares what the feature contributes (list below). Called once at load.
- **init / shutdown**: start and stop, with access to core services.

Extension points a feature may contribute to:

| Extension point | Example |
|---|---|
| Panels (dockable windows) | DBC table view, asset browser |
| Menu entries and shortcuts | Map > New map |
| Commands | "open creature <id>", "paint texture" |
| Inspectors, per selection type | creature inspector, doodad inspector |
| Viewport layers (drawing and picking) | terrain, spawns, liquids |
| Viewport tools | sculpt brush, placement gizmo |
| Asset handlers, per file type | open or preview `.blp`, `.m2` |
| Settings page | brush defaults |
| Services implementing an interface defined in core/api | "creature lookup" |
| Event subscriptions | "project saved", "tile changed" |
| Project data section owned by the feature | spawn edits not yet deployed |

Rules:

| Id | Rule |
|---|---|
| F1 | A feature talks to another only through the core: commands, events, services by interface. Example: the quest editor issues "open creature 1234"; the feature registered for creatures handles it. If no such feature is loaded, the link is shown disabled. |
| F2 | Every modification goes through an undoable command (single undo history). |
| F3 | A feature owns its project data section and its settings; no other feature reads them directly. |
| F4 | A consumer of a service handles its absence (R4). |

---

## 4. Discovery and loading

### Retained mechanism: compiled-in features, discovered automatically

Build time:

1. The workspace includes `features/*` by pattern: a new folder is part of the build without
   editing the workspace.
2. `cargo xtask build` scans `features/` and regenerates the list of features linked into the
   editor (a generated file, never edited by hand, not part of the core's code).
3. Each feature registers itself through the macro; the core enumerates what is registered.

Run time:

4. The kernel enumerates registered features, rejects those built for another API version,
   orders initialization so that service providers start before their consumers, collects
   contributions, then calls `init`.
5. Each feature can be enabled or disabled per project from the **Features** panel, without
   rebuilding.
6. A failure (error or panic) in `register`, `init` or while drawing a panel disables that
   feature only and is shown in the Features panel and the log (R5).

Adding a feature:

```
cargo xtask new-feature <id>     creates features/<id>/ from a template (one empty panel)
cargo xtask run                  rebuilds and starts the editor; the feature is loaded
```

Copying an existing feature folder into `features/` and running `cargo xtask run` works the same.

### Alternative not retained: DLL plugins loaded at run time

The core would scan a `features/` folder next to the executable and load each DLL.

- Gain: add or replace a feature without rebuilding the editor.
- Cost: Rust has no stable binary interface. Every DLL must be built with exactly the same
  compiler and dependency versions as the editor, otherwise behaviour is undefined (crashes
  without a clear message). egui and wgpu types cross the boundary. Loading is `unsafe`.
  Bevy deprecated its equivalent mechanism (0.14) and points to safer alternatives.
- Since features are built in the same workspace anyway, the gain is small.

---

## 5. Core services (core/kernel)

| Service | Role |
|---|---|
| Shell | Main window, menus, dockable layout (egui_dock), layouts saved per user |
| Feature loader | Section 4 |
| Features panel | Lists features, version, state, errors; enable or disable |
| Commands and history | Undo, redo, unsaved-changes tracking |
| Events | Publish and subscribe, typed |
| Services | Registry of interface implementations |
| Selection | Current selection, any type |
| Project | Open, save; content defined in a later step |
| Settings | Global, per project, per feature |
| Jobs | Background tasks with progress and cancel |
| Log | Log panel shared by all features |
| Viewport host | 3D views, camera, picking, gizmos; drawing comes from feature layers |
| Inspector host | Shows the selection with the inspector registered for its type |

---

## 6. Libraries (libs/*)

| Library | Role |
|---|---|
| formats | Read and write MPQ, DBC, ADT, WDT, WDL, WMO, M2, BLP. Based on warcraft-rs (MIT/Apache) where its writing is verified, own code otherwise |
| defs | DBC layouts for build 12340 (WoWDBDefs) |
| vfs | Client archive chain in the 3.3.5a load order, plus the project's own files on top |
| render | wgpu renderer: terrain, M2, WMO, liquids, sky |
| db | MySQL access to the AzerothCore databases |
| server-link | SOAP client, server process control |
| client-link | Protocol with the WXL client module |
| ids | Id range allocation per module |

---

## 7. Feature catalogue

Each line is one feature, in its own folder. The list is open: new features are added through
section 4 without touching the core.

### World

| Id | Feature |
|---|---|
| maps | Map list (Map.dbc), WDT and WDL, create or duplicate a map, tile management, minimap tiles |
| terrain | Height sculpting, texture painting (layers, alpha maps), holes, vertex shading, area painting, chunk flags |
| liquids | Water, lava, slime: create, edit heights and types |
| placement | Place, move, rotate, scale M2 doodads and WMOs in tiles, with gizmos and snapping |
| environment | Lighting and sky (Light tables, skyboxes), zone music and ambience |
| spawns | Server creatures and game objects placed in the 3D view, waypoints, formations |
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
```

---

## 9. Milestone 1: proof of the architecture

Content:

- Workspace, `xtask` (new-feature, build, run, check).
- Kernel: shell with dockable panels, feature loader, Features panel, commands and undo,
  events, settings, log panel.
- Empty 3D viewport drawn by wgpu inside egui: camera and grid only.
- Two sample features created from the template, communicating only through an event.

No WoW file is handled in this milestone.

Acceptance:

| Check | Expected result |
|---|---|
| `cargo xtask new-feature third`, then `cargo xtask run` | A third panel appears; no core file changed (`git status` shows only `features/third/`) |
| Delete `features/third/`, then `cargo xtask run` | The editor builds and starts without it |
| A sample feature panics while drawing | That feature is shown as failed in the Features panel; the rest keeps working |
| Disable a feature in the Features panel | Its panels disappear; the choice persists |
| A sample feature depends on the other in its Cargo.toml | `cargo xtask check` fails and names the offending dependency |

---

## 10. Open questions

- Name of the tool (crate prefix).
- Project model: what a project contains, where it is stored, how it maps to a WoW-mods module.
- Installations targeted: client with WXL, server, database connection.
- Order of the features after milestone 1.
