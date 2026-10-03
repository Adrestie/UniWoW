//! The C interface of `sdk/uniwow.h`, implemented over an `Editor` handle, and the loading of a
//! module DLL.

use std::ffi::{CStr, CString, c_char, c_void};
use std::os::windows::ffi::OsStrExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use uniwow_api::serde_json::{self, Value, json};
use uniwow_api::{Editor, log};

const API_VERSION: u32 = 1;
const INIT_SYMBOL: &[u8] = b"uniwow_module_init\0";

type Reply = extern "C" fn(*mut c_void, *const c_char);
type Handler = extern "C" fn(*mut c_void, *const c_char, Reply, *mut c_void) -> i32;
type InitFn = unsafe extern "C" fn(*const Api, *mut ModuleInfo, Reply, *mut c_void) -> i32;

#[repr(C)]
struct Api {
    version: u32,
    context: *mut c_void,
    commands: extern "C" fn(*mut c_void, Reply, *mut c_void),
    call: extern "C" fn(*mut c_void, *const c_char, *const c_char, Reply, *mut c_void) -> i32,
    publish: extern "C" fn(*mut c_void, *const c_char, *const c_char),
    subscribe: extern "C" fn(*mut c_void, *const c_char) -> u64,
    next_event: extern "C" fn(*mut c_void, u64, u32, Reply, *mut c_void) -> i32,
    unsubscribe: extern "C" fn(*mut c_void, u64),
    setting: extern "C" fn(*mut c_void, *const c_char, Reply, *mut c_void),
    set_setting: extern "C" fn(*mut c_void, *const c_char, *const c_char),
    log: extern "C" fn(*mut c_void, i32, *const c_char),
    begin_group: extern "C" fn(*mut c_void, *const c_char),
    end_group: extern "C" fn(*mut c_void),
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

#[repr(C)]
struct ModuleInfo {
    name: *const c_char,
    version: *const c_char,
    commands: *const CommandEntry,
    command_count: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(file: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn GetLastError() -> u32;
}

/// What the C functions know of the module calling them. Lives until the process ends.
pub struct ModuleContext {
    /// Name used for the module's `Editor`, e.g. `native-modules#sample-cpp`.
    pub name: String,
    pub editor: OnceLock<Editor>,
}

/// A command handler of a module.
#[derive(Clone, Copy)]
pub struct NativeHandler {
    handler: Handler,
    user: *mut c_void,
}

// SAFETY: uniwow.h requires every command handler to be callable from any thread, several at once.
unsafe impl Send for NativeHandler {}
unsafe impl Sync for NativeHandler {}

impl NativeHandler {
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

pub struct ModuleCommand {
    pub name: String,
    pub description: String,
    pub arguments: Value,
    pub result: Value,
    pub handler: NativeHandler,
}

pub struct Loaded {
    pub name: String,
    pub version: String,
    pub context: &'static ModuleContext,
    pub commands: Vec<ModuleCommand>,
}

/// Loads a module DLL and calls its entry point. A loaded module is never unloaded.
pub fn load(path: &Path) -> Result<Loaded, String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: a NUL-terminated wide path.
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        // SAFETY: no other call in between.
        return Err(format!("could not be loaded (Windows error {})", unsafe {
            GetLastError()
        }));
    }
    // SAFETY: a valid module handle and a NUL-terminated name.
    let init = unsafe { GetProcAddress(module, INIT_SYMBOL.as_ptr().cast()) };
    if init.is_null() {
        return Err("no uniwow_module_init entry point: not a UniWoW module".to_owned());
    }
    // SAFETY: uniwow.h fixes the signature of the entry point.
    let init: InitFn = unsafe { std::mem::transmute::<*mut c_void, InitFn>(init) };

    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let context: &'static ModuleContext = Box::leak(Box::new(ModuleContext {
        name: stem.into_owned(),
        editor: OnceLock::new(),
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
    }));

    let mut info = ModuleInfo {
        name: std::ptr::null(),
        version: std::ptr::null(),
        commands: std::ptr::null(),
        command_count: 0,
    };
    let mut error = String::new();
    // SAFETY: the module receives valid pointers that outlive it.
    let status = unsafe { init(api, &mut info, collect, text_target(&mut error)) };
    if status != 0 {
        return Err(format!("refused to start: {error}"));
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
        commands.push(ModuleCommand {
            name: read(entry.name).map_err(|e| format!("command {index}: {e}"))?,
            description: read(entry.description).unwrap_or_default(),
            arguments: schema(entry.arguments_schema),
            result: schema(entry.result_schema),
            handler: NativeHandler {
                handler,
                user: entry.user,
            },
        });
    }
    Ok(Loaded {
        name: read(info.name).unwrap_or_else(|_| context.name.clone()),
        version: read(info.version).unwrap_or_default(),
        context,
        commands,
    })
}

