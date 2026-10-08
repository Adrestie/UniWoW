//! The interface objects in the C interface (section 3): typed functions over handles. Each one
//! can be called from any thread; a refusal returns a non-zero status, or a null handle, and is
//! logged under the module's name.

use std::ffi::{c_char, c_void};
use std::sync::Arc;

use super::{Reply, UserPointer, c_text, guarded, module, read, reply_with};
use uniwow_api::curve::ShownCurve;
use uniwow_api::sequence::{self, Sequence, Track};
use uniwow_api::ui::data::{self, Rows, RowsInOrder, TreeItem};
use uniwow_api::ui::{self, Kind, PaintCommand, Property, Signal, SignalData, Ui};

/// What a slot receives; the fields its signal does not use are zero.
#[repr(C)]
pub struct CSignal {
    sender: u64,
    signal: u32,
    button: u32,
    modifiers: u32,
    boolean: i32,
    item: u64,
    integer: i64,
    number: f64,
    text: *const c_char,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
    width: f64,
    height: f64,
    painter: u64,
}

type SlotFn = extern "C-unwind" fn(*mut c_void, *const CSignal);

/// The functions of the interface objects, at the end of the table of `uniwow.h`.
#[repr(C)]
pub struct Table {
    panel: extern "C" fn(*mut c_void, *const c_char) -> u64,
    create: extern "C" fn(*mut c_void, u32, u64) -> u64,
    destroy: extern "C" fn(*mut c_void, u64),
    add_to: extern "C" fn(*mut c_void, u64, u64, u32, u32, u32, u32) -> i32,
    set_text: extern "C" fn(*mut c_void, u64, u32, *const c_char) -> i32,
    set_numbers: extern "C" fn(*mut c_void, u64, u32, *const f64, u32) -> i32,
    text: extern "C" fn(*mut c_void, u64, u32, Option<Reply>, *mut c_void) -> i32,
    numbers: extern "C" fn(*mut c_void, u64, u32, *mut f64, u32) -> u32,
    add_entry: extern "C" fn(*mut c_void, u64, *const c_char) -> i32,
    clear_entries: extern "C" fn(*mut c_void, u64) -> i32,
    set_scene: extern "C" fn(*mut c_void, u64, u64) -> i32,
    update: extern "C" fn(*mut c_void, u64) -> i32,
    connect: extern "C" fn(*mut c_void, u64, u32, Option<SlotFn>, *mut c_void) -> u64,
    disconnect: extern "C" fn(*mut c_void, u64),
    set_pen: extern "C" fn(*mut c_void, u64, u32, f64),
    set_brush: extern "C" fn(*mut c_void, u64, u32),
    draw_line: extern "C" fn(*mut c_void, u64, f64, f64, f64, f64),
    draw_rect: extern "C" fn(*mut c_void, u64, f64, f64, f64, f64, f64),
    draw_ellipse: extern "C" fn(*mut c_void, u64, f64, f64, f64, f64),
    draw_text: extern "C" fn(*mut c_void, u64, f64, f64, *const c_char, f64),
    translate: extern "C" fn(*mut c_void, u64, f64, f64),
    scale: extern "C" fn(*mut c_void, u64, f64, f64),
    save: extern "C" fn(*mut c_void, u64),
    restore: extern "C" fn(*mut c_void, u64),
}

pub const TABLE: Table = Table {
    panel,
    create,
    destroy,
    add_to,
    set_text,
    set_numbers,
    text,
    numbers,
    add_entry,
    clear_entries,
    set_scene,
    update,
    connect,
    disconnect,
    set_pen,
    set_brush,
    draw_line,
    draw_rect,
    draw_ellipse,
    draw_text,
    translate,
    scale,
    save,
    restore,
};

/// Runs `body` on the module's objects; a refusal is logged under `what` and gives `fallback`.
fn with_ui<R: Copy>(
    context: *mut c_void,
    what: &str,
    fallback: R,
    body: impl FnOnce(&mut Ui) -> Result<R, String>,
) -> R {
    guarded(context, fallback, || {
        let module = module(context);
        let result = body(&mut ui::lock(&module.ui));
        result.unwrap_or_else(|error| {
            module.refuse(what, &error);
            fallback
        })
    })
}

fn property(value: u32) -> Result<Property, String> {
    Property::from_u32(value).ok_or_else(|| format!("unknown property {value}"))
}

fn status(result: Result<(), String>) -> Result<i32, String> {
    result.map(|()| 0)
}

extern "C" fn panel(context: *mut c_void, id: *const c_char) -> u64 {
    with_ui(context, "panel", 0, |ui| Ok(ui.panel(&read(id)?)))
}

