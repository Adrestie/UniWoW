//! The shell run without a window, as eframe runs it, on input made up here and with modules
//! defined here: the checks of the milestones that need no eye.

use std::any::Any;
use std::sync::Mutex;

use uniwow_api::Command;
use uniwow_api::egui::{Key, Modifiers, RawInput, ViewportCommand, ViewportEvent, ViewportId};
use uniwow_api::hotkey::{Hotkey, HotkeyKind, Keys};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::ui::{self, Kind, Property, SharedUi, Signal, SignalData, Ui};

use super::*;
use crate::compiled::CompiledModule;
use crate::random::Random;

type Jobs = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

struct Harness {
    shell: Shell,
    ctx: egui::Context,
}

impl Harness {
    fn new(modules: Vec<(&str, Box<dyn Module>)>) -> Self {
        Self::with_slots(
            modules
                .into_iter()
                .map(|(id, module)| Slot::loaded(id, module))
                .collect(),
        )
    }

    /// The modules start as in the editor; the settings stay in memory.
    fn with_slots(slots: Vec<Slot>) -> Self {
        Self::on_gpu(slots, None)
    }

    /// As `with_slots`, on a GPU of `gpu_memory` bytes of its own.
    fn on_gpu(slots: Vec<Slot>, gpu_memory: Option<u64>) -> Self {
        let settings = Settings {
            in_memory: true,
            ..Settings::default()
        };
        Self::started(slots, gpu_memory, settings)
    }

    /// As `with_slots`, with `settings` as the editor found them, kept in memory.
    fn with_settings(slots: Vec<Slot>, settings: Settings) -> Self {
        Self::started(
            slots,
            None,
            Settings {
                in_memory: true,
                ..settings
            },
        )
    }

    fn started(slots: Vec<Slot>, gpu_memory: Option<u64>, settings: Settings) -> Self {
        let ctx = egui::Context::default();
        let (bridge, requests) = Bridge::new(Some(ctx.clone()));
        let pool = Pool::new(2, Some(ctx.clone()), Some(bridge.clone()));
        let mut host = KernelHost::new(None, settings, pool, bridge);
        host.gpu_memory = gpu_memory;
        let shell = Shell::start(host, requests, slots, None, PathBuf::new());
        Self { shell, ctx }
    }

    /// A frame with the window shown: `logic` then `ui`. Returns the commands sent to the window.
    fn frame(&mut self, input: RawInput) -> Vec<ViewportCommand> {
        let shell = &mut self.shell;
        let mut output = self.ctx.run_ui(input, |ui| {
            shell.logic_pass(ui.ctx());
            shell.ui_pass(ui);
        });
        output.textures_delta.clear();
        output
            .viewport_output
            .remove(&ViewportId::ROOT)
            .map(|viewport| viewport.commands)
            .unwrap_or_default()
    }

    /// A frame as `frame`, giving all that egui gives back.
    fn frame_output(&mut self, input: RawInput) -> egui::FullOutput {
        let shell = &mut self.shell;
        let mut output = self.ctx.run_ui(input, |ui| {
            shell.logic_pass(ui.ctx());
            shell.ui_pass(ui);
        });
        output.textures_delta.clear();
        output
    }

    /// A frame with the window shown on a screen of 1280 × 800, as `frame`, at `input`'s events;
    /// the texts drawn, with their rectangles.
    fn frame_texts(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Rect)> {
        self.frame_texts_at(events, None)
    }

    /// As `frame_texts`, the window's content at `origin` on the screens when one is given.
    fn frame_texts_at(&mut self, events: Vec<egui::Event>, origin: Option<egui::Pos2>) -> Vec<(String, egui::Rect)> {
        let size = egui::vec2(1280.0, 800.0);
        let mut input = RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..RawInput::default()
        };
        if let Some(origin) = origin {
            input.viewports.entry(ViewportId::ROOT).or_default().inner_rect =
                Some(egui::Rect::from_min_size(origin, size));
        }
        let output = self.frame_output(input);
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some((
                    text.galley.text().to_owned(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
                _ => None,
            })
            .collect()
    }

    /// Clicks the text `text` drawn at the frame before: pressed in a frame, released in the next.
    fn click_text(&mut self, texts: &[(String, egui::Rect)], text: &str) {
        let at = texts
            .iter()
            .find(|(drawn, _)| drawn == text)
            .map(|(_, rect)| rect.center())
            .unwrap_or_else(|| panic!("'{text}' not drawn"));
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame_texts(vec![egui::Event::PointerMoved(at), button(true)]);
        self.frame_texts(vec![button(false)]);
    }

    /// The window minimised: eframe runs `logic` alone.
    fn minimised(&mut self, input: RawInput) -> Vec<ViewportCommand> {
        let shell = &mut self.shell;
        let output = self.ctx.run_logic(&input, |ctx| shell.logic_pass(ctx));
        output
            .viewport_commands
            .get(&ViewportId::ROOT)
            .cloned()
            .unwrap_or_default()
    }

    /// Runs frames until `done`, for what other threads do.
    fn until(&mut self, what: &str, done: impl Fn(&Shell) -> bool) {
        let started = Instant::now();
        while !done(&self.shell) {
            assert!(started.elapsed() < Duration::from_secs(10), "never: {what}");
            self.frame(RawInput::default());
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Calls a command from the interface thread, as a panel does.
    fn call(&mut self, caller: &str, name: &str, arguments: Value) -> Result<Value, String> {
        self.shell
            .run_command(caller, std::thread::current().id(), name, arguments)
    }

    /// Runs frames until the jobs posted so far on a compiled module's thread have run.
    fn settle(&mut self, thread: &'static capi::ModuleContext) {
        let (done, ran) = std::sync::mpsc::channel();
        ui::lock(&thread.ui).post_job(Box::new(move || {
            let _ = done.send(());
        }));
        self.until("the module's thread done", |_| ran.try_recv().is_ok());
    }

    fn index(&self, id: &str) -> usize {
        self.shell.running_index(id).expect("running")
    }

    /// Moves the players on to `at`, then waits for the writes on `thread`; returns whether one
    /// plays on.
    fn play_at(&mut self, at: Instant, thread: &'static capi::ModuleContext) -> bool {
        let playing = self.tick(at);
        Self::wait_for(thread);
        playing
    }

    /// Moves the players on to `at`.
    fn tick(&mut self, at: Instant) -> bool {
        let stores = self.shell.adopted_stores();
        self.shell.players.tick(&stores, at)
    }

    /// Waits for the jobs posted so far on `thread`, without a frame: a frame would move the
    /// players on by the real time.
    fn wait_for(thread: &'static capi::ModuleContext) {
        let (done, ran) = std::sync::mpsc::channel();
        ui::lock(&thread.ui).post_job(Box::new(move || {
            let _ = done.send(());
        }));
        ran.recv_timeout(Duration::from_secs(10))
            .expect("the module's thread runs");
    }
}

/// The window asked to close, shown or minimised.
fn closing(minimised: bool) -> RawInput {
    let mut input = RawInput::default();
    let viewport = input.viewports.entry(ViewportId::ROOT).or_default();
    viewport.events.push(ViewportEvent::Close);
    viewport.minimized = Some(minimised);
    input
}

/// A key pressed with Ctrl, or alone.
fn key(key: Key, ctrl: bool) -> RawInput {
    let modifiers = if ctrl {
        Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::default()
        }
    } else {
        Modifiers::default()
    };
    RawInput {
        events: vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }],
        ..RawInput::default()
    }
}

/// A number changed by its command `<id>.add` as one undo entry each time, whose command
/// `<id>.panic` panics and `<id>.discard` drops its changes, as closing a document without saving.
struct Counter {
    id: &'static str,
    value: Arc<Mutex<i64>>,
    saved: i64,
}

fn counter(id: &'static str) -> (Box<dyn Module>, Arc<Mutex<i64>>) {
    let value = Arc::new(Mutex::new(0));
    let module = Counter {
        id,
        value: value.clone(),
        saved: 0,
    };
    (Box::new(module), value)
}

impl Module for Counter {
    fn register(&mut self, reg: &mut Registrar) {
        reg.command(&format!("{}.add", self.id), "Adds to the number", json!({}), json!({}));
        reg.command(&format!("{}.panic", self.id), "Panics", json!({}), json!({}));
        reg.command(
            &format!("{}.discard", self.id),
            "Drops the changes",
            json!({}),
            json!({}),
        );
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        if name.ends_with(".panic") {
            panic!("the counter breaks");
        }
        if name.ends_with(".discard") {
            *lock(&self.value) = self.saved;
            ctx.forget_document("the number");
            return Ok(json!({}));
        }
        ctx.execute(Add(arguments["by"].as_i64().unwrap_or(1)));
        Ok(json!({}))
    }

    fn unsaved(&self) -> Vec<String> {
        if *lock(&self.value) == self.saved {
            Vec::new()
        } else {
            vec!["the number".to_owned()]
        }
    }

    fn save_unsaved(&mut self, _ctx: &mut Context) -> Result<(), String> {
        self.saved = *lock(&self.value);
        Ok(())
    }
}

struct Add(i64);

impl Command for Add {
    fn label(&self) -> String {
        format!("add {}", self.0)
    }

    fn apply(&mut self, module: &mut dyn Any) {
        let counter: &mut Counter = module.downcast_mut().expect("a counter");
        *lock(&counter.value) += self.0;
    }

    fn revert(&mut self, module: &mut dyn Any) {
        let counter: &mut Counter = module.downcast_mut().expect("a counter");
        *lock(&counter.value) -= self.0;
    }

    fn document(&self) -> Option<String> {
        Some("the number".to_owned())
    }
}

/// Offers `ui.dialog` as the module dialogs does, keeping the windows asked for: the tests answer.
struct Dialogs {
    asked: Arc<Mutex<Vec<Value>>>,
}

fn dialogs() -> (Box<dyn Module>, Arc<Mutex<Vec<Value>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    (Box::new(Dialogs { asked: asked.clone() }), asked)
}

impl Module for Dialogs {
    fn register(&mut self, reg: &mut Registrar) {
        reg.command(DIALOG_COMMAND, "Opens a window", json!({}), json!({}));
    }

    fn on_command(&mut self, _name: &str, arguments: Value, _ctx: &mut Context) -> Result<Value, String> {
        let mut asked = lock(&self.asked);
        asked.push(arguments);
        Ok(json!({ "dialog": asked.len() }))
    }
}

impl Harness {
    /// The user's answer in the window `dialog` of the module `dialogs`.
    fn answer(&mut self, dialog: usize, button: &str) {
        self.shell.host.publish(
            "dialogs",
            DIALOG_ANSWERED_TOPIC,
            json!({ "dialog": dialog, "button": button }),
        );
    }
}

/// A dialog object shown at start and drawn by the kernel, as a compiled module shows one.
struct Modal {
    ui: SharedUi,
    rejected: Arc<Mutex<u32>>,
}

impl Module for Modal {
    fn register(&mut self, _reg: &mut Registrar) {}

    fn init(&mut self, _ctx: &mut Context) {
        let mut store = ui::lock(&self.ui);
        let dialog = store.create(Kind::Dialog, None).unwrap();
        let layout = store.create(Kind::VBoxLayout, None).unwrap();
        store.add_to(dialog, layout, [0, 0, 1, 1]).unwrap();
        let rejected = self.rejected.clone();
        store
            .connect(
                dialog,
                Signal::Rejected,
                Arc::new(move |_: &SignalData| *lock(&rejected) += 1),
            )
            .unwrap();
        store.set_numbers(dialog, Property::Visible, &[1.0]).unwrap();
    }

    fn windows_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        ctx.draw_dialogs(&self.ui, egui);
    }
}

fn modal() -> (Box<dyn Module>, Jobs, Arc<Mutex<u32>>) {
    let jobs: Jobs = Arc::default();
    let queue = jobs.clone();
    let rejected = Arc::new(Mutex::new(0));
    let module = Modal {
        ui: Ui::new(Arc::new(move |job| lock(&queue).push(job))),
        rejected: rejected.clone(),
    };
    (Box::new(module), jobs, rejected)
}

