//! The forms of connections and agents: opening one, and reading what it
//! was submitted with.
use super::*;

/// Opens the form of a new connection, or of connection `id`: its own form
/// for a subscription with a pasted key.
pub(super) fn connection_form(
    state: &mut State,
    current: &SessionConfig,
    id: Option<&str>,
) -> Result<CommandResult, String> {
    let existing = match id {
        Some(id) => Some(
            state
                .providers
                .value
                .profiles
                .get(id)
                .ok_or("Unknown provider")?
                .clone(),
        ),
        None => None,
    };
    let Some(existing) = existing else {
        state.providers.value.check_room()?;
        let form = provider_form(None, state, uuid::Uuid::new_v4().to_string());
        return Ok(open_form(state, current, form, Target::Provider(None)));
    };
    if let Some(subscription) = existing.account_managed() {
        return Err(managed_elsewhere(subscription));
    }
    let form_id = uuid::Uuid::new_v4().to_string();
    let (form, target) = match existing.subscription() {
        Some(subscription) => (
            super::subscription_form::form(subscription, Some(&existing), form_id),
            Target::Subscription {
                subscription,
                original: Some(existing.id.clone()),
            },
        ),
        None => (
            provider_form(Some(&existing), state, form_id),
            Target::Provider(Some(existing.id.clone())),
        ),
    };
    Ok(open_form(state, current, form, target))
}

/// Opens the form of a new connection to subscription `id`, one whose
/// credential is a pasted key.
pub(super) fn subscription_form(
    state: &mut State,
    current: &SessionConfig,
    id: &str,
) -> Result<CommandResult, String> {
    let subscription = super::subscriptions::find(id)
        .filter(|s| matches!(s.credential, super::subscriptions::Credential::Key { .. }))
        .ok_or("Unknown subscription")?;
    state.providers.value.check_room()?;
    let target = Target::Subscription {
        subscription,
        original: None,
    };
    let form = super::subscription_form::form(subscription, None, uuid::Uuid::new_v4().to_string());
    Ok(open_form(state, current, form, target))
}

/// Opens the form of a new agent on a connection's `model`, with the
/// capacity its server reported for it: a listed model, or one to type
/// when `typed`.
pub(super) fn new_agent_form(
    state: &mut State,
    current: &SessionConfig,
    (provider, model, reported): (&str, &str, Option<u64>),
    typed: bool,
) -> Result<CommandResult, String> {
    if !state.providers.value.profiles.contains_key(provider)
        || model.is_empty() != typed
        || !(model.is_empty() || listable(model))
    {
        return Err("Invalid provider/model selection".into());
    }
    let on = Some((provider, model, reported));
    let form = agent_form(None, state, uuid::Uuid::new_v4().to_string(), on)?;
    Ok(open_form(state, current, form, Target::Agent(None)))
}

/// Opens an agent's form: a new one, or `existing`'s.
pub(super) fn edit_agent_form(
    state: &mut State,
    current: &SessionConfig,
    existing: Option<&Agent>,
) -> Result<CommandResult, String> {
    let target = Target::Agent(existing.map(|agent| agent.id.clone()));
    let form = agent_form(existing, state, uuid::Uuid::new_v4().to_string(), None)?;
    Ok(open_form(state, current, form, target))
}

pub(super) fn open_form(
    state: &mut State,
    current: &SessionConfig,
    form: FormSpec,
    target: Target,
) -> CommandResult {
    state.pending = Some(Pending {
        room: current.room.to_string(),
        form: form.clone(),
        target,
    });
    CommandResult::Events(vec![Event::Form {
        conversation: current.room.as_str().into(),
        form,
    }])
}

/// Whether a choice's `model` may preset a form: an ID a saved agent may
/// carry, whatever sent it.
pub(super) fn listable(model: &str) -> bool {
    !model.trim().is_empty() && model.len() <= 256 && !model.chars().any(char::is_control)
}

pub(in crate::profiles) fn next_id(prefix: &str, occupied: impl Fn(&str) -> bool) -> String {
    (1..=129)
        .map(|n| format!("{prefix}-{n}"))
        .find(|id| !occupied(id))
        .unwrap_or_default()
}

fn provider_type(kind: ProviderKind) -> FormField {
    field(
        "kind",
        "Provider type",
        kind.value(),
        "The API protocol this connection uses",
        true,
        FieldKind::Choice {
            choices: vec![
                choice("openai", "OpenAI-compatible", "Chat Completions"),
                choice("anthropic", "Anthropic", "Native Messages API"),
                choice("llamacpp", "Local llama.cpp", "Run a GGUF on this computer"),
            ],
        },
    )
}

