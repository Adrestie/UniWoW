//! The C interface of `sdk/uniwow.h` over an `Editor`: what a compiled module receives, and the
//! start of a compiled module once the kernel has loaded its DLL.

mod objects;
mod properties;

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use uniwow_api::serde_json::{self, Value, json};

use uniwow_api::ui::{self, Post, SharedUi, Ui};
use uniwow_api::{DockArea, Editor, PanelSpec, log};

const API_VERSION: u32 = 4;
/// The name under which a compiled module exports its entry point.
pub const INIT_SYMBOL: &[u8] = b"uniwow_module_init\0";

// The functions a module gives are "C-unwind": a C++ exception reaching the editor through them
// ends the process in a defined way, instead of being undefined behaviour.
type Reply = extern "C-unwind" fn(*mut c_void, *const c_char);
type Handler = extern "C-unwind" fn(*mut c_void, *const c_char, Reply, *mut c_void) -> i32;
/// Applies an undo or redo value of the module: the value, then where to reply an error.
type ApplyFn = extern "C-unwind" fn(*mut c_void, *const c_char, Reply, *mut c_void) -> i32;
/// The entry point of a compiled module.
pub type InitFn = unsafe extern "C-unwind" fn(*const Api, *mut ModuleInfo, Reply, *mut c_void) -> i32;

/// The table of `uniwow.h`. A reply function a module passes may be NULL: the text is then
/// ignored.
#[repr(C)]
pub struct Api {
    version: u32,
    context: *mut c_void,
    commands: extern "C" fn(*mut c_void, Option<Reply>, *mut c_void),
    call: extern "C" fn(*mut c_void, *const c_char, *const c_char, Option<Reply>, *mut c_void) -> i32,
    publish: extern "C" fn(*mut c_void, *const c_char, *const c_char),
    subscribe: extern "C" fn(*mut c_void, *const c_char) -> u64,
    next_event: extern "C" fn(*mut c_void, u64, u32, Option<Reply>, *mut c_void) -> i32,
    unsubscribe: extern "C" fn(*mut c_void, u64),
    setting: extern "C" fn(*mut c_void, *const c_char, Option<Reply>, *mut c_void),
    set_setting: extern "C" fn(*mut c_void, *const c_char, *const c_char),
    log: extern "C" fn(*mut c_void, i32, *const c_char),
    begin_group: extern "C" fn(*mut c_void, *const c_char),
    end_group: extern "C" fn(*mut c_void),
    record_change: extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char) -> i32,
    objects: objects::Table,
    properties: extern "C" fn(*mut c_void, Option<Reply>, *mut c_void),
    read_property: extern "C" fn(*mut c_void, *const c_char, *mut f64, u32) -> u32,
    write_property: extern "C" fn(*mut c_void, *const c_char, *const f64, u32) -> i32,
    set_property: extern "C" fn(*mut c_void, *const c_char, *const f64, u32) -> i32,
}

#[repr(C)]
struct CommandEntry {
    name: *const c_char,
    description: *const c_char,
    arguments_schema: *const c_char,
    result_schema: *const c_char,
    handler: Option<Handler>,
    user: *mut c_void,
}

/// A panel a module declares: its id, its title, and its area (1 centre, 2 left, 3 right,
/// 4 bottom).
#[repr(C)]
struct PanelEntry {
    id: *const c_char,
    title: *const c_char,
    area: u32,
}

/// What the entry point of a compiled module fills in.
#[repr(C)]
pub struct ModuleInfo {
    name: *const c_char,
    version: *const c_char,
    commands: *const CommandEntry,
    command_count: u32,
    /// `UNIWOW_API_VERSION` of the header the module was built with; 0 from an older header.
    header_version: u32,
    /// `sizeof(uniwow_command)` in the module, so that its table is read with the right step.
    command_size: u32,
    panel_count: u32,
    panels: *const PanelEntry,
    /// Applies an undo or redo value recorded with `record_change`, on the module's thread.
    apply_change: Option<ApplyFn>,
    user: *mut c_void,
    properties: *const properties::PropertyEntry,
    property_count: u32,
    /// `sizeof(uniwow_property)` in the module.
    property_size: u32,
}

/// A pointer a module gives back to its own functions, which uniwow.h requires to accept any
/// thread.
#[derive(Clone, Copy)]
struct UserPointer(*mut c_void);

// SAFETY: uniwow.h requires the module's functions to accept being called from any thread.
unsafe impl Send for UserPointer {}
unsafe impl Sync for UserPointer {}

/// What the C functions know of the module calling them. Lives until the process ends.
pub struct ModuleContext {
    /// The module's id, for its log lines.
    pub id: String,
    /// The module's `Editor`, set when the module starts (`Module::init`).
    pub editor: OnceLock<Editor>,
    /// Its interface objects; jobs for the module's own thread are posted through them.
    pub ui: SharedUi,
    /// What waits for or runs on the module's thread.
    pub activity: Arc<Activity>,
    apply: OnceLock<(ApplyFn, UserPointer)>,
    /// Its animatable properties by name, once it has started.
    properties: OnceLock<HashMap<String, Arc<properties::CompiledProperty>>>,
}

