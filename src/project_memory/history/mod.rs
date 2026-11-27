//! Read visible messages from the workspace's RoomKit store on explicit recall.
use roomkit::{
    ConversationStore,
    model::{ChannelId, EventType, Room, RoomContext, RoomEvent, RoomId, RoomStatus},
};
use roomkit_ai::ToolError;
use roomkit_providers_sqlite::SqliteStore;

mod passage;
mod search;

#[derive(Clone)]
pub(crate) struct History {
    store: SqliteStore,
    agent: ChannelId,
    terminal: ChannelId,
}

impl History {
    pub fn for_agent(store: Option<&SqliteStore>, channel: &str) -> Option<Self> {
        if channel == crate::names::REMOTE_AGENT {
            return None;
        }
        Some(Self {
            store: store?.clone(),
            agent: ChannelId::new(channel).ok()?,
            terminal: ChannelId::new(crate::names::TERMINAL).ok()?,
        })
    }

    pub(super) async fn lookup(
        &self,
        query: Option<&str>,
        reference: Option<&str>,
        allowance: usize,
    ) -> Result<String, ToolError> {
        let room = roomkit_ai::current_tool_room().ok_or_else(unavailable)?;
        if self.readable(&room.id, &room).await?.is_none() {
            return Err(unavailable());
        }
        match (query, reference) {
            (Some(query), None)
                if query.len() <= 512 && query.chars().any(char::is_alphanumeric) =>
            {
                self.search(&room, query.trim(), allowance).await
            }
            (None, Some(reference)) => self.passage(&room, reference, allowance).await,
            _ => Err(ToolError::refused(
                "Use history with either a short topic query or a session#message reference.",
            )),
        }
    }

    async fn readable(&self, id: &RoomId, caller: &Room) -> Result<Option<RoomContext>, ToolError> {
        let (room, bindings, _) = self
            .store
            .load_room_context(id)
            .await
            .map_err(|e| ToolError::raised(&e))?;
        let Some(room) = room else { return Ok(None) };
        let can_read = |id: &ChannelId| {
            bindings
                .iter()
                .any(|b| &b.channel_id == id && b.access.reads())
        };
        if room.organization_id != caller.organization_id
            || room.status != RoomStatus::Active
            || room.metadata.contains_key("parent_room_id")
            || !can_read(&self.agent)
            || !can_read(&self.terminal)
        {
            return Ok(None);
        }
        let mut context = RoomContext::new(room);
        context.bindings = bindings;
        Ok(Some(context))
    }

    /// A message must reach both the person using the terminal and the agent
    /// that will send the excerpt to its model. RoomKit also withholds deletions.
    fn messages(&self, context: &mut RoomContext, events: Vec<RoomEvent>) -> Vec<Message> {
        context.recent_events = events;
        context.recent_events = roomkit::visible_events(context, &self.terminal);
        let shown = roomkit::visible_events(context, &self.agent);
        context.recent_events.clear();
        shown
            .into_iter()
            .filter(|e| e.r#type == EventType::Message)
            .filter_map(|e| {
                let text = crate::hooks::prompt_text(&e.content);
                (!text.trim().is_empty()).then(|| Message {
                    reference: format!("{}#{}", e.room_id, e.index),
                    index: e.index,
                    author: e.source.channel_id,
                    text,
                })
            })
            .collect()
    }
}

fn unavailable() -> ToolError {
    ToolError::refused("Conversation reference unavailable in this session.")
}

struct Message {
    reference: String,
    index: u64,
    author: ChannelId,
    text: String,
}

impl Message {
    fn line(&self, text: &str) -> String {
        format!("{} [{}] {}", self.reference, self.author, text)
    }
}

#[cfg(test)]
mod tests;