#[test]
fn undo_and_redo_go_back_and_forth_from_the_menu_and_the_keyboard() {
    let (module, value) = counter("a");
    let mut harness = Harness::new(vec![("a", module)]);
    harness.call("a", "a.add", json!({ "by": 1 })).unwrap();
    harness.call("a", "a.add", json!({ "by": 2 })).unwrap();
    assert_eq!(*lock(&value), 3);
    harness.shell.undo();
    assert_eq!(*lock(&value), 1);
    harness.shell.redo();
    assert_eq!(*lock(&value), 3);
    harness.frame(key(Key::Z, true));
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 0);
    harness.frame(key(Key::Y, true));
    assert_eq!(*lock(&value), 1);
}

#[test]
fn a_group_is_one_entry_and_blocks_undo_while_it_holds_a_change() {
    let (module, value) = counter("a");
    let mut harness = Harness::new(vec![("a", module)]);
    let editor = harness.shell.host.editor("a");
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let script = std::thread::spawn(move || {
        editor.begin_group("both").unwrap();
        editor.call("a.add", json!({ "by": 1 })).unwrap();
        editor.call("a.add", json!({ "by": 2 })).unwrap();
        wait.recv().unwrap();
        editor.end_group().unwrap();
    });
    harness.until("the script's changes", |_| *lock(&value) == 3);
    assert!(harness.shell.blocking_undo().is_some());
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 3, "Undo refused while the group holds changes");
    release.send(()).unwrap();
    harness.until("the group's end", |shell| shell.groups.list().is_empty());
    script.join().unwrap();
    assert_eq!(harness.shell.history.undo_label().as_deref(), Some("both"));
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 0, "both changes undone at once");
}

#[test]
fn a_document_closed_without_saving_takes_its_changes_out_of_the_history() {
    let (first, a) = counter("a");
    let (second, _) = counter("b");
    let mut harness = Harness::new(vec![("a", first), ("b", second)]);
    harness.call("b", "b.add", json!({})).unwrap();
    harness.call("a", "a.add", json!({ "by": 2 })).unwrap();
    harness.shell.undo();
    harness.call("a", "a.add", json!({ "by": 3 })).unwrap();
    harness.call("a", "a.discard", json!({})).unwrap();
    assert_eq!(
        harness.shell.history.undo_label().as_deref(),
        Some("add 1"),
        "b's change stays"
    );
    assert_eq!(harness.shell.history.redo_label(), None);
    harness.shell.undo();
    assert_eq!(*lock(&a), 0, "nothing brings the dropped changes back");
}

#[test]
fn closing_with_unsaved_changes_asks_first_and_closes_once_answered() {
    let (module, _) = counter("a");
    let (dialogs, asked) = dialogs();
    let mut harness = Harness::new(vec![("a", module), ("dialogs", dialogs)]);
    harness.call("a", "a.add", json!({})).unwrap();
    assert!(harness.frame(closing(false)).contains(&ViewportCommand::CancelClose));
    assert_eq!(lock(&asked).len(), 1);
    assert_eq!(lock(&asked)[0]["first"], json!(true), "before every other window");
    // Asked again while the question waits: still open, and asked once.
    assert!(harness.frame(closing(false)).contains(&ViewportCommand::CancelClose));
    assert_eq!(lock(&asked).len(), 1);
    harness.answer(1, "save");
    assert!(harness.frame(RawInput::default()).contains(&ViewportCommand::Close));
    assert!(harness.shell.unsaved().is_empty(), "saved");
}

#[test]
fn closing_while_minimised_asks_and_brings_the_window_back() {
    let (module, _) = counter("a");
    let (dialogs, asked) = dialogs();
    let mut harness = Harness::new(vec![("a", module), ("dialogs", dialogs)]);
    harness.call("a", "a.add", json!({})).unwrap();
    let commands = harness.minimised(closing(true));
    assert!(commands.contains(&ViewportCommand::CancelClose), "{commands:?}");
    assert!(commands.contains(&ViewportCommand::Minimized(false)), "{commands:?}");
    assert!(commands.contains(&ViewportCommand::Focus), "{commands:?}");
    assert_eq!(lock(&asked).len(), 1);
    harness.answer(1, "discard");
    assert!(harness.frame(RawInput::default()).contains(&ViewportCommand::Close));
}

#[test]
fn closing_without_a_window_to_ask_in_loses_the_changes_and_closes() {
    let (module, _) = counter("a");
    let (dialogs, _) = dialogs();
    let mut harness = Harness::new(vec![("a", module), ("dialogs", dialogs)]);
    harness.call("a", "a.add", json!({})).unwrap();
    assert!(harness.frame(closing(false)).contains(&ViewportCommand::CancelClose));
    // The module showing the question fails while it waits: the next request closes.
    let index = harness.index("dialogs");
    harness.shell.fail(index, "broken".to_owned());
    assert!(!harness.frame(closing(false)).contains(&ViewportCommand::CancelClose));

    let (module, _) = counter("b");
    let mut alone = Harness::new(vec![("b", module)]);
    alone.call("b", "b.add", json!({})).unwrap();
    assert!(!alone.frame(closing(false)).contains(&ViewportCommand::CancelClose));
}

#[test]
fn a_failed_module_leaves_the_history_and_the_others_keep_theirs() {
    let (first, a) = counter("a");
    let (second, b) = counter("b");
    let mut harness = Harness::new(vec![("a", first), ("b", second)]);
    harness.call("b", "b.add", json!({ "by": 5 })).unwrap();
    harness.call("a", "a.add", json!({ "by": 1 })).unwrap();
    assert!(harness.call("a", "a.panic", json!({})).is_err());
    assert!(matches!(harness.shell.slots[0].state, State::Failed(_)));
    assert!(harness.call("b", "a.add", json!({})).is_err(), "its commands are gone");
    harness.shell.undo();
    assert_eq!(
        (*lock(&a), *lock(&b)),
        (1, 0),
        "Undo goes to the change of the module still running"
    );
}

#[test]
fn a_modal_window_takes_the_keyboard_until_escape_closes_it() {
    let (module, value) = counter("a");
    let (window, jobs, rejected) = modal();
    let mut harness = Harness::new(vec![("a", module), ("modal", window)]);
    harness.call("a", "a.add", json!({})).unwrap();
    harness.frame(RawInput::default());
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 1, "no shortcut under a modal window");
    harness.frame(key(Key::Escape, false));
    for job in std::mem::take(&mut *lock(&jobs)) {
        job();
    }
    assert_eq!(*lock(&rejected), 1, "Escape hides it and sends rejected");
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 0);
}

/// A Rust module drawing a modal window of its own with egui.
struct OwnModal;

impl Module for OwnModal {
    fn register(&mut self, _reg: &mut Registrar) {}

    fn windows_ui(&mut self, egui: &egui::Context, _ctx: &mut Context) {
        egui::Modal::new(egui::Id::new("own modal")).show(egui, |ui| ui.label("Busy"));
    }
}

#[test]
fn a_modal_window_a_rust_module_draws_with_egui_takes_the_keyboard_too() {
    let (module, value) = counter("a");
    let mut harness = Harness::new(vec![("a", module), ("own", Box::new(OwnModal))]);
    harness.call("a", "a.add", json!({})).unwrap();
    harness.frame(RawInput::default());
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 1, "no shortcut under the module's modal window");
}

/// Declares properties whose ranges are wrong, and a right one.
struct Ranges;

impl Module for Ranges {
    fn register(&mut self, reg: &mut Registrar) {
        let number = || uniwow_api::PropertyValue::Number(0.0);
        reg.animatable(
            "upside_down",
            "Upside down",
            uniwow_api::PropertyKind::Number,
            [1.0, 0.0],
            number,
            |_| {},
        );
        reg.animatable(
            "nan",
            "NaN",
            uniwow_api::PropertyKind::Number,
            [f64::NAN, 1.0],
            number,
            |_| {},
        );
        reg.animatable(
            "right",
            "Right",
            uniwow_api::PropertyKind::Number,
            [0.0, 1.0],
            number,
            |_| {},
        );
        for wrong in ["", "a/b", "with space"] {
            reg.animatable(
                wrong,
                "Wrong",
                uniwow_api::PropertyKind::Number,
                [0.0, 1.0],
                number,
                |_| {},
            );
        }
    }
}

#[test]
fn a_property_with_a_wrong_name_or_range_is_refused_and_its_module_goes_on() {
    let harness = Harness::new(vec![("ranges", Box::new(Ranges))]);
    let properties = harness.shell.host.bridge.properties.read().unwrap();
    let paths: Vec<&String> = properties.keys().collect();
    assert_eq!(paths, vec!["ranges/right"]);
    assert!(harness.shell.slots[0].state.is_running());
}

#[test]
fn a_compiled_module_command_called_from_the_interface_runs_on_its_thread() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let (reply, answer) = std::sync::mpsc::channel();
    harness.shell.answer(Request::Call {
        caller: KERNEL.to_owned(),
        thread: std::thread::current().id(),
        name: "native.where".to_owned(),
        arguments: json!({}),
        reply: ReplyTo::Thread(reply),
    });
    assert!(
        answer.try_recv().is_err(),
        "answered later, not on the interface thread"
    );
    let started = Instant::now();
    let result = loop {
        if let Ok(result) = answer.try_recv() {
            break result;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "no answer");
        harness.frame(RawInput::default());
    };
    assert_eq!(result, Ok(json!("uniwow module native")));
}

#[test]
fn undo_waits_while_a_compiled_module_has_work_on_its_thread() {
    let (module, value) = counter("a");
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::loaded("a", module), Slot::compiled("native", native)]);
    harness.call("a", "a.add", json!({})).unwrap();
    let thread = harness.shell.slots[1].compiled.expect("compiled");
    let (release, wait) = std::sync::mpsc::channel::<()>();
    ui::lock(&thread.ui).post_job(Box::new(move || {
        let _ = wait.recv();
    }));
    assert!(
        harness
            .shell
            .blocking_undo()
            .is_some_and(|reason| reason.contains("native"))
    );
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 1, "Undo refused while the module works");
    release.send(()).unwrap();
    harness.until("the module's work done", |shell| shell.blocking_undo().is_none());
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 0);
}

#[test]
fn undo_refuses_when_a_call_it_serves_starts_work_on_a_compiled_module() {
    let (module, value) = counter("a");
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::loaded("a", module), Slot::compiled("native", native)]);
    harness.call("a", "a.add", json!({})).unwrap();
    // Queued during the frame, as Context::call or the Commands panel does: served by Undo itself.
    let (reply, answer) = std::sync::mpsc::channel();
    harness.shell.host.bridge.queue(Request::Call {
        caller: KERNEL.to_owned(),
        thread: std::thread::current().id(),
        name: "native.wait".to_owned(),
        arguments: json!({}),
        reply: ReplyTo::Thread(reply),
    });
    harness.shell.undo();
    assert_eq!(
        *lock(&value),
        1,
        "Undo refused: the call it served keeps the module working"
    );
    let thread = harness.shell.slots[1].compiled.expect("compiled");
    capi::testing::open(thread);
    harness.until("the module's answer", |_| answer.try_recv().is_ok());
    harness.until("the module's work done", |shell| shell.blocking_undo().is_none());
    harness.shell.undo();
    assert_eq!(*lock(&value), 0);
}

#[test]
fn undo_refuses_when_a_call_it_serves_puts_a_first_change_into_a_group() {
    let (module, value) = counter("a");
    let mut harness = Harness::new(vec![("a", module)]);
    harness.call("a", "a.add", json!({})).unwrap();
    // A script's group, still empty, and its first change, both waiting in the queue.
    let script = std::thread::spawn(|| std::thread::current().id()).join().unwrap();
    let bridge = harness.shell.host.bridge.clone();
    bridge.queue(Request::BeginGroup {
        caller: "a".to_owned(),
        thread: script,
        label: "script".to_owned(),
    });
    let (reply, _answer) = std::sync::mpsc::channel();
    bridge.queue(Request::Call {
        caller: "a".to_owned(),
        thread: script,
        name: "a.add".to_owned(),
        arguments: json!({ "by": 5 }),
        reply: ReplyTo::Thread(reply),
    });
    harness.shell.undo();
    assert_eq!(
        *lock(&value),
        6,
        "the script's change is applied, and Undo refused while its group holds it"
    );
    assert!(harness.shell.blocking_undo().is_some());
}

