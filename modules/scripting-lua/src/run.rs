//! One run of Lua code on a thread of its own, in a Lua state of its own (rule T6), with the `uniwow`
//! module translating Lua values to and from the JSON of the generic interface (rule S1).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use uniwow_api::mlua::chunk::ChunkMode;
use uniwow_api::mlua::{self, HookTriggers, Lua, LuaSerdeExt, MultiValue, Table, VmState};
use uniwow_api::serde_json::Value;
use uniwow_api::{Editor, log};

use crate::loading;
use crate::output::{Kind, Output};

/// Lua instructions between two checks of the cancellation.
const HOOK_INTERVAL: u32 = 1_000;
/// Longest wait between two checks of the cancellation while waiting for an event.
const EVENT_SLICE: Duration = Duration::from_millis(50);
const STOPPED: &str = "stopped";
/// The memory a run may take: beyond it, the script gets the error "not enough memory" instead of
/// the editor running out of it.
const MEMORY_LIMIT: usize = 1 << 30;

/// Run before the script: once the run is stopped, `pcall`, `xpcall` and `coroutine.resume` raise
/// the stop again instead of catching it, so that no script can go on by catching it.
const PRELUDE: &str = r#"
local stopped, STOPPED = ...
local raw_pcall, raw_xpcall, raw_resume, error = pcall, xpcall, coroutine.resume, error
local function rethrow(...)
    if stopped() then
        error(STOPPED, 0)
    end
    return ...
end
pcall = function(...) return rethrow(raw_pcall(...)) end
xpcall = function(...) return rethrow(raw_xpcall(...)) end
coroutine.resume = function(...) return rethrow(raw_resume(...)) end
"#;

/// The cancellation of a run, checked by the hook and by every function of `uniwow`.
#[derive(Clone)]
struct Stop(Arc<AtomicBool>);

