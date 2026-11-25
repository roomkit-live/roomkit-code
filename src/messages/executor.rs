//! User actions on the pool reuse RoomKit delegation and the ordinary task room.
use crate::{
    sessions::{Sessions, task_rooms},
    system_row::SystemRow,
};
use roomkit::model::{ChannelId, EventContent, Metadata, RoomEvent, TextContent};
use roomkit::{CommitEventOptions, ConversationStore, DelegateOptions, UpdateEventOptions};
use roomkit_tui::protocol::{
    ActorId, Event, FieldKind, FormField, FormSpec, Item, Message, Request, RowPosition,
};
use roomkit_tui_engine::{CommandResult, SessionConfig};
use serde_json::json;

pub(super) async fn handle(
    app: &Sessions,
    view: &SessionConfig,
    address: &str,
    text: Option<&str>,
    request: &Request,
) -> Result<CommandResult, String> {
    let pool = app
        .profiles
        .active_pool()
        .ok_or("Executor scheduling is off in this workspace")?;
    if let Some(id) = address.strip_prefix("task:") {
        if text.is_some_and(|s| !s.trim().is_empty()) {
            return Err(
                "Choose the running task first, then send your message in its conversation".into(),
            );
        }
        let task = app
            .profiles
            .resources
            .tasks
            .find(view.room.as_str(), id)
            .filter(|t| t.running() && pool.task_choice(t).is_some())
            .ok_or("This executor task is no longer running in this conversation")?;
        return task_rooms::open_task(app, view, &task).await;
    }
    let context = pool.target_context(address)?;
    let choice = pool
        .profile_choices()
        .await?
        .into_iter()
        .find(|c| c.value == address)
        .ok_or("The selected executor or profile is no longer available")?;
    let Some(text) = text else {
        return Ok(CommandResult::Events(vec![Event::Form {
            conversation: view.room.as_str().into(),
            form: FormSpec::new(
                address,
                "message",
                choice.label,
                vec![FormField::new("text", "Task", FieldKind::Multiline)],
            ),
        }]));
    };
    super::record::check_text(text)?;
    start(app, view, request, address, &choice.label, context, text)
        .await
        .map_err(|e| e.to_string())
}

async fn start(
    app: &Sessions,
    view: &SessionConfig,
    request: &Request,
    address: &str,
    label: &str,
    context: Metadata,
    text: &str,
) -> crate::Result<CommandResult> {
    let agent = ChannelId::new(crate::names::REMOTE_AGENT)?;
    if let Some(error) = app
        .profiles
        .resources
        .roster
        .refusal(view.agent.as_str(), agent.as_str())
    {
        return Err(error.into());
    }
    let row = match reserve(app, view, request, address, label, text).await? {
        Reservation::New(row) => row,
        Reservation::AlreadyRecorded(row) => return Ok(show(&row)),
    };
    let delegated = app
        .kit
        .delegate(
            &view.room,
            &agent,
            text,
            DelegateOptions {
                context: Some(context),
                share_channels: Some(vec![app.channel.clone()]),
                notify: Some(view.agent.clone()),
                organization_id: view.organization.clone(),
                max_result_chars: crate::delegation::MAX_RESULT_CHARS,
                ..Default::default()
            },
        )
        .await;
    let outcome = match delegated {
        Ok(task) => format!("Task {} submitted", task.id),
        Err(error) => format!("Could not start task: {error}"),
    };
    finish(app, view, &row, label, text, &outcome).await
}

enum Reservation {
    New(RoomEvent),
    AlreadyRecorded(RoomEvent),
}

async fn reserve(
    app: &Sessions,
    view: &SessionConfig,
    request: &Request,
    address: &str,
    label: &str,
    text: &str,
) -> crate::Result<Reservation> {
    // Reserve the operation before creating the task. A retried form or a crash
    // after admission must never create a second task.
    let key = crate::digest::sha256_hex(serde_json::to_vec(&(
        &view.room,
        &view.sender,
        &request.operation,
    ))?);
    let id = format!("executor-request-{key}");
    let action = json!({"address":address, "text":text});
    let mut row = SystemRow {
        room: view.room.as_str(),
        id: &id,
        text: &format!("{label}\n{text}\nSubmission unconfirmed. Check /tasks before retrying."),
        about: ("executor_request", action.clone()),
    }
    .event(&app.channel)?;
    row.idempotency_key = Some(id);
    let options = CommitEventOptions {
        organization_id: view.organization.clone(),
    };
    match app.kit.commit_event(&view.room, row.clone(), options).await {
        Ok(saved) => Ok(Reservation::New(saved)),
        Err(error) => {
            let saved = app.kit.store().get_event(&row.id).await?;
            if let Some(saved) = saved.filter(|s| {
                s.room_id == view.room
                    && s.source == row.source
                    && s.metadata.get("executor_request") == Some(&action)
            }) {
                return Ok(Reservation::AlreadyRecorded(saved));
            }
            Err(error.into())
        }
    }
}

async fn finish(
    app: &Sessions,
    view: &SessionConfig,
    row: &RoomEvent,
    label: &str,
    text: &str,
    outcome: &str,
) -> crate::Result<CommandResult> {
    let final_text = format!("{label}\n{text}\n{outcome}");
    let options = UpdateEventOptions {
        organization_id: view.organization.clone(),
        content: Some(TextContent::new(final_text).into()),
        ..Default::default()
    };
    match app.kit.update_event(&view.room, &row.id, options).await {
        Ok(Some(saved)) => Ok(show(&saved)),
        _ => Err(format!(
            "{outcome}. Could not update the local receipt. Check /tasks before retrying."
        )
        .into()),
    }
}

fn show(row: &RoomEvent) -> CommandResult {
    let text = match &row.content {
        EventContent::Text(c) => c.body.as_str(),
        _ => "Check /tasks for the submission state",
    };
    let mut message = Message::new(
        row.id.to_string(),
        ActorId::new(crate::names::TERMINAL),
        text,
    );
    message.position = Some(RowPosition::stored(row.index, None));
    CommandResult::Events(vec![Event::Item {
        conversation: row.room_id.as_str().into(),
        item: Item::Message(message),
    }])
}
