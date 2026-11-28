//! The process's standard error, written whatever became of it.

use std::io::Write;

/// Writes `line` to the standard error. The write fails once its reader
/// left, as `2>&1 | head` does, or once the terminal is gone: `eprintln!`
/// makes a panic of that, which skips a close or ends a background task.
pub(crate) fn say(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}
