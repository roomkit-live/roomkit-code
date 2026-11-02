//! Private, bounded operational records projected from RoomKit's audit hooks.
//! Never serialize whole framework events, tool payloads or provider errors.

use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::AtomicUsize},
    time::{SystemTime, UNIX_EPOCH},
};

use roomkit::{FrameworkEvent, HookExecution, HookOptions, HookTrigger, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{Choice, Event, PanelSpec};
use roomkit_tui_engine::{CommandResult, Diagnostic, SessionConfig};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const LIMIT: u64 = 1024 * 1024;

pub(crate) struct Diagnostics {
    path: PathBuf,
    instance: String,
    debug: bool,
    state: Mutex<State>,
    /// Cancelled once the application stopped: framework events are no
    /// longer recorded.
    stop: CancellationToken,
    /// Panics in this run, recorded or not.
    panics: AtomicUsize,
}

struct State {
    recent: VecDeque<String>,
    write_failed: bool,
}

impl Diagnostics {
    pub fn open(path: PathBuf, debug: bool) -> std::io::Result<Arc<Self>> {
        private_file(&path)?;
        let diagnostics = Arc::new(Self {
            path,
            instance: uuid::Uuid::new_v4().to_string(),
            debug,
            state: Mutex::new(State {
                recent: VecDeque::new(),
                write_failed: false,
            }),
            stop: CancellationToken::new(),
            panics: AtomicUsize::new(0),
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
        fields["instance"] = json!(self.instance);
        if let Some(error) = error {
            let diagnostic = Diagnostic::classify(error);
            fields["code"] = json!(diagnostic.code);
            fields["summary"] = json!(diagnostic.summary);
        }
        let (mut state, _recording) = self.locked();
        let line = format!("{fields}\n");
        if state.recent.len() == 100 {
            state.recent.pop_front();
        }
        state.recent.push_back(line.clone());
        let written = (|| -> std::io::Result<()> {
            let _lease = crate::storage::FileLease::exclusive_wait(crate::storage::private_file(
                &self.path.with_extension("lock"),
            )?)?;
            // Always reopen under the short log lock: another instance may have
            // rotated the file since our previous record.
            let mut file = private_file(&self.path)?;
            if file.metadata()?.len().saturating_add(line.len() as u64) > LIMIT {
                std::fs::rename(&self.path, self.path.with_extension("previous.jsonl"))?;
                file = private_file(&self.path)?;
            }
            file.write_all(line.as_bytes())?;
            Ok(())
        })();
        state.write_failed = written.is_err();
    }

    pub async fn install(self: &Arc<Self>, kit: &RoomKit<SqliteStore>) -> crate::Result<()> {
        let log = self.clone();
        kit.hook(
            HookTrigger::OnError,
            move |event: roomkit::model::RoomEvent, _| {
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
            observer("code-diagnostics-errors"),
        )?;
        let log = self.clone();
        kit.hook(
            HookTrigger::OnToolCall,
            move |event: roomkit::ToolCallEvent, _| {
                let log = log.clone();
                async move {
                    // Tool names come from this application's known inventory;
                    // call IDs, arguments, results and error bodies are not logged.
                    let name = if crate::profiles::model::TOOLS.contains(&event.name.as_str()) {
                        event.name.as_str()
                    } else {
                        "unknown"
                    };
                    log.record(
                        "tool_finished",
                        json!({
                            "room":event.room_id.as_ref().map(|id| id.as_str()),
                            "channel":event.channel_id.as_str(),
                            "tool":name, "outcome":crate::hooks::tool_outcome(&event),
                        }),
                        event.error_detail.as_deref(),
                    );
                }
            },
            observer("code-diagnostics-tools"),
        )?;
        let mut types = vec![
            "delivery_failed",
            "hook_error",
            "process_timeout",
            "hook_timeout",
        ];
        if self.debug {
            types.extend(["event_processed", "delivery_succeeded", "room_created"]);
        }
        for r#type in types {
            let log = self.clone();
            kit.on(r#type, move |event| {
                if !log.stop.is_cancelled() {
                    log.framework(&event);
                }
                async {}
            })?;
        }
        Ok(())
    }

    fn framework(&self, event: &FrameworkEvent) {
        let room = event.room_id.as_ref().map(|id| id.as_str());
        let event_id = event.event_id.as_ref().map(|id| id.as_str());
        let channel = event.channel_id.as_ref().map(|id| id.as_str());
        let data = |key: &str| event.data.get(key).cloned().unwrap_or(Value::Null);
        let text = |key: &str| event.data.get(key).and_then(Value::as_str);
        match event.r#type.as_str() {
            "delivery_failed" => self.record(
                "delivery_failed",
                json!({"room":room,"event_id":event_id,"channel":channel}),
                text("error"),
            ),
            "hook_error" => self.record(
                "hook_failed",
                json!({"room":room,"event_id":event_id}),
                text("error"),
            ),
            "process_timeout" => self.record(
                "processing_timeout",
                json!({"room":room,"event_id":event_id,"timeout_seconds":data("timeout")}),
                Some("process_timeout"),
            ),
            "hook_timeout" => self.record(
                "hook_timeout",
                json!({"room":room,"timeout_seconds":data("timeout")}),
                None,
            ),
            "event_processed" => self.record(
                "event_processed",
                json!({"room":room,"event_id":event_id}),
                None,
            ),
            "delivery_succeeded" => self.record(
                "delivery_succeeded",
                json!({"room":room,"event_id":event_id,"channel":channel}),
                None,
            ),
            "room_created" => self.record("room_created", json!({"room":room}), None),
            _ => {}
        }
    }

    pub async fn close(&self) {
        self.stop.cancel();
        self.record("application_stopped", json!({}), None);
    }

    /// The log's state, with this thread marked as holding it: a panic while
    /// it is held is counted, not recorded, rather than waiting for it.
    fn locked(&self) -> (std::sync::MutexGuard<'_, State>, panics::Recording) {
        let recording = panics::Recording::start();
        (crate::sync::lock(&self.state), recording)
    }

    pub fn panel(&self, current: &SessionConfig, action: &str) -> CommandResult {
        if action == "dismiss" {
            return CommandResult::Events(vec![]);
        }
        let (recent, failed) = {
            let (state, _recording) = self.locked();
            let recent = state.recent.iter().rev().take(20).cloned();
            (recent.collect::<Vec<_>>().join("\n"), state.write_failed)
        };
        CommandResult::Events(vec![Event::Panel {
            conversation: current.room.as_str().into(),
            panel: PanelSpec::new(
                uuid::Uuid::new_v4().to_string(),
                "diagnostics",
                "Diagnostics · current application",
                format!(
                    "Log: {}\nDebug: {}\nStorage: {}\n\nErrors and tool outcomes are recorded automatically. Use --debug for additional framework events.\nNo prompts, credentials, tool arguments/results or raw provider errors are recorded.\nTwo files, at most 1 MiB each. Recent records, newest first:\n\n{recent}",
                    self.path.display(),
                    self.debug,
                    if failed {
                        "WRITE FAILED · recent records are only in memory"
                    } else {
                        "available"
                    }
                ),
            )
            .with_actions(vec![
                Choice::new("refresh", "Refresh").with_description("Reload recent diagnostics"),
            ])
            .with_dismiss("dismiss"),
        }])
    }
}

/// An observer named `name`: it reads what the engine reports and decides
/// nothing.
fn observer(name: &str) -> HookOptions {
    HookOptions {
        execution: HookExecution::Async,
        name: name.into(),
        ..HookOptions::default()
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

mod panics;

pub(crate) use panics::report as report_panics;

#[cfg(test)]
mod tests;
