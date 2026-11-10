use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Current `agents.json` version; schema 4 files migrate through `Project::migrate`.
pub(crate) const PROJECT_SCHEMA: u32 = 5;

pub(crate) const TOOLS: &[&str] = &[
    "code_outline",
    "list",
    "glob",
    "grep",
    "read",
    "diff",
    "write",
    "edit",
    "patch",
    "shell",
    "web_fetch",
    "web_search",
    "process_start",
    "process_status",
    "process_read",
    "process_write",
    "process_stop",
    "output_read",
    "output_forget",
    "plan_create",
    "plan_read",
    "plan_list",
    "plan_update",
    "plan_delete",
    "docs_list",
    "docs_search",
    "docs_read",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Authentication {
    None,
    Stored { key: String },
    Environment { variable: String },
    ChatGpt { account: String },
}

impl Authentication {
    pub fn key(&self) -> Result<String, String> {
        match self {
            Self::ChatGpt { .. } => {
                Err("ChatGPT credentials must be resolved through the account manager".into())
            }
            Self::None => Ok(String::new()),
            Self::Stored { key } => Ok(key.clone()),
            Self::Environment { variable } => std::env::var(variable)
                .ok()
                .filter(|key| !key.is_empty())
                .ok_or_else(|| {
                    "The provider's credential environment variable is missing or empty".into()
                }),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Provider {
    #[serde(default)]
    pub kind: crate::providers::ProviderKind,
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub authentication: Authentication,
    pub timeout_seconds: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Agent {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp: BTreeMap<String, Vec<String>>,
    pub context_window: Option<u64>,
    /// The most tokens one model reply may write; `None` sends no limit and
    /// the endpoint decides. Written only when set, so a file without it
    /// stays readable by earlier versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    pub instructions: String,
    pub tools: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Providers {
    pub schema: u32,
    pub profiles: BTreeMap<String, Provider>,
}
impl Default for Providers {
    fn default() -> Self {
        Self {
            schema: 1,
            profiles: BTreeMap::new(),
        }
    }
}

/// A project's own choices for a global agent: the MCP servers configured in
/// this project and the skills it enables there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Extension {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp: BTreeMap<String, Vec<String>>,
}

/// A project's agents (`projects/<hash>/agents.json`) or the global ones
/// shared by every project (`agents.json` beside `connections.json`).
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Project {
    pub schema: u32,
    pub selected: Option<String>,
    pub agents: BTreeMap<String, Agent>,
    /// In a project file, its choices for global agents, by agent ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Extension>,
}
impl Project {
    /// Bring a schema 4 file to the current schema, which only adds global
    /// agents' extensions; whether it changed.
    pub(super) fn migrate(&mut self) -> Result<bool, String> {
        match self.schema {
            PROJECT_SCHEMA => Ok(false),
            4 => {
                self.schema = PROJECT_SCHEMA;
                Ok(true)
            }
            _ => Err("Unsupported profile file version".into()),
        }
    }
}

impl Default for Project {
    fn default() -> Self {
        Self {
            schema: PROJECT_SCHEMA,
            selected: None,
            agents: BTreeMap::new(),
            extensions: BTreeMap::new(),
        }
    }
}

pub(super) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !matches!(id, "terminal" | "local-user")
}

fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

impl Provider {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) || !text(&self.label, 128) {
            return Err(
                "Use a short provider name and an ID containing letters, digits, - or _".into(),
            );
        }
        let url = url::Url::parse(&self.base_url).map_err(|_| "Invalid provider URL")?;
        if self.base_url.len() > 2048
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Use an HTTP(S) API root without credentials, query or fragment".into());
        }
        if !(1..=86_400).contains(&self.timeout_seconds) {
            return Err("Timeout must be between 1 and 86400 seconds".into());
        }
        // Its credential may only go to the subscription's own API.
        if let Some(subscription) = self.subscription()
            && (self.kind != subscription.kind || self.base_url != subscription.base_url)
        {
            return Err(format!(
                "{} connections use only its own API, {}",
                subscription.name, subscription.base_url
            ));
        }
        match &self.authentication {
            Authentication::ChatGpt { account } if uuid::Uuid::parse_str(account).is_err() => {
                return Err("Invalid ChatGPT account ID".into());
            }
            Authentication::Stored { key } if !text(key, 8192) => {
                return Err("Enter a nonempty API key without control characters".into());
            }
            Authentication::Environment { variable }
                if !text(variable, 128)
                    || !variable
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    || variable.starts_with(|c: char| c.is_ascii_digit()) =>
            {
                return Err("Enter a valid environment variable name".into());
            }
            _ => {}
        }
        Ok(())
    }
}

impl Provider {
    /// The subscription this connection goes through, if any: an account
    /// credential belongs to its service.
    pub fn subscription(&self) -> Option<&'static super::subscriptions::Subscription> {
        match self.authentication {
            Authentication::ChatGpt { .. } => Some(&super::subscriptions::CHATGPT),
            _ => None,
        }
    }

    /// The subscription whose account manages this connection, if any: its
    /// settings change through that account's menu, not the provider form.
    pub fn account_managed(&self) -> Option<&'static super::subscriptions::Subscription> {
        self.subscription()
            .filter(|s| s.credential == super::subscriptions::Credential::Account)
    }
}

