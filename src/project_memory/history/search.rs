//! Rank candidate messages with the store's full-text search. Audit events
//! can also match its index, so widen the candidate page before giving up.
use super::*;
use roomkit_providers_sqlite::SearchEventsOptions;
use std::collections::{HashMap, hash_map::Entry};

const RESULTS: usize = 4;
const MAX_CANDIDATES: usize = 512;

impl History {
    pub(super) async fn search(
        &self,
        room: &Room,
        query: &str,
        allowance: usize,
    ) -> Result<String, ToolError> {
        let mut rooms = HashMap::new();
        let matches = self.matches(room, query, &mut rooms).await?;
        if !matches.is_empty() {
            return Ok(excerpts(&matches, query, allowance));
        }
        // Natural-language queries often add a generic word absent from the exchange.
        // Keep at least two words and tell the reader which subset matched.
        let terms: Vec<_> = query.split_whitespace().collect();
        if (3..=5).contains(&terms.len()) {
            for omit in (0..terms.len()).rev() {
                let words: Vec<_> = terms
                    .iter()
                    .enumerate()
                    .filter_map(|(i, t)| (i != omit).then_some(*t))
                    .collect();
                let subset = words.join(" ");
                let matches = self.matches(room, &subset, &mut rooms).await?;
                if !matches.is_empty() {
                    return Ok(format!(
                        "Partial match ({subset})\n{}",
                        excerpts(&matches, &subset, allowance)
                    ));
                }
            }
        }
        Ok("No matching visible conversation messages in the bounded search. Try a more distinctive topic word.".into())
    }

    async fn matches(
        &self,
        caller: &Room,
        query: &str,
        rooms: &mut HashMap<RoomId, Option<RoomContext>>,
    ) -> Result<Vec<Message>, ToolError> {
        let mut limit = 32;
        loop {
            let candidates = self
                .store
                .search_events(
                    query,
                    SearchEventsOptions {
                        room_id: None,
                        limit: limit as u64,
                    },
                )
                .await
                .map_err(|e| ToolError::raised(&e))?;
            let full = candidates.len() == limit;
            let mut matches = Vec::new();
            for event in candidates {
                if event.r#type != EventType::Message || event.room_id == caller.id {
                    continue;
                }
                let context = match rooms.entry(event.room_id.clone()) {
                    Entry::Vacant(entry) => {
                        let context = self.readable(entry.key(), caller).await?;
                        entry.insert(context)
                    }
                    Entry::Occupied(entry) => entry.into_mut(),
                };
                if let Some(context) = context {
                    matches.extend(self.messages(context, vec![event]));
                }
                if matches.len() == RESULTS {
                    break;
                }
            }
            if matches.len() == RESULTS || !full || limit == MAX_CANDIDATES {
                return Ok(matches);
            }
            limit *= 4;
        }
    }
}

fn excerpts(messages: &[Message], query: &str, allowance: usize) -> String {
    let per_message = (allowance.saturating_mul(4) / messages.len()).min(220);
    let words: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let excerpts = messages
        .iter()
        .map(|m| {
            // Start near the match, including when a long message opens on another topic.
            let mut position = 0;
            if let Some(word) = m.text.split_whitespace().find(|word| {
                let lower = word.to_lowercase();
                words.iter().any(|term| lower.contains(term))
            }) {
                position = m.text.find(word).unwrap_or(0);
            }
            let start = m.text[..position]
                .char_indices()
                .rev()
                .nth(36)
                .map_or(0, |(i, _)| i);
            let snippet = super::super::view::excerpt(&m.text[start..], per_message / 4);
            m.line(&snippet)
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{excerpts}\nRead the surrounding exchange with action read and reference {}.",
        messages[0].reference
    )
}
