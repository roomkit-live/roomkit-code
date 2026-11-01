//! Explicit user commands. They share process supervision with agent tools but
//! never invoke a model or pass through model tool permissions.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use roomkit::model::{
    Access, ChannelId, ChannelType, EventId, EventSource, ParticipantId, RoomEvent, TextContent,
    Timestamp,
};
use roomkit::{
    CommitEventOptions, ConversationStore, ListBindingsOptions, RoomKit, UpdateEventOptions,
};
use roomkit_sqlite::SqliteStore;
use roomkit_tools_process::{
    ObservedProcess, ProcessChunk, ProcessEnd, ProcessManager, ProcessReadInput, ProcessReadResult,
    ProcessStartInput, ProcessState, ProcessStopInput, ProcessWriteInput,
};
use roomkit_tui::protocol::{Command, Event, Item, Message, Request, RunOutcome};
use roomkit_tui_engine::{CommandExecution, CommandResult, ExecutionContext, SessionConfig};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::diagnostics::Diagnostics;

const OUTPUT_BYTES: u64 = 4096;

pub(crate) struct Shell {
    manager: ProcessManager,
    diagnostics: Arc<Diagnostics>,
    shutdown: CancellationToken,
    slot: Arc<Semaphore>,
    /// Manual commands' process IDs, oldest first: the first finished one is
    /// reclaimed when a conversation keeps too many.
    started: Mutex<VecDeque<String>>,
}

impl Shell {
    pub fn new(manager: ProcessManager, diagnostics: Arc<Diagnostics>) -> Arc<Self> {
        Arc::new(Self {
            manager,
            diagnostics,
            shutdown: CancellationToken::new(),
            slot: Arc::new(Semaphore::new(1)),
            started: Mutex::new(VecDeque::new()),
        })
    }

    pub async fn close(&self) {
        self.shutdown.cancel();
        // The permit is reserved before the task is scheduled, and released
        // only after process cleanup and the final conversation record.
        let _joined = self.slot.acquire().await;
    }