impl Agent {
    pub fn validate(&self) -> Result<(), String> {
        if self.mcp.len() > 32
            || self.mcp.values().map(Vec::len).sum::<usize>() > 64
            || self.mcp.iter().any(|(id, names)| {
                !super::mcp::valid_id(id)
                    || names
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != names.len()
                    || names.iter().any(|name| {
                        name.is_empty()
                            || format!("mcp__{id}__{name}").len() > 128
                            || !name
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    })
            })
        {
            return Err("Select at most 64 valid MCP tools across 32 servers; use individual tool selection for large catalogues".into());
        }

        if self.skills.len() > 128
            || self.skills.iter().any(|name| {
                name.is_empty()
                    || name.len() > 64
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
        {
            return Err("Skills must contain at most 128 valid skill names".into());
        }
        if self
            .reasoning_effort
            .as_deref()
            .is_some_and(|e| !EFFORTS.contains(&e))
        {
            return Err("Choose a supported reasoning effort or provider default".into());
        }
        if !valid_id(&self.id)
            || !text(&self.label, 128)
            || !valid_id(&self.provider)
            || !text(&self.model, 256)
        {
            return Err("Supply valid agent/provider IDs, a name and a model identifier".into());
        }
        if self.context_window == Some(0) || self.context_window.is_some_and(|n| n > 1_000_000_000)
        {
            return Err(
                "Context capacity must be between 1 and 1000000000 tokens, or empty".into(),
            );
        }
        if self.max_output_tokens == Some(0) {
            return Err("Max output tokens must be a positive number of tokens, or empty".into());
        }
        if self.instructions.len() > 8192
            || self
                .instructions
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err("Instructions must contain at most 8192 bytes of text".into());
        }
        let unique: std::collections::BTreeSet<_> = self.tools.iter().collect();
        if self.tools.len() > TOOLS.len()
            || unique.len() != self.tools.len()
            || self
                .tools
                .iter()
                .any(|tool| !TOOLS.contains(&tool.as_str()))
        {
            return Err("Choose distinct tool names from the available inventory".into());
        }
        Ok(())
    }
}

pub(crate) const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

#[cfg(test)]
mod tests {
    #[test]
    fn existing_connections_without_a_kind_keep_the_openai_protocol() {
        let value = serde_json::json!({"id":"local", "label":"Local", "base_url":"http://localhost:8000/v1", "authentication":{"kind":"none"}, "timeout_seconds":600});
        assert!(
            serde_json::from_value::<super::Provider>(value)
                .is_ok_and(|p| p.kind == crate::providers::ProviderKind::OpenAi)
        );
    }
}
