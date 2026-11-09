//! Host-owned command hooks. No scripts are exposed as model tools.
mod catalogue;
mod ui;

use catalogue::{Catalogue, Entry};
use roomkit::ToolCallEvent;
use roomkit_hooks::{Decision, Event as HookEvent, Report, RunStatus};
use roomkit_tui::protocol::{OperationId, Rejection};
use roomkit_tui_engine::{LifecycleFuture, SessionConfig, SessionLifecycle, TurnOutcome};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    /// A delegated task's room: part of its conversation's session.
    task: bool,
    ready: tokio::sync::OnceCell<()>,
}

struct Turn {
    context: Context,
    /// When the turn began, in the order turns begin: 0 until then.
    begun: AtomicU64,
    /// Set once the engine reports the turn finished: its TurnEnd hooks run
    /// to their own budget, even while the application closes.
    ending: AtomicBool,
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
    /// The turns begun so far, which orders them.
    begun: AtomicU64,
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
            begun: AtomicU64::new(0),
            changes: tokio::sync::watch::channel(()).0,
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        crate::sync::lock(&self.state)
    }

    pub fn note_source(&self, room: &str, source: &str) {
        self.state().sources.insert(room.into(), source.into());
    }

    fn session(&self, view: &SessionConfig) -> Arc<Session> {
        let mut state = self.state();
        if let Some(session) = state.sessions.get(view.room.as_str()) {
            *crate::sync::lock(&session.agent) = view.agent.to_string();
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
            task: view.group.is_some(),
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
        if session.task {
            return;
        }
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
        let options = roomkit::HookOptions {
            execution: roomkit::HookExecution::Async,
            name: "code-command-hook-outcomes".into(),
            timeout: crate::timeouts::HOOKS,
            ..roomkit::HookOptions::default()
        };
        kit.hook(
            roomkit::HookTrigger::OnToolCall,
            move |event: ToolCallEvent, _| {
                // The engine calls this observer on a task of its own after it
                // reported the call, and the turn is found by its conversation
                // and agent: the turn's TurnEnd waits for the post-tool hook
                // registered here, but a report that reaches this late, such
                // as a call cut at the very end of an interrupted turn, runs
                // no hook, runs alongside or after its turn's TurnEnd, or
                // runs with that agent's next turn there once it has begun,
                // even before the first turn's TurnEnd. The engine offers no
                // way to wait for its observers of a turn, and its event
                // names no turn.
                let turn = hooks.tool_turn(&event);
                let hooks = hooks.clone();
                let task = turn.map(|turn| {
                    let tracker = turn.posts.clone();
                    tracker.spawn(async move {
                        let mut context = turn.context.clone();
                        tool_payload(&mut context.payload, &event);
                        context.payload["tool_response"] = tool_response(&event);
                        context.payload["roomkit"]["outcome"] = json!(tool_outcome(&event));
                        let name = if event.is_error {
                            HookEvent::PostToolUseFailure
                        } else {
                            HookEvent::PostToolUse
                        };
                        hooks.dispatch(name, &event.name, &context).await;
                    })
                });
                async move {
                    if let Some(task) = task {
                        let _ = task.await;
                    }
                }
            },
            options,
        )?;
        Ok(())
    }

    /// The turn a tool call belongs to: the agent's latest begun turn in the
    /// conversation. An agent's turns there run one after the other, but the
    /// engine may report the previous one finished only after the next one
    /// began, as when a message written during a turn gets its own.
    fn tool_turn(&self, event: &ToolCallEvent) -> Option<Arc<Turn>> {
        let room = event.room_id.as_ref()?.as_str();
        let agent = event.channel_id.as_str();
        self.state()
            .turns
            .iter()
            .filter(|((r, a, _), _)| r == room && a == agent)
            .map(|(_, turn)| (turn.begun.load(Ordering::Acquire), turn))
            .filter(|(begun, _)| *begun > 0)
            .max_by_key(|(begun, _)| *begun)
            .map(|(_, turn)| turn.clone())
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
        self.dispatch(HookEvent::PreToolUse, &event.name, &context)
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
        self.close_within(crate::timeouts::SETTLE).await;
    }

    /// `close`, giving up after `orphaned` on turns the engine never began
    /// to finish.
    async fn close_within(&self, orphaned: Duration) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stopping.cancel();
        let left = self.settle(orphaned).await;
        // Session hooks stop with `stopping`; HOOKS bounds them anyway.
        self.background.close();
        let _ = tokio::time::timeout(crate::timeouts::HOOKS, self.background.wait()).await;
        if left > 0 {
            self.diagnostics
                .record("hooks_close_timeout", json!({ "turns": left }), None);
        }
        let sessions: Vec<_> = self.state().sessions.values().cloned().collect();
        for session in sessions.into_iter().filter(|session| !session.task) {
            let agent = crate::sync::lock(&session.agent).clone();
            let mut context = self.context(&session, &agent, None);
            context.payload["reason"] = json!("shutdown");
            self.dispatch(HookEvent::SessionEnd, "shutdown", &context)
                .await;
        }
    }
}

