//! Creating a session's room, completing one left part-way, and reusing an
//! empty draft.
use super::*;

pub(super) const DRAFT_MARKER: &str = "roomkit_code_draft";

// Bindings are infrastructure, not conversation history. Even blocked messages
// prevent reuse: an attempted conversation must never become another draft.
pub(crate) async fn empty(kit: &RoomKit<SqliteStore>, room: &RoomId) -> Result<bool> {
    Ok(kit
        .store()
        .get_event_count(
            room,
            GetEventCountOptions {
                event_filter: Some(EventFilter {
                    exclude_types: Some(vec![
                        EventType::ChannelAttached,
                        EventType::ChannelDetached,
                    ]),
                    include_blocked: true,
                    ..Default::default()
                }),
            },
        )
        .await?
        == 0)
}

/// The active rooms binding `channel`, oldest first.
pub(crate) async fn active_rooms(
    kit: &RoomKit<SqliteStore>,
    channel: &ChannelId,
) -> Result<Vec<Room>> {
    let store = kit.store();
    let options = FindRoomIdsByChannelOptions {
        room_filter: Some(RoomFilter {
            statuses: Some(vec![RoomStatus::Active]),
            ..Default::default()
        }),
        limit: usize::MAX,
        group: None,
    };
    let mut rooms = Vec::new();
    for id in store.find_room_ids_by_channel(channel, options).await? {
        // A delegated task's room belongs to its conversation: the terminal
        // is bound to it, but it is no session of its own.
        let room = store.get_room(&id).await?;
        rooms.extend(room.filter(|room| !room.metadata.contains_key("parent_room_id")));
    }
    Ok(rooms)
}

pub(crate) async fn create(
    kit: &RoomKit<SqliteStore>,
    leases: &crate::storage::SessionLeases,
    channel: &roomkit::model::ChannelId,
    agent: &roomkit::model::ChannelId,
    name: Option<&str>,
    draft: bool,
) -> Result<RoomId> {
    if name.is_none()
        && draft
        && let Some(id) = unused_draft(kit, leases, channel, agent).await?
    {
        // Nothing was sent in a draft: it takes the agent it is opened with,
        // and starts anew, with that agent's own model and the launch's mode.
        super::super::designate(kit, &id, agent).await?;
        super::super::forget(kit, &id).await?;
        return Ok(id);
    }
    let id = RoomId::new(name.map_or_else(
        || format!("session-{}", uuid::Uuid::new_v4()),
        str::to_owned,
    ))?;
    let claim = leases
        .claim(id.as_str())?
        .ok_or(crate::storage::SESSION_BUSY)?;
    match kit.get_room(&id, GetRoomOptions::default()).await {
        Ok(_) => {
            complete(kit, &id, channel, agent).await?;
            leases.keep(claim)?;
            return Ok(id);
        }
        Err(roomkit::RoomKitError::RoomNotFound(_)) => {}
        Err(error) => return Err(error.into()),
    }
    let room = kit
        .create_room(CreateRoomOptions {
            room_id: Some(id),
            metadata: if draft {
                [(DRAFT_MARKER.into(), serde_json::Value::Bool(true))]
                    .into_iter()
                    .collect()
            } else {
                Default::default()
            },
            ..Default::default()
        })
        .await?;
    for id in [channel, agent] {
        kit.attach_channel(&room.id, id, Default::default()).await?;
    }
    // A conversation designates its agent from its start: an existing one,
    // returned above, keeps the agent selected in it.
    super::super::designate(kit, &room.id, agent).await?;
    leases.keep(claim)?;
    Ok(room.id)
}

/// Ends the creation of `room` when an earlier launch stopped part-way: a
/// room is created, then bound to the terminal and to its agent, then
/// designates its agent, and a failure or a kill between two of these left
/// a room that no later launch could use. A room that designates an agent
/// was created whole, and keeps the agent selected in it. One bound to
/// other channels and not to the terminal is not this terminal's to take.
pub(super) async fn complete(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    channel: &ChannelId,
    agent: &ChannelId,
) -> Result<()> {
    if super::super::designated(kit, room).await?.is_some() {
        return Ok(());
    }
    let bindings = kit
        .list_bindings(room, ListBindingsOptions::default())
        .await?;
    let bound = |id: &ChannelId| bindings.iter().any(|binding| &binding.channel_id == id);
    if !bindings.is_empty() && !bound(channel) {
        return Ok(());
    }
    for id in [channel, agent] {
        if !bound(id) {
            kit.attach_channel(room, id, Default::default()).await?;
        }
    }
    super::super::designate(kit, room, agent).await
}

/// The newest empty draft both channels can still write to, claimed. The
/// claim comes before the inspection: another process may own an apparently
/// empty room or still be initializing its bindings.
pub(super) async fn unused_draft(
    kit: &RoomKit<SqliteStore>,
    leases: &crate::storage::SessionLeases,
    channel: &roomkit::model::ChannelId,
    agent: &roomkit::model::ChannelId,
) -> Result<Option<RoomId>> {
    let mut rooms = active_rooms(kit, channel).await?;
    rooms.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    for room in rooms {
        if room.organization_id.is_some()
            || room
                .metadata
                .get(DRAFT_MARKER)
                .and_then(serde_json::Value::as_bool)
                != Some(true)
        {
            continue;
        }
        let Some(claim) = leases.claim(room.id.as_str())? else {
            continue;
        };
        if !empty(kit, &room.id).await? {
            continue;
        }
        let bindings = kit
            .list_bindings(&room.id, ListBindingsOptions::default())
            .await?;
        if bindings
            .iter()
            .any(|b| b.channel_id == *channel && b.access == Access::ReadWrite)
            && bindings
                .iter()
                .any(|b| b.channel_id == *agent && b.access == Access::ReadWrite)
        {
            leases.keep(claim)?;
            return Ok(Some(room.id));
        }
    }
    Ok(None)
}
