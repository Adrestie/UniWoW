use std::panic::{AssertUnwindSafe, catch_unwind};

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
