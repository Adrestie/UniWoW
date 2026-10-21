//! The applications sent to another server (step 16.8): the rows they made, written there as they
//! are here once the rows of their keys there are read and compared with what they replaced here;
//! the rows that differ shown, overwritten or left; one transaction; the server's bridge asked to
//! load them again.

use std::collections::BTreeSet;

use uniwow_api::db::Writer;
use uniwow_api::db::secret;
use uniwow_api::db::write::{Entry, Row, RowChange};
use uniwow_api::serde_json::json;
use uniwow_api::world_db::{Reload, Server};
use uniwow_api::{Context, JobId, egui, log};

use crate::{Application, TIMEOUT, WorldDbModule, bridge, now, servers, write_script_to};

/// The entries of the applications `picked` of `applications`, in the order they were made.
pub fn entries_of(applications: &[Application], picked: &BTreeSet<usize>) -> (Vec<Entry>, Vec<Reload>) {
    let (mut entries, mut reloads) = (Vec::new(), Vec::new());
    for at in picked {
        if let Some(application) = applications.get(*at) {
            entries.extend(
                application
                    .entries
                    .iter()
                    .filter(|entry| entry.before != entry.after)
                    .cloned(),
            );
            reloads.extend(application.reloads.iter().cloned());
        }
    }
    reloads.sort();
    reloads.dedup();
    (entries, reloads)
}

/// The entries whose rows on the target (`there`, in their order) are not those they replaced here.
pub fn differing(entries: &[Entry], there: &[Vec<Row>]) -> Vec<usize> {
    let sorted = |rows: &[Row]| {
        let mut rows: Vec<String> = rows.iter().map(|row| json!(row).to_string()).collect();
        rows.sort();
        rows
    };
    entries
        .iter()
        .zip(there)
        .enumerate()
        .filter(|(_, (entry, there))| sorted(&entry.before) != sorted(there))
        .map(|(at, _)| at)
        .collect()
}

/// The changes making on the target what `entries` made here, those of `left` left out: each key's
/// rows taken away, those after put.
pub fn changes_of(entries: &[Entry], left: &BTreeSet<usize>) -> Vec<RowChange> {
    let mut out = Vec::new();
    for (at, entry) in entries.iter().enumerate() {
        if left.contains(&at) {
            continue;
        }
        out.push(RowChange::Delete {
            table: entry.table.clone(),
            key: entry.key.clone(),
        });
        if !entry.after.is_empty() {
            out.push(RowChange::Insert {
                table: entry.table.clone(),
                key: entry.key.clone(),
                rows: entry.after.clone(),
            });
        }
    }
    out
}

/// Where a sending stands.
pub enum Sending {
    /// The password of the target asked.
    Password { target: Server, password: String },
    /// The rows of the target being read.
    Reading {
        job: JobId,
        target: Server,
        password: String,
    },
    /// The rows that differ shown, the user choosing.
    Review {
        target: Server,
        password: String,
        entries: Vec<Entry>,
        differing: Vec<usize>,
    },
    /// Being written.
    Writing {
        job: JobId,
        target: Server,
        reloads: Vec<Reload>,
    },
}

impl WorldDbModule {
    /// Sends the applications picked to `target`: its password kept, or asked.
    pub(crate) fn send(&mut self, target: Server, ctx: &mut Context) {
        match self.vault.get(&secret::target(&servers::settings(&target))) {
            Ok(Some(password)) if !password.is_empty() => self.read_target(target, password, ctx),
            _ => {
                self.sending = Some(Sending::Password {
                    target,
                    password: String::new(),
                })
            }
        }
    }

    /// Reads, on `target`, the rows of the keys of the applications picked, from a job.
    fn read_target(&mut self, target: Server, password: String, ctx: &mut Context) {
        let (entries, _) = entries_of(&self.applications, &self.picked);
        let keys: Vec<(String, Row)> = entries
            .iter()
            .map(|entry| (entry.table.clone(), entry.key.clone()))
            .collect();
        let (settings, secret) = (servers::settings(&target), password.clone());
        let job = ctx.spawn(
            "Read the rows of another server",
            move |_| -> Result<Vec<Vec<Row>>, String> { Writer::open(&settings, &secret, TIMEOUT)?.read(&keys) },
        );
        self.sending = Some(Sending::Reading { job, target, password });
    }

