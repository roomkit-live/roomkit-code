//! The memory an agent with a known window reads its history through: the
//! room's messages since its last compaction, whole, after the summary that
//! stands for the earlier ones; a new compaction once they pass the tier, or
//! at once when `/compact` asks outside the agent's turns.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use std::{future::Future, pin::Pin};

use roomkit::ChannelError;
use roomkit::model::{EventType, RoomContext, RoomEvent, RoomId};
use roomkit_ai::AiProvider;
use roomkit_ai::memory::{
    EstimateEventTokensOptions, MemoryProvider, MemoryResult, RetrieveOptions, TurnFootprint,
    current_turn_footprint, estimate_event_tokens,
};
use serde_json::Value;

use super::history::History;
use super::{Compacted, Compactions, Compactor, WINDOW, summary, tiers};

/// Reads `agent`'s history under `ceiling` tokens, compacting it by tiers
/// with `provider`, its own model; `compactions` keeps each conversation's.
pub(crate) struct TieredMemory<P> {
    provider: P,
    ceiling: u64,
    agent: String,
    compactions: Arc<Compactions>,
    /// What the rest of the agent's latest turn in each room took besides
    /// its history, for a compaction outside its turns to estimate by.
    fixed: Mutex<HashMap<String, usize>>,
}

/// What a turn's request costs besides its summary: its history's events,
/// each with what the turn replays after it, the rest of the turn, and the
/// agent's latest request as its provider measured it, with what came since.
struct Measured {
    costs: Vec<usize>,
    fixed: usize,
    last: Option<usize>,
}

impl Measured {
    /// The request with `compacted`'s summary and the events from `from` on,
    /// as estimated.
    fn total(&self, compacted: Option<&Compacted>, from: usize) -> usize {
        let events: usize = self.costs.iter().skip(from).sum();
        self.fixed + summary::tokens(compacted) + events
    }

    /// The request as it stands: estimated, or as last measured when that
    /// says more. Estimates at four characters a token undercount code.
    fn now(&self, compacted: Option<&Compacted>) -> usize {
        self.total(compacted, 0).max(self.last.unwrap_or(0))
    }

    /// What the latest measured request took besides its history and what
    /// it replays, as no turn of this run measured it yet: the system prompt
    /// and the tools.
    fn fixed_measured(&self, compacted: Option<&Compacted>) -> usize {
        let history = self.total(compacted, 0).saturating_sub(self.fixed);
        self.last.unwrap_or(0).saturating_sub(history)
    }
}

impl<P: AiProvider> TieredMemory<P> {
    pub fn new(provider: P, (ceiling, agent): (u64, &str), compactions: Arc<Compactions>) -> Self {
        Self {
            provider,
            ceiling,
            agent: agent.into(),
            compactions,
            fixed: Mutex::default(),
        }
    }

    /// What `agent`'s turn on `current` reads of `context` in `room`: the
    /// events since its last compaction after its summary, compacted anew
    /// once they pass the tier, or when `/compact` asked. A compaction
    /// running outside the turns ends first: the turn reads what it made.
    async fn read(
        &self,
        (room, agent): (&str, &str),
        current: &RoomEvent,
        context: &RoomContext,
    ) -> MemoryResult {
        let _held = self.compactions.hold(room).await;
        let compacted = self.compactions.get(room, agent);
        let mut events: Vec<RoomEvent> = context
            .recent_events
            .iter()
            .filter(|event| event.id != current.id)
            .filter(|event| compacted.as_ref().is_none_or(|c| event.index >= c.from))
            .cloned()
            .collect();
        events.sort_by_key(|event| event.index);
        let after = compacted.as_ref().map_or(0, |c| c.at);
        let footprint = current_turn_footprint().unwrap_or_else(|| TurnFootprint::new(0, 0));
        let measured = measure(&events, (agent, after), &footprint);
        crate::sync::lock(&self.fixed).insert(room.into(), measured.fixed);
        let forced = self.compactions.take_forced(room, agent);
        // The rows of the room since the summary, the agent's own included,
        // which its context reads among the room's latest.
        let rows = current
            .index
            .saturating_sub(compacted.as_ref().map_or(0, |c| c.from));
        if !forced
            && !tiers::due(measured.now(compacted.as_ref()), self.ceiling)
            && !tiers::rows_due(rows)
        {
            return result(compacted.as_ref(), events);
        }
        let read = (room, agent);
        let to = (current.index, forced);
        match self
            .compact(read, compacted.as_ref(), &mut events, &measured, to)
            .await
        {
            Some(made) => result(Some(&made), events),
            None => result(compacted.as_ref(), events),
        }
    }

