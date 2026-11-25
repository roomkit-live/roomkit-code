//! The signals that end an interactive launch.
//!
//! The terminal reads the keyboard, so Ctrl+C never raises SIGINT here. A
//! SIGTERM, or the SIGHUP a closing terminal sends, would end the process
//! where it stands: the agents' processes would outlive it and nothing
//! would be closed. The first one closes the view instead, and the launch
//! closes as it does when the user leaves.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::signal::unix::{SignalKind, signal};
use tokio::task::AbortHandle;

use crate::Result;

/// A signal that ends the launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Signal {
    Terminate = 1,
    /// The terminal is gone: nothing can be written to it any more.
    Hangup = 2,
}

impl Signal {
    /// The signal's number.
    fn number(self) -> u8 {
        match self {
            Self::Terminate => 15,
            Self::Hangup => 1,
        }
    }

    /// The exit code a shell gives a process this signal ended.
    pub(super) fn exit_code(self) -> ExitCode {
        ExitCode::from(128 + self.number())
    }

    fn from_mark(mark: u8) -> Option<Self> {
        match mark {
            1 => Some(Self::Terminate),
            2 => Some(Self::Hangup),
            _ => None,
        }
    }
}

/// The signals the launch received.
pub(super) struct Signals {
    first: Arc<AtomicU8>,
}

impl Signals {
    /// The first signal received, if any.
    pub(super) fn received(&self) -> Option<Signal> {
        Signal::from_mark(self.first.load(Ordering::SeqCst))
    }
}

/// The signals after which the launch exits without waiting for its close.
const EXIT_AT: u8 = 3;

/// Listens for SIGTERM and SIGHUP. The first one aborts `host`, the task
/// that feeds the view, so the view returns. The second changes nothing:
/// the close is under way. The third exits at once, should the close never
/// end.
pub(super) fn listen(host: AbortHandle) -> Result<Signals> {
    let first = Arc::new(AtomicU8::new(0));
    let count = Arc::new(AtomicU8::new(0));
    let kinds = [
        (SignalKind::terminate(), Signal::Terminate),
        (SignalKind::hangup(), Signal::Hangup),
    ];
    for (kind, received) in kinds {
        let mut signals = signal(kind)?;
        let (first, count, host) = (first.clone(), count.clone(), host.clone());
        tokio::spawn(async move {
            while signals.recv().await.is_some() {
                let mark = received as u8;
                let _ = first.compare_exchange(0, mark, Ordering::SeqCst, Ordering::SeqCst);
                if count.fetch_add(1, Ordering::SeqCst) + 1 >= EXIT_AT {
                    // The terminal may be gone: a failed write must not
                    // panic here.
                    let _ = writeln!(
                        std::io::stderr(),
                        "Stopped without waiting for the close to end."
                    );
                    std::process::exit(128 + i32::from(received.number()));
                }
                host.abort();
            }
        });
    }
    Ok(Signals { first })
}