impl ModuleContext {
    /// Logs a refusal under the module's name.
    fn refuse(&self, what: &str, error: &str) {
        match self.editor.get() {
            Some(editor) => editor.log(log::Level::Warn, &format!("{what}: {error}")),
            None => log::warn!("module '{}': {what}: {error}", self.id),
        }
    }

    /// Whether the module still runs; once it failed, nothing reaches it any more.
    fn active(&self) -> bool {
        self.editor.get().is_none_or(Editor::is_active)
    }
}

/// A command handler of a compiled module.
#[derive(Clone, Copy)]
pub struct CompiledHandler {
    handler: Handler,
    user: *mut c_void,
}

// SAFETY: uniwow.h requires every command handler to be callable from any thread, several at once.
unsafe impl Send for CompiledHandler {}
unsafe impl Sync for CompiledHandler {}

impl CompiledHandler {
    pub fn invoke(&self, arguments: &Value) -> Result<Value, String> {
        let arguments = c_text(&arguments.to_string());
        let mut answer = String::new();
        let status = (self.handler)(self.user, arguments.as_ptr(), collect, text_target(&mut answer));
        if status != 0 {
            return Err(answer);
        }
        Ok(serde_json::from_str(&answer).unwrap_or(Value::String(answer)))
    }
}

/// A command a compiled module offers.
pub struct OfferedCommand {
    pub name: String,
    pub description: String,
    pub arguments: Value,
    pub result: Value,
    pub handler: CompiledHandler,
}

/// A compiled module whose entry point accepted to start.
pub struct Started {
    /// Name and version the module gives itself.
    pub name: String,
    pub version: String,
    pub context: &'static ModuleContext,
    pub commands: Vec<OfferedCommand>,
    pub panels: Vec<PanelSpec>,
    pub properties: Vec<Arc<properties::CompiledProperty>>,
}

/// The work waiting for or running on a compiled module's thread: Undo and Redo wait for it,
/// and the Modules panel tells a module that no longer answers.
#[derive(Default)]
pub struct Activity {
    /// Jobs posted and not finished, the running one included.
    pending: AtomicUsize,
    /// When the running job started.
    since: Mutex<Option<Instant>>,
}

impl Activity {
    /// How many jobs wait or run.
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    /// How long the running job has been running.
    pub fn running_for(&self) -> Option<Duration> {
        self.since
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|since| since.elapsed())
    }
}

/// The thread a module's signals, paintings and undo values run on, in order.
fn module_thread(id: &str) -> (Post, Arc<Activity>) {
    let (sender, receiver) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
    let name = id.to_owned();
    let activity = Arc::new(Activity::default());
    let worker = activity.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("uniwow module {id}"))
        .spawn(move || {
            for job in receiver {
                *worker.since.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                if catch_unwind(AssertUnwindSafe(job)).is_err() {
                    log::error!("module '{name}': a call on its thread panicked in the editor");
                }
                *worker.since.lock().unwrap_or_else(|e| e.into_inner()) = None;
                worker.pending.fetch_sub(1, Ordering::Release);
            }
        });
    if let Err(error) = spawned {
        log::error!("module '{id}': its thread could not start: {error}");
    }
    let counter = activity.clone();
    let post: Post = Arc::new(move |job| {
        counter.pending.fetch_add(1, Ordering::AcqRel);
        if sender.send(job).is_err() {
            counter.pending.fetch_sub(1, Ordering::Release);
        }
    });
    (post, activity)
}

