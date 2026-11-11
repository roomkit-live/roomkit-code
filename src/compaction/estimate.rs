//! What a compacted conversation is estimated to take, for its views to show
//! until its agent's next turn tells better (roomkit-tui RMK-1646). A
//! compaction outside a turn sends no request of the agent's: nothing
//! measures what it left, and a view kept showing the measure of the history
//! before it. The estimate is the size the compaction's line tells, kept with
//! the compaction's record: a view attached later, after a relaunch, shows
//! it too.
//!
//! It is forgotten once a turn of the agent ended after the compaction: a
//! view attached from then on reads that turn's measure from its snapshot,
//! which the estimate would replace. A turn that measured nothing ends it
//! too, as a provider that reports no usage always does: the estimate tells
//! the conversation before that turn, older with every one.

use roomkit::model::{ChannelId, RoomEvent, RoomId};
use roomkit::store::ListEventsOptions;
use roomkit::{ConversationStore, EventFilter, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{ContextUsage, Event};
use serde::{Deserialize, Serialize};

use super::{Compactions, Record};

/// How many of the agent's latest rows are read for a turn's record, which
/// its last row holds.
const LATEST_ROWS: u64 = 16;

/// The key of a turn's record that says how the turn ended: the engine
/// writes the record, with what the turn measured, on the turn's last row.
const TURN_END: &str = "loop_end_reason";

/// What a compaction left, as estimated when it was made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Estimate {
    /// The tokens the next request takes, as the compaction's line tells.
    pub tokens: u64,
    /// The provider that compacted, as the engine names it on a measure: a
    /// view shows an estimate for the model on screen.
    pub provider: String,
    /// The model that compacted.
    pub model: String,
    /// The index of the agent's newest row when it was made: a row of the
    /// agent's above it is of a turn that came after. A turn cut in a tool
    /// call leaves its record on the call's row, past the conversation's
    /// last message.
    pub after: u64,
}

impl Compactions {
    /// The estimate of a compaction of `agent`'s conversation in `room`
    /// that leaves `tokens`, made by `model` of `provider`. `newest` is the
    /// index of the newest event the compaction read, which stands for the
    /// agent's newest row when the store cannot be read.
    pub(super) async fn estimated(
        &self,
        (room, agent): (&str, &str),
        tokens: usize,
        (provider, model): (&str, &str),
        newest: u64,
    ) -> Estimate {
        let rows = match (self.journal.kit(), RoomId::new(room), ChannelId::new(agent)) {
            (Some(kit), Ok(room), Ok(agent)) => latest(kit, &room, &agent).await,
            _ => None,
        };
        let after = rows.map(|rows| rows.first().map_or(0, |row| row.index));
        Estimate {
            tokens: u64::try_from(tokens).unwrap_or(u64::MAX),
            provider: provider.to_owned(),
            model: model.to_owned(),
            after: after.unwrap_or(newest),
        }
    }

    /// What the views of `agent`'s conversation in `room` show as its
    /// context, as its latest compaction left it: none once a turn of the
    /// agent ended after it, or when no compaction estimated it.
    pub async fn estimate(&self, room: &RoomId, agent: &ChannelId) -> Option<Event> {
        let estimate = self.current(room, agent).await?;
        Some(Event::ContextEstimated {
            conversation: room.as_str().into(),
            provider: estimate.provider,
            model: estimate.model,
            usage: ContextUsage::new(estimate.tokens),
        })
    }

    /// A turn of `agent`'s in `room` finished: the estimate a compaction
    /// left before it is forgotten.
    pub async fn turn_told(&self, room: &RoomId, agent: &ChannelId) {
        let _ = self.current(room, agent).await;
    }

