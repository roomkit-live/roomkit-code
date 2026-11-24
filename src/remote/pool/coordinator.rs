//! Keep the interactive room owner and its local turns alive across pool edits.
use super::Scheduler;
use crate::sync::{read, write};
use roomkit::{InMemoryTurnBackend, RoomKitError, turns::*};
use std::sync::{Arc, OnceLock, RwLock};

type Result<T> = std::result::Result<T, RoomKitError>;

pub(crate) struct Coordinator {
    active: RwLock<Option<Arc<Scheduler>>>,
    local: InMemoryTurnBackend,
    owner: OnceLock<Arc<dyn RoomOwner>>,
}

impl Coordinator {
    pub(crate) fn for_launch(
        settings: &crate::config::Settings,
        tasks: &Arc<crate::delegation::Tasks>,
    ) -> crate::Result<Option<Arc<Self>>> {
        if settings.local_only
            || settings.unattended
            || settings.remote_worker.is_some()
            || settings.serve_pool.is_some()
        {
            return Ok(None);
        }
        let active = settings
            .executor_pool
            .as_deref()
            .map(|group| Scheduler::new(group, settings.executor_url.as_deref(), tasks))
            .transpose()?;
        Ok(Some(Arc::new(Self {
            active: RwLock::new(active),
            local: InMemoryTurnBackend::new(),
            owner: OnceLock::new(),
        })))
    }

    pub(crate) fn active(&self) -> Option<Arc<Scheduler>> {
        read(&self.active).clone()
    }

    pub(crate) fn replace(&self, pool: Option<Arc<Scheduler>>) -> Option<Arc<Scheduler>> {
        std::mem::replace(&mut *write(&self.active), pool)
    }

    #[allow(
        clippy::result_large_err,
        reason = "Forward the error to TurnBackend without wrapping it"
    )]
    async fn attend_pool(&self, pool: &Scheduler) -> Result<()> {
        let owner = self
            .owner
            .get()
            .ok_or_else(|| RoomKitError::Runtime("Missing room owner".into()))?;
        pool.attending
            .get_or_try_init(|| TurnBackend::attend(&pool.inventory.backend, owner.clone()))
            .await?;
        Ok(())
    }
}

impl TurnBackend for Coordinator {
    fn submit(&self, request: TurnRequest) -> TurnFuture<'_, Result<Arc<dyn TurnLink>>> {
        if request.channel_id.as_str() != crate::remote::AGENT {
            return self.local.submit(request);
        }
        let pool = self.active();
        let target = request
            .context
            .room
            .metadata
            .get("task_context")
            .and_then(|c| c.get("executor_target"));
        if let Some(target) = target
            && !pool.as_ref().is_some_and(|p| p.accepts_target(target))
        {
            return Box::pin(async {
                Err(RoomKitError::Value(
                    "The selected executor pool or worker is unavailable".into(),
                ))
            });
        }
        if let Some(pool) = pool {
            return Box::pin(async move {
                self.attend_pool(&pool).await?;
                pool.schedule(request).await
            });
        }
        self.local.submit(request)
    }

    fn control(&self, message: ChannelControl) -> TurnFuture<'_, Result<Option<ChannelResult>>> {
        use ChannelControl::*;
        let channel = match &message {
            ChannelCall(call) => &call.channel_id,
            ChannelReleased { channel_id }
            | RoomAttached { channel_id, .. }
            | RoomDetached { channel_id, .. }
            | CommittedEvent { channel_id, .. } => channel_id,
        };
        if channel.as_str() == crate::remote::AGENT
            && let Some(pool) = self.active()
        {
            return Box::pin(async move {
                self.attend_pool(&pool).await?;
                pool.inventory.backend.control(message).await
            });
        }
        self.local.control(message)
    }

    fn serve(&self, e: Arc<dyn ServedExecutor>) -> TurnFuture<'_, Result<()>> {
        self.local.serve(e)
    }

    fn attend(&self, owner: Arc<dyn RoomOwner>) -> TurnFuture<'_, Result<()>> {
        let _ = self.owner.set(owner.clone());
        self.local.attend(owner)
    }

    fn claim_presence<'a>(
        &'a self,
        channel: &'a str,
        room: &'a str,
        online: Option<bool>,
    ) -> Option<TurnFuture<'a, Result<bool>>> {
        if channel == crate::remote::AGENT
            && let Some(pool) = self.active()
        {
            return Some(Box::pin(async move {
                pool.inventory
                    .backend
                    .claim_presence(channel, room, online)
                    .await
            }));
        }
        Some(self.local.claim_presence(channel, room, online))
    }

    fn executors(&self) -> Option<TurnFuture<'_, Result<Vec<ExecutorInfo>>>> {
        // Only the queue is declared remotely; it also accepts work with no workers online.
        let active = self.active().is_some();
        Some(Box::pin(async move {
            Ok(if active {
                vec![ExecutorInfo::new(
                    "code-executor-queue",
                    ExecutorInfoOptions {
                        hosts: Some([crate::remote::AGENT.into()].into()),
                        ..Default::default()
                    },
                )]
            } else {
                vec![]
            })
        }))
    }

    fn close(&self) -> TurnFuture<'_, Result<()>> {
        Box::pin(async move {
            let remote = if let Some(pool) = self.replace(None) {
                pool.close().await
            } else {
                Ok(())
            };
            self.local.close().await?;
            remote
        })
    }
}
