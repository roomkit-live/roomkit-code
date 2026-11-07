//! A launch nobody watches (`--prompt`): one message, sent through the host
//! protocol the terminal speaks, so its turn runs under the same hooks,
//! permissions, plans and history as in the terminal. Standard output gets
//! the agent's answer, or every event as one JSON line; standard error gets
//! what a person should know and the exit report.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use roomkit_tui::{Client, connection};

use super::{Application, QUEUE, integrations::Resume};
use crate::{Result, config::Cli, config::Settings};

mod driver;

use driver::{Driver, Ending, Step};

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
}

pub(crate) async fn run(settings: Settings, prompt: Prompt) -> Result<ExitCode> {
    let api_key = std::env::var("ROOMKIT_API_KEY").unwrap_or_default();
    let app = Application::open(&settings, api_key).await?;
    if let Err(error) = app.session().snapshot().await {
        let _ = app.close().await;
        return Err(error.into());
    }
    let (client, host) = connection(QUEUE);
    let adapter = tokio::spawn(
        app.session()
            .run_with_commands(host, app.router(&settings.workspace)),
    );
    let stop = listen()?;
    let Prompt { text, json } = prompt;
    let driven =
        tokio::task::spawn_blocking(move || drive(&client, Driver::new(text), json, &stop)).await;
    // The view ended with its client: the host stops serving it.
    let attached = adapter.await;
    let summary = match &driven {
        Ok(Ok(outcome)) => report(&app, &settings, outcome, json).await,
        _ => None,
    };
    let closed = app.close().await;
    if let Some(summary) = &summary {
        for line in summary.process_report(closed.is_ok()) {
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

/// Write the answer, unless the events were, then the conversation's part of
/// the exit report: before closing, which can take a while. The rest of the
/// report, the processes closing stops, comes once it has.
async fn report(
    app: &Application,
    settings: &Settings,
    outcome: &Outcome,
    json: bool,
) -> Option<crate::exit_summary::Summary> {
    if !json && let Some(answer) = &outcome.answer {
        println!("{answer}");
    }
    let summary = summarize(app, settings, outcome).await?;
    for line in summary.conversation_report() {
        eprintln!("{line}");
    }
    Some(summary)
}

/// Serve the driver the host's events until the launch ends; with `json`,
/// write each event first.
fn drive(client: &Client, mut driver: Driver, json: bool, stop: &AtomicBool) -> Result<Outcome> {
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
            });
        }
        match client.recv_timeout(POLL) {
            Ok(event) => {
                if json {
                    serde_json::to_writer(&mut out, &event)?;
                    out.write_all(b"\n")?;
                    out.flush()?;
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

fn apply(client: &Client, steps: Vec<Step>) -> Result<()> {
    for step in steps {
        match step {
            Step::Send(request) => client
                .try_send(request)
                .map_err(|_| "The conversation no longer accepts requests")?,
            Step::Tell(text) => eprintln!("{text}"),
        }
    }
    Ok(())
}

/// A flag every SIGINT or SIGTERM raises; the launch reads it between events.
fn listen() -> Result<Arc<AtomicBool>> {
    use tokio::signal::unix::{SignalKind, signal};
    let stop = Arc::new(AtomicBool::new(false));
    for kind in [SignalKind::interrupt(), SignalKind::terminate()] {
        let mut signals = signal(kind)?;
        let stop = stop.clone();
        tokio::spawn(async move {
            while signals.recv().await.is_some() {
                stop.store(true, Ordering::SeqCst);
            }
        });
    }
    Ok(stop)
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
    let command = Resume::new(settings).command(room, &mode);
    let priced = super::priced(app, room, agent).await;
    let processes = &app.profiles.resources.processes;
    let shown = Some((room.clone(), command));
    Some(crate::exit_summary::collect(&app.kit, processes, shown, priced).await)
}