    /// The estimate that still stands for `agent`'s conversation in `room`,
    /// forgotten here once a turn of the agent ended after it.
    async fn current(&self, room: &RoomId, agent: &ChannelId) -> Option<Estimate> {
        let estimate = self.get(room.as_str(), agent.as_str())?.estimate?;
        // A store that cannot be read says nothing: no view is told an
        // estimate that a measure may have replaced.
        let rows = latest(self.journal.kit()?, room, agent).await?;
        let ended = |row: &RoomEvent| row.metadata.contains_key(TURN_END);
        if rows
            .iter()
            .any(|row| row.index > estimate.after && ended(row))
        {
            self.forget(room.as_str(), agent.as_str(), &estimate);
            return None;
        }
        Some(estimate)
    }

    /// Forgets `estimate` of `agent`'s conversation in `room`, on disk too.
    /// A compaction made meanwhile keeps its own.
    fn forget(&self, room: &str, agent: &str, estimate: &Estimate) {
        let record = {
            let mut rooms = crate::sync::lock(&self.rooms);
            let Some(record) = rooms.get_mut(room) else {
                return;
            };
            match record.agents.get_mut(agent) {
                Some(compacted) if compacted.estimate.as_ref() == Some(estimate) => {
                    compacted.estimate = None;
                }
                _ => return,
            }
            Record {
                agents: record.agents.clone(),
            }
        };
        // Forgotten for this launch if not on disk: the next one reads the
        // agent's rows again.
        let _ = self.write(room, &record);
    }
}

/// `agent`'s latest rows in `room`, newest first; none when the store
/// cannot be read.
async fn latest(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    agent: &ChannelId,
) -> Option<Vec<RoomEvent>> {
    let latest = ListEventsOptions {
        limit: LATEST_ROWS,
        newest_first: true,
        event_filter: Some(EventFilter {
            source_channel_id: Some(agent.clone()),
            include_blocked: true,
            ..EventFilter::default()
        }),
        ..ListEventsOptions::default()
    };
    let mut rows = kit.store().list_events(room, latest).await.ok()?;
    rows.sort_by_key(|row| std::cmp::Reverse(row.index));
    Some(rows)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::super::Compacted;
    use super::*;
    use std::time::Duration;

    fn compacted(tokens: u64) -> Compacted {
        Compacted {
            from: 4,
            summary: "The user asked for tests.".into(),
            snapshot: None,
            summarized: 2,
            at: 9,
            recorded: None,
            estimate: Some(Estimate {
                tokens,
                provider: "OpenAIAIProvider".into(),
                model: "fixture-model".into(),
                after: 9,
            }),
        }
    }

    /// An estimate is forgotten on disk too, and only the one a turn
    /// outdated: a compaction made meanwhile keeps its own.
    #[tokio::test]
    async fn an_estimate_is_forgotten_on_disk_unless_a_compaction_replaced_it() {
        let dir = tempfile::tempdir().unwrap();
        let compactions = Compactions::new(dir.path());
        let told = ((1, 1), Duration::ZERO, false);
        let first = compacted(40_000);
        compactions
            .record(("room", "code"), (first.clone(), 2), told)
            .await;
        // A second compaction, recorded before the first one's is forgotten.
        let second = compacted(30_000);
        compactions
            .record(("room", "code"), (second.clone(), 2), told)
            .await;
        compactions.forget("room", "code", first.estimate.as_ref().unwrap());
        assert_eq!(compactions.get("room", "code"), Some(second.clone()));

        compactions.forget("room", "code", second.estimate.as_ref().unwrap());
        let kept = Compactions::new(dir.path()).get("room", "code").unwrap();
        assert_eq!(kept.estimate, None);
        assert_eq!(kept.summary, second.summary);
    }

    /// A record written before compactions kept an estimate is read as one
    /// with none.
    #[test]
    fn a_record_without_an_estimate_is_read() {
        let stored = r#"{"from":4,"summary":"s","snapshot":null,"summarized":2,"at":9}"#;
        let compacted: Compacted = serde_json::from_str(stored).unwrap();
        assert_eq!(compacted.estimate, None);
    }
}