#[test]
fn disabling_a_compiled_module_that_never_answers_gives_undo_back() {
    let (module, value) = counter("a");
    let native = CompiledModule::started(capi::testing::native("stuck"));
    let mut harness = Harness::with_slots(vec![Slot::loaded("a", module), Slot::compiled("stuck", native)]);
    harness.call("a", "a.add", json!({})).unwrap();
    let (reply, _answer) = std::sync::mpsc::channel();
    harness.shell.answer(Request::Call {
        caller: KERNEL.to_owned(),
        thread: std::thread::current().id(),
        name: "native.wait".to_owned(),
        arguments: json!({}),
        reply: ReplyTo::Thread(reply),
    });
    assert!(
        harness.shell.blocking_undo().is_some(),
        "its thread never ends its work"
    );
    // What the button of the Modules panel does.
    let index = harness.index("stuck");
    harness.shell.fail(index, "disabled from the Modules panel".to_owned());
    assert!(harness.shell.blocking_undo().is_none());
    harness.shell.undo();
    assert_eq!(*lock(&value), 0, "Undo is back");
    capi::testing::open(harness.shell.slots[1].compiled.expect("compiled"));
}

#[test]
fn undo_at_random_moments_never_crosses_a_compiled_module_s_changes() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let module = harness.shell.slots[0].compiled.expect("compiled");
    const CHANGES: usize = 60;
    for seed in 1..=3 {
        let mut random = Random::new(seed);
        let mut posted = 0;
        while posted < CHANGES || module.activity.pending() > 0 {
            if posted < CHANGES && random.below(3) == 0 {
                let (label, pause) = (format!("{seed}/{posted}"), random.below(300));
                posted += 1;
                ui::lock(&module.ui).post_job(Box::new(move || {
                    std::thread::sleep(Duration::from_micros(pause));
                    capi::testing::change(module, &label, 1);
                }));
            }
            if random.below(4) == 0 {
                let before = harness.shell.history.done.len();
                harness.shell.undo();
                if harness.shell.history.done.len() < before && posted > 0 {
                    // It ran: the module's work was done, and its last change is in the history.
                    let last = format!("{seed}/{}", posted - 1);
                    let history = &harness.shell.history;
                    assert!(
                        history
                            .done
                            .iter()
                            .chain(&history.undone)
                            .any(|entry| entry.label == last),
                        "seed {seed}: Undo ran before the change {last} reached the history"
                    );
                }
            } else {
                harness.frame(RawInput::default());
            }
        }
        let last = format!("{seed}/{}", CHANGES - 1);
        harness.until("the last change", |shell| {
            shell
                .history
                .done
                .iter()
                .chain(&shell.history.undone)
                .any(|entry| entry.label == last)
        });
        // The undo values went through the module's thread: it agrees with the history.
        harness.until("the module's work done", |shell| shell.blocking_undo().is_none());
        assert_eq!(
            capi::testing::value(module),
            harness.shell.history.done.len() as i64,
            "seed {seed}"
        );
    }
}

#[test]
fn a_compiled_module_s_property_is_read_at_once_and_written_on_its_thread_merged() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let module = harness.shell.slots[0].compiled.expect("compiled");
    let editor = harness.shell.host.editor(KERNEL);
    let listed = editor.properties();
    let level = listed.iter().find(|p| p.path == "native/level").expect("listed");
    assert_eq!(level.range, [0.0, 10.0]);
    assert_eq!(
        editor.read_property("native/level"),
        Ok(uniwow_api::PropertyValue::Number(2.0))
    );
    // The module's thread busy: writes neither wait nor pile up.
    let (release, wait) = std::sync::mpsc::channel::<()>();
    ui::lock(&module.ui).post_job(Box::new(move || {
        let _ = wait.recv();
    }));
    for value in [3.4, 5.6, 4.2] {
        editor
            .write_property("native/level", uniwow_api::PropertyValue::Number(value))
            .unwrap();
    }
    assert_eq!(
        editor.read_property("native/level"),
        Ok(uniwow_api::PropertyValue::Number(4.2))
    );
    release.send(()).unwrap();
    harness.settle(module);
    assert_eq!(
        capi::testing::level(module),
        (4.0, 1),
        "one write, the last, rounded by the module"
    );
    assert_eq!(
        editor.read_property("native/level"),
        Ok(uniwow_api::PropertyValue::Number(4.0)),
        "the kernel's copy follows what the module kept"
    );
}

#[test]
fn a_write_its_module_fails_on_makes_the_module_fail() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let editor = harness.shell.host.editor(KERNEL);
    editor
        .write_property("native/level", uniwow_api::PropertyValue::Number(7.0))
        .unwrap();
    harness.until("the module's failure", |shell| {
        matches!(shell.slots[0].state, State::Failed(_))
    });
    let State::Failed(reason) = &harness.shell.slots[0].state else {
        unreachable!()
    };
    assert!(reason.contains("level") && reason.contains("seven"), "{reason}");
}

#[test]
fn a_compiled_module_lists_reads_writes_and_tells_properties_through_the_c_functions() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let module = harness.shell.slots[0].compiled.expect("compiled");
    let listed: Value = uniwow_api::serde_json::from_str(&capi::testing::properties_json(module)).unwrap();
    let level = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["path"] == json!("native/level"))
        .expect("listed");
    assert_eq!(level["kind"], json!("number"));
    assert_eq!(capi::testing::read_number(module, c"native/level"), (1, 2.0));
    assert_eq!(capi::testing::write_numbers(module, c"native/level", &[9.0]), 0);
    assert_eq!(
        capi::testing::write_numbers(module, c"native/level", &[1.0, 2.0]),
        1,
        "a number is one number"
    );
    harness.settle(module);
    assert_eq!(capi::testing::level(module).0, 9.0);
    assert_eq!(capi::testing::tell_numbers(module, c"level", &[5.0]), 0);
    assert_eq!(capi::testing::read_number(module, c"native/level"), (1, 5.0));
    assert_eq!(capi::testing::tell_numbers(module, c"level", &[f64::NAN]), 1);
    assert_eq!(capi::testing::tell_numbers(module, c"other", &[1.0]), 1);
    assert!(
        !listed
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["path"] == json!("native/wrong")),
        "its range goes down"
    );
    assert_eq!(
        capi::testing::tell_numbers(module, c"wrong", &[3.0]),
        1,
        "a property refused when the module started"
    );
    assert_eq!(capi::testing::tell_numbers(module, c"level", &[50.0]), 0);
    assert_eq!(
        capi::testing::read_number(module, c"native/level"),
        (1, 10.0),
        "kept within its range"
    );
}

#[test]
fn the_writes_of_a_property_neither_block_undo_nor_show_the_module_busy() {
    let (module, value) = counter("a");
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::loaded("a", module), Slot::compiled("native", native)]);
    harness.call("a", "a.add", json!({})).unwrap();
    let thread = harness.shell.slots[1].compiled.expect("compiled");
    let editor = harness.shell.host.editor(KERNEL);
    // A write the module takes its time over: 8 waits until the gate opens.
    editor
        .write_property("native/level", uniwow_api::PropertyValue::Number(8.0))
        .unwrap();
    harness.until("the write running", |_| {
        thread.activity.running_for().is_some() || capi::testing::level(thread).1 > 0
    });
    assert!(
        harness.shell.blocking_undo().is_none(),
        "a write records nothing: Undo is not blocked"
    );
    assert!(
        thread.activity.running_for().is_none(),
        "nor is the module busy, with nothing waiting behind"
    );
    harness.shell.undo();
    assert_eq!(*lock(&value), 0, "Undo ran during the write");
    // Counted work waiting behind the write: the module is busy.
    ui::lock(&thread.ui).post_job(Box::new(|| {}));
    assert!(harness.shell.blocking_undo().is_some());
    assert!(thread.activity.running_for().is_some());
    capi::testing::open(thread);
    harness.until("the module's work done", |shell| shell.blocking_undo().is_none());
}

#[test]
fn a_value_a_module_keeps_that_is_not_finite_leaves_the_one_before() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    let editor = harness.shell.host.editor(KERNEL);
    editor
        .write_property("native/free", uniwow_api::PropertyValue::Number(1.0))
        .unwrap();
    harness.settle(thread);
    assert_eq!(
        editor.read_property("native/free"),
        Ok(uniwow_api::PropertyValue::Number(1.0)),
        "the infinity the module kept is refused"
    );
    assert!(harness.shell.slots[0].state.is_running());
}

#[test]
fn a_change_recorded_in_a_property_s_write_is_refused() {
    let (module, value) = counter("a");
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::loaded("a", module), Slot::compiled("native", native)]);
    harness.call("a", "a.add", json!({})).unwrap();
    let thread = harness.shell.slots[1].compiled.expect("compiled");
    let editor = harness.shell.host.editor(KERNEL);
    editor
        .write_property("native/level", uniwow_api::PropertyValue::Number(6.0))
        .unwrap();
    harness.settle(thread);
    harness.frame(RawInput::default());
    assert_ne!(
        capi::testing::recorded_in_write(thread),
        0,
        "record_change refused in a write"
    );
    assert_eq!(
        harness.shell.history.undo_label().as_deref(),
        Some("add 1"),
        "the history did not change"
    );
    assert!(harness.shell.blocking_undo().is_none());
    harness.shell.undo();
    assert_eq!(*lock(&value), 0, "Undo goes to the change before");
    // Out of a write, the module records as before.
    capi::testing::change(thread, "after", 1);
    harness.frame(RawInput::default());
    assert_eq!(harness.shell.history.undo_label().as_deref(), Some("after"));
}

#[test]
fn a_write_that_unwinds_leaves_the_module_s_thread_recording() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    let editor = harness.shell.host.editor(KERNEL);
    editor
        .write_property("native/level", uniwow_api::PropertyValue::Number(1.0))
        .unwrap();
    harness.settle(thread);
    // A change recorded on the module's thread, out of a write.
    let (done, recorded) = std::sync::mpsc::channel();
    ui::lock(&thread.ui).post_job(Box::new(move || {
        let _ = done.send(capi::testing::record(thread, "after an unwind", 1));
    }));
    harness.settle(thread);
    assert_eq!(recorded.try_recv(), Ok(0), "recorded once the write unwound");
    harness.frame(RawInput::default());
    assert_eq!(harness.shell.history.undo_label().as_deref(), Some("after an unwind"));
}

/// Tracks on `native/level`, with a key of each value at each frame given.
fn level_tracks(keys: &[(u32, f64)]) -> String {
    let keys: Vec<Value> = keys
        .iter()
        .map(|(frame, value)| json!({ "time": frame, "value": value, "mode": "flat" }))
        .collect();
    json!([{ "property": "native/level", "kind": "number", "curves": [{ "keys": keys }] }]).to_string()
}

