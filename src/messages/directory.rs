//! Combine live project sessions and the native catalogue of the selected group.
use super::network::{ADDRESS, Network};
use crate::sessions::{self, Sessions};
use roomkit::{
    ConversationStore,
    model::{ChannelId, RoomId},
};
use roomkit_tui_engine::SessionConfig;
use std::collections::BTreeSet;

pub(super) struct Contact {
    pub room: RoomId,
    pub name: String,
    pub remote: Option<ChannelId>,
}
impl Contact {
    pub fn key(&self) -> String {
        self.remote
            .as_ref()
            .map_or_else(|| format!("id:{}", self.room), |ch| format!("peer:{ch}"))
    }
    pub fn description(&self) -> String {
        self.remote.as_ref().map_or_else(
            || self.room.to_string(),
            |ch| format!("{} · network · {}", self.room, ch),
        )
    }
}

pub(super) async fn contacts(
    app: &Sessions,
    view: &SessionConfig,
    local_only: bool,
) -> Result<Vec<Contact>, String> {
    let net = app
        .profiles
        .resources
        .messages
        .network
        .get()
        .filter(|net| !local_only && net.room == view.room);
    let mut result = Vec::new();
    let mut local_addresses = BTreeSet::new();
    for room in sessions::active_rooms(&app.kit, &app.channel)
        .await
        .map_err(|_| "Cannot list project sessions")?
    {
        if room.id == view.room || room.organization_id != view.organization {
            continue;
        }
        if !app
            .services
            .sessions
            .accepts_messages(room.id.as_str())
            .map_err(|_| "Cannot inspect session ownership")?
            || !super::writable(&app.kit, &room.id, view.organization.clone()).await?
        {
            continue;
        }
        if net.is_some()
            && let Some(address) = local_address(app, &room.id).await?
        {
            local_addresses.insert(address);
        }
        let name = sessions::name(&app.kit, &room.id)
            .await
            .unwrap_or_else(|| room.id.to_string());
        result.push(Contact {
            room: room.id,
            name,
            remote: None,
        });
    }
    if let Some(net) = net {
        extend_network(net, local_addresses, &mut result).await?;
    }
    Ok(result)
}

async fn local_address(app: &Sessions, room: &RoomId) -> Result<Option<String>, String> {
    Ok(app
        .kit
        .store()
        .get_private_state(room, ADDRESS)
        .await
        .map_err(|_| "Cannot inspect session address")?
        .and_then(|v| v.get("channel")?.as_str().map(str::to_owned)))
}

async fn extend_network(
    net: &Network,
    local: BTreeSet<String>,
    contacts: &mut Vec<Contact>,
) -> Result<(), String> {
    let peers = group_contacts(net).await.map_err(
        |_| "Cannot list the session group. Use /message id:SESSION for an exact local recipient.",
    )?;
    contacts.extend(peers.into_iter().filter(|contact| {
        contact
            .remote
            .as_ref()
            .is_none_or(|id| !local.contains(id.as_str()))
    }));
    Ok(())
}

pub(super) async fn group_contacts(net: &Network) -> Result<Vec<Contact>, String> {
    Ok(net
        .contacts()
        .await
        .map_err(|_| "Cannot list the session group")?
        .into_iter()
        .map(|(id, room)| Contact {
            name: room.to_string(),
            room,
            remote: Some(id),
        })
        .collect())
}

pub(super) fn resolve<'a>(contacts: &'a [Contact], target: &str) -> Option<&'a Contact> {
    if target.starts_with("id:") || target.starts_with("peer:") {
        return contacts.iter().find(|c| c.key() == target);
    }
    let mut matches = contacts
        .iter()
        .filter(|c| c.room.as_str() == target || c.name == target);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}
