//! Subscriptions: the services a connection reaches through a person's plan
//! rather than a plain endpoint. Each one is an entry here, which says where
//! its credential comes from and how the service is called. The rest of the
//! profiles code asks this registry instead of naming a service. A service
//! signed in through roomkit-auth also needs that account's menus.
use crate::providers::ProviderKind;
use roomkit_openai::{OpenAiApi, OpenAiConfig};
use std::{collections::BTreeMap, path::Path};

/// Where a subscription's credential comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Credential {
    /// A sign-in that roomkit-auth keeps and renews. The connection's
    /// credential names the subscription.
    Account,
    /// An API key the person copies from the service's console. The
    /// connection names its subscription in its `subscription` field.
    Key { console: &'static str },
}

/// How a subscription routes one agent's requests to one prompt cache: a key
/// stable for a workspace and an agent, sent in a body field and in headers.
pub(crate) struct CacheRouting {
    pub body: Option<&'static str>,
    pub headers: &'static [&'static str],
}

/// Which of the service's models a connection offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Models {
    /// The ones the signed-in account lists.
    Account,
    /// The ones `GET /models` lists as served over `api`, at a price: a free
    /// model only answers the service's own client.
    Listed { api: &'static str },
}

/// Which assistant message field the service reads a model's earlier
/// reasoning back from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Thinking {
    /// The endpoint's own rule (`reasoning::thinking_field`).
    Endpoint,
    /// `field` for every model, except those whose ID starts with one of
    /// `except`. Never guessed: it is what the service's model list states.
    Field {
        field: &'static str,
        except: &'static [&'static str],
    },
}

/// The entry the providers menu shows for a subscription.
pub(crate) struct Offer {
    pub action: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

/// A service a connection reaches through a person's plan.
pub(crate) struct Subscription {
    /// The ID a connection with a pasted key names it by.
    pub id: &'static str,
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
    /// Headers every request carries besides the cache routing.
    pub headers: &'static [(&'static str, &'static str)],
    pub models: Models,
    pub thinking: Thinking,
    pub offer: Offer,
}

/// How roomkit-code names itself to a service that asks.
const USER_AGENT: &str = concat!("roomkit-code/", env!("CARGO_PKG_VERSION"));

/// ChatGPT plan usage, through an account roomkit-auth signs in.
pub(crate) const CHATGPT: Subscription = Subscription {
    id: "chatgpt",
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
    headers: &[],
    models: Models::Account,
    thinking: Thinking::Endpoint,
    offer: Offer {
        action: "chatgpt:menu",
        label: "Sign in with ChatGPT",
        description: "Use your ChatGPT account and manage connected accounts",
    },
};

/// OpenCode Go, a subscription to open models through one API key
/// (<https://opencode.ai/docs/go/>). This connection serves the models Go
/// lists over Chat Completions.
pub(crate) const OPENCODE_GO: Subscription = Subscription {
    id: "opencode-go",
    name: "OpenCode Go",
    credential: Credential::Key {
        console: "https://opencode.ai/console",
    },
    kind: ProviderKind::OpenAi,
    base_url: "https://opencode.ai/inference/go/openai/v1",
    api: OpenAiApi::ChatCompletions,
    tool_namespace: None,
    // Go refuses a request without a session ID (HTTP 400
    // `MissingSessionID`) and routes and caches by it: the agent's cache key,
    // stable for the agent in this workspace, as ChatGPT's routing uses.
    cache_routing: CacheRouting {
        body: None,
        headers: &["x-opencode-session"],
    },
    // Go asks a client to name itself rather than its HTTP library.
    headers: &[("user-agent", USER_AGENT)],
    models: Models::Listed {
        api: "chat-completions",
    },
    // Go's model list states `reasoning_content` for every Chat Completions
    // model it serves except the Hy family (2026-11-10).
    thinking: Thinking::Field {
        field: "reasoning_content",
        except: &["hy"],
    },
    offer: Offer {
        action: "subscription:opencode-go",
        label: "Connect OpenCode Go",
        description: "Use your OpenCode Go subscription with its API key",
    },
};

/// Every subscription, in the order the providers menu offers them.
pub(crate) const ALL: &[&Subscription] = &[&CHATGPT, &OPENCODE_GO];

/// The subscription named `id`.
pub(crate) fn find(id: &str) -> Option<&'static Subscription> {
    ALL.iter()
        .copied()
        .find(|subscription| subscription.id == id)
}

impl Subscription {
    /// `config` with this subscription's wire settings, for `agent` in
    /// `workspace`: its API root and protocol, its tool namespace, its
    /// headers and the key that routes the agent's requests to one prompt
    /// cache.
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
            .chain(
                self.headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
            )
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

    /// The field `model`'s earlier reasoning goes back in, if any.
    pub fn thinking_field(&self, base_url: &str, model: &str) -> Option<String> {
        match self.thinking {
            Thinking::Endpoint => super::reasoning::thinking_field(base_url, model),
            Thinking::Field { field, except } => {
                (!except.iter().any(|prefix| model.starts_with(prefix))).then(|| field.to_owned())
            }
        }
    }

