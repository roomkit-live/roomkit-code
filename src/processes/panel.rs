//! The `/processes` panel: what its actions do and what it shows.
use std::num::NonZeroUsize;

use roomkit::RoomKit;
use roomkit::model::ChannelId;
use roomkit_sqlite::SqliteStore;
use roomkit_tools_process::{ProcessChunk, ProcessReadInput, ProcessState, ProcessStopInput};
use roomkit_tui::protocol::{Choice, Event, PanelSpec};
use roomkit_tui_engine::{CommandResult, SessionConfig};
use tokio_util::sync::CancellationToken;

use super::{Job, Processes};

use crate::panel_action::Verb;

const PAGE: u64 = 4096;
const GONE: &str = "Process is no longer available to this view";

#[derive(Clone)]
pub(super) struct Panel {
    id: String,
    room: String,
    agent: String,
    selected: Option<String>,
    offsets: Option<(u64, u64)>, // None follows the tail; Some reads fixed pages.
}

impl Panel {
    /// Whether `current`'s view opened this panel.
    pub(super) fn belongs_to(&self, current: &SessionConfig) -> bool {
        self.room == current.room.as_str() && self.agent == current.agent.as_str()
    }

    /// Shows the list, or process `selected` from its latest output.
    fn select(&mut self, selected: Option<String>) {
        self.selected = selected;
        self.offsets = None;
    }
}

impl Processes {
    pub async fn handle(
        &self,
        kit: &RoomKit<SqliteStore>,
        viewer: &ChannelId,
        current: &SessionConfig,
        arguments: &str,
    ) -> Result<CommandResult, String> {
        let jobs = self.jobs(kit, viewer, current).await;
        let mut panel = match crate::panel_action::addressed(arguments) {
            None => open(current, arguments, &jobs)?,
            Some((id, verb)) => match self.act(current, id, verb, &jobs).await? {
                Some(panel) => panel,
                None => return Ok(CommandResult::Events(Vec::new())),
            },
        };
        // Fresh identity for every navigation/action prevents late refreshes and
        // double clicks from applying to another command or output page.
        panel.id = uuid::Uuid::new_v4().to_string();
        let jobs = self.jobs(kit, viewer, current).await;
        let spec = self.render(&panel, &jobs).await;
        self.panel.send_replace(Some(panel));
        Ok(CommandResult::Events(vec![Event::Panel {
            conversation: current.room.as_str().into(),
            panel: spec,
        }]))
    }

    /// The open panel `id` once `action` applies to it; none once dismissed.
    async fn act(
        &self,
        current: &SessionConfig,
        id: &str,
        verb: Verb<'_>,
        jobs: &[Job],
    ) -> Result<Option<Panel>, String> {
        let mut panel = self
            .panel
            .borrow()
            .clone()
            .ok_or("This process panel is no longer open")?;
        if id != panel.id || !panel.belongs_to(current) {
            return Err("This process action belongs to an older view".into());
        }
        match verb {
            Verb::Dismiss => {
                self.panel.send_replace(None);
                return Ok(None);
            }
            Verb::List => panel.select(None),
            Verb::Open(id) => {
                if !listed(jobs, id) {
                    return Err(GONE.into());
                }
                panel.select(Some(id.into()));
            }
            Verb::Own(action) => self.act_on_selected(&mut panel, action, jobs).await?,
        }
        Ok(Some(panel))
    }

    /// Applies `action` to the panel's selected process.
    async fn act_on_selected(
        &self,
        panel: &mut Panel,
        action: &str,
        jobs: &[Job],
    ) -> Result<(), String> {
        let job = jobs
            .iter()
            .find(|j| Some(&j.process.status.process_id) == panel.selected.as_ref())
            .ok_or(GONE)?;
        match action {
            "stop" | "forget" => {
                let forget = action == "forget";
                self.stop(job, forget).await?;
                if forget {
                    panel.select(None);
                }
            }
            "tail" => panel.offsets = None,
            "first" => panel.offsets = Some(first_page(job)),
            "previous" => panel.offsets = Some(previous_page(panel, job)),
            "next" => panel.offsets = Some(next_page(panel, job)),
            _ => return Err("Unknown process action".into()),
        }
        Ok(())
    }