impl Stop {
    fn requested(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn check(&self) -> mlua::Result<()> {
        if self.requested() {
            Err(mlua::Error::runtime(STOPPED))
        } else {
            Ok(())
        }
    }
}

pub enum Source {
    /// A script file: its name and its content.
    /// `tool` is the folder of its tool, where `require` looks first (S9).
    Script {
        name: String,
        text: Vec<u8>,
        tool: Option<PathBuf>,
    },
    /// One console line: evaluated as an expression whose values are printed, or else as a
    /// statement.
    Console(String),
}

/// Runs the code and prints its output, then how it ended, prefixed with `run_name`.
pub fn run(source: Source, editor: Editor, cancelled: Arc<AtomicBool>, output: Arc<Output>, run_name: &str) {
    let started = Instant::now();
    match execute(&source, &editor, &cancelled, &output, MEMORY_LIMIT) {
        Ok(()) => {
            if let Source::Script { .. } = source {
                output.push(
                    Kind::Info,
                    format!("{run_name}: done in {:.2} s", started.elapsed().as_secs_f64()),
                );
            }
        }
        Err(_) if cancelled.load(Ordering::Relaxed) => output.push(Kind::Info, format!("{run_name}: stopped")),
        Err(error) => output.push(Kind::Error, format!("{run_name}: {}", describe(&error))),
    }
}

/// Runs `source` in a Lua state of its own, which may take `memory_limit` bytes.
fn execute(
    source: &Source,
    editor: &Editor,
    cancelled: &Arc<AtomicBool>,
    output: &Arc<Output>,
    memory_limit: usize,
) -> mlua::Result<()> {
    // The standard libraries mlua deems safe, without `debug`: C modules cannot be loaded, and Lua
    // code is loaded from source text only (rule S6).
    let lua = Lua::new();
    lua.set_memory_limit(memory_limit)?;
    loading::install(&lua)?;
    let stop = Stop(cancelled.clone());
    // A global hook also runs in the coroutines, which a hook of the main thread does not reach.
    let hook = stop.clone();
    lua.set_global_hook(HookTriggers::new().every_nth_instruction(HOOK_INTERVAL), move |_, _| {
        hook.check().map(|()| VmState::Continue)
    })?;
    let requested = stop.clone();
    lua.load(PRELUDE)
        .set_name("=uniwow")
        .call::<()>((lua.create_function(move |_, ()| Ok(requested.requested()))?, STOPPED))?;

    let globals = lua.globals();
    let printed = output.clone();
    globals.set(
        "print",
        lua.create_function(move |lua, values: MultiValue| {
            printed.push(Kind::Normal, as_text(lua, values)?);
            Ok(())
        })?,
    )?;
    let package: Table = globals.get("package")?;
    let mut folders = Vec::new();
    if let Source::Script { tool: Some(tool), .. } = source {
        folders.push(tool.clone());
    }
    folders.push(crate::scripts_dir());
    let path: Vec<String> = folders
        .iter()
        .map(|folder| format!("{0}\\?.lua;{0}\\?\\init.lua", folder.display()))
        .collect();
    package.set("path", path.join(";"))?;
    let subscriptions = Rc::new(RefCell::new(HashSet::new()));
    globals.set("uniwow", module(&lua, editor, &stop, &subscriptions)?)?;

    editor
        .begin_group(&match source {
            Source::Script { name, .. } => format!("Lua: {name}"),
            Source::Console(line) => format!("Lua console: {}", shorten(line)),
        })
        .map_err(mlua::Error::runtime)?;
    // Ends the run however it ends: normally, by an error returned with `?`, or by a panic.
    let _end = RunEnd {
        editor,
        subscriptions: subscriptions.clone(),
    };
    match source {
        Source::Script { name, text, .. } => match loading::load_text(&lua, text, &format!("@{name}"))? {
            Ok(function) => function.call::<()>(()),
            Err(message) => Err(mlua::Error::runtime(message)),
        },
        Source::Console(line) => evaluate(&lua, line, output),
    }
}

/// The end of a run, done when it is dropped: its undo group ends and the subscriptions it left
/// open close.
struct RunEnd<'a> {
    editor: &'a Editor,
    subscriptions: Rc<RefCell<HashSet<u64>>>,
}

impl Drop for RunEnd<'_> {
    fn drop(&mut self) {
        // Refused only when the module failed meanwhile: its groups are already closed then.
        let _ = self.editor.end_group();
        for subscription in self.subscriptions.borrow().iter() {
            self.editor.unsubscribe(*subscription);
        }
    }
}

fn evaluate(lua: &Lua, line: &str, output: &Output) -> mlua::Result<()> {
    let chunk = |code: String| lua.load(code).set_name("=console").set_mode(ChunkMode::Text);
    let values: MultiValue = match chunk(format!("return {line}")).into_function() {
        Ok(function) => function.call(())?,
        Err(_) => chunk(line.to_owned()).call(())?,
    };
    if values.is_empty() {
        return Ok(());
    }
    // A table is shown as JSON, like the results of uniwow.call; any other value as print does.
    let tostring: mlua::Function = lua.globals().get("tostring")?;
    let mut texts = Vec::with_capacity(values.len());
    for value in values {
        let json = match &value {
            mlua::Value::Table(_) => from_lua(lua, value.clone()).ok().map(|json| json.to_string()),
            _ => None,
        };
        texts.push(match json {
            Some(json) => json,
            None => tostring.call::<String>(value)?,
        });
    }
    output.push(Kind::Normal, texts.join("\t"));
    Ok(())
}

/// The values as `print` writes them: through `tostring`, separated by tabs.
fn as_text(lua: &Lua, values: MultiValue) -> mlua::Result<String> {
    let tostring: mlua::Function = lua.globals().get("tostring")?;
    let mut texts = Vec::with_capacity(values.len());
    for value in values {
        texts.push(tostring.call::<String>(value)?);
    }
    Ok(texts.join("\t"))
}

