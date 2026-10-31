//! Host-owned command hooks. No scripts are exposed as model tools.
mod catalogue;
mod ui;

use catalogue::{Catalogue, Entry};
use roomkit::ToolCallEvent;
use roomkit_cli_roomkit::{LifecycleFuture, SessionConfig, SessionLifecycle, TurnOutcome};
use roomkit_hooks::{Decision, Event as HookEvent, Report, RunStatus};
use roomkit_tui::protocol::OperationId;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

type Key = (String, String, String);

#[derive(Clone)]
struct Context {
    payload: Value,
    entries: Vec<Entry>,
}

struct Session {
    execution: String,
    room: String,
    agent: Mutex<String>,
    source: String,
    ready: tokio::sync::OnceCell<()>,
}

struct Turn {
    context: Context,
    begun: AtomicBool,
    posts: TaskTracker,
}

#[derive(Clone)]
struct Receipt {
    id: String,
    hook: String,
    label: String,
    event: HookEvent,
    room: String,
    turn: Option<String>,
    agent: String,
    status: String,
    elapsed_ms: u128,
    detail: String,
}

// Dropping an in-flight hook (for example Esc during PreToolUse) still settles
// its activity and audit receipt. The subprocess future owns process cleanup.
struct RunGuard<'a> {
    hooks: &'a Hooks,
    id: &'a str,
    payload: &'a Value,
    event: HookEvent,
    started: std::time::Instant,
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.hooks.state();
        let Some(receipt) = state
            .recent
            .iter_mut()
            .find(|r| r.id == self.id && r.status == "running")
        else {
            return;
        };
        receipt.status = "cancelled".into();
        receipt.elapsed_ms = self.started.elapsed().as_millis();
        receipt.detail = "Interrupted before the command completed".into();
        self.hooks.diagnostics.record(
            "command_hook_finished",
            json!({
                "hook":receipt.hook, "hook_event":self.event.name(), "session":receipt.room,
                "execution_id":self.payload["roomkit"]["execution_id"], "turn":receipt.turn,
                "agent":receipt.agent, "tool_call_id":self.payload.get("tool_use_id"),
                "status":"cancelled", "duration_ms":receipt.elapsed_ms,
            }),
            None,
        );
        drop(state);
        self.hooks.changes.send_replace(());
    }
}

struct State {
    catalogue: Catalogue,
    sessions: BTreeMap<String, Arc<Session>>,
    sources: BTreeMap<String, String>,
    turns: BTreeMap<Key, Arc<Turn>>,
    completed: BTreeSet<Key>,
    recent: VecDeque<Receipt>,
    panel: Option<ui::Panel>,
}

pub(crate) struct Hooks {
    workspace: PathBuf,
    policy: crate::permissions::Policy,
    diagnostics: Arc<crate::diagnostics::Diagnostics>,
    state: Mutex<State>,
    background: TaskTracker,
    stopping: CancellationToken,
    closing: AtomicBool,
    pub changes: tokio::sync::watch::Sender<()>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}