impl Hooks {
    /// Wait for the turns to end; how many are left. A turn the engine is
    /// finishing runs its TurnEnd hooks, which `stopping` does not cut, so
    /// it gets their whole budget; any other gets only `orphaned`: a turn
    /// the engine accepted but never finished must not hold the exit.
    async fn settle(&self, orphaned: Duration) -> usize {
        let mut changed = self.changes.subscribe();
        let begun = tokio::time::Instant::now();
        loop {
            let (left, ending) = {
                let state = self.state();
                let ending = state
                    .turns
                    .values()
                    .filter(|turn| turn.ending.load(Ordering::Acquire));
                (state.turns.len(), ending.count())
            };
            if left == 0 {
                return 0;
            }
            let limit = if ending > 0 {
                crate::timeouts::HOOKS
            } else {
                orphaned
            };
            let Some(remaining) = limit.checked_sub(begun.elapsed()).filter(|r| !r.is_zero())
            else {
                return left;
            };
            // A change or the limit: either way, look again.
            if let Ok(Err(_)) = tokio::time::timeout(remaining, changed.changed()).await {
                return left;
            }
        }
    }
}

fn tool_payload(payload: &mut Value, event: &ToolCallEvent) {
    payload["tool_name"] = json!(event.name);
    payload["tool_use_id"] = json!(event.tool_call_id);
    payload["tool_input"] = json!(event.arguments);
}

/// What a command hook reads of a call's result: a tool's data and text as
/// one JSON text, as it always has, or the result as the model read it.
fn tool_response(event: &ToolCallEvent) -> Value {
    let read = event.result.clone().unwrap_or(Value::Null);
    let data = event
        .structured_content
        .as_ref()
        .and_then(|copy| copy.get("data"));
    let (Some(data), Some(text)) = (data, read.as_str()) else {
        return read;
    };
    let mut envelope = json!({ "data": data, "text": text });
    // Nested project instructions reach the model as JSON beside the text.
    if let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(text)
        && fields.len() == 2
        && let (Some(text), Some(rules)) =
            (fields.remove("text"), fields.remove("project_instructions"))
    {
        envelope["text"] = text;
        envelope["project_instructions"] = rules;
    }
    Value::String(envelope.to_string())
}

/// How a finished tool call ended, as far as the engine's tool event tells:
/// `served`, `refused`, `cancelled`, or `failed` for any other error. The
/// event carries no outcome of its own, as Python's, so a call a hook
/// withheld or nothing served reads as `failed` too.
pub(crate) fn tool_outcome(event: &ToolCallEvent) -> &'static str {
    if !event.is_error {
        "served"
    } else if event.refused {
        "refused"
    } else if event.cancelled {
        "cancelled"
    } else {
        "failed"
    }
}

/// At most `limit` bytes of `text`, cut at a character boundary, without
/// control characters other than newlines and tabs. Taking `limit`
/// characters first only bounds the copy of a long text: no more can fit.
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
        operation.as_str().to_owned(),
    )
}

