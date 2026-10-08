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
        let ctx = egui::Context::default();
        let (bridge, requests) = Bridge::new(Some(ctx.clone()));
        let pool = Pool::new(2, Some(ctx.clone()), Some(bridge.clone()));
        let settings = Settings {
            in_memory: true,
            ..Settings::default()
        };
        let host = KernelHost::new(None, settings, pool, bridge);
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
