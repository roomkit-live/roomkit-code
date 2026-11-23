//! The composer prefix and recipient picker resolve only writable project sessions.
use super::record::{Mail, check_text};
use crate::sessions::{self, Sessions};
use roomkit::model::{EventId, RoomId};
use roomkit_tui::protocol::{
    ActorId, Choice, Command, Event, FieldKind, FormField, FormSpec, Item, Message, Request,
};
use roomkit_tui_engine::{CommandResult, SessionConfig};

struct Contact {
    room: RoomId,
    name: String,
}

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
    let contacts = contacts(app, &view).await?;
    let (target, text) = match &request.command {
        Command::SubmitForm { form, values, .. } if form.starts_with("id:") => {
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
    let event = match (resolve(&contacts, target), text) {
        (Some(c), Some(text)) => send(app, &view, c, text, request.operation.as_str()).await?,
        (Some(c), None) => compose(&view.room, c),
        (None, None) if !target.starts_with("id:") => pick(&view.room, &contacts, target)?,
        _ => {
            return Err(
                "Recipient unavailable or ambiguous. Use /message to choose an open session".into(),
            );
        }
    };
    Ok(CommandResult::Events(vec![event]))
}

fn resolve<'a>(contacts: &'a [Contact], target: &str) -> Option<&'a Contact> {
    if let Some(id) = target.strip_prefix("id:") {
        return contacts.iter().find(|c| c.room.as_str() == id);
    }
    let mut matches = contacts
        .iter()
        .filter(|c| c.room.as_str() == target || c.name == target);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn compose(room: &RoomId, c: &Contact) -> Event {
    Event::Form {
        conversation: room.as_str().into(),
        form: FormSpec::new(
            format!("id:{}", c.room),
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
        .map(|c| {
            Choice::new(format!("id:{}", c.room), &c.name).with_description(c.room.to_string())
        })
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

async fn contacts(app: &Sessions, view: &SessionConfig) -> Result<Vec<Contact>, String> {
    let rooms = sessions::active_rooms(&app.kit, &app.channel)
        .await
        .map_err(|_| "Cannot list project sessions")?;
    let mut result = Vec::new();
    for room in rooms {
        if room.id == view.room || room.organization_id != view.organization {
            continue;
        }
        if app
            .services
            .sessions
            .accepts_messages(room.id.as_str())
            .map_err(|_| "Cannot inspect session ownership")?
            && super::writable(&app.kit, &room.id, view.organization.clone()).await?
        {
            let name = sessions::name(&app.kit, &room.id)
                .await
                .unwrap_or_else(|| room.id.to_string());
            result.push(Contact {
                room: room.id,
                name,
            });
        }
    }
    Ok(result)
}

async fn send(
    app: &Sessions,
    view: &SessionConfig,
    to: &Contact,
    text: &str,
    op: &str,
) -> Result<Event, String> {
    check_text(text)?;
    let hash = crate::digest::sha256_hex(
        serde_json::to_vec(&(view.room.as_str(), app.channel.as_str(), &view.sender, op))
            .map_err(|_| "Invalid operation")?,
    );
    let id = EventId::new(format!("session-msg-{hash}")).map_err(|_| "Invalid operation")?;
    let mail = Mail {
        from: view.room.clone(),
        text: text.into(),
        kind: super::record::Kind::Message,
    };
    mail.write(&app.kit, &to.room, id.clone(), view.organization.clone())
        .await
        .map_err(|e| e.to_string())?;
    let text = format!("Message to @{}\n{text}", to.room);
    let id = format!("sent-{id}");
    crate::system_row::commit(
        &app.kit,
        &app.channel,
        crate::system_row::SystemRow {
            room: view.room.as_str(),
            id: &id,
            text: &text,
            about: ("session_recipient", serde_json::json!(to.room)),
        },
    )
    .await;
    Ok(Event::Item {
        conversation: view.room.as_str().into(),
        item: Item::Message(Message::new(id, ActorId::new("terminal"), text)),
    })
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
        };
        let contacts = [contact("a", "review"), contact("review", "review")];
        assert!(resolve(&contacts, "review").is_none());
        assert_eq!(resolve(&contacts, "id:a").unwrap().room.as_str(), "a");
        let remaining = [contact("a", "review")];
        assert!(resolve(&remaining, "id:review").is_none());
        assert_eq!(resolve(&remaining, "review").unwrap().room.as_str(), "a");
    }
}