impl Lifecycle {
    /// The gate every turn passes, and the turn `started` and `finished` find:
    /// its context, or none for an operation that already ended.
    async fn open_turn(
        &self,
        view: &SessionConfig,
        operation: &OperationId,
    ) -> Result<Option<Context>, Rejection> {
        let hooks = &self.0;
        if hooks.closing.load(Ordering::Acquire) {
            return Err(Rejection::unavailable("Application is closing"));
        }
        let session = hooks.session(view);
        let key = key(view, operation);
        let context = hooks.context(&session, view.agent.as_str(), Some(operation.as_str()));
        {
            let mut state = hooks.state();
            if !state.catalogue.problems.is_empty() {
                return Err(Rejection::refused(
                    "Hook configuration has errors; inspect /hooks and reload after correcting it",
                ));
            }
            if state.completed.contains(&key) {
                return Ok(None);
            }
            state.turns.insert(
                key,
                Arc::new(Turn {
                    context: context.clone(),
                    begun: AtomicU64::new(0),
                    ending: AtomicBool::new(false),
                    posts: TaskTracker::new(),
                }),
            );
        }
        hooks.open_session(&session, view.agent.as_str()).await;
        Ok(Some(context))
    }
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
    ) -> LifecycleFuture<'a, Result<(), Rejection>> {
        Box::pin(async move {
            let Some(mut context) = self.open_turn(view, operation).await? else {
                return Ok(());
            };
            // Images stay in their protected attachment store. No image bytes or
            // storage references are exposed implicitly to command hooks.
            context.payload["prompt"] = json!(prompt_text(content));
            context.payload["roomkit"]["origin"] = json!("user");
            match self
                .0
                .dispatch(HookEvent::UserPromptSubmit, "", &context)
                .await
            {
                Decision::Deny(reason) => Err(Rejection::refused(reason)),
                _ => Ok(()),
            }
        })
    }

    /// A turn with no new words: the same gate and the same turn for its hooks,
    /// and no `UserPromptSubmit`: the person's messages passed it as they came,
    /// and what the agent is told here is not theirs.
    fn admit_answer<'a>(
        &'a self,
        view: &'a SessionConfig,
        operation: &'a OperationId,
        _: &'a roomkit::model::EventContent,
    ) -> LifecycleFuture<'a, Result<(), Rejection>> {
        Box::pin(async move { self.open_turn(view, operation).await.map(|_| ()) })
    }

    fn started<'a>(
        &'a self,
        view: &'a SessionConfig,
        operation: &'a OperationId,
    ) -> LifecycleFuture<'a, ()> {
        Box::pin(async move {
            let turn = self.0.state().turns.get(&key(view, operation)).cloned();
            if let Some(turn) = turn {
                let order = self.0.begun.fetch_add(1, Ordering::AcqRel) + 1;
                turn.begun.store(order, Ordering::Release);
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
                turn.ending.store(true, Ordering::Release);
                self.0.changes.send_replace(());
                // The post-tool hooks registered so far; see `install`.
                turn.posts.close();
                turn.posts.wait().await;
                if turn.begun.load(Ordering::Acquire) > 0 {
                    let mut context = turn.context.clone();
                    context.payload["roomkit"]["outcome"] = json!(match outcome {
                        TurnOutcome::Completed => "completed",
                        TurnOutcome::Failed => "failed",
                        TurnOutcome::Interrupted => "cancelled",
                        TurnOutcome::Blocked => "blocked",
                        // An outcome added by a newer adapter is never reported as success.
                        _ => "failed",
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "Invalid fixtures fail the test"
    )]
    use super::*;
    use clap::Parser;

    fn answered(result: Value, copy: Value) -> ToolCallEvent {
        let mut event = ToolCallEvent::new(
            roomkit::model::ChannelId::new("code").unwrap(),
            roomkit::model::ChannelType::AI,
            "call-1",
            "read",
            Default::default(),
        );
        event.result = Some(result);
        event.structured_content = Some(copy);
        event
    }

    #[test]
    fn a_hook_reads_a_tool_s_data_and_text_as_one_json_text() {
        let read = |response: Value| -> Value {
            serde_json::from_str(response.as_str().unwrap()).unwrap()
        };
        let plain = tool_response(&answered(json!("1: a\n"), json!({"data": {"n": 1}})));
        assert_eq!(read(plain), json!({"data": {"n": 1}, "text": "1: a\n"}));
        let nested = json!({"project_instructions": "rules", "text": "1: a\n"}).to_string();
        let ruled = tool_response(&answered(json!(nested), json!({"data": 1})));
        assert_eq!(
            read(ruled),
            json!({"data": 1, "text": "1: a\n", "project_instructions": "rules"})
        );
    }

    fn hooks(state: &std::path::Path, workspace: &std::path::Path) -> (Arc<Hooks>, PathBuf) {
        let cli = crate::config::Cli::try_parse_from([
            "roomkit-code",
            workspace.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
        ])
        .unwrap();
        let settings = crate::config::Settings::load(&cli).unwrap();
        let path = state.join("diagnostics.jsonl");
        let log = crate::diagnostics::Diagnostics::open(path.clone(), false).unwrap();
        let policy = crate::permissions::Policy::new(crate::permissions::Mode::Ask);
        (Hooks::load(&settings, state, policy, log), path)
    }

    fn turn(hooks: &Hooks, operation: &str, ending: bool) -> Key {
        let key: Key = ("room".into(), "agent".into(), operation.into());
        let turn = Arc::new(Turn {
            context: Context {
                payload: json!({}),
                entries: Vec::new(),
            },
            begun: AtomicU64::new(0),
            ending: AtomicBool::new(ending),
            posts: TaskTracker::new(),
        });
        hooks.state().turns.insert(key.clone(), turn);
        key
    }

    /// Close, failing rather than hanging if closing never returns.
    async fn close(hooks: &Hooks, orphaned: Duration) {
        tokio::time::timeout(Duration::from_secs(5), hooks.close_within(orphaned))
            .await
            .expect("Closing never returned");
    }

    #[tokio::test]
    async fn closing_gives_up_on_a_turn_the_engine_never_finished() {
        let (workspace, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (hooks, path) = hooks(state.path(), workspace.path());
        turn(&hooks, "orphan", false);
        close(&hooks, Duration::from_millis(50)).await;
        let records = std::fs::read_to_string(path).unwrap();
        assert!(
            records.contains("hooks_close_timeout") && records.contains("\"turns\":1"),
            "{records}"
        );
    }

    #[tokio::test]
    async fn closing_waits_for_a_turn_the_engine_is_finishing() {
        let (workspace, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (hooks, path) = hooks(state.path(), workspace.path());
        let key = turn(&hooks, "ending", true);
        // Its TurnEnd hooks outlast the orphan wait, then the turn ends.
        let finisher = hooks.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            finisher.state().turns.remove(&key);
            finisher.changes.send_replace(());
        });
        let started = std::time::Instant::now();
        close(&hooks, Duration::from_millis(50)).await;
        assert!(started.elapsed() >= Duration::from_millis(300));
        let records = std::fs::read_to_string(path).unwrap();
        assert!(!records.contains("hooks_close_timeout"), "{records}");
    }

    /// A message written during a turn may get a turn of its own, which
    /// begins before the engine reports the previous one finished: the
    /// agent's tool calls belong to the turn begun last.
    #[tokio::test]
    async fn a_tool_call_belongs_to_the_agent_s_latest_begun_turn() {
        let (workspace, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (hooks, _) = hooks(state.path(), workspace.path());
        let lifecycle = hooks.lifecycle();
        let (room, agent) = (
            roomkit::model::RoomId::new("room").unwrap(),
            roomkit::model::ChannelId::new("agent").unwrap(),
        );
        let view = SessionConfig::new(room.clone(), agent.clone(), "person");
        let content = roomkit::model::TextContent::new("Go on").into();
        for operation in ["first", "second"] {
            let operation = OperationId::new(operation);
            lifecycle.admit(&view, &operation, &content).await.unwrap();
            lifecycle.started(&view, &operation).await;
        }
        let mut event = ToolCallEvent::new(
            agent,
            roomkit::model::ChannelType::AI,
            "call",
            "read",
            Default::default(),
        );
        event.room_id = Some(room);
        let second = ("room".into(), "agent".into(), "second".into());
        let second = hooks.state().turns.get(&second).cloned().unwrap();
        let found = hooks.tool_turn(&event).unwrap();
        assert!(
            Arc::ptr_eq(&found, &second),
            "The call went to the first turn"
        );
        close(&hooks, Duration::from_millis(50)).await;
    }

    /// The engine's tool event carries markers, no outcome: each marker
    /// names the outcome it stands for, any other error reads `failed`.
    #[test]
    fn a_tool_outcome_is_read_from_the_event_markers() {
        let event = |is_error, refused, cancelled| {
            let mut event = ToolCallEvent::new(
                roomkit::model::ChannelId::new("code").unwrap(),
                roomkit::model::ChannelType::AI,
                "call",
                "read",
                Default::default(),
            );
            (event.is_error, event.refused, event.cancelled) = (is_error, refused, cancelled);
            event
        };
        let outcomes = [
            event(false, false, false),
            event(true, true, false),
            event(true, false, true),
            event(true, false, false),
        ]
        .map(|event| tool_outcome(&event));
        assert_eq!(outcomes, ["served", "refused", "cancelled", "failed"]);
    }
}
