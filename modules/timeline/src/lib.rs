//! The Timeline, in Animation mode, as the Animation window of Unity: sequences of keys on the
//! animatable properties the modules declare, kept in readable JSON files in `sequences\` beside
//! the executable. Its sequences, their playback, the dopesheet and the Curves view are objects of
//! the core (section 3): a `Sequence` per sequence opened, a `Player`, a `DopesheetView` and a
//! `CurveView`. The module keeps the files and its bars.

mod panel;
#[cfg(test)]
mod testing;

use std::any::Any;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use uniwow_api::hotkey::{Hotkey, HotkeyKind, Keys};
use uniwow_api::sequence::Sequence;
use uniwow_api::serde_json;
use uniwow_api::ui::{self, Handle, Kind, Property, SharedUi, Ui};
use uniwow_api::{
    CallId, Command, Context, DIALOG_ANSWERED_TOPIC, DockArea, Editor, Event, Module, Registrar, egui, log,
};

/// The panel of the objects, which holds the dopesheet and the Curves view.
const VIEWS: &str = "views";
/// The keys that play or pause, by default.
const PLAY: Keys = Keys::key(egui::Key::Space);

/// A sequence opened during this session: its object, and what its file holds.
struct Document {
    sequence: Handle,
    saved: Sequence,
}

/// A question about unsaved changes, asked before showing another sequence.
struct Question {
    /// The call opening its window, until it answers with the window's number.
    call: CallId,
    dialog: Option<u64>,
    /// The sequence to show then.
    target: String,
}

/// The objects the Timeline shows and plays its sequences with.
struct Objects {
    store: SharedUi,
    player: Handle,
    dopesheet: Handle,
    curves: Handle,
}

impl Objects {
    fn new() -> Self {
        // The Timeline connects no slot: no job reaches its objects.
        let store = Ui::new(Arc::new(|_job| {}));
        let (player, dopesheet, curves) = {
            let mut objects = ui::lock(&store);
            let made = (|| {
                let player = objects.create(Kind::Player, None)?;
                let dopesheet = objects.create(Kind::DopesheetView, None)?;
                let curves = objects.create(Kind::CurveView, None)?;
                let layout = objects.create(Kind::VBoxLayout, None)?;
                for view in [dopesheet, curves] {
                    objects.set_numbers(view, Property::Player, &[player as f64])?;
                    objects.add_to(layout, view, [0, 0, 1, 1])?;
                }
                objects.set_numbers(curves, Property::Visible, &[0.0])?;
                let panel = objects.panel(VIEWS);
                objects.add_to(panel, layout, [0, 0, 1, 1])?;
                Ok::<_, String>((player, dopesheet, curves))
            })();
            made.unwrap_or_else(|error| {
                log::error!("the Timeline's objects could not be made: {error}");
                (0, 0, 0)
            })
        };
        Self {
            store,
            player,
            dopesheet,
            curves,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Ui> {
        ui::lock(&self.store)
    }

    /// The player's time, whether it plays, and whether it loops.
    fn playback(&self) -> (f64, bool, bool) {
        let objects = self.lock();
        let number = |property| {
            objects
                .numbers(self.player, property)
                .ok()
                .and_then(|numbers| numbers.first().copied())
                .unwrap_or(0.0)
        };
        (
            number(Property::Time),
            number(Property::Playing) != 0.0,
            number(Property::Loop) != 0.0,
        )
    }

    fn set(&self, object: Handle, property: Property, value: f64) {
        if let Err(error) = self.lock().set_numbers(object, property, &[value]) {
            log::warn!("{error}");
        }
    }

    /// The playhead at `frame`, paused.
    fn seek(&self, frame: f64) {
        self.set(self.player, Property::Playing, 0.0);
        self.set(self.player, Property::Time, frame);
    }

    fn toggle_playback(&self) {
        let (_, playing, _) = self.playback();
        self.set(self.player, Property::Playing, if playing { 0.0 } else { 1.0 });
    }

    /// The sequence the views show, under its name, and the player plays, from frame 0.
    fn show(&self, sequence: Handle, name: &str) {
        for object in [self.player, self.dopesheet, self.curves] {
            self.set(object, Property::Sequence, sequence as f64);
        }
        for view in [self.dopesheet, self.curves] {
            if let Err(error) = self.lock().set_text(view, Property::Title, name) {
                log::warn!("{error}");
            }
        }
        self.seek(0.0);
    }

    /// The dopesheet shown, or the Curves view.
    fn show_curves(&self, curves: bool) {
        self.set(self.dopesheet, Property::Visible, if curves { 0.0 } else { 1.0 });
        self.set(self.curves, Property::Visible, if curves { 1.0 } else { 0.0 });
    }
}

struct TimelineModule {
    editor: Option<Editor>,
    folder: PathBuf,
    /// The sequences of the folder, by name.
    names: Vec<String>,
    /// The sequences opened during this session, by name.
    documents: BTreeMap<String, Document>,
    current: Option<String>,
    objects: Objects,
    question: Option<Question>,
    panel: panel::State,
    play: Hotkey,
}

impl Default for TimelineModule {
    fn default() -> Self {
        Self {
            editor: None,
            folder: PathBuf::new(),
            names: Vec::new(),
            documents: BTreeMap::new(),
            current: None,
            objects: Objects::new(),
            question: None,
            panel: panel::State::default(),
            play: Hotkey::new(HotkeyKind::Press, PLAY),
        }
    }
}

impl Module for TimelineModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("timeline", "Timeline", DockArea::Bottom)
            .subscribe(DIALOG_ANSWERED_TOPIC);
        self.play = reg.hotkey("play", "Play or pause", HotkeyKind::Press, PLAY);
    }

