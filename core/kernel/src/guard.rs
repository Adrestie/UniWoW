use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

thread_local! {
    /// The module the kernel is calling on this thread, read by the panic hook.
    static CURRENT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Runs `f`, turning a panic into an error message.
pub fn guarded<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        if let Some(text) = payload.downcast_ref::<&str>() {
            (*text).to_owned()
        } else if let Some(text) = payload.downcast_ref::<String>() {
            text.clone()
        } else {
            "panic without message".to_owned()
        }
    })
}

/// Runs `f` on behalf of `module`: a panic becomes an error message and is logged under its name.
pub fn guarded_as<R>(module: &str, f: impl FnOnce() -> R) -> Result<R, String> {
    let previous = CURRENT.with(|current| current.replace(Some(module.to_owned())));
    let result = guarded(f);
    CURRENT.with(|current| *current.borrow_mut() = previous);
    result
}

/// The module being called on this thread, if any.
pub fn current_module() -> Option<String> {
    CURRENT.with(|current| current.borrow().clone())
}