    /// Compacts `events`, the history since `compacted`, up to the room's
    /// row `newest`, leaving in it the events kept whole: the latest within
    /// the tier's budget, or the fewest when `forced`. None when nothing
    /// would be summarized, or the summary failed: the history is read as it
    /// is, and the next turn tries again.
    async fn compact(
        &self,
        (room, agent): (&str, &str),
        compacted: Option<&Compacted>,
        events: &mut Vec<RoomEvent>,
        measured: &Measured,
        (newest, forced): (u64, bool),
    ) -> Option<Compacted> {
        let cut = self.cut((events, agent), measured, (newest, forced));
        if cut == 0 {
            if forced {
                self.compactions.notice(
                    room,
                    "Nothing to compact yet: the conversation is too short",
                );
            }
            return None;
        }
        let prior = compacted.map(|c| c.summary.as_str());
        // The calls of the turns the summary stands for: a cut falls on a
        // turn's start, so they come before the first event kept whole.
        let kept_from = events.get(cut).map_or(u64::MAX, |kept| kept.index);
        let summarized_from = compacted.map_or(0, |c| c.from);
        let calls = self
            .compactions
            .calls((room, agent), summarized_from..kept_from)
            .await;
        let read = (&events[..cut], calls.as_slice(), agent);
        let chars = tiers::summary_chars(self.ceiling);
        let (_running, began) = (self.compactions.running(room), Instant::now());
        let Some(written) = summary::summarize(&self.provider, prior, read, chars).await else {
            if forced {
                self.compactions.force(room, agent);
            }
            self.compactions.notice(
                room,
                "Compaction failed: the model wrote no summary; it is tried again at the next message",
            );
            return None;
        };
        let (written, cut_short) = (written.text, written.cut);
        let newest = events.last().map_or(0, |event| event.index);
        // The messages the summary stands for, not the rows of tool calls
        // and read instructions between them.
        let summarized = events
            .drain(..cut)
            .filter(|event| event.r#type == EventType::Message)
            .count();
        let kept = events.first().map_or(0, |event| event.index);
        let mut made = self
            .made((room, agent), compacted, (written, kept, newest), measured)
            .await;
        made.summarized += summarized;
        let sizes = (measured.now(compacted), measured.total(Some(&made), cut));
        // The size after it, which its line tells, is the views' to show
        // until a turn measures the conversation again.
        let by = (self.provider.name(), self.provider.model_name());
        let left = self
            .compactions
            .estimated((room, agent), sizes.1, by, newest);
        made.estimate = Some(left.await);
        let told = (sizes, began.elapsed(), cut_short);
        self.compactions
            .record((room, agent), (made.clone(), summarized), told)
            .await;
        Some(made)
    }

    /// The compaction `written` makes of `agent`'s history in `room` since
    /// `compacted`, keeping whole the events from the index `kept` on, as of
    /// the newest event, `newest`: what it carries beside the summary, and
    /// the messages `compacted` summarized. It estimates nothing yet.
    async fn made(
        &self,
        (room, agent): (&str, &str),
        compacted: Option<&Compacted>,
        (written, kept, newest): (String, u64, u64),
        measured: &Measured,
    ) -> Compacted {
        Compacted {
            from: kept,
            summary: written,
            // After a relaunch, until a turn tells the project's snapshot,
            // the previous compaction's stands for it.
            snapshot: self
                .compactions
                .snapshot(room, agent)
                .or_else(|| compacted.and_then(|c| c.snapshot.clone())),
            summarized: compacted.map_or(0, |c| c.summarized),
            at: newest,
            recorded: self.compactions.recorded(room, agent).await,
            estimate: None,
            fixed: u64::try_from(measured.fixed)
                .ok()
                .filter(|fixed| *fixed > 0),
        }
    }

    /// How many of `events`, `agent`'s history up to the room's row
    /// `newest`, a compaction summarizes: the events before the latest
    /// within the tier's budget and its rows, or before the latest turn when
    /// `forced`, a turn's start the cut.
    fn cut(
        &self,
        (events, agent): (&[RoomEvent], &str),
        measured: &Measured,
        (newest, forced): (u64, bool),
    ) -> usize {
        let written: Vec<bool> = events
            .iter()
            .map(|event| event.source.channel_id.as_str() == agent)
            .collect();
        let ended: Vec<bool> = events
            .iter()
            .zip(&written)
            .map(|(event, own)| *own && event.metadata.contains_key("loop_end_reason"))
            .collect();
        let starts = tiers::turn_starts(&written, &ended);
        let budget = if forced {
            0
        } else {
            tiers::kept_budget(self.ceiling)
        };
        let oldest = newest.saturating_sub(tiers::kept_rows());
        let first = events.partition_point(|event| event.index < oldest);
        tiers::cut(&measured.costs, &starts, (budget, first))
    }
}

impl<P: AiProvider> Compactor for TieredMemory<P> {
    /// Compacts `events` whatever their tier, as the agent's latest turn in
    /// `room` measured the rest of its request.
    fn compact_now<'a>(
        &'a self,
        room: &'a str,
        History {
            mut events,
            replayed,
        }: History,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let agent = self.agent.as_str();
            let compacted = self.compactions.get(room, agent);
            let compacted = compacted.as_ref();
            let after = compacted.map_or(0, |c| c.at);
            let mut footprint = TurnFootprint::new(0, 0);
            footprint.replayed = replayed;
            // This launch's latest turn measured it, else the latest request
            // since the compaction, else the compaction counted it.
            let known = crate::sync::lock(&self.fixed).get(room).copied();
            let carried = compacted
                .and_then(|c| c.fixed)
                .and_then(|fixed| usize::try_from(fixed).ok());
            footprint.input_tokens = known.unwrap_or_else(|| {
                let measured =
                    measure(&events, (agent, after), &footprint).fixed_measured(compacted);
                if measured > 0 {
                    measured
                } else {
                    carried.unwrap_or(0)
                }
            });
            let measured = measure(&events, (agent, after), &footprint);
            let newest = events.last().map_or(0, |event| event.index);
            let to = (newest, true);
            self.compact((room, agent), compacted, &mut events, &measured, to)
                .await;
        })
    }
}