/// Calls the entry point of the compiled module `id` with the table of the C interface, and reads
/// what it offers. The table and the context live until the process ends, as the module does.
pub fn start(init: InitFn, id: &str) -> Result<Started, String> {
    let (post, activity) = module_thread(id);
    let context: &'static ModuleContext = Box::leak(Box::new(ModuleContext {
        id: id.to_owned(),
        editor: OnceLock::new(),
        ui: Ui::new(post),
        activity,
        apply: OnceLock::new(),
        properties: OnceLock::new(),
    }));
    let api: &'static Api = Box::leak(Box::new(Api {
        version: API_VERSION,
        context: std::ptr::from_ref(context).cast_mut().cast(),
        commands: api_commands,
        call: api_call,
        publish: api_publish,
        subscribe: api_subscribe,
        next_event: api_next_event,
        unsubscribe: api_unsubscribe,
        setting: api_setting,
        set_setting: api_set_setting,
        log: api_log,
        begin_group: api_begin_group,
        end_group: api_end_group,
        record_change: api_record_change,
        objects: objects::TABLE,
        properties: properties::api_properties,
        read_property: properties::api_read_property,
        write_property: properties::api_write_property,
        set_property: properties::api_set_property,
    }));

    let mut info = ModuleInfo {
        name: std::ptr::null(),
        version: std::ptr::null(),
        commands: std::ptr::null(),
        command_count: 0,
        header_version: 0,
        command_size: 0,
        panel_count: 0,
        panels: std::ptr::null(),
        apply_change: None,
        user: std::ptr::null_mut(),
        properties: std::ptr::null(),
        property_count: 0,
        property_size: 0,
    };
    let mut error = String::new();
    // SAFETY: the module receives valid pointers that outlive it.
    let status = unsafe { init(api, &mut info, collect, text_target(&mut error)) };
    if status != 0 {
        return Err(format!("refused to start: {error}"));
    }
    if info.header_version != API_VERSION {
        return Err(format!(
            "built with version {} of uniwow.h, the editor has version {API_VERSION}: rebuild it",
            info.header_version
        ));
    }
    if info.command_size as usize != std::mem::size_of::<CommandEntry>() {
        return Err(format!(
            "its uniwow_command is {} bytes, the editor's is {}: rebuild it with this uniwow.h",
            info.command_size,
            std::mem::size_of::<CommandEntry>()
        ));
    }
    if let Some(apply) = info.apply_change {
        let _ = context.apply.set((apply, UserPointer(info.user)));
    }

    let mut commands = Vec::new();
    for index in 0..info.command_count as usize {
        // SAFETY: the module declared `command_count` entries, valid while it is loaded.
        let entry = unsafe { &*info.commands.add(index) };
        let Some(handler) = entry.handler else {
            return Err(format!("command {index} has no handler"));
        };
        let schema = |text: *const c_char| {
            read(text)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(json!({}))
        };
        commands.push(OfferedCommand {
            name: read(entry.name).map_err(|e| format!("command {index}: {e}"))?,
            description: read(entry.description).unwrap_or_default(),
            arguments: schema(entry.arguments_schema),
            result: schema(entry.result_schema),
            handler: CompiledHandler {
                handler,
                user: entry.user,
            },
        });
    }
    let mut panels = Vec::new();
    for index in 0..info.panel_count as usize {
        // SAFETY: the module declared `panel_count` panels, valid while it is loaded.
        let entry = unsafe { &*info.panels.add(index) };
        let id = read(entry.id).map_err(|e| format!("panel {index}: {e}"))?;
        panels.push(PanelSpec {
            title: read(entry.title).unwrap_or_else(|_| id.clone()),
            id,
            area: match entry.area {
                2 => DockArea::Left,
                3 => DockArea::Right,
                4 => DockArea::Bottom,
                _ => DockArea::Center,
            },
            open_by_default: true,
        });
    }
    let properties = properties::declared(info.properties, info.property_count, info.property_size, context)?;
    let _ = context.properties.set(properties::by_name(&properties));
    Ok(Started {
        name: read(info.name).unwrap_or_default(),
        version: read(info.version).unwrap_or_default(),
        context,
        commands,
        panels,
        properties,
    })
}

/// A change a compiled module recorded: undoing or redoing hands the matching value to its
/// `apply_change`, on its thread.
struct CompiledChange {
    context: &'static ModuleContext,
    undo: CString,
    redo: CString,
}

impl CompiledChange {
    fn send(&self, value: CString) {
        let context = self.context;
        ui::lock(&context.ui).post_job(Box::new(move || {
            let (Some((apply, user)), Some(editor)) = (context.apply.get(), context.editor.get()) else {
                return;
            };
            if !editor.is_active() {
                return;
            }
            let mut error = String::new();
            if apply(user.0, value.as_ptr(), collect, text_target(&mut error)) != 0 {
                editor.report_failure(&format!("could not apply an undo or redo value: {error}"));
            }
        }));
    }
}

impl uniwow_api::AppliedChange for CompiledChange {
    fn undo(&mut self) {
        self.send(self.undo.clone());
    }

    fn redo(&mut self) {
        self.send(self.redo.clone());
    }
}

extern "C" fn api_record_change(
    context: *mut c_void,
    label: *const c_char,
    undo: *const c_char,
    redo: *const c_char,
) -> i32 {
    guarded(1, || {
        let module = module(context);
        let result = (|| {
            if module.apply.get().is_none() {
                return Err("the module gives no apply_change".to_owned());
            }
            let change = CompiledChange {
                context: module,
                undo: c_text(&read(undo)?),
                redo: c_text(&read(redo)?),
            };
            editor(context)?.record_change(&read(label)?, Box::new(change))
        })();
        match result {
            Ok(()) => 0,
            Err(error) => {
                module.refuse("a change could not be recorded", &error);
                1
            }
        }
    })
}

