//! A launch nobody watches (`--prompt`): one message, sent through the host
//! protocol the terminal speaks, so its turn runs under the same hooks,
//! permissions, plans and history as in the terminal. Standard output gets
//! the agent's answer, or every event as one JSON line; standard error gets
//! what a person should know and the exit report.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use roomkit_tui::protocol::{Snapshot, limits::MAX_TEXT_BYTES};
use roomkit_tui::{Client, connection};

use super::{Application, QUEUE, integrations::Resume};
use crate::{Result, config::Cli, config::Settings};

mod driver;
mod progress;

use driver::{Driver, Ending, Step};
use progress::Progress;

/// How often the launch looks for a signal between host events.
const POLL: Duration = Duration::from_millis(100);
/// The exit status of a launch a signal stopped, as shells report SIGINT.
const INTERRUPTED: u8 = 130;

/// The message an unattended launch sends, and how it reports.
pub(crate) struct Prompt {
    text: String,
    json: bool,
}

impl Prompt {
    /// The message `--prompt` names, read from standard input for `-`; none
    /// for a launch with the terminal interface.
    pub fn from_cli(cli: &Cli) -> Result<Option<Self>> {
        let Some(text) = &cli.prompt else {
            return Ok(None);
        };
        let text = if text == "-" {
            std::io::read_to_string(std::io::stdin())
                .map_err(|_| "Cannot read the message from standard input")?
        } else {
            text.clone()
        };
        if text.trim().is_empty() {
            return Err("The message to send is empty".into());
        }
        if text.len() > MAX_TEXT_BYTES {
            let error =
                format!("The message exceeds {MAX_TEXT_BYTES} bytes, the most one may hold");
            return Err(error.into());
        }
        Ok(Some(Self {
            text,
            json: cli.json,
        }))
    }
}

/// What the launch learned of its conversation once it ended.
struct Outcome {
    ending: Ending,
    answer: Option<String>,
    /// The conversation and its agent, once the host named them.
    conversation: Option<(String, String)>,
    /// Someone watched the launch's progress: the answer and the report
    /// stand apart from it.
    framed: bool,
}

pub(crate) async fn run(settings: Settings, prompt: Prompt) -> Result<ExitCode> {
    // Listen first: a signal while the application opens keeps the message
    // from being sent, and the launch still closes what it opened.
    let signals = listen()?;
    let api_key = settings.initial_api_key();
    let (app, first) = open(&settings, api_key).await?;
    let progress = watcher(&app, &settings, &first, prompt.json).await;
    let (client, host) = connection(QUEUE);
    let adapter = tokio::spawn(
        app.session()
            .run_with_commands(host, app.router(&settings.workspace)),
    );
    let Prompt { text, json } = prompt;
    let launch = format!("headless-{}", uuid::Uuid::new_v4().simple());
    let driver = Driver::new(text, launch);
    let driven = tokio::task::spawn_blocking(move || {
        drive(&client, driver, json, &signals.pending, progress)
    })
    .await;
    // The view ended with its client: the host stops serving it.
    let attached = adapter.await;
    let summary = match &driven {
        Ok(Ok(outcome)) => report(&app, &settings, outcome, json).await,
        // A launch cut short, its output closed for one, still says what it
        // leaves running.
        _ => Some(
            crate::exit_summary::Summary::of_processes(&app.profiles.resources.processes).await,
        ),
    };
    let closed = app.close().await;
    if let Some(summary) = &summary {
        // Closing an unattended launch lets its processes go, still running.
        let fate = if closed.is_ok() {
            crate::exit_summary::Fate::LeftRunning
        } else {
            crate::exit_summary::Fate::Unknown
        };
        for line in summary.process_report(fate) {
            eprintln!("{line}");
        }
    }
    let outcome = driven??;
    attached??;
    closed?;
    Ok(match outcome.ending {
        Ending::Completed => ExitCode::SUCCESS,
        Ending::Failed => ExitCode::FAILURE,
        Ending::Interrupted => ExitCode::from(INTERRUPTED),
    })
}

/// The application and its conversation as the launch opens it, closed again
/// should the conversation not read.
async fn open(settings: &Settings, api_key: String) -> Result<(Application, Snapshot)> {
    let app = Application::open(settings, api_key).await?;
    match app.session().snapshot().await {
        Ok(snapshot) => Ok((app, snapshot)),
        Err(error) => {
            let _ = app.close().await;
            Err(error.into())
        }
    }
}

/// What shows the launch's progress, to a person at a terminal: a program
/// reads the events, and a log has the report.
async fn watcher(
    app: &Application,
    settings: &Settings,
    first: &Snapshot,
    json: bool,
) -> Option<Progress> {
    if json || !std::io::stderr().is_terminal() {
        return None;
    }
    let provider = app
        .profiles
        .provider_label(first.selected_agent.as_str())
        .await;
    Some(Progress::new(&settings.workspace, provider))
}

