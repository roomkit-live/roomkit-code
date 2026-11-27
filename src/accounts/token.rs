//! Resolve a saved account's token at the time of an API request.
use super::Accounts;
use roomkit_auth::{ChatGpt, Google};
use roomkit_providers_openai::{TokenError, TokenFuture, TokenSource};
use std::sync::Arc;

pub(crate) enum Credential {
    ChatGpt(ChatGpt, String),
    Google(Google, String),
}

impl TokenSource for Credential {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async {
            let result = match self {
                Self::ChatGpt(auth, account) => auth.access_token(account).await,
                Self::Google(auth, account) => auth.access_token(account).await,
            };
            result
                .map(|s| s.expose_secret().to_owned())
                .map_err(|error| match error {
                    roomkit_auth::Error::SignInRequired | roomkit_auth::Error::Identity => {
                        TokenError::SignInRequired
                    }
                    roomkit_auth::Error::PlanUsageRequired
                    | roomkit_auth::Error::GoogleScopeRequired => TokenError::PermissionRequired,
                    _ => TokenError::Unavailable,
                })
        })
    }
}

impl Accounts {
    pub fn google_token_source(&self, account: &str) -> Result<Arc<dyn TokenSource>, String> {
        let auth = self.google()?;
        let connected = auth
            .accounts()
            .map_err(|e| e.to_string())?
            .iter()
            .any(|a| a.id == account && a.connected);
        if !connected {
            return Err("Sign in to this Google account with /providers".into());
        }
        Ok(Arc::new(Credential::Google(auth.clone(), account.into())))
    }
}
