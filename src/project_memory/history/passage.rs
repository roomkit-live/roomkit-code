//! Resolve a citation again before revealing the conversation around it.
use super::*;
use roomkit::EventFilter;
use roomkit::store::{GetConversationOptions, ListEventsOptions};

const NEIGHBORS: usize = 2;
const PAGE: u64 = 32;

impl History {
    pub(super) async fn passage(
        &self,
        caller: &Room,
        reference: &str,
        allowance: usize,
    ) -> Result<String, ToolError> {
        let (room, index) = reference.rsplit_once('#').ok_or_else(unavailable)?;
        let room = RoomId::new(room).map_err(|_| unavailable())?;
        let index: u64 = index.parse().map_err(|_| unavailable())?;
        if room == caller.id || index >= i64::MAX as u64 {
            return Err(unavailable());
        }
        let mut context = self
            .readable(&room, caller)
            .await?
            .ok_or_else(unavailable)?;
        let events = self.before(&room, index + 1, 1).await?;
        let anchor = self
            .messages(&mut context, events)
            .pop()
            .filter(|m| m.index == index)
            .ok_or_else(unavailable)?;
        let mut messages = self.neighbors(&mut context, index, true).await?;
        messages.push(anchor);
        messages.extend(self.neighbors(&mut context, index, false).await?);
        Ok(render(&messages, index, allowance))
    }

    async fn before(
        &self,
        room: &RoomId,
        index: u64,
        limit: u64,
    ) -> Result<Vec<RoomEvent>, ToolError> {
        // get_conversation has a forward cursor. For the preceding messages
        // use the same message projection with the store's backward cursor.
        self.store
            .list_events(
                room,
                ListEventsOptions {
                    limit,
                    before_index: Some(index),
                    event_filter: Some(EventFilter {
                        event_types: Some(vec![EventType::Message]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| ToolError::raised(&e))
    }

    async fn neighbors(
        &self,
        context: &mut RoomContext,
        mut cursor: u64,
        before: bool,
    ) -> Result<Vec<Message>, ToolError> {
        let mut found = Vec::new();
        loop {
            let page = if before {
                self.before(&context.room.id, cursor, PAGE).await?
            } else {
                self.store
                    .get_conversation(
                        &context.room.id,
                        GetConversationOptions {
                            after_index: Some(cursor),
                            limit: PAGE,
                        },
                    )
                    .await
                    .map_err(|e| ToolError::raised(&e))?
            };
            let full = page.len() == PAGE as usize;
            let edge = if before { page.first() } else { page.last() };
            let Some(edge) = edge else { break };
            cursor = edge.index;
            let mut visible = self.messages(context, page);
            if before {
                visible.reverse();
            }
            found.extend(visible.into_iter().take(NEIGHBORS - found.len()));
            if found.len() == NEIGHBORS || !full {
                break;
            }
        }
        if before {
            found.reverse();
        }
        Ok(found)
    }
}

fn render(mut messages: &[Message], anchor: u64, allowance: usize) -> String {
    use super::super::view::clip_chars;
    let space = allowance.saturating_mul(4).saturating_sub(1);
    // Keep complete citations and some body text. Drop the farthest
    // neighbors first if long session names consume a small context.
    let headers = loop {
        let headers: usize = messages
            .iter()
            .map(|m| m.line("").chars().count() + 1)
            .sum();
        if messages.len() == 1 || headers + 24 * messages.len() <= space {
            break headers;
        }
        let last = messages.len() - 1;
        messages = if anchor - messages[0].index >= messages[last].index - anchor {
            &messages[1..]
        } else {
            &messages[..last]
        };
    };
    // Short replies return their unused share to longer messages.
    let mut lengths: Vec<_> = messages.iter().map(|m| m.text.chars().count()).collect();
    lengths.sort_unstable();
    let mut remaining = space.saturating_sub(headers);
    let mut share = 0;
    for (i, length) in lengths.iter().enumerate() {
        share = remaining / (lengths.len() - i);
        if *length >= share {
            break;
        }
        remaining -= length;
    }
    messages
        .iter()
        .map(|m| m.line(&clip_chars(&m.text, share)))
        .collect::<Vec<_>>()
        .join("\n")
}
