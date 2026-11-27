//! The same bounded explanation for the originating conversation and diagnostics panel.
use std::{collections::VecDeque, time::Instant};

use roomkit_tui::protocol::{Event, Item, Message};
use tokio::sync::watch;

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
