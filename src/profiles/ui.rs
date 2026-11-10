use roomkit_tui::protocol::{
    Choice, Command, Event, FieldKind, FormField, FormSpec, FormValues, Request,
};
use roomkit_tui_engine::CommandResult;

use super::discovery::discover;
use super::*;
use crate::providers::ProviderKind;
use catalog::Scope;

pub(super) struct Pending {
    room: String,
    form: FormSpec,
    target: Target,
}
#[derive(Clone)]
enum Target {
    Provider(Option<String>),
    Agent(Option<String>),
}

pub(super) fn choice(
    value: impl Into<String>,
    label: impl Into<String>,
    description: impl Into<String>,
) -> Choice {
    Choice::new(value, label).with_description(description)
}
fn effort_choices() -> Vec<Choice> {
    std::iter::once(choice(
        "default",
        "Provider default",
        "Let the model use its default effort",
    ))
    .chain(
        model::EFFORTS
            .iter()
            .map(|e| choice(*e, *e, "Choose a level supported by this model")),
    )
    .collect()
}

fn field(
    name: &str,
    label: &str,
    value: impl Into<String>,
    help: &str,
    required: bool,
    kind: FieldKind,
) -> FormField {
    let field = FormField::new(name, label, kind)
        .with_value(value)
        .with_help(help);
    if required { field.required() } else { field }
}
pub(super) fn choices(
    current: &SessionConfig,
    command: &str,
    title: &str,
    choices: Vec<Choice>,
) -> CommandResult {
    CommandResult::Events(vec![Event::Choices {
        conversation: current.room.as_str().into(),
        command: command.into(),
        title: title.into(),
        choices,
    }])
}