    pub async fn prepare(
        self: &Arc<Self>,
        kit: &RoomKit<SqliteStore>,
        channel: &ChannelId,
        view: &SessionConfig,
        request: Request,
    ) -> Result<CommandResult, String> {
        let Command::Invoke {
            arguments: command, ..
        } = request.command
        else {
            return Err("Expected a shell command".into());
        };
        if command.trim().is_empty() || command.contains('\0') || command.len() > 16 * 1024 {
            return Err("Enter a shell command of at most 16 KiB, without NUL".into());
        }
        if self.shutdown.is_cancelled() {
            return Err("The application is closing".into());
        }
        let bindings = kit
            .list_bindings(
                &view.room,
                ListBindingsOptions {
                    organization_id: view.organization.clone(),
                },
            )
            .await
            .map_err(|_| "Could not authorize this shell command")?;
        if !bindings
            .iter()
            .any(|b| &b.channel_id == channel && b.access == Access::ReadWrite)
        {
            return Err("This attachment cannot execute shell commands".into());
        }
        let identity = serde_json::to_vec(&(
            view.room.as_str(),
            channel.as_str(),
            &view.sender,
            request.operation.as_str(),
        ))
        .map_err(|_| "Invalid operation identity")?;
        let id = EventId::new(format!("shell-{:x}", Sha256::digest(identity)))
            .map_err(|_| "Invalid operation identity")?;
        if let Some(row) = kit
            .store()
            .get_event(&id)
            .await
            .map_err(|_| "Could not inspect previous execution")?
        {
            if row.room_id != view.room
                || row.metadata.get("shell_command").and_then(|v| v.as_str()) != Some(&command)
            {
                return Err(
                    "This operation identity was already used with a different command".into(),
                );
            }
            return Ok(CommandResult::Events(vec![Event::Item {
                conversation: view.room.as_str().into(),
                item: item(&row),
            }]));
        }
        let permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| "A shell command is already running")?;
        let mut source = EventSource::new(channel.clone(), ChannelType::WEBSOCKET);
        source.participant_id =
            Some(ParticipantId::new(&view.sender).map_err(|_| "Invalid sender")?);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid clock")?
            .as_millis();
        let timestamp =
            Timestamp::from_unix_millis(i64::try_from(millis).map_err(|_| "Invalid clock")?)
                .map_err(|_| "Invalid clock")?;
        let mut row = RoomEvent::new(
            view.room.clone(),
            source,
            TextContent::new(format!(
                "User shell command\n\n{}\nExecution requested; no final result recorded. Do not assume success or rerun automatically.",
                fenced(&command, "sh")
            )),
        );
        row.id = id;
        row.created_at = timestamp;
        row.metadata.insert("shell_command".into(), json!(command));
        row.metadata.insert("shell_state".into(), json!("pending"));
        let row = kit
            .commit_event(
                &view.room,
                row,
                CommitEventOptions {
                    organization_id: view.organization.clone(),
                },
            )
            .await
            .map_err(|_| "Could not save the command; nothing was executed")?;
        let owner = serde_json::to_string(&(view.room.as_str(), format!("user:{}", view.sender)))
            .map_err(|_| "Invalid process owner")?;
        let shell = self.clone();
        let kit = kit.clone();
        let organization = view.organization.clone();
        Ok(CommandResult::Execute(CommandExecution::new(
            row.id.as_str().into(),
            move |context| async move {
                let _permit = permit;
                shell
                    .execute(kit, organization, row, command, owner, context)
                    .await
            },
        )))
    }

    async fn execute(
        &self,
        kit: RoomKit<SqliteStore>,
        organization: Option<String>,
        mut row: RoomEvent,
        command: String,
        owner: String,
        mut context: ExecutionContext,
    ) -> Result<RunOutcome, String> {
        let started = Instant::now();
        self.diagnostics.record(
            "shell_started",
            json!({"room":row.room_id,"event":row.id}),
            None,
        );
        let result = self.run(&owner, &command, &row, &mut context).await;
        let (text, outcome) = match result {
            Ok(read) => {
                let outcome = match read.status.end {
                    Some(ProcessEnd::Stopped) => RunOutcome::Interrupted,
                    Some(ProcessEnd::Exited) if read.status.exit_code == Some(0) => {
                        RunOutcome::Completed
                    }
                    _ => RunOutcome::Failed,
                };
                (format_output(&command, &read, started.elapsed()), outcome)
            }
            Err(error) => (
                format!("User shell command\n\n{}\n{error}", fenced(&command, "sh")),
                RunOutcome::Failed,
            ),
        };
        row.content = TextContent::new(text).into();
        row.metadata.insert("shell_state".into(), json!(outcome));
        let updated = kit
            .update_event(
                &row.room_id,
                &row.id,
                UpdateEventOptions {
                    content: Some(row.content.clone()),
                    metadata: Some(row.metadata.clone()),
                    organization_id: organization,
                    ..Default::default()
                },
            )
            .await;
        self.diagnostics.record("shell_finished", json!({"room":row.room_id,"event":row.id,"outcome":outcome,"duration_ms":started.elapsed().as_millis()}), None);
        context.publish(item(&row));
        if !matches!(updated, Ok(Some(_))) {
            self.diagnostics.record(
                "shell_history_failed",
                json!({"event":row.id}),
                Some("Could not save final shell output"),
            );
            return Err(
                "Command ended, but its result could not be saved; do not retry automatically"
                    .into(),
            );
        }
        Ok(outcome)
    }

    /// The finished manual command `owner` started first. The processes'
    /// elapsed time is their runtime, frozen at the end, not their age, so
    /// the order is the one this shell started them in.
    fn oldest_finished<'a>(
        &self,
        jobs: &'a [ObservedProcess],
        owner: &str,
    ) -> Option<&'a ObservedProcess> {
        let started = crate::sync::lock(&self.started);
        started.iter().find_map(|id| {
            jobs.iter().find(|job| {
                job.status.process_id == *id
                    && job.owner == owner
                    && job.status.state == ProcessState::Finished
            })
        })
    }

    async fn run(
        &self,
        owner: &str,
        command: &str,
        row: &RoomEvent,
        context: &mut ExecutionContext,
    ) -> Result<ProcessReadResult, &'static str> {
        let cleanup = CancellationToken::new();
        // Reclaim only old completed manual commands in this conversation. Model
        // jobs and live processes remain under their owner's explicit control.
        let jobs = self.manager.observe().await;
        // Forget the IDs of processes the manager no longer keeps.
        crate::sync::lock(&self.started)
            .retain(|id| jobs.iter().any(|job| job.status.process_id == *id));
        if jobs.iter().filter(|job| job.owner == owner).count() >= 8
            && let Some(oldest) = self.oldest_finished(&jobs, owner)
        {
            let _ = self
                .manager
                .stop(
                    owner,
                    ProcessStopInput {
                        process_id: oldest.status.process_id.clone(),
                        forget: true,
                    },
                    &cleanup,
                )
                .await;
        }
        let process = self.manager.start(owner, ProcessStartInput::new(command), &self.shutdown).await
            .map_err(|_| "Could not start command. Inspect /processes for retained jobs or /diagnostics for application failures.")?.data;
        let id = process.process_id;
        crate::sync::lock(&self.started).push_back(id.clone());
        // These are noninteractive commands. EOF avoids programs waiting forever
        // for input; terminal applications and prompts need a separate PTY.
        let _ = self
            .manager
            .write(
                owner,
                ProcessWriteInput {
                    process_id: id.clone(),
                    text: String::new(),
                    close_stdin: true,
                },
                &cleanup,
            )
            .await;
        let mut ticker = tokio::time::interval(Duration::from_millis(200));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let started = Instant::now();
        let mut stopping = false;
        let mut last = String::new();
        loop {
            tokio::select! {
                _ = context.cancelled(), if !stopping => {
                    stopping = true;
                    let _ = self.manager.stop(owner, ProcessStopInput { process_id: id.clone(), forget: false }, &cleanup).await;
                }
                _ = self.shutdown.cancelled(), if !stopping => {
                    stopping = true;
                    let _ = self.manager.stop(owner, ProcessStopInput { process_id: id.clone(), forget: false }, &cleanup).await;
                }
                _ = ticker.tick() => {}
            }
            let read = match self
                .manager
                .read(owner, ProcessReadInput::new(&id), &cleanup)
                .await
            {
                Ok(output) => output.data,
                Err(_) => {
                    let _ = self
                        .manager
                        .stop(
                            owner,
                            ProcessStopInput {
                                process_id: id.clone(),
                                forget: false,
                            },
                            &cleanup,
                        )
                        .await;
                    return Err(
                        "Command output became unavailable; inspect /processes before retrying.",
                    );
                }
            };
            if read.status.state == ProcessState::Finished {
                return Ok(read);
            }
            let text = format_output(command, &read, started.elapsed());
            if text != last {
                let mut live = row.clone();
                live.content = TextContent::new(text.clone()).into();
                context.publish(item(&live));
                last = text;
            }
        }
    }
}