extern "C" fn create(context: *mut c_void, kind: u32, parent: u64) -> u64 {
    with_ui(context, "create", 0, |ui| {
        let kind = Kind::from_u32(kind).ok_or_else(|| format!("unknown kind {kind}"))?;
        ui.create(kind, (parent != 0).then_some(parent))
    })
}

extern "C" fn destroy(context: *mut c_void, object: u64) {
    with_ui(context, "destroy", (), |ui| ui.destroy(object));
}

extern "C" fn add_to(
    context: *mut c_void,
    container: u64,
    child: u64,
    row: u32,
    column: u32,
    row_span: u32,
    column_span: u32,
) -> i32 {
    with_ui(context, "add_to", 1, |ui| {
        status(ui.add_to(container, child, [row, column, row_span.max(1), column_span.max(1)]))
    })
}

/// Curves, tracks, items, rows, columns or paths read from their JSON.
enum Read {
    Curves(Result<Vec<ShownCurve>, String>),
    Tracks(Result<Vec<Track>, String>),
    Items(Result<Vec<TreeItem>, String>),
    Rows(Result<Rows, String>),
    Columns(Result<Vec<String>, String>),
    Paths(Result<Vec<String>, String>),
}

extern "C" fn set_text(context: *mut c_void, object: u64, which: u32, text: *const c_char) -> i32 {
    let which = property(which);
    let text = read(text);
    // JSON is read, and rows made ready, before taking the lock: a long text would hold the
    // interface waiting.
    let parsed = match (&which, &text) {
        (Ok(Property::Curves), Ok(text)) => Some(Read::Curves(ui::read_curves(text))),
        (Ok(Property::Tracks), Ok(text)) => Some(Read::Tracks(ui::read_tracks(text))),
        (Ok(Property::Items), Ok(text)) => Some(Read::Items(ui::read_items(text))),
        (Ok(Property::Rows), Ok(text)) => Some(Read::Rows(ui::read_rows(text).and_then(Rows::new))),
        (Ok(Property::Columns), Ok(text)) => Some(Read::Columns(ui::read_columns(text))),
        (Ok(Property::Paths), Ok(text)) => Some(Read::Paths(ui::read_paths(text))),
        _ => None,
    };
    // Items or rows replaced, freed once the lock is released.
    let mut replaced: Option<Box<dyn std::any::Any>> = None;
    let code = with_ui(context, "set_text", 1, |ui| match parsed {
        Some(Read::Curves(curves)) => status(ui.set_curves(object, curves?)),
        Some(Read::Tracks(tracks)) => status(ui.set_tracks(object, tracks?)),
        Some(Read::Items(items)) => {
            replaced = Some(Box::new(ui.set_items(object, items?)?));
            Ok(0)
        }
        Some(Read::Rows(rows)) => {
            replaced = Some(Box::new(ui.set_rows(object, rows?)?));
            Ok(0)
        }
        Some(Read::Columns(columns)) => status(ui.set_columns(object, columns?)),
        Some(Read::Paths(paths)) => status(ui.set_paths(object, paths?)),
        None => status(ui.set_text(object, which?, &text?)),
    });
    drop(replaced);
    code
}

extern "C" fn set_numbers(context: *mut c_void, object: u64, which: u32, values: *const f64, count: u32) -> i32 {
    with_ui(context, "set_numbers", 1, |ui| {
        if values.is_null() {
            return Err("no values".to_owned());
        }
        // SAFETY: the module passes `count` numbers.
        let values = unsafe { std::slice::from_raw_parts(values, count as usize) };
        status(ui.set_numbers(object, property(which)?, values))
    })
}

/// A text copied under the lock, to be answered once it is released.
enum Copied {
    Text(String),
    Curves(Vec<ShownCurve>),
    Sequence(std::sync::Arc<Sequence>),
    Items(std::sync::Arc<Vec<TreeItem>>),
    Rows(RowsInOrder),
}

