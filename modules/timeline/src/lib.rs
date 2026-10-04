//! The Timeline, in Animation mode, as the Animation window of Unity: keys on the animatable
//! properties the modules declare, a dopesheet, a playhead whose values are written to the
//! properties as it moves, and playback. Sequences are JSON files in `sequences\` beside the
//! executable.

mod panel;
mod sequence;
#[cfg(test)]
mod testing;

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use uniwow_api::serde_json;
use uniwow_api::{
    CallId, Command, Context, DIALOG_ANSWERED_TOPIC, DockArea, Editor, Event, Module, Registrar, egui, log,
};

use sequence::{KeyId, Sequence};

/// A sequence opened during this session.
struct Document {
    sequence: Sequence,
    /// Changed since it was last saved or read.
    dirty: bool,
}

/// A question about unsaved changes, asked before showing another sequence.
struct Question {
    /// The call opening its window, until it answers with the window's number.
    call: CallId,
    dialog: Option<u64>,
    /// The sequence to show then.
    target: String,
}

/// Playback under way: the frame it started from, and when.
struct Playing {
    from: f64,
    since: Instant,
}

#[derive(Default)]
struct TimelineModule {
    editor: Option<Editor>,
    folder: PathBuf,
    /// The sequences of the folder, by name.
    names: Vec<String>,
    /// The sequences opened during this session, by name.
    documents: BTreeMap<String, Document>,
    current: Option<String>,
    /// The frame at the playhead; fractional while playing.
    playhead: f64,
    playing: Option<Playing>,
    looping: bool,
    /// Set when the keys change: the values at the playhead are then written again.
    keys_changed: bool,
    /// The frame whose values were written last.
    written: Option<f64>,
    selection: BTreeSet<KeyId>,
    question: Option<Question>,
    panel: panel::State,
}