/// The module calling a C function.
fn module(context: *mut c_void) -> &'static ModuleContext {
    // SAFETY: the `context` field of the table, a leaked `ModuleContext`.
    unsafe { &*context.cast::<ModuleContext>() }
}

extern "C-unwind" fn collect(target: *mut c_void, text: *const c_char) {
    if target.is_null() || text.is_null() {
        return;
    }
    // SAFETY: `target` is the `String` given with this callback, `text` a NUL-terminated string.
    unsafe { *target.cast::<String>() = CStr::from_ptr(text).to_string_lossy().into_owned() };
}

fn text_target(text: &mut String) -> *mut c_void {
    std::ptr::from_mut(text).cast()
}

fn read(text: *const c_char) -> Result<String, String> {
    if text.is_null() {
        return Err("missing text".to_owned());
    }
    // SAFETY: a NUL-terminated string given by the module.
    unsafe { CStr::from_ptr(text) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| "text is not UTF-8".to_owned())
}

fn c_text(text: &str) -> CString {
    CString::new(text.replace('\0', " ")).expect("NUL bytes removed")
}

/// Hands a text to the module's reply function, unless it gave none.
fn reply_with(reply: Option<Reply>, reply_context: *mut c_void, text: &str) {
    if let Some(reply) = reply {
        let text = c_text(text);
        reply(reply_context, text.as_ptr());
    }
}

fn editor(context: *mut c_void) -> Result<&'static Editor, String> {
    module(context)
        .editor
        .get()
        .ok_or_else(|| "the editor is not ready yet: call it from the module's commands or threads".to_owned())
}

/// Runs the body of a C function: a panic must not cross into the module's code.
fn guarded<R>(fallback: R, body: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| {
        log::error!("a call of a compiled module panicked in the editor");
        fallback
    })
}

extern "C" fn api_commands(context: *mut c_void, reply: Option<Reply>, reply_context: *mut c_void) {
    guarded((), || {
        let commands: Vec<Value> = editor(context)
            .map(|editor| editor.commands())
            .unwrap_or_default()
            .into_iter()
            .map(|c| {
                json!({ "name": c.name, "owner": c.owner, "description": c.description,
                        "arguments": c.arguments, "result": c.result, "on_caller": c.on_caller })
            })
            .collect();
        reply_with(reply, reply_context, &Value::Array(commands).to_string());
    })
}

extern "C" fn api_call(
    context: *mut c_void,
    name: *const c_char,
    arguments: *const c_char,
    reply: Option<Reply>,
    reply_context: *mut c_void,
) -> i32 {
    guarded(1, || {
        let result = (|| {
            let editor = editor(context)?;
            let arguments: Value =
                serde_json::from_str(&read(arguments)?).map_err(|e| format!("invalid JSON arguments: {e}"))?;
            editor.call(&read(name)?, arguments)
        })();
        match result {
            Ok(value) => {
                reply_with(reply, reply_context, &value.to_string());
                0
            }
            Err(error) => {
                reply_with(reply, reply_context, &error);
                1
            }
        }
    })
}

extern "C" fn api_publish(context: *mut c_void, topic: *const c_char, payload: *const c_char) {
    guarded((), || {
        let result = (|| {
            let payload: Value =
                serde_json::from_str(&read(payload)?).map_err(|e| format!("invalid JSON payload: {e}"))?;
            editor(context)?.publish(&read(topic)?, payload)
        })();
        if let Err(error) = result {
            log::warn!("a compiled module could not publish: {error}");
        }
    })
}

extern "C" fn api_subscribe(context: *mut c_void, topic: *const c_char) -> u64 {
    guarded(0, || {
        let subscribed = read(topic).and_then(|topic| editor(context)?.subscribe(&topic));
        subscribed.unwrap_or_else(|error| {
            log::warn!("a compiled module could not subscribe: {error}");
            0
        })
    })
}

extern "C" fn api_next_event(
    context: *mut c_void,
    subscription: u64,
    timeout_ms: u32,
    reply: Option<Reply>,
    reply_context: *mut c_void,
) -> i32 {
    guarded(-1, || {
        let next = editor(context)
            .and_then(|editor| editor.next_event(subscription, Duration::from_millis(u64::from(timeout_ms))));
        match next {
            Ok(Some(event)) => {
                let event = json!({ "topic": event.topic, "source": event.source, "payload": event.payload });
                reply_with(reply, reply_context, &event.to_string());
                1
            }
            Ok(None) => 0,
            Err(error) => {
                reply_with(reply, reply_context, &error);
                -1
            }
        }
    })
}

extern "C" fn api_unsubscribe(context: *mut c_void, subscription: u64) {
    guarded((), || {
        if let Ok(editor) = editor(context) {
            editor.unsubscribe(subscription);
        }
    })
}