impl Hooks {
    pub fn load(
        settings: &crate::config::Settings,
        private: &std::path::Path,
        policy: crate::permissions::Policy,
        diagnostics: Arc<crate::diagnostics::Diagnostics>,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspace: settings.workspace.clone(),
            policy,
            diagnostics,
            state: Mutex::new(State {
                catalogue: Catalogue::load(&settings.workspace, &settings.state_dir, private),
                sessions: BTreeMap::new(),
                sources: BTreeMap::new(),
                turns: BTreeMap::new(),
                completed: BTreeSet::new(),
                recent: VecDeque::new(),
                panel: None,
            }),
            background: TaskTracker::new(),
            stopping: CancellationToken::new(),
            closing: AtomicBool::new(false),
            changes: tokio::sync::watch::channel(()).0,
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn note_source(&self, room: &str, source: &str) {
        self.state().sources.insert(room.into(), source.into());
    }

    fn session(&self, view: &SessionConfig) -> Arc<Session> {
        let mut state = self.state();
        if let Some(session) = state.sessions.get(view.room.as_str()) {
            *session
                .agent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = view.agent.to_string();
            return session.clone();
        }
        let source = state
            .sources
            .remove(view.room.as_str())
            .unwrap_or_else(|| "resume".into());
        let session = Arc::new(Session {
            room: view.room.to_string(),
            execution: uuid::Uuid::new_v4().to_string(),
            source,
            agent: Mutex::new(view.agent.to_string()),
            ready: tokio::sync::OnceCell::new(),
        });
        state.sessions.insert(session.room.clone(), session.clone());
        session
    }

    fn context(&self, session: &Session, agent: &str, turn: Option<&str>) -> Context {
        let mut payload = json!({
            "session_id": session.room, "cwd": self.workspace,
            "permission_mode": self.policy.state(&session.room).selected,
            "roomkit": {"version":1, "execution_id":session.execution, "agent_id":agent, "origin":"agent"},
        });
        if let Some(turn) = turn {
            payload["roomkit"]["turn_id"] = json!(turn);
        }
        Context {
            payload,
            entries: self.state().catalogue.selected(),
        }
    }

    async fn open_session(&self, session: &Session, agent: &str) {
        session
            .ready
            .get_or_init(|| async {
                let mut context = self.context(session, agent, None);
                context.payload["source"] = json!(session.source);
                self.dispatch(HookEvent::SessionStart, &session.source, &context)
                    .await;
            })
            .await;
    }

    pub fn lifecycle(self: &Arc<Self>) -> Arc<dyn SessionLifecycle> {
        Arc::new(Lifecycle(self.clone()))
    }

    pub fn install(
        self: &Arc<Self>,
        kit: &roomkit::RoomKit<roomkit_sqlite::SqliteStore>,
    ) -> crate::Result<()> {
        let hooks = self.clone();
        kit.observe_tool_calls(
            roomkit::ToolHookSpec::new("code-command-hook-outcomes")
                .with_timeout(Duration::from_secs(125)),
            move |event, _| {
                // Capture correlation and register the task immediately, while the
                // engine reports the call, not later after another turn has started.
                let turn = hooks.tool_turn(&event);
                let hooks = hooks.clone();
                let task = turn.map(|turn| {
                    let tracker = turn.posts.clone();
                    tracker.spawn(async move {
                        let mut context = turn.context.clone();
                        tool_payload(&mut context.payload, &event);
                        context.payload["tool_response"] = event.result.clone();
                        context.payload["roomkit"]["outcome"] = json!(event.outcome);
                        let name = if event.is_error() {
                            HookEvent::PostToolUseFailure
                        } else {
                            HookEvent::PostToolUse
                        };
                        hooks.dispatch(name, &event.call.name, &context).await;
                    })
                });
                async move {
                    if let Some(task) = task {
                        let _ = task.await;
                    }
                }
            },
        )?;
        Ok(())
    }

    fn tool_turn(&self, event: &ToolCallEvent) -> Option<Arc<Turn>> {
        self.state()
            .turns
            .iter()
            .find_map(|((room, agent, _), turn)| {
                (room == event.room_id.as_str()
                    && agent == event.channel_id.as_str()
                    && turn.begun.load(Ordering::Acquire))
                .then(|| turn.clone())
            })
    }

    pub async fn before_tool(&self, event: &ToolCallEvent) -> Decision {
        let Some(turn) = self.tool_turn(event) else {
            // Headless callers must install the same lifecycle if configured
            // controls are enabled. Never silently bypass their checks.
            return if self
                .state()
                .catalogue
                .selected()
                .iter()
                .any(|e| e.event == HookEvent::PreToolUse)
            {
                Decision::Deny("Configured hooks require an application turn lifecycle".into())
            } else {
                Decision::Continue
            };
        };
        let mut context = turn.context.clone();
        tool_payload(&mut context.payload, event);
        self.dispatch(HookEvent::PreToolUse, &event.call.name, &context)
            .await
    }

    async fn dispatch(&self, event: HookEvent, matcher: &str, context: &Context) -> Decision {
        let room = context.payload["session_id"].as_str().unwrap_or_default();
        let mut decision = Decision::Continue;
        for entry in &context.entries {
            if entry.event != event || !entry.group.matches(matcher) {
                continue;
            }
            let mode = self.policy.mode(room);
            if mode == crate::permissions::Mode::Plan && !entry.hook.roomkit.run_in_plan {
                continue;
            }
            let mut payload = context.payload.clone();
            payload["hook_event_name"] = json!(event.name());
            payload["permission_mode"] = json!(self.policy.state(room).selected);
            payload["roomkit"]["hook_id"] = json!(entry.id);
            let id = uuid::Uuid::new_v4().to_string();
            let receipt = Receipt {
                id: id.clone(),
                hook: entry.id.clone(),
                label: entry.label().into(),
                event,
                room: room.into(),
                agent: payload["roomkit"]["agent_id"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                turn: payload["roomkit"]["turn_id"].as_str().map(str::to_owned),
                status: "running".into(),
                elapsed_ms: 0,
                detail: String::new(),
            };
            {
                let mut state = self.state();
                if state.recent.len() == 100 {
                    state.recent.pop_front();
                }
                state.recent.push_back(receipt);
            }
            self.changes.send_replace(());
            let start = std::time::Instant::now();
            let _guard = RunGuard {
                hooks: self,
                id: &id,
                payload: &payload,
                event,
                started: start,
            };
            let report = tokio::select! {
                report = roomkit_hooks::run(&entry.hook, &self.workspace, &payload) => report,
                () = self.stopping.cancelled(), if !matches!(event, HookEvent::SessionEnd | HookEvent::TurnEnd) => Report {
                    status: RunStatus::Cancelled, stdout: String::new(), stderr: String::new(), elapsed: start.elapsed(),
                },
            };
            let interpreted = roomkit_hooks::interpret(event, &report);
            let status = match &interpreted {
                Ok(Decision::Continue) => "completed",
                Ok(Decision::Deny(_)) => "blocked",
                Ok(Decision::Ask(_)) => "approval required",
                Err(_) if report.status == RunStatus::Cancelled => "cancelled",
                Err(_) => "failed",
            };
            let error = interpreted.as_ref().err().cloned().unwrap_or_default();
            self.diagnostics.record("command_hook_finished", json!({
                "hook":entry.id, "hook_event":event.name(), "session":room,
                "execution_id":payload["roomkit"]["execution_id"], "turn":payload["roomkit"]["turn_id"],
                "agent":payload["roomkit"]["agent_id"], "tool_call_id":payload.get("tool_use_id"),
                "status":status, "duration_ms":report.elapsed.as_millis(),
            }), None);
            if let Some(receipt) = self.state().recent.iter_mut().find(|r| r.id == id) {
                receipt.status = status.into();
                receipt.elapsed_ms = report.elapsed.as_millis();
                receipt.detail = format!(
                    "{}\n{error}\n\nstdout:\n{}\n\nstderr:\n{}",
                    report.status.label(),
                    report.stdout,
                    report.stderr
                );
            }
            self.changes.send_replace(());
            match interpreted {
                Ok(Decision::Deny(reason)) if event.blocking() => {
                    return Decision::Deny(format!(
                        "{}: {}",
                        entry.label(),
                        bounded(&reason, 2048)
                    ));
                }
                Ok(Decision::Ask(reason)) if event == HookEvent::PreToolUse => {
                    decision =
                        Decision::Ask(format!("{}: {}", entry.label(), bounded(&reason, 2048)))
                }
                Err(_) if event.blocking() => {
                    return Decision::Deny(format!(
                        "{} failed; inspect /hooks before retrying",
                        entry.label()
                    ));
                }
                _ => {}
            }
        }
        decision
    }

    pub async fn close(&self) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stopping.cancel();
        let mut changed = self.changes.subscribe();
        while !self.state().turns.is_empty() {
            if changed.changed().await.is_err() {
                break;
            }
        }
        self.background.close();
        self.background.wait().await;
        let sessions: Vec<_> = self.state().sessions.values().cloned().collect();
        for session in sessions {
            let agent = session
                .agent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let mut context = self.context(&session, &agent, None);
            context.payload["reason"] = json!("shutdown");
            self.dispatch(HookEvent::SessionEnd, "shutdown", &context)
                .await;
        }
    }
}

fn tool_payload(payload: &mut Value, event: &ToolCallEvent) {
    payload["tool_name"] = json!(event.call.name);
    payload["tool_use_id"] = json!(event.call.id);
    payload["tool_input"] = json!(event.call.arguments);
}

fn bounded(text: &str, limit: usize) -> String {
    let mut text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(limit)
        .collect();
    text.truncate(text.floor_char_boundary(limit.min(text.len())));
    text
}

#[derive(Debug)]
struct Lifecycle(Arc<Hooks>);

fn key(view: &SessionConfig, operation: &OperationId) -> Key {
    (
        view.room.to_string(),
        view.agent.to_string(),
        operation.0.clone(),
    )
}

impl SessionLifecycle for Lifecycle {
    fn attached(&self, view: &SessionConfig) {
        if self.0.closing.load(Ordering::Acquire) {
            return;
        }
        let session = self.0.session(view);
        let agent = view.agent.to_string();
        let hooks = self.0.clone();
        self.0.background.spawn(async move {
            hooks.open_session(&session, &agent).await;
        });
    }