/// Write the answer, unless the events were, then the conversation's part of
/// the exit report: before closing, which can take a while. The rest of the
/// report, the processes closing leaves running, comes once it has.
async fn report(
    app: &Application,
    settings: &Settings,
    outcome: &Outcome,
    json: bool,
) -> Option<crate::exit_summary::Summary> {
    if !json && let Some(answer) = &outcome.answer {
        apart(outcome);
        write_answer(answer);
    }
    let summary = summarize(app, settings, outcome).await?;
    apart(outcome);
    for line in summary.conversation_report() {
        eprintln!("{line}");
    }
    Some(summary)
}

/// A blank line, under the progress someone watched.
fn apart(outcome: &Outcome) {
    if outcome.framed {
        eprintln!();
    }
}

/// Serve the driver the host's events until the launch ends; with `json`,
/// write each event first, and with `progress`, what it says of each.
fn drive(
    client: &Client,
    mut driver: Driver,
    json: bool,
    stop: &AtomicBool,
    mut progress: Option<Progress>,
) -> Result<Outcome> {
    let mut out = std::io::stdout().lock();
    loop {
        if stop.swap(false, Ordering::SeqCst) {
            apply(client, driver.interrupt())?;
        }
        if let Some(ending) = driver.ending() {
            let conversation = driver
                .conversation()
                .map(|(room, agent)| (room.as_str().to_owned(), agent.as_str().to_owned()));
            return Ok(Outcome {
                ending,
                answer: driver.answer().map(str::to_owned),
                conversation,
                framed: progress.is_some(),
            });
        }
        match client.recv_timeout(POLL) {
            Ok(event) => {
                if json {
                    write_event(&mut out, &event)
                        .map_err(|error| format!("Cannot write the events: {error}"))?;
                }
                if let Some(progress) = &mut progress {
                    tell(&progress.on_event(&event));
                }
                apply(client, driver.on_event(&event))?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("The conversation stopped before its turn ended".into());
            }
        }
    }
}

fn tell(lines: &[String]) {
    for line in lines {
        eprintln!("{line}");
    }
}

fn write_event(out: &mut impl Write, event: &roomkit_tui::protocol::Event) -> Result<()> {
    serde_json::to_writer(&mut *out, event)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

/// Write the answer on standard output. A reader that stopped reading, as
/// `head` does, leaves the rest unread: the launch still closes and reports.
fn write_answer(answer: &str) {
    let mut out = std::io::stdout().lock();
    let written = writeln!(out, "{answer}").and_then(|()| out.flush());
    if let Err(error) = written
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        eprintln!("Cannot write the answer: {error}");
    }
}

fn apply(client: &Client, steps: Vec<Step>) -> Result<()> {
    for step in steps {
        match step {
            Step::Send(request) => client
                .try_send(request)
                .map_err(|_| "The conversation no longer accepts requests")?,
            Step::Tell(text) => {
                let lines: Vec<String> = text.lines().map(progress::printable).collect();
                tell(&lines);
            }
        }
    }
    Ok(())
}

/// The signals the launch received. The first SIGINT or SIGTERM raises
/// `pending`, which the launch reads between events to cut its turn; the
/// next one exits at once, should the cut or the close never end.
struct Signals {
    pending: Arc<AtomicBool>,
}

fn listen() -> Result<Signals> {
    use tokio::signal::unix::{SignalKind, signal};
    let pending = Arc::new(AtomicBool::new(false));
    let received = Arc::new(AtomicU32::new(0));
    for kind in [SignalKind::interrupt(), SignalKind::terminate()] {
        let mut signals = signal(kind)?;
        let (pending, received) = (pending.clone(), received.clone());
        tokio::spawn(async move {
            while signals.recv().await.is_some() {
                if received.fetch_add(1, Ordering::SeqCst) > 0 {
                    eprintln!("Stopped without waiting for the turn to close.");
                    std::process::exit(i32::from(INTERRUPTED));
                }
                pending.store(true, Ordering::SeqCst);
            }
        });
    }
    Ok(Signals { pending })
}

/// The exit report of the launch's conversation, while the store and the
/// processes are still open.
async fn summarize(
    app: &Application,
    settings: &Settings,
    outcome: &Outcome,
) -> Option<crate::exit_summary::Summary> {
    let (room, agent) = outcome.conversation.as_ref()?;
    let mode = app.policy.state(room).selected;
    let command = Resume::shown(settings).command(room, &mode);
    let priced = super::priced(app, room, agent).await;
    let processes = &app.profiles.resources.processes;
    let shown = Some((room.clone(), command));
    Some(crate::exit_summary::collect(&app.kit, processes, shown, priced).await)
}