pub(super) fn provider_form(existing: Option<&Provider>, state: &State, id: String) -> FormSpec {
    let default = Provider {
        subscription: None,
        kind: ProviderKind::OpenAi,
        id: next_id("provider", |id| {
            state.providers.value.profiles.contains_key(id)
        }),
        label: "New provider".into(),
        base_url: String::new(),
        authentication: Authentication::None,
        timeout_seconds: 600,
    };
    let p = existing.unwrap_or(&default);
    let (auth, variable) = match &p.authentication {
        Authentication::ChatGpt { .. } => ("none", ""),
        Authentication::None => ("none", ""),
        Authentication::Stored { .. } => ("stored", ""),
        Authentication::Environment { variable } => ("environment", variable.as_str()),
    };
    FormSpec::new(
        id,
        "providers",
        "Provider connection",
        vec![
            field(
                "id",
                "ID",
                &p.id,
                "Stable ID; existing profiles cannot be renamed",
                true,
                FieldKind::Text,
            ),
            field(
                "label",
                "Name",
                &p.label,
                "Display name for this connection",
                true,
                FieldKind::Text,
            ),
            provider_type(p.kind),
            field(
                "url",
                "API base URL",
                &p.base_url,
                "Leave empty for local llama.cpp or default Anthropic. Include /v1 for OpenAI-compatible servers when required.",
                false,
                FieldKind::Text,
            ),
            field(
                "auth",
                "Authentication",
                auth,
                "Choose no authentication, a local stored key, or an environment variable",
                true,
                FieldKind::Choice {
                    choices: vec![
                        choice("none", "No key", ""),
                        choice("stored", "Stored API key", ""),
                        choice("environment", "Environment variable", ""),
                    ],
                },
            ),
            field(
                "key",
                "API key",
                "",
                "Masked; empty keeps the existing stored key. Changing auth mode clears it",
                false,
                FieldKind::Secret,
            ),
            field(
                "variable",
                "Environment variable",
                variable,
                "Used only with environment authentication; e.g. MY_PROVIDER_API_KEY",
                false,
                FieldKind::Text,
            ),
            field(
                "timeout",
                "Timeout (seconds)",
                p.timeout_seconds.to_string(),
                "Maximum duration of one model request (1–86400)",
                true,
                FieldKind::Text,
            ),
        ],
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "Debt recorded when the limit was set: split it, never grow it"
)]
pub(super) fn agent_form(
    existing: Option<&Agent>,
    state: &State,
    id: String,
    preset: Option<(&str, &str, Option<u64>)>,
) -> Result<FormSpec, String> {
    if state.providers.value.profiles.is_empty() {
        return Err("Add a connection with /providers first".into());
    }
    // Only a new agent's default ID avoids other projects' IDs.
    let elsewhere = if existing.is_none() {
        state.other_ids()
    } else {
        Vec::new()
    };
    let default = Agent {
        id: next_id("agent", |id| {
            state.scope(id).is_some() || elsewhere.iter().any(|used| used == id)
        }),
        label: "New agent".into(),
        provider: preset.map_or("", |p| p.0).into(),
        model: preset.map_or("", |p| p.1).into(),
        reasoning_effort: None,
        vision: false,
        skills: Vec::new(),
        mcp: BTreeMap::new(),
        context_window: preset.and_then(|p| p.2),
        max_output_tokens: None,
        instructions: String::new(),
        tools: TOOLS.iter().map(|s| (*s).into()).collect(),
    };
    let a = existing.unwrap_or(&default);
    Ok(FormSpec::new(
        id,
        "agents",
        "Agent profile · save and select",
        vec![
            field(
                "id",
                "ID",
                &a.id,
                "Stable author identity in history; existing profiles cannot be renamed",
                true,
                FieldKind::Text,
            ),
            field(
                "label",
                "Name",
                &a.label,
                "Name shown above this agent's messages",
                true,
                FieldKind::Text,
            ),
            field(
                "provider",
                "Provider",
                &a.provider,
                "Connection shared with other agents",
                true,
                FieldKind::Choice {
                    choices: state
                        .providers
                        .value
                        .profiles
                        .values()
                        .map(|p| choice(&p.id, &p.label, ""))
                        .collect(),
                },
            ),
            field(
                "model",
                "Model",
                &a.model,
                "Exact served model ID; /providers offers connection testing and discovery",
                true,
                FieldKind::Text,
            ),
            field(
                "context",
                "Context capacity",
                a.context_window.map_or_else(String::new, |n| n.to_string()),
                "Effective server capacity in tokens; empty means unknown",
                false,
                FieldKind::Text,
            ),
            field(
                "output",
                "Output budget (advanced)",
                a.max_output_tokens
                    .map_or_else(String::new, |n| n.to_string()),
                "Empty = Auto (includes reasoning); enter a token count only to set a fixed limit",
                false,
                FieldKind::Text,
            ),
            field(
                "effort",
                "Reasoning effort",
                a.reasoning_effort.as_deref().unwrap_or("default"),
                "Model-specific; default omits the parameter. Also available through /effort",
                true,
                FieldKind::Choice {
                    choices: effort_choices(),
                },
            ),
            field(
                "instructions",
                "Instructions",
                &a.instructions,
                "Additional instructions for this agent; Alt+Enter inserts a line break",
                false,
                FieldKind::Multiline,
            ),
            field(
                "tools",
                "Tools",
                a.tools.join(", "),
                &format!(
                    "Comma-separated: {}. Pool workers can also use cargo_test. Empty = no tools",
                    TOOLS.join(", ")
                ),
                false,
                FieldKind::Text,
            ),
            field(
                "vision",
                "Image input",
                if a.vision { "enabled" } else { "disabled" },
                "Enable only for models that accept images; Ctrl+V attaches a clipboard image",
                true,
                FieldKind::Choice {
                    choices: vec![
                        choice("disabled", "Disabled", "Text only"),
                        choice("enabled", "Enabled", "Model supports vision"),
                    ],
                },
            ),
            field(
                "scope",
                "Scope",
                existing
                    .and_then(|a| state.scope(&a.id))
                    .unwrap_or(Scope::Global)
                    .value(),
                "Global agents are available in every project; MCP servers and skills stay per project",
                true,
                FieldKind::Choice {
                    choices: vec![
                        choice("global", "Global", "Available in every project"),
                        choice("project", "This project", "Only in this project"),
                    ],
                },
            ),
        ],
    ))
}