/// The value of the first track of `sequence` at `frame`, as the module keeps it: rounded.
fn level_at(module: &capi::ModuleContext, sequence: u64, frame: f64) -> f64 {
    let data = ui::lock(&module.ui).sequence(sequence).unwrap();
    match data.tracks[0].evaluate(frame, None) {
        Some(uniwow_api::PropertyValue::Number(value)) => value.round(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_value_changed_elsewhere_gives_way_to_the_track_once_the_playhead_moves() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    let player = {
        let mut objects = ui::lock(&thread.ui);
        let sequence = objects.create(Kind::Sequence, None).unwrap();
        objects.set_numbers(sequence, Property::Length, &[60.0]).unwrap();
        objects
            .set_text(sequence, Property::Tracks, &level_tracks(&[(0, 5.0), (60, 5.0)]))
            .unwrap();
        let player = objects.create(Kind::Player, None).unwrap();
        objects
            .set_numbers(player, Property::Sequence, &[sequence as f64])
            .unwrap();
        objects.set_numbers(player, Property::Time, &[30.0]).unwrap();
        player
    };
    let now = Instant::now();
    harness.play_at(now, thread);
    assert_eq!(capi::testing::level(thread).0, 5.0, "the track's value");
    assert_eq!(capi::testing::write_numbers(thread, c"native/level", &[9.0]), 0);
    harness.play_at(now, thread);
    assert_eq!(
        capi::testing::level(thread).0,
        9.0,
        "changed elsewhere, the playhead still"
    );
    ui::lock(&thread.ui)
        .set_numbers(player, Property::Time, &[40.0])
        .unwrap();
    harness.play_at(now, thread);
    assert_eq!(
        capi::testing::level(thread).0,
        5.0,
        "the playhead moved: the track's value again"
    );
}

#[test]
fn a_player_writes_its_values_through_the_catalogue_where_they_change() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    let (sequence, player) = {
        let mut objects = ui::lock(&thread.ui);
        let sequence = objects.create(Kind::Sequence, None).unwrap();
        objects.set_numbers(sequence, Property::Length, &[60.0]).unwrap();
        objects
            .set_text(sequence, Property::Tracks, &level_tracks(&[(0, 0.0), (60, 10.0)]))
            .unwrap();
        let player = objects.create(Kind::Player, None).unwrap();
        objects
            .set_numbers(player, Property::Sequence, &[sequence as f64])
            .unwrap();
        objects.set_numbers(player, Property::Time, &[30.0]).unwrap();
        (sequence, player)
    };
    let start = Instant::now();
    let after = |seconds: f64| start + Duration::from_secs_f64(seconds);
    assert!(!harness.play_at(after(0.0), thread), "paused");
    assert_eq!(
        capi::testing::level(thread),
        (5.0, 1),
        "the value at its time, set by the module"
    );
    harness.play_at(after(0.0), thread);
    assert_eq!(capi::testing::level(thread).1, 1, "the same time: nothing written");
    // Other keys, the same value at frame 30: nothing written; another value: written.
    let set_tracks = |keys: &[(u32, f64)]| {
        ui::lock(&thread.ui)
            .set_text(sequence, Property::Tracks, &level_tracks(keys))
            .unwrap();
    };
    set_tracks(&[(0, 0.0), (30, 5.0), (60, 10.0)]);
    harness.play_at(after(0.0), thread);
    assert_eq!(capi::testing::level(thread).1, 1, "the value did not change");
    set_tracks(&[(0, 0.0), (30, 3.0), (60, 6.0)]);
    harness.play_at(after(0.0), thread);
    assert_eq!(capi::testing::level(thread), (3.0, 2));
    // Played: on by the time elapsed, at the frame rate (30), from the first frame it plays in.
    ui::lock(&thread.ui)
        .set_numbers(player, Property::Playing, &[1.0])
        .unwrap();
    assert!(harness.play_at(after(10.0), thread), "plays on");
    assert_eq!(
        capi::testing::level(thread).1,
        2,
        "the first frame it plays in counts no time"
    );
    assert!(harness.play_at(after(10.5), thread));
    assert_eq!(capi::testing::level(thread).0, level_at(thread, sequence, 45.0));
    assert!(!harness.play_at(after(20.0), thread), "stopped at the end");
    assert_eq!(capi::testing::level(thread).0, 6.0);
    assert_eq!(
        ui::lock(&thread.ui).numbers(player, Property::Time).unwrap(),
        vec![60.0]
    );
}

#[test]
fn each_change_of_a_sequence_s_tracks_is_an_undo_entry_of_its_module() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    let sequence = ui::lock(&thread.ui).create(Kind::Sequence, None).unwrap();
    ui::lock(&thread.ui)
        .set_text(sequence, Property::Tracks, &level_tracks(&[(0, 4.0)]))
        .unwrap();
    harness.frame(RawInput::default());
    assert_eq!(harness.shell.history.undo_label().as_deref(), Some("edit a sequence"));
    let keys = || {
        let data = ui::lock(&thread.ui).sequence(sequence).unwrap();
        data.tracks
            .iter()
            .map(|track| track.curves[0].keys.len())
            .sum::<usize>()
    };
    harness.shell.undo();
    assert_eq!(keys(), 0, "the tracks before");
    harness.shell.redo();
    assert_eq!(keys(), 1);
    // The module failing takes its changes out of the history.
    let index = harness.index("native");
    harness.shell.fail(index, "test".to_owned());
    assert!(harness.shell.history.undo_label().is_none());
}

#[test]
fn the_tracks_of_a_sequence_cannot_change_in_a_property_s_write() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    let sequence = ui::lock(&thread.ui).create(Kind::Sequence, None).unwrap();
    capi::testing::set_sequence(thread, sequence);
    let editor = harness.shell.host.editor(KERNEL);
    editor
        .write_property("native/free", uniwow_api::PropertyValue::Number(2.0))
        .unwrap();
    harness.settle(thread);
    harness.frame(RawInput::default());
    assert_eq!(capi::testing::tracks_refused(thread), 1);
    assert!(ui::lock(&thread.ui).sequence(sequence).unwrap().tracks.is_empty());
    assert!(harness.shell.history.undo_label().is_none());
}

/// A Rust module handing its interface objects to the kernel.
struct Adopting {
    objects: SharedUi,
}

impl Module for Adopting {
    fn register(&mut self, _reg: &mut Registrar) {}

    fn init(&mut self, ctx: &mut Context) {
        ctx.adopt_objects(&self.objects);
    }
}

#[test]
fn the_objects_a_rust_module_adopts_play_and_record_as_a_compiled_module_s() {
    let objects = Ui::new(Arc::new(|job| job()));
    let adopting = Adopting {
        objects: objects.clone(),
    };
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![
        Slot::loaded("rust", Box::new(adopting)),
        Slot::compiled("native", native),
    ]);
    let thread = harness.shell.slots[1].compiled.expect("compiled");
    let player = {
        let mut store = ui::lock(&objects);
        let sequence = store.create(Kind::Sequence, None).unwrap();
        store
            .set_text(sequence, Property::Tracks, &level_tracks(&[(0, 0.0), (120, 10.0)]))
            .unwrap();
        let player = store.create(Kind::Player, None).unwrap();
        store
            .set_numbers(player, Property::Sequence, &[sequence as f64])
            .unwrap();
        player
    };
    harness.frame(RawInput::default());
    assert_eq!(harness.shell.history.undo_label().as_deref(), Some("edit a sequence"));
    ui::lock(&objects)
        .set_numbers(player, Property::Playing, &[1.0])
        .unwrap();
    let start = Instant::now();
    harness.play_at(start, thread);
    harness.play_at(start + Duration::from_secs(2), thread);
    assert_eq!(ui::lock(&objects).numbers(player, Property::Time).unwrap(), vec![60.0]);
    assert_eq!(capi::testing::level(thread).0, 5.0, "the value at frame 60 of 120");
}

/// A player playing, among the objects of `module`, a sequence of 120 frames with `tracks`.
fn playing(module: &capi::ModuleContext, tracks: &str) -> u64 {
    let mut objects = ui::lock(&module.ui);
    let sequence = objects.create(Kind::Sequence, None).unwrap();
    objects.set_numbers(sequence, Property::Length, &[120.0]).unwrap();
    objects.set_text(sequence, Property::Tracks, tracks).unwrap();
    let player = objects.create(Kind::Player, None).unwrap();
    objects
        .set_numbers(player, Property::Sequence, &[sequence as f64])
        .unwrap();
    objects.set_numbers(player, Property::Playing, &[1.0]).unwrap();
    player
}

/// A track on `property`, of `kind`, rising from 0 to 10 over 120 frames.
fn rising(property: &str, kind: &str) -> String {
    let curves: Vec<Value> = (0..uniwow_api::PropertyKind::from_name(kind).unwrap().components())
        .map(|_| json!({ "keys": [{ "time": 0, "value": 0 }, { "time": 120, "value": 10 }] }))
        .collect();
    json!([{ "property": property, "kind": kind, "curves": curves }]).to_string()
}

#[test]
fn undo_stays_available_while_a_player_whose_time_a_slot_follows_plays() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    let player = playing(thread, &rising("native/level", "number"));
    // A slow slot, as a module showing the time in its panel.
    ui::lock(&thread.ui)
        .connect(
            player,
            Signal::TimeChanged,
            Arc::new(|_: &SignalData| std::thread::sleep(Duration::from_millis(20))),
        )
        .unwrap();
    let start = Instant::now();
    for frame in 0..10 {
        harness.tick(start + Duration::from_millis(33 * frame));
        assert_eq!(harness.shell.blocking_undo(), None, "frame {frame}");
    }
    Harness::wait_for(thread);
}

#[test]
fn one_time_changed_at_most_waits_per_player_with_the_last_time() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    // Two players, one animating a property of the module, whose writes post jobs in between.
    let players = [
        playing(thread, &rising("native/level", "number")),
        playing(thread, &rising("native/free", "number")),
    ];
    let seen: Arc<Mutex<Vec<(u64, f64)>>> = Arc::default();
    for player in players {
        let seen = seen.clone();
        ui::lock(&thread.ui)
            .connect(
                player,
                Signal::TimeChanged,
                Arc::new(move |data: &SignalData| lock(&seen).push((data.sender, data.number))),
            )
            .unwrap();
    }
    // The module's thread busy for 20 frames.
    let (release, gate) = std::sync::mpsc::channel::<()>();
    ui::lock(&thread.ui).post_job(Box::new(move || {
        let _ = gate.recv();
    }));
    let start = Instant::now();
    for frame in 0..20 {
        harness.tick(start + Duration::from_secs_f64(f64::from(frame) / 30.0));
    }
    release.send(()).unwrap();
    Harness::wait_for(thread);
    let seen = lock(&seen);
    for player in players {
        let times: Vec<f64> = seen
            .iter()
            .filter(|(sender, _)| *sender == player)
            .map(|(_, time)| *time)
            .collect();
        let last = ui::lock(&thread.ui).numbers(player, Property::Time).unwrap()[0];
        assert_eq!(times, vec![last], "player {player}");
    }
}

#[test]
fn a_change_recorded_in_a_slot_of_time_changed_is_refused() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    let player = playing(thread, &rising("native/level", "number"));
    let status = Arc::new(std::sync::atomic::AtomicI32::new(-1));
    let answered = status.clone();
    ui::lock(&thread.ui)
        .connect(
            player,
            Signal::TimeChanged,
            Arc::new(move |_: &SignalData| {
                answered.store(
                    capi::testing::record(thread, "in a slot", 1),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }),
        )
        .unwrap();
    let start = Instant::now();
    harness.play_at(start, thread);
    harness.play_at(start + Duration::from_millis(100), thread);
    assert!(status.load(std::sync::atomic::Ordering::SeqCst) > 0, "refused");
    harness.frame(RawInput::default());
    assert_eq!(harness.shell.history.undo_label().as_deref(), Some("edit a sequence"));
}

#[test]
fn a_track_no_property_can_take_is_told_once_per_player() {
    let native = CompiledModule::started(capi::testing::native("native"));
    let mut harness = Harness::with_slots(vec![Slot::compiled("native", native)]);
    let thread = harness.shell.slots[0].compiled.expect("compiled");
    let mut tracks: Vec<Value> = uniwow_api::serde_json::from_str(&rising("nobody/thing", "number")).unwrap();
    // A vector on a number.
    tracks.extend(uniwow_api::serde_json::from_str::<Vec<Value>>(&rising("native/level", "vector")).unwrap());
    let player = playing(thread, &Value::Array(tracks).to_string());
    let start = Instant::now();
    for frame in 0..3 {
        harness.play_at(start + Duration::from_millis(100 * frame), thread);
    }
    assert_eq!(
        harness.shell.players.warned("native", player),
        vec!["native/level".to_owned(), "nobody/thing".to_owned()]
    );
    assert_eq!(capi::testing::level(thread).1, 0, "nothing written");
}

