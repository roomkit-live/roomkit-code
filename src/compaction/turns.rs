//! The turns under way in each room, which a `/compact` asked meanwhile waits
//! for: the view's lifecycle, told as each turn is admitted and finishes,
//! whatever the application's own lifecycle does with them.

use std::sync::Arc;

use roomkit::model::EventContent;
use roomkit_tui::protocol::{OperationId, Rejection};
use roomkit_tui_engine::{LifecycleFuture, SessionConfig, SessionLifecycle, TurnOutcome};

use super::Compactions;

/// `inner`, telling `compactions` when a turn is admitted and finishes: the
/// engine finishes every turn it admits, refused or interrupted ones
/// included.
pub(crate) fn lifecycle(
    inner: Arc<dyn SessionLifecycle>,
    compactions: Arc<Compactions>,
) -> Arc<dyn SessionLifecycle> {
    Arc::new(Watched { inner, compactions })
}

struct Watched {
    inner: Arc<dyn SessionLifecycle>,
    compactions: Arc<Compactions>,
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
        self.inner.admit(session, operation, content)
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
        })
    }
}