    fn init(&mut self, ctx: &mut Context) {
        self.editor = Some(ctx.editor());
        ctx.adopt_objects(&self.objects.store);
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
        panel::show(self, ui, ctx);
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
            .keys()
            .filter(|name| self.dirty(name))
            .map(|name| format!("Sequence '{name}'"))
            .collect()
    }

    fn save_unsaved(&mut self, _ctx: &mut Context) -> Result<(), String> {
        let names: Vec<String> = self.documents.keys().filter(|name| self.dirty(name)).cloned().collect();
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

    /// Drops the unsaved changes of the sequence `name`: its object, and its changes, which Undo
    /// would otherwise bring back.
    fn discard(&mut self, name: &str, ctx: &mut Context) {
        if let Some(document) = self.documents.remove(name) {
            let _ = self.objects.lock().destroy(document.sequence);
        }
        ctx.forget_document(name);
    }

    fn panel_message(&mut self, message: &str) {
        log::warn!("{message}");
        self.panel.say(message);
    }

    /// What the sequence `name` holds now.
    fn data(&self, name: &str) -> Option<Arc<Sequence>> {
        let document = self.documents.get(name)?;
        self.objects.lock().sequence(document.sequence).ok()
    }

    /// The sequence shown.
    fn sequence(&self) -> Option<Arc<Sequence>> {
        self.data(self.current.as_ref()?)
    }

    /// Whether the sequence `name` differs from its file.
    fn dirty(&self, name: &str) -> bool {
        let saved = self.documents.get(name).map(|document| &document.saved);
        self.data(name).is_some_and(|data| Some(&*data) != saved)
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

    /// The object of the sequence `name`, made from `sequence` and named after its document.
    fn adopt(&mut self, name: &str, sequence: Sequence) -> Result<(), String> {
        let handle = {
            let mut objects = self.objects.lock();
            let handle = objects.create_sequence(sequence.clone())?;
            objects.set_text(handle, Property::Title, name)?;
            handle
        };
        self.documents.insert(
            name.to_owned(),
            Document {
                sequence: handle,
                saved: sequence,
            },
        );
        Ok(())
    }

    /// Shows the sequence `name`, read from its file unless it was opened already. A frame rate or
    /// a length being dragged is put back.
    fn open(&mut self, name: &str) -> Result<(), String> {
        panel::cancel_editing(self);
        if !self.documents.contains_key(name) {
            let path = self.path(name);
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            let sequence = Sequence::from_json(&value).map_err(|e| format!("{}: {e}", path.display()))?;
            self.adopt(name, sequence)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        self.current = Some(name.to_owned());
        if let Some(document) = self.documents.get(name) {
            self.objects.show(document.sequence, name);
        }
        Ok(())
    }

    /// Writes the sequence `name` to its file, through a temporary file so that a failure leaves
    /// the previous one whole.
    fn save(&mut self, name: &str) -> Result<(), String> {
        let path = self.path(name);
        let data = self.data(name).ok_or("nothing to save")?;
        // A file the Timeline could not read back is never written.
        data.check()
            .map_err(|error| format!("'{name}' is not saved: {error}"))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, data.to_text())
            .and_then(|()| std::fs::rename(&temporary, &path))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(document) = self.documents.get_mut(name) {
            document.saved = (*data).clone();
        }
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
        self.adopt(name, sequence)?;
        self.refresh_names();
        Ok(())
    }

    /// Sets the frame rate or the length of the sequence `name`, returning the value it had; nothing
    /// for a sequence closed without saving, which a change never opens again.
    fn set_number(&mut self, name: &str, property: Property, value: f64) -> Option<f64> {
        let sequence = self.documents.get(name)?.sequence;
        let mut objects = self.objects.lock();
        let before = objects.numbers(sequence, property).ok()?.first().copied()?;
        objects.set_numbers(sequence, property, &[value]).ok()?;
        Some(before)
    }
}

/// A change of a sequence's frame rate or length, as one undo entry: the value it replaces is read
/// when applied (see `Command`).
struct NumberEdit {
    name: String,
    label: String,
    property: Property,
    after: f64,
    before: Option<f64>,
}

impl NumberEdit {
    fn timeline(module: &mut dyn Any) -> &mut TimelineModule {
        module
            .downcast_mut()
            .expect("commands of this module are applied to it")
    }
}

impl Command for NumberEdit {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn Any) {
        self.before = Self::timeline(module).set_number(&self.name, self.property, self.after);
    }

    fn revert(&mut self, module: &mut dyn Any) {
        if let Some(before) = self.before {
            Self::timeline(module).set_number(&self.name, self.property, before);
        }
    }

    fn document(&self) -> Option<String> {
        Some(self.name.clone())
    }
}

uniwow_api::export_module!(TimelineModule::default());

#[cfg(test)]
mod tests {
    use uniwow_api::sequence::{Sequence, Track};
    use uniwow_api::ui::Property;
    use uniwow_api::{Command, Module, PropertyKind};