impl Profiles {
    pub async fn handle(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        mut request: Request,
        current: SessionConfig,
    ) -> Result<CommandResult, String> {
        // Human-readable aliases; choice values remain structured actions.
        if let Command::Invoke { name, arguments } = &mut request.command
            && matches!(name.as_str(), "agents" | "providers")
        {
            let trimmed = arguments.trim();
            if trimmed == "list" {
                arguments.clear();
            } else if let Some(id) = trimmed.strip_prefix("delete ") {
                *arguments = format!("delete:{}", id.trim());
            }
        }
        if let Command::Invoke { name, arguments } = &request.command
            && matches!(name.as_str(), "agents" | "providers")
            && (arguments.starts_with("delete:") || arguments.starts_with("confirm-delete:"))
        {
            return self.delete(kit, terminal, name, arguments, current).await;
        }
        if let Command::Invoke { name, arguments } = &request.command
            && name == "providers"
            && let Some(action) = arguments.strip_prefix("chatgpt:")
        {
            self.state.lock().await.pending_delete = None;
            return self.accounts.handle(current.room.as_str(), action).await;
        }
        let mut state = self.state.lock().await;
        state.pending_delete = None;
        // Menus and model discovery do not hold the cross-process edit lease.
        let mutating = match &request.command {
            Command::SubmitForm { .. } => true,
            Command::Invoke { name, arguments } => {
                (name == "agents" && arguments.starts_with("select:"))
                    || (name == "effort" && !arguments.trim().is_empty())
            }
            _ => false,
        };
        let _edit_lease = mutating
            .then(|| store::edit_lease(&self.state_dir))
            .transpose()?;
        match request.command {
            Command::Invoke { name, arguments } => {
                state.pending = None;
                // Refresh shared connections only when opening a menu/form,
                // never just before saving an older form's revision.
                state.providers.reload()?;
                state.reload_agents()?;
                if state.providers.value.schema != 1 {
                    return Err("Unsupported profile file version".into());
                }
                let mut parts = arguments.trim().splitn(3, ':');
                let action = parts.next().unwrap_or_default();
                let id = parts.next().unwrap_or_default();
                let model = parts.next().unwrap_or_default();
                match (name.as_str(), action) {
                    ("effort", effort) => {
                        let mut agent = state
                            .agent(current.agent.as_str())
                            .ok_or("Configure an agent with /agents first")?
                            .clone();
                        if effort.is_empty() {
                            return Ok(choices(
                                &current,
                                "effort",
                                "Reasoning effort · supported levels depend on the model",
                                effort_choices(),
                            ));
                        }
                        agent.reasoning_effort = if effort == "default" {
                            None
                        } else {
                            Some(effort.into())
                        };
                        agent.validate()?;
                        let provider = state
                            .providers
                            .value
                            .profiles
                            .get(&agent.provider)
                            .ok_or("Unknown provider")?;
                        let (runtime, tools) = self.assemble(&agent, provider).await?;
                        state.update_agent(agent)?;
                        state.install(kit, current.agent.as_str(), runtime, tools)?;
                        let mut next = current;
                        state.view(&mut next);
                        Ok(CommandResult::Switch(Box::new(next)))
                    }
                    ("providers", "") => Ok(provider_choices(&state, &current)),
                    ("providers", "chatgpt-use") => {
                        let account = self
                            .accounts
                            .auth
                            .accounts()
                            .map_err(|e| e.to_string())?
                            .into_iter()
                            .find(|a| a.id == id)
                            .ok_or("Unknown ChatGPT account")?;
                        let models = self
                            .accounts
                            .auth
                            .models(id)
                            .await
                            .map_err(|e| e.to_string())?;
                        if models.is_empty() {
                            return Err("This ChatGPT account returned no available models".into());
                        }
                        let _edit_lease = store::edit_lease(&self.state_dir)?;
                        state.providers.reload()?;
                        let provider_id = format!("chatgpt-{id}");
                        let chatgpt = &super::subscriptions::CHATGPT;
                        let provider = Provider {
                            kind: chatgpt.kind,
                            id: provider_id.clone(),
                            label: format!(
                                "ChatGPT · {}",
                                account
                                    .email
                                    .as_deref()
                                    .unwrap_or("Account")
                                    .chars()
                                    .take(80)
                                    .collect::<String>()
                            ),
                            base_url: chatgpt.base_url.into(),
                            authentication: Authentication::ChatGpt { account: id.into() },
                            timeout_seconds: 600,
                        };
                        provider.validate()?;
                        if !state.providers.value.profiles.contains_key(&provider_id)
                            && state.providers.value.profiles.len() >= 128
                        {
                            return Err("Maximum of 128 providers reached".into());
                        }
                        let mut providers = state.providers.value.clone();
                        providers.profiles.insert(provider_id.clone(), provider);
                        state.providers.save(providers)?;
                        self.accounts.close().await;
                        Ok(choices(
                            &current,
                            "providers",
                            "ChatGPT models · choose one to create an agent",
                            models
                                .into_iter()
                                .map(|m| {
                                    choice(format!("model:{provider_id}:{}", m.id), m.name, m.id)
                                })
                                .collect(),
                        ))
                    }
                    ("providers", "open") => {
                        let p = state
                            .providers
                            .value
                            .profiles
                            .get(id)
                            .ok_or("Unknown provider")?;
                        if let Authentication::ChatGpt { account } = &p.authentication {
                            return Ok(choices(
                                &current,
                                "providers",
                                &p.label,
                                vec![
                                    choice(
                                        format!("chatgpt:account:{account}"),
                                        "Manage ChatGPT account",
                                        "Choose models, sign in or sign out",
                                    ),
                                    choice(
                                        format!("delete:{id}"),
                                        "Delete provider",
                                        "Remove this connection; account sign-out is separate",
                                    ),
                                ],
                            ));
                        }
                        Ok(choices(
                            &current,
                            "providers",
                            &p.label,
                            vec![
                                choice(
                                    format!("edit:{id}"),
                                    "Edit connection",
                                    "URL, authentication and timeout",
                                ),
                                choice(
                                    format!("test:{id}"),
                                    "Test connection and list models",
                                    "Read-only GET /models; no conversation is sent",
                                ),
                                choice(
                                    format!("delete:{id}"),
                                    "Delete provider",
                                    "Remove this shared connection if no agents use it",
                                ),
                            ],
                        ))
                    }
                    ("providers", "add" | "edit") => {
                        let existing = if action == "edit" {
                            Some(
                                state
                                    .providers
                                    .value
                                    .profiles
                                    .get(id)
                                    .ok_or("Unknown provider")?,
                            )
                        } else {
                            None
                        };
                        if existing.is_none() && state.providers.value.profiles.len() >= 128 {
                            return Err("Maximum of 128 provider profiles reached".into());
                        }
                        if let Some(subscription) = existing.and_then(Provider::account_managed) {
                            return Err(managed_elsewhere(subscription));
                        }
                        let target = Target::Provider(existing.map(|p| p.id.clone()));
                        let form =
                            provider_form(existing, &state, uuid::Uuid::new_v4().to_string());
                        Ok(open_form(&mut state, &current, form, target))
                    }
                    ("providers", "test") => {
                        let provider = state
                            .providers
                            .value
                            .profiles
                            .get(id)
                            .ok_or("Unknown provider")?;
                        let models = discover(provider).await?;
                        Ok(choices(
                            &current,
                            "providers",
                            "Connection successful · choose a model to create an agent",
                            models
                                .into_iter()
                                .map(|(model, capacity)| {
                                    choice(
                                        format!("model:{id}:{model}"),
                                        model,
                                        capacity.map_or_else(
                                            || "Create agent with this model".into(),
                                            |n| {
                                                format!(
                                                    "Reported capacity: {n} tokens · create agent"
                                                )
                                            },
                                        ),
                                    )
                                })
                                .collect(),
                        ))
                    }
                    ("providers", "model") => {
                        if !state.providers.value.profiles.contains_key(id)
                            || model.is_empty()
                            || model.len() > 256
                        {
                            return Err("Invalid provider/model selection".into());
                        }
                        let form = agent_form(
                            None,
                            &state,
                            uuid::Uuid::new_v4().to_string(),
                            Some((id, model)),
                        )?;
                        Ok(open_form(&mut state, &current, form, Target::Agent(None)))
                    }
                    ("agents", "") => Ok(agent_choices(&state, &current)),
                    ("agents", "open") => {
                        let agent = state.agent(id).ok_or("Unknown agent")?;
                        Ok(choices(
                            &current,
                            "agents",
                            &agent.label,
                            vec![
                                choice(
                                    format!("select:{id}"),
                                    "Select agent",
                                    format!("{} · {}", agent.provider, agent.model),
                                ),
                                choice(
                                    format!("edit:{id}"),
                                    "Edit agent",
                                    "Provider, model, effort, instructions and tools",
                                ),
                                choice(
                                    format!("delete:{id}"),
                                    "Delete agent",
                                    "Remove this profile; keep conversation history",
                                ),
                            ],
                        ))
                    }
                    ("agents", "add" | "edit") => {
                        let existing = if action == "edit" {
                            Some(state.agent(id).ok_or("Unknown agent")?)
                        } else {
                            None
                        };
                        let target = Target::Agent(existing.as_ref().map(|a| a.id.clone()));
                        let form = agent_form(
                            existing.as_ref(),
                            &state,
                            uuid::Uuid::new_v4().to_string(),
                            None,
                        )?;
                        Ok(open_form(&mut state, &current, form, target))
                    }
                    ("agents", "select") => {
                        let agent = state.agent(id).ok_or("Unknown agent")?.clone();
                        let provider = state
                            .providers
                            .value
                            .profiles
                            .get(&agent.provider)
                            .ok_or("Unknown provider")?;
                        let (runtime, tools) = self.assemble(&agent, provider).await?;
                        state.ensure_slot(kit, id)?;
                        let mut next = current;
                        next.agent = ChannelId::new(id).map_err(|_| "Invalid agent ID")?;
                        self.attach(kit, &next, terminal).await?;
                        state.select_agent(id)?;
                        state.install(kit, id, runtime, tools)?;
                        state.view(&mut next);
                        Ok(CommandResult::Switch(Box::new(next)))
                    }
                    _ => Err("Unknown profile action; reopen /providers or /agents".into()),
                }
            }
            Command::SubmitForm { name, form, values } => {
                let pending = state
                    .pending
                    .as_ref()
                    .ok_or("This form is no longer open")?;
                if pending.room != current.room.as_str()
                    || pending.form.id != form
                    || pending.form.command != name
                {
                    return Err("This form does not belong to the current view".into());
                }
                if values.len() != pending.form.fields.len()
                    || pending
                        .form
                        .fields
                        .iter()
                        .any(|f| values.get(&f.name).is_none())
                {
                    return Err("The form fields changed; cancel and reopen it".into());
                }
                let target = pending.target.clone();
                let mut next = current;
                match &target {
                    Target::Provider(original) => {
                        let profile = parse_provider(&values, original.as_deref(), &state)?;
                        let runtime = if let Some(agent) = state
                            .agent(next.agent.as_str())
                            .filter(|a| a.provider == profile.id)
                        {
                            Some(self.assemble(&agent, &profile).await?)
                        } else {
                            None
                        };
                        let mut providers = state.providers.value.clone();
                        providers.profiles.insert(profile.id.clone(), profile);
                        state.providers.save(providers)?;
                        if let Some((agent, tools)) = runtime {
                            state.install(kit, next.agent.as_str(), agent, tools)?;
                        }
                    }
                    Target::Agent(original) => {
                        // A provider may have been deleted while this form was open.
                        state.providers.reload()?;
                        let (profile, scope) = parse_agent(&values, original.as_deref(), &state)?;
                        let provider = state
                            .providers
                            .value
                            .profiles
                            .get(&profile.provider)
                            .ok_or("Unknown provider")?;
                        let (runtime, tools) = self.assemble(&profile, provider).await?;
                        state.ensure_slot(kit, &profile.id)?;
                        next.agent = ChannelId::new(&profile.id).map_err(|_| "Invalid agent ID")?;
                        self.attach(kit, &next, terminal).await?;
                        state.save_agent(profile, scope)?;
                        state.select_agent(next.agent.as_str())?;
                        state.install(kit, next.agent.as_str(), runtime, tools)?;
                    }
                }
                state.pending = None;
                state.view(&mut next);
                Ok(CommandResult::Switch(Box::new(next)))
            }
            _ => Err("Unsupported profile request".into()),
        }
    }
}

