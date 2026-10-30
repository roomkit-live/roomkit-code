use roomkit_cli_roomkit::CommandResult;
use roomkit_tui::protocol::{
    Choice, Command, Event, FieldKind, FormField, FormSpec, FormValues, Request,
};

use super::*;

pub(super) struct Pending {
    room: String,
    form: FormSpec,
    target: Target,
}
enum Target {
    Provider(Option<String>),
    Agent(Option<String>),
}

fn choice(
    value: impl Into<String>,
    label: impl Into<String>,
    description: impl Into<String>,
) -> Choice {
    Choice {
        value: value.into(),
        label: label.into(),
        description: description.into(),
    }
}
fn field(
    name: &str,
    label: &str,
    value: impl Into<String>,
    help: &str,
    required: bool,
    kind: FieldKind,
) -> FormField {
    FormField {
        name: name.into(),
        label: label.into(),
        value: value.into(),
        help: help.into(),
        required,
        kind,
    }
}
fn choices(
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
        request: Request,
        current: SessionConfig,
    ) -> Result<CommandResult, String> {
        let mut state = self.state.lock().await;
        match request.command {
            Command::Invoke { name, arguments } => {
                state.pending = None;
                // Refresh shared connections only when opening a menu/form,
                // never just before saving an older form's revision.
                state.providers.reload()?;
                state.project.reload()?;
                if state.providers.value.schema != 1 || state.project.value.schema != 1 {
                    return Err("Unsupported profile file version".into());
                }
                let mut parts = arguments.trim().splitn(3, ':');
                let action = parts.next().unwrap_or_default();
                let id = parts.next().unwrap_or_default();
                let model = parts.next().unwrap_or_default();
                match (name.as_str(), action) {
                    ("providers", "") => Ok(provider_choices(&state, &current)),
                    ("providers", "open") => {
                        let p = state
                            .providers
                            .value
                            .profiles
                            .get(id)
                            .ok_or("Unknown provider")?;
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
                    ("agents", "add" | "edit") => {
                        let existing = if action == "edit" {
                            Some(state.project.value.agents.get(id).ok_or("Unknown agent")?)
                        } else {
                            None
                        };
                        let target = Target::Agent(existing.map(|a| a.id.clone()));
                        let form =
                            agent_form(existing, &state, uuid::Uuid::new_v4().to_string(), None)?;
                        Ok(open_form(&mut state, &current, form, target))
                    }
                    ("agents", "select") => {
                        let agent = state
                            .project
                            .value
                            .agents
                            .get(id)
                            .ok_or("Unknown agent")?
                            .clone();
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
                        let mut project = state.project.value.clone();
                        project.selected = Some(id.into());
                        state.project.save(project)?;
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
                if values.0.len() != pending.form.fields.len()
                    || pending
                        .form
                        .fields
                        .iter()
                        .any(|f| !values.0.contains_key(&f.name))
                {
                    return Err("The form fields changed; cancel and reopen it".into());
                }
                let mut next = current;
                match &pending.target {
                    Target::Provider(original) => {
                        let profile = parse_provider(&values, original.as_deref(), &state)?;
                        let runtime = if let Some(agent) = state
                            .project
                            .value
                            .agents
                            .get(next.agent.as_str())
                            .filter(|a| a.provider == profile.id)
                        {
                            Some(self.assemble(agent, &profile).await?)
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
                        let profile = parse_agent(&values, original.as_deref(), &state)?;
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
                        let mut project = state.project.value.clone();
                        project.selected = Some(profile.id.clone());
                        project.agents.insert(profile.id.clone(), profile);
                        state.project.save(project)?;
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

fn provider_choices(state: &State, current: &SessionConfig) -> CommandResult {
    let mut entries = vec![choice(
        "add",
        "+ Add provider",
        "OpenAI-compatible connection",
    )];
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
    for agent in state.project.value.agents.values() {
        entries.push(choice(
            format!("select:{}", agent.id),
            format!(
                "{}{}",
                if current.agent.as_str() == agent.id {
                    "● "
                } else {
                    ""
                },
                agent.label
            ),
            format!("{} · {} · select interlocutor", agent.provider, agent.model),
        ));
        entries.push(choice(
            format!("edit:{}", agent.id),
            format!("Edit {}", agent.label),
            "Save changes and select this agent",
        ));
    }
    choices(
        current,
        "agents",
        "Project agents · shared conversation",
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
        Authentication::None => ("none", ""),
        Authentication::Stored { .. } => ("stored", ""),
        Authentication::Environment { variable } => ("environment", variable.as_str()),
    };
    FormSpec {
        id,
        command: "providers".into(),
        title: "Provider connection · OpenAI-compatible".into(),
        fields: vec![
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
                "url",
                "API base URL",
                &p.base_url,
                "API root including /v1 when required; no credentials in the URL",
                true,
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
    }
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
    if existing.is_none() && state.project.value.agents.len() >= 64 {
        return Err("Maximum of 64 agents reached".into());
    }
    let default = Agent {
        id: next_id("agent", |id| state.project.value.agents.contains_key(id)),
        label: "New agent".into(),
        provider: preset.map_or("", |p| p.0).into(),
        model: preset.map_or("", |p| p.1).into(),
        context_window: None,
        instructions: String::new(),
        tools: TOOLS.iter().map(|s| (*s).into()).collect(),
    };
    let a = existing.unwrap_or(&default);
    Ok(FormSpec {
        id,
        command: "agents".into(),
        title: "Agent profile · save and select".into(),
        fields: vec![
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
                "Comma-separated: code_outline, list, glob, grep, read, diff, write, edit, patch, shell. Empty = no tools",
                false,
                FieldKind::Text,
            ),
        ],
    })
}

fn value<'a>(values: &'a FormValues, key: &str) -> &'a str {
    values.0.get(key).map_or("", String::as_str)
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
    let provider = Provider {
        id: id.into(),
        label: value(values, "label").trim().into(),
        base_url: value(values, "url").trim().trim_end_matches('/').into(),
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
) -> Result<Agent, String> {
    let id = value(values, "id").trim();
    identity(id, original, state.project.value.agents.contains_key(id))?;
    let capacity = value(values, "context").trim();
    let profile = Agent {
        id: id.into(),
        label: value(values, "label").trim().into(),
        provider: value(values, "provider").into(),
        model: value(values, "model").trim().into(),
        context_window: if capacity.is_empty() {
            None
        } else {
            Some(
                capacity
                    .parse()
                    .map_err(|_| "Context capacity must be a positive integer")?,
            )
        },
        instructions: value(values, "instructions").into(),
        tools: value(values, "tools")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
    };
    profile.validate()?;
    Ok(profile)
}

async fn discover(provider: &Provider) -> Result<Vec<(String, Option<u64>)>, String> {
    provider.validate()?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| "Cannot initialize connection check")?;
    let mut request = client.get(format!(
        "{}/models",
        provider.base_url.trim_end_matches('/')
    ));
    let key = provider.authentication.key()?;
    if !key.is_empty() {
        request = request.bearer_auth(key);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| "Connection failed; verify the URL, network and credential")?;
    if !response.status().is_success() {
        return Err(format!(
            "Model discovery returned HTTP {}; verify the URL and authentication (some servers do not support /models)",
            response.status().as_u16()
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not read model catalogue")?
    {
        if body.len() + chunk.len() > 1_048_576 {
            return Err("Model catalogue exceeds 1 MiB".into());
        }
        body.extend_from_slice(&chunk);
    }
    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| "Server did not return a JSON model catalogue")?;
    let mut models = json
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or("Server did not return a model list")?
        .iter()
        .filter_map(|model| {
            Some((
                model.get("id")?.as_str()?.to_owned(),
                model
                    .get("max_model_len")
                    .and_then(serde_json::Value::as_u64),
            ))
        })
        .filter(|(id, _)| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        .take(500)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup_by(|a, b| a.0 == b.0);
    if models.is_empty() {
        return Err(
            "Connection succeeded but no models were returned; enter a model manually in /agents"
                .into(),
        );
    }
    Ok(models)
}
