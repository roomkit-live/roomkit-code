//! The session created by a delegation belongs to the parent that launched it.
use super::{
    record::{Kind, Mail},
    request::origin,
};
use roomkit::{
    ConversationStore, RoomKit,
    model::{Room, RoomId, RoomStatus},
    store::FindRoomsOptions,
};
use roomkit_ai::ToolError;
use roomkit_sqlite::SqliteStore;
use serde_json::{Value, json};

type Kit = RoomKit<SqliteStore>;

fn reachable(parent: &Room, child: &Room) -> bool {
    child.status == RoomStatus::Active
        && child.organization_id == parent.organization_id
        && child.metadata.get("parent_room_id") == Some(&json!(parent.id))
        && child.metadata.get("task_agent_id") == Some(&json!(crate::remote::AGENT))
}

pub(super) async fn contacts(kit: &Kit, parent: &Room) -> crate::Result<Vec<Value>> {
    let mut result = Vec::new();
    let mut options = FindRoomsOptions {
        organization_id: parent.organization_id.clone(),
        metadata_filter: Some(
            [
                ("parent_room_id".into(), json!(parent.id)),
                ("task_agent_id".into(), json!(crate::remote::AGENT)),
            ]
            .into_iter()
            .collect(),
        ),
        status: Some(RoomStatus::Active),
        ..Default::default()
    };
    loop {
        let rooms = kit.store().find_rooms(options.clone()).await?;
        let count = rooms.len() as u64;
        for room in rooms.iter().filter(|child| reachable(parent, child)) {
            let title: String = room
                .metadata
                .get("task_input")
                .and_then(Value::as_str)
                .unwrap_or(room.id.as_str())
                .chars()
                .take(100)
                .collect();
            result.push(
                json!({"name":title,"to":room.id,"task_status":room.metadata.get("task_status")}),
            );
        }
        if count < options.limit {
            return Ok(result);
        }
        options.offset += count;
    }
}

pub(super) async fn send(
    kit: &Kit,
    parent: &Room,
    child: Room,
    text: String,
) -> Result<Value, ToolError> {
    let fail =
        || ToolError::refused("Session unavailable. Choose an exact address from list_sessions.");
    if !super::writable(kit, &child.id, parent.organization_id.clone())
        .await
        .map_err(ToolError::refused)?
    {
        return Err(fail());
    }
    let (id, request) = origin(&parent.id, parent.id.as_str(), child.id.as_str(), &text)?;
    let mail = Mail {
        from: parent.id.clone(),
        text,
        remote: None,
        kind: Kind::ToSession {
            to: child.id.clone(),
            agent: request.agent,
            turn: request.turn,
        },
    };
    // The parent already owns a durable inbox. Its receiver admits the child
    // post independently, including when the parent was closed after this receipt.
    mail.write(kit, &parent.id, id.clone(), parent.organization_id.clone())
        .await
        .map_err(|e| ToolError::refused(e.to_string()))?;
    Ok(
        json!({"status":"sent","message_id":id,"to":child.id,"delivery":"Continue working. The reply or delivery notice arrives asynchronously."}),
    )
}

pub(super) async fn destination(
    kit: &Kit,
    parent: &Room,
    id: &RoomId,
) -> crate::Result<Option<Room>> {
    Ok(kit
        .store()
        .get_room(id)
        .await?
        .filter(|c| reachable(parent, c)))
}

/// Steer only the original delegated turn. Once it has settled, use a fresh,
/// addressed conversation turn so private follow-ups are never steered.
pub(super) async fn prepare(
    messages: &super::Messages,
    kit: &Kit,
    event: &roomkit::model::RoomEvent,
    mail: &Mail,
    target: &RoomId,
    diagnostics: &std::sync::Arc<crate::diagnostics::Diagnostics>,
) -> crate::Result<Option<super::dispatch::PendingPost>> {
    use super::{
        delivery::{notice, question_prompt},
        dispatch::{Checkpoint, PendingPost},
    };
    let parent = kit.get_room(&event.room_id, Default::default()).await?;
    let Some(child) = destination(kit, &parent, target).await? else {
        notice(
            None,
            kit,
            event,
            mail,
            "This session is unavailable for correspondence.",
        )
        .await?;
        return Ok(None);
    };
    if messages.pool().is_none()
        || !super::writable(kit, target, child.organization_id.clone()).await?
    {
        notice(
            None,
            kit,
            event,
            mail,
            "This session is unavailable for correspondence.",
        )
        .await?;
        return Ok(None);
    }
    if matches!(
        child.metadata.get("task_status").and_then(Value::as_str),
        Some("pending" | "in_progress")
    ) {
        return steer_active(messages, kit, event, mail, &child, diagnostics)
            .await
            .map(|()| None);
    }
    let Some(target) = crate::remote::pool::session_placement(kit, &child, &parent).await? else {
        notice(
            None,
            kit,
            event,
            mail,
            "The original executor placement is unavailable for this session.",
        )
        .await?;
        return Ok(None);
    };
    let post = PendingPost {
        agent: roomkit::model::ChannelId::new(crate::remote::AGENT)?,
        text: question_prompt(event, mail),
        executor_target: Some(target),
        organization: child.organization_id,
    };
    Checkpoint::Post(post.clone()).save(kit, event).await?;
    Ok(Some(post))
}

async fn steer_active(
    messages: &super::Messages,
    kit: &Kit,
    event: &roomkit::model::RoomEvent,
    mail: &Mail,
    child: &Room,
    diagnostics: &std::sync::Arc<crate::diagnostics::Diagnostics>,
) -> crate::Result<()> {
    use super::{
        delivery::{notice, question_prompt},
        dispatch::Checkpoint,
    };
    let task = child
        .metadata
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some((turn, closed)) = messages
        .pool()
        .and_then(|p| p.delegated_turn(&event.room_id, &child.id, task))
    else {
        let reason = "The task is waiting for its worker or handing back. Try again once it is running or finished.";
        return notice(None, kit, event, mail, reason).await;
    };
    Checkpoint::Steering.save(kit, event).await?;
    let directive = roomkit::model::InjectMessage {
        role: "assistant".into(),
        content: question_prompt(event, mail),
    };
    let options = roomkit::SteerOptions {
        turn_id: Some(turn),
        organization_id: child.organization_id.clone(),
    };
    if kit
        .steer(
            &child.id,
            &roomkit::model::ChannelId::new(crate::remote::AGENT)?,
            directive,
            options,
        )
        .await?
        == 0
    {
        return notice(
            None,
            kit,
            event,
            mail,
            "The worker finished before it received this message.",
        )
        .await;
    }
    Checkpoint::Delivered
        .save_known(kit, event, diagnostics)
        .await;
    let (owner, row, question, stop) = (
        kit.clone(),
        event.clone(),
        mail.clone(),
        messages.stop.clone(),
    );
    let diagnostics = diagnostics.clone();
    messages.workers.spawn(async move {
        let confirm = async {
            closed.cancelled().await;
            while let Err(error) =
                super::delivery::after_post(None, &owner, &row, &question, false).await
            {
                super::receiver::report_unavailable(&diagnostics, error.as_ref()).await;
            }
        };
        tokio::select! { () = stop.cancelled() => (), () = confirm => () }
    });
    Ok(())
}