fn item(row: &RoomEvent) -> Item {
    let actor = format!(
        "person:{}",
        row.source
            .participant_id
            .as_ref()
            .map_or("", |id| id.as_str())
    );
    Item::Message(Message::new(
        row.id.as_str(),
        roomkit_tui::protocol::ActorId::new(actor),
        roomkit::text_of(&row.content),
    ))
}

fn fenced(text: &str, language: &str) -> String {
    let longest = text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{language}\n{text}\n{fence}\n")
}

fn format_output(command: &str, read: &ProcessReadResult, elapsed: Duration) -> String {
    let status = &read.status;
    let state = match (status.state, status.end) {
        (ProcessState::Running, _) => "Running",
        (_, Some(ProcessEnd::Exited)) => "Exited",
        (_, Some(ProcessEnd::Stopped)) => "Interrupted",
        (_, Some(ProcessEnd::TimedOut)) => "Timed out",
        _ => "Failed",
    };
    let mut text = format!(
        "User shell command\n\n{}\n{state} · {}s · {}",
        fenced(command, "sh"),
        elapsed.as_secs(),
        status.process_id
    );
    if let Some(code) = status.exit_code {
        text.push_str(&format!(" · exit {code}"));
    }
    if let Some(signal) = status.signal {
        text.push_str(&format!(" · signal {signal}"));
    }
    text.push('\n');
    for (name, chunk) in [("stdout", &read.stdout), ("stderr", &read.stderr)] {
        append_chunk(&mut text, name, chunk, &status.process_id);
    }
    if status.stdout_bytes == 0 && status.stderr_bytes == 0 {
        text.push_str("\nNo output.\n");
    }
    if let Some(error) = &status.error {
        text.push_str(&format!("\nCapture/cleanup: {error}\n"));
    }
    text
}

fn append_chunk(text: &mut String, name: &str, chunk: &ProcessChunk, process_id: &str) {
    if chunk.total_bytes == 0 {
        return;
    }
    text.push_str(&format!("\n**{name}**\n\n{}", fenced(&chunk.text, "text")));
    if chunk.omitted_bytes > 0 || chunk.has_more || chunk.total_bytes > OUTPUT_BYTES {
        text.push_str(&format!("\nOutput shortened: showing bytes {}–{} of {}. Inspect /processes {process_id} while this application is open.\n", chunk.offset, chunk.next_offset, chunk.total_bytes));
    }
}
