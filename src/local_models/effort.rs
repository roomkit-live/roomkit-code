//! Native effort controls advertised by the managed model's chat template.
use std::{sync::Arc, time::Duration};

use serde::Deserialize;

use super::Local;

pub(crate) enum EffortLevels {
    NotLoaded,
    Unreported,
    BudgetOnly,
    Native(Vec<String>),
}

#[derive(Deserialize)]
struct Properties {
    chat_template_caps: Template,
}

#[derive(Deserialize)]
struct Template {
    supports_reasoning_effort: bool,
    #[serde(default)]
    reasoning_effort_levels: Vec<String>,
}

impl Local {
    /// Opening a menu never loads weights. Only explicit discovery can switch servers.
    pub(crate) async fn effort_levels(&self, load: bool) -> Result<EffortLevels, String> {
        let _active = if load {
            let guard = self.acquire().await.map_err(|e| e.to_string())?;
            tokio::select! {
                result = self.inner.start() => result.map_err(|e| e.to_string())?,
                () = self.stop.cancelled() => return Err("Local inference is shutting down".into()),
            }
            guard
        } else {
            let Ok(guard) = self.active.clone().try_read_owned() else {
                return Ok(EffortLevels::NotLoaded);
            };
            if !guard
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &self.inner))
            {
                return Ok(EffortLevels::NotLoaded);
            }
            guard
        };
        let base = self.inner.base_url();
        let url = format!("{}/props", base.trim_end_matches("/v1"));
        Ok(reported_levels(&url)
            .await
            .unwrap_or(EffortLevels::Unreported))
    }
}

async fn reported_levels(url: &str) -> Option<EffortLevels> {
    let client = crate::http_client::builder()
        .ok()?
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let response = client.get(url).send().await.ok()?.error_for_status().ok()?;
    let caps = response.json::<Properties>().await.ok()?.chat_template_caps;
    if !caps.supports_reasoning_effort {
        return Some(EffortLevels::BudgetOnly);
    }
    let levels = crate::profiles::model::EFFORTS
        .iter()
        .filter(|known| {
            caps.reasoning_effort_levels
                .iter()
                .any(|level| level == **known)
        })
        .map(|level| (*level).to_owned())
        .collect::<Vec<_>>();
    (!levels.is_empty()).then_some(EffortLevels::Native(levels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};
    use serde_json::json;

    #[tokio::test]
    async fn live_template_levels_are_distinct_from_absent_metadata() -> crate::Result<()> {
        let props = json!({"chat_template_caps":{"supports_reasoning_effort":true,
            "reasoning_effort_levels":["minimal","low","high","high","unsupported"]}});
        let none = json!({"chat_template_caps":{"supports_reasoning_effort":false,"reasoning_effort_levels":[]}});
        let app = Router::new()
            .route("/native", get(|| async { Json(props) }))
            .route("/budget", get(|| async { Json(none) }))
            .route("/old", get(|| async { Json(json!({})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        assert!(
            matches!(reported_levels(&format!("{url}/native")).await,Some(EffortLevels::Native(levels)) if levels == ["minimal","low","high"])
        );
        assert!(matches!(
            reported_levels(&format!("{url}/budget")).await,
            Some(EffortLevels::BudgetOnly)
        ));
        assert!(reported_levels(&format!("{url}/old")).await.is_none());
        assert!(reported_levels(&format!("{url}/missing")).await.is_none());
        server.abort();
        Ok(())
    }
}
