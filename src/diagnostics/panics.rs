//! Panics, recorded in the diagnostic log. While the terminal runs, its
//! screen hides the message the default hook prints, and the session goes
//! on (`crate::sync`), so without a record a panic would leave no trace. A
//! record keeps where the panic happened and its kind of failure, never its
//! message, which can quote the user's text.

use std::{
    cell::Cell,
    panic::PanicHookInfo,
    sync::{Arc, atomic::Ordering},
};

use serde_json::json;

use super::Diagnostics;

thread_local! {
    /// Set while this thread holds the log's lock: a panic there must not
    /// wait for the lock it already holds.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

/// Marks this thread as holding the log's lock until dropped.
pub(super) struct Recording;

impl Recording {
    pub(super) fn start() -> Self {
        RECORDING.with(|recording| recording.set(true));
        Self
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        RECORDING.with(|recording| recording.set(false));
    }
}

/// Record every panic from now on, then let the previous hook run.
pub(crate) fn report(diagnostics: Arc<Diagnostics>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        diagnostics.record_panic(info);
        previous(info);
    }));
}

impl Diagnostics {
    fn record_panic(&self, info: &PanicHookInfo<'_>) {
        self.panics.fetch_add(1, Ordering::Relaxed);
        if RECORDING.with(Cell::get) {
            return;
        }
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or_default();
        self.record(
            "panic",
            json!({
                "location": info.location().map(|at| format!("{}:{}", at.file(), at.line())),
                "thread": std::thread::current().name(),
                "kind": kind(message),
            }),
            None,
        );
    }

    /// The line the exit report adds once a panic happened in this run.
    pub(crate) fn panic_notice(&self) -> Option<String> {
        let count = self.panics.load(Ordering::Relaxed);
        let plural = if count == 1 { "" } else { "s" };
        (count > 0).then(|| {
            format!(
                "RoomKit Code hit {count} internal error{plural}; see the diagnostic log: {}",
                self.path.display()
            )
        })
    }
}

/// Kinds of failure the standard library and the async runtime report, by
/// the start of their message on the pinned toolchain and on the oldest
/// supported one. Anything else is "other": a message can begin with any
/// value, the user's text included.
const KINDS: &[&str] = &[
    "index out of bounds",
    "start byte index",
    "end byte index",
    "byte range starts at",
    "byte index",
    "begin <= end",
    "range start index",
    "range end index",
    "slice index starts at",
    "attempt to add with overflow",
    "attempt to subtract with overflow",
    "attempt to multiply with overflow",
    "attempt to negate with overflow",
    "attempt to shift left with overflow",
    "attempt to shift right with overflow",
    "attempt to divide by zero",
    "attempt to calculate the remainder with a divisor of zero",
    "called `Option::unwrap()` on a `None` value",
    "called `Result::unwrap()` on an `Err` value",
    "called `Result::unwrap_err()` on an `Ok` value",
    "RefCell already borrowed",
    "RefCell already mutably borrowed",
    "capacity overflow",
    "assertion failed",
    "assertion `left == right` failed",
    "assertion `left != right` failed",
    "removal index",
    "insertion index",
    "swap_remove index",
    "Cannot start a runtime from within a runtime",
    "Cannot drop a runtime in a context where blocking is not allowed",
    "there is no reactor running",
];

/// What kind of failure a panic message reports, never its text.
pub(super) fn kind(message: &str) -> &'static str {
    KINDS
        .iter()
        .find(|kind| message.starts_with(*kind))
        .copied()
        .unwrap_or("other")
}