extern "C" fn api_setting(context: *mut c_void, key: *const c_char, reply: Option<Reply>, reply_context: *mut c_void) {
    guarded((), || {
        let value = editor(context)
            .and_then(|editor| editor.setting(&read(key)?))
            .unwrap_or_else(|error| {
                log::warn!("a compiled module could not read a setting: {error}");
                None
            });
        reply_with(reply, reply_context, &value.unwrap_or(Value::Null).to_string());
    })
}

extern "C" fn api_set_setting(context: *mut c_void, key: *const c_char, value: *const c_char) {
    guarded((), || {
        let result = (|| {
            let value: Value = serde_json::from_str(&read(value)?).map_err(|e| format!("invalid JSON value: {e}"))?;
            editor(context)?.set_setting(&read(key)?, value)
        })();
        if let Err(error) = result {
            log::warn!("a compiled module could not write a setting: {error}");
        }
    })
}

extern "C" fn api_log(context: *mut c_void, level: i32, message: *const c_char) {
    guarded((), || {
        let level = match level {
            1 => log::Level::Error,
            2 => log::Level::Warn,
            4 => log::Level::Debug,
            _ => log::Level::Info,
        };
        if let (Ok(editor), Ok(message)) = (editor(context), read(message)) {
            editor.log(level, &message);
        }
    })
}

extern "C" fn api_begin_group(context: *mut c_void, label: *const c_char) {
    guarded((), || {
        if let Err(error) = read(label).and_then(|label| editor(context)?.begin_group(&label)) {
            log::warn!("a compiled module could not open an undo group: {error}");
        }
    })
}

extern "C" fn api_end_group(context: *mut c_void) {
    guarded((), || {
        if let Err(error) = editor(context).and_then(Editor::end_group) {
            log::warn!("a compiled module could not end an undo group: {error}");
        }
    })
}

