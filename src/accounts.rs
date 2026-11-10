//! Application menus and browser launching over reusable account authentication.
use roomkit_auth::{Account, ChatGpt, SignOut};
use roomkit_tui::protocol::{Choice, Event, PanelSpec};
use roomkit_tui_engine::CommandResult;
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
    url: url::Url,
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
    ) -> Result<Arc<dyn roomkit_openai::TokenSource>, String> {
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
        argument: &str,
    ) -> Result<CommandResult, String> {
        let (action, id) = argument.split_once(':').unwrap_or((argument, ""));
        match action {
            "menu" | "" => {
                self.close().await;
                self.menu(room)
            }
            "account" => self.account_menu(room, id),
            "connect" => self.connect(room, None).await,
            "login" => self.connect(room, Some(id)).await,
            "logout" => {
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
            "cancel" | "dismiss" | "browser" => {
                let mut state = self.pending.lock().await;
                let Some(pending) = state
                    .as_ref()
                    .filter(|p| p.room == room && p.panel.id == id)
                else {
                    return Ok(CommandResult::Events(Vec::new()));
                };
                if action == "browser" {
                    open_browser(&pending.url)?;
                    return Ok(CommandResult::Events(Vec::new()));
                }
                state.take();
                self.events.send_replace(None);
                if action == "cancel" {
                    self.menu(room)
                } else {
                    Ok(CommandResult::Events(Vec::new()))
                }
            }
            _ => Err("Unknown ChatGPT account action; reopen /providers".into()),
        }
    }
    fn menu(&self, room: &str) -> Result<CommandResult, String> {
        let mut entries = vec![choice(
            "chatgpt:connect",
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
                        format!("chatgpt:account:{}", a.id),
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
                format!("chatgpt-use:{id}"),
                "Use this account · choose a model",
                "Create an agent using this account's available models",
            ));
        }
        entries.push(choice(
            format!("chatgpt:login:{id}"),
            if account.connected {
                "Sign in again"
            } else {
                "Sign in"
            },
            "Reauthorize this account in your browser",
        ));
        if account.connected {
            entries.push(choice(
                format!("chatgpt:logout:{id}"),
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
    async fn connect(
        self: &Arc<Self>,
        room: &str,
        account: Option<&str>,
    ) -> Result<CommandResult, String> {
        self.close().await;
        let sign_in = self
            .auth
            .sign_in(account)
            .await
            .map_err(|e| e.to_string())?;
        let url = sign_in.url().clone();
        let id = uuid::Uuid::new_v4().to_string();
        let browser_opened = open_browser(&url).is_ok();
        let panel = PanelSpec::new(
            id.clone(),
            "providers",
            "Sign in with ChatGPT",
            format!(
                "{}\n\nComplete sign-in in your browser, then return here.\nIf no browser opened, copy this local URL:\n\n{url}\n\nThis attempt expires in ten minutes. Esc cancels sign-in.",
                if browser_opened {
                    "Browser launch requested."
                } else {
                    "Could not launch a browser automatically."
                }
            ),
        )
        .with_actions(vec![
            choice(format!("chatgpt:browser:{id}"), "Open browser", ""),
            choice(format!("chatgpt:cancel:{id}"), "Cancel sign-in", ""),
        ])
        .with_dismiss(format!("chatgpt:dismiss:{id}"));
        let mut state = self.pending.lock().await;
        *state = Some(Pending {
            room: room.into(),
            panel: panel.clone(),
            url,
            task: None,
        });
        let weak = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            let result = sign_in.wait().await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            let mut state = this.pending.lock().await;
            let Some(pending) = state.as_mut().filter(|p| p.panel.id == id) else {
                return;
            };
            match result {
                Ok(account) => {
                    pending.panel.title = "ChatGPT account connected".into();
                    pending.panel.body = format!(
                        "Connected as {}.\n\n{}",
                        label(&account),
                        if account.plan_usage {
                            "Choose a model to create an agent with this account."
                        } else {
                            "Identity verified, but ChatGPT plan usage was not granted. Sign in again to authorize model access."
                        }
                    );
                    pending.panel.actions = if account.plan_usage {
                        vec![choice(
                            format!("chatgpt-use:{}", account.id),
                            "Choose a model",
                            "",
                        )]
                    } else {
                        vec![choice(
                            format!("chatgpt:login:{}", account.id),
                            "Authorize plan usage",
                            "",
                        )]
                    };
                    pending.panel.actions.push(choice(
                        format!("chatgpt:account:{}", account.id),
                        "Manage account",
                        "",
                    ));
                }
                Err(error) => {
                    pending.panel.title = "ChatGPT sign-in did not complete".into();
                    pending.panel.body = error.to_string();
                    pending.panel.actions = vec![choice("chatgpt:menu", "Back to accounts", "")];
                }
            }
            this.events.send_replace(Some(Event::PanelUpdated {
                conversation: pending.room.as_str().into(),
                panel: pending.panel.clone(),
            }));
        });
        if let Some(pending) = state.as_mut() {
            pending.task = Some(task);
        }
        Ok(CommandResult::Events(vec![Event::Panel {
            conversation: room.into(),
            panel,
        }]))
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
fn open_browser(url: &url::Url) -> Result<(), String> {
    // This URL is library-generated loopback state, never a model tool argument.
    #[cfg(target_os = "macos")]
    let launcher = "open";
    #[cfg(not(target_os = "macos"))]
    let launcher = "xdg-open";
    let mut child = tokio::process::Command::new(launcher)
        .arg(url.as_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "Could not open the browser; copy the local URL shown above")?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

pub(crate) struct Credential {
    pub auth: ChatGpt,
    pub account: String,
}
impl roomkit_openai::TokenSource for Credential {
    fn token(&self) -> roomkit_openai::TokenFuture<'_> {
        Box::pin(async {
            self.auth
                .access_token(&self.account)
                .await
                .map(|s| s.expose_secret().to_owned())
                .map_err(|error| match error {
                    roomkit_auth::Error::SignInRequired | roomkit_auth::Error::Identity => {
                        roomkit_openai::TokenError::SignInRequired
                    }
                    roomkit_auth::Error::PlanUsageRequired => {
                        roomkit_openai::TokenError::PermissionRequired
                    }
                    _ => roomkit_openai::TokenError::Unavailable,
                })
        })
    }
}
