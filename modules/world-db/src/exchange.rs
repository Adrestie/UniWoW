//! The changes exported to a file and imported from one (step 16.9): the applications picked, or the
//! changes waiting, made a `.uniwow` with what they name and what belongs to what they make, read
//! from the server chosen; a file imported into the server chosen once its problems are resolved in
//! a window, one transaction, its bridge asked.

use std::collections::BTreeMap;
use std::path::PathBuf;

use uniwow_api::db::write::{self, Entry, Memory, Row, RowChange, Store as _};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::{self, Reload};
use uniwow_api::{Context, JobId, egui, log, rfd};

use crate::package::{self, Choice, Kind, Package, Problem, Reference};
use crate::sending::entries_of;
use crate::{Application, WorldDbModule, bridge, now, server_of, write_script_to};

/// The rows of `table` of `key` on the server, through the service.
fn rows_there(database: &world_db::Handle, table: &str, key: &Row) -> Result<Vec<Row>, String> {
    let (sql, parameters) = package::select(table, key).ok_or_else(|| format!("{table}: no table or column"))?;
    let rows = database.query(&sql, &parameters)?;
    Ok(rows
        .rows
        .iter()
        .map(|values| rows.columns.iter().cloned().zip(values.iter().cloned()).collect())
        .collect())
}

/// What `changes` would make on the server: its rows read, the changes made on a copy in memory.
fn simulated(database: &world_db::Handle, changes: &[RowChange]) -> Result<Vec<Entry>, String> {
    let mut memory = Memory::default();
    let mut read = std::collections::BTreeSet::new();
    for change in changes {
        let mut keys = vec![change.key().clone()];
        if let RowChange::Put { from: Some(from), .. } = change {
            keys.push(from.clone());
        }
        for key in keys {
            if read.insert((change.table().to_owned(), json!(key).to_string())) {
                for row in rows_there(database, change.table(), &key)? {
                    memory.insert(change.table(), &row)?;
                }
            }
        }
    }
    write::apply(&mut memory, changes)
}

/// Where an import stands.
pub enum Importing {
    /// The file read and the server's rows: being made.
    Reading(JobId),
    /// The problems shown, the user choosing.
    Review {
        package: Box<Package>,
        problems: Vec<Problem>,
        chosen: Vec<Choice>,
        fresh: BTreeMap<Kind, i64>,
        file: PathBuf,
    },
    /// Being written.
    Writing {
        job: JobId,
        reloads: Vec<Reload>,
        label: String,
    },
}

/// What the job reading an import gives.
pub type Read = Result<Option<(PathBuf, Package, Vec<Problem>, BTreeMap<Kind, i64>)>, String>;

impl WorldDbModule {
    /// Exports the applications picked, or else the changes waiting, to a file the user names.
    pub(crate) fn export(&mut self, ctx: &mut Context) {
        let Some(database) = ctx.service(world_db::SERVICE) else {
            return;
        };
        let server = server_of(ctx);
        let (picked, reloads) = entries_of(&self.applications, &self.picked);
        let pending = database.pending();
        let from_pending = picked.is_empty();
        let changes: Vec<RowChange> = pending.iter().flat_map(|held| held.changes.clone()).collect();
        let mut reloads = if from_pending {
            pending.iter().flat_map(|held| held.reloads.clone()).collect()
        } else {
            reloads
        };
        reloads.sort();
        reloads.dedup();
        let label = if from_pending {
            format!("{} changes waiting on {}", changes.len(), server.name)
        } else {
            format!("{} applications of {}", self.picked.len(), server.name)
        };
        let job = ctx.spawn_thread(
            "Export changes to a file",
            move |_| -> Result<Option<PathBuf>, String> {
                let rows = if from_pending {
                    simulated(&database, &changes)?
                } else {
                    picked
                };
                let rows: Vec<Entry> = rows.into_iter().filter(|entry| entry.before != entry.after).collect();
                if rows.is_empty() {
                    return Err("nothing to export".to_owned());
                }
                let mut references = Vec::new();
                for (table, key, why) in package::wanted(&rows) {
                    match rows_there(&database, &table, &key) {
                        Ok(rows) if !rows.is_empty() => references.push(Reference { table, key, rows, why }),
                        Ok(_) => {}
                        Err(reason) => log::warn!("{table} not read for the export: {reason}"),
                    }
                }
                let package = Package {
                    label: label.clone(),
                    made: now(),
                    server: server.name.clone(),
                    database: server.database.clone(),
                    rows,
                    references,
                    reloads,
                };
                let name = format!("{}.{}", label.replace([' ', ':'], "_"), package::EXTENSION);
                let Some(path) = rfd::FileDialog::new()
                    .set_title("Export the changes")
                    .add_filter("Changes of UniWoW", &[package::EXTENSION])
                    .set_file_name(name)
                    .save_file()
                else {
                    return Ok(None);
                };
                let text = uniwow_api::serde_json::to_string_pretty(&package.to_json()).map_err(|e| e.to_string())?;
                std::fs::write(&path, text).map_err(|error| format!("{} is not written: {error}", path.display()))?;
                Ok(Some(path))
            },
        );
        self.exporting = Some(job);
    }