/// The `uniwow` module: the generic interface, nothing more.
/// `subscriptions` receives the subscriptions the run has open.
fn module(lua: &Lua, editor: &Editor, stop: &Stop, subscriptions: &Rc<RefCell<HashSet<u64>>>) -> mlua::Result<Table> {
    let module = lua.create_table()?;

    let (e, s) = (editor.clone(), stop.clone());
    module.set(
        "commands",
        lua.create_function(move |lua, ()| {
            s.check()?;
            let commands: Vec<Value> = e
                .commands()
                .into_iter()
                .map(|c| {
                    uniwow_api::serde_json::json!({ "name": c.name, "owner": c.owner,
                        "description": c.description, "arguments": c.arguments, "result": c.result,
                        "on_caller": c.on_caller })
                })
                .collect();
            to_lua(lua, &Value::Array(commands))
        })?,
    )?;

    let (e, s) = (editor.clone(), stop.clone());
    module.set(
        "call",
        lua.create_function(move |lua, (name, arguments): (String, Option<mlua::Value>)| {
            s.check()?;
            let arguments = match arguments {
                Some(value) => from_lua(lua, value)?,
                None => Value::Object(Default::default()),
            };
            let result = e.call(&name, arguments).map_err(mlua::Error::runtime)?;
            to_lua(lua, &result)
        })?,
    )?;

    let (e, s) = (editor.clone(), stop.clone());
    module.set(
        "publish",
        lua.create_function(move |lua, (topic, payload): (String, Option<mlua::Value>)| {
            s.check()?;
            let payload = match payload {
                Some(value) => from_lua(lua, value)?,
                None => Value::Null,
            };
            e.publish(&topic, payload).map_err(mlua::Error::runtime)
        })?,
    )?;

    let (e, s) = (editor.clone(), stop.clone());
    let open = subscriptions.clone();
    module.set(
        "subscribe",
        lua.create_function(move |_, topic: String| {
            s.check()?;
            let subscription = e.subscribe(&topic).map_err(mlua::Error::runtime)?;
            open.borrow_mut().insert(subscription);
            Ok(subscription)
        })?,
    )?;

    let (e, s) = (editor.clone(), stop.clone());
    module.set(
        "next_event",
        lua.create_function(move |lua, (subscription, timeout_ms): (u64, Option<u64>)| {
            let deadline = timeout_ms.map(|ms| Instant::now() + Duration::from_millis(ms));
            loop {
                s.check()?;
                let slice = match deadline {
                    Some(deadline) => deadline.saturating_duration_since(Instant::now()).min(EVENT_SLICE),
                    None => EVENT_SLICE,
                };
                if let Some(event) = e.next_event(subscription, slice).map_err(mlua::Error::runtime)? {
                    let event = uniwow_api::serde_json::json!({ "topic": event.topic,
                        "source": event.source, "payload": event.payload });
                    return to_lua(lua, &event);
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    return Ok(mlua::Value::Nil);
                }
            }
        })?,
    )?;

    let (e, s) = (editor.clone(), stop.clone());
    let open = subscriptions.clone();
    module.set(
        "unsubscribe",
        lua.create_function(move |_, subscription: u64| {
            s.check()?;
            e.unsubscribe(subscription);
            open.borrow_mut().remove(&subscription);
            Ok(())
        })?,
    )?;

    let (e, s) = (editor.clone(), stop.clone());
    module.set(
        "setting",
        lua.create_function(move |lua, key: String| {
            s.check()?;
            let value = e.setting(&key).map_err(mlua::Error::runtime)?;
            to_lua(lua, &value.unwrap_or(Value::Null))
        })?,
    )?;

    let (e, s) = (editor.clone(), stop.clone());
    module.set(
        "set_setting",
        lua.create_function(move |lua, (key, value): (String, mlua::Value)| {
            s.check()?;
            e.set_setting(&key, from_lua(lua, value)?).map_err(mlua::Error::runtime)
        })?,
    )?;

    let (e, s) = (editor.clone(), stop.clone());
    module.set(
        "log",
        lua.create_function(move |_, (level, message): (String, String)| {
            s.check()?;
            let level = match level.as_str() {
                "error" => log::Level::Error,
                "warn" | "warning" => log::Level::Warn,
                "info" => log::Level::Info,
                "debug" => log::Level::Debug,
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "unknown log level '{other}': error, warn, info or debug"
                    )));
                }
            };
            e.log(level, &message);
            Ok(())
        })?,
    )?;

    // The groups the script opened itself: `end_group` never ends the group of the run.
    let opened = Rc::new(Cell::new(0_u32));
    let (e, s, o) = (editor.clone(), stop.clone(), opened.clone());
    module.set(
        "begin_group",
        lua.create_function(move |_, label: String| {
            s.check()?;
            e.begin_group(&label).map_err(mlua::Error::runtime)?;
            o.set(o.get() + 1);
            Ok(())
        })?,
    )?;

    let (e, s, o) = (editor.clone(), stop.clone(), opened);
    module.set(
        "end_group",
        lua.create_function(move |_, ()| {
            s.check()?;
            if o.get() == 0 {
                return Err(mlua::Error::runtime(
                    "end_group without begin_group: the script has no undo group of its own open",
                ));
            }
            e.end_group().map_err(mlua::Error::runtime)?;
            o.set(o.get() - 1);
            Ok(())
        })?,
    )?;

    Ok(module)
}