#[test]
fn a_document_forgotten_takes_the_changes_of_the_tracks_of_its_sequence_with_it() {
    let objects = Ui::new(Arc::new(|job| job()));
    let adopting = Adopting {
        objects: objects.clone(),
    };
    let mut harness = Harness::with_slots(vec![Slot::loaded("rust", Box::new(adopting))]);
    {
        let mut store = ui::lock(&objects);
        let intro = store.create(Kind::Sequence, None).unwrap();
        store.set_text(intro, Property::Title, "intro").unwrap();
        store
            .set_text(intro, Property::Tracks, &level_tracks(&[(0, 1.0)]))
            .unwrap();
        let other = store.create(Kind::Sequence, None).unwrap();
        store
            .set_text(other, Property::Tracks, &level_tracks(&[(0, 2.0)]))
            .unwrap();
    }
    harness.frame(RawInput::default());
    assert_eq!(harness.shell.history.done.len(), 2);
    uniwow_api::Host::forget_document(&mut harness.shell.host, "rust", "intro");
    harness.frame(RawInput::default());
    assert_eq!(harness.shell.history.done.len(), 1, "the other sequence's change stays");
}

/// How often the first hotkey of `Keyed` was pressed, and whether the second was held at the last
/// frame.
type Seen = Arc<Mutex<(u32, bool)>>;

/// A module acting at each frame on two hotkeys of its own.
struct Keyed {
    act: Hotkey,
    walk: Hotkey,
    seen: Seen,
}

impl Module for Keyed {
    fn register(&mut self, reg: &mut Registrar) {
        self.act = reg.hotkey("act", "Act", HotkeyKind::Press, Keys::key(Key::F));
        self.walk = reg.hotkey("walk", "Walk", HotkeyKind::Hold, Keys::key(Key::W));
    }

    fn windows_ui(&mut self, egui: &egui::Context, _ctx: &mut Context) {
        let mut seen = lock(&self.seen);
        seen.0 += u32::from(self.act.pressed(egui));
        seen.1 = self.walk.held(egui);
    }
}

fn keyed() -> (Box<dyn Module>, Seen) {
    let seen = Seen::default();
    let module = Keyed {
        act: Hotkey::new(HotkeyKind::Press, Keys::NONE),
        walk: Hotkey::new(HotkeyKind::Hold, Keys::NONE),
        seen: seen.clone(),
    };
    (Box::new(module), seen)
}

/// The modifiers `held` down from this frame on.
fn modifiers(held: Modifiers) -> RawInput {
    RawInput {
        events: vec![egui::Event::ModifiersChanged(held)],
        ..RawInput::default()
    }
}

fn hotkey(shell: &Shell, id: &str) -> usize {
    shell.hotkeys.entries.iter().position(|entry| entry.id() == id).unwrap()
}

#[test]
fn a_hotkey_bound_in_its_window_acts_by_its_new_keys_which_are_saved() {
    let (module, seen) = keyed();
    let (counter, value) = counter("a");
    let mut harness = Harness::new(vec![("m", module), ("a", counter)]);
    harness.frame(key(Key::F, false));
    assert_eq!(lock(&seen).0, 1, "F by default");
    let act = hotkey(&harness.shell, "m/act");
    harness.shell.hotkey_window.open = true;
    harness.shell.hotkey_window.awaiting = Some((act, Keys::NONE));
    harness.frame(key(Key::G, false));
    assert_eq!(lock(&seen).0, 1, "the keys awaited act on nothing");
    assert!(harness.shell.hotkey_window.awaiting.is_none());
    assert_eq!(harness.shell.host.settings.hotkeys["m/act"], "G");
    harness.frame(key(Key::F, false));
    harness.frame(key(Key::G, false));
    assert_eq!(lock(&seen).0, 2, "G now, F no longer");

    harness.shell.hotkey_window.awaiting = Some((act, Keys::NONE));
    harness.frame(key(Key::Escape, false));
    assert!(harness.shell.hotkey_window.awaiting.is_none(), "Escape cancels");
    assert_eq!(harness.shell.hotkeys.entries[act].hotkey.keys(), Keys::key(Key::G));

    harness.call("a", "a.add", json!({ "by": 1 })).unwrap();
    harness.shell.hotkey_window.awaiting = Some((act, Keys::NONE));
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 1, "Ctrl+Z awaited does not undo");
    assert_eq!(harness.shell.hotkeys.entries[act].hotkey.keys(), Keys::ctrl(Key::Z));
    let undo = hotkey(&harness.shell, "kernel/undo");
    assert_eq!(
        harness.shell.hotkeys.conflicts(act, |_| true),
        vec![undo],
        "said to be Undo's keys"
    );
    harness.frame(RawInput::default());

    harness.shell.hotkey_window.open = false;
    harness.frame(RawInput::default());
    assert!(harness.shell.hotkey_window.awaiting.is_none());
}

#[test]
fn a_held_hotkey_takes_modifiers_alone_and_none_acts_while_keys_are_awaited() {
    let (module, seen) = keyed();
    let mut harness = Harness::new(vec![("m", module)]);
    harness.frame(key(Key::W, false));
    assert!(lock(&seen).1, "W held");
    let walk = hotkey(&harness.shell, "m/walk");
    harness.shell.hotkey_window.open = true;
    harness.shell.hotkey_window.awaiting = Some((walk, Keys::NONE));
    harness.frame(modifiers(Modifiers::SHIFT));
    assert!(!lock(&seen).1, "nothing acts while keys are awaited");
    assert!(
        harness.shell.hotkey_window.awaiting.is_some(),
        "Shift may come with a key"
    );
    harness.frame(modifiers(Modifiers::NONE));
    assert_eq!(
        harness.shell.hotkeys.entries[walk].hotkey.keys(),
        Keys::SHIFT,
        "Shift alone, once released"
    );
    assert_eq!(harness.shell.host.settings.hotkeys["m/walk"], "Shift");
    harness.frame(modifiers(Modifiers::SHIFT));
    assert!(lock(&seen).1, "Shift held");

    let act = hotkey(&harness.shell, "m/act");
    harness.shell.hotkey_window.awaiting = Some((act, Keys::NONE));
    harness.frame(modifiers(Modifiers::ALT));
    harness.frame(modifiers(Modifiers::NONE));
    assert!(
        harness.shell.hotkey_window.awaiting.is_some(),
        "a hotkey pressed takes no modifiers alone"
    );
    harness.frame(key(Key::H, false));
    assert_eq!(harness.shell.hotkeys.entries[act].hotkey.keys(), Keys::key(Key::H));
}

#[test]
fn undo_and_redo_follow_the_keys_they_are_bound_to() {
    let (module, value) = counter("a");
    let mut harness = Harness::new(vec![("a", module)]);
    harness.call("a", "a.add", json!({ "by": 1 })).unwrap();
    harness.call("a", "a.add", json!({ "by": 1 })).unwrap();
    let (undo, redo) = (
        hotkey(&harness.shell, "kernel/undo"),
        hotkey(&harness.shell, "kernel/redo"),
    );
    let shell = &mut harness.shell;
    shell
        .hotkeys
        .bind(undo, Keys::ctrl(Key::U), &mut shell.host.settings.hotkeys);
    shell
        .hotkeys
        .bind(redo, Keys::ctrl(Key::R), &mut shell.host.settings.hotkeys);
    harness.frame(key(Key::Z, true));
    assert_eq!(*lock(&value), 2, "Ctrl+Z no longer undoes");
    harness.frame(key(Key::U, true));
    assert_eq!(*lock(&value), 1);
    harness.frame(key(Key::R, true));
    assert_eq!(*lock(&value), 2);
}

const OFFERED: uniwow_api::ServiceKey<()> = uniwow_api::ServiceKey::new("offered");

/// A module that declares the command `x.do`, may provide the service `offered`, and may panic
/// in `init`.
struct Offering {
    provides: bool,
    panics: bool,
}

impl Module for Offering {
    fn register(&mut self, reg: &mut Registrar) {
        reg.command("x.do", "Does x", json!({}), json!({}));
        if self.provides {
            reg.provide(OFFERED, ());
        }
    }

    fn init(&mut self, _ctx: &mut Context) {
        assert!(!self.panics, "init failed on purpose");
    }
}

fn offering(id: &str, provides: bool, panics: bool) -> Slot {
    Slot::loaded(id, Box::new(Offering { provides, panics }))
}

/// A running module whose manifest requires and uses the services given, as `discover` leaves it.
fn needing(id: &str, requires: &[&str], uses: &[&str]) -> Slot {
    let mut slot = offering(id, false, false);
    slot.manifest = Some(crate::manifest::Manifest {
        id: id.to_owned(),
        kind: crate::manifest::Kind::Rust,
        package: String::new(),
        name: id.to_owned(),
        version: "1".to_owned(),
        category: String::new(),
        description: String::new(),
        requires: requires.iter().map(|s| (*s).to_owned()).collect(),
        uses: uses.iter().map(|s| (*s).to_owned()).collect(),
        dll: String::new(),
        dll_hash: String::new(),
        runtime: String::new(),
    });
    slot
}

fn slot<'a>(shell: &'a Shell, id: &str) -> &'a Slot {
    shell
        .slots
        .iter()
        .find(|slot| slot.id == id)
        .expect("a slot of that id")
}

#[test]
fn a_command_set_aside_comes_back_when_the_module_that_won_does_not_start() {
    let harness = Harness::with_slots(vec![offering("a", false, true), offering("b", false, false)]);
    let shell = &harness.shell;
    assert!(matches!(slot(shell, "a").state, State::Failed(_)));
    assert_eq!(
        shell.host.bridge.lookup("x.do").map(|(owner, _)| owner),
        Ok("b".to_owned())
    );
    assert_eq!(slot(shell, "b").commands, [("x.do".to_owned(), None)]);
    assert_eq!(
        slot(shell, "a").commands,
        [("x.do".to_owned(), Some("offered by 'b'".to_owned()))]
    );
}

#[test]
fn a_command_set_aside_comes_back_when_the_module_that_won_fails_later() {
    let mut harness = Harness::with_slots(vec![offering("a", false, false), offering("b", false, false)]);
    assert_eq!(
        harness.shell.host.bridge.lookup("x.do").map(|(owner, _)| owner),
        Ok("a".to_owned())
    );
    harness.shell.fail(0, "on purpose".to_owned());
    assert_eq!(
        harness.shell.host.bridge.lookup("x.do").map(|(owner, _)| owner),
        Ok("b".to_owned())
    );
    assert_eq!(slot(&harness.shell, "b").commands, [("x.do".to_owned(), None)]);
}

#[test]
fn a_module_whose_required_service_nobody_provides_is_blocked() {
    let harness = Harness::with_slots(vec![needing("consumer", &["offered"], &[])]);
    assert!(
        matches!(
            &slot(&harness.shell, "consumer").state,
            State::Blocked(reason) if reason == "requires the service 'offered', which no running module provides"
        ),
        "{}",
        state_text(&slot(&harness.shell, "consumer").state)
    );
}

#[test]
fn a_module_whose_provider_failed_in_its_init_is_blocked_and_one_using_it_starts_without_it() {
    // Registered first, so that only the order of the providers puts the provider before them.
    let harness = Harness::with_slots(vec![
        needing("consumer", &["offered"], &[]),
        needing("user", &[], &["offered"]),
        offering("provider", true, true),
    ]);
    let shell = &harness.shell;
    assert!(matches!(slot(shell, "provider").state, State::Failed(_)));
    assert!(
        matches!(
            &slot(shell, "consumer").state,
            State::Blocked(reason) if reason == "requires the service 'offered', whose provider failed"
        ),
        "{}",
        state_text(&slot(shell, "consumer").state)
    );
    assert!(slot(shell, "user").state.is_running());
    assert!(!shell.host.services.contains_key("offered"), "withdrawn");
}

