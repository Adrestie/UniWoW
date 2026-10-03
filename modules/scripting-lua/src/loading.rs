//! Lua code is loaded from source text only (rule S6): the bytecode checker of Lua 5.1 is too weak
//! to keep a forged precompiled chunk from corrupting the memory of the editor.

use uniwow_api::mlua::chunk::ChunkMode;
use uniwow_api::mlua::{self, Function, Lua, LuaString, MultiValue, Table, Value};

/// First byte of a precompiled Lua chunk.
const SIGNATURE: u8 = 0x1b;
pub const REFUSED: &str = "precompiled Lua chunks are refused: only source text is loaded";

/// Compiles source text. A precompiled chunk or a syntax error gives `Err` with the message.
/// `name` is the chunk name, as Lua takes it: `@file`, `=label`, or the text itself.
pub fn load_text(lua: &Lua, source: &[u8], name: &str) -> mlua::Result<Result<Function, String>> {
    if source.first() == Some(&SIGNATURE) {
        return Ok(Err(REFUSED.to_owned()));
    }
    match lua
        .load(source)
        .set_name(name)
        .set_mode(ChunkMode::Text)
        .into_function()
    {
        Ok(function) => Ok(Ok(function)),
        Err(mlua::Error::SyntaxError { message, .. }) => Ok(Err(message)),
        Err(error) => Err(error),
    }
}

/// Removes `string.dump` and replaces every function that loads Lua code (`loadstring`, `load`,
/// `loadfile`, `dofile`, and the Lua file searcher of `require`) with one accepting source text
/// only.
pub fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.get::<Table>("string")?.set("dump", Value::Nil)?;

    globals.set(
        "loadstring",
        lua.create_function(|lua, (text, name): (LuaString, Option<String>)| {
            let source = text.as_bytes().to_vec();
            let name = name.unwrap_or_else(|| String::from_utf8_lossy(&source).into_owned());
            as_lua(load_text(lua, &source, &name)?)
        })?,
    )?;
    globals.set(
        "load",
        lua.create_function(|lua, (reader, name): (Function, Option<String>)| {
            let mut source = Vec::new();
            loop {
                match reader.call::<Value>(())? {
                    Value::Nil => break,
                    Value::String(piece) if piece.as_bytes().is_empty() => break,
                    Value::String(piece) => source.extend_from_slice(&piece.as_bytes()),
                    _ => return as_lua(Err("reader function must return a string".to_owned())),
                }
            }
            let name = name.unwrap_or_else(|| "=(load)".to_owned());
            as_lua(load_text(lua, &source, &name)?)
        })?,
    )?;
    globals.set(
        "loadfile",
        lua.create_function(|lua, path: Option<String>| as_lua(load_file(lua, path)?))?,
    )?;
    globals.set(
        "dofile",
        lua.create_function(|lua, path: Option<String>| match load_file(lua, path)? {
            Ok(function) => function.call::<MultiValue>(()),
            Err(message) => Err(mlua::Error::runtime(message)),
        })?,
    )?;
    let loaders: Table = globals.get::<Table>("package")?.get("loaders")?;
    loaders.raw_set(2, lua.create_function(search_module)?)?;
    Ok(())
}

/// What the Lua loading functions return: the function, or nil and the message.
fn as_lua(loaded: Result<Function, String>) -> mlua::Result<(Value, Option<String>)> {
    Ok(match loaded {
        Ok(function) => (Value::Function(function), None),
        Err(message) => (Value::Nil, Some(message)),
    })
}

fn load_file(lua: &Lua, path: Option<String>) -> mlua::Result<Result<Function, String>> {
    let Some(path) = path else {
        return Ok(Err(
            "scripts have no standard input to read: give a file name".to_owned()
        ));
    };
    match file_text(&path) {
        Ok(source) => load_text(lua, &source, &format!("@{path}")),
        Err(error) => Ok(Err(format!("cannot open {path}: {error}"))),
    }
}

/// The text of a Lua file as Lua reads it: a first line starting with `#` is skipped, leaving
/// its line empty so that the line numbers stay right.
fn file_text(path: &str) -> std::io::Result<Vec<u8>> {
    let mut bytes = std::fs::read(path)?;
    if bytes.first() == Some(&b'#') {
        let end = bytes.iter().position(|&b| b == b'\n').unwrap_or(bytes.len());
        bytes.drain(..end);
    }
    Ok(bytes)
}

/// The searcher of Lua files used by `require`, along `package.path` as Lua's own does.
fn search_module(lua: &Lua, name: String) -> mlua::Result<Value> {
    let path: String = lua.globals().get::<Table>("package")?.get("path")?;
    let file_name = name.replace('.', std::path::MAIN_SEPARATOR_STR);
    let mut tried = String::new();
    for template in path.split(';').filter(|t| !t.is_empty()) {
        let candidate = template.replace('?', &file_name);
        let Ok(source) = file_text(&candidate) else {
            tried.push_str(&format!("\n\tno file '{candidate}'"));
            continue;
        };
        return match load_text(lua, &source, &format!("@{candidate}"))? {
            Ok(function) => Ok(Value::Function(function)),
            Err(message) => Err(mlua::Error::runtime(format!(
                "error loading module '{name}' from file '{candidate}':\n\t{message}"
            ))),
        };
    }
    Ok(Value::String(lua.create_string(&tried)?))
}