/// A compiled module defined in the process, for the tests of the kernel.
#[cfg(test)]
pub(crate) mod testing {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};

    use super::properties::PropertyEntry;
    use super::{API_VERSION, Api, CommandEntry, ModuleContext, ModuleInfo, Reply, Started, api_record_change, start};

    /// What the module keeps: a value its undo and redo values change, a gate `native.wait` waits
    /// on, and its property `level`, with the number of writes it received.
    pub struct Native {
        value: AtomicI64,
        open: Mutex<bool>,
        opened: Condvar,
        level: Mutex<f64>,
        writes: AtomicUsize,
    }

    thread_local! {
        /// The state of the module being started, for `init`.
        static STARTING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn native_of(user: *mut c_void) -> &'static Native {
        // SAFETY: `user` is the leaked `Native` given in `init`.
        unsafe { &*user.cast::<Native>() }
    }

    /// Answers the name of the thread it runs on.
    extern "C-unwind" fn where_it_runs(
        _user: *mut c_void,
        _arguments: *const c_char,
        reply: Reply,
        context: *mut c_void,
    ) -> i32 {
        let name = std::thread::current().name().unwrap_or_default().to_owned();
        let answer = CString::new(format!("\"{name}\"")).expect("no NUL");
        reply(context, answer.as_ptr());
        0
    }

    /// Waits until the test opens the gate.
    extern "C-unwind" fn wait(user: *mut c_void, _arguments: *const c_char, reply: Reply, context: *mut c_void) -> i32 {
        let native = native_of(user);
        let mut open = native.open.lock().unwrap_or_else(|e| e.into_inner());
        while !*open {
            open = native.opened.wait(open).unwrap_or_else(|e| e.into_inner());
        }
        reply(context, c"null".as_ptr());
        0
    }

    /// Adds an undo or redo value, a number, to the module's value.
    extern "C-unwind" fn apply(user: *mut c_void, value: *const c_char, _error: Reply, _context: *mut c_void) -> i32 {
        // SAFETY: the editor passes a NUL-terminated string.
        let text = unsafe { CStr::from_ptr(value) }.to_string_lossy();
        native_of(user)
            .value
            .fetch_add(text.parse::<i64>().unwrap_or(0), Ordering::SeqCst);
        0
    }

    /// Writes `level`, rounded to a whole number; 7 is an error.
    extern "C-unwind" fn write_level(
        user: *mut c_void,
        values: *mut f64,
        _count: u32,
        error: Reply,
        context: *mut c_void,
    ) -> i32 {
        let native = native_of(user);
        native.writes.fetch_add(1, Ordering::SeqCst);
        // SAFETY: the editor passes one number for a number.
        let value = unsafe { &mut *values };
        if *value == 7.0 {
            error(context, c"seven is refused".as_ptr());
            return 1;
        }
        *value = value.round();
        *native.level.lock().unwrap_or_else(|e| e.into_inner()) = *value;
        0
    }

    unsafe extern "C-unwind" fn init(
        _api: *const Api,
        info: *mut ModuleInfo,
        _error: Reply,
        _context: *mut c_void,
    ) -> i32 {
        let user = STARTING.get() as *mut c_void;
        let command = |name: &'static CStr, handler| CommandEntry {
            name: name.as_ptr(),
            description: c"A command of the tests".as_ptr(),
            arguments_schema: c"{}".as_ptr(),
            result_schema: c"{}".as_ptr(),
            handler: Some(handler),
            user,
        };
        let commands: &'static [CommandEntry] = Box::leak(Box::new([
            command(c"native.where", where_it_runs),
            command(c"native.wait", wait),
        ]));
        // SAFETY: the editor gives a valid `info`.
        let info = unsafe { &mut *info };
        info.name = c"native".as_ptr();
        info.version = c"1.0".as_ptr();
        info.commands = commands.as_ptr();
        info.command_count = commands.len() as u32;
        info.header_version = API_VERSION;
        info.command_size = std::mem::size_of::<CommandEntry>() as u32;
        info.apply_change = Some(apply);
        info.user = user;
        let properties: &'static [PropertyEntry] = Box::leak(Box::new([PropertyEntry {
            name: c"level".as_ptr(),
            label: c"Level".as_ptr(),
            kind: uniwow_api::PropertyKind::Number as u32,
            minimum: 0.0,
            maximum: 10.0,
            initial: [2.0, 0.0, 0.0],
            write: Some(write_level),
            user,
        }]));
        info.properties = properties.as_ptr();
        info.property_count = 1;
        info.property_size = std::mem::size_of::<PropertyEntry>() as u32;
        0
    }

    /// The module `id`, offering `native.where` and `native.wait`.
    pub fn native(id: &str) -> Started {
        let state: &'static Native = Box::leak(Box::new(Native {
            value: AtomicI64::new(0),
            open: Mutex::new(false),
            opened: Condvar::new(),
            level: Mutex::new(2.0),
            writes: AtomicUsize::new(0),
        }));
        STARTING.set(std::ptr::from_ref(state) as usize);
        start(init, id).expect("starts")
    }

    fn state(module: &ModuleContext) -> &'static Native {
        native_of(module.apply.get().expect("given at start").1.0)
    }

    /// The module's value.
    pub fn value(module: &ModuleContext) -> i64 {
        state(module).value.load(Ordering::SeqCst)
    }

    /// The level the module has, and how many writes it received.
    pub fn level(module: &ModuleContext) -> (f64, usize) {
        let native = state(module);
        (
            *native.level.lock().unwrap_or_else(|e| e.into_inner()),
            native.writes.load(Ordering::SeqCst),
        )
    }

    /// The JSON `properties` replies to the module.
    pub fn properties_json(module: &'static ModuleContext) -> String {
        let mut text = String::new();
        super::properties::api_properties(context_of(module), Some(super::collect), super::text_target(&mut text));
        text
    }

    /// `read_property`: how many numbers, and the first one.
    pub fn read_number(module: &'static ModuleContext, path: &CStr) -> (u32, f64) {
        let mut values = [0.0; 3];
        let count = super::properties::api_read_property(context_of(module), path.as_ptr(), values.as_mut_ptr(), 3);
        (count, values[0])
    }

    /// `write_property` of numbers.
    pub fn write_numbers(module: &'static ModuleContext, path: &CStr, values: &[f64]) -> i32 {
        super::properties::api_write_property(context_of(module), path.as_ptr(), values.as_ptr(), values.len() as u32)
    }

    /// `set_property` of numbers.
    pub fn tell_numbers(module: &'static ModuleContext, name: &CStr, values: &[f64]) -> i32 {
        super::properties::api_set_property(context_of(module), name.as_ptr(), values.as_ptr(), values.len() as u32)
    }

    fn context_of(module: &'static ModuleContext) -> *mut c_void {
        std::ptr::from_ref(module).cast_mut().cast()
    }

    /// Lets `native.wait` return.
    pub fn open(module: &ModuleContext) {
        let native = state(module);
        *native.open.lock().unwrap_or_else(|e| e.into_inner()) = true;
        native.opened.notify_all();
    }

    /// Adds `by` to the module's value, then records it through the C function, as a module does.
    pub fn change(module: &'static ModuleContext, label: &str, by: i64) {
        state(module).value.fetch_add(by, Ordering::SeqCst);
        let context = std::ptr::from_ref(module).cast_mut().cast::<c_void>();
        let (label, undo, redo) = (
            CString::new(label).expect("no NUL"),
            CString::new((-by).to_string()).expect("no NUL"),
            CString::new(by.to_string()).expect("no NUL"),
        );
        assert_eq!(
            api_record_change(context, label.as_ptr(), undo.as_ptr(), redo.as_ptr()),
            0
        );
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{CStr, c_char, c_void};
    use std::sync::atomic::{AtomicPtr, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use uniwow_api::serde_json::{Value, json};

    use super::{API_VERSION, Api, CommandEntry, ModuleInfo, PanelEntry, Reply, start};
    use uniwow_api::{AppliedChange, CommandInfo, DockArea, Editor, EditorBackend, Event};

    extern "C-unwind" fn echo(_user: *mut c_void, arguments: *const c_char, reply: Reply, context: *mut c_void) -> i32 {
        reply(context, arguments);
        0
    }

    /// Fills `info` as a module would, with one command, then lets `adjust` spoil it.
    fn fill(info: *mut ModuleInfo, adjust: impl FnOnce(&mut ModuleInfo)) -> i32 {
        let commands: &'static [CommandEntry] = Box::leak(Box::new([CommandEntry {
            name: c"test.echo".as_ptr(),
            description: c"Answers its arguments".as_ptr(),
            arguments_schema: c"{}".as_ptr(),
            result_schema: c"{}".as_ptr(),
            handler: Some(echo),
            user: std::ptr::null_mut(),
        }]));
        // SAFETY: the editor gives a valid `info`.
        let info = unsafe { &mut *info };
        info.name = c"test".as_ptr();
        info.version = c"1.0".as_ptr();
        info.commands = commands.as_ptr();
        info.command_count = 1;
        info.header_version = API_VERSION;
        info.command_size = std::mem::size_of::<CommandEntry>() as u32;
        adjust(info);
        0
    }

    unsafe extern "C-unwind" fn good(
        _api: *const Api,
        info: *mut ModuleInfo,
        _error: Reply,
        _context: *mut c_void,
    ) -> i32 {
        fill(info, |_| {})
    }

    unsafe extern "C-unwind" fn older_header(
        _api: *const Api,
        info: *mut ModuleInfo,
        _error: Reply,
        _context: *mut c_void,
    ) -> i32 {
        fill(info, |info| info.header_version = 1)
    }

    unsafe extern "C-unwind" fn other_command_size(
        _api: *const Api,
        info: *mut ModuleInfo,
        _error: Reply,
        _context: *mut c_void,
    ) -> i32 {
        fill(info, |info| info.command_size += 8)
    }

    unsafe extern "C-unwind" fn refuses(
        _api: *const Api,
        _info: *mut ModuleInfo,
        error: Reply,
        context: *mut c_void,
    ) -> i32 {
        error(context, c"no licence file".as_ptr());
        1
    }

    #[test]
    fn the_work_of_a_module_thread_is_counted_until_it_ends() {
        let started = start(good, "test").expect("starts");
        let activity = &started.context.activity;
        assert_eq!(activity.pending(), 0);
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let mut ui = uniwow_api::ui::lock(&started.context.ui);
        ui.post_job(Box::new(move || {
            let _ = wait.recv();
        }));
        ui.post_job(Box::new(|| {}));
        drop(ui);
        assert_eq!(activity.pending(), 2);
        let started_at = Instant::now();
        while activity.running_for().is_none() {
            assert!(
                started_at.elapsed() < Duration::from_secs(5),
                "the first job never started"
            );
            std::thread::yield_now();
        }
        release.send(()).unwrap();
        while activity.pending() > 0 {
            assert!(started_at.elapsed() < Duration::from_secs(5), "the jobs never ended");
            std::thread::yield_now();
        }
        assert!(activity.running_for().is_none());
    }

    #[test]
    fn a_module_starts_and_its_commands_answer() {
        let started = start(good, "test").expect("starts");
        assert_eq!((started.name.as_str(), started.version.as_str()), ("test", "1.0"));
        assert_eq!(started.commands.len(), 1);
        assert_eq!(started.commands[0].name, "test.echo");
        assert_eq!(
            started.commands[0].handler.invoke(&json!({ "a": 1 })),
            Ok(json!({ "a": 1 }))
        );
    }

    unsafe extern "C-unwind" fn with_panels(
        _api: *const Api,
        info: *mut ModuleInfo,
        _error: Reply,
        _context: *mut c_void,
    ) -> i32 {
        let panels: &'static [PanelEntry] = Box::leak(Box::new([
            PanelEntry {
                id: c"board".as_ptr(),
                title: c"Board".as_ptr(),
                area: 2,
            },
            PanelEntry {
                id: c"other".as_ptr(),
                title: std::ptr::null(),
                area: 9,
            },
        ]));
        fill(info, |info| {
            info.panels = panels.as_ptr();
            info.panel_count = 2;
        })
    }

    #[test]
    fn a_module_declares_its_panels() {
        let started = start(with_panels, "test").expect("starts");
        let panels: Vec<_> = started
            .panels
            .iter()
            .map(|p| (p.id.as_str(), p.title.as_str(), p.area))
            .collect();
        assert_eq!(
            panels,
            vec![("board", "Board", DockArea::Left), ("other", "other", DockArea::Center)]
        );
    }

    static CHANGES_API: AtomicPtr<Api> = AtomicPtr::new(std::ptr::null_mut());
    static APPLIED: Mutex<Vec<(String, Option<String>)>> = Mutex::new(Vec::new());

    /// Applies a value on the module's side, refusing `"fail"`.
    extern "C-unwind" fn apply(_user: *mut c_void, value: *const c_char, error: Reply, context: *mut c_void) -> i32 {
        // SAFETY: the editor passes a valid text.
        let value = unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned();
        if value == "\"fail\"" {
            error(context, c"cannot".as_ptr());
            return 1;
        }
        let thread = std::thread::current().name().map(str::to_owned);
        APPLIED.lock().expect("applied").push((value, thread));
        0
    }

    unsafe extern "C-unwind" fn with_changes(
        api: *const Api,
        info: *mut ModuleInfo,
        _error: Reply,
        _context: *mut c_void,
    ) -> i32 {
        CHANGES_API.store(api.cast_mut(), Ordering::SeqCst);
        fill(info, |info| info.apply_change = Some(apply))
    }

    /// What a module asks of the editor about its changes.
    #[derive(Default)]
    struct Recorder {
        changes: Mutex<Vec<(String, Box<dyn AppliedChange>)>>,
        failures: Mutex<Vec<String>>,
    }

    impl EditorBackend for Recorder {
        fn commands(&self) -> Vec<CommandInfo> {
            Vec::new()
        }

        fn call(&self, _caller: &str, _name: &str, _arguments: Value) -> Result<Value, String> {
            Err("no commands here".to_owned())
        }

        fn publish(&self, _source: &str, _topic: &str, _payload: Value) -> Result<(), String> {
            Ok(())
        }

        fn subscribe(&self, _caller: &str, _topic: &str) -> Result<u64, String> {
            Ok(0)
        }

        fn next_event(&self, _caller: &str, _subscription: u64, _timeout: Duration) -> Result<Option<Event>, String> {
            Ok(None)
        }

        fn unsubscribe(&self, _subscription: u64) {}

        fn setting(&self, _caller: &str, _space: &str, _key: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }

        fn set_setting(&self, _caller: &str, _space: &str, _key: &str, _value: Value) -> Result<(), String> {
            Ok(())
        }

        fn begin_group(&self, _caller: &str, _label: &str) -> Result<(), String> {
            Ok(())
        }

        fn end_group(&self, _caller: &str) -> Result<(), String> {
            Ok(())
        }

        fn record_change(&self, _caller: &str, label: &str, change: Box<dyn AppliedChange>) -> Result<(), String> {
            self.changes.lock().expect("changes").push((label.to_owned(), change));
            Ok(())
        }

        fn report_failure(&self, _caller: &str, message: &str) {
            self.failures.lock().expect("failures").push(message.to_owned());
        }
    }

    /// Waits at most two seconds for `done`.
    fn wait_for(done: impl Fn() -> bool) -> bool {
        let start = Instant::now();
        while !done() {
            if start.elapsed() > Duration::from_secs(2) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    #[test]
    fn a_recorded_change_is_applied_on_the_module_thread_and_a_failure_reported() {
        let started = start(with_changes, "changes").expect("starts");
        let recorder = Arc::new(Recorder::default());
        let _ = started.context.editor.set(Editor::new(recorder.clone(), "changes"));
        // SAFETY: the table lives until the process ends.
        let api = unsafe { &*CHANGES_API.load(Ordering::SeqCst) };
        let status = (api.record_change)(
            api.context,
            c"set".as_ptr(),
            c"\"before\"".as_ptr(),
            c"\"fail\"".as_ptr(),
        );
        assert_eq!(status, 0);
        let (label, mut change) = recorder.changes.lock().expect("changes").pop().expect("recorded");
        assert_eq!(label, "set");

        change.undo();
        assert!(wait_for(|| !APPLIED.lock().expect("applied").is_empty()));
        assert_eq!(
            APPLIED.lock().expect("applied")[0],
            ("\"before\"".to_owned(), Some("uniwow module changes".to_owned()))
        );

        change.redo();
        assert!(wait_for(|| !recorder.failures.lock().expect("failures").is_empty()));
        let failure = recorder.failures.lock().expect("failures")[0].clone();
        assert!(
            failure.contains("could not apply an undo or redo value: cannot"),
            "{failure}"
        );
    }

    #[test]
    fn a_module_built_with_another_header_is_refused_with_the_reason() {
        let older = start(older_header, "test").err().expect("refused");
        assert!(older.contains("version 1 of uniwow.h"), "{older}");
        let size = start(other_command_size, "test").err().expect("refused");
        assert!(size.contains("uniwow_command"), "{size}");
        let refused = start(refuses, "test").err().expect("refused");
        assert!(refused.contains("no licence file"), "{refused}");
    }
}
