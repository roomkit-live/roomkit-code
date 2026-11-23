//! Pin each accepted remote turn to the worker whose slot was reserved.
use super::{Requirements, Scheduler, lease::Lease};
use roomkit::{
    RoomKitError,
    turns::{
        wire::{Control, Frame},
        *,
    },
};
use std::{sync::Arc, time::Duration};
use tokio::time::{Instant, sleep};

type Result<T> = std::result::Result<T, RoomKitError>;
impl Scheduler {
    #[allow(
        clippy::result_large_err,
        reason = "TurnBackend requires the engine error type"
    )]
    pub(super) async fn schedule(&self, mut request: TurnRequest) -> Result<Arc<dyn TurnLink>> {
        let permit = self.turns.clone().read_owned().await;
        if self.closed.is_cancelled() {
            return Err(RoomKitError::Runtime("Executor pool changed".into()));
        }
        let req = Requirements::parse(request.context.room.metadata.get("task_context"))
            .map_err(RoomKitError::Value)?;
        let profile = req.profile.clone();
        let ram = req.min_ram_mib;
        let ticket = Arc::new(self.queue.enter(&request, req));
        let (worker, claimed) = self
            .reserve(&request.turn_id, ram, &ticket)
            .await
            .map_err(RoomKitError::Runtime)?;
        let lease = Lease::start(
            self.registry.clone(),
            &worker.instance,
            request.turn_id.clone(),
            claimed,
            ticket.clone(),
        );
        let placement = worker.placement(profile.as_deref());
        request.room_hooks.push(super::route(&worker.instance));
        let link = tokio::select! {
            biased;
            () = lease.lost.cancelled() => return Err(lost_slot()),
            result = self.redis.submit(request) => result?,
        };
        ticket.assign(&placement, &worker.name, &link.request().turn_id);
        self.announce(link.request(), &placement).await;
        Ok(Arc::new(ScheduledLink {
            inner: link,
            lease,
            _permit: permit,
        }))
    }

    async fn reserve(
        &self,
        turn: &str,
        ram: u64,
        ticket: &super::queue::Ticket,
    ) -> std::result::Result<(super::Worker, Instant), String> {
        loop {
            let guard = self.queue.admission.lock().await;
            let rows = self.registry.rows().await?;
            for worker in ticket.candidates(&rows) {
                let sent = Instant::now();
                if self.registry.claim(worker, turn, ram).await? {
                    return Ok((worker.clone(), sent));
                }
            }
            drop(guard);
            tokio::select! {
                () = self.closed.cancelled() => return Err("Executor pool closed".into()),
                () = sleep(Duration::from_millis(350)) => {}
            }
        }
    }

    async fn announce(&self, request: &TurnRequest, placement: &str) {
        let metadata = &request.context.room.metadata;
        if let (Some(tasks), Some(parent), Some(task)) = (
            self.tasks.upgrade(),
            metadata
                .get("parent_room_id")
                .and_then(serde_json::Value::as_str),
            metadata.get("task_id").and_then(serde_json::Value::as_str),
        ) {
            tasks
                .assigned(parent, task, &request.turn_id, placement)
                .await;
        }
    }
}
impl Scheduler {
    pub(crate) fn close(&self) -> TurnFuture<'_, Result<()>> {
        self.closed.cancel();
        TurnBackend::close(&self.redis)
    }
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