    /// Stops `job` and, with `forget`, removes the record of its finished run.
    async fn stop(&self, job: &Job, forget: bool) -> Result<(), String> {
        if !job.writable {
            return Err("This conversation is read-only".into());
        }
        if forget && job.process.status.state != ProcessState::Finished {
            return Err("Stop the process before forgetting its record".into());
        }
        self.manager
            .stop(
                job.process.owner.as_deref(),
                ProcessStopInput {
                    process_id: job.process.status.process_id.clone(),
                    forget,
                },
                &CancellationToken::new(),
            )
            .await
            .map_err(|_| "Could not stop or forget this process")?;
        Ok(())
    }

    pub(super) async fn render(&self, panel: &Panel, jobs: &[Job]) -> PanelSpec {
        let mut spec = PanelSpec::new(
            panel.id.clone(),
            "processes",
            "Background processes · current application",
            "",
        )
        .with_dismiss(Verb::Dismiss.to(&panel.id));
        let Some(id) = &panel.selected else {
            list(&mut spec, panel, jobs);
            return spec;
        };
        spec.actions
            .push(choice(panel, Verb::List, "Back to process list", ""));
        let Some(job) = jobs.iter().find(|j| &j.process.status.process_id == id) else {
            spec.body = "Process is no longer retained or accessible.".into();
            return spec;
        };
        spec.title = format!("Process {} · {}", id, state(job));
        spec.body = details(job);
        spec.body.push_str(&self.output(panel, job).await);
        spec.actions.extend(job_actions(panel, job));
        spec
    }

    /// The page of `job`'s output that `panel` shows.
    async fn output(&self, panel: &Panel, job: &Job) -> String {
        let (stdout, stderr) = offsets(panel, job);
        let read = self
            .manager
            .read(
                job.process.owner.as_deref(),
                ProcessReadInput {
                    stdout_offset: stdout,
                    stderr_offset: stderr,
                    max_bytes: NonZeroUsize::new(PAGE as usize),
                    ..ProcessReadInput::new(job.process.status.process_id.clone())
                },
                &CancellationToken::new(),
            )
            .await;
        let Ok(output) = read else {
            return "Output is no longer available.\n".into();
        };
        let mut body = String::from(if panel.offsets.is_none() {
            "Following latest output (bounded tail)\n"
        } else {
            "Fixed output page\n"
        });
        chunk(&mut body, "stdout", &output.data.stdout);
        chunk(&mut body, "stderr", &output.data.stderr);
        body
    }
}

/// A new panel on `current`'s view: the list, or process `arguments` when
/// this view can see it.
fn open(current: &SessionConfig, arguments: &str, jobs: &[Job]) -> Result<Panel, String> {
    let selected = if arguments.is_empty() {
        None
    } else if listed(jobs, arguments) {
        Some(arguments.into())
    } else {
        return Err(GONE.into());
    };
    Ok(Panel {
        // `handle` gives every panel it shows a fresh identity.
        id: String::new(),
        room: current.room.to_string(),
        agent: current.agent.to_string(),
        selected,
        offsets: None,
    })
}

/// Whether process `id` is one of `jobs`.
fn listed(jobs: &[Job], id: &str) -> bool {
    jobs.iter().any(|job| job.process.status.process_id == id)
}

/// The choice that sends `value` back to `panel`.
fn choice(panel: &Panel, verb: Verb<'_>, label: &str, description: &str) -> Choice {
    Choice::new(verb.to(&panel.id), label).with_description(description)
}

/// The running processes of `jobs`, each with the choice that opens it.
fn list(spec: &mut PanelSpec, panel: &Panel, jobs: &[Job]) {
    spec.body = "Running processes across accessible conversations and owners.\nFinished processes leave this list automatically. Closing the application stops running processes.\n\n".into();
    let running: Vec<_> = jobs
        .iter()
        .filter(|job| job.process.status.state == ProcessState::Running)
        .collect();
    if running.is_empty() {
        spec.body.push_str("No running processes.\n");
    }
    for job in running {
        let p = &job.process;
        let command = p.command.split_whitespace().collect::<Vec<_>>().join(" ");
        let command: String = command.chars().take(48).collect();
        let description = format!(
            "{} · {}s · owner {} · conversation {}",
            state(job),
            p.elapsed.as_secs(),
            job.agent,
            job.room
        )
        .chars()
        .take(120)
        .collect::<String>();
        spec.body.push_str(&format!(
            "{}  {command}\n  {description}\n",
            p.status.process_id
        ));
        spec.actions.push(choice(
            panel,
            Verb::Open(&p.status.process_id),
            &format!("{}  {command}", p.status.process_id),
            &description,
        ));
    }
}

