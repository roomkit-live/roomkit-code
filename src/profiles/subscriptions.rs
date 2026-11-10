//! Subscriptions: the services a connection reaches through a person's plan
//! rather than a plain endpoint. Each one is an entry here, which says where
//! its credential comes from and how the service is called. The rest of the
//! profiles code asks this registry instead of naming a service, so adding one
//! is an entry and its tests.
use crate::providers::ProviderKind;
use roomkit_openai::{OpenAiApi, OpenAiConfig};
use std::{collections::BTreeMap, path::Path};

/// Where a subscription's credential comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Credential {
    /// A sign-in that roomkit-auth keeps and renews.
    Account,
}

/// How a subscription routes one agent's requests to one prompt cache: a key
/// stable for a workspace and an agent, sent in a body field and in headers.
pub(crate) struct CacheRouting {
    pub body: Option<&'static str>,
    pub headers: &'static [&'static str],
}

/// The entry the providers menu shows for a subscription.
pub(crate) struct Offer {
    pub action: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

/// A service a connection reaches through a person's plan.
pub(crate) struct Subscription {
    /// The name people know the service by.
    pub name: &'static str,
    pub credential: Credential,
    pub kind: ProviderKind,
    /// The only API root its credential may be sent to.
    pub base_url: &'static str,
    pub api: OpenAiApi,
    /// The namespace its tools are declared under, if it expects one.
    pub tool_namespace: Option<(&'static str, &'static str)>,
    pub cache_routing: CacheRouting,
    pub offer: Offer,
}

/// ChatGPT plan usage, through an account roomkit-auth signs in.
pub(crate) const CHATGPT: Subscription = Subscription {
    name: "ChatGPT",
    credential: Credential::Account,
    kind: ProviderKind::OpenAi,
    base_url: roomkit_auth::CHATGPT_API_ROOT,
    api: OpenAiApi::Responses,
    tool_namespace: Some(("functions", "Tools available to this assistant")),
    // ChatGPT's backend derives its cache affinity from the `session-id`
    // header; Codex sends it and `prompt_cache_key`, the same (RMK-1481).
    cache_routing: CacheRouting {
        body: Some("prompt_cache_key"),
        headers: &["session-id"],
    },
    offer: Offer {
        action: "chatgpt:menu",
        label: "Sign in with ChatGPT",
        description: "Use your ChatGPT account and manage connected accounts",
    },
};

/// Every subscription, in the order the providers menu offers them.
pub(crate) const ALL: &[&Subscription] = &[&CHATGPT];

impl Subscription {
    /// `config` with this subscription's wire settings, for `agent` in
    /// `workspace`: its API root and protocol, its tool namespace and the key
    /// that routes the agent's requests to one prompt cache.
    pub fn configure(&self, config: OpenAiConfig, workspace: &Path, agent: &str) -> OpenAiConfig {
        let key = cache_key(workspace, agent);
        let extra_body = self.cache_routing.body.map(|field| {
            let mut body = roomkit::model::Metadata::new();
            body.insert(field.into(), key.clone().into());
            body
        });
        let headers: BTreeMap<String, String> = self
            .cache_routing
            .headers
            .iter()
            .map(|header| ((*header).to_owned(), key.clone()))
            .collect();
        OpenAiConfig {
            base_url: Some(self.base_url.into()),
            api: self.api,
            tool_namespace: self
                .tool_namespace
                .map(|(name, description)| (name.into(), description.into())),
            extra_body: extra_body.or(config.extra_body),
            default_headers: if headers.is_empty() {
                config.default_headers
            } else {
                Some(headers)
            },
            ..config
        }
    }
}

/// One key per agent and workspace, the same at every launch: a digest that
/// tells neither.
fn cache_key(workspace: &Path, agent: &str) -> String {
    let digest = crate::digest::sha256_hex(format!("{}\0{agent}", workspace.display()));
    format!("roomkit-code-{}", &digest[..32])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "Invalid fixtures fail the test")]
    use super::*;

    /// RMK-1481: one key per agent and workspace, the same at every launch,
    /// that names neither.
    #[test]
    fn an_agent_s_cache_key_is_its_own_and_stable() {
        let key = |workspace: &str, agent: &str| {
            let config =
                CHATGPT.configure(OpenAiConfig::new("", "model"), Path::new(workspace), agent);
            let key = config.extra_body.unwrap()["prompt_cache_key"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(
                config.default_headers.unwrap(),
                BTreeMap::from([("session-id".into(), key.clone())])
            );
            key
        };
        let code = key("/work/project", "code");
        assert_eq!(code, key("/work/project", "code"));
        assert_ne!(code, key("/work/project", "review"));
        assert_ne!(code, key("/work/other", "code"));
        assert!(
            code.starts_with("roomkit-code-") && code.len() == 45,
            "{code}"
        );
        assert!(!code.contains("work") && !code.contains("project"));
    }

    /// A ChatGPT connection calls the public OpenAI API with Responses and
    /// the `functions` namespace, as before the registry.
    #[test]
    fn chatgpt_requests_keep_their_endpoint_protocol_and_namespace() {
        let config = CHATGPT.configure(
            OpenAiConfig::new("", "model"),
            Path::new("/work/project"),
            "code",
        );
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(config.api, OpenAiApi::Responses);
        assert_eq!(
            config.tool_namespace,
            Some((
                "functions".into(),
                "Tools available to this assistant".into()
            ))
        );
    }
}