    pub(crate) fn exported(&mut self, outcome: Option<Result<Option<PathBuf>, String>>) {
        self.said = match outcome {
            Some(Ok(Some(path))) => Some((true, format!("Exported to {}", path.display()))),
            Some(Ok(None)) | None => None,
            Some(Err(reason)) => Some((false, format!("Not exported: {reason}"))),
        };
    }

    /// Imports a file the user picks into the server chosen: read, its problems found, a window.
    pub(crate) fn import(&mut self, ctx: &mut Context) {
        let Some(database) = ctx.service(world_db::SERVICE) else {
            return;
        };
        let job = ctx.spawn_thread("Import changes from a file", move |_| -> Read {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Import changes")
                .add_filter("Changes of UniWoW", &[package::EXTENSION])
                .pick_file()
            else {
                return Ok(None);
            };
            let text =
                std::fs::read_to_string(&path).map_err(|error| format!("{} is not read: {error}", path.display()))?;
            let value: Value =
                uniwow_api::serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
            let package = Package::of_json(&value)?;
            let rows = package
                .rows
                .iter()
                .map(|entry| rows_there(&database, &entry.table, &entry.key))
                .collect::<Result<Vec<_>, _>>()?;
            let references: Vec<Vec<Row>> = package
                .references
                .iter()
                .map(|reference| rows_there(&database, &reference.table, &reference.key).unwrap_or_default())
                .collect();
            let problems = package::problems(&package, &rows, &references);
            // The first free id of each kind renumbered: the quests in the editor's block.
            let mut fresh = BTreeMap::new();
            for (kind, sql) in [
                (Kind::Creature, "SELECT MAX(guid) AS top FROM creature"),
                (Kind::GameObject, "SELECT MAX(guid) AS top FROM gameobject"),
                (Kind::Path, "SELECT MAX(id) AS top FROM waypoint_data"),
                (
                    Kind::Quest,
                    "SELECT MAX(ID) AS top FROM quest_template WHERE ID BETWEEN 97000 AND 97099",
                ),
            ] {
                let top = database
                    .query(sql, &[])
                    .ok()
                    .and_then(|rows| rows.rows.first()?.first().cloned())
                    .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()));
                let next = match kind {
                    Kind::Quest => top.map_or(97000, |top| top + 1),
                    _ => top.unwrap_or(0) + 1,
                };
                fresh.insert(kind, next);
            }
            Ok(Some((path, package, problems, fresh)))
        });
        self.importing = Some(Importing::Reading(job));
    }

    pub(crate) fn import_read(&mut self, outcome: Option<Read>) {
        self.importing = None;
        match outcome {
            Some(Ok(Some((file, package, problems, fresh)))) => {
                let chosen = problems
                    .iter()
                    .map(|problem| package::choices(problem, &package).remove(0))
                    .collect();
                self.importing = Some(Importing::Review {
                    package: Box::new(package),
                    problems,
                    chosen,
                    fresh,
                    file,
                });
            }
            Some(Ok(None)) | None => {}
            Some(Err(reason)) => self.said = Some((false, format!("Not imported: {reason}"))),
        }
    }

    /// The window of an import: what it holds, each problem and its choices; written once asked.
    pub(crate) fn importing_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        let Some(Importing::Review {
            package,
            problems,
            chosen,
            fresh,
            file,
        }) = &mut self.importing
        else {
            return;
        };
        let server = server_of(ctx);
        let (mut import, mut cancel) = (false, false);
        egui::Window::new("Import changes")
            .collapsible(false)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(egui, |ui| {
                ui.label(format!(
                    "{} — {} rows and {} references, made on {} ({}) on {}; into {}.",
                    package.label,
                    package.rows.len(),
                    package.references.len(),
                    package.server,
                    package.database,
                    package.made,
                    server.name
                ));
                ui.label(egui::RichText::new(file.display().to_string()).weak().small());
                ui.separator();
                if problems.is_empty() {
                    ui.label("No problem: each row is on the target what the change found, or what it makes.");
                }
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for (at, problem) in problems.iter().enumerate() {
                        let (text, table, key) = match problem {
                            Problem::Taken(row) => {
                                let entry = &package.rows[*row];
                                ("Its id is taken there by something else", &entry.table, &entry.key)
                            }
                            Problem::Changed(row) => {
                                let entry = &package.rows[*row];
                                ("Changed there since", &entry.table, &entry.key)
                            }
                            Problem::Missing(reference) => {
                                let held = &package.references[*reference];
                                ("Missing there", &held.table, &held.key)
                            }
                            Problem::Differs(reference) => {
                                let held = &package.references[*reference];
                                ("Otherwise there", &held.table, &held.key)
                            }
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new(format!("{table} {}", json!(key))).monospace());
                            ui.label(egui::RichText::new(text).color(ui.visuals().warn_fg_color));
                        });
                        let offered = package::choices(problem, package);
                        ui.horizontal(|ui| {
                            for choice in offered {
                                let label = match &choice {
                                    Choice::Renumber => {
                                        let kind = match problem {
                                            Problem::Taken(row) => {
                                                let entry = &package.rows[*row];
                                                package::entity_of(&entry.table, &entry.key).map(|(kind, _)| kind)
                                            }
                                            _ => None,
                                        };
                                        let next = kind.and_then(|kind| fresh.get(&kind)).copied().unwrap_or(0);
                                        format!("Renumber (from {next})")
                                    }
                                    Choice::Overwrite => "Overwrite".to_owned(),
                                    Choice::Keep => "Keep the target's".to_owned(),
                                    Choice::Insert => "Insert it".to_owned(),
                                    Choice::Link(_) => "Link to an id there".to_owned(),
                                    Choice::Ignore => "Leave it out".to_owned(),
                                };
                                let same = std::mem::discriminant(&chosen[at]) == std::mem::discriminant(&choice);
                                let hint = match &choice {
                                    Choice::Renumber => "Gives it the next free id on this server; what names it follows",
                                    Choice::Overwrite => "The file's rows written over this server's",
                                    Choice::Keep => "This server's rows kept, the file's left out",
                                    Choice::Insert => "The rows of the file inserted on this server",
                                    Choice::Link(_) => "Named by the id typed, of an entity of this server, in place of the file's",
                                    Choice::Ignore => "Left out of the import",
                                };
                                if ui.selectable_label(same, label).on_hover_text(hint).clicked() && !same {
                                    chosen[at] = choice;
                                }
                            }
                            if let Choice::Link(id) = &mut chosen[at] {
                                ui.add(egui::TextEdit::singleline(id).hint_text("id").desired_width(80.0))
                                    .on_hover_text("The id on this server of the entity to name in its place");
                            }
                        });
                        ui.add_space(4.0);
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    import = ui
                        .button(format!("Import into {}", server.name))
                        .on_hover_text("Writes the rows as chosen into the world database of this server, one transaction; then its bridge loads them again")
                        .clicked();
                    cancel = ui.button("Cancel").on_hover_text("Nothing written").clicked();
                });
            });
        if cancel {
            self.importing = None;
            return;
        }
        if !import {
            return;
        }
        let mut next = fresh.clone();
        let made = package::resolved(package, problems, chosen, |kind| {
            let id = next.get_mut(&kind)?;
            if kind == Kind::Quest && *id > 97099 {
                return None;
            }
            *id += 1;
            Some(*id - 1)
        });
        let (changes, reloads, renamed) = match made {
            Ok(made) => made,
            Err(reason) => {
                self.said = Some((false, format!("Not imported: {reason}")));
                return;
            }
        };
        for ((kind, old), new) in &renamed {
            log::info!("imported: the {} {old} is {new} here", kind.name());
        }
        let label = package.label.clone();
        let shared = self.shared.clone();
        let job = ctx.spawn("Import changes into the world database", move |_| {
            use world_db::WorldDb as _;
            crate::Service(shared).apply(&changes)
        });
        self.importing = Some(Importing::Writing { job, reloads, label });
    }

    /// The import written: kept as an application of the server chosen, the modules reading their
    /// rows again, its bridge asked.
    pub(crate) fn imported(&mut self, outcome: Option<Result<Vec<Entry>, String>>, ctx: &mut Context) {
        let Some(Importing::Writing { reloads, label, .. }) = self.importing.take() else {
            return;
        };
        let entries = match outcome {
            Some(Ok(entries)) => entries,
            Some(Err(reason)) => {
                self.said = Some((false, format!("Nothing imported: {reason}")));
                return;
            }
            None => return,
        };
        let server = server_of(ctx);
        let when = now();
        let application = Application {
            when: when.clone(),
            server: server.name.clone(),
            database: format!("{} on {}:{}", server.database, server.host, server.port),
            owners: Vec::new(),
            entries,
            reloads: reloads.clone(),
        };
        let rows = application.rows();
        let title = format!(
            "Imported by UniWoW on {when} into {}: {label}, {rows} rows",
            server.name
        );
        let (mark, failed) = write_script_to(&server.sql_folder, &title, &when, false, &application.entries);
        self.mark(mark, ctx);
        self.applications.push(application);
        let message = format!("{label}: {rows} rows imported into {}", server.name);
        log::info!("{message}");
        self.said = Some((
            failed.is_none(),
            failed.map_or(message.clone(), |failed| format!("{message}; {failed}")),
        ));
        // The modules read the rows again, their own changes kept.
        ctx.publish(
            world_db::APPLIED_TOPIC,
            json!({ "owners": [], "read": ["spawns", "quests"] }),
        );
        let demands = bridge::demands(&reloads);
        if !demands.is_empty() {
            let (port, token) = (server.bridge_port, server.bridge_token.clone());
            self.bridging = Some(ctx.spawn("Ask the server to load the changes again", move |_| {
                bridge::reload(port, &token, &demands)
            }));
        }
    }
}
