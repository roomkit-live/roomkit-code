//! Panics, recorded in the diagnostic log. While the terminal runs, its
//! screen hides the message the default hook prints, and the session goes
//! on (`crate::sync`), so without a record a panic would leave no trace. A
//! record keeps where the panic happened and what kind it is, never its full
//! message, which can quote the user's text.

use std::{
    cell::Cell,
    panic::PanicHookInfo,
    sync::{Arc, atomic::Ordering},
};

use serde_json::json;

use super::Diagnostics;

thread_local! {
    /// Set while this thread writes a record: a panic there must not wait
    /// for the log's lock it already holds.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

/// Marks this thread as writing a record until dropped.
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
                "RoomKit Code hit {count} internal error{plural}; see /diagnostics or {}",
                self.path.display()
            )
        })
    }
}

/// What a panic message reports, without what it quotes: its text before
/// the first ": ", "; " or line break, at most 80 characters.
pub(super) fn kind(message: &str) -> String {
    let head = [": ", "; ", "\n"]
        .iter()
        .filter_map(|separator| message.find(separator))
        .min()
        .map_or(message, |end| &message[..end]);
    head.chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect::<String>()
        .trim()
        .to_owned()
}
