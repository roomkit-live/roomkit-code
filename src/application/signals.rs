//! The signals that end an interactive launch.
//!
//! A SIGTERM, the SIGHUP a closing terminal sends, or a SIGINT would end
//! the process where it stands: the agents' processes would outlive it and
//! nothing would be closed. The view reads the keyboard, so Ctrl+C raises
//! no SIGINT while it shows; it does while the application opens and once
//! the view returned the terminal, when a close is under way. The launch
//! listens from before it opens anything, and closes on the first signal
//! as it does when the user leaves.

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
    Interrupt = 3,
}

impl Signal {
    /// The signal's number.
    fn number(self) -> u8 {
        match self {
            Self::Terminate => 15,
            Self::Hangup => 1,
            Self::Interrupt => 2,
        }
    }

    /// The exit code a shell gives a process this signal ended.
    pub(super) fn exit_code(self) -> ExitCode {
        ExitCode::from(128 + self.number())
    }

    /// Ends the process with [`Self::exit_code`], without waiting for the
    /// runtime's threads: the view's may never end once its terminal is
    /// gone. Nothing is flushed here: the view's thread may hold the
    /// standard output's lock in a write that never returns.
    pub(super) fn exit(self) -> ! {
        std::process::exit(128 + i32::from(self.number()))
    }

    fn from_mark(mark: u8) -> Option<Self> {
        match mark {
            1 => Some(Self::Terminate),
            2 => Some(Self::Hangup),
            3 => Some(Self::Interrupt),
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

/// Listens for SIGTERM, SIGHUP and SIGINT. The launch closes on the first
/// one. The second changes nothing: the close is under way. The third exits
/// at once, should the close never end.
pub(super) fn listen() -> Result<Signals> {
    let (first, received) = watch::channel(0);
    let first = Arc::new(first);
    let count = Arc::new(AtomicU8::new(0));
    let kinds = [
        (SignalKind::terminate(), Signal::Terminate),
        (SignalKind::hangup(), Signal::Hangup),
        (SignalKind::interrupt(), Signal::Interrupt),
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
                    crate::stderr::say("Stopped without waiting for the close to end.");
                    std::process::exit(128 + i32::from(signalled.number()));
                }
            }
        });
    }
    Ok(Signals { first: received })
}

/// Whether this process was started with SIGHUP ignored, as `nohup` starts
/// it: a launch nobody watches then goes on when its terminal closes, and
/// listening for the signal would undo that, a handler taking the place of
/// the ignore. Asked before any handler is installed.
///
/// The standard library reads no signal's disposition, and the application
/// forbids unsafe code. A child inherits an ignored signal, and only that:
/// a shell that sends itself SIGHUP lives to answer when it is ignored.
pub(crate) fn hangup_ignored() -> bool {
    let probe = std::process::Command::new("/bin/sh")
        .args(["-c", "kill -HUP $$; echo ignored"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    probe.is_ok_and(|output| output.stdout.starts_with(b"ignored"))
}
