//! Services available from the shared account menus.
use super::{Accounts, Action};
use roomkit_auth::{Account, Google, GoogleAccount, SignOut};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Service {
    ChatGpt,
    Google,
}

pub(super) struct Entry {
    pub id: String,
    pub email: Option<String>,
    pub connected: bool,
    pub can_use: bool,
}

impl From<Account> for Entry {
    fn from(a: Account) -> Self {
        Self {
            id: a.id,
            email: a.email,
            connected: a.connected,
            can_use: a.plan_usage,
        }
    }
}
impl From<GoogleAccount> for Entry {
    fn from(a: GoogleAccount) -> Self {
        Self {
            id: a.id,
            email: a.email,
            connected: a.connected,
            can_use: a.connected,
        }
    }
}

impl Service {
    pub fn name(self) -> &'static str {
        match self {
            Self::ChatGpt => "ChatGPT",
            Self::Google => "Google",
        }
    }

    pub(super) fn prefix(self) -> &'static str {
        match self {
            Self::ChatGpt => "chatgpt",
            Self::Google => "google",
        }
    }

    pub fn action(self, action: Action<'_>) -> String {
        let prefix = self.prefix();
        match action {
            Action::Use(id) => format!("{prefix}-use:{id}"),
            _ => format!("{prefix}:{action}"),
        }
    }

    pub(super) fn entries(self, accounts: &Accounts) -> Result<Vec<Entry>, String> {
        match self {
            Self::ChatGpt => accounts
                .auth
                .accounts()
                .map(|v| v.into_iter().map(Entry::from).collect()),
            Self::Google => accounts
                .google()?
                .accounts()
                .map(|v| v.into_iter().map(Entry::from).collect()),
        }
        .map_err(|e| e.to_string())
    }

    pub(super) async fn sign_out(self, accounts: &Accounts, id: &str) -> Result<SignOut, String> {
        match self {
            Self::ChatGpt => accounts.auth.sign_out(id).await,
            Self::Google => accounts.google()?.sign_out(id).await,
        }
        .map_err(|e| e.to_string())
    }
}

pub(super) fn google_client(path: &Path) -> Result<Google, String> {
    #[cfg(test)]
    if let Ok(client) = super::TEST_GOOGLE_CLIENT.try_with(Clone::clone) {
        return Ok(client);
    }
    let runtime = (
        std::env::var("ROOMKIT_GOOGLE_CLIENT_ID").ok(),
        std::env::var("ROOMKIT_GOOGLE_CLIENT_SECRET").ok(),
    );
    let (id, secret) = match runtime {
        (None, None) => (
            option_env!("ROOMKIT_GOOGLE_CLIENT_ID").map(str::to_owned),
            option_env!("ROOMKIT_GOOGLE_CLIENT_SECRET").map(str::to_owned),
        ),
        pair => pair,
    };
    let (Some(id), Some(secret)) = (
        id.filter(|s| !s.is_empty()),
        secret.filter(|s| !s.is_empty()),
    ) else {
        return Err("Google sign-in requires the publisher's Google OAuth client. Configure ROOMKIT_GOOGLE_CLIENT_ID and ROOMKIT_GOOGLE_CLIENT_SECRET or use a Gemini API key in /providers.".into());
    };
    Google::new(path, id, secret).map_err(|e| e.to_string())
}
