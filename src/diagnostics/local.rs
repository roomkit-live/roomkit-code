//! The same bounded explanation for the originating conversation and diagnostics panel.
use std::{collections::VecDeque, time::Instant};

use roomkit_tui::protocol::{Event, Item, Message};
use tokio::sync::watch;

/// Format the latest retained local generation for the room that opened the panel.
pub(super) fn generation_heading(line: &str, room: &str) -> Option<String> {
    let record: serde_json::Value = serde_json::from_str(line).ok()?;
    if record["room"].as_str() != Some(room)
        || !matches!(
            record["event"].as_str(),
            Some("local_generation_completed" | "local_generation_incomplete")
        )
    {
        return None;
    }
    let output = record["output_budget"].as_u64()?;
    let thinking = match record["thinking_budget"].as_i64() {
        None => "server default".to_owned(),
        Some(0) => "disabled".to_owned(),
        Some(n) => format!("{n} tokens"),
    };
    let finish = record["finish_reason"].as_str().unwrap_or("unreported");
    Some(format!(
        "Last llama.cpp generation\nOutput budget: {output} tokens · Reasoning budget: {thinking} · Finish reason: {finish}\n\n"
    ))
}

struct Issue {
    room: String,
    id: String,
    at: Instant,
    text: String,
}

#[derive(Default)]
pub(crate) struct LocalIssues {
    issues: std::sync::Mutex<VecDeque<Issue>>,
    changed: watch::Sender<()>,
}

impl LocalIssues {
    pub(super) fn report(&self, room: &str, summary: &str) {
        {
            let mut issues = crate::sync::lock(&self.issues);
            if issues.len() == 32 {
                issues.pop_front();
            }
            issues.push_back(Issue {
                room: room.into(),
                at: Instant::now(),
                id: format!("local-generation-{}", uuid::Uuid::new_v4()),
                text: summary.into(),
            });
        }
        self.changed.send_replace(());
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }

    pub(super) fn heading(&self, room: &str) -> String {
        crate::sync::lock(&self.issues)
            .iter()
            .rev()
            .find(|i| i.room == room)
            .map(|i| format!("Last incomplete local generation\n{}\n\n", i.text))
            .unwrap_or_default()
    }

    /// Only generations observed while this view was open belong in its live transcript.
    pub(crate) fn lines(&self, room: &str, since: Instant) -> Vec<Event> {
        crate::sync::lock(&self.issues)
            .iter()
            .filter(|i| i.room == room && i.at >= since)
            .map(|i| Event::Item {
                conversation: room.into(),
                item: Item::Message(Message::new(
                    i.id.clone(),
                    crate::names::TERMINAL,
                    i.text.clone(),
                )),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generation_heading_shows_the_requested_budgets_in_its_room() {
        let mut record = serde_json::json!({"room":"test","event":"local_generation_completed","finish_reason":"stop","output_budget":4096});
        for (budget, shown) in [
            (None, "server default"),
            (Some(0), "disabled"),
            (Some(1024), "1024 tokens"),
        ] {
            record["thinking_budget"] = serde_json::json!(budget);
            let line = record.to_string();
            assert!(generation_heading(&line, "other").is_none());
            assert_eq!(
                generation_heading(&line, "test"),
                Some(format!(
                    "Last llama.cpp generation\nOutput budget: 4096 tokens · Reasoning budget: {shown} · Finish reason: stop\n\n"
                ))
            );
        }
    }

    #[test]
    fn conversations_only_receive_their_own_generation_explanation() -> crate::Result<()> {
        let issues = LocalIssues::default();
        let changes = issues.subscribe();
        let since = Instant::now();
        issues.report("first", "Output budget reached.");
        issues.report("second", "No visible output.");
        assert_eq!(changes.has_changed().ok(), Some(true));
        assert!(issues.lines("other", since).is_empty());
        assert!(issues.heading("other").is_empty());
        let lines = issues.lines("first", since);
        assert_eq!(lines, issues.lines("first", since));
        assert!(
            issues
                .lines("first", Instant::now() + std::time::Duration::from_secs(1))
                .is_empty()
        );
        let Some(Event::Item {
            conversation,
            item: Item::Message(line),
        }) = lines.first()
        else {
            return Err("missing explanation for first room".into());
        };
        assert_eq!(conversation.as_str(), "first");
        assert_eq!(line.text, "Output budget reached.");
        assert!(issues.heading("first").contains(&line.text));
        Ok(())
    }
}
