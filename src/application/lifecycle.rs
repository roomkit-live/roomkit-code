//! Observe interactive turns for compaction and automatic conversation titles.
use std::sync::Arc;

use roomkit::model::EventContent;
use roomkit_tui::protocol::{OperationId, Rejection};
use roomkit_tui_engine::{LifecycleFuture, SessionConfig, SessionLifecycle, TurnOutcome};

use super::Application;
use crate::sessions::titles::TitleJobs;

pub(super) fn for_app(app: &Application) -> Arc<dyn SessionLifecycle> {
    Arc::new(Watched {
        inner: app.services.hooks.lifecycle(),
        compactions: app.profiles.resources.compactions.clone(),
        titles: TitleJobs::new(
            app.services.titles.clone(),
            app.kit.clone(),
            app.profiles.clone(),
            app.services.sessions.clone(),
        ),
    })
}

struct Watched {
    inner: Arc<dyn SessionLifecycle>,
    compactions: Arc<crate::compaction::Compactions>,
    titles: TitleJobs,
}

impl std::fmt::Debug for Watched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watched")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl SessionLifecycle for Watched {
    fn attached(&self, session: &SessionConfig) {
        self.inner.attached(session);
    }

    fn admit<'a>(
        &'a self,
        session: &'a SessionConfig,
        operation: &'a OperationId,
        content: &'a EventContent,
    ) -> LifecycleFuture<'a, Result<(), Rejection>> {
        self.compactions.turn_admitted(session.room.as_str());
        Box::pin(async move {
            self.inner.admit(session, operation, content).await?;
            self.titles.admitted(session, operation, content);
            Ok(())
        })
    }

    fn admit_answer<'a>(
        &'a self,
        session: &'a SessionConfig,
        operation: &'a OperationId,
        content: &'a EventContent,
    ) -> LifecycleFuture<'a, Result<(), Rejection>> {
        self.compactions.turn_admitted(session.room.as_str());
        self.inner.admit_answer(session, operation, content)
    }

    fn started<'a>(
        &'a self,
        session: &'a SessionConfig,
        operation: &'a OperationId,
    ) -> LifecycleFuture<'a, ()> {
        self.inner.started(session, operation)
    }

    fn finished<'a>(
        &'a self,
        session: &'a SessionConfig,
        operation: &'a OperationId,
        outcome: TurnOutcome,
    ) -> LifecycleFuture<'a, ()> {
        Box::pin(async move {
            self.inner.finished(session, operation, outcome).await;
            // The turn tells what the conversation takes: what a compaction
            // estimated before it is no longer its views' to show.
            self.compactions
                .turn_told(&session.room, &session.agent)
                .await;
            self.compactions.turn_finished(session.room.as_str());
            self.titles.finished(session, operation, outcome).await;
        })
    }
}