/// What a turn on `events`, `agent`'s history, takes: each event with what
/// the turn replays after it, the rest of the turn as `footprint` says, and
/// the agent's latest request as measured, one sent after the event at
/// `after`: the latest compaction's newest, before which a request measured
/// the history it cut.
fn measure(
    events: &[RoomEvent],
    (agent, after): (&str, u64),
    footprint: &TurnFootprint,
) -> Measured {
    let costs = costs(events, footprint);
    let latest = events.iter().rposition(|event| {
        event.index > after
            && event.source.channel_id.as_str() == agent
            && reported(event).is_some()
    });
    let last = latest.and_then(|at| tiers::measured(reported(&events[at]), &costs[at..]));
    Measured {
        costs,
        fixed: footprint.input_tokens + footprint.reply_tokens,
        last,
    }
}

/// The input of the request that wrote `event`, as its provider reported it:
/// what it sent, read from the cache or written there included.
fn reported(event: &RoomEvent) -> Option<usize> {
    let usage = event.metadata.get("ai_last_usage")?.as_object()?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let input = usage.get("input_tokens").and_then(Value::as_u64)?;
    let total = input + count("cache_read_input_tokens") + count("cache_creation_input_tokens");
    usize::try_from(total).ok()
}

/// What each of `events` costs, with what the turn replays after it.
fn costs(events: &[RoomEvent], footprint: &TurnFootprint) -> Vec<usize> {
    let own: Vec<(u64, usize)> = events
        .iter()
        .map(|event| {
            let tokens = estimate_event_tokens(event, EstimateEventTokensOptions::default());
            (event.index, tokens)
        })
        .collect();
    let replayed: Vec<(u64, usize)> = footprint
        .replayed
        .iter()
        .map(|(index, _, tokens)| (*index, *tokens))
        .collect();
    tiers::costs(&own, &replayed)
}

fn result(compacted: Option<&Compacted>, events: Vec<RoomEvent>) -> MemoryResult {
    MemoryResult {
        messages: compacted.map(summary::message).into_iter().collect(),
        events,
        ..MemoryResult::default()
    }
}

impl<P: AiProvider> MemoryProvider for TieredMemory<P> {
    fn name(&self) -> String {
        "TieredMemory".to_owned()
    }

    fn recent_events_window(&self) -> usize {
        WINDOW
    }

