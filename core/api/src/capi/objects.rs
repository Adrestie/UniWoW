//! The interface objects in the C interface (section 3): typed functions over handles. Each one
//! can be called from any thread; a refusal returns a non-zero status, or a null handle, and is
//! logged under the module's name.

use std::ffi::{c_char, c_void};
use std::sync::Arc;

use super::{Reply, UserPointer, c_text, guarded, module, read, reply_with};
use crate::ui::{self, Kind, PaintCommand, Property, Signal, SignalData, Ui};

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
    guarded(fallback, || {
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

extern "C" fn set_text(context: *mut c_void, object: u64, which: u32, text: *const c_char) -> i32 {
    with_ui(context, "set_text", 1, |ui| {
        status(ui.set_text(object, property(which)?, &read(text)?))
    })
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

extern "C" fn text(
    context: *mut c_void,
    object: u64,
    which: u32,
    reply: Option<Reply>,
    reply_context: *mut c_void,
) -> i32 {
    with_ui(context, "text", 1, |ui| {
        let text = ui.text(object, property(which)?)?;
        reply_with(reply, reply_context, &text);
        Ok(0)
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
