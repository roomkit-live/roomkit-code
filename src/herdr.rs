//! Optional Herdr TUI boundary. Receives compact reports, never application state.

mod sequence;

use std::{ffi::OsString, path::PathBuf, process::Stdio, time::Duration};
use tokio::{process::Command, sync::oneshot, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const SOURCE: &str = "roomkit-code";
const TITLE_SOURCE: &str = "roomkit-code-title";
const CALL_TIMEOUT: Duration = Duration::from_millis(500);
const POLL: Duration = Duration::from_millis(100);
const RETRY: Duration = Duration::from_secs(1);

pub(crate) struct Endpoint {
    binary: PathBuf,
    socket: OsString,
    pane: String,
    storage: PathBuf,
}

impl Endpoint {
    pub fn detect(storage: Option<PathBuf>) -> Option<Self> {
        Self::from_environment(storage, |key| std::env::var_os(key))
    }

    fn from_environment(
        storage: Option<PathBuf>,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Option<Self> {
        if env("HERDR_ENV")?.as_os_str() != "1" {
            return None;
        }
        let binary = PathBuf::from(env("HERDR_BIN_PATH")?);
        let socket = env("HERDR_SOCKET_PATH")?;
        let pane = env("HERDR_PANE_ID")?.into_string().ok()?;
        if binary.as_os_str().is_empty()
            || socket.is_empty()
            || pane.is_empty()
            || pane.len() > 256
            || pane.starts_with('-')
            || pane.chars().any(char::is_control)
        {
            return None;
        }
        Some(Self {
            binary,
            socket,
            pane,
            storage: storage?.join("integrations/herdr"),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Idle,
    Working,
    Blocked,
}

impl State {
    fn value(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionRef {
    pub id: String,
    pub resume: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    pub state: State,
    pub session: SessionRef,
    pub title: String,
}

pub(crate) fn valid_resume(args: &[String]) -> bool {
    let Some(command) = args.first() else {
        return false;
    };
    !command.is_empty()
        && !command.starts_with('-')
        && command
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        && args.len() <= 64
        && args.iter().map(String::len).sum::<usize>() <= 8192
        && args
            .iter()
            .all(|arg| !arg.contains('\'') && !arg.chars().any(char::is_control))
}

pub(crate) struct Reporter {
    freeze: CancellationToken,
    release: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Reporter {
    pub fn start(
        endpoint: Endpoint,
        mut latest: impl FnMut() -> Option<Report> + Send + 'static,
        diagnostic: impl Fn(&'static str) + Send + 'static,
    ) -> Self {
        let freeze = CancellationToken::new();
        let stop = freeze.clone();
        let (release, released) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut sequence = match sequence::Sequence::open(&endpoint) {
                Ok(sequence) => sequence,
                Err(_) => {
                    diagnostic("sequence_unavailable");
                    return;
                }
            };
            let mut sent = Sent::default();
            let mut attempted = false;
            let mut invalid_logged = false;
            loop {
                if stop.is_cancelled() {
                    break;
                }
                let Some(mut report) = latest() else {
                    if wait(POLL, &stop).await {
                        break;
                    }
                    continue;
                };
                report.title = report
                    .title
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(80)
                    .collect();
                if report
                    .session
                    .resume
                    .as_ref()
                    .is_some_and(|args| !valid_resume(args))
                {
                    report.session.resume = None;
                }
                if report.session.resume.is_none() && !invalid_logged {
                    diagnostic("resume_unavailable");
                }
                invalid_logged = report.session.resume.is_none();
                let Some(action) = sent.next(&report) else {
                    if wait(POLL, &stop).await {
                        break;
                    }
                    continue;
                };
                attempted = true;
                match call(&endpoint, &mut sequence, action.args(&report), &stop).await {
                    Ok(()) => sent.applied(action, &report),
                    Err("stopped") => break,
                    Err(code) => {
                        diagnostic(code);
                        // Reacquire authority after a restart or a partially applied report.
                        sent = Sent::default();
                        if wait(RETRY, &stop).await {
                            break;
                        }
                    }
                }
            }
            // The application freezes updates on terminal exit, then authorizes
            // release only after its engine/tools have finished cleanup.
            let _ = released.await;
            if attempted {
                let cleanup = CancellationToken::new();
                for args in [metadata_args(None), lifecycle_args("release-agent")] {
                    if let Err(code) = call(&endpoint, &mut sequence, args, &cleanup).await {
                        diagnostic(code);
                    }
                }
            }
        });
        Self {
            freeze,
            release,
            task,
        }
    }

    pub fn freeze(&self) {
        self.freeze.cancel();
    }

    pub async fn close(self) {
        self.freeze();
        let _ = self.release.send(());
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(3), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

#[derive(Default)]
struct Sent {
    state: Option<State>,
    session: Option<SessionRef>,
    title: Option<String>,
    resume_cleared: bool,
}

#[derive(Clone, Copy)]
enum Action {
    ClearResume,
    State,
    Session,
    Title,
}

impl Sent {
    fn next(&self, report: &Report) -> Option<Action> {
        // Omitting resume_argv does not remove a previously stored command in
        // Herdr. An unrepresentable launch requires a scoped release/reclaim.
        if report.session.resume.is_none() && !self.resume_cleared {
            Some(Action::ClearResume)
        } else if self.state != Some(report.state) {
            Some(Action::State)
        } else if self.session.as_ref() != Some(&report.session) {
            Some(Action::Session)
        } else if self.title.as_ref() != Some(&report.title) {
            Some(Action::Title)
        } else {
            None
        }
    }

    fn applied(&mut self, action: Action, report: &Report) {
        match action {
            Action::ClearResume => {
                *self = Self {
                    resume_cleared: true,
                    ..Self::default()
                };
            }
            Action::State => {
                self.state = Some(report.state);
                self.session = Some(report.session.clone());
            }
            Action::Session => self.session = Some(report.session.clone()),
            Action::Title => self.title = Some(report.title.clone()),
        }
        if report.session.resume.is_some() {
            self.resume_cleared = false;
        }
    }
}

impl Action {
    fn args(self, report: &Report) -> Vec<String> {
        match self {
            Self::ClearResume => lifecycle_args("release-agent"),
            Self::Title => metadata_args(Some(&report.title)),
            Self::State | Self::Session => {
                let mut args = lifecycle_args(if matches!(self, Self::State) {
                    "report-agent"
                } else {
                    "report-agent-session"
                });
                if matches!(self, Self::State) {
                    args.extend(["--state".into(), report.state.value().into()]);
                }
                args.extend(["--agent-session-id".into(), report.session.id.clone()]);
                if let Some(resume) = &report.session.resume {
                    args.push("--".into());
                    args.extend(resume.clone());
                }
                args
            }
        }
    }
}

fn lifecycle_args(command: &str) -> Vec<String> {
    ["pane", command, "--source", SOURCE, "--agent", SOURCE]
        .map(String::from)
        .to_vec()
}

fn metadata_args(title: Option<&str>) -> Vec<String> {
    let mut args = [
        "pane",
        "report-metadata",
        "--source",
        TITLE_SOURCE,
        "--agent",
        SOURCE,
        "--applies-to-source",
        SOURCE,
    ]
    .map(String::from)
    .to_vec();
    match title {
        Some(title) if !title.is_empty() => args.extend(["--title".into(), title.into()]),
        _ => args.push("--clear-title".into()),
    }
    args
}

async fn wait(duration: Duration, stop: &CancellationToken) -> bool {
    tokio::select! { biased; () = stop.cancelled() => true, () = tokio::time::sleep(duration) => false }
}

async fn call(
    endpoint: &Endpoint,
    sequence: &mut sequence::Sequence,
    mut args: Vec<String>,
    stop: &CancellationToken,
) -> Result<(), &'static str> {
    if stop.is_cancelled() {
        return Err("stopped");
    }
    let seq = sequence.reserve().map_err(|_| "sequence_unavailable")?;
    args.splice(
        2..2,
        [endpoint.pane.clone(), "--seq".into(), seq.to_string()],
    );
    if stop.is_cancelled() {
        return Err("stopped");
    }
    let mut child = Command::new(&endpoint.binary)
        .args(args)
        .env("HERDR_SOCKET_PATH", &endpoint.socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "reporter_unavailable")?;
    let failure = tokio::select! {
        biased;
        () = stop.cancelled() => "stopped",
        status = child.wait() => return match status { Ok(status) if status.success() => Ok(()), _ => Err("report_failed") },
        () = tokio::time::sleep(CALL_TIMEOUT) => "report_timeout",
    };
    let _ = child.kill().await;
    let _ = child.wait().await;
    Err(failure)
}

#[cfg(test)]
mod tests;