    fn admit<'a>(
        &'a self,
        view: &'a SessionConfig,
        operation: &'a OperationId,
        content: &'a roomkit::model::EventContent,
    ) -> LifecycleFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let hooks = &self.0;
            if hooks.closing.load(Ordering::Acquire) {
                return Err("Application is closing".into());
            }
            let session = hooks.session(view);
            let key = key(view, operation);
            let context = hooks.context(&session, view.agent.as_str(), Some(&operation.0));
            {
                let mut state = hooks.state();
                if !state.catalogue.problems.is_empty() {
                    return Err("Hook configuration has errors; inspect /hooks and reload after correcting it".into());
                }
                if state.completed.contains(&key) {
                    return Ok(());
                }
                state.turns.insert(
                    key,
                    Arc::new(Turn {
                        context: context.clone(),
                        begun: AtomicBool::new(false),
                        posts: TaskTracker::new(),
                    }),
                );
            }
            hooks.open_session(&session, view.agent.as_str()).await;
            let mut context = context;
            // Images stay in their protected attachment store. No image bytes or
            // storage references are exposed implicitly to command hooks.
            context.payload["prompt"] = json!(prompt_text(content));
            context.payload["roomkit"]["origin"] = json!("user");
            match hooks
                .dispatch(HookEvent::UserPromptSubmit, "", &context)
                .await
            {
                Decision::Deny(reason) => Err(reason),
                _ => Ok(()),
            }
        })
    }

    fn started<'a>(
        &'a self,
        view: &'a SessionConfig,
        operation: &'a OperationId,
    ) -> LifecycleFuture<'a, ()> {
        Box::pin(async move {
            let turn = self.0.state().turns.get(&key(view, operation)).cloned();
            if let Some(turn) = turn {
                turn.begun.store(true, Ordering::Release);
                self.0
                    .dispatch(HookEvent::TurnStart, "", &turn.context)
                    .await;
            }
        })
    }

    fn finished<'a>(
        &'a self,
        view: &'a SessionConfig,
        operation: &'a OperationId,
        outcome: TurnOutcome,
    ) -> LifecycleFuture<'a, ()> {
        Box::pin(async move {
            let key = key(view, operation);
            let turn = self.0.state().turns.get(&key).cloned();
            if let Some(turn) = turn {
                turn.posts.close();
                turn.posts.wait().await;
                if turn.begun.load(Ordering::Acquire) {
                    let mut context = turn.context.clone();
                    context.payload["roomkit"]["outcome"] = json!(match outcome {
                        TurnOutcome::Completed => "completed",
                        TurnOutcome::Failed => "failed",
                        TurnOutcome::Interrupted => "cancelled",
                        TurnOutcome::Blocked => "blocked",
                    });
                    self.0.dispatch(HookEvent::TurnEnd, "", &context).await;
                }
                let mut state = self.0.state();
                state.turns.remove(&key);
                // Avoid reexecuting hooks for a transport retry in this runtime.
                state.completed.insert(key);
            }
            self.0.changes.send_replace(());
        })
    }
}

fn prompt_text(content: &roomkit::model::EventContent) -> String {
    use roomkit::model::EventContent;
    match content {
        EventContent::Text(text) => text.body.clone(),
        EventContent::Rich(text) => text.body.clone(),
        EventContent::Composite(parts) => parts
            .parts()
            .iter()
            .map(prompt_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
