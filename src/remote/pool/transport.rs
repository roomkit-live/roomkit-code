//! Pin each accepted remote turn to the worker whose slot was reserved.
use super::{
    Requirements, Scheduler,
    queue::Ticket,
    registry::{Registry, Worker},
};
use roomkit::{
    RoomKitError,
    turns::{
        wire::{Control, Frame},
        *,
    },
};
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, RoomKitError>;
impl Scheduler {
    #[allow(
        clippy::result_large_err,
        reason = "TurnBackend requires the engine error type"
    )]
    async fn schedule(&self, mut request: TurnRequest) -> Result<Arc<dyn TurnLink>> {
        let req = Requirements::parse(request.context.room.metadata.get("task_context"))
            .map_err(RoomKitError::Value)?;
        let ram = req.min_ram_mib;
        let profile = req.profile.clone();
        let ticket = self.queue.enter(&request, req);
        let (worker, placement) = loop {
            let guard = self.queue.admission.lock().await;
            let rows = self.registry.rows().await.map_err(RoomKitError::Runtime)?;
            let mut found = None;
            for w in ticket.candidates(&rows) {
                if self
                    .registry
                    .claim(w, &request.turn_id, ram)
                    .await
                    .map_err(RoomKitError::Runtime)?
                {
                    let placement = w.placement(profile.as_deref());
                    ticket.assign(&placement);
                    found = Some((w.clone(), placement));
                    break;
                }
            }
            drop(guard);
            if let Some(w) = found {
                break w;
            }
            tokio::select! {()=self.closed.cancelled()=>return Err(RoomKitError::Runtime("Executor pool closed".into())),()=sleep(Duration::from_millis(350))=>{}}
        };
        let lease = Lease::start(
            self.registry.clone(),
            &worker,
            request.turn_id.clone(),
            ticket,
        );
        request.room_hooks.push(super::route(&worker.instance));
        let link = self.redis.submit(request).await?;
        self.announce(link.request(), &placement).await;
        Ok(Arc::new(ScheduledLink { inner: link, lease }))
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
impl TurnBackend for Scheduler {
    fn submit(&self, r: TurnRequest) -> TurnFuture<'_, Result<Arc<dyn TurnLink>>> {
        if r.channel_id.as_str() == super::super::AGENT {
            Box::pin(self.schedule(r))
        } else {
            TurnBackend::submit(&self.redis, r)
        }
    }
    fn serve(&self, e: Arc<dyn ServedExecutor>) -> TurnFuture<'_, Result<()>> {
        TurnBackend::serve(&self.redis, e)
    }
    fn attend(&self, o: Arc<dyn RoomOwner>) -> TurnFuture<'_, Result<()>> {
        TurnBackend::attend(&self.redis, o)
    }
    fn control(&self, m: ChannelControl) -> TurnFuture<'_, Result<Option<ChannelResult>>> {
        TurnBackend::control(&self.redis, m)
    }
    fn claim_presence<'a>(
        &'a self,
        c: &'a str,
        r: &'a str,
        o: Option<bool>,
    ) -> Option<TurnFuture<'a, Result<bool>>> {
        TurnBackend::claim_presence(&self.redis, c, r, o)
    }
    fn executors(&self) -> Option<TurnFuture<'_, Result<Vec<ExecutorInfo>>>> {
        Some(Box::pin(async move {
            let mut info = self.redis.executors().await?;
            // The queue accepts requests even when its last worker goes offline.
            info.push(ExecutorInfo::new(
                "code-executor-queue",
                ExecutorInfoOptions {
                    hosts: Some([super::super::AGENT.into()].into()),
                    ..Default::default()
                },
            ));
            Ok(info)
        }))
    }
    fn timeouts(&self) -> Option<TurnTimeouts> {
        Some(self.redis.timeouts())
    }
    fn close(&self) -> TurnFuture<'_, Result<()>> {
        self.closed.cancel();
        Box::pin(self.redis.close())
    }
}

struct Lease {
    stop: CancellationToken,
    lost: CancellationToken,
}
impl Lease {
    fn start(reg: Registry, w: &Worker, turn: String, ticket: Ticket) -> Self {
        let stop = CancellationToken::new();
        let lost = CancellationToken::new();
        let done = stop.clone();
        let expired = lost.clone();
        let instance = w.instance.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {biased; ()=done.cancelled()=>break,()=sleep(Duration::from_secs(3))=>{}}
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(6), reg.renew(&instance, &turn)).await,
                    Ok(Ok(true))
                ) {
                    expired.cancel();
                    break;
                }
            }
            reg.release(&instance, &turn).await;
            drop(ticket);
        });
        Self { stop, lost }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
struct ScheduledLink {
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
                ()=self.lease.lost.cancelled()=>{self.inner.release();Err(RoomKitError::Runtime("Lost the worker slot reservation".into()))}
                result=self.inner.receive()=>result
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
