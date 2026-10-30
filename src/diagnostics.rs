//! Private, bounded operational records projected from RoomKit's audit hooks.
//! Never serialize whole framework events, tool payloads or provider errors.

use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use roomkit::{FrameworkEvent, HookTrigger, RoomKit};
use roomkit_cli_roomkit::{CommandResult, Diagnostic, SessionConfig};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{Choice, Event, PanelSpec};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const LIMIT: u64 = 1024 * 1024;

pub(crate) struct Diagnostics {
    path: PathBuf,
    debug: bool,
    state: Mutex<State>,
    stop: CancellationToken,
    listener: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

struct State {
    file: File,
    bytes: u64,
    recent: VecDeque<String>,
    write_failed: bool,
}

impl Diagnostics {
    pub fn open(path: PathBuf, debug: bool) -> std::io::Result<Arc<Self>> {
        let file = private_file(&path)?;
        let bytes = file.metadata()?.len();
        let diagnostics = Arc::new(Self {
            path,
            debug,
            state: Mutex::new(State {
                file,
                bytes,
                recent: VecDeque::new(),
                write_failed: false,
            }),
            stop: CancellationToken::new(),
            listener: tokio::sync::Mutex::new(None),
        });
        diagnostics.record("application_started", json!({"debug":debug}), None);
        Ok(diagnostics)
    }