    fn retrieve<'a>(
        &'a self,
        room_id: &'a RoomId,
        current_event: &'a RoomEvent,
        context: &'a RoomContext,
        options: RetrieveOptions,
    ) -> Pin<Box<dyn Future<Output = Result<MemoryResult, ChannelError>> + Send + 'a>> {
        Box::pin(async move {
            let agent = options
                .channel_id
                .as_ref()
                .map_or_else(String::new, ToString::to_string);
            Ok(self
                .read((room_id.as_str(), &agent), current_event, context)
                .await)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;
    use roomkit::model::{ChannelId, ChannelType, EventSource, Room, TextContent};
    use roomkit_ai::testing::{ScriptedProvider, ScriptedRound};

    fn said(index: u64, channel: &str, usage: Option<u64>) -> RoomEvent {
        let source = EventSource::new(ChannelId::new(channel).unwrap(), ChannelType::AI);
        let mut event = RoomEvent::new(
            RoomId::new("room").unwrap(),
            source,
            TextContent::new("Text."),
        );
        event.index = index;
        if let Some(input) = usage {
            let usage =
                serde_json::json!({"input_tokens": input, "cache_read_input_tokens": 1_000});
            event.metadata.insert("ai_last_usage".into(), usage);
        }
        event
    }

    /// A request the provider measured past the estimate counts toward the
    /// tier, unless it was sent before the latest compaction: it measured
    /// the history that compaction cut, and would compact it again.
    #[test]
    fn the_latest_measured_request_since_the_compaction_counts() {
        let events = [
            said(1, "terminal", None),
            said(2, "code", Some(50_000)),
            said(3, "terminal", None),
        ];
        let none = TurnFootprint::new(0, 0);
        let measured = measure(&events, ("code", 0), &none);
        let since: usize = measured.costs[1..].iter().sum();
        assert_eq!(measured.last, Some(51_000 + since));
        assert_eq!(measured.now(None), 51_000 + since);
        assert!(tiers::due(measured.now(None), 60_000));
        // Measured at or before the compaction's newest event: estimated.
        let stale = measure(&events, ("code", 2), &none);
        assert_eq!(stale.last, None);
        assert!(!tiers::due(stale.now(None), 60_000));
    }

    /// After a restart no turn measured the rest of the request: the latest
    /// measured one says what it took besides the history.
    #[test]
    fn the_rest_of_a_request_is_read_from_the_latest_measured_one() {
        let events = [
            said(1, "terminal", None),
            said(2, "code", Some(50_000)),
            said(3, "terminal", None),
        ];
        let none = TurnFootprint::new(0, 0);
        let measured = measure(&events, ("code", 0), &none);
        let history: usize = measured.costs.iter().sum();
        assert_eq!(measured.fixed_measured(None), 51_000 - measured.costs[0]);
        assert!(history < 100, "{history}");
        assert_eq!(measure(&events, ("code", 2), &none).fixed_measured(None), 0);
    }

    /// A turn's context holds the room's latest `WINDOW` rows. Short
    /// messages filled them long before their tokens reached the tier: the
    /// oldest then left the agent's history with no summary standing for
    /// them. The history now compacts past the tier of those rows too, and
    /// keeps whole no more rows than a compaction's share of them.
    #[tokio::test]
    async fn a_history_compacts_before_its_oldest_rows_leave_the_context() {
        let dir = tempfile::tempdir().unwrap();
        let compactions = Arc::new(Compactions::new(dir.path()));
        let provider = ScriptedProvider::new([ScriptedRound::answering("They talked.")]);
        let memory = TieredMemory::new(provider, (1_000_000, "code"), compactions.clone());
        let rows = 1_700;
        let mut context = RoomContext::new(Room::new(RoomId::new("room").unwrap()));
        context.recent_events = (0..rows)
            .map(|index| {
                let own = index % 2 == 1;
                let mut event = said(index, if own { "code" } else { "terminal" }, None);
                if own {
                    let ended = serde_json::json!("complete");
                    event.metadata.insert("loop_end_reason".into(), ended);
                }
                event
            })
            .collect();
        let current = said(rows, "terminal", None);
        let read = memory.read(("room", "code"), &current, &context).await;
        assert_eq!(memory.provider.requests().len(), 1, "a summary was asked");
        let compacted = compactions.get("room", "code").unwrap();
        assert!(
            rows - compacted.from <= tiers::kept_rows(),
            "{}",
            compacted.from
        );
        assert_eq!(read.events.first().map(|e| e.index), Some(compacted.from));
        assert_eq!(read.messages.len(), 1, "the summary stands for the rest");
    }

    /// `agent`'s history of `count` turns, from index 1: a message, then
    /// the agent's answer, which ends its turn.
    fn turns(count: u64) -> Vec<RoomEvent> {
        (1..=count * 2)
            .map(|index| {
                let own = index % 2 == 0;
                let mut event = said(index, if own { "code" } else { "terminal" }, None);
                if own {
                    let ended = serde_json::json!("complete");
                    event.metadata.insert("loop_end_reason".into(), ended);
                }
                event
            })
            .collect()
    }

    /// After a relaunch no turn told the project's snapshot, nor measured
    /// what a request takes besides its history: `/compact` then wrote a
    /// summary without the snapshot and told a size short of the system
    /// prompt and the tools. It now carries the previous compaction's.
    #[tokio::test]
    async fn a_compaction_after_a_relaunch_carries_the_previous_snapshot_and_rest() {
        let dir = tempfile::tempdir().unwrap();
        let previous = Compacted {
            from: 0,
            summary: "Earlier.".into(),
            snapshot: Some("PROJECT RULES".into()),
            summarized: 1,
            at: 0,
            recorded: None,
            estimate: None,
            fixed: Some(30_000),
        };
        let told = ((1, 1), std::time::Duration::ZERO, false);
        Compactions::new(dir.path())
            .record(("room", "code"), (previous, 1), told)
            .await;
        let relaunched = Arc::new(Compactions::new(dir.path()));
        let provider = ScriptedProvider::new([ScriptedRound::answering("They talked.")]);
        let memory = TieredMemory::new(provider, (100_000, "code"), relaunched.clone());
        let history = History {
            events: turns(6),
            replayed: Vec::new(),
        };
        memory.compact_now("room", history).await;
        let made = relaunched.get("room", "code").unwrap();
        assert_ne!(made.summary, "Earlier.", "it compacted");
        assert_eq!(made.snapshot.as_deref(), Some("PROJECT RULES"));
        assert!(made.estimate.unwrap().tokens > 30_000);
        assert_eq!(made.fixed, Some(30_000));
    }

    /// The text of the request a summary was asked with.
    fn asked(request: &roomkit_ai::AiContext) -> String {
        match &request.messages[0].content {
            roomkit_ai::AiMessageContent::Text(text) => text.clone(),
            _ => String::new(),
        }
    }

    /// Review finding: a summary read the newest lines that fit its request
    /// and said how many older ones it left out, and the compaction dropped
    /// them all from the history: the user's first constraint reached no
    /// summary and no request again. Each line now reaches a summary: what
    /// does not fit one request is summarized in the next, after the summary
    /// of what came before.
    #[tokio::test]
    async fn a_history_longer_than_a_summary_reads_is_summarized_in_parts() {
        let dir = tempfile::tempdir().unwrap();
        let compactions = Arc::new(Compactions::new(dir.path()));
        let answers = (1..=10).map(|n| ScriptedRound::answering(format!("Summary {n}.")));
        let provider = ScriptedProvider::new(answers);
        let ceiling = 16_000;
        let memory = TieredMemory::new(provider, (ceiling, "code"), compactions.clone());
        let mut events = turns(30);
        for event in &mut events {
            let said = format!("Message {}: {}", event.index, "x".repeat(1_000));
            event.content = TextContent::new(said).into();
        }
        events[0].content = TextContent::new("CONSTRAINT: never touch main.rs.").into();
        let history = History {
            events,
            replayed: Vec::new(),
        };
        memory.compact_now("room", history).await;
        let made = compactions.get("room", "code").unwrap();
        let requests: Vec<String> = memory.provider.requests().iter().map(asked).collect();
        assert!(requests.len() > 1, "{requests:#?}");
        assert!(requests[0].contains("CONSTRAINT: never touch main.rs."));
        // Every message summarized reached a request, within what one reads.
        for index in 2..made.from {
            let message = format!("Message {index}: ");
            assert!(
                requests.iter().any(|asked| asked.contains(&message)),
                "{index}"
            );
        }
        for (at, asked) in requests.iter().enumerate() {
            assert!(!asked.contains("left out"), "{asked}");
            assert!(
                asked.len() <= tiers::summary_chars(ceiling),
                "{}",
                asked.len()
            );
            if at > 0 {
                assert!(asked.contains(&format!("Summary {at}.")), "{asked}");
            }
        }
        assert_eq!(made.summary, format!("Summary {}.", requests.len()));
    }
}
