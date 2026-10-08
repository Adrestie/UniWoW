//! The animatable properties of compiled modules (step 8.2): the kernel keeps their values, so
//! that reading never waits for a module, and hands each write to the module's thread, merging the
//! writes still waiting there.

use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::serde_json::{Value, json};
use uniwow_api::{PropertyKind, PropertyValue, log, ui};

use super::{ModuleContext, Reply, UserPointer, collect, editor, guarded, module, read, reply_with, text_target};

/// Receives a value written to a property, which it may change into the value it keeps.
pub type WriteFn = extern "C-unwind" fn(*mut c_void, *mut f64, u32, Reply, *mut c_void) -> i32;

/// `uniwow_property` of `uniwow.h`.
#[repr(C)]
pub struct PropertyEntry {
    pub name: *const c_char,
    pub label: *const c_char,
    pub kind: u32,
    pub minimum: f64,
    pub maximum: f64,
    pub initial: [f64; 3],
    pub write: Option<WriteFn>,
    pub user: *mut c_void,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A property a compiled module declared.
pub struct CompiledProperty {
    pub name: String,
    pub label: String,
    pub kind: PropertyKind,
    pub range: [f64; 2],
    /// The value readers see: written, told by the module, or kept by its write function.
    value: Mutex<PropertyValue>,
    /// The value waiting for the module's thread, if any; a newer write replaces it.
    waiting: Mutex<Option<PropertyValue>>,
    write: WriteFn,
    user: UserPointer,
    context: &'static ModuleContext,
}

impl CompiledProperty {
    pub fn read(&self) -> PropertyValue {
        *lock(&self.value)
    }

    /// Stores a value written from elsewhere, already within the range, and hands it to the
    /// module's thread, unless a write is already waiting there: that one then carries it.
    pub fn write(self: &Arc<Self>, value: PropertyValue) {
        *lock(&self.value) = value;
        if lock(&self.waiting).replace(value).is_some() {
            return;
        }
        // A write records nothing in the history: it neither blocks Undo nor counts as work, and
        // the module's thread refuses what it would record.
        let property = self.clone();
        ui::lock(&self.context.ui).post_uncounted_job(Box::new(move || property.deliver()));
    }

    /// The value the module tells its property now has.
    fn set(&self, value: PropertyValue) {
        *lock(&self.value) = value.clamped(self.range);
    }

    /// On the module's thread: the waiting value to its write function.
    fn deliver(&self) {
        let Some(value) = lock(&self.waiting).take() else {
            return;
        };
        let Some(editor) = self.context.editor.get().filter(|editor| editor.is_active()) else {
            return;
        };
        let mut numbers = value.components();
        let mut error = String::new();
        let status = (self.write)(
            self.user.0,
            numbers.as_mut_ptr(),
            numbers.len() as u32,
            collect,
            text_target(&mut error),
        );
        if status != 0 {
            editor.report_failure(&format!("could not write its property '{}': {error}", self.name));
            return;
        }
        if !numbers.iter().all(|number| number.is_finite()) {
            log::warn!(
                "module '{}': the value its property '{}' kept is not a finite number: the one before stays",
                self.context.id,
                self.name
            );
            return;
        }
        // What the module kept is what readers see, unless a newer write waits already.
        let kept = PropertyValue::from_components(self.kind, &numbers).clamped(self.range);
        let waiting = lock(&self.waiting);
        if waiting.is_none() {
            *lock(&self.value) = kept;
        }
    }
}

/// Reads the properties a module declared in its `uniwow_module_info`.
pub fn declared(
    entries: *const PropertyEntry,
    count: u32,
    size: u32,
    context: &'static ModuleContext,
) -> Result<Vec<Arc<CompiledProperty>>, String> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if size as usize != std::mem::size_of::<PropertyEntry>() {
        return Err(format!(
            "its uniwow_property is {size} bytes, the editor's is {}: rebuild it with this uniwow.h",
            std::mem::size_of::<PropertyEntry>()
        ));
    }
    let mut properties = Vec::new();
    for index in 0..count as usize {
        // SAFETY: the module declared `count` properties, valid while it is loaded.
        let entry = unsafe { &*entries.add(index) };
        let name = read(entry.name).map_err(|e| format!("property {index}: {e}"))?;
        let kind = PropertyKind::from_u32(entry.kind).ok_or_else(|| format!("property '{name}': unknown kind"))?;
        let write = entry
            .write
            .ok_or_else(|| format!("property '{name}' has no write function"))?;
        let range = [entry.minimum, entry.maximum];
        let initial = PropertyValue::from_components(kind, &entry.initial).clamped(range);
        properties.push(Arc::new(CompiledProperty {
            label: read(entry.label).unwrap_or_else(|_| name.clone()),
            name,
            kind,
            range,
            value: Mutex::new(initial),
            waiting: Mutex::new(None),
            write,
            user: UserPointer(entry.user),
            context,
        }));
    }
    Ok(properties)
}

