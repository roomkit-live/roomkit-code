//! A signed-in Gemini connection uses the person's own quota project.
use super::*;

impl Profiles {
    pub(super) async fn google_project_form(
        &self,
        current: &SessionConfig,
        account: &str,
    ) -> Result<CommandResult, String> {
        self.accounts.google_token_source(account)?;
        self.accounts.close().await;
        let mut state = self.state.lock().await;
        state.providers.reload()?;
        state.pending_delete = None;
        let saved = state
            .providers
            .value
            .profiles
            .get(&format!("google-{account}"))
            .and_then(|p| match &p.authentication {
                Authentication::Google {
                    account: id,
                    project,
                } if id == account => Some(project.clone()),
                _ => None,
            });
        let project = saved
            .or_else(|| std::env::var("GOOGLE_CLOUD_QUOTA_PROJECT").ok())
            .unwrap_or_default();
        let form = FormSpec::new(
            uuid::Uuid::new_v4().to_string(),
            "providers",
            "Gemini · Google Cloud project",
            vec![field(
                "project",
                "Quota project ID",
                project,
                "Enable the Generative Language API on this project. Gemini usage is attributed to it.",
                true,
                FieldKind::Text,
            )],
        );
        Ok(open_form(
            &mut state,
            current,
            form,
            Target::GoogleProject {
                account: account.into(),
            },
        ))
    }

    pub(super) fn google_connection(
        &self,
        state: &State,
        account: &str,
        project: &str,
    ) -> Result<Provider, String> {
        let entry = self
            .accounts
            .google()?
            .accounts()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|a| a.id == account && a.connected)
            .ok_or("Sign in to this Google account with /providers")?;
        let id = format!("google-{account}");
        if let Some(existing) = state.providers.value.profiles.get(&id) {
            if existing.authentication.account()
                != Some((crate::accounts::Service::Google, account))
            {
                return Err("Another connection already uses this Google connection ID".into());
            }
        } else {
            state.providers.value.check_room()?;
        }
        let kind = ProviderKind::Gemini;
        let label = entry
            .email
            .as_deref()
            .unwrap_or("Account")
            .chars()
            .take(80)
            .collect::<String>();
        let provider = Provider {
            subscription: None,
            kind,
            id,
            label: format!("Gemini · {label}"),
            base_url: kind.default_url().into(),
            authentication: Authentication::Google {
                account: account.into(),
                project: project.into(),
            },
            timeout_seconds: 600,
        };
        provider.validate()?;
        Ok(provider)
    }
}
