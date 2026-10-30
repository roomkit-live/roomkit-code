use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    pub context_window: Option<u64>,
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Project {
    pub schema: u32,
    pub selected: Option<String>,
    pub agents: BTreeMap<String, Agent>,
}
impl Project {
    /// Upgrade only the original complete catalogue, once. Explicit subsets stay restricted.
    pub(super) fn migrate(&mut self) -> Result<bool, String> {
        match self.schema {
            2 => Ok(false),
            1 => {
                for agent in self.agents.values_mut() {
                    if agent.tools.len() == 10
                        && TOOLS[..10]
                            .iter()
                            .all(|name| agent.tools.iter().any(|tool| tool == name))
                    {
                        agent.tools = TOOLS.iter().map(|name| (*name).into()).collect();
                    }
                }
                self.schema = 2;
                Ok(true)
            }
            _ => Err("Unsupported profile file version".into()),
        }
    }
}

impl Default for Project {
    fn default() -> Self {
        Self {
            schema: 2,
            selected: None,
            agents: BTreeMap::new(),
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
        if let Authentication::ChatGpt { account } = &self.authentication
            && (self.base_url != roomkit_auth::CHATGPT_API_ROOT
                || uuid::Uuid::parse_str(account).is_err())
        {
            return Err(
                "ChatGPT accounts require the trusted public OpenAI API and a valid account ID"
                    .into(),
            );
        }
        match &self.authentication {
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

impl Agent {
    pub fn validate(&self) -> Result<(), String> {
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

pub(super) const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
