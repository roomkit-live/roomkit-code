//! Application menus over reusable account authentication.
use roomkit_auth::{ChatGpt, Google, RemoteSignIn, SignOut};
use roomkit_tui::protocol::{Choice, Event, PanelSpec};

mod action;
mod service;
mod signin;
mod token;
pub(crate) use action::Action;
use roomkit_tui_engine::CommandResult;
use service::Entry;
pub(crate) use service::Service;
pub(crate) use signin::callback_form_id;
use std::{path::Path, sync::Arc};
pub(crate) use token::Credential;
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};

pub(crate) struct Accounts {
    pub auth: ChatGpt,
    google: Result<Google, String>,
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
            google: service::google_client(path),
            pending: Mutex::new(None),
            events: watch::channel(None).0,
        }))
    }
    pub fn google(&self) -> Result<&Google, String> {
        self.google.as_ref().map_err(Clone::clone)
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
        Ok(Arc::new(Credential::ChatGpt(
            self.auth.clone(),
            account.into(),
        )))
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
        service: Service,
        action: Action<'_>,
    ) -> Result<CommandResult, String> {
        match action {
            Action::Menu => {
                self.close().await;
                self.menu(room, service)
            }
            Action::Account(id) => self.account_menu(room, service, id),
            Action::Connect => self.connect(room, service, None).await,
            Action::Login(id) => self.connect(room, service, Some(id)).await,
            Action::Logout(id) => {
                let outcome = service.sign_out(self, id).await?;
                let mut result = self.account_menu(room, service, id)?;
                if let CommandResult::Events(events) = &mut result {
                    events.push(Event::Notice { conversation: room.into(), text: match outcome {
                        SignOut::Revoked => format!("Signed out of this {} account", service.name()),
                        SignOut::LocalOnly => format!("Signed out locally. Confirm revocation in your {} account settings.", service.name()),
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
                    self.menu(room, service)
                } else {
                    Ok(CommandResult::Events(Vec::new()))
                }
            }
            // The profiles save an account's connection.
            Action::Use(_) => Err("Choose a model for this account from /providers".into()),
        }
    }
    fn menu(&self, room: &str, service: Service) -> Result<CommandResult, String> {
        let mut entries = vec![choice(
            service.action(Action::Connect),
            format!("+ Connect a {} account", service.name()),
            &format!("Sign in with {} in your browser", service.name()),
        )];
        entries.extend(service.entries(self)?.iter().map(|a| {
            choice(
                service.action(Action::Account(&a.id)),
                label(a),
                if !a.connected {
                    "Signed out"
                } else if !a.can_use {
                    "Connected · plan usage not enabled"
                } else {
                    "Connected · choose models or manage account"
                },
            )
        }));
        Ok(choices(
            room,
            &format!("{} accounts", service.name()),
            entries,
        ))
    }
    fn account_menu(
        &self,
        room: &str,
        service: Service,
        id: &str,
    ) -> Result<CommandResult, String> {
        let account = service
            .entries(self)?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| format!("Unknown {} account", service.name()))?;
        let mut entries = Vec::new();
        if account.connected && account.can_use {
            entries.push(choice(
                service.action(Action::Use(id)),
                "Use this account · choose a model",
                "Create an agent using this account's available models",
            ));
        }
        entries.push(choice(
            service.action(Action::Login(id)),
            if account.connected {
                "Sign in again"
            } else {
                "Sign in"
            },
            "Reauthorize this account in your browser",
        ));
        if account.connected {
            entries.push(choice(
                service.action(Action::Logout(id)),
                "Sign out",
                "Revoke access and remove this account's local tokens",
            ));
        }
        Ok(choices(
            room,
            &format!("{} · {}", service.name(), label(&account)),
            entries,
        ))
    }
}

fn label(account: &Entry) -> String {
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

#[cfg(test)]
tokio::task_local! {
    pub(crate) static TEST_GOOGLE_CLIENT: Google;
}