extern "C" fn text(
    context: *mut c_void,
    object: u64,
    which: u32,
    reply: Option<Reply>,
    reply_context: *mut c_void,
) -> i32 {
    guarded(context, 1, || {
        let module = module(context);
        let copied = property(which).and_then(|which| {
            let ui = ui::lock(&module.ui);
            match which {
                Property::Curves => ui.curves(object).map(Copied::Curves),
                Property::Tracks => ui.sequence(object).map(Copied::Sequence),
                Property::Items => ui.items(object).map(Copied::Items),
                Property::Rows => ui.table(object).map(|table| Copied::Rows(table.rows_in_order())),
                _ => ui.text(object, which).map(Copied::Text),
            }
        });
        // The lock is released: the reply is the module's code, which may call its objects again.
        match copied {
            Ok(Copied::Text(text)) => reply_with(reply, reply_context, &text),
            Ok(Copied::Curves(curves)) => {
                reply_with(reply, reply_context, &ShownCurve::list_to_json(&curves).to_string())
            }
            Ok(Copied::Sequence(data)) => reply_with(
                reply,
                reply_context,
                &sequence::tracks_to_json(&data.tracks).to_string(),
            ),
            Ok(Copied::Items(items)) => reply_with(reply, reply_context, &data::items_to_json(&items).to_string()),
            Ok(Copied::Rows(rows)) => reply_with(reply, reply_context, &data::rows_to_json(rows.iter()).to_string()),
            Err(error) => {
                module.refuse("text", &error);
                return 1;
            }
        }
        0
    })
}

/// One cell of a table view, of the row `row`.
pub(super) extern "C" fn set_cell(context: *mut c_void, table: u64, row: u64, column: u32, text: *const c_char) -> i32 {
    let text = read(text);
    with_ui(context, "set_cell", 1, |ui| {
        status(ui.set_cell(table, row, column as usize, &text?))
    })
}

/// Rows inserted at `at` in the module's order of a table view, given as the JSON of `ROWS`.
pub(super) extern "C" fn insert_rows(context: *mut c_void, table: u64, at: u32, rows: *const c_char) -> i32 {
    // Read before taking the lock, as the rows set whole.
    let rows = read(rows).and_then(|rows| ui::read_rows(&rows));
    with_ui(context, "insert_rows", 1, |ui| {
        status(ui.insert_rows(table, at as usize, rows?))
    })
}

/// The rows of these ids removed from a table view.
pub(super) extern "C" fn remove_rows(context: *mut c_void, table: u64, rows: *const u64, count: u32) -> i32 {
    with_ui(context, "remove_rows", 1, |ui| {
        if rows.is_null() && count > 0 {
            return Err("no rows".to_owned());
        }
        let ids = if count == 0 {
            &[][..]
        } else {
            // SAFETY: the module passes `count` ids.
            unsafe { std::slice::from_raw_parts(rows, count as usize) }
        };
        status(ui.remove_rows(table, ids))
    })
}

extern "C" fn numbers(context: *mut c_void, object: u64, which: u32, values: *mut f64, capacity: u32) -> u32 {
    with_ui(context, "numbers", 0, |ui| {
        let numbers = ui.numbers(object, property(which)?)?;
        if !values.is_null() {
            let written = numbers.len().min(capacity as usize);
            // SAFETY: the module gives room for `capacity` numbers.
            unsafe { std::ptr::copy_nonoverlapping(numbers.as_ptr(), values, written) };
        }
        Ok(numbers.len() as u32)
    })
}

extern "C" fn add_entry(context: *mut c_void, combo: u64, text: *const c_char) -> i32 {
    with_ui(context, "add_entry", 1, |ui| status(ui.add_entry(combo, &read(text)?)))
}

extern "C" fn clear_entries(context: *mut c_void, combo: u64) -> i32 {
    with_ui(context, "clear_entries", 1, |ui| status(ui.clear_entries(combo)))
}

extern "C" fn set_scene(context: *mut c_void, view: u64, scene: u64) -> i32 {
    with_ui(context, "set_scene", 1, |ui| status(ui.set_scene(view, scene)))
}

extern "C" fn update(context: *mut c_void, area: u64) -> i32 {
    with_ui(context, "update", 1, |ui| status(ui.update(area)))
}

extern "C" fn connect(context: *mut c_void, sender: u64, signal: u32, slot: Option<SlotFn>, user: *mut c_void) -> u64 {
    let user = UserPointer(user);
    with_ui(context, "connect", 0, |ui| {
        let signal = Signal::from_u32(signal).ok_or_else(|| format!("unknown signal {signal}"))?;
        let slot = slot.ok_or("no slot")?;
        let module = module(context);
        ui.connect(
            sender,
            signal,
            Arc::new(move |data: &SignalData| {
                if !module.active() {
                    return;
                }
                let text = c_text(&data.text);
                let signal = CSignal {
                    sender: data.sender,
                    signal: data.signal,
                    button: data.button,
                    modifiers: data.modifiers,
                    boolean: i32::from(data.boolean),
                    item: data.item,
                    integer: data.integer,
                    number: data.number,
                    text: text.as_ptr(),
                    x: data.x,
                    y: data.y,
                    dx: data.dx,
                    dy: data.dy,
                    width: data.width,
                    height: data.height,
                    painter: data.painter,
                };
                let user = user;
                slot(user.0, &signal);
            }),
        )
    })
}