    /// Whether the connection offers this entry of the service's model list.
    pub fn offers(&self, model: &serde_json::Value) -> bool {
        match self.models {
            Models::Account => true,
            Models::Listed { api } => {
                let served = model.get("api").and_then(|apis| apis.get(api)).is_some();
                let paid = model
                    .pointer("/cost/text/input/price")
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(|price| price > 0.0);
                served && paid
            }
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
    use serde_json::json;

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
        assert_eq!(CHATGPT.thinking_field(CHATGPT.base_url, "gpt-6-sol"), None);
    }

    /// Go's requests name the conversation, by the agent's cache key, and
    /// the client; they carry no ChatGPT routing body or tool namespace.
    #[test]
    fn opencode_go_requests_carry_the_session_and_the_client() {
        let config = OPENCODE_GO.configure(
            OpenAiConfig::new("go-key", "kimi-k3"),
            Path::new("/work/project"),
            "code",
        );
        let headers = config.default_headers.unwrap();
        assert_eq!(
            headers["x-opencode-session"],
            cache_key(Path::new("/work/project"), "code")
        );
        assert_eq!(
            headers["user-agent"],
            concat!("roomkit-code/", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(headers.len(), 2);
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://opencode.ai/inference/go/openai/v1")
        );
        assert_eq!(config.api, OpenAiApi::ChatCompletions);
        assert!(config.extra_body.is_none() && config.tool_namespace.is_none());
        assert_eq!(config.api_key, "go-key");
    }

    /// The reasoning field follows Go's model list: `reasoning_content`
    /// except for the Hy family, which states none.
    #[test]
    fn opencode_go_reasoning_follows_its_model_list() {
        for model in ["kimi-k3", "deepseek-v4-pro", "glm-5.3", "mimo-v2.6-pro"] {
            assert_eq!(
                OPENCODE_GO
                    .thinking_field(OPENCODE_GO.base_url, model)
                    .as_deref(),
                Some("reasoning_content"),
                "{model}"
            );
        }
        for model in ["hy3", "hy4", "hy4-preview"] {
            assert_eq!(
                OPENCODE_GO.thinking_field(OPENCODE_GO.base_url, model),
                None,
                "{model}"
            );
        }
    }

    /// Of Go's list, a connection offers the paid models served over Chat
    /// Completions: a free one answers only OpenCode's own client, and another
    /// API needs another protocol.
    #[test]
    fn opencode_go_offers_its_paid_chat_completions_models() {
        let entry = |api: &str, price: f64| json!({"id": "m", "api": {api: {}}, "cost": {"text": {"input": {"price": price}}}});
        assert!(OPENCODE_GO.offers(&entry("chat-completions", 0.15)));
        assert!(!OPENCODE_GO.offers(&entry("chat-completions", 0.0)));
        assert!(!OPENCODE_GO.offers(&entry("responses", 1.0)));
        assert!(!OPENCODE_GO.offers(&entry("messages", 1.0)));
        assert!(!OPENCODE_GO.offers(&json!({"id": "m"})));
    }

    #[test]
    fn subscriptions_are_found_by_their_id() {
        assert_eq!(find("opencode-go").map(|s| s.name), Some("OpenCode Go"));
        assert_eq!(find("chatgpt").map(|s| s.name), Some("ChatGPT"));
        assert!(find("other").is_none());
    }

    /// End to end over HTTP: an endpoint that, like Go, refuses a request
    /// without a session ID. A connection set up as a plain endpoint, as
    /// roomkit-code did before Go was an entry, is refused; the same key and
    /// model through the subscription are answered.
    #[tokio::test]
    async fn an_endpoint_that_requires_a_session_answers_only_the_subscription() {
        use axum::{
            Json, Router,
            http::{HeaderMap, StatusCode},
            routing::post,
        };
        use roomkit_ai::{AiContext, AiMessage, AiProvider, AiRole};
        use std::sync::{Arc, Mutex};

        let seen = Arc::new(Mutex::new(Vec::<HeaderMap>::new()));
        let recorded = seen.clone();
        let app = Router::new().route(
            "/chat/completions",
            post(move |headers: HeaderMap| {
                let recorded = recorded.clone();
                async move {
                    recorded.lock().unwrap().push(headers.clone());
                    if !headers.contains_key("x-opencode-session") {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"type": "error", "error": {"type": "MissingSessionID"}})),
                        );
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "fixture", "object": "chat.completion", "model": "kimi-k3",
                            "choices": [{"index": 0, "finish_reason": "stop",
                                "message": {"role": "assistant", "content": "Hello"}}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        })),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let context = AiContext {
            messages: vec![AiMessage::new(AiRole::User, "Hi")],
            ..AiContext::default()
        };
        let ask = |config: OpenAiConfig| {
            let context = context.clone();
            async move {
                let provider = roomkit_openai::OpenAiAiProvider::new(config).unwrap();
                provider.generate(&context).await
            }
        };

        let plain = OpenAiConfig {
            base_url: Some(local.clone()),
            ..OpenAiConfig::new("go-key", "kimi-k3")
        };
        assert!(ask(plain).await.is_err());

        let subscribed = OpenAiConfig {
            // The fixture stands in for Go's API root.
            base_url: Some(local),
            ..OPENCODE_GO.configure(
                OpenAiConfig::new("go-key", "kimi-k3"),
                Path::new("/work/project"),
                "code",
            )
        };
        assert!(ask(subscribed).await.is_ok());

        let seen = seen.lock().unwrap();
        let last = seen.last().unwrap();
        assert_eq!(last["authorization"], "Bearer go-key");
        assert!(
            last["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("roomkit-code/")
        );
        server.abort();
    }
}
