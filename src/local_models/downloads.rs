//! Application-owned GGUF transfers shared by model selection and background downloads.
mod view;
use super::{
    cache::Cache,
    hub::{Hub, ModelFile},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

struct Active {
    name: String,
    bytes: Option<(u64, u64)>,
    ticket: uuid::Uuid,
    cancel: CancellationToken,
}

#[derive(Default)]
struct State {
    active: BTreeMap<String, Active>,
    /// Recent outcomes from different background downloads survive coalesced UI updates.
    recent: VecDeque<(String, String, Result<(), String>)>,
}

#[derive(Default)]
pub(crate) struct Downloads {
    state: Mutex<State>,
    pub changes: watch::Sender<()>,
    stop: CancellationToken,
    tasks: TaskTracker,
}

impl Downloads {
    pub fn start(self: &Arc<Self>, cache: Cache, file: ModelFile) -> Result<(), String> {
        let id = file.id();
        let mut state = crate::sync::lock(&self.state);
        if self.stop.is_cancelled() {
            return Err("Model downloads are shutting down".into());
        }
        if state.active.contains_key(&id) {
            return Ok(());
        }
        let hub = Hub::public()?;
        let cancel = CancellationToken::new();
        state.recent.retain(|(previous, _, _)| previous != &id);
        state.active.insert(
            id.clone(),
            Active {
                name: format!(
                    "{} ({})",
                    super::display_name(&id).unwrap_or_else(|| file.name.clone()),
                    file.repo
                ),
                bytes: None,
                ticket: uuid::Uuid::new_v4(),
                cancel: cancel.clone(),
            },
        );
        self.changes.send_replace(());
        let owner = self.clone();
        self.tasks.spawn(async move {
            let outcome = tokio::select! {
                biased;
                () = owner.stop.cancelled() => Err("Model download stopped. Select the file again to resume.".into()),
                outcome = owner.fetch(hub, cache, &file, &id, &cancel) => outcome,
            };
            let mut state = crate::sync::lock(&owner.state);
            if let Some(active) = state.active.remove(&id) {
                state.recent.push_front((id, active.name, outcome));
                state.recent.truncate(8);
            }
            owner.changes.send_replace(());
        });
        Ok(())
    }

    pub fn cancel(&self, ticket: &str) -> Result<(), String> {
        let state = crate::sync::lock(&self.state);
        let ticket = uuid::Uuid::try_parse(ticket).map_err(|_| "Invalid download selection")?;
        let job = state
            .active
            .values()
            .find(|job| job.ticket == ticket)
            .ok_or("This download is no longer running")?;
        job.cancel.cancel();
        Ok(())
    }

    pub fn forget(&self, file: &ModelFile) {
        let removed = file.id();
        crate::sync::lock(&self.state)
            .recent
            .retain(|(id, _, _)| id != &removed);
        self.changes.send_replace(());
    }

    async fn fetch(
        &self,
        hub: Hub,
        cache: Cache,
        file: &ModelFile,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let variants = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err("Download cancelled".into()),
            found = hub.variants(&file.repo, Some(&file.revision)) => found?,
        };
        let model = variants
            .into_iter()
            .find(|v| &v.first == file)
            .ok_or("The chosen GGUF is unavailable")?;
        self.progress(id, 0, model.size);
        let last_percent = std::sync::atomic::AtomicU64::new(0);
        cache
            .download(&hub, &model, cancel, |done| {
                let percent = percentage(done, model.size);
                if last_percent.swap(percent, std::sync::atomic::Ordering::Relaxed) != percent {
                    self.progress(id, done, model.size);
                }
            })
            .await
    }

    fn progress(&self, id: &str, bytes: u64, total: u64) {
        if let Some(job) = crate::sync::lock(&self.state).active.get_mut(id) {
            job.bytes = Some((bytes, total));
            self.changes.send_replace(());
        }
    }

    pub fn stop(&self) {
        // Serialize closing admission with registering a task so wait() joins every worker.
        let _state = crate::sync::lock(&self.state);
        self.stop.cancel();
        self.tasks.close();
    }

    pub async fn join(&self) {
        self.tasks.wait().await;
    }
}

fn percentage(done: u64, total: u64) -> u64 {
    (u128::from(done) * 100 / u128::from(total)) as u64
}
