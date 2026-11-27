//! Local model selection, download progress and shared llama.cpp runtimes.
pub(crate) mod cache;
mod downloads;
pub(crate) mod hub;
mod label;
mod observation;
pub(crate) use label::display_name;
pub(crate) use runtime::resolve;
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

pub(crate) const CONTEXT_HINT: &str = "In /model, raise Agent context capacity";
pub(crate) const DEFAULT_CONTEXT: u32 = 16_384;

pub(crate) fn validate_thinking_budget(
    provider: crate::providers::ProviderKind,
    thinking: Option<u32>,
    output: Option<u32>,
) -> Result<(), String> {
    let Some(thinking) = thinking else {
        return Ok(());
    };
    if provider != crate::providers::ProviderKind::LlamaCpp {
        return Err("Reasoning budget in tokens requires a llama.cpp connection".into());
    }
    let output = output.unwrap_or_else(|| LlamaCppConfig::default().max_tokens);
    if thinking >= output {
        return Err(format!(
            "Reasoning budget must be less than the output budget ({output} tokens)"
        ));
    }
    if thinking > i32::MAX as u32 {
        return Err("Reasoning budget exceeds the llama.cpp token limit".into());
    }
    Ok(())
}

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
        diagnostics: Arc<crate::diagnostics::Diagnostics>,
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
        let report = observation::Reporter::new(
            diagnostics,
            &profile.id,
            config.context_size,
            profile.max_output_tokens.unwrap_or(config.max_tokens),
        );
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
            report,
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
