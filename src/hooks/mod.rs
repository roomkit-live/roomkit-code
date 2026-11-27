//! Host-owned command hooks. No scripts are exposed as model tools.
mod catalogue;
mod dispatch;
mod lifecycle;
mod payload;
mod remote;
mod ui;

use catalogue::{Catalogue, Entry};
use lifecycle::Lifecycle;
pub(crate) use payload::tool_outcome;
use payload::*;
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
    worker_reports: BTreeMap<String, remote::Pending>,
    panel: Option<ui::Panel>,
}

pub(crate) struct Hooks {
    workspace: PathBuf,
    /// The host variables a hook command starts with: those of the agent's
    /// shell.
    inherit_env: Vec<String>,
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
            inherit_env: crate::command_env::command_env(&settings.inherit_env).inherit_env,
            policy,
            diagnostics,
            state: Mutex::new(State {
                catalogue: Catalogue::load(&settings.workspace, &settings.state_dir, private),
                sessions: BTreeMap::new(),
                sources: BTreeMap::new(),
                turns: BTreeMap::new(),
                completed: BTreeSet::new(),
                recent: VecDeque::new(),
                worker_reports: BTreeMap::new(),
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
        kit: &roomkit::RoomKit<roomkit_providers_sqlite::SqliteStore>,
    ) -> crate::Result<()> {
        let hooks = self.clone();
        let options = roomkit::turns::HookOptions {
            execution: roomkit::HookExecution::Async,
            name: "code-command-hook-outcomes".into(),
            timeout: crate::timeouts::HOOKS,
            ..roomkit::turns::HookOptions::default()
        };
        kit.turn_executor().hook(
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
                        hooks.post_tool(&event, turn.context.clone()).await;
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
        self.pre_tool(event, turn.context.clone()).await
    }

    async fn pre_tool(&self, event: &ToolCallEvent, mut context: Context) -> Decision {
        tool_payload(&mut context.payload, event);
        self.dispatch(HookEvent::PreToolUse, &event.name, &context)
            .await
    }

    async fn post_tool(&self, event: &ToolCallEvent, mut context: Context) {
        tool_payload(&mut context.payload, event);
        context.payload["tool_response"] = tool_response(event);
        context.payload["roomkit"]["outcome"] = json!(tool_outcome(event));
        let kind = if event.is_error {
            HookEvent::PostToolUseFailure
        } else {
            HookEvent::PostToolUse
        };
        self.dispatch(kind, &event.name, &context).await;
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

#[cfg(test)]
mod tests;