    use super::{NumberEdit, TimelineModule};
    use crate::testing::FakeHost;

    /// A Timeline over a folder of its own, made for the test `name`.
    fn timeline(name: &str) -> (TimelineModule, std::path::PathBuf) {
        let folder = std::env::temp_dir().join(format!("uniwow-timeline-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let timeline = TimelineModule {
            folder: folder.clone(),
            ..TimelineModule::default()
        };
        (timeline, folder)
    }

    fn frame_rate(timeline: &TimelineModule, name: &str) -> u32 {
        timeline.data(name).unwrap().frame_rate
    }

    #[test]
    fn a_sequence_read_from_its_file_is_shown_unchanged_and_no_change_to_undo() {
        let (mut timeline, folder) = timeline("open");
        let mut sequence = Sequence::default();
        sequence.tracks.push(Track::new("cube/scale", PropertyKind::Vector));
        std::fs::write(folder.join("intro.json"), sequence.to_text()).unwrap();
        timeline.open("intro").unwrap();
        assert_eq!(*timeline.sequence().unwrap(), sequence);
        assert!(!timeline.dirty("intro"));
        let (time, playing, _) = timeline.objects.playback();
        assert!(time == 0.0 && !playing, "shown from frame 0, paused");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_change_of_frame_rate_is_undone_and_redone_and_marks_the_sequence() {
        let (mut timeline, folder) = timeline("rate");
        timeline.create("intro").unwrap();
        let mut edit = NumberEdit {
            name: "intro".to_owned(),
            label: "frame rate".to_owned(),
            property: Property::FrameRate,
            after: 24.0,
            before: None,
        };
        edit.apply(&mut timeline);
        assert_eq!(frame_rate(&timeline, "intro"), 24);
        assert!(timeline.dirty("intro"));
        edit.revert(&mut timeline);
        assert_eq!(frame_rate(&timeline, "intro"), 30);
        assert!(!timeline.dirty("intro"), "back as saved");
        assert_eq!(edit.document().as_deref(), Some("intro"));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_change_of_a_sequence_closed_without_saving_does_not_open_it_again() {
        let mut timeline = TimelineModule::default();
        let mut edit = NumberEdit {
            name: "intro".to_owned(),
            label: "length".to_owned(),
            property: Property::Length,
            after: 60.0,
            before: None,
        };
        edit.apply(&mut timeline);
        edit.revert(&mut timeline);
        assert!(timeline.documents.is_empty());
    }

    #[test]
    fn leaving_an_unsaved_sequence_without_a_window_to_ask_in_forgets_it_and_its_changes() {
        let (mut timeline, folder) = timeline("leave");
        std::fs::write(folder.join("outro.json"), Sequence::default().to_text()).unwrap();
        timeline.create("intro").unwrap();
        timeline.open("intro").unwrap();
        let sequence = timeline.documents["intro"].sequence;
        timeline.set_number("intro", Property::Length, 60.0);
        let mut host = FakeHost::default();
        crate::panel::switch(&mut timeline, &mut host.context(), "outro".to_owned(), true);
        assert_eq!(host.forgotten, vec!["intro".to_owned()]);
        assert!(!timeline.documents.contains_key("intro"));
        assert!(
            timeline.objects.lock().object(sequence).is_none(),
            "its object destroyed"
        );
        assert_eq!(timeline.current.as_deref(), Some("outro"));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_saved_sequence_is_its_file_and_no_more_unsaved() {
        let (mut timeline, folder) = timeline("save");
        timeline.create("intro").unwrap();
        timeline.set_number("intro", Property::Length, 60.0);
        assert_eq!(timeline.unsaved(), vec!["Sequence 'intro'".to_owned()]);
        timeline.save("intro").unwrap();
        assert!(timeline.unsaved().is_empty());
        let text = std::fs::read_to_string(folder.join("intro.json")).unwrap();
        assert!(text.contains("\"length\": 60"), "{text}");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_frame_rate_dragged_then_dropped_puts_the_sequence_back_as_it_was() {
        let (mut timeline, folder) = timeline("drop");
        timeline.create("intro").unwrap();
        timeline.open("intro").unwrap();
        crate::panel::change_live(&mut timeline, "frame rate", Property::FrameRate, 24.0);
        assert_eq!(frame_rate(&timeline, "intro"), 24);
        crate::panel::cancel_editing(&mut timeline);
        assert_eq!(frame_rate(&timeline, "intro"), 30);
        assert!(!timeline.dirty("intro"));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_new_sequence_never_replaces_a_file_whose_name_differs_by_its_case() {
        let (mut timeline, folder) = timeline("case");
        std::fs::write(folder.join("Intro.json"), "kept").unwrap();
        assert!(timeline.create("intro").is_err());
        assert_eq!(std::fs::read_to_string(folder.join("Intro.json")).unwrap(), "kept");
        assert!(timeline.create("outro").is_ok());
        assert!(timeline.names.contains(&"outro".to_owned()));
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