#[test]
fn what_the_interface_kept_of_the_objects_of_a_failed_module_goes() {
    let mut harness = Harness::with_slots(vec![offering("a", false, false), offering("b", false, false)]);
    for owner in ["a", "b"] {
        harness
            .shell
            .host
            .views
            .insert((owner.to_owned(), 1), crate::draw::PanelView::default());
    }
    harness.shell.fail(0, "on purpose".to_owned());
    let kept: Vec<&str> = harness
        .shell
        .host
        .views
        .keys()
        .map(|(owner, _)| owner.as_str())
        .collect();
    assert_eq!(kept, ["b"]);
}

/// A module whose command `breaking.now`, run on the calling thread, panics.
struct Breaking;

impl Module for Breaking {
    fn register(&mut self, reg: &mut Registrar) {
        reg.command_on_caller(
            "breaking.now",
            "Panics",
            json!({}),
            json!({}),
            Arc::new(|_| panic!("broken on purpose")),
        );
    }
}

#[test]
fn a_panic_in_a_command_run_on_the_caller_makes_its_module_fail_and_the_next_call_is_refused() {
    let mut harness = Harness::new(vec![("breaking", Box::new(Breaking)), ("other", counter("other").0)]);
    let editor = uniwow_api::Editor::new(harness.shell.host.bridge.clone(), "other");
    let failed = editor.call("breaking.now", json!({})).expect_err("it panicked");
    assert!(failed.contains("broken on purpose"), "{failed}");
    harness.frame(RawInput::default());
    assert!(
        matches!(&slot(&harness.shell, "breaking").state, State::Failed(reason) if reason.contains("command 'breaking.now' panicked")),
        "{}",
        state_text(&slot(&harness.shell, "breaking").state)
    );
    let refused = editor.call("breaking.now", json!({})).expect_err("refused");
    assert!(refused.contains("which is not running"), "{refused}");
    assert!(slot(&harness.shell, "other").state.is_running());
}