/// Its properties by name, for `set_property`.
pub fn by_name(properties: &[Arc<CompiledProperty>]) -> HashMap<String, Arc<CompiledProperty>> {
    properties.iter().map(|p| (p.name.clone(), p.clone())).collect()
}

/// The numbers given by a module for a value of `kind`: as many as it has, all finite.
fn value_of(kind: PropertyKind, values: *const f64, count: u32) -> Result<PropertyValue, String> {
    if values.is_null() || count as usize != kind.components() {
        return Err(format!("a {} has {} numbers", kind.name(), kind.components()));
    }
    // SAFETY: the module passes `count` numbers.
    let numbers = unsafe { std::slice::from_raw_parts(values, count as usize) };
    if !numbers.iter().all(|n| n.is_finite()) {
        return Err("a value's numbers must be finite".to_owned());
    }
    Ok(PropertyValue::from_components(kind, numbers))
}

pub extern "C" fn api_properties(context: *mut c_void, reply: Option<Reply>, reply_context: *mut c_void) {
    guarded(context, (), || {
        let listed = editor(context).map(|editor| {
            editor
                .properties()
                .into_iter()
                .map(|p| {
                    json!({
                        "path": p.path,
                        "owner": p.owner,
                        "label": p.label,
                        "kind": p.kind.name(),
                        "range": p.range,
                    })
                })
                .collect::<Vec<Value>>()
        });
        match listed {
            Ok(listed) => reply_with(reply, reply_context, &Value::Array(listed).to_string()),
            Err(error) => module(context).refuse("properties", &error),
        }
    });
}

pub extern "C" fn api_read_property(context: *mut c_void, path: *const c_char, values: *mut f64, capacity: u32) -> u32 {
    guarded(context, 0, || {
        let numbers = read(path).and_then(|path| editor(context)?.read_property(&path));
        match numbers {
            Ok(value) => {
                let numbers = value.components();
                if !values.is_null() {
                    for (index, number) in numbers.iter().take(capacity as usize).enumerate() {
                        // SAFETY: the module gives room for `capacity` numbers.
                        unsafe { *values.add(index) = *number };
                    }
                }
                numbers.len() as u32
            }
            Err(error) => {
                module(context).refuse("read_property", &error);
                0
            }
        }
    })
}

pub extern "C" fn api_write_property(context: *mut c_void, path: *const c_char, values: *const f64, count: u32) -> i32 {
    guarded(context, 1, || {
        let written = (|| {
            let path = read(path)?;
            let editor = editor(context)?;
            let kind = editor
                .property(&path)
                .map(|p| p.kind)
                .ok_or_else(|| format!("unknown property '{path}'"))?;
            editor.write_property(&path, value_of(kind, values, count)?)
        })();
        match written {
            Ok(()) => 0,
            Err(error) => {
                module(context).refuse("write_property", &error);
                1
            }
        }
    })
}

pub extern "C" fn api_set_property(context: *mut c_void, name: *const c_char, values: *const f64, count: u32) -> i32 {
    guarded(context, 1, || {
        let module = module(context);
        let told = (|| {
            let name = read(name)?;
            let property = module
                .properties
                .get()
                .and_then(|properties| properties.get(&name))
                .ok_or_else(|| format!("the module declares no property '{name}'"))?;
            property.set(value_of(property.kind, values, count)?);
            Ok::<(), String>(())
        })();
        match told {
            Ok(()) => 0,
            Err(error) => {
                module.refuse("set_property", &error);
                1
            }
        }
    })
}