extern "C" fn disconnect(context: *mut c_void, connection: u64) {
    with_ui(context, "disconnect", (), |ui| {
        ui.disconnect(connection);
        Ok(())
    });
}

fn paint(context: *mut c_void, painter: u64, command: PaintCommand) {
    with_ui(context, "paint", (), |ui| ui.paint(painter, command));
}

extern "C" fn set_pen(context: *mut c_void, painter: u64, color: u32, width: f64) {
    paint(
        context,
        painter,
        PaintCommand::SetPen {
            color,
            width: width as f32,
        },
    );
}

extern "C" fn set_brush(context: *mut c_void, painter: u64, color: u32) {
    paint(context, painter, PaintCommand::SetBrush { color });
}

extern "C" fn draw_line(context: *mut c_void, painter: u64, x1: f64, y1: f64, x2: f64, y2: f64) {
    let [x1, y1, x2, y2] = [x1, y1, x2, y2].map(|v| v as f32);
    paint(context, painter, PaintCommand::DrawLine { x1, y1, x2, y2 });
}

extern "C" fn draw_rect(context: *mut c_void, painter: u64, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let [x, y, width, height, radius] = [x, y, width, height, radius].map(|v| v as f32);
    paint(
        context,
        painter,
        PaintCommand::DrawRect {
            x,
            y,
            width,
            height,
            radius,
        },
    );
}

extern "C" fn draw_ellipse(context: *mut c_void, painter: u64, x: f64, y: f64, width: f64, height: f64) {
    let [x, y, width, height] = [x, y, width, height].map(|v| v as f32);
    paint(context, painter, PaintCommand::DrawEllipse { x, y, width, height });
}

extern "C" fn draw_text(context: *mut c_void, painter: u64, x: f64, y: f64, text: *const c_char, size: f64) {
    let text = read(text).unwrap_or_default();
    paint(
        context,
        painter,
        PaintCommand::DrawText {
            x: x as f32,
            y: y as f32,
            text,
            size: size as f32,
        },
    );
}

extern "C" fn translate(context: *mut c_void, painter: u64, dx: f64, dy: f64) {
    paint(
        context,
        painter,
        PaintCommand::Translate {
            dx: dx as f32,
            dy: dy as f32,
        },
    );
}

extern "C" fn scale(context: *mut c_void, painter: u64, sx: f64, sy: f64) {
    paint(
        context,
        painter,
        PaintCommand::Scale {
            sx: sx as f32,
            sy: sy as f32,
        },
    );
}

extern "C" fn save(context: *mut c_void, painter: u64) {
    paint(context, painter, PaintCommand::Save);
}

extern "C" fn restore(context: *mut c_void, painter: u64) {
    paint(context, painter, PaintCommand::Restore);
}

#[cfg(test)]
mod tests {
    use std::ffi::{CStr, c_char, c_void};
    use std::sync::{Arc, OnceLock};

    use super::{create, insert_rows, panel, remove_rows, set_cell, set_text, text};
    use crate::capi::ModuleContext;
    use uniwow_api::ui::{Kind, Property, Ui};

    /// What `answer` saw: the text, and whether the module could take its objects' lock.
    #[derive(Default)]
    struct Seen {
        context: usize,
        text: String,
        free: bool,
    }

    extern "C-unwind" fn answer(target: *mut c_void, text: *const c_char) {
        // SAFETY: `target` is the `Seen` of the test, `text` a NUL-terminated string.
        let seen = unsafe { &mut *target.cast::<Seen>() };
        seen.text = unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned();
        let module = unsafe { &*(seen.context as *const ModuleContext) };
        seen.free = module.ui.try_lock().is_ok();
    }