impl Module for TimelineModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("timeline", "Timeline", DockArea::Bottom)
            .subscribe(DIALOG_ANSWERED_TOPIC);
    }

    fn init(&mut self, ctx: &mut Context) {
        self.editor = Some(ctx.editor());
        self.folder = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("sequences")))
            .unwrap_or_else(|| PathBuf::from("sequences"));
        if let Err(error) = std::fs::create_dir_all(&self.folder) {
            log::warn!("{}: {error}", self.folder.display());
        }
        self.refresh_names();
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        self.advance(ui.ctx());
        self.write_values();
        panel::show(self, ui, ctx);
        // What the panel changed shows in the 3D view at once.
        self.write_values();
    }

    fn on_reply(&mut self, call: CallId, result: Result<serde_json::Value, String>, _ctx: &mut Context) {
        let Some(question) = self.question.as_mut().filter(|q| q.call == call) else {
            return;
        };
        match result.ok().and_then(|answer| answer["dialog"].as_u64()) {
            Some(dialog) => question.dialog = Some(dialog),
            None => {
                self.question = None;
                self.panel_message("the question about the unsaved changes could not be asked");
            }
        }
    }

    fn on_event(&mut self, event: &Event, ctx: &mut Context) {
        let answered = self
            .question
            .as_ref()
            .is_some_and(|q| q.dialog.is_some() && q.dialog == event.payload["dialog"].as_u64());
        if answered && let Some(button) = event.payload["button"].as_str() {
            self.answered(button, ctx);
        }
    }

    fn unsaved(&self) -> Vec<String> {
        self.documents
            .iter()
            .filter(|(_, document)| document.dirty)
            .map(|(name, _)| format!("Sequence '{name}'"))
            .collect()
    }

    fn save_unsaved(&mut self, _ctx: &mut Context) -> Result<(), String> {
        let names: Vec<String> = self
            .documents
            .iter()
            .filter(|(_, document)| document.dirty)
            .map(|(name, _)| name.clone())
            .collect();
        let failures: Vec<String> = names.iter().filter_map(|name| self.save(name).err()).collect();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

impl TimelineModule {
    /// Does what the user chose about the unsaved changes of the sequence shown, then shows the
    /// one asked for, unless the user cancelled.
    fn answered(&mut self, button: &str, ctx: &mut Context) {
        let Some(question) = self.question.take() else {
            return;
        };
        let current = self.current.clone().unwrap_or_default();
        match button {
            "save" => {
                if let Err(error) = self.save(&current) {
                    self.panel_message(&error);
                    return;
                }
            }
            "discard" => self.discard(&current, ctx),
            _ => return,
        }
        panel::open(self, &question.target);
    }

    /// Drops the unsaved changes of the sequence `name`: Undo would otherwise bring them back.
    fn discard(&mut self, name: &str, ctx: &mut Context) {
        self.documents.remove(name);
        ctx.forget_document(name);
    }

    fn panel_message(&mut self, message: &str) {
        log::warn!("{message}");
        self.panel.say(message);
    }

    fn sequence(&self) -> Option<&Sequence> {
        self.current
            .as_ref()
            .and_then(|name| self.documents.get(name))
            .map(|document| &document.sequence)
    }

    fn refresh_names(&mut self) {
        let mut names: Vec<String> = std::fs::read_dir(&self.folder)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().is_some_and(|e| e == "json"))
                    .filter_map(|path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        self.names = names;
    }

    fn path(&self, name: &str) -> PathBuf {
        self.folder.join(format!("{name}.json"))
    }

    /// Shows the sequence `name`, read from its file unless it was opened already.
    fn open(&mut self, name: &str) -> Result<(), String> {
        if !self.documents.contains_key(name) {
            let path = self.path(name);
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            let sequence = Sequence::from_json(&value).map_err(|e| format!("{}: {e}", path.display()))?;
            self.documents
                .insert(name.to_owned(), Document { sequence, dirty: false });
        }
        self.current = Some(name.to_owned());
        self.playing = None;
        self.playhead = 0.0;
        self.selection.clear();
        self.keys_changed = true;
        self.panel.fitted = false;
        Ok(())
    }

    /// Writes the sequence `name` to its file, through a temporary file so that a failure leaves
    /// the previous one whole.
    fn save(&mut self, name: &str) -> Result<(), String> {
        let path = self.path(name);
        let document = self.documents.get_mut(name).ok_or("nothing to save")?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, document.sequence.to_text())
            .and_then(|()| std::fs::rename(&temporary, &path))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        document.dirty = false;
        self.refresh_names();
        Ok(())
    }

    /// Creates an empty sequence and its file.
    fn create(&mut self, name: &str) -> Result<(), String> {
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_');
        if !valid {
            return Err("a name is made of letters, digits, spaces, '-' and '_'".to_owned());
        }
        // Windows does not tell names apart by their case: nor does the list.
        self.refresh_names();
        if let Some(existing) = self.names.iter().find(|existing| existing.eq_ignore_ascii_case(name)) {
            return Err(format!("'{existing}' exists already"));
        }
        let sequence = Sequence::default();
        let path = self.path(name);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| file.write_all(sequence.to_text().as_bytes()))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.documents
            .insert(name.to_owned(), Document { sequence, dirty: false });
        self.refresh_names();
        Ok(())
    }

    /// Moves the playhead while playing.
    fn advance(&mut self, ctx: &egui::Context) {
        let (Some(playing), Some(sequence)) = (&self.playing, self.sequence()) else {
            self.playing = None;
            return;
        };
        let length = f64::from(sequence.length);
        let mut frame = playing.from + playing.since.elapsed().as_secs_f64() * f64::from(sequence.frame_rate);
        if frame >= length {
            if self.looping {
                frame %= length;
            } else {
                frame = length;
                self.playing = None;
            }
        }
        self.playhead = frame;
        if self.playing.is_some() {
            ctx.request_repaint();
        }
    }

    fn play(&mut self) {
        let Some(length) = self.sequence().map(|s| f64::from(s.length)) else {
            return;
        };
        if self.playhead >= length {
            self.playhead = 0.0;
        }
        self.playing = Some(Playing {
            from: self.playhead,
            since: Instant::now(),
        });
    }

    /// Writes the values at the playhead to the properties, when the playhead or the keys moved.
    fn write_values(&mut self) {
        if self.written == Some(self.playhead) && !self.keys_changed {
            return;
        }
        self.written = Some(self.playhead);
        self.keys_changed = false;
        let (Some(editor), Some(sequence)) = (&self.editor, self.sequence()) else {
            return;
        };
        for track in &sequence.tracks {
            // A number without keys keeps the value the property has.
            let current = if track.has_bare_number() {
                editor.read_property(&track.property).ok()
            } else {
                None
            };
            if let Some(value) = track.evaluate(self.playhead, current) {
                // A property no module declares now is shown greyed; there is nothing to write.
                let _ = editor.write_property(&track.property, value);
            }
        }
    }

    /// Replaces the shown sequence by `after`, as one undo entry.
    fn edit(&mut self, ctx: &mut Context, label: &str, after: Sequence) {
        if let Some(name) = self.current.clone() {
            ctx.execute(SequenceEdit {
                name,
                label: label.to_owned(),
                after,
                before: None,
            });
        }
    }
}

/// A change of a sequence, as one undo entry: the sequence it replaces is read when applied (see
/// `Command`).
struct SequenceEdit {
    name: String,
    label: String,
    after: Sequence,
    before: Option<Sequence>,
}

