//! The composer prefix and recipient picker resolve only writable project sessions.
use super::directory::{Contact, contacts, resolve};
use super::record::{Mail, check_text};
use crate::sessions::Sessions;
use roomkit::model::{EventId, RoomId};
use roomkit_tui::protocol::{Choice, Command, Event, FieldKind, FormField, FormSpec, Request};
use roomkit_tui_engine::{CommandResult, SessionConfig};

pub(crate) async fn handle(
    app: &Sessions,
    view: SessionConfig,
    request: Request,
) -> Result<CommandResult, String> {
    if view.group.is_some()
        || !app.services.sessions.owns(view.room.as_str())
        || !super::writable(&app.kit, &view.room, view.organization.clone()).await?
    {
        return Err("Send session messages from a writable project conversation".into());
    }
    let (target, text) = match &request.command {
        Command::SubmitForm { form, values, .. }
            if form.starts_with("id:") || form.starts_with("peer:") =>
        {
            (form.as_str(), Some(values.get("text").unwrap_or("")))
        }
        Command::Invoke { arguments, .. } => arguments
            .trim()
            .split_once(char::is_whitespace)
            .map_or((arguments.trim(), None), |(to, text)| {
                (to, Some(text.trim()))
            }),
        _ => return Err("Invalid session message".into()),
    };
    let contacts = contacts(app, &view, target.starts_with("id:")).await?;
    let event = match (resolve(&contacts, target), text) {
        (Some(c), Some(text)) => send(app, &view, c, text, request.operation.as_str()).await?,
        (Some(c), None) => compose(&view.room, c),
        (None, None) if !target.starts_with("id:") && !target.starts_with("peer:") => {
            pick(&view.room, &contacts, target)?
        }
        _ => {
            return Err(
                "Recipient unavailable or ambiguous. Use /message to choose an open session".into(),
            );
        }
    };
    Ok(CommandResult::Events(vec![event]))
}

fn compose(room: &RoomId, c: &Contact) -> Event {
    Event::Form {
        conversation: room.as_str().into(),
        form: FormSpec::new(
            c.key(),
            "message",
            format!("Message to {}", c.name),
            vec![FormField::new("text", "Message", FieldKind::Multiline)],
        ),
    }
}

fn pick(room: &RoomId, contacts: &[Contact], query: &str) -> Result<Event, String> {
    let query = query.to_lowercase();
    let choices: Vec<_> = contacts
        .iter()
        .filter(|c| {
            c.name.to_lowercase().contains(&query)
                || c.room.as_str().to_lowercase().contains(&query)
        })
        .map(|c| Choice::new(c.key(), &c.name).with_description(c.description()))
        .collect();
    if choices.is_empty() {
        return Err("No matching open session in this project".into());
    }
    Ok(Event::Choices {
        conversation: room.as_str().into(),
        command: "message".into(),
        title: "Message an open session".into(),
        choices,
    })
}

async fn send(
    app: &Sessions,
    view: &SessionConfig,
    to: &Contact,
    text: &str,
    op: &str,
) -> Result<Event, String> {
    check_text(text)?;
    let net = app.profiles.resources.messages.network.get();
    let route = to
        .remote
        .as_ref()
        .map(|peer| net.map(|n| (n, peer)).ok_or("Session network unavailable"))
        .transpose()?;
    let endpoints = route.map(|(net, peer)| (&net.channel, peer));
    let id = message_id(app, view, endpoints, op)?;
    let outgoing =
        super::outgoing::Outgoing::record(&app.kit, view, &app.channel, &to.room, &id, text)
            .await
            .map_err(|_| "Could not save the outgoing message. It was not sent.")?;
    if outgoing.confirmed() {
        return Ok(outgoing.item());
    }
    let mail = Mail {
        from: view.room.clone(),
        text: text.into(),
        remote: None,
        kind: super::record::Kind::Message,
    };
    if let Some((net, peer)) = route {
        net.request(&app.kit, peer, &id, &mail, None).await
    } else {
        mail.write(&app.kit, &to.room, id.clone(), view.organization.clone())
            .await
    }
    .map_err(|e| e.to_string())?;
    outgoing.confirm(&app.kit, view.organization.clone()).await
        .map_err(|_| "The recipient accepted the message, but its local receipt could not be saved. Check the conversation before resending.")?;
    Ok(outgoing.item())
}

fn message_id(
    app: &Sessions,
    view: &SessionConfig,
    endpoints: Option<(&roomkit::model::ChannelId, &roomkit::model::ChannelId)>,
    op: &str,
) -> Result<EventId, String> {
    let operation = (view.room.as_str(), app.channel.as_str(), &view.sender, op);
    let identity = match endpoints {
        Some((from, to)) => serde_json::to_vec(&(from, to, operation)),
        None => serde_json::to_vec(&operation),
    }
    .map_err(|_| "Invalid operation")?;
    let hash = crate::digest::sha256_hex(identity);
    EventId::new(format!("session-msg-{hash}")).map_err(|_| "Invalid operation".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[expect(clippy::unwrap_used, reason = "the test uses valid room IDs")]
    fn picker_ids_remain_unambiguous_after_a_rename_or_disconnect() {
        let contact = |room: &str, name: &str| Contact {
            room: RoomId::new(room).unwrap(),
            name: name.into(),
            remote: None,
        };
        let contacts = [contact("a", "review"), contact("review", "review")];
        assert!(resolve(&contacts, "review").is_none());
        assert_eq!(resolve(&contacts, "id:a").unwrap().room.as_str(), "a");
        let remaining = [contact("a", "review")];
        assert!(resolve(&remaining, "id:review").is_none());
        assert_eq!(resolve(&remaining, "review").unwrap().room.as_str(), "a");
    }
}