    /// The rows of the target read: compared, the differing ones shown; none, written at once.
    pub(crate) fn target_read(&mut self, outcome: Option<Result<Vec<Vec<Row>>, String>>, ctx: &mut Context) {
        let Some(Sending::Reading { target, password, .. }) = self.sending.take() else {
            return;
        };
        let there = match outcome {
            Some(Ok(there)) => there,
            Some(Err(reason)) => {
                self.said = Some((false, format!("{} is not read: {reason}", target.name)));
                return;
            }
            None => return,
        };
        let (entries, _) = entries_of(&self.applications, &self.picked);
        let differing = differing(&entries, &there);
        if differing.is_empty() {
            self.write_target(target, password, entries, &BTreeSet::new(), ctx);
        } else {
            self.sending = Some(Sending::Review {
                target,
                password,
                entries,
                differing,
            });
        }
    }

    /// Writes `entries`, those of `left` left out, on `target`, from a job.
    fn write_target(
        &mut self,
        target: Server,
        password: String,
        entries: Vec<Entry>,
        left: &BTreeSet<usize>,
        ctx: &mut Context,
    ) {
        let (_, reloads) = entries_of(&self.applications, &self.picked);
        let changes = changes_of(&entries, left);
        let settings = servers::settings(&target);
        let remember = self.remember;
        let vault = self.vault.clone();
        let job = ctx.spawn(
            "Write the changes into another server",
            move |_| -> Result<Vec<Entry>, String> {
                let made = Writer::open(&settings, &password, TIMEOUT)?.apply(&changes)?;
                if remember && let Err(reason) = vault.set(&secret::target(&settings), &settings.user, &password) {
                    log::warn!("{reason}");
                }
                Ok(made)
            },
        );
        self.sending = Some(Sending::Writing { job, target, reloads });
    }

    /// What was written on the target: kept as an application of that server, its script written,
    /// its bridge asked.
    pub(crate) fn target_written(&mut self, outcome: Option<Result<Vec<Entry>, String>>, ctx: &mut Context) {
        let Some(Sending::Writing { target, reloads, .. }) = self.sending.take() else {
            return;
        };
        let entries = match outcome {
            Some(Ok(entries)) => entries,
            Some(Err(reason)) => {
                self.said = Some((false, format!("Nothing written to {}: {reason}", target.name)));
                return;
            }
            None => return,
        };
        let when = now();
        let application = Application {
            when: when.clone(),
            server: target.name.clone(),
            database: format!("{} on {}:{}", target.database, target.host, target.port),
            owners: Vec::new(),
            entries,
            reloads: reloads.clone(),
        };
        let rows = application.rows();
        let title = format!("Sent by UniWoW on {when} to {}: {rows} rows", target.name);
        let failed = write_script_to(&target.sql_folder, &title, &when, false, &application.entries);
        self.applications.push(application);
        self.picked.clear();
        let message = format!("{rows} rows written to {}", target.name);
        log::info!("{message}");
        self.said = Some((
            failed.is_none(),
            failed.map_or(message.clone(), |failed| format!("{message}; {failed}")),
        ));
        let demands = bridge::demands(&reloads);
        if !demands.is_empty() {
            let (port, token) = (target.bridge_port, target.bridge_token.clone());
            self.bridging = Some(ctx.spawn("Ask another server to load the changes again", move |_| {
                bridge::reload(port, &token, &demands)
            }));
        }
    }