    #[test]
    fn a_text_is_answered_once_the_lock_of_the_objects_is_released() {
        let module: &'static ModuleContext = Box::leak(Box::new(ModuleContext {
            id: "test".to_owned(),
            editor: OnceLock::new(),
            ui: Ui::new(Arc::new(|job| job())),
            activity: Arc::default(),
            apply: OnceLock::new(),
            properties: OnceLock::new(),
        }));
        let context = std::ptr::from_ref(module).cast_mut().cast::<c_void>();
        let root = panel(context, c"p".as_ptr());
        let view = create(context, Kind::CurveView as u32, 0);
        assert_ne!(root, 0);
        assert_ne!(view, 0);
        let curves = c"[{\"label\":\"x\",\"keys\":[{\"time\":0,\"value\":1}]}]";
        assert_eq!(set_text(context, view, Property::Curves as u32, curves.as_ptr()), 0);
        let mut seen = Seen {
            context: context as usize,
            ..Seen::default()
        };
        let target = std::ptr::from_mut(&mut seen).cast::<c_void>();
        assert_eq!(text(context, view, Property::Curves as u32, Some(answer), target), 0);
        assert!(seen.free, "the reply ran under the lock");
        assert!(seen.text.contains("\"value\":1.0"), "{}", seen.text);
        assert_eq!(set_text(context, view, Property::Curves as u32, c"[{".as_ptr()), 1);
    }

    #[test]
    fn the_tracks_of_a_sequence_cross_the_c_functions() {
        let module: &'static ModuleContext = Box::leak(Box::new(ModuleContext {
            id: "test".to_owned(),
            editor: OnceLock::new(),
            ui: Ui::new(Arc::new(|job| job())),
            activity: Arc::default(),
            apply: OnceLock::new(),
            properties: OnceLock::new(),
        }));
        let context = std::ptr::from_ref(module).cast_mut().cast::<c_void>();
        let sequence = create(context, Kind::Sequence as u32, 0);
        assert_ne!(sequence, 0);
        let tracks = c"[{\"property\":\"cube/opacity\",\"kind\":\"number\",\"curves\":[{\"keys\":[{\"time\":3,\"value\":0.5}]}]}]";
        assert_eq!(set_text(context, sequence, Property::Tracks as u32, tracks.as_ptr()), 0);
        let mut seen = Seen {
            context: context as usize,
            ..Seen::default()
        };
        let target = std::ptr::from_mut(&mut seen).cast::<c_void>();
        assert_eq!(
            text(context, sequence, Property::Tracks as u32, Some(answer), target),
            0
        );
        assert!(seen.free, "the reply ran under the lock");
        assert!(seen.text.contains("\"time\":3.0"), "{}", seen.text);
        let between = c"[{\"property\":\"cube/opacity\",\"kind\":\"number\",\"curves\":[{\"keys\":[{\"time\":3.5,\"value\":0.5}]}]}]";
        assert_eq!(
            set_text(context, sequence, Property::Tracks as u32, between.as_ptr()),
            1,
            "between frames"
        );
    }

    #[test]
    fn the_rows_of_a_table_cross_the_c_functions() {
        let module: &'static ModuleContext = Box::leak(Box::new(ModuleContext {
            id: "test".to_owned(),
            editor: OnceLock::new(),
            ui: Ui::new(Arc::new(|job| job())),
            activity: Arc::default(),
            apply: OnceLock::new(),
            properties: OnceLock::new(),
        }));
        let context = std::ptr::from_ref(module).cast_mut().cast::<c_void>();
        let table = create(context, Kind::TableView as u32, 0);
        let rows = c"[{\"id\":1,\"cells\":[\"a\"]},{\"id\":2,\"cells\":[\"b\"]}]";
        assert_eq!(set_text(context, table, Property::Rows as u32, rows.as_ptr()), 0);
        assert_eq!(set_cell(context, table, 2, 1, c"x".as_ptr()), 0);
        assert_eq!(
            set_cell(context, table, 9, 0, c"x".as_ptr()),
            1,
            "a row it does not hold"
        );
        let third = c"[{\"id\":3,\"cells\":[\"c\"]}]";
        assert_eq!(insert_rows(context, table, 1, third.as_ptr()), 0);
        assert_eq!(insert_rows(context, table, 0, third.as_ptr()), 1, "an id taken");
        assert_eq!(insert_rows(context, table, 0, c"[{".as_ptr()), 1, "not JSON");
        let absent = [1u64, 9];
        assert_eq!(
            remove_rows(context, table, absent.as_ptr(), 2),
            1,
            "a row it does not hold, none removed"
        );
        assert_eq!(remove_rows(context, table, absent.as_ptr(), 1), 0);
        assert_eq!(remove_rows(context, table, std::ptr::null(), 1), 1, "no ids");
        assert_eq!(remove_rows(context, table, std::ptr::null(), 0), 0, "none to remove");
        let mut seen = Seen {
            context: context as usize,
            ..Seen::default()
        };
        let target = std::ptr::from_mut(&mut seen).cast::<c_void>();
        assert_eq!(text(context, table, Property::Rows as u32, Some(answer), target), 0);
        let rows: uniwow_api::serde_json::Value = uniwow_api::serde_json::from_str(&seen.text).unwrap();
        assert_eq!(
            rows,
            uniwow_api::serde_json::json!([{ "id": 3, "cells": ["c"] }, { "id": 2, "cells": ["b", "x"] }]),
            "inserted at its place, removed, a cell set beyond the row's"
        );
    }
}
