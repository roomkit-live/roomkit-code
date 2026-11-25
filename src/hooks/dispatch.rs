//! Running the hooks of one event, in order, and reading what they decide.
use super::*;

impl Hooks {
    /// Runs `entry`'s command on `payload`, unless a project script it
    /// launches changed since it was reviewed: it then fails as a hook that
    /// could not start, until `/hooks` reviews it again.
    async fn run_reviewed(&self, entry: &catalogue::Entry, payload: &Value) -> Report {
        let started = std::time::Instant::now();
        let reviewed = entry.clone();
        let unchanged = tokio::task::spawn_blocking(move || reviewed.scripts_unchanged())
            .await
            .unwrap_or(false);
        if unchanged {
            return roomkit_hooks::run(&entry.hook, &self.workspace, &self.inherit_env, payload)
                .await;
        }
        Report {
            status: RunStatus::Failed,
            stdout: String::new(),
            stderr: "A project script this hook runs changed since it was reviewed; review it again in /hooks".into(),
            elapsed: started.elapsed(),
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "Debt recorded when the limit was set: split it, never grow it"
    )]
    pub(super) async fn dispatch(
        &self,
        event: HookEvent,
        matcher: &str,
        context: &Context,
    ) -> Decision {
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
                report = self.run_reviewed(entry, &payload) => report,
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
}
