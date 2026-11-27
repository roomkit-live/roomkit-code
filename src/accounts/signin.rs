//! Browser sign-in with a link and a private callback field on one screen.

use super::*;
use roomkit_tui::protocol::{FieldKind, FormField, FormSpec, FormValues};

pub(crate) fn callback_form_id(id: &str) -> bool {
    id.starts_with("chatgpt-callback:") || id.starts_with("google-callback:")
}

impl Accounts {
    pub(super) async fn connect(
        self: &Arc<Self>,
        room: &str,
        service: Service,
        account: Option<&str>,
    ) -> Result<CommandResult, String> {
        self.close().await;
        match service {
            Service::ChatGpt => {
                self.open(room, service, self.auth.sign_in(account).await)
                    .await
            }
            Service::Google => {
                self.open(room, service, self.google()?.sign_in(account).await)
                    .await
            }
        }
    }

    async fn open<A: Into<Entry> + Send + 'static>(
        self: &Arc<Self>,
        room: &str,
        service: Service,
        attempt: Result<roomkit_auth::SignIn<A>, roomkit_auth::Error>,
    ) -> Result<CommandResult, String> {
        let sign_in = attempt.map_err(|e| e.to_string())?;
        let remote = sign_in.remote();
        let form = form(service, remote.url());
        self.install(room, service, &form.id, remote, sign_in).await;
        Ok(CommandResult::Events(vec![Event::Form {
            conversation: room.into(),
            form,
        }]))
    }

    async fn install<A: Into<Entry> + Send + 'static>(
        self: &Arc<Self>,
        room: &str,
        service: Service,
        id: &str,
        remote: RemoteSignIn,
        sign_in: roomkit_auth::SignIn<A>,
    ) {
        let id = id.to_owned();
        let mut state = self.pending.lock().await;
        *state = Some(Pending {
            room: room.into(),
            panel: PanelSpec::new(
                &id,
                "providers",
                format!("Sign in with {}", service.name()),
                "",
            )
            .with_dismiss(service.action(Action::Dismiss(&id))),
            remote: Some(remote),
            task: None,
        });
        let weak = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            let result = sign_in.wait().await;
            if let Some(this) = weak.upgrade() {
                this.finished(service, &id, result.map(Into::into)).await;
            }
        });
        if let Some(pending) = state.as_mut() {
            pending.task = Some(task);
        }
    }

    async fn finished(
        &self,
        service: Service,
        id: &str,
        result: Result<Entry, roomkit_auth::Error>,
    ) {
        let mut state = self.pending.lock().await;
        let Some(pending) = state.as_mut().filter(|p| p.panel.id == id) else {
            return;
        };
        pending.remote = None;
        completed_panel(service, &mut pending.panel, result);
        self.events.send_replace(Some(Event::FormCompleted {
            conversation: pending.room.as_str().into(),
            form: id.into(),
            panel: pending.panel.clone(),
        }));
    }

    pub(crate) async fn submit_callback(
        &self,
        room: &str,
        form: &str,
        values: &FormValues,
    ) -> Result<CommandResult, String> {
        self.accept_callback(room, form, values).await?;
        // Acknowledge before the network exchange so cancellation can run.
        // The scoped FormCompleted update is the only completion path.
        Ok(CommandResult::Events(Vec::new()))
    }

    async fn accept_callback(
        &self,
        room: &str,
        form: &str,
        values: &FormValues,
    ) -> Result<(), String> {
        let mut state = self.pending.lock().await;
        let pending = state
            .as_mut()
            .filter(|pending| pending.room == room && pending.panel.id == form)
            .ok_or("This sign-in form is no longer open in this conversation")?;
        let callback = values
            .get("callback")
            .filter(|_| values.len() == 1)
            .ok_or("Paste the complete callback URL in the sign-in form")?;
        pending
            .remote
            .as_ref()
            .ok_or("This sign-in is no longer waiting")?
            .submit_callback(callback)
            .map_err(|e| e.to_string())?;
        pending.remote = None;
        Ok(())
    }
}

fn form(service: Service, url: &url::Url) -> FormSpec {
    let id = format!("{}-callback:{}", service.prefix(), uuid::Uuid::new_v4());
    FormSpec::new(&id, "providers", format!("Sign in with {}", service.name()), vec![
        FormField::new("callback", "Paste the return URL", FieldKind::Secret).required().with_help(
            "The whole address the browser ends on, from http, even on a connection error."
        ),
    ])
    .with_markdown_description(format!(
        "Cmd-click (macOS) or Ctrl-click (Linux), or copy this link:\n\n[Open sign-in page](<{url}>)\n\nOn RoomKit Code's computer, sign-in completes by itself. On another, the browser ends on a connection error at `127.0.0.1`, which is expected: copy its whole address, paste it below and press Enter. Expires in ten minutes."
    ))
    .with_dismiss(service.action(Action::Dismiss(&id)))
    .with_async_completion()
}

fn completed_panel(
    service: Service,
    panel: &mut PanelSpec,
    result: Result<Entry, roomkit_auth::Error>,
) {
    match result {
        Ok(account) => connected_panel(service, panel, &account),
        Err(error) => {
            panel.title = format!("{} sign-in did not complete", service.name());
            panel.body = error.to_string();
            panel.actions = vec![choice(service.action(Action::Menu), "Back to accounts", "")];
        }
    }
}

fn connected_panel(service: Service, panel: &mut PanelSpec, account: &Entry) {
    panel.title = format!("{} account connected", service.name());
    panel.body = format!(
        "Connected as {}.\n\n{}",
        label(account),
        if account.can_use {
            "Choose a model to create an agent with this account."
        } else {
            "Identity verified, but ChatGPT plan usage was not granted. Sign in again to authorize model access."
        }
    );
    panel.actions = vec![
        if account.can_use {
            choice(
                service.action(Action::Use(&account.id)),
                "Choose a model",
                "",
            )
        } else {
            choice(
                service.action(Action::Login(&account.id)),
                "Authorize plan usage",
                "",
            )
        },
        choice(
            service.action(Action::Account(&account.id)),
            "Manage account",
            "",
        ),
    ];
}

#[cfg(test)]
mod tests;
