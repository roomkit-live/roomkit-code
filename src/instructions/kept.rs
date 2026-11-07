//! What an agent's history still holds of the project's instructions.
//!
//! The kit keeps the instruction each turn read as the agent's own row and
//! replays it where that turn read it in the agent's later turns (roomkit-rs
//! RMK-1407). A turn whose agent's history compacts by tiers therefore sends
//! the project's snapshot again only when the latest one the agent still
//! reads differs. A row replays no more once it precedes the oldest message
//! the agent's memory reads whole, or comes at or after the event the turn
//! answers; the compaction that cut there carries the snapshot its turn read.

use roomkit::model::{ChannelId, EventDirection, EventType, RoomEvent};
use roomkit_core::rules::{edits::is_deleted, transcoding::text_of};

use super::GUIDANCE;

/// What an agent's memory reads whole of its history: the events from
/// `from`, after the summary of a compaction that carries `carried`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct View<'a> {
    /// The index of the oldest event the memory reads whole: 0 before any
    /// compaction.
    pub from: u64,
    /// The project snapshot the compaction's summary carries.
    pub carried: Option<&'a str>,
}

/// Whether the turn of `channel` on `trigger` that reads `history` through
/// `view` still holds `snapshot` as the latest project snapshot the agent
/// read.
pub(crate) fn still_read(
    snapshot: &str,
    history: &[RoomEvent],
    channel: &ChannelId,
    trigger: &RoomEvent,
    view: &View<'_>,
) -> bool {
    // The kit replays what an earlier turn read only after the first event
    // its memory kept, and before the turn's event; one never numbered has
    // nothing before it (roomkit-core's `place` and `rounds_before`). A turn
    // that started earlier may keep its row later.
    let first = first_read(history, channel, view.from);
    let replays = |index: u64| {
        first.is_some_and(|first| index > first) && (trigger.index == 0 || index < trigger.index)
    };
    let latest = history
        .iter()
        .filter(|event| is_kept_instruction(event, channel) && replays(event.index))
        .map(|event| (event.index, text_of(&event.content)))
        .filter(|(_, text)| text.contains(GUIDANCE))
        .max_by_key(|(index, _)| *index);
    match latest {
        Some((_, text)) => ends_with_snapshot(&text, snapshot),
        None => view.carried == Some(snapshot),
    }
}

/// Whether `text`, an instruction a turn read, carries `snapshot` as its
/// last part: a turn's instruction ends with the snapshot it sends.
fn ends_with_snapshot(text: &str, snapshot: &str) -> bool {
    text == snapshot
        || text
            .strip_suffix(snapshot)
            .is_some_and(|before| before.ends_with("\n\n"))
}

/// The index of the oldest event of `history` from `from` the agent's memory
/// reads, beside the agent's own rows, which it does not read. A row kept
/// before it replays no more (or only with the answer of its turn, which
/// this does not count on).
fn first_read(history: &[RoomEvent], channel: &ChannelId, from: u64) -> Option<u64> {
    history
        .iter()
        .filter(|event| !is_own_row(event, channel) && event.index >= from)
        .map(|event| event.index)
        .min()
}

/// Whether `event` is a row of `channel`'s own its memory does not read: its
/// tool calls and the instructions its turns read.
fn is_own_row(event: &RoomEvent, channel: &ChannelId) -> bool {
    &event.source.channel_id == channel
        && matches!(
            event.r#type,
            EventType::ToolCallStart | EventType::ToolCallEnd | EventType::InstructionRead
        )
}