impl SequenceEdit {
    /// Replaces the sequence and returns the one it replaced; nothing for a sequence closed
    /// without saving, which an edit never opens again.
    fn replace(&self, module: &mut dyn Any, sequence: Sequence) -> Option<Sequence> {
        let timeline: &mut TimelineModule = module
            .downcast_mut()
            .expect("commands of this module are applied to it");
        let document = timeline.documents.get_mut(&self.name)?;
        document.dirty = true;
        let before = std::mem::replace(&mut document.sequence, sequence);
        timeline.keys_changed = true;
        Some(before)
    }
}

impl Command for SequenceEdit {
    fn label(&self) -> String {
        format!("timeline: {}", self.label)
    }

    fn apply(&mut self, module: &mut dyn Any) {
        self.before = self.replace(module, self.after.clone());
    }

    fn revert(&mut self, module: &mut dyn Any) {
        if let Some(before) = self.before.clone() {
            self.replace(module, before);
        }
    }

    fn document(&self) -> Option<String> {
        Some(self.name.clone())
    }
}

uniwow_api::export_module!(TimelineModule::default());

#[cfg(test)]
mod tests {
    use uniwow_api::{Command, Module, PropertyKind};

    use super::{Document, SequenceEdit, TimelineModule};
    use crate::sequence::{Sequence, Track};
    use crate::testing::FakeHost;

    #[test]
    fn an_edit_is_undone_and_redone_and_marks_the_sequence() {
        let mut timeline = TimelineModule::default();
        timeline.documents.insert(
            "intro".to_owned(),
            Document {
                sequence: Sequence::default(),
                dirty: false,
            },
        );
        let mut after = Sequence::default();
        after.tracks.push(Track::new("cube/scale", PropertyKind::Vector));
        let mut edit = SequenceEdit {
            name: "intro".to_owned(),
            label: "add a track".to_owned(),
            after: after.clone(),
            before: None,
        };
        edit.apply(&mut timeline);
        assert_eq!(timeline.documents["intro"].sequence, after);
        assert!(timeline.documents["intro"].dirty && timeline.keys_changed);
        edit.revert(&mut timeline);
        assert_eq!(timeline.documents["intro"].sequence, Sequence::default());
        assert_eq!(edit.label(), "timeline: add a track");
    }

    #[test]
    fn an_edit_of_a_sequence_closed_without_saving_does_not_open_it_again() {
        let mut timeline = TimelineModule::default();
        let mut after = Sequence::default();
        after.tracks.push(Track::new("cube/scale", PropertyKind::Vector));
        let mut edit = SequenceEdit {
            name: "intro".to_owned(),
            label: "add a track".to_owned(),
            after,
            before: None,
        };
        assert_eq!(edit.document().as_deref(), Some("intro"));
        edit.apply(&mut timeline);
        edit.revert(&mut timeline);
        assert!(timeline.documents.is_empty() && !timeline.keys_changed);
    }

    #[test]
    fn leaving_an_unsaved_sequence_without_a_window_to_ask_in_forgets_its_changes() {
        let folder = std::env::temp_dir().join(format!("uniwow-timeline-leave-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("outro.json"), Sequence::default().to_text()).unwrap();
        let mut timeline = TimelineModule {
            folder: folder.clone(),
            current: Some("intro".to_owned()),
            ..TimelineModule::default()
        };
        timeline.documents.insert(
            "intro".to_owned(),
            Document {
                sequence: Sequence::default(),
                dirty: true,
            },
        );
        let mut host = FakeHost::default();
        crate::panel::switch(&mut timeline, &mut host.context(), "outro".to_owned(), true);
        assert_eq!(host.forgotten, vec!["intro".to_owned()]);
        assert!(!timeline.documents.contains_key("intro"));
        assert_eq!(timeline.current.as_deref(), Some("outro"));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_new_sequence_never_replaces_a_file_whose_name_differs_by_its_case() {
        let folder = std::env::temp_dir().join(format!("uniwow-timeline-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Intro.json"), "kept").unwrap();
        let mut timeline = TimelineModule {
            folder: folder.clone(),
            ..TimelineModule::default()
        };
        assert!(timeline.create("intro").is_err());
        assert_eq!(std::fs::read_to_string(folder.join("Intro.json")).unwrap(), "kept");
        assert!(timeline.create("outro").is_ok());
        assert!(timeline.names.contains(&"outro".to_owned()));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn its_unsaved_documents_are_its_sequences_with_unsaved_changes() {
        let mut timeline = TimelineModule::default();
        for (name, dirty) in [("intro", true), ("outro", false)] {
            timeline.documents.insert(
                name.to_owned(),
                Document {
                    sequence: Sequence::default(),
                    dirty,
                },
            );
        }
        assert_eq!(timeline.unsaved(), vec!["Sequence 'intro'".to_owned()]);
    }
}
