//! The form a subscription with a pasted key is connected and edited through:
//! the connection's name, its key and its timeout. Its API root and protocol
//! are the service's, so the form does not offer them.
use roomkit_tui::protocol::{FieldKind, FormSpec, FormValues};

use super::State;
use super::model::{Authentication, Provider};
use super::subscriptions::{Credential, Subscription};
use super::ui::{field, next_id, value};

/// The form that connects `subscription`, or edits `existing`, its connection.
pub(super) fn form(
    subscription: &Subscription,
    existing: Option<&Provider>,
    form_id: String,
) -> FormSpec {
    let console = match subscription.credential {
        Credential::Key { console } => console,
        Credential::Account => "",
    };
    let key_help = match existing {
        None => format!("Copy it from {console}; stored in a private local file"),
        Some(_) => format!("Masked; empty keeps the stored key. Copy a new one from {console}"),
    };
    FormSpec::new(
        form_id,
        "providers",
        subscription.name,
        vec![
            field(
                "label",
                "Name",
                existing.map_or(subscription.name, |p| p.label.as_str()),
                "Display name for this connection",
                true,
                FieldKind::Text,
            ),
            field(
                "key",
                "API key",
                "",
                &key_help,
                existing.is_none(),
                FieldKind::Secret,
            ),
            field(
                "timeout",
                "Timeout (seconds)",
                existing.map_or(600, |p| p.timeout_seconds).to_string(),
                "Maximum duration of one model request (1–86400)",
                true,
                FieldKind::Text,
            ),
        ],
    )
}

/// The connection the submitted form describes: a new one under the next
/// free ID, or `original` with its new name, key or timeout.
pub(super) fn parse(
    values: &FormValues,
    subscription: &'static Subscription,
    original: Option<&str>,
    state: &State,
) -> Result<Provider, String> {
    let profiles = &state.providers.value.profiles;
    let existing = match original {
        Some(id) => Some(profiles.get(id).ok_or("Unknown provider")?),
        None => None,
    };
    if existing.is_some_and(|p| p.subscription.as_deref() != Some(subscription.id)) {
        return Err(format!(
            "This connection is not an {} connection",
            subscription.name
        ));
    }
    // A key copied from the console may come with a line break.
    let entered = value(values, "key").trim();
    let key = if entered.is_empty() {
        existing
            .and_then(|p| match &p.authentication {
                Authentication::Stored { key } => Some(key.clone()),
                _ => None,
            })
            .unwrap_or_default()
    } else {
        entered.into()
    };
    let id = match original {
        Some(id) => id.to_owned(),
        None => next_id(subscription.id, |id| profiles.contains_key(id)),
    };
    if id.is_empty() {
        return Err("Maximum of 128 provider profiles reached".into());
    }
    let provider = Provider {
        subscription: Some(subscription.id.into()),
        kind: subscription.kind,
        id,
        label: value(values, "label").trim().into(),
        base_url: subscription.base_url.into(),
        authentication: Authentication::Stored { key },
        timeout_seconds: value(values, "timeout")
            .trim()
            .parse()
            .map_err(|_| "Timeout must be an integer")?,
    };
    provider.validate()?;
    Ok(provider)
}
