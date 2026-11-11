#![expect(clippy::unwrap_used, reason = "fixture failures must fail the test")]

use super::*;
use roomkit_tui::protocol::{Actor, CommandSpec, Snapshot};

fn authorization() -> url::Url {
    url::Url::parse("https://auth.example/authorize?state=fixture").unwrap()
}

#[test]
fn sign_in_displays_the_link_and_private_return_field_together() {
    let url = authorization();
    let form = form(&url);
    assert!(callback_form_id(&form.id));
    assert_ne!(form.id, super::form(&url).id);
    assert_eq!(form.command, "providers");
    assert!(form.wait_for_completion);
    assert!(form.description.contains(url.as_str()));
    assert!(form.description.contains("connection error"));
    assert!(!form.description.contains("SSH"));
    assert_eq!(form.dismiss, format!("chatgpt:dismiss:{}", form.id));
    assert_eq!(form.fields.len(), 1);
    let field = &form.fields[0];
    assert_eq!(field.name, "callback");
    assert_eq!(field.kind, FieldKind::Secret);
    assert!(field.required && field.value.is_empty());
}

fn pending(id: &str) -> Pending {
    Pending {
        room: "room".into(),
        panel: PanelSpec::new(id, "providers", "Sign in", ""),
        remote: None,
        task: None,
    }
}

#[tokio::test]
async fn callback_forms_belong_to_one_conversation_and_one_revision() {
    let state = tempfile::tempdir().unwrap();
    let accounts = Accounts::new(&state.path().join("auth")).unwrap();
    *accounts.pending.lock().await = Some(pending("revision"));
    let mut values = FormValues::new();
    values.insert("callback", "PRIVATE_CALLBACK");
    for (room, form) in [("other-room", "revision"), ("room", "stale")] {
        let error = accounts
            .accept_callback(room, form, &values)
            .await
            .unwrap_err();
        assert!(error.contains("sign-in form"));
        assert!(!error.contains("PRIVATE_CALLBACK"));
    }
    assert_eq!(
        accounts.pending.lock().await.as_ref().unwrap().panel.id,
        "revision"
    );
    accounts.close().await;
}

#[tokio::test]
async fn expiration_and_local_completion_replace_the_open_form_without_submission() {
    let state = tempfile::tempdir().unwrap();
    let accounts = Accounts::new(&state.path().join("auth")).unwrap();
    for (result, expected) in [
        (Err(roomkit_auth::Error::Timeout), "timed out"),
        (
            Ok(Account {
                id: "account".into(),
                email: None,
                connected: true,
                plan_usage: true,
            }),
            "Connected as",
        ),
    ] {
        let mut updates = accounts.subscribe();
        let form = form(&authorization());
        *accounts.pending.lock().await = Some(pending(&form.id));
        let mut session = roomkit_tui::Session::default();
        let mut snapshot = Snapshot::new("room", "Test", Actor::new("agent", "Agent"));
        snapshot
            .commands
            .push(CommandSpec::new("providers", "Providers"));
        session.apply(Event::Snapshot(snapshot));
        session.apply(Event::Form {
            conversation: "room".into(),
            form: form.clone(),
        });
        accounts.finished(&form.id, result).await;
        updates.changed().await.unwrap();
        session.apply(updates.borrow_and_update().clone().unwrap());
        assert!(session.form().is_none());
        assert!(session.panel().unwrap().spec().body.contains(expected));
    }
    accounts.close().await;
}

#[tokio::test]
async fn dismissing_a_submitted_form_aborts_the_pending_operation_without_waiting() {
    let state = tempfile::tempdir().unwrap();
    let accounts = Accounts::new(&state.path().join("auth")).unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (finished, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut attempt = pending("revision");
    attempt.task = Some(tokio::spawn(async move {
        let _held_until_cancelled = finished;
        let _ = started.send(());
        std::future::pending::<()>().await;
    }));
    *accounts.pending.lock().await = Some(attempt);
    ready.await.unwrap();
    accounts.handle("room", "dismiss:revision").await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), stopped)
            .await
            .unwrap()
            .is_err()
    );
    assert!(accounts.pending.lock().await.is_none());
}

#[test]
fn sign_in_renders_the_exact_browser_destination_as_a_native_terminal_link() {
    let url = authorization();
    let mut session = roomkit_tui::Session::default();
    let mut snapshot = Snapshot::new("room", "Test", Actor::new("agent", "Agent"));
    snapshot
        .commands
        .push(CommandSpec::new("providers", "Providers"));
    session.apply(Event::Snapshot(snapshot));
    session.apply(Event::Form {
        conversation: "room".into(),
        form: form(&url),
    });
    let frame = roomkit_tui::render::screen(
        &session,
        100,
        30,
        &roomkit_tui::render::Renderers::default(),
    );
    assert!(frame.spans.iter().flatten().any(|span| {
        span.hyperlink
            .as_ref()
            .is_some_and(|link| link.url() == url.as_str())
    }));
}
