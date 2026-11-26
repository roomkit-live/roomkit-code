//! Local model selection, download progress and shared llama.cpp runtimes.
pub(crate) mod cache;
pub(crate) mod hub;
mod provider;
mod runtime;
mod transfer;
pub(crate) use provider::Local;

use crate::profiles::model::{Agent, Provider};
use roomkit_ai::AiProvider;
use roomkit_providers_llamacpp::{LlamaCpp, LlamaCppConfig};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;

pub(crate) const DEFAULT_CONTEXT: u32 = 16_384;

type ActiveServer = Arc<tokio::sync::RwLock<Option<Arc<LlamaCpp>>>>;

#[derive(Default)]
pub(crate) struct LocalModels {
    active: ActiveServer,
    stop: tokio_util::sync::CancellationToken,
    servers: Mutex<HashMap<String, Weak<LlamaCpp>>>,
    pub changes: watch::Sender<HashMap<String, String>>,
}

impl LocalModels {
    pub fn provider(
        &self,
        profile: &Agent,
        connection: &Provider,
        workspace: &Path,
    ) -> Result<Local, String> {
        let model = runtime::resolve(&profile.model, workspace)?;
        let mut config = LlamaCppConfig::new(model);
        let (binary, managed) = runtime::binary(&profile.model)?;
        config.binary = binary.clone();
        let fork = binary.filter(|_| managed);
        config.context_size = profile
            .context_window
            .map(u32::try_from)
            .transpose()
            .map_err(|_| "Invalid context size")?
            .unwrap_or(DEFAULT_CONTEXT);
        config.timeout = std::time::Duration::from_secs(connection.timeout_seconds);
        let key = serde_json::to_string(&config).map_err(|e| e.to_string())?;
        let mut servers = crate::sync::lock(&self.servers);
        servers.retain(|_, server| server.strong_count() > 0);
        let server = match servers.get(&key).and_then(Weak::upgrade) {
            Some(server) => server,
            None => {
                let server = Arc::new(LlamaCpp::new(config).map_err(|e| e.to_string())?);
                servers.insert(key, Arc::downgrade(&server));
                server
            }
        };
        Ok(Local {
            active: self.active.clone(),
            max_tokens: profile.max_output_tokens,
            model: profile.model.clone(),
            stop: self.stop.clone(),
            inner: server,
            fork,
        })
    }

    pub fn cancel_preparation(&self) {
        self.stop.cancel();
    }

    pub async fn close(&self) {
        if let Some(server) = self.active.write().await.take() {
            server.close().await;
        }
    }

    pub fn progress<'a>(&'a self, room: &'a str) -> Progress<'a> {
        Progress { models: self, room }
    }
}

pub(crate) struct Progress<'a> {
    models: &'a LocalModels,
    room: &'a str,
}

impl Progress<'_> {
    pub fn show(&self, note: String) {
        self.models.changes.send_if_modified(|all| {
            if all.get(self.room) == Some(&note) {
                return false;
            }
            all.insert(self.room.into(), note);
            true
        });
    }
}

impl Drop for Progress<'_> {
    fn drop(&mut self) {
        self.models
            .changes
            .send_if_modified(|all| all.remove(self.room).is_some());
    }
}

#[cfg(test)]
tokio::task_local! {
    pub(crate) static TEST_HUB: (url::Url, std::path::PathBuf);
}
