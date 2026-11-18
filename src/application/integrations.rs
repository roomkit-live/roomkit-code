//! Assemble optional consumers from CLI facts; product paths and policy stay here.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use crate::{
    config::Settings,
    herdr::{self, Report, SessionRef, State},
};
use roomkit_tools_process::{ProcessSession, ProcessState};
use roomkit_tui::{SessionObservation, SessionObserver};
use tokio::task::JoinHandle;

/// How long the end of the last delegated task keeps the application
/// working. roomkit-rs records a task's end before its result starts the
/// turn that reads it (`kit/tasks.rs`); the view shows that turn within it.
const HAND_BACK: Duration = Duration::from_secs(2);

pub(super) struct Integration {
    reporter: Option<herdr::Reporter>,
    processes: Option<JoinHandle<()>>,
}

impl Integration {
    /// Report the observed view to Herdr when its endpoint is detected,
    /// with what runs in the background of the whole application: its
    /// delegated `tasks` and the `processes` closing it stops.
    pub fn start(
        settings: &Settings,
        diagnostics: Arc<crate::diagnostics::Diagnostics>,
        observer: &SessionObserver,
        tasks: Arc<crate::delegation::Tasks>,
        processes: ProcessSession,
    ) -> Self {
        let endpoint = settings
            .herdr
            .then(|| herdr::Endpoint::detect(crate::config::default_state_dir()))
            .flatten();
        let Some(endpoint) = endpoint else {
            return Self {
                reporter: None,
                processes: None,
            };
        };
        let running = Arc::new(AtomicUsize::new(0));
        let counter = tokio::spawn(count_running(processes, running.clone()));
        let reporter = herdr::Reporter::start(
            endpoint,
            observed(observer.clone(), Resume::new(settings), tasks, running),
            move |code| {
                diagnostics.record("herdr_integration", serde_json::json!({"code":code}), None)
            },
        );
        Self {
            reporter: Some(reporter),
            processes: Some(counter),
        }
    }

    pub fn freeze(&self) {
        if let Some(reporter) = &self.reporter {
            reporter.freeze();
        }
    }
    pub async fn close(self) {
        if let Some(reporter) = self.reporter {
            reporter.close().await;
        }
        if let Some(counter) = self.processes {
            counter.abort();
        }
    }
}

/// The report of the view `source` observes, with what runs in the
/// background: `tasks` and the `running` processes.
fn observed(
    source: SessionObserver,
    resume: Resume,
    tasks: Arc<crate::delegation::Tasks>,
    running: Arc<AtomicUsize>,
) -> impl FnMut() -> Option<Report> + Send + 'static {
    let mut delegated = Delegated::default();
    move || {
        source.latest().map(|mut state| {
            // A view can be in a task's room: the session is its
            // conversation, the one a resume command reopens.
            state.conversation = tasks.root_of(state.conversation.as_str()).into();
            let background = Background {
                delegated: delegated.working(tasks.running(), Instant::now()),
                processes: running.load(Ordering::Relaxed),
            };
            resume.report(state, background)
        })
    }
}

/// Keeps `running` at the number of `manager`'s running processes, every
/// owner's: the ones closing the application stops.
async fn count_running(manager: ProcessSession, running: Arc<AtomicUsize>) {
    let mut lifecycle = manager.subscribe();
    loop {
        let count = manager
            .observe()
            .await
            .iter()
            .filter(|process| process.status.state == ProcessState::Running)
            .count();
        running.store(count, Ordering::Relaxed);
        if lifecycle.changed().await.is_err() {
            return;
        }
    }
}

/// What runs beside the view's turns, in the whole application.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Background {
    /// Whether delegated work keeps the application working ([`Delegated`]).
    pub delegated: bool,
    /// Running processes.
    pub processes: usize,
}

impl Background {
    /// What Herdr shows beside the idle and done states: the processes that
    /// run on. They keep no agent working: a server runs until closing.
    fn label(self) -> Option<String> {
        match self.processes {
            0 => None,
            1 => Some("1 process".into()),
            n => Some(format!("{n} processes")),
        }
    }
}

/// Follows the delegated tasks: a running task keeps the application
/// working, since its result starts a turn of its own, and so does the end
/// of the last one for [`HAND_BACK`].
#[derive(Debug, Default)]
pub(super) struct Delegated {
    /// When a task was last seen running.
    seen: Option<Instant>,
}

impl Delegated {
    /// Whether delegated work keeps the application working at `now`, with
    /// `running` tasks.
    pub(super) fn working(&mut self, running: usize, now: Instant) -> bool {
        if running > 0 {
            self.seen = Some(now);
        }
        self.seen
            .is_some_and(|seen| now.saturating_duration_since(seen) < HAND_BACK)
    }
}

/// The command line that reopens a conversation of this project with the
/// launch's explicit options. Herdr and the exit summary share it.
pub(super) struct Resume {
    prefix: Option<Vec<String>>,
    options: Option<Vec<String>>,
}

impl Resume {
    pub(super) fn new(settings: &Settings) -> Self {
        let prefix = settings
            .workspace
            .to_str()
            .zip(settings.state_dir.to_str())
            .map(|(workspace, storage)| {
                ["roomkit-code", workspace, "--state-dir", storage]
                    .map(String::from)
                    .to_vec()
            });
        Self {
            prefix,
            options: settings.resume_options.clone(),
        }
    }

    /// The command as a person reads it in the exit report: the default
    /// storage goes without saying. Herdr keeps the full one, which it runs
    /// later, possibly from another environment.
    pub(super) fn shown(settings: &Settings) -> Self {
        let mut resume = Self::new(settings);
        if settings.default_storage
            && let Some(prefix) = &mut resume.prefix
        {
            // The program and the workspace stay.
            prefix.truncate(2);
        }
        resume
    }

    /// Arguments reopening `conversation` in `mode`, or None when they cannot
    /// be expressed safely (unknown mode, non-UTF-8 path, quote in a path).
    pub(super) fn command(&self, conversation: &str, mode: &str) -> Option<Vec<String>> {
        if !matches!(mode, "ask" | "plan" | "yolo") {
            return None;
        }
        let mut args = self.prefix.clone()?;
        args.extend([
            "--session".into(),
            conversation.into(),
            "--mode".into(),
            mode.into(),
        ]);
        args.extend(self.options.clone()?);
        herdr::valid_resume(&args).then_some(args)
    }

    fn report(&self, observation: SessionObservation, background: Background) -> Report {
        let resume = observation.mode.as_ref().and_then(|mode| {
            self.command(observation.conversation.as_str(), mode.selected.as_str())
        });
        let state = if !observation.permissions.is_empty() || !observation.questions.is_empty() {
            State::Blocked
        } else if observation
            .operation
            .as_ref()
            .is_some_and(|operation| operation.outcome.is_none())
            || observation
                .run
                .as_ref()
                .is_some_and(|run| run.outcome.is_none())
            || background.delegated
        {
            State::Working
        } else {
            State::Idle
        };
        Report {
            state,
            background: background.label(),
            title: observation.title,
            session: SessionRef {
                id: observation.conversation.into_inner(),
                resume,
            },
        }
    }
}

#[cfg(test)]
mod tests;