/// What the panel states of `job` above its output.
fn details(job: &Job) -> String {
    let p = &job.process;
    let mut body = format!(
        "Command: {}\nOwner: {}\nConversation: {}\nDirectory: {}\nElapsed: {}s · Lifetime limit: {}s\nState: {} · Exit: {} · Signal: {}\n\n",
        p.command,
        job.agent,
        job.room,
        p.status.cwd,
        p.elapsed.as_secs(),
        p.status.timeout_ms / 1000,
        state(job),
        p.status
            .exit_code
            .map_or_else(|| "—".into(), |n| n.to_string()),
        p.status
            .signal
            .map_or_else(|| "—".into(), |n| n.to_string())
    );
    if let Some(error) = &p.status.error {
        body.push_str(&format!("Capture/cleanup: {error}\n"));
    }
    body
}

/// The choices on `job`: stop or forget it in a writable conversation, then
/// page through its output.
fn job_actions(panel: &Panel, job: &Job) -> Vec<Choice> {
    let mut actions = Vec::new();
    if job.writable {
        actions.push(if job.process.status.state == ProcessState::Running {
            choice(
                panel,
                Verb::Own("stop"),
                "Stop process",
                "Terminate this command and its process group",
            )
        } else {
            choice(
                panel,
                Verb::Own("forget"),
                "Forget completed process",
                "Remove its retained process record",
            )
        });
    }
    for (value, label) in [
        ("first", "First retained output"),
        ("previous", "Previous output page"),
        ("next", "Next output page"),
        ("tail", "Follow latest output"),
    ] {
        actions.push(choice(panel, Verb::Own(value), label, ""));
    }
    actions
}

/// The first retained byte of each stream.
fn first_page(job: &Job) -> (u64, u64) {
    let status = &job.process.status;
    (status.stdout_retained_from, status.stderr_retained_from)
}

/// The page before the shown one, from the first retained byte at the
/// earliest.
fn previous_page(panel: &Panel, job: &Job) -> (u64, u64) {
    let status = &job.process.status;
    let (out, err) = offsets(panel, job);
    (
        out.saturating_sub(PAGE).max(status.stdout_retained_from),
        err.saturating_sub(PAGE).max(status.stderr_retained_from),
    )
}

/// The page after the shown one, from the end of the output at the latest.
fn next_page(panel: &Panel, job: &Job) -> (u64, u64) {
    let status = &job.process.status;
    let (out, err) = offsets(panel, job);
    (
        out.saturating_add(PAGE).min(status.stdout_bytes),
        err.saturating_add(PAGE).min(status.stderr_bytes),
    )
}

fn offsets(panel: &Panel, job: &Job) -> (u64, u64) {
    panel.offsets.unwrap_or_else(|| {
        (
            job.process
                .status
                .stdout_bytes
                .saturating_sub(PAGE)
                .max(job.process.status.stdout_retained_from),
            job.process
                .status
                .stderr_bytes
                .saturating_sub(PAGE)
                .max(job.process.status.stderr_retained_from),
        )
    })
}

fn state(job: &Job) -> &'static str {
    use roomkit_tools_process::ProcessEnd;
    match (job.process.status.state, job.process.status.end) {
        (ProcessState::Running, _) => "running",
        (_, Some(ProcessEnd::Stopped)) => "stopped",
        (_, Some(ProcessEnd::TimedOut)) => "timed out",
        (_, Some(ProcessEnd::Failed)) => "failed",
        _ if job.process.status.exit_code == Some(0) => "completed",
        _ if job.process.status.exit_code.is_some_and(|code| code != 0)
            || job.process.status.signal.is_some() =>
        {
            "failed"
        }
        _ => "finished",
    }
}

fn chunk(body: &mut String, name: &str, chunk: &ProcessChunk) {
    body.push_str(&format!(
        "\n{name} · bytes {}–{} / {}{}\n",
        chunk.offset,
        chunk.next_offset,
        chunk.total_bytes,
        if chunk.has_more {
            " · more available"
        } else {
            ""
        }
    ));
    if chunk.omitted_bytes > 0 {
        body.push_str(&format!(
            "{} earlier bytes no longer retained\n",
            chunk.omitted_bytes
        ));
    }
    body.push_str(if chunk.text.is_empty() {
        "(no output)"
    } else {
        &chunk.text
    });
    if !body.ends_with('\n') {
        body.push('\n');
    }
}
