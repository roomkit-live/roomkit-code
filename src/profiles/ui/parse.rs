//! Reading a submitted connection or agent form into the profile it
//! describes.
use super::*;

pub(super) fn managed_elsewhere(subscription: &super::subscriptions::Subscription) -> String {
    match subscription.credential {
        super::subscriptions::Credential::Account => format!(
            "Manage {name} connections through the {name} account menu",
            name = subscription.name
        ),
        super::subscriptions::Credential::Key { .. } => format!(
            "Edit {} connections from their own form in /providers",
            subscription.name
        ),
    }
}
pub(in crate::profiles) fn value<'a>(values: &'a FormValues, key: &str) -> &'a str {
    values.get(key).unwrap_or("")
}
pub(super) fn identity(id: &str, original: Option<&str>, exists: bool) -> Result<(), String> {
    if original.is_some_and(|old| old != id) {
        return Err("Existing profile IDs cannot be renamed".into());
    }
    if original.is_none() && exists {
        return Err("This profile ID already exists".into());
    }
    Ok(())
}
pub(super) fn parse_provider(
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
        .and_then(Provider::subscription)
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
        subscription: None,
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
pub(super) fn parse_agent(
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