fn open_form(
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

pub(super) fn provider_choices(state: &State, current: &SessionConfig) -> CommandResult {
    let mut entries = vec![choice(
        "add",
        "+ Add provider",
        "OpenAI-compatible or Anthropic connection",
    )];
    entries.extend(super::subscriptions::ALL.iter().map(|subscription| {
        let offer = &subscription.offer;
        choice(offer.action, offer.label, offer.description)
    }));
    entries.extend(state.providers.value.profiles.values().map(|p| {
        choice(
            format!("open:{}", p.id),
            &p.label,
            format!("{} · {}", p.id, p.base_url),
        )
    }));
    choices(
        current,
        "providers",
        "Providers · shared across your projects",
        entries,
    )
}
fn agent_choices(state: &State, current: &SessionConfig) -> CommandResult {
    let mut entries = vec![choice(
        "add",
        "+ Add agent",
        "Choose its provider, model, instructions and tools",
    )];
    for (agent, scope) in state.agents() {
        entries.push(choice(
            format!("open:{}", agent.id),
            format!(
                "{}{}",
                if current.agent.as_str() == agent.id {
                    "● "
                } else {
                    ""
                },
                agent.label
            ),
            format!(
                "{} · {} · {} · {}",
                agent.id,
                scope.value(),
                agent.provider,
                agent.model
            ),
        ));
    }
    choices(
        current,
        "agents",
        "Agents · global and this project's · shared conversation",
        entries,
    )
}

fn next_id(prefix: &str, occupied: impl Fn(&str) -> bool) -> String {
    (1..=129)
        .map(|n| format!("{prefix}-{n}"))
        .find(|id| !occupied(id))
        .unwrap_or_default()
}

fn provider_form(existing: Option<&Provider>, state: &State, id: String) -> FormSpec {
    let default = Provider {
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
            field(
                "kind",
                "Provider type",
                p.kind.value(),
                "The API protocol this connection uses",
                true,
                FieldKind::Choice {
                    choices: vec![
                        choice("openai", "OpenAI-compatible", "Chat Completions"),
                        choice("anthropic", "Anthropic", "Native Messages API"),
                    ],
                },
            ),
            field(
                "url",
                "API base URL",
                &p.base_url,
                "OpenAI: include /v1 when required. Anthropic: empty uses https://api.anthropic.com",
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

fn agent_form(
    existing: Option<&Agent>,
    state: &State,
    id: String,
    preset: Option<(&str, &str)>,
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
        context_window: None,
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
                &format!("Comma-separated: {}. Empty = no tools", TOOLS.join(", ")),
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

fn managed_elsewhere(subscription: &super::subscriptions::Subscription) -> String {
    format!(
        "Manage {name} connections through the {name} account menu",
        name = subscription.name
    )
}
fn value<'a>(values: &'a FormValues, key: &str) -> &'a str {
    values.get(key).unwrap_or("")
}
fn identity(id: &str, original: Option<&str>, exists: bool) -> Result<(), String> {
    if original.is_some_and(|old| old != id) {
        return Err("Existing profile IDs cannot be renamed".into());
    }
    if original.is_none() && exists {
        return Err("This profile ID already exists".into());
    }
    Ok(())
}
fn parse_provider(
    values: &FormValues,
    original: Option<&str>,
    state: &State,
) -> Result<Provider, String> {
    let id = value(values, "id").trim();
    identity(
        id,
        original,
        state.providers.value.profiles.contains_key(id),
    )?;
    if let Some(subscription) = original
        .and_then(|id| state.providers.value.profiles.get(id))
        .and_then(Provider::account_managed)
    {
        return Err(managed_elsewhere(subscription));
    }
    let authentication = match value(values, "auth") {
        "none" => Authentication::None,
        "environment" => Authentication::Environment {
            variable: value(values, "variable").trim().into(),
        },
        "stored" => {
            let entered = value(values, "key");
            let key = if entered.is_empty() {
                original
                    .and_then(|id| state.providers.value.profiles.get(id))
                    .and_then(|p| match &p.authentication {
                        Authentication::Stored { key } => Some(key.clone()),
                        _ => None,
                    })
                    .unwrap_or_default()
            } else {
                entered.into()
            };
            Authentication::Stored { key }
        }
        _ => return Err("Unknown authentication mode".into()),
    };
    let kind = match value(values, "kind") {
        "" => original
            .and_then(|id| state.providers.value.profiles.get(id))
            .map_or(ProviderKind::OpenAi, |p| p.kind),
        "openai" => ProviderKind::OpenAi,
        "anthropic" => ProviderKind::Anthropic,
        _ => return Err("Choose OpenAI-compatible or Anthropic".into()),
    };
    let url = value(values, "url").trim().trim_end_matches('/');
    let provider = Provider {
        kind,
        id: id.into(),
        label: value(values, "label").trim().into(),
        base_url: if url.is_empty() {
            kind.default_url()
        } else {
            url
        }
        .into(),
        authentication,
        timeout_seconds: value(values, "timeout")
            .trim()
            .parse()
            .map_err(|_| "Timeout must be an integer")?,
    };
    provider.validate()?;
    Ok(provider)
}
fn parse_agent(
    values: &FormValues,
    original: Option<&str>,
    state: &State,
) -> Result<(Agent, Scope), String> {
    let id = value(values, "id").trim();
    identity(id, original, state.scope(id).is_some())?;
    let scope = match value(values, "scope") {
        "global" => Scope::Global,
        "project" => Scope::Project,
        _ => return Err("Choose a global or project scope".into()),
    };
    if scope == Scope::Global && state.scope(id) != Some(Scope::Global) {
        state.check_global_id(id)?;
    }
    let capacity = value(values, "context").trim();
    let output = value(values, "output").trim();
    let profile =
        Agent {
            id: id.into(),
            label: value(values, "label").trim().into(),
            provider: value(values, "provider").into(),
            model: value(values, "model").trim().into(),
            vision: match value(values, "vision") {
                "" | "disabled" => false,
                "enabled" => true,
                _ => return Err("Choose enabled or disabled for image input".into()),
            },
            reasoning_effort: match value(values, "effort") {
                "" | "default" => None,
                effort => Some(effort.into()),
            },
            context_window: if capacity.is_empty() {
                None
            } else {
                Some(
                    capacity
                        .parse()
                        .map_err(|_| "Context capacity must be a positive integer")?,
                )
            },
            max_output_tokens: if output.is_empty() {
                None
            } else {
                Some(output.parse().map_err(
                    |_| "Max output tokens must be a positive number of tokens, or empty",
                )?)
            },
            instructions: value(values, "instructions").into(),
            mcp: original
                .and_then(|id| state.agent(id))
                .map(|agent| agent.mcp.clone())
                .unwrap_or_default(),
            skills: original
                .and_then(|id| state.agent(id))
                .map(|agent| agent.skills.clone())
                .unwrap_or_default(),
            tools: value(values, "tools")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        };
    profile.validate()?;
    Ok((profile, scope))
}