/// Whether `event` is an instruction `channel` kept and replays: one it
/// wrote, not deleted.
fn is_kept_instruction(event: &RoomEvent, channel: &ChannelId) -> bool {
    event.r#type == EventType::InstructionRead
        && &event.source.channel_id == channel
        && event.source.direction == EventDirection::Outbound
        && !is_deleted(event)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;
    use roomkit::model::{ChannelType, EventSource, Room, RoomContext, RoomId, TextContent};
    use roomkit_core::rules::transcript::conversation;

    const AGENT: &str = "code";

    fn event(index: u64, kind: EventType, channel: &str, text: &str) -> RoomEvent {
        let mut source = EventSource::new(ChannelId::new(channel).unwrap(), ChannelType::AI);
        source.direction = EventDirection::Outbound;
        let mut event =
            RoomEvent::new(RoomId::new("room").unwrap(), source, TextContent::new(text));
        event.r#type = kind;
        event.index = index;
        event
    }

    fn message(index: u64) -> RoomEvent {
        event(index, EventType::Message, "terminal", "A question.")
    }

    fn snapshot(rules: &str) -> String {
        format!("{GUIDANCE}\n\n{rules}")
    }

    fn read(index: u64, text: &str) -> RoomEvent {
        event(index, EventType::InstructionRead, AGENT, text)
    }

    fn agent() -> ChannelId {
        ChannelId::new(AGENT).unwrap()
    }

    /// The message a turn answers, the newest of its history.
    fn trigger() -> RoomEvent {
        message(100)
    }

    /// `still_read` for the agent before any compaction, from `from` on,
    /// checked against the kit's own transcript of the turn: what it says
    /// replays must be in what the kit sends, the history read as an AI
    /// channel reads it, its own rows apart and its memory's events from
    /// `from` (roomkit-ai's `recalled`).
    fn replayed(snapshot: &str, history: &[RoomEvent], trigger: &RoomEvent, from: u64) -> bool {
        let view = View {
            from,
            carried: None,
        };
        let read = still_read(snapshot, history, &agent(), trigger, &view);
        let (rows, viewed): (Vec<RoomEvent>, Vec<RoomEvent>) = history
            .iter()
            .cloned()
            .partition(|event| is_own_row(event, &agent()));
        let mut context = RoomContext::new(Room::new(RoomId::new("room").unwrap()));
        context.recent_events = viewed
            .into_iter()
            .filter(|event| event.index >= from)
            .chain(rows)
            .collect();
        let sent = conversation(&agent(), trigger, &context, usize::MAX);
        let kit = sent.iter().any(|message| message.text.contains(snapshot));
        assert!(
            !read || kit,
            "said replayed, but the kit sends no snapshot: {sent:?}"
        );
        read
    }

    #[test]
    fn the_latest_snapshot_read_since_the_first_message_read_is_still_read() {
        let one = snapshot("ROOT_ONE");
        let history = vec![
            message(1),
            read(2, &format!("Application mode: Ask.\n\n{one}")),
            message(3),
            read(4, "Application mode: Ask."),
            message(5),
        ];
        assert!(replayed(&one, &history, &trigger(), 0));
        // Another agent's turn reads none of this agent's rows.
        assert!(!still_read(
            &one,
            &history,
            &ChannelId::new("other").unwrap(),
            &trigger(),
            &View::default()
        ));
        // A turn told something by the application ends with the snapshot.
        let told = vec![
            message(1),
            read(2, &format!("Mode.\n\nAnswer it.\n\n{one}")),
            message(3),
        ];
        assert!(replayed(&one, &told, &trigger(), 0));
    }

    #[test]
    fn a_changed_snapshot_or_a_first_turn_reads_none() {
        let one = snapshot("ROOT_ONE");
        let two = snapshot("ROOT_TWO");
        assert!(!replayed(&one, &[message(1)], &trigger(), 0));
        let history = vec![message(1), read(2, &format!("Mode.\n\n{one}")), message(3)];
        assert!(!replayed(&two, &history, &trigger(), 0));
        // Back to the first rules: the latest snapshot read is the second.
        let back = vec![
            message(1),
            read(2, &format!("Mode.\n\n{one}")),
            message(3),
            read(4, &format!("Mode.\n\n{two}")),
            message(5),
        ];
        assert!(!replayed(&one, &back, &trigger(), 0));
        // A longer snapshot that only begins as the current one is another.
        let longer = format!("{one}\n\nSource: \"nested\"");
        let grown = vec![
            message(1),
            read(2, &format!("Mode.\n\n{longer}")),
            message(3),
        ];
        assert!(!replayed(&one, &grown, &trigger(), 0));
    }

    /// What a memory reads after a compaction that cut at `from`.
    fn carried(snapshot: &str, from: u64) -> View<'_> {
        View {
            from,
            carried: Some(snapshot),
        }
    }

    #[test]
    fn a_snapshot_before_the_first_event_read_whole_replays_no_more_but_is_carried() {
        let one = snapshot("ROOT_ONE");
        let two = snapshot("ROOT_TWO");
        let mut history = vec![message(1), read(2, &format!("Mode.\n\n{one}"))];
        // The agent's own rows are no events its memory reads.
        history.extend((3..=6).map(|index| event(index, EventType::ToolCallStart, AGENT, "")));
        history.extend((7..=9).map(message));
        assert!(replayed(&one, &history, &trigger(), 0));
        assert!(replayed(&one, &history, &trigger(), 1));
        assert!(!replayed(&one, &history, &trigger(), 7));
        // The compaction that cut there carries the snapshot its turn read.
        assert!(still_read(
            &one,
            &history,
            &agent(),
            &trigger(),
            &carried(&one, 7)
        ));
        assert!(!still_read(
            &two,
            &history,
            &agent(),
            &trigger(),
            &carried(&one, 7)
        ));
        // A snapshot read since the cut is the latest, whatever it carries.
        history.extend([read(10, &format!("Mode.\n\n{two}")), message(11)]);
        assert!(still_read(
            &two,
            &history,
            &agent(),
            &trigger(),
            &carried(&one, 7)
        ));
        assert!(!still_read(
            &one,
            &history,
            &agent(),
            &trigger(),
            &carried(&one, 7)
        ));
    }

    /// A turn that started before the message this turn answers may keep
    /// its instruction after it: the kit replays no row from there on.
    #[test]
    fn a_snapshot_kept_after_the_turn_s_event_is_not_read() {
        let one = snapshot("ROOT_ONE");
        let history = vec![message(1), message(2), read(3, &format!("Mode.\n\n{one}"))];
        assert!(!replayed(&one, &history, &message(2), 0));
        assert!(replayed(&one, &history, &message(4), 0));
        // An event the store never numbered has nothing before it.
        assert!(replayed(&one, &history, &message(0), 0));
    }

    #[test]
    fn a_deleted_or_an_inbound_row_is_no_snapshot_read() {
        let one = snapshot("ROOT_ONE");
        let mut inbound = read(2, &one);
        inbound.source.direction = EventDirection::Inbound;
        assert!(!replayed(
            &one,
            &[message(1), inbound, message(3)],
            &trigger(),
            0
        ));
        let mut deleted = read(2, &one);
        deleted.metadata.insert("deleted".into(), true.into());
        assert!(!replayed(
            &one,
            &[message(1), deleted, message(3)],
            &trigger(),
            0
        ));
        assert!(replayed(
            &one,
            &[message(1), read(2, &one), message(3)],
            &trigger(),
            0
        ));
    }
}