extern "C" fn collect(target: *mut c_void, text: *const c_char) {
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

fn reply_with(reply: Reply, reply_context: *mut c_void, text: &str) {
    let text = c_text(text);
    reply(reply_context, text.as_ptr());
}

fn editor(context: *mut c_void) -> Result<&'static Editor, String> {
    // SAFETY: the `context` field of the table, a leaked `ModuleContext`.
    let context = unsafe { &*context.cast::<ModuleContext>() };
    context
        .editor
        .get()
        .ok_or_else(|| "the editor is not ready yet: call it from the module's commands or threads".to_owned())
}

/// Runs the body of a C function: a panic must not cross into the module's code.
fn guarded<R>(fallback: R, body: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| {
        log::error!("a call of a native module panicked in the editor");
        fallback
    })
}

extern "C" fn api_commands(context: *mut c_void, reply: Reply, reply_context: *mut c_void) {
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
    reply: Reply,
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
            editor(context)?.publish(&read(topic)?, payload);
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            log::warn!("a native module could not publish: {error}");
        }
    })
}

extern "C" fn api_subscribe(context: *mut c_void, topic: *const c_char) -> u64 {
    guarded(0, || match (editor(context), read(topic)) {
        (Ok(editor), Ok(topic)) => editor.subscribe(&topic),
        _ => 0,
    })
}

extern "C" fn api_next_event(
    context: *mut c_void,
    subscription: u64,
    timeout_ms: u32,
    reply: Reply,
    reply_context: *mut c_void,
) -> i32 {
    guarded(0, || {
        let Ok(editor) = editor(context) else {
            return 0;
        };
        match editor.next_event(subscription, Duration::from_millis(u64::from(timeout_ms))) {
            Some(event) => {
                let event = json!({ "topic": event.topic, "source": event.source, "payload": event.payload });
                reply_with(reply, reply_context, &event.to_string());
                1
            }
            None => 0,
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

extern "C" fn api_setting(context: *mut c_void, key: *const c_char, reply: Reply, reply_context: *mut c_void) {
    guarded((), || {
        let value = editor(context)
            .and_then(|editor| editor.setting(&read(key)?))
            .unwrap_or_else(|error| {
                log::warn!("a native module could not read a setting: {error}");
                None
            });
        reply_with(reply, reply_context, &value.unwrap_or(Value::Null).to_string());
    })
}

extern "C" fn api_set_setting(context: *mut c_void, key: *const c_char, value: *const c_char) {
    guarded((), || {
        let result = (|| {
            let value: Value = serde_json::from_str(&read(value)?).map_err(|e| format!("invalid JSON value: {e}"))?;
            editor(context)?.set_setting(&read(key)?, value);
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            log::warn!("a native module could not write a setting: {error}");
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
        if let (Ok(editor), Ok(label)) = (editor(context), read(label)) {
            editor.begin_group(&label);
        }
    })
}

extern "C" fn api_end_group(context: *mut c_void) {
    guarded((), || {
        if let Ok(editor) = editor(context) {
            editor.end_group();
        }
    })
}
