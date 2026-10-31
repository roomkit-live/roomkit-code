//! Human controls for app-owned background processes; never model tool calls.
use std::{collections::BTreeMap, num::NonZeroUsize, sync::Arc, time::Duration};

use roomkit::model::{Access, ChannelId, RoomId};
use roomkit::{GetRoomOptions, ListBindingsOptions, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tools_process::{
    ObservedProcess, ProcessChunk, ProcessManager, ProcessReadInput, ProcessState, ProcessStopInput,
};
use roomkit_tui::protocol::{
    BackgroundStatus, Choice, Event, PanelSpec, SidebarContent, SidebarEntry, SidebarSection,
    SidebarTone,
};
use roomkit_tui_engine::{CommandResult, SessionConfig};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const PAGE: u64 = 4096;

#[derive(Clone)]
struct Panel {
    id: String,
    room: String,
    agent: String,
    selected: Option<String>,
    offsets: Option<(u64, u64)>, // None follows the tail; Some reads fixed pages.
}

pub(crate) struct Processes {
    manager: ProcessManager,
    panel: watch::Sender<Option<Panel>>,
    sidebar: watch::Sender<(uuid::Uuid, SidebarSection)>,
}

struct Job {
    process: ObservedProcess,
    room: String,
    agent: String,
    writable: bool,
}

impl Processes {
    pub fn new(manager: ProcessManager) -> Arc<Self> {
        Arc::new(Self {
            manager,
            panel: watch::channel(None).0,
            sidebar: watch::channel((uuid::Uuid::nil(), sidebar(&[]))).0,
        })
    }

    pub fn sidebar(&self) -> SidebarSection {
        self.sidebar.borrow().1.clone()
    }

    /// Closing the application stops every running process, whatever its
    /// owner or conversation; the terminal states it before detaching.
    async fn exit_warning(&self, conversation: &str) -> Event {
        let running = self
            .manager
            .observe()
            .await
            .iter()
            .filter(|process| process.status.state == ProcessState::Running)
            .count();
        Event::ExitWarning {
            conversation: conversation.into(),
            text: match running {
                0 => None,
                1 => Some("1 running process will be stopped".into()),
                n => Some(format!("{n} running processes will be stopped")),
            },
        }
    }

    /// The UI owns observation lifetime. No periodic work while panels are closed.
    pub fn subscribe(
        self: &Arc<Self>,
        kit: RoomKit<SqliteStore>,
        viewer: ChannelId,
        current: SessionConfig,
    ) -> watch::Receiver<Vec<Event>> {
        self.panel.send_replace(None);
        let observation = uuid::Uuid::new_v4();
        self.sidebar.send_replace((observation, sidebar(&[])));
        let (tx, rx) = watch::channel(Vec::new());
        let this = self.clone();
        let mut lifecycle = this.manager.subscribe();
        let mut panel_changes = this.panel.subscribe();
        tokio::spawn(async move {
            loop {
                let panel = panel_changes.borrow_and_update().clone().filter(|p| {
                    p.room == current.room.as_str() && p.agent == current.agent.as_str()
                });
                let jobs = this.jobs(&kit, &viewer, &current).await;
                let running = jobs
                    .iter()
                    .filter(|j| j.process.status.state == ProcessState::Running)
                    .count();
                let mut events = vec![
                    Event::BackgroundChanged {
                        conversation: current.room.as_str().into(),
                        status: (running > 0).then(|| {
                            BackgroundStatus::new(
                                format!("Processes: {running} running"),
                                "processes",
                                "",
                            )
                        }),
                    },
                    this.exit_warning(current.room.as_str()).await,
                ];
                if let Some(panel) = &panel {
                    events.push(Event::PanelUpdated {
                        conversation: current.room.as_str().into(),
                        panel: this.render(panel, &jobs).await,
                    });
                }
                let section = sidebar(&jobs);
                // A cancelled observer can finish its in-flight read after a
                // view switch. It must not overwrite the new observer's section.
                let section_changed = this.sidebar.send_if_modified(|(current, previous)| {
                    if *current != observation || *previous == section {
                        return false;
                    }
                    *previous = section;
                    true
                });
                let changed = *tx.borrow() != events || section_changed;
                if changed && tx.send(events).is_err() {
                    break;
                }
                tokio::select! {
                    biased;
                    () = tx.closed() => break,
                    changed = lifecycle.changed() => if changed.is_err() { break; },
                    changed = panel_changes.changed() => if changed.is_err() { break; },
                    () = tokio::time::sleep(Duration::from_secs(1)), if panel.is_some() => {},
                }
            }
        });
        rx
    }

    async fn jobs(
        &self,
        kit: &RoomKit<SqliteStore>,
        viewer: &ChannelId,
        current: &SessionConfig,
    ) -> Vec<Job> {
        let mut access = BTreeMap::new();
        let mut jobs = Vec::new();
        for process in self.manager.observe().await {
            // The RoomKit adapters, not the model, construct this owner tuple.
            let Ok((room, agent)) = serde_json::from_str::<(String, String)>(&process.owner) else {
                continue;
            };
            let permission = if let Some(permission) = access.get(&room) {
                *permission
            } else {
                let permission = room_access(kit, viewer, current, &room).await;
                access.insert(room.clone(), permission);
                permission
            };
            if let Some(writable) = permission {
                jobs.push(Job {
                    process,
                    room,
                    agent,
                    writable,
                });
            }
        }
        jobs
    }

    pub async fn handle(
        &self,
        kit: &RoomKit<SqliteStore>,
        viewer: &ChannelId,
        current: &SessionConfig,
        arguments: &str,
    ) -> Result<CommandResult, String> {
        let jobs = self.jobs(kit, viewer, current).await;
        let mut panel = if !arguments.contains(':') {
            let selected = if arguments.is_empty() {
                None
            } else {
                if !jobs
                    .iter()
                    .any(|job| job.process.status.process_id == arguments)
                {
                    return Err("Process is no longer available to this view".into());
                }
                Some(arguments.into())
            };
            Panel {
                id: uuid::Uuid::new_v4().to_string(),
                room: current.room.to_string(),
                agent: current.agent.to_string(),
                selected,
                offsets: None,
            }
        } else {
            let previous = self
                .panel
                .borrow()
                .clone()
                .ok_or("This process panel is no longer open")?;
            let (id, action) = arguments.split_once(':').ok_or("Invalid process action")?;
            if id != previous.id
                || previous.room != current.room.as_str()
                || previous.agent != current.agent.as_str()
            {
                return Err("This process action belongs to an older view".into());
            }
            if action == "dismiss" {
                self.panel.send_replace(None);
                return Ok(CommandResult::Events(Vec::new()));
            }
            let mut panel = previous;
            if action == "list" {
                panel.selected = None;
                panel.offsets = None;
            } else if let Some(id) = action.strip_prefix("open:") {
                if !jobs.iter().any(|j| j.process.status.process_id == id) {
                    return Err("Process is no longer available to this view".into());
                }
                panel.selected = Some(id.into());
                panel.offsets = None;
            } else {
                let job = jobs
                    .iter()
                    .find(|j| Some(&j.process.status.process_id) == panel.selected.as_ref())
                    .ok_or("Process is no longer available to this view")?;
                match action {
                    "stop" | "forget" => {
                        if !job.writable {
                            return Err("This conversation is read-only".into());
                        }
                        if action == "forget" && job.process.status.state != ProcessState::Finished
                        {
                            return Err("Stop the process before forgetting its record".into());
                        }
                        self.manager
                            .stop(
                                &job.process.owner,
                                ProcessStopInput {
                                    process_id: job.process.status.process_id.clone(),
                                    forget: action == "forget",
                                },
                                &CancellationToken::new(),
                            )
                            .await
                            .map_err(|_| "Could not stop or forget this process")?;
                        if action == "forget" {
                            panel.selected = None;
                            panel.offsets = None;
                        }
                    }
                    "tail" => panel.offsets = None,
                    "first" => {
                        panel.offsets = Some((
                            job.process.status.stdout_retained_from,
                            job.process.status.stderr_retained_from,
                        ))
                    }
                    "previous" | "next" => {
                        let (out, err) = offsets(&panel, job);
                        panel.offsets = Some(if action == "previous" {
                            (
                                out.saturating_sub(PAGE)
                                    .max(job.process.status.stdout_retained_from),
                                err.saturating_sub(PAGE)
                                    .max(job.process.status.stderr_retained_from),
                            )
                        } else {
                            (
                                out.saturating_add(PAGE)
                                    .min(job.process.status.stdout_bytes),
                                err.saturating_add(PAGE)
                                    .min(job.process.status.stderr_bytes),
                            )
                        });
                    }
                    _ => return Err("Unknown process action".into()),
                }
            }
            panel
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

    async fn render(&self, panel: &Panel, jobs: &[Job]) -> PanelSpec {
        let mut spec = PanelSpec::new(
            panel.id.clone(),
            "processes",
            "Background processes · current application",
            "",
        )
        .with_dismiss(format!("{}:dismiss", panel.id));
        let action = |value: &str, label: &str, description: &str| {
            Choice::new(format!("{}:{value}", panel.id), label).with_description(description)
        };
        if let Some(id) = &panel.selected {
            spec.actions
                .push(action("list", "Back to process list", ""));
            let Some(job) = jobs.iter().find(|j| &j.process.status.process_id == id) else {
                spec.body = "Process is no longer retained or accessible.".into();
                return spec;
            };
            let p = &job.process;
            spec.title = format!("Process {} · {}", id, state(job));
            spec.body = format!(
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
                spec.body.push_str(&format!("Capture/cleanup: {error}\n"));
            }
            let (stdout, stderr) = offsets(panel, job);
            let read = self
                .manager
                .read(
                    &p.owner,
                    ProcessReadInput {
                        process_id: id.clone(),
                        stdout_offset: stdout,
                        stderr_offset: stderr,
                        max_bytes: NonZeroUsize::new(PAGE as usize),
                    },
                    &CancellationToken::new(),
                )
                .await;
            match read {
                Ok(output) => {
                    spec.body.push_str(if panel.offsets.is_none() {
                        "Following latest output (bounded tail)\n"
                    } else {
                        "Fixed output page\n"
                    });
                    chunk(&mut spec.body, "stdout", &output.data.stdout);
                    chunk(&mut spec.body, "stderr", &output.data.stderr);
                }
                Err(_) => spec.body.push_str("Output is no longer available.\n"),
            }
            if job.writable {
                if p.status.state == ProcessState::Running {
                    spec.actions.push(action(
                        "stop",
                        "Stop process",
                        "Terminate this command and its process group",
                    ));
                } else {
                    spec.actions.push(action(
                        "forget",
                        "Forget completed process",
                        "Remove its retained process record",
                    ));
                }
            }
            for (value, label) in [
                ("first", "First retained output"),
                ("previous", "Previous output page"),
                ("next", "Next output page"),
                ("tail", "Follow latest output"),
            ] {
                spec.actions.push(action(value, label, ""));
            }
        } else {
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
                spec.actions.push(action(
                    &format!("open:{}", p.status.process_id),
                    &format!("{}  {command}", p.status.process_id),
                    &description,
                ));
            }
        }
        spec
    }
}

async fn room_access(
    kit: &RoomKit<SqliteStore>,
    viewer: &ChannelId,
    current: &SessionConfig,
    id: &str,
) -> Option<bool> {
    let id = RoomId::new(id).ok()?;
    let room = kit
        .get_room(
            &id,
            GetRoomOptions {
                organization_id: current.organization.clone(),
            },
        )
        .await
        .ok()?;
    if room.organization_id != current.organization {
        return None;
    }
    let bindings = kit
        .list_bindings(
            &id,
            ListBindingsOptions {
                organization_id: current.organization.clone(),
            },
        )
        .await
        .ok()?;
    bindings
        .into_iter()
        .find_map(|b| (b.channel_id == *viewer).then_some(b.access))
        .and_then(|a| match a {
            Access::ReadWrite => Some(true),
            Access::ReadOnly => Some(false),
            _ => None,
        })
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

// Only running jobs already authorized by jobs() reach the sidebar. Commands
// and owners are clipped before crossing the bounded UI protocol.
fn sidebar(jobs: &[Job]) -> SidebarSection {
    let mut entries = jobs
        .iter()
        .filter(|j| j.process.status.state == ProcessState::Running)
        .take(128)
        .map(|job| {
            SidebarEntry::new(format!(
                "● {}",
                job.process
                    .command
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(60)
                    .collect::<String>()
            ))
            .with_detail(format!(
                "Running · {}",
                job.agent.chars().take(64).collect::<String>()
            ))
            .with_tone(SidebarTone::Accent)
            .with_action(crate::profiles::sidebar::action(
                "processes",
                &job.process.status.process_id,
            ))
        })
        .collect::<Vec<_>>();
    if entries.is_empty() {
        entries.push(SidebarEntry::new("No running processes").with_tone(SidebarTone::Muted));
    }
    SidebarSection::new("Processes", SidebarContent::Entries { entries })
        .with_action(crate::profiles::sidebar::action("processes", ""))
}
