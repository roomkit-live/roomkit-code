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

type ResultReceiver = watch::Receiver<Option<Result<(), String>>>;

pub(crate) struct Transfer(ResultReceiver);

impl Transfer {
    pub async fn wait(mut self) -> Result<(), String> {
        let result = self
            .0
            .wait_for(Option::is_some)
            .await
            .map_err(|_| "Model download stopped")?;
        result
            .clone()
            .unwrap_or_else(|| Err("Model download stopped".into()))
    }
}

struct Active {
    name: String,
    bytes: Option<(u64, u64)>,
    result: ResultReceiver,
    background: bool,
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
    pub fn start(
        self: &Arc<Self>,
        cache: Cache,
        file: ModelFile,
        background: bool,
    ) -> Result<Transfer, String> {
        let id = file.id();
        let mut state = crate::sync::lock(&self.state);
        if self.stop.is_cancelled() {
            return Err("Model downloads are shutting down".into());
        }
        if let Some(active) = state.active.get_mut(&id) {
            active.background |= background;
            return Ok(Transfer(active.result.clone()));
        }
        let hub = Hub::public()?;
        let (sender, result) = watch::channel(None);
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
                result: result.clone(),
                background,
            },
        );
        self.changes.send_replace(());
        let owner = self.clone();
        self.tasks.spawn(async move {
            let outcome = tokio::select! {
                () = owner.stop.cancelled() => Err("Model download stopped. Select the file again to resume.".into()),
                outcome = owner.fetch(hub, cache, &file, &id) => outcome,
            };
            let mut state = crate::sync::lock(&owner.state);
            if let Some(active) = state.active.remove(&id) && active.background {
                state.recent.push_front((id, active.name, outcome.clone()));
                state.recent.truncate(8);
            }
            sender.send_replace(Some(outcome));
            owner.changes.send_replace(());
        });
        Ok(Transfer(result))
    }

    async fn fetch(
        &self,
        hub: Hub,
        cache: Cache,
        file: &ModelFile,
        id: &str,
    ) -> Result<(), String> {
        let model = hub
            .variants(&file.repo, Some(&file.revision))
            .await?
            .into_iter()
            .find(|v| &v.first == file)
            .ok_or("The chosen GGUF is unavailable")?;
        self.progress(id, 0, model.size);
        let last_percent = std::sync::atomic::AtomicU64::new(0);
        cache
            .download(&hub, &model, |done| {
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
