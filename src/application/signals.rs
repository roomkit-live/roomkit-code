//! The signals that end an interactive launch.
//!
//! The terminal reads the keyboard, so Ctrl+C never raises SIGINT here. A
//! SIGTERM, or the SIGHUP a closing terminal sends, would end the process
//! where it stands: the agents' processes would outlive it and nothing
//! would be closed. The launch listens from before it opens anything, and
//! closes on the first one as it does when the user leaves.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;

use crate::Result;

/// A signal that ends the launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Signal {
    Terminate = 1,
    /// The terminal is gone: nothing can be written to it any more, and
    /// the view that reads it may never return.
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

    /// Ends the process with [`Self::exit_code`], without waiting for the
    /// runtime's threads: the view's may never end once its terminal is
    /// gone.
    pub(super) fn exit(self) -> ! {
        let _ = std::io::stdout().flush();
        std::process::exit(128 + i32::from(self.number()))
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
    first: watch::Receiver<u8>,
}

impl Signals {
    /// The first signal received, if any.
    pub(super) fn received(&self) -> Option<Signal> {
        Signal::from_mark(*self.first.borrow())
    }

    /// Waits for the first signal.
    pub(super) async fn wait(&self) -> Signal {
        let mut first = self.first.clone();
        loop {
            if let Some(signal) = Signal::from_mark(*first.borrow_and_update()) {
                return signal;
            }
            if first.changed().await.is_err() {
                // The listeners live as long as the runtime.
                std::future::pending::<()>().await;
            }
        }
    }
}

/// The signals after which the launch exits without waiting for its close.
const EXIT_AT: u8 = 3;

/// Listens for SIGTERM and SIGHUP. The launch closes on the first one. The
/// second changes nothing: the close is under way. The third exits at once,
/// should the close never end.
pub(super) fn listen() -> Result<Signals> {
    let (first, received) = watch::channel(0);
    let first = Arc::new(first);
    let count = Arc::new(AtomicU8::new(0));
    let kinds = [
        (SignalKind::terminate(), Signal::Terminate),
        (SignalKind::hangup(), Signal::Hangup),
    ];
    for (kind, signalled) in kinds {
        let mut signals = signal(kind)?;
        let (first, count) = (first.clone(), count.clone());
        tokio::spawn(async move {
            while signals.recv().await.is_some() {
                first.send_if_modified(|mark| {
                    let unset = *mark == 0;
                    if unset {
                        *mark = signalled as u8;
                    }
                    unset
                });
                if count.fetch_add(1, Ordering::SeqCst) + 1 >= EXIT_AT {
                    // The terminal may be gone: a failed write must not
                    // panic here.
                    let _ = writeln!(
                        std::io::stderr(),
                        "Stopped without waiting for the close to end."
                    );
                    std::process::exit(128 + i32::from(signalled.number()));
                }
            }
        });
    }
    Ok(Signals { first: received })
}