fn to_lua(lua: &Lua, value: &Value) -> mlua::Result<mlua::Value> {
    // JSON null becomes nil, so that scripts test it as usual.
    let options = mlua::serde::SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false);
    lua.to_value_with(value, options)
}

/// The JSON of a Lua value; NaN becomes null. A table JSON cannot hold whole is refused rather than
/// losing keys without a word.
fn from_lua(lua: &Lua, value: mlua::Value) -> mlua::Result<Value> {
    whole_in_json(&value, &mut HashSet::new())?;
    lua.from_value(value)
}

/// Refuses a table, or one inside it, that holds a list, keys 1 to its length, and other keys
/// besides: a JSON value is one or the other. `seen` holds the tables already checked; a table
/// inside itself is refused by `from_value`.
fn whole_in_json(value: &mlua::Value, seen: &mut HashSet<*const c_void>) -> mlua::Result<()> {
    let mlua::Value::Table(table) = value else {
        return Ok(());
    };
    if !seen.insert(table.to_pointer()) {
        return Ok(());
    }
    let length = table.raw_len();
    table.for_each(|key: mlua::Value, item: mlua::Value| {
        let listed = match &key {
            mlua::Value::Integer(index) => (1..=length as i64).contains(index),
            mlua::Value::Number(index) => index.fract() == 0.0 && (1.0..=length as f64).contains(index),
            _ => false,
        };
        if length > 0 && !listed {
            let key = match &key {
                mlua::Value::String(text) => format!("\"{}\"", text.to_string_lossy()),
                mlua::Value::Integer(index) => index.to_string(),
                mlua::Value::Number(index) => index.to_string(),
                other => other.type_name().to_owned(),
            };
            return Err(mlua::Error::runtime(format!(
                "a table holds a list of {length} values and the key {key} besides: a JSON value is a list or a map, not both"
            )));
        }
        whole_in_json(&item, seen)
    })
}

/// The message of a Lua error, with its line, without the Rust wrapping.
fn describe(error: &mlua::Error) -> String {
    match error {
        mlua::Error::RuntimeError(message) => message.clone(),
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::CallbackError { traceback, cause } => format!("{}\n{traceback}", describe(cause)),
        mlua::Error::WithContext { context, cause } => format!("{context}: {}", describe(cause)),
        other => other.to_string(),
    }
}

