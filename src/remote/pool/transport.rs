//! Pin each accepted remote turn to the worker whose slot was reserved.
use super::{Requirements, Scheduler, lease::Lease};
use roomkit::{
    RoomKitError,
    turns::{
        wire::{Control, Frame},
        *,
    },
};
use std::sync::Arc;
use tokio::time::{Instant, sleep};

type Result<T> = std::result::Result<T, RoomKitError>;

/// How long Redis may fail to answer while a task waits for a slot before
/// the task fails.
const UNREACHABLE_FOR: std::time::Duration = std::time::Duration::from_secs(30);
/// The most one slot claim waits for Redis.
const CLAIM_WITHIN: std::time::Duration = std::time::Duration::from_secs(3);
impl Scheduler {
    #[allow(
        clippy::result_large_err,
        reason = "TurnBackend requires the engine error type"
    )]
    pub(super) async fn schedule(&self, request: TurnRequest) -> Result<Arc<dyn TurnLink>> {
        let permit = self.turns.clone().read_owned().await;
        if self.closed.is_cancelled() {
            return Err(RoomKitError::Runtime("Executor pool changed".into()));
        }
        let req = Requirements::parse(request.context.room.metadata.get("task_context"))
            .map_err(RoomKitError::Value)?;
        let profile = req.profile.clone();
        let ram = req.min_ram_mib;
        let ticket = Arc::new(self.queue.enter(&request, req));
        let (worker, lease, link) = self.submit_reserved(&request, ram, &ticket).await?;
        let placement = worker.placement(profile.as_deref());
        ticket.assign(&placement, &worker.name, &link.request().turn_id);
        if super::queue::delegated_turn(link.request()) {
            self.announce(link.request(), &worker.name, &placement)
                .await;
        }
        Ok(Arc::new(ScheduledLink {
            inner: link,
            lease,
            _permit: permit,
        }))
    }

    #[allow(
        clippy::result_large_err,
        reason = "Preserve the native pre-admission error for the turn backend"
    )]
    async fn submit_reserved(
        &self,
        request: &TurnRequest,
        ram: u64,
        ticket: &Arc<super::queue::Ticket>,
    ) -> Result<(super::Worker, Lease, Arc<dyn TurnLink>)> {
        loop {
            // Each attempt owns its own lease, including while a previous release is in flight.
            let claim = uuid::Uuid::new_v4().to_string();
            let (worker, at) = self
                .reserve(&claim, ram, ticket)
                .await
                .map_err(RoomKitError::Runtime)?;
            let lease = Lease::start(
                self.inventory.registry.clone(),
                &worker.instance,
                claim,
                at,
                ticket.clone(),
            );
            let executor = ExecutorInfo::new(&worker.instance, ExecutorInfoOptions::default());
            let result = tokio::select! {
                biased;
                () = lease.lost.cancelled() => return Err(lost_slot()),
                result = self.inventory.backend.submit_to(request.clone(), executor) => result,
            };
            match result {
                Ok(link) => return Ok((worker, lease, link)),
                Err(error) if refused_before_admission(&error, request.handed()) => {
                    drop(lease);
                    sleep(super::inventory::REFRESH).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Waits for a worker slot and claims it. Redis failing to answer does
    /// not fail the task at once: the wait goes on, as the lease's renewal
    /// does, until Redis has not answered for [`UNREACHABLE_FOR`].
    async fn reserve(
        &self,
        turn: &str,
        ram: u64,
        ticket: &super::queue::Ticket,
    ) -> std::result::Result<(super::Worker, Instant), String> {
        let mut unreachable: Option<Instant> = None;
        loop {
            match self.claim_one(turn, ram, ticket).await {
                Ok(Some(claimed)) => return Ok(claimed),
                Ok(None) => unreachable = None,
                Err(error) => {
                    let since = *unreachable.get_or_insert_with(Instant::now);
                    if since.elapsed() >= UNREACHABLE_FOR {
                        return Err(error);
                    }
                }
            }
            tokio::select! {
                () = self.closed.cancelled() => return Err("Executor pool closed".into()),
                () = sleep(super::inventory::REFRESH) => {}
            }
        }
    }

    /// One pass over the workers that can take the ticket: the first whose
    /// slot Redis grants, or none. The admission lock is held for the pass,
    /// each request of which is bounded: the tasks waiting behind it are
    /// not held back by a Redis that stopped answering.
    async fn claim_one(
        &self,
        turn: &str,
        ram: u64,
        ticket: &super::queue::Ticket,
    ) -> std::result::Result<Option<(super::Worker, Instant)>, String> {
        let registry = &self.inventory.registry;
        let mut guard = self.queue.admission.lock().await;
        let rows = guard.rows(self.inventory.rows()).await?;
        for worker in ticket.candidates(rows) {
            let sent = Instant::now();
            match tokio::time::timeout(CLAIM_WITHIN, registry.claim(worker, turn, ram)).await {
                Ok(Ok(true)) => return Ok(Some((worker.clone(), sent))),
                Ok(Ok(false)) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    // Redis may have granted it without the answer arriving:
                    // give the slot back rather than hold it until it expires.
                    let release = registry.release(&worker.instance, turn);
                    let _ = tokio::time::timeout(CLAIM_WITHIN, release).await;
                    return Err("Redis did not answer a slot claim in time".into());
                }
            }
        }
        Ok(None)
    }

    async fn announce(&self, request: &TurnRequest, worker: &str, placement: &str) {
        let metadata = &request.context.room.metadata;
        if let (Some(tasks), Some(parent), Some(task)) = (
            self.tasks.upgrade(),
            metadata
                .get("parent_room_id")
                .and_then(serde_json::Value::as_str),
            metadata.get("task_id").and_then(serde_json::Value::as_str),
        ) {
            tasks
                .assigned(
                    parent,
                    task,
                    serde_json::json!({"worker":worker, "pool":self.scope}),
                    placement,
                )
                .await;
        }
    }
}
impl Scheduler {
    pub(crate) fn close(&self) -> TurnFuture<'_, Result<()>> {
        self.closed.cancel();
        TurnBackend::close(&self.inventory.backend)
    }
}

fn refused_before_admission(error: &RoomKitError, handed: bool) -> bool {
    !handed && matches!(error, RoomKitError::TurnLink { reason, .. } if reason == "no_executor")
}

fn lost_slot() -> RoomKitError {
    RoomKitError::Runtime("Lost the worker slot reservation".into())
}

struct ScheduledLink {
    _permit: tokio::sync::OwnedRwLockReadGuard<()>,
    inner: Arc<dyn TurnLink>,
    lease: Lease,
}
impl TurnLink for ScheduledLink {
    fn request(&self) -> &TurnRequest {
        self.inner.request()
    }
    fn taken(&self) -> bool {
        self.inner.taken()
    }
    fn receive(&self) -> TurnFuture<'_, Result<(u64, Frame)>> {
        Box::pin(async {
            tokio::select! {
                biased;
                () = self.lease.lost.cancelled() => {
                    // The slot is another task's to claim once it expires:
                    // the worker is told to cut this turn before the link is
                    // let go. Nothing confirms it did when the broker is
                    // the one that stopped answering.
                    let cancel = self.inner.send(Control::Cancel {
                        reason: "slot_lost".into(),
                    });
                    let _ = tokio::time::timeout(CLAIM_WITHIN, cancel).await;
                    self.inner.release();
                    Err(lost_slot())
                }
                result = self.inner.receive() => result
            }
        })
    }
    fn send(&self, m: Control) -> TurnFuture<'_, Result<()>> {
        self.inner.send(m)
    }
    fn release(&self) {
        self.inner.release();
        self.lease.stop.cancel();
    }
    fn beat_heard(&self) -> bool {
        self.inner.beat_heard()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_confirmed_pre_admission_refusal_can_retry() {
        let missing = RoomKitError::TurnLink {
            reason: "no_executor".into(),
            message: None,
        };
        assert!(refused_before_admission(&missing, false));
        assert!(!refused_before_admission(&missing, true));
        assert!(!refused_before_admission(&lost_slot(), false));
    }
}
