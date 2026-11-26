//! Local model selection, download progress and shared llama.cpp runtimes.
pub(crate) mod cache;
mod downloads;
pub(crate) mod hub;
mod label;
pub(crate) use label::display_name;
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

pub(crate) const DEFAULT_CONTEXT: u32 = 16_384;

type ActiveServer = Arc<tokio::sync::RwLock<Option<Arc<LlamaCpp>>>>;

#[derive(Default)]
pub(crate) struct LocalModels {
    active: ActiveServer,
    stop: tokio_util::sync::CancellationToken,
    servers: Mutex<HashMap<String, Weak<LlamaCpp>>>,
    pub downloads: Arc<downloads::Downloads>,
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
        self.downloads.stop();
    }

    pub async fn close(&self) {
        self.downloads.join().await;
        if let Some(server) = self.active.write().await.take() {
            server.close().await;
        }
    }
}

#[cfg(test)]
tokio::task_local! {
    pub(crate) static TEST_HUB: (url::Url, std::path::PathBuf);
}
