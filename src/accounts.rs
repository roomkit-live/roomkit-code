//! Application menus over reusable account authentication.
use roomkit_auth::{Account, ChatGpt, RemoteSignIn, SignOut};
use roomkit_tui::protocol::{Choice, Event, PanelSpec};

mod action;
mod signin;
pub(crate) use action::Action;
use roomkit_tui_engine::CommandResult;
pub(crate) use signin::callback_form_id;
use std::{path::Path, sync::Arc};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};

pub(crate) struct Accounts {
    pub auth: ChatGpt,
    pending: Mutex<Option<Pending>>,
    events: watch::Sender<Option<Event>>,
}
struct Pending {
    room: String,
    panel: PanelSpec,
    remote: Option<RemoteSignIn>,
    task: Option<JoinHandle<()>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Accounts {
    pub fn new(path: &Path) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            auth: ChatGpt::new(path, "RoomKit Code").map_err(|e| e.to_string())?,
            pending: Mutex::new(None),
            events: watch::channel(None).0,
        }))
    }
    /// The renewable credential of a saved account that may make model
    /// requests; an account signed out or without plan usage has none.
    pub fn token_source(
        &self,
        account: &str,
    ) -> Result<Arc<dyn roomkit_providers_openai::TokenSource>, String> {
        let saved = self
            .auth
            .accounts()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|a| a.id == account)
            .ok_or("ChatGPT account not found; open /providers")?;
        if !saved.connected {
            return Err("Sign in to this ChatGPT account with /providers".into());
        }
        if !saved.plan_usage {
            return Err("Authorize ChatGPT plan usage with /providers".into());
        }
        Ok(Arc::new(Credential {
            auth: self.auth.clone(),
            account: account.into(),
        }))
    }
    pub fn subscribe(&self) -> watch::Receiver<Option<Event>> {
        self.events.subscribe()
    }
    pub async fn close(&self) {
        self.pending.lock().await.take();
        self.events.send_replace(None);
    }
    pub async fn handle(
        self: &Arc<Self>,
        room: &str,
        action: Action<'_>,
    ) -> Result<CommandResult, String> {
        match action {
            Action::Menu => {
                self.close().await;
                self.menu(room)
            }
            Action::Account(id) => self.account_menu(room, id),
            Action::Connect => self.connect(room, None).await,
            Action::Login(id) => self.connect(room, Some(id)).await,
            Action::Logout(id) => {
                let outcome = self.auth.sign_out(id).await.map_err(|e| e.to_string())?;
                let mut result = self.account_menu(room, id)?;
                if let CommandResult::Events(events) = &mut result {
                    events.push(Event::Notice { conversation: room.into(), text: match outcome {
                        SignOut::Revoked => "Signed out of this ChatGPT account".into(),
                        SignOut::LocalOnly => "Signed out locally. Remote revocation could not be confirmed; revoke access in your ChatGPT account settings if needed.".into(),
                    } });
                }
                Ok(result)
            }
            Action::Cancel(id) | Action::Dismiss(id) => {
                let mut state = self.pending.lock().await;
                if !state
                    .as_ref()
                    .is_some_and(|p| p.room == room && p.panel.id == id)
                {
                    return Ok(CommandResult::Events(Vec::new()));
                }
                state.take();
                self.events.send_replace(None);
                if matches!(action, Action::Cancel(_)) {
                    self.menu(room)
                } else {
                    Ok(CommandResult::Events(Vec::new()))
                }
            }
            // The profiles save an account's connection.
            Action::Use(_) => Err("Choose a model for this account from /providers".into()),
        }
    }
    fn menu(&self, room: &str) -> Result<CommandResult, String> {
        let mut entries = vec![choice(
            Action::Connect.to_string(),
            "+ Connect a ChatGPT account",
            "Sign in with ChatGPT in your browser",
        )];
        entries.extend(
            self.auth
                .accounts()
                .map_err(|e| e.to_string())?
                .iter()
                .map(|a| {
                    choice(
                        Action::Account(&a.id).to_string(),
                        label(a),
                        if !a.connected {
                            "Signed out"
                        } else if !a.plan_usage {
                            "Connected · plan usage not enabled"
                        } else {
                            "Connected · choose models or manage account"
                        },
                    )
                }),
        );
        Ok(choices(room, "ChatGPT accounts", entries))
    }
    fn account_menu(&self, room: &str, id: &str) -> Result<CommandResult, String> {
        let account = self
            .auth
            .accounts()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or("Unknown ChatGPT account")?;
        let mut entries = Vec::new();
        if account.connected && account.plan_usage {
            entries.push(choice(
                Action::Use(id).to_string(),
                "Use this account · choose a model",
                "Create an agent using this account's available models",
            ));
        }
        entries.push(choice(
            Action::Login(id).to_string(),
            if account.connected {
                "Sign in again"
            } else {
                "Sign in"
            },
            "Reauthorize this account in your browser",
        ));
        if account.connected {
            entries.push(choice(
                Action::Logout(id).to_string(),
                "Sign out",
                "Revoke access and remove this account's local tokens",
            ));
        }
        Ok(choices(
            room,
            &format!("ChatGPT · {}", label(&account)),
            entries,
        ))
    }
}

fn label(account: &Account) -> String {
    account
        .email
        .clone()
        .unwrap_or_else(|| format!("Account {}", account.id.chars().take(8).collect::<String>()))
}
fn choice(value: impl Into<String>, label: impl Into<String>, description: &str) -> Choice {
    Choice::new(value, label).with_description(description)
}
fn choices(room: &str, title: &str, choices: Vec<Choice>) -> CommandResult {
    CommandResult::Events(vec![Event::Choices {
        conversation: room.into(),
        command: "providers".into(),
        title: title.into(),
        choices,
    }])
}
pub(crate) struct Credential {
    pub auth: ChatGpt,
    pub account: String,
}
impl roomkit_providers_openai::TokenSource for Credential {
    fn token(&self) -> roomkit_providers_openai::TokenFuture<'_> {
        Box::pin(async {
            self.auth
                .access_token(&self.account)
                .await
                .map(|s| s.expose_secret().to_owned())
                .map_err(|error| match error {
                    roomkit_auth::Error::SignInRequired | roomkit_auth::Error::Identity => {
                        roomkit_providers_openai::TokenError::SignInRequired
                    }
                    roomkit_auth::Error::PlanUsageRequired => {
                        roomkit_providers_openai::TokenError::PermissionRequired
                    }
                    _ => roomkit_providers_openai::TokenError::Unavailable,
                })
        })
    }
}