fn shorten(line: &str) -> String {
    const LIMIT: usize = 40;
    match line.char_indices().nth(LIMIT) {
        Some((index, _)) => format!("{}…", &line[..index]),
        None => line.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use uniwow_api::serde_json::{Value, json};
    use uniwow_api::{CommandInfo, Editor, EditorBackend, Event};

    use super::{Source, run};
    use crate::output::Output;

    /// Records what the script asks of the editor; `echo` answers with its arguments.
    #[derive(Default)]
    struct Recorder {
        log: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn note(&self, entry: String) {
            self.log.lock().expect("log").push(entry);
        }

        fn entries(&self) -> Vec<String> {
            self.log.lock().expect("log").clone()
        }
    }

    impl EditorBackend for Recorder {
        fn commands(&self) -> Vec<CommandInfo> {
            Vec::new()
        }

        fn call(&self, _caller: &str, name: &str, arguments: Value) -> Result<Value, String> {
            self.note(format!("call {name} {arguments}"));
            match name {
                "echo" => Ok(arguments),
                _ => Err(format!("no command '{name}'")),
            }
        }

        fn publish(&self, _source: &str, _topic: &str, _payload: Value) -> Result<(), String> {
            Ok(())
        }

        fn subscribe(&self, _caller: &str, topic: &str) -> Result<u64, String> {
            self.note(format!("subscribe {topic}"));
            Ok(7)
        }

        fn next_event(&self, _caller: &str, subscription: u64, timeout: Duration) -> Result<Option<Event>, String> {
            if subscription != 7 {
                return Err(format!("unknown subscription {subscription}"));
            }
            std::thread::sleep(timeout);
            Ok(None)
        }

        fn unsubscribe(&self, subscription: u64) {
            self.note(format!("unsubscribe {subscription}"));
        }

        fn setting(&self, _caller: &str, _space: &str, _key: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }

        fn set_setting(&self, _caller: &str, _space: &str, _key: &str, _value: Value) -> Result<(), String> {
            Ok(())
        }

        fn begin_group(&self, caller: &str, label: &str) -> Result<(), String> {
            self.note(format!("begin {caller} {label}"));
            Ok(())
        }

        fn end_group(&self, caller: &str) -> Result<(), String> {
            self.note(format!("end {caller}"));
            Ok(())
        }
    }

    fn script(text: &str) -> Source {
        Source::Script {
            name: "test.lua".to_owned(),
            text: text.as_bytes().to_vec(),
            tool: None,
        }
    }

    fn execute(source: Source, cancelled: Arc<AtomicBool>) -> (Vec<String>, Vec<String>) {
        let recorder = Arc::new(Recorder::default());
        let editor = Editor::new(recorder.clone(), "scripting-lua#test.lua #1");
        let output = Arc::new(Output::default());
        run(source, editor, cancelled, output.clone(), "test.lua #1");
        (recorder.entries(), output.texts())
    }

    #[test]
    fn values_cross_as_json_and_the_run_is_one_group() {
        let (calls, printed) = execute(
            script(
                r#"local answer = uniwow.call("echo", {color = {1, 0.5, 0}, name = "red"}) print(answer.name, answer.color[2])"#,
            ),
            Arc::default(),
        );
        assert_eq!(calls[0], "begin scripting-lua#test.lua #1 Lua: test.lua");
        let sent: Value = calls[1]
            .strip_prefix("call echo ")
            .map(|text| uniwow_api::serde_json::from_str(text).expect("JSON"))
            .expect("one call");
        assert_eq!(sent, json!({ "color": [1, 0.5, 0], "name": "red" }));
        assert_eq!(calls[2], "end scripting-lua#test.lua #1");
        assert_eq!(printed[0], "red\t0.5");
    }

    #[test]
    fn a_table_that_json_cannot_hold_whole_is_refused() {
        let (calls, printed) = execute(
            script(
                r#"local function send(value)
                    local sent, why = pcall(uniwow.call, "echo", value)
                    print(sent and "sent" or why)
                end
                send({[1] = "a", [3] = "c"})
                send({1, 2, key = "v"})
                send({list = {1, x = 2}})
                send({1, nil, 3})
                send({})
                send({n = 0/0})"#,
            ),
            Arc::default(),
        );
        let refused: Vec<&String> = printed.iter().filter(|line| line.contains("not both")).collect();
        assert_eq!(refused.len(), 3, "{printed:?}");
        assert!(refused[0].contains("a list of 1 values and the key 3"), "{refused:?}");
        assert!(refused[1].contains("the key \"key\""), "{refused:?}");
        let sent: Vec<&str> = calls
            .iter()
            .filter_map(|call| call.strip_prefix("call echo "))
            .collect();
        assert_eq!(sent, ["[1,null,3]", "{}", "{\"n\":null}"]);
    }

    #[test]
    fn a_run_taking_too_much_memory_gets_an_error_and_the_editor_goes_on() {
        let recorder = Arc::new(Recorder::default());
        let editor = Editor::new(recorder, "scripting-lua#test.lua #1");
        let source = script(r#"local t = {} while true do t[#t + 1] = string.rep("x", 100000) .. #t end"#);
        let error =
            super::execute(&source, &editor, &Arc::default(), &Arc::default(), 16 << 20).expect_err("out of memory");
        assert!(super::describe(&error).contains("not enough memory"), "{error}");
    }

    #[test]
    fn end_group_never_ends_the_group_of_the_run() {
        let (calls, printed) = execute(
            script(r#"uniwow.begin_group("mine") uniwow.end_group() print(pcall(uniwow.end_group))"#),
            Arc::default(),
        );
        assert!(
            printed[0].starts_with("false") && printed[0].contains("end_group without begin_group"),
            "{printed:?}"
        );
        // Its own group, then the group of the run, at its end.
        let ends: Vec<usize> = (0..calls.len()).filter(|&i| calls[i].starts_with("end ")).collect();
        assert_eq!(ends, [2, 3], "{calls:?}");
        assert_eq!(calls[1], "begin scripting-lua#test.lua #1 mine");
    }

    #[test]
    fn an_error_shows_its_line() {
        let (_, printed) = execute(script("local x = 1\nlocal y = nil\nprint(y.field)"), Arc::default());
        assert!(printed.iter().any(|line| line.contains("test.lua:3:")), "{printed:?}");
    }

    #[test]
    fn a_failed_command_is_a_lua_error() {
        let (_, printed) = execute(script(r#"uniwow.call("missing")"#), Arc::default());
        assert!(
            printed.iter().any(|line| line.contains("no command 'missing'")),
            "{printed:?}"
        );
    }

    #[test]
    fn c_modules_cannot_be_loaded() {
        let (_, printed) = execute(
            script(
                r#"local ok, message = pcall(require, "lfs") print(ok) print(package.loadlib("lfs.dll", "luaopen_lfs"))"#,
            ),
            Arc::default(),
        );
        assert_eq!(printed[0], "false");
        assert!(printed.iter().any(|line| line.contains("disabled")), "{printed:?}");
    }

    #[test]
    fn the_console_prints_the_value_of_an_expression() {
        let (_, printed) = execute(
            Source::Console(r#"1 + 2, "three", uniwow.call("echo", {name = "red"})"#.to_owned()),
            Arc::default(),
        );
        assert_eq!(printed, ["3\tthree\t{\"name\":\"red\"}"]);
    }

    #[test]
    fn a_script_loads_the_other_files_of_its_tool() {
        let tool = std::env::temp_dir().join(format!("uniwow-tool-{}", std::process::id()));
        std::fs::create_dir_all(&tool).expect("tool folder");
        std::fs::write(tool.join("palette.lua"), "return { red = 'red from the tool' }").expect("helper");
        let (_, printed) = execute(
            Source::Script {
                name: "tool/paint.lua".to_owned(),
                text: b"print(require('palette').red)".to_vec(),
                tool: Some(tool.clone()),
            },
            Arc::default(),
        );
        std::fs::remove_dir_all(&tool).expect("cleaned");
        assert_eq!(printed[0], "red from the tool", "{printed:?}");
    }

    #[test]
    fn precompiled_chunks_are_refused() {
        let bytecode = uniwow_api::mlua::Lua::new()
            .load("return 1")
            .into_function()
            .expect("compiles")
            .dump(false);
        let (_, printed) = execute(
            Source::Script {
                name: "test.lua".to_owned(),
                text: bytecode.clone(),
                tool: None,
            },
            Arc::default(),
        );
        assert!(printed[0].contains(crate::loading::REFUSED), "{printed:?}");

        let folder = std::env::temp_dir().join(format!("uniwow-lua-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("folder");
        std::fs::write(folder.join("compiled.lua"), &bytecode).expect("bytecode");
        std::fs::write(folder.join("source.lua"), "return 'from source'").expect("source");
        let code = format!(
            r#"local folder = [[{}]]
            package.path = folder .. "/?.lua"
            print(string.dump)
            print(loadstring("\27LuaQ"))
            print(loadfile(folder .. "/compiled.lua"))
            print(pcall(dofile, folder .. "/compiled.lua"))
            print(pcall(require, "compiled"))
            print(require("source"), loadstring("return 1 + 1")())"#,
            folder.display()
        );
        let (_, printed) = execute(script(&code), Arc::default());
        std::fs::remove_dir_all(&folder).expect("cleaned");
        assert_eq!(printed[0], "nil", "string.dump is removed");
        let all = printed.join("\n");
        // loadstring, loadfile, dofile and require.
        assert_eq!(all.matches(crate::loading::REFUSED).count(), 4, "{printed:?}");
        assert!(
            printed.contains(&"from source\t2".to_owned()),
            "source text still loads"
        );
    }

    #[test]
    fn a_run_ends_its_group_and_subscriptions_even_on_an_error() {
        let (calls, _) = execute(
            script(r#"uniwow.subscribe("demo") error("on purpose")"#),
            Arc::default(),
        );
        let ended = calls.iter().position(|c| c.starts_with("end ")).expect("end_group");
        assert!(calls[ended..].contains(&"unsubscribe 7".to_owned()), "{calls:?}");
    }

    #[test]
    fn an_unknown_subscription_is_an_error() {
        let (_, printed) = execute(script("uniwow.next_event(42)"), Arc::default());
        assert!(
            printed.iter().any(|line| line.contains("unknown subscription 42")),
            "{printed:?}"
        );
    }

    #[test]
    fn stop_cannot_be_escaped() {
        let escapes = [
            "coroutine.wrap(function() while true do end end)()",
            "while true do pcall(function() while true do end end) end",
            "while true do xpcall(function() while true do end end, function(e) return e end) end",
            "while true do coroutine.resume(coroutine.create(function() while true do end end)) end",
        ];
        for code in escapes {
            let cancelled = Arc::new(AtomicBool::new(false));
            let stop = cancelled.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                stop.store(true, Ordering::Relaxed);
            });
            let started = std::time::Instant::now();
            let (_, printed) = execute(script(code), cancelled);
            assert!(started.elapsed() < Duration::from_secs(1), "{code}");
            assert_eq!(
                printed.last().map(String::as_str),
                Some("test.lua #1: stopped"),
                "{code}"
            );
        }
    }

    #[test]
    fn a_stopped_run_ends_and_closes_its_subscriptions() {
        for code in [
            "while true do end",
            r#"local s = uniwow.subscribe("demo") uniwow.next_event(s)"#,
        ] {
            let cancelled = Arc::new(AtomicBool::new(false));
            let stop = cancelled.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                stop.store(true, Ordering::Relaxed);
            });
            let (calls, printed) = execute(script(code), cancelled);
            assert_eq!(printed.last().map(String::as_str), Some("test.lua #1: stopped"));
            if code.contains("subscribe") {
                assert!(calls.contains(&"unsubscribe 7".to_owned()), "{calls:?}");
            }
        }
    }
}