/// A module declaring its category of the window *Settings*, titled `.0`.
struct Configured(&'static str);

impl Module for Configured {
    fn register(&mut self, reg: &mut Registrar) {
        reg.settings(
            self.0,
            vec![uniwow_api::SettingSpec::integer("far", "Far (tiles)", [1, 90], 45)],
        );
    }
}

#[test]
fn the_window_settings_shows_the_categories_of_the_running_modules_merged_by_title_and_keeps_their_values() {
    let mut harness = Harness::new(vec![
        ("aaa", Box::new(Configured("Zeta"))),
        ("beta", Box::new(Configured("Alpha"))),
        ("alpha", Box::new(Configured("Alpha"))),
        ("plain", counter("plain").0),
    ]);
    let categories = |shell: &Shell| -> Vec<(String, Vec<String>)> {
        shell
            .settings_categories()
            .into_iter()
            .map(|category| {
                let settings = category
                    .settings
                    .into_iter()
                    .map(|(module, spec)| format!("{module}/{}", spec.key))
                    .collect();
                (category.title, settings)
            })
            .collect()
    };
    assert_eq!(
        categories(&harness.shell),
        [
            ("Alpha".to_owned(), vec!["alpha/far".to_owned(), "beta/far".to_owned()]),
            ("Zeta".to_owned(), vec!["aaa/far".to_owned()])
        ],
        "by their titles, not by the ids of their modules; one title merged, its modules by their ids; none for a module declaring none"
    );
    // The first category shown, its values as stored; back to its default by its button.
    harness.shell.host.set_setting("alpha", "far", json!(8));
    harness.shell.settings_window.open = true;
    let texts = harness.frame_texts(Vec::new());
    for shown in ["Settings", "Alpha", "Zeta", "Far (tiles)", "8", "45"] {
        assert!(texts.iter().any(|(text, _)| text == shown), "'{shown}' in {texts:?}");
    }
    harness.click_text(&texts, "Default");
    assert_eq!(harness.shell.host.setting("alpha", "far"), Some(json!(45)));
    assert_eq!(
        harness.shell.host.setting("beta", "far"),
        None,
        "another module's untouched"
    );
    // A value typed in the field of the slider.
    let texts = harness.frame_texts(Vec::new());
    harness.click_text(&texts, "45");
    let key = |key, pressed| egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let select_all = egui::Event::Key {
        key: egui::Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    };
    harness.frame_texts(vec![select_all, egui::Event::Text("30".to_owned())]);
    harness.frame_texts(vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
    assert_eq!(harness.shell.host.setting("alpha", "far"), Some(json!(30)), "typed");
    assert_eq!(harness.shell.host.setting("beta", "far"), None);
    // Another category chosen.
    let texts = harness.frame_texts(Vec::new());
    harness.click_text(&texts, "Zeta");
    assert_eq!(harness.shell.settings_window.shown.as_deref(), Some("Zeta"));
    // Modules failed: their settings gone, a category kept while one of its modules runs.
    for module in ["beta", "aaa"] {
        let index = harness.index(module);
        harness.shell.fail(index, "on purpose".to_owned());
    }
    assert_eq!(
        categories(&harness.shell),
        [("Alpha".to_owned(), vec!["alpha/far".to_owned()])]
    );
    let texts = harness.frame_texts(Vec::new());
    assert!(!texts.iter().any(|(text, _)| text == "Zeta"), "{texts:?}");
}

/// A module keeping the memory of the GPU it is told when it registers.
struct Told(Arc<Mutex<Option<Option<u64>>>>);

impl Module for Told {
    fn register(&mut self, reg: &mut Registrar) {
        *lock(&self.0) = Some(reg.gpu_memory);
    }
}

/// Starts `count` jobs at its start, each giving its number, and takes `took` to take each back,
/// keeping the numbers in the order they come back; counts the passes of the kernel by its windows,
/// drawn once a pass after the jobs are handed back (egui may make two passes of a frame).
struct Slow {
    count: u32,
    took: Duration,
    back: Arc<Mutex<Vec<u32>>>,
    passes: Arc<std::sync::atomic::AtomicUsize>,
}

impl Module for Slow {
    fn register(&mut self, _reg: &mut Registrar) {}

    fn init(&mut self, ctx: &mut Context) {
        for number in 0..self.count {
            ctx.spawn("number", move |_| number);
        }
    }

    fn on_job(&mut self, _job: uniwow_api::JobId, outcome: uniwow_api::JobOutcome, _ctx: &mut Context) {
        std::thread::sleep(self.took);
        lock(&self.back).push(outcome.take::<u32>().expect("a number"));
    }

    fn windows_ui(&mut self, _egui: &egui::Context, _ctx: &mut Context) {
        self.passes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A harness running a module of `Slow`, once its `count` jobs all ended; what it took back, and
/// the passes of the kernel.
type SlowHarness = (Harness, Arc<Mutex<Vec<u32>>>, Arc<std::sync::atomic::AtomicUsize>);

fn slow(count: u32, took: Duration) -> SlowHarness {
    let back = Arc::new(Mutex::new(Vec::new()));
    let passes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let module = Slow {
        count,
        took,
        back: back.clone(),
        passes: passes.clone(),
    };
    let harness = Harness::new(vec![("slow", Box::new(module))]);
    let started = Instant::now();
    while harness.shell.host.pool.finished_count() < count as usize {
        assert!(started.elapsed() < Duration::from_secs(10), "the jobs never ended");
        std::thread::sleep(Duration::from_millis(1));
    }
    (harness, back, passes)
}

#[test]
fn the_jobs_ended_are_handed_back_for_a_few_milliseconds_a_frame_one_at_least_the_rest_at_the_next() {
    // Each taking longer than the time of a frame: one a pass of the kernel, the others still
    // listed.
    let (mut harness, back, passes) = slow(6, Duration::from_millis(10));
    let mut frames = 0;
    while lock(&back).len() < 6 {
        harness.frame(RawInput::default());
        frames += 1;
        assert!(frames <= 12, "never all handed back");
        let back = lock(&back).len();
        assert_eq!(back, passes.load(std::sync::atomic::Ordering::Relaxed).min(6));
        assert_eq!(harness.shell.host.pool.running().len(), 6 - back, "not yet handed back");
    }
    let mut numbers = lock(&back).clone();
    numbers.sort_unstable();
    assert_eq!(numbers, [0, 1, 2, 3, 4, 5], "each handed back once");
    // Quick to take back: all in one frame.
    let (mut harness, back, _) = slow(20, Duration::ZERO);
    harness.frame(RawInput::default());
    assert_eq!(lock(&back).len(), 20);
}

#[test]
fn a_module_is_told_the_memory_of_the_gpu_when_it_registers() {
    let told = Arc::new(Mutex::new(None));
    Harness::on_gpu(vec![Slot::loaded("told", Box::new(Told(told.clone())))], Some(8 << 30));
    assert_eq!(*lock(&told), Some(Some(8 << 30)));
}

/// A module with one panel on the right, which writes `.0` in it.
struct Paneled(&'static str);

impl Module for Paneled {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("main", "Main", uniwow_api::DockArea::Right);
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, _ctx: &mut Context) {
        ui.label(self.0);
    }
}

fn drawn(texts: &[(String, egui::Rect)], text: &str) -> bool {
    texts.iter().any(|(drawn, _)| drawn == text)
}

#[test]
fn a_panel_torn_off_shows_in_its_window_is_docked_back_and_both_are_saved_and_restored() {
    let main = Tab::new("p", "main");
    let mut harness = Harness::new(vec![("p", Box::new(Paneled("inside the panel")))]);
    assert!(harness.shell.dock.find_tab(&main).is_some(), "in the dock at first");
    harness
        .shell
        .tear_off(main.clone(), Some([1500.0, 120.0]), [640.0, 480.0]);
    assert!(harness.shell.dock.find_tab(&main).is_none(), "out of the dock");
    assert!(harness.shell.is_torn(&main));
    // Drawn in its window: here inside the main one, egui opening none without a system.
    harness.frame_texts(Vec::new());
    let texts = harness.frame_texts(Vec::new());
    assert!(drawn(&texts, "inside the panel"), "{texts:?}");
    // Saved with the layout, then restored in its window, opened where it was.
    harness.shell.keep_layout();
    let kept = &harness.shell.host.settings;
    assert_eq!(
        kept.windows,
        [TornOff::new(main.clone(), Some([1500.0, 120.0]), [640.0, 480.0])]
    );
    let saved = Settings {
        layout: kept.layout.clone(),
        windows: kept.windows.clone(),
        ..Settings::default()
    };
    let slots = vec![Slot::loaded("p", Box::new(Paneled("inside the panel")))];
    let mut again = Harness::with_settings(slots, saved);
    assert!(again.shell.is_torn(&main), "torn off again at start");
    assert!(
        again.shell.dock.find_tab(&main).is_none(),
        "and not in the dock as well"
    );
    assert_eq!(again.shell.torn[0].opened.position, Some([1500.0, 120.0]));
    // Its button puts it back in the dock, where the panels of its area go.
    again.frame_texts(Vec::new());
    let texts = again.frame_texts(Vec::new());
    again.click_text(&texts, theme::icon::DOCK_BACK);
    assert!(!again.shell.is_torn(&main));
    assert!(again.shell.dock.find_tab(&main).is_some(), "back in the dock");
    again.shell.keep_layout();
    assert!(again.shell.host.settings.windows.is_empty(), "no window saved any more");
    // Closed from the Window menu while torn off, it goes with its window, and comes back docked.
    again.shell.tear_off(main.clone(), None, layout::TORN_SIZE);
    again.shell.set_panel_open(&main, false);
    assert!(!again.shell.is_torn(&main) && again.shell.dock.find_tab(&main).is_none());
    again.shell.set_panel_open(&main, true);
    assert!(again.shell.dock.find_tab(&main).is_some());
}

#[test]
fn a_floating_window_of_the_dock_becomes_a_window_of_its_own_where_it_floated() {
    let main = Tab::new("p", "main");
    let mut harness = Harness::new(vec![("p", Box::new(Paneled("inside the panel")))]);
    harness.shell.native_windows = Some(true);
    // As the dock does when the tab is dropped outside of its parts, or sent away from its menu.
    let rect = egui::Rect::from_min_size(egui::pos2(200.0, 100.0), egui::vec2(500.0, 400.0));
    let path = harness.shell.dock.find_tab(&main).expect("docked");
    harness.shell.dock.detach_tab(path, rect);
    harness
        .shell
        .take_floating(Some(egui::pos2(1000.0, 50.0)), 1.0, |_| None);
    assert!(harness.shell.is_torn(&main));
    assert_eq!(
        harness
            .shell
            .dock
            .iter_surfaces()
            .filter(|s| s.node_tree().is_some())
            .count(),
        1,
        "the main one only"
    );
    let window = &harness.shell.torn[0].opened;
    assert_eq!(
        window.position,
        Some([1200.0, 150.0]),
        "at the same place on the screens"
    );
    assert_eq!(window.size, [400.0, 320.0], "at the size the dock gave it");
    // Its place kept in pixels, the main window drawn at two pixels a point.
    harness.shell.dock_back(0);
    let path = harness.shell.dock.find_tab(&main).expect("docked back");
    harness.shell.dock.detach_tab(path, rect);
    harness
        .shell
        .take_floating(Some(egui::pos2(1000.0, 50.0)), 2.0, |_| None);
    assert_eq!(harness.shell.torn[0].opened.position, Some([2400.0, 300.0]));
    // Through a frame, the window drawn once by the dock first.
    harness.shell.dock_back(0);
    let path = harness.shell.dock.find_tab(&main).expect("docked back");
    harness.shell.dock.detach_tab(path, rect);
    harness.frame_texts_at(Vec::new(), Some(egui::pos2(1000.0, 50.0)));
    assert!(harness.shell.is_torn(&main), "taken in the frame");
    let at = harness.shell.torn[0].opened.position.expect("its place");
    assert!((at[0] - 1200.0).abs() < 40.0 && (at[1] - 150.0).abs() < 40.0, "{at:?}");
    // Without windows of the system, the floating windows of the dock stay as they are.
    harness.shell.native_windows = Some(false);
    harness.shell.dock_back(0);
    let path = harness.shell.dock.find_tab(&main).expect("docked back");
    harness.shell.dock.detach_tab(path, rect);
    harness.frame_texts(Vec::new());
    assert!(!harness.shell.is_torn(&main));
    assert!(
        harness
            .shell
            .dock
            .find_tab(&main)
            .is_some_and(|path| !path.surface.is_main())
    );
}

#[test]
fn a_tab_dragged_out_of_the_main_window_is_torn_off_where_it_is_dropped() {
    let main = Tab::new("p", "main");
    let mut harness = Harness::new(vec![("p", Box::new(Paneled("inside the panel")))]);
    harness.shell.native_windows = Some(true);
    let origin = Some(egui::pos2(300.0, 200.0));
    let texts = harness.frame_texts_at(Vec::new(), origin);
    let at = texts
        .iter()
        .find(|(text, _)| text == "Main")
        .map(|(_, rect)| rect.center())
        .expect("its tab");
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let outside = egui::pos2(1400.0, 300.0);
    harness.frame_texts_at(vec![egui::Event::PointerMoved(at), button(at, true)], origin);
    harness.frame_texts_at(vec![egui::Event::PointerMoved(at + egui::vec2(40.0, 10.0))], origin);
    harness.frame_texts_at(vec![egui::Event::PointerMoved(outside)], origin);
    assert!(!harness.shell.is_torn(&main), "not while it is dragged");
    harness.frame_texts_at(vec![button(outside, false)], origin);
    assert!(harness.shell.is_torn(&main), "torn off once dropped outside");
    assert!(harness.shell.dock.find_tab(&main).is_none());
    let window = &harness.shell.torn[0].opened;
    assert_eq!(
        window.position,
        Some([300.0 + 1400.0 - 90.0, 200.0 + 300.0 - 14.0]),
        "its title under the pointer"
    );
}

/// A module telling the user, in a notice, what it did.
struct Telling;

impl Module for Telling {
    fn register(&mut self, reg: &mut Registrar) {
        reg.command("tell.saved", "Says it saved", json!({}), json!({}));
    }

    fn on_command(&mut self, _name: &str, _arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        ctx.notify(Level::Success, "Sequence 'intro' saved");
        Ok(json!({}))
    }
}

#[test]
fn undo_a_module_failing_and_a_module_telling_are_shown_in_notices_over_the_window() {
    let (module, _) = counter("a");
    let mut harness = Harness::new(vec![("a", module), ("tell", Box::new(Telling))]);
    let last = |shell: &Shell| shell.toasts.list().last().cloned().expect("a notice");
    harness.call("a", "a.add", json!({})).unwrap();
    harness.shell.undo();
    let toast = last(&harness.shell);
    assert_eq!(
        (toast.level, toast.title.as_str(), toast.text.as_str()),
        (Level::Info, "Undone", "add 1")
    );
    harness.shell.redo();
    assert_eq!(last(&harness.shell).title, "Redone");
    assert_eq!(
        harness.shell.toasts.list().len(),
        1,
        "one notice for the history, taken again"
    );
    // A module failing: its name, and why.
    assert!(harness.call("a", "a.panic", json!({})).is_err());
    let toast = last(&harness.shell);
    assert_eq!((toast.level, toast.title.as_str()), (Level::Error, "a failed"));
    assert!(toast.text.contains("the counter breaks"), "{}", toast.text);
    harness.frame_texts(Vec::new());
    let texts = harness.frame_texts(Vec::new());
    assert!(drawn(&texts, "a failed"), "drawn over the window: {texts:?}");
    // The errors logged, one notice for each source, the same text counted.
    let error = |source: &str, message: &str| logger::LogLine {
        seconds: 0.0,
        level: log::Level::Error,
        source: source.to_owned(),
        message: message.to_owned(),
    };
    harness.shell.notice_errors(vec![
        error("tell", "the file is locked"),
        error("tell", "the file is locked"),
        error("kernel", "GPU error"),
        error("wgpu_hal", "device lost"),
    ]);
    let titles: Vec<(String, u32)> = harness
        .shell
        .toasts
        .list()
        .iter()
        .filter(|toast| toast.title.starts_with("Error in"))
        .map(|toast| (toast.title.clone(), toast.count))
        .collect();
    assert_eq!(
        titles,
        [
            ("Error in tell".to_owned(), 2),
            ("Error in the editor".to_owned(), 1),
            ("Error in wgpu_hal".to_owned(), 1)
        ]
    );
    // A module telling what it did, under its name, the times counted.
    harness.call("kernel", "tell.saved", json!({})).unwrap();
    harness.call("kernel", "tell.saved", json!({})).unwrap();
    harness.frame(RawInput::default());
    let toast = last(&harness.shell);
    assert_eq!(
        (toast.level, toast.title.as_str(), toast.text.as_str(), toast.count),
        (Level::Success, "tell", "Sequence 'intro' saved", 2)
    );
}

fn repaint_delay(output: &egui::FullOutput) -> Duration {
    output.viewport_output[&ViewportId::ROOT].repaint_delay
}

#[test]
fn an_idle_editor_asks_for_no_frame_and_a_job_running_for_the_frames_of_its_indicators() {
    let mut harness = Harness::new(vec![("p", Box::new(Paneled("inside the panel")))]);
    // What moves at first, such as the line of the tab shown growing, ends within a few frames.
    for _ in 0..40 {
        harness.frame_output(RawInput::default());
    }
    let idle = harness.frame_output(RawInput::default());
    assert_eq!(repaint_delay(&idle), Duration::MAX, "nothing moves, no frame asked for");
    let (release, wait) = std::sync::mpsc::channel::<()>();
    harness.shell.host.spawn(
        "p",
        "waits",
        Box::new(move |_| {
            let _ = wait.recv_timeout(Duration::from_secs(10));
            Box::new(())
        }),
    );
    let busy = harness.frame_output(RawInput::default());
    assert!(repaint_delay(&busy) <= theme::time::TICK, "{:?}", repaint_delay(&busy));
    let texts = harness.frame_texts(Vec::new());
    assert!(drawn(&texts, "1 job"), "the status bar tells it: {texts:?}");
    release.send(()).unwrap();
    harness.until("the job's end", |shell| shell.host.pool.running().is_empty());
    for _ in 0..40 {
        harness.frame_output(RawInput::default());
    }
    let idle = harness.frame_output(RawInput::default());
    assert_eq!(repaint_delay(&idle), Duration::MAX, "idle again");
    let texts = harness.frame_texts(Vec::new());
    assert!(drawn(&texts, "No job running"));
}

#[test]
fn the_window_settings_closed_fades_out_then_is_drawn_no_more() {
    let mut harness = Harness::new(vec![("alpha", Box::new(Configured("Alpha")))]);
    harness.shell.settings_window.open = true;
    harness.frame_texts(Vec::new());
    assert!(drawn(&harness.frame_texts(Vec::new()), "Settings"));
    harness.shell.settings_window.open = false;
    assert!(drawn(&harness.frame_texts(Vec::new()), "Settings"), "fading out");
    for _ in 0..30 {
        harness.frame_texts(Vec::new());
    }
    assert!(!drawn(&harness.frame_texts(Vec::new()), "Settings"), "gone");
    let idle = harness.frame_output(RawInput::default());
    assert_eq!(repaint_delay(&idle), Duration::MAX, "and nothing asked for after");
}

/// A module that keeps the payloads of the kernel's topic of the window minimised, counts its
/// shutdowns, panics when asked, and in its shutdown too when told.
struct Watcher {
    seen: Arc<Mutex<Vec<Value>>>,
    shut: Arc<Mutex<u32>>,
    panics_in_shutdown: bool,
}

/// The payloads a watcher kept, and how many times it was shut down.
type Watched = (Arc<Mutex<Vec<Value>>>, Arc<Mutex<u32>>);

fn watcher(panics_in_shutdown: bool) -> (Box<dyn Module>, Watched) {
    let (seen, shut) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(0)));
    let module = Watcher {
        seen: seen.clone(),
        shut: shut.clone(),
        panics_in_shutdown,
    };
    (Box::new(module), (seen, shut))
}

impl Module for Watcher {
    fn register(&mut self, reg: &mut Registrar) {
        reg.subscribe(MINIMISED_TOPIC);
        reg.subscribe(uniwow_api::MODULE_STOPPED_TOPIC);
        reg.command("watcher.panic", "Panics", json!({}), json!({}));
    }

    fn on_event(&mut self, event: &Event, _ctx: &mut Context) {
        lock(&self.seen).push(event.payload.clone());
    }

    fn on_command(&mut self, _name: &str, _arguments: Value, _ctx: &mut Context) -> Result<Value, String> {
        panic!("the watcher breaks");
    }

    fn shutdown(&mut self) {
        *lock(&self.shut) += 1;
        if self.panics_in_shutdown {
            panic!("its shutdown breaks too");
        }
    }
}

/// The input of a frame of the window minimised.
fn minimised_input() -> RawInput {
    let mut input = RawInput::default();
    input.viewports.entry(ViewportId::ROOT).or_default().minimized = Some(true);
    input
}

#[test]
fn the_modules_are_told_when_the_editor_goes_out_of_sight_and_back_even_while_minimised() {
    let (module, (seen, _)) = watcher(false);
    let mut harness = Harness::new(vec![("watcher", module)]);
    harness.frame(RawInput::default());
    assert!(lock(&seen).is_empty(), "shown: nothing told");
    // Minimised, eframe running `logic` alone: told then, once.
    harness.minimised(minimised_input());
    assert_eq!(*lock(&seen), [json!({ "minimised": true })]);
    harness.minimised(minimised_input());
    assert_eq!(lock(&seen).len(), 1, "once");
    // Shown again.
    harness.frame(RawInput::default());
    assert_eq!(
        *lock(&seen),
        [json!({ "minimised": true }), json!({ "minimised": false })]
    );
    harness.frame(RawInput::default());
    assert_eq!(lock(&seen).len(), 2, "once");
}

#[test]
fn a_module_that_fails_is_shut_down_once_a_failure_there_only_logged() {
    let (module, (_, shut)) = watcher(false);
    let (breaking, (_, broken)) = watcher(true);
    let (counting, value) = counter("a");
    let mut harness = Harness::new(vec![("watcher", module), ("a", counting)]);
    assert!(harness.call("watcher", "watcher.panic", json!({})).is_err());
    assert!(matches!(slot(&harness.shell, "watcher").state, State::Failed(_)));
    assert_eq!(*lock(&shut), 1, "shut down as it stops");
    // Not again when the editor closes, the others then.
    eframe::App::on_exit(&mut harness.shell);
    assert_eq!(*lock(&shut), 1);
    // A shutdown that panics: logged, the module failed all the same, the others working.
    let mut harness = Harness::new(vec![
        ("watcher", breaking),
        (
            "a",
            Box::new(Counter {
                id: "a",
                value: value.clone(),
                saved: 0,
            }),
        ),
    ]);
    assert!(harness.call("watcher", "watcher.panic", json!({})).is_err());
    assert_eq!(*lock(&broken), 1);
    assert!(matches!(slot(&harness.shell, "watcher").state, State::Failed(_)));
    harness.call("a", "a.add", json!({ "by": 2 })).unwrap();
    harness.frame(RawInput::default());
    assert_eq!(*lock(&value), 2);
}

/// What a gesture of the module `Gesturer` does at a frame: its label, its own change, the calls it
/// makes (a command and how much it adds), and whether it ends the gesture or leaves it open.
#[derive(Clone, Default)]
struct Plan {
    label: &'static str,
    own: Option<i64>,
    calls: Vec<(&'static str, i64)>,
    end: bool,
    /// Panics once the gesture is open, as a module failing in the middle of one.
    panic: bool,
    /// Keeps its gesture open, taken up again at each frame, as a drag held over frames.
    hold: bool,
    /// Takes up the gesture a plan before held.
    resume: bool,
}

/// Opens a gesture at each frame a plan is waiting, as the Inspector does when a value is typed.
struct Gesturer {
    plans: Arc<Mutex<std::collections::VecDeque<Plan>>>,
    value: Arc<Mutex<i64>>,
    /// The gesture a plan left open, taken up again at each frame.
    open: Option<uniwow_api::Gesture>,
}

impl Module for Gesturer {
    fn register(&mut self, _reg: &mut Registrar) {}

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        // A drag held, still or moving: taken up again at each frame.
        if let Some(open) = self.open
            && !ctx.resume_gesture(open)
        {
            self.open = None;
        }
        let Some(plan) = lock(&self.plans).pop_front() else {
            return;
        };
        let gesture = match self.open {
            Some(open) if plan.resume => open,
            _ => ctx.begin_gesture(plan.label),
        };
        if let Some(by) = plan.own {
            ctx.execute(Own(by));
        }
        for (name, by) in &plan.calls {
            ctx.call(name, json!({ "by": by }));
        }
        if plan.panic {
            panic!("the gesturer breaks in its gesture");
        }
        if plan.end {
            ctx.end_gesture(gesture);
            self.open = None;
        } else if plan.hold || plan.resume {
            self.open = Some(gesture);
        }
    }
}

struct Own(i64);

impl Command for Own {
    fn label(&self) -> String {
        format!("own {}", self.0)
    }

    fn apply(&mut self, module: &mut dyn Any) {
        let gesturer: &mut Gesturer = module.downcast_mut().expect("the gesturer");
        *lock(&gesturer.value) += self.0;
    }

    fn revert(&mut self, module: &mut dyn Any) {
        let gesturer: &mut Gesturer = module.downcast_mut().expect("the gesturer");
        *lock(&gesturer.value) -= self.0;
    }
}

/// Offers `relay.add`, which adds through another module's command, called in turn.
struct Relay;

impl Module for Relay {
    fn register(&mut self, reg: &mut Registrar) {
        reg.command("relay.add", "Adds through a.add", json!({}), json!({}));
    }

    fn on_command(&mut self, _name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        ctx.call("a.add", arguments);
        Ok(json!({}))
    }
}

type Plans = Arc<Mutex<std::collections::VecDeque<Plan>>>;

/// The modules `a` and `b` (counters), `relay`, and the gesturer `g`.
fn gesturing() -> (Harness, Plans, [Arc<Mutex<i64>>; 3]) {
    let (a, a_value) = counter("a");
    let (b, b_value) = counter("b");
    let plans: Plans = Arc::default();
    let own = Arc::new(Mutex::new(0));
    let gesturer = Gesturer {
        plans: plans.clone(),
        value: own.clone(),
        open: None,
    };
    let harness = Harness::new(vec![
        ("a", a),
        ("b", b),
        ("relay", Box::new(Relay)),
        ("g", Box::new(gesturer)),
    ]);
    (harness, plans, [a_value, b_value, own])
}

fn values(values: &[Arc<Mutex<i64>>; 3]) -> [i64; 3] {
    [0, 1, 2].map(|at| *lock(&values[at]))
}

#[test]
fn a_gesture_across_modules_and_the_calls_of_its_calls_is_one_entry() {
    let (mut harness, plans, counted) = gesturing();
    lock(&plans).push_back(Plan {
        label: "Move 3 doodads",
        own: Some(100),
        calls: vec![("a.add", 1), ("b.add", 2), ("relay.add", 4)],
        end: true,
        ..Plan::default()
    });
    harness.until("the gesture's entry", |shell| {
        shell.history.undo_label().is_some() && shell.host.gestures.is_empty()
    });
    assert_eq!(values(&counted), [5, 2, 100]);
    assert_eq!(harness.shell.history.undo_label().as_deref(), Some("Move 3 doodads"));
    harness.shell.undo();
    assert_eq!(values(&counted), [0, 0, 0], "every part undone at once");
    assert_eq!(harness.shell.history.undo_label(), None, "one entry");
    harness.shell.redo();
    assert_eq!(values(&counted), [5, 2, 100]);
    // A change outside any gesture is an entry of its own.
    harness.call("a", "a.add", json!({ "by": 10 })).unwrap();
    harness.shell.undo();
    assert_eq!(values(&counted), [5, 2, 100]);
}

#[test]
fn a_gesture_blocks_undo_while_it_holds_a_change_and_one_left_open_is_closed() {
    let (mut harness, plans, counted) = gesturing();
    lock(&plans).push_back(Plan {
        label: "left open",
        calls: vec![("a.add", 1)],
        ..Plan::default()
    });
    harness.until("the call served", |_| values(&counted)[0] == 1);
    assert!(
        harness.shell.blocking_undo().is_some() || harness.shell.host.gestures.is_empty(),
        "Undo waits while the gesture holds its change"
    );
    harness.until("the gesture closed", |shell| shell.host.gestures.is_empty());
    assert_eq!(harness.shell.history.undo_label().as_deref(), Some("left open"));
    harness.shell.undo();
    assert_eq!(values(&counted)[0], 0);
}

#[test]
fn a_module_failing_in_its_gesture_closes_it_its_calls_refused_with_it() {
    let (mut harness, plans, counted) = gesturing();
    lock(&plans).push_back(Plan {
        label: "broken",
        own: Some(7),
        calls: vec![("a.add", 3)],
        panic: true,
        ..Plan::default()
    });
    harness.until("the gesturer failed", |shell| {
        matches!(shell.slots[3].state, State::Failed(_))
    });
    harness.until("the gesture closed", |shell| shell.host.gestures.is_empty());
    for _ in 0..5 {
        harness.frame(RawInput::default());
    }
    // Its own change never applied, its call refused with it: nothing to undo.
    assert_eq!(values(&counted), [0, 0, 0]);
    assert_eq!(harness.shell.history.undo_label(), None);
    assert!(harness.shell.blocking_undo().is_none());
}

/// Gestures and changes of their own at random, seeded: each gesture one entry, every entry undone
/// leaving nothing, every one redone giving the same values back.
#[test]
fn gestures_and_lone_changes_at_random_are_each_one_entry() {
    let mut random = Random::new(15_05);
    for _round in 0..20 {
        let (mut harness, plans, counted) = gesturing();
        let mut entries = 0;
        let mut expected = [0i64; 3];
        for step in 0..12 {
            if random.below(3) == 0 {
                let by = random.below(9) as i64 + 1;
                harness.call("b", "b.add", json!({ "by": by })).unwrap();
                expected[1] += by;
                entries += 1;
                continue;
            }
            let mut plan = Plan {
                label: ["one", "two", "three"][step % 3],
                end: true,
                ..Plan::default()
            };
            if random.below(2) == 0 {
                let by = random.below(5) as i64 + 1;
                plan.own = Some(by);
                expected[2] += by;
            }
            for _ in 0..random.below(4) {
                let (name, at) = [("a.add", 0), ("b.add", 1), ("relay.add", 0)][random.below(3) as usize];
                let by = random.below(5) as i64 + 1;
                plan.calls.push((name, by));
                expected[at] += by;
            }
            if plan.own.is_none() && plan.calls.is_empty() {
                continue;
            }
            lock(&plans).push_back(plan);
            entries += 1;
            harness.until("the gesture's entry", |shell| {
                lock(&plans).is_empty() && shell.host.gestures.is_empty()
            });
        }
        assert_eq!(values(&counted), expected);
        for _ in 0..entries {
            assert!(harness.shell.history.undo_label().is_some());
            harness.shell.undo();
        }
        assert_eq!(values(&counted), [0, 0, 0], "every entry undone");
        assert_eq!(harness.shell.history.undo_label(), None, "{entries} entries");
        for _ in 0..entries {
            harness.shell.redo();
        }
        assert_eq!(values(&counted), expected, "every entry redone");
    }
}

#[test]
fn the_modules_are_told_when_another_stops_so_that_what_they_hold_for_it_goes() {
    let (watching, (seen, _)) = watcher(false);
    let (stopping, _) = counter("a");
    let mut harness = Harness::new(vec![("watching", watching), ("a", stopping)]);
    let index = harness.index("a");
    harness.shell.fail(index, "disabled from the Modules panel".to_owned());
    harness.frame(RawInput::default());
    assert_eq!(*lock(&seen), [json!({ "id": "a" })]);
}

#[test]
fn a_gesture_taken_up_at_each_frame_of_a_drag_is_one_entry_once_let_go() {
    let (mut harness, plans, counted) = gesturing();
    lock(&plans).push_back(Plan {
        label: "Drag",
        calls: vec![("a.add", 1)],
        hold: true,
        ..Plan::default()
    });
    harness.until("the first change", |_| values(&counted)[0] == 1);
    // The pointer held still: frames without changes, the gesture kept open by being taken up.
    for _ in 0..10 {
        harness.frame(RawInput::default());
    }
    assert!(
        !harness.shell.host.gestures.is_empty(),
        "not closed as left open while it is taken up"
    );
    assert_eq!(harness.shell.history.undo_label(), None, "no entry before it is let go");
    lock(&plans).push_back(Plan {
        calls: vec![("a.add", 2)],
        resume: true,
        ..Plan::default()
    });
    lock(&plans).push_back(Plan {
        calls: vec![("a.add", 3)],
        resume: true,
        end: true,
        ..Plan::default()
    });
    harness.until("the drag let go", |shell| {
        shell.history.undo_label().is_some() && shell.host.gestures.is_empty()
    });
    assert_eq!(values(&counted)[0], 6);
    assert_eq!(harness.shell.history.undo_label().as_deref(), Some("Drag"));
    harness.shell.undo();
    assert_eq!(values(&counted)[0], 0, "the whole drag undone at once");
    assert_eq!(harness.shell.history.undo_label(), None, "one entry");
}