    pub fn record(&self, kind: &'static str, mut fields: Value, error: Option<&str>) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        fields["timestamp_ms"] = json!(now);
        fields["event"] = json!(kind);
        if let Some(error) = error {
            let diagnostic = Diagnostic::classify(error);
            fields["code"] = json!(diagnostic.code);
            fields["summary"] = json!(diagnostic.summary);
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let line = format!("{fields}\n");
        if state.recent.len() == 100 {
            state.recent.pop_front();
        }
        state.recent.push_back(line.clone());
        let written = (|| -> std::io::Result<()> {
            if state.bytes.saturating_add(line.len() as u64) > LIMIT {
                // One current and one previous file, both private. The project
                // storage lease prevents another application rotating this log.
                std::fs::rename(&self.path, self.path.with_extension("previous.jsonl"))?;
                state.file = private_file(&self.path)?;
                state.bytes = 0;
            }
            state.file.write_all(line.as_bytes())?;
            state.bytes += line.len() as u64;
            Ok(())
        })();
        state.write_failed = written.is_err();
    }

    pub async fn install(self: &Arc<Self>, kit: &RoomKit<SqliteStore>) -> crate::Result<()> {
        let log = self.clone();
        kit.observe(
            "code-diagnostics-errors",
            HookTrigger::OnError,
            move |event, _| {
                let log = log.clone();
                async move {
                    log.record("engine_error", json!({
                    "room":event.room_id.as_str(),
                    "channel":event.source.channel_id.as_str(),
                    "responds_to":event.responds_to.as_ref().map(|id| id.as_str()),
                    "stage":match event.metadata.get("error_category").and_then(Value::as_str) {
                        Some("streaming") => "streaming",
                        Some("generation") => "generation",
                        _ => "engine",
                    },
                }), Some(event.metadata.get("error").and_then(Value::as_str).unwrap_or("unknown")));
                }
            },
        )?;
        let log = self.clone();
        kit.observe_tool_calls("code-diagnostics-tools", move |event, _| {
            let log = log.clone();
            async move {
                // Tool names come from this application's known inventory;
                // call IDs, arguments, results and error bodies are not logged.
                let name = if crate::profiles::model::TOOLS.contains(&event.call.name.as_str()) {
                    event.call.name.as_str()
                } else {
                    "unknown"
                };
                log.record(
                    "tool_finished",
                    json!({
                        "room":event.room_id.as_str(), "channel":event.channel_id.as_str(),
                        "tool":name, "outcome":event.outcome,
                    }),
                    event.error_detail.as_deref(),
                );
            }
        })?;
        let mut events = kit.subscribe();
        let log = self.clone();
        *self.listener.lock().await = Some(tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    event = events.recv() => match event {
                        Ok(event) => log.framework(event),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) =>
                            log.record("audit_events_skipped", json!({"count":count}), None),
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    () = log.stop.cancelled() => break,
                }
            }
        }));
        Ok(())
    }

    fn framework(&self, event: FrameworkEvent) {
        match event {
            FrameworkEvent::DeliveryFailed {
                room_id,
                event_id,
                channel_id,
                error,
                ..
            } => self.record(
                "delivery_failed",
                json!({"room":room_id,"event_id":event_id,"channel":channel_id}),
                Some(&error),
            ),
            FrameworkEvent::HookError {
                room_id,
                event_id,
                error,
                ..
            } => self.record(
                "hook_failed",
                json!({"room":room_id,"event_id":event_id}),
                Some(&error),
            ),
            FrameworkEvent::ProcessTimeout {
                room_id,
                event_id,
                timeout,
                ..
            } => self.record(
                "processing_timeout",
                json!({"room":room_id,"event_id":event_id,"timeout_seconds":timeout}),
                Some("process_timeout"),
            ),
            FrameworkEvent::HookTimeout {
                room_id, timeout, ..
            } => self.record(
                "hook_timeout",
                json!({"room":room_id,"timeout_seconds":timeout}),
                None,
            ),
            FrameworkEvent::EventProcessed {
                room_id, event_id, ..
            } if self.debug => self.record(
                "event_processed",
                json!({"room":room_id,"event_id":event_id}),
                None,
            ),
            FrameworkEvent::DeliverySucceeded {
                room_id,
                event_id,
                channel_id,
                ..
            } if self.debug => self.record(
                "delivery_succeeded",
                json!({"room":room_id,"event_id":event_id,"channel":channel_id}),
                None,
            ),
            FrameworkEvent::RoomCreated { room_id, .. } if self.debug => {
                self.record("room_created", json!({"room":room_id}), None)
            }
            _ => {}
        }
    }

    pub async fn close(&self) {
        self.stop.cancel();
        if let Some(listener) = self.listener.lock().await.take() {
            let _ = listener.await;
        }
        self.record("application_stopped", json!({}), None);
    }

    pub fn panel(&self, current: &SessionConfig, action: &str) -> CommandResult {
        if action == "dismiss" {
            return CommandResult::Events(vec![]);
        }
        let (recent, failed) = self
            .state
            .lock()
            .map(|state| {
                (
                    state
                        .recent
                        .iter()
                        .rev()
                        .take(20)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n"),
                    state.write_failed,
                )
            })
            .unwrap_or_else(|_| (String::new(), true));
        CommandResult::Events(vec![Event::Panel {
            conversation: current.room.as_str().into(),
            panel: PanelSpec {
                id: uuid::Uuid::new_v4().to_string(),
                command: "diagnostics".into(),
                title: "Diagnostics · current application".into(),
                body: format!(
                    "Log: {}\nDebug: {}\nStorage: {}\n\nErrors and tool outcomes are recorded automatically. Use --debug for additional framework events.\nNo prompts, credentials, tool arguments/results or raw provider errors are recorded.\nTwo files, at most 1 MiB each. Recent records, newest first:\n\n{recent}",
                    self.path.display(),
                    self.debug,
                    if failed {
                        "WRITE FAILED · recent records are only in memory"
                    } else {
                        "available"
                    }
                ),
                actions: vec![Choice {
                    value: "refresh".into(),
                    label: "Refresh".into(),
                    description: "Reload recent diagnostics".into(),
                }],
                dismiss: "dismiss".into(),
            },
        }])
    }
}

fn private_file(path: &std::path::Path) -> std::io::Result<File> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(std::io::Error::other(
            "Diagnostic log must be a regular file",
        ));
    }
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        let file = options.open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
    #[cfg(not(unix))]
    options.open(path)
}

#[cfg(test)]
mod tests;