    /// The windows of a sending: the password of the target, the rows that differ there.
    pub(crate) fn sending_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        match self.sending.take() {
            Some(Sending::Password { target, mut password }) => {
                let (mut go, mut cancel) = (false, false);
                egui::Window::new("Password of another server")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(egui, |ui| {
                        ui.label(format!(
                            "The password of {} for {} on {}:{} ({}).",
                            target.user, target.database, target.host, target.port, target.name
                        ));
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut password)
                                .password(true)
                                .hint_text("Password"),
                        );
                        go = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.checkbox(&mut self.remember, "Remember password")
                            .on_hover_text("Kept in the Credential Manager of Windows once the changes are written");
                        ui.horizontal(|ui| {
                            go |= ui
                                .button("Send")
                                .on_hover_text("Reads the rows there first; writes nothing before you choose")
                                .clicked();
                            cancel = ui
                                .button("Cancel")
                                .on_hover_text("Nothing read, nothing written")
                                .clicked();
                        });
                    });
                if go {
                    self.read_target(target, password, ctx);
                } else if !cancel {
                    self.sending = Some(Sending::Password { target, password });
                }
            }
            Some(Sending::Review {
                target,
                password,
                entries,
                differing,
            }) => {
                let (mut overwrite, mut leave, mut cancel) = (false, false, false);
                egui::Window::new(format!("Send to {}", target.name))
                    .collapsible(false)
                    .default_width(480.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(egui, |ui| {
                        ui.label(format!(
                            "{} of the {} rows to write are not, on {}, what they were here before the changes: \
                             changed there since, or never alike.",
                            differing.len(),
                            entries.len(),
                            target.name
                        ));
                        egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                            for at in &differing {
                                let entry = &entries[*at];
                                ui.label(
                                    egui::RichText::new(format!("{} {}", entry.table, json!(entry.key)))
                                        .monospace()
                                        .small(),
                                );
                            }
                        });
                        ui.horizontal(|ui| {
                            overwrite = ui
                                .button("Overwrite them")
                                .on_hover_text("Writes every row there as it is here, these ones too; one transaction")
                                .clicked();
                            leave = ui
                                .button("Leave them as they are there")
                                .on_hover_text("Writes the others alone; these keep what the other server has")
                                .clicked();
                            cancel = ui.button("Cancel").on_hover_text("Nothing written").clicked();
                        });
                    });
                if overwrite {
                    self.write_target(target, password, entries, &BTreeSet::new(), ctx);
                } else if leave {
                    let left: BTreeSet<usize> = differing.into_iter().collect();
                    self.write_target(target, password, entries, &left, ctx);
                } else if !cancel {
                    self.sending = Some(Sending::Review {
                        target,
                        password,
                        entries,
                        differing,
                    });
                }
            }
            other => self.sending = other,
        }
    }

    /// The applications of the project, picked to be sent to another server.
    pub(crate) fn applications_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        if self.applications.is_empty() {
            return;
        }
        let chosen = crate::server_of(ctx);
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Applications of the project").weak());
        egui::ScrollArea::vertical()
            .id_salt("world-db applications")
            .max_height(140.0)
            .show(ui, |ui| {
                for (at, application) in self.applications.iter().enumerate().rev() {
                    let mut picked = self.picked.contains(&at);
                    let server = if application.server.is_empty() {
                        "?"
                    } else {
                        application.server.as_str()
                    };
                    if ui
                        .checkbox(
                            &mut picked,
                            format!("{} · {server} · {} rows", application.when, application.rows()),
                        )
                        .on_hover_text("Picked to be sent to another server or exported to a file; writes nothing")
                        .changed()
                    {
                        if picked {
                            self.picked.insert(at);
                        } else {
                            self.picked.remove(&at);
                        }
                    }
                }
            });
        let others: Vec<Server> = crate::servers_of(ctx)
            .into_iter()
            .filter(|server| server.name != chosen.name)
            .collect();
        ui.horizontal(|ui| {
            let busy = self.sending.is_some();
            for target in others {
                if ui
                    .add_enabled(
                        !self.picked.is_empty() && !busy,
                        egui::Button::new(format!("Send to {}…", target.name)),
                    )
                    .on_hover_text(
                        "Writes there the rows the applications picked made here, its rows read and compared first",
                    )
                    .clicked()
                {
                    self.send(target, ctx);
                }
            }
            if busy {
                ui.spinner();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::db::write::{Memory, apply, row};

    #[test]
    fn an_application_is_sent_as_it_was_made_the_rows_that_differ_there_shown_and_left_if_asked() {
        let before = row([("guid", json!(1)), ("position_x", json!(0.0))]);
        let after = row([("guid", json!(1)), ("position_x", json!(5.0))]);
        let made = Entry {
            table: "creature".to_owned(),
            key: row([("guid", json!(1))]),
            before: vec![before.clone()],
            after: vec![after.clone()],
        };
        let created = Entry {
            table: "creature".to_owned(),
            key: row([("guid", json!(2))]),
            before: Vec::new(),
            after: vec![row([("guid", json!(2)), ("position_x", json!(7.0))])],
        };
        let application = Application {
            when: "2026-10-21 10:00:00".to_owned(),
            server: "Test".to_owned(),
            database: "acore_world_test".to_owned(),
            owners: Vec::new(),
            entries: vec![made.clone(), created.clone()],
            reloads: vec![Reload::Creature(1), Reload::Creature(2)],
        };
        let (entries, reloads) = entries_of(&[application], &BTreeSet::from([0]));
        assert_eq!(entries.len(), 2);
        assert_eq!(reloads.len(), 2);
        // There, the first as it was here before; the second's guid taken by another.
        let there = vec![
            vec![before.clone()],
            vec![row([("guid", json!(2)), ("position_x", json!(1.0))])],
        ];
        assert_eq!(differing(&entries, &there), [1]);
        let mut target = Memory::default();
        target
            .tables
            .insert("creature".to_owned(), vec![before, there[1][0].clone()]);
        let mut left = target.clone();
        apply(&mut left, &changes_of(&entries, &BTreeSet::from([1]))).unwrap();
        assert!(left.tables["creature"].contains(&after));
        assert!(left.tables["creature"].contains(&there[1][0]), "left as it was there");
        apply(&mut target, &changes_of(&entries, &BTreeSet::new())).unwrap();
        assert!(target.tables["creature"].contains(&created.after[0]), "overwritten");
        assert_eq!(target.tables["creature"].len(), 2);
    }
}
