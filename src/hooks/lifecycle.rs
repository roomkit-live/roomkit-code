//! The turns' lifecycle as the terminal's adapter reports it: the events
//! that open, admit and end a turn's hooks.
use super::*;

#[derive(Debug)]
pub(super) struct Lifecycle(pub(super) Arc<Hooks>);

pub(super) fn key(view: &SessionConfig, operation: &OperationId) -> Key {
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
                return Err(Rejection::refused(super::CONFIGURATION_ERRORS));
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

pub(crate) fn prompt_text(content: &roomkit::model::EventContent) -> String {
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
