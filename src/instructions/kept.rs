//! What an agent's history still holds of the project's instructions.
//!
//! The kit keeps the instruction each turn read as the agent's own row and
//! replays it where that turn read it in the agent's later turns (roomkit-rs
//! RMK-1407). A turn therefore sends the project's snapshot again only when
//! the latest one the agent read differs, or no longer replays: its row
//! precedes every message the agent's memory still reads, or comes at or
//! after the event the turn answers, which the kit replays nothing after.

use roomkit::model::{ChannelId, EventDirection, EventType, RoomEvent};
use roomkit_core::rules::{edits::is_deleted, transcoding::text_of};

use super::GUIDANCE;

/// Whether the turn of `channel` on `trigger` that reads `history`, through
/// a memory keeping its latest `window` events (0 keeping every one), still
/// replays `snapshot` as the latest project snapshot the agent read.
pub(crate) fn still_read(
    snapshot: &str,
    history: &[RoomEvent],
    channel: &ChannelId,
    trigger: &RoomEvent,
    window: usize,
) -> bool {
    // The kit replays what an earlier turn read only before the turn's
    // event; one never numbered has nothing before it (roomkit-core's
    // `rounds_before`). A turn that started earlier may keep its row later.
    let before = |index: u64| trigger.index == 0 || index < trigger.index;
    let latest = history
        .iter()
        .filter(|event| is_kept_instruction(event, channel) && before(event.index))
        .map(|event| (event.index, text_of(&event.content)))
        .filter(|(_, text)| text.contains(GUIDANCE))
        .max_by_key(|(index, _)| *index);
    let Some((index, text)) = latest else {
        return false;
    };
    ends_with_snapshot(&text, snapshot)
        && first_read(history, channel, window).is_some_and(|first| index > first)
}

/// Whether `text`, an instruction a turn read, carries `snapshot` as its
/// last part: a turn's instruction ends with the snapshot it sends.
fn ends_with_snapshot(text: &str, snapshot: &str) -> bool {
    text == snapshot
        || text
            .strip_suffix(snapshot)
            .is_some_and(|before| before.ends_with("\n\n"))
}

/// The index of the oldest event of `history` the agent's memory reads: the
/// latest `window` of them, beside the agent's own rows, which take none of
/// its window. An instruction kept before it replays no more (or only with
/// the answer of its turn, which this does not count on).
fn first_read(history: &[RoomEvent], channel: &ChannelId, window: usize) -> Option<u64> {
    let viewed: Vec<u64> = history
        .iter()
        .filter(|event| !is_own_row(event, channel))
        .map(|event| event.index)
        .collect();
    let skip = match window {
        0 => 0,
        window => viewed.len().saturating_sub(window),
    };
    viewed.into_iter().skip(skip).min()
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

    /// `still_read` for the agent, checked against the kit's own transcript
    /// of the turn: what it says replays must be in what the kit sends, the
    /// history read as an AI channel reads it, its own rows apart and its
    /// memory's window over the rest (roomkit-ai's `recalled` and
    /// `SlidingWindowMemory`).
    fn replayed(snapshot: &str, history: &[RoomEvent], trigger: &RoomEvent, window: usize) -> bool {
        let read = still_read(snapshot, history, &agent(), trigger, window);
        let (rows, viewed): (Vec<RoomEvent>, Vec<RoomEvent>) = history
            .iter()
            .cloned()
            .partition(|event| is_own_row(event, &agent()));
        let skip = match window {
            0 => 0,
            window => viewed.len().saturating_sub(window),
        };
        let mut context = RoomContext::new(Room::new(RoomId::new("room").unwrap()));
        context.recent_events = viewed.into_iter().skip(skip).chain(rows).collect();
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
        assert!(replayed(&one, &history, &trigger(), 50));
        // Another agent's turn reads none of this agent's rows.
        assert!(!still_read(
            &one,
            &history,
            &ChannelId::new("other").unwrap(),
            &trigger(),
            50
        ));
        // A turn told something by the application ends with the snapshot.
        let told = vec![
            message(1),
            read(2, &format!("Mode.\n\nAnswer it.\n\n{one}")),
            message(3),
        ];
        assert!(replayed(&one, &told, &trigger(), 50));
    }

    #[test]
    fn a_changed_snapshot_or_a_first_turn_reads_none() {
        let one = snapshot("ROOT_ONE");
        let two = snapshot("ROOT_TWO");
        assert!(!replayed(&one, &[message(1)], &trigger(), 50));
        let history = vec![message(1), read(2, &format!("Mode.\n\n{one}")), message(3)];
        assert!(!replayed(&two, &history, &trigger(), 50));
        // Back to the first rules: the latest snapshot read is the second.
        let back = vec![
            message(1),
            read(2, &format!("Mode.\n\n{one}")),
            message(3),
            read(4, &format!("Mode.\n\n{two}")),
            message(5),
        ];
        assert!(!replayed(&one, &back, &trigger(), 50));
        // A longer snapshot that only begins as the current one is another.
        let longer = format!("{one}\n\nSource: \"nested\"");
        let grown = vec![
            message(1),
            read(2, &format!("Mode.\n\n{longer}")),
            message(3),
        ];
        assert!(!replayed(&one, &grown, &trigger(), 50));
    }

    #[test]
    fn a_snapshot_before_every_message_the_window_reads_is_read_no_more() {
        let one = snapshot("ROOT_ONE");
        let mut history = vec![message(1), read(2, &format!("Mode.\n\n{one}"))];
        // The agent's own rows take none of the window.
        history.extend((3..=6).map(|index| event(index, EventType::ToolCallStart, AGENT, "")));
        history.extend((7..=9).map(message));
        assert!(replayed(&one, &history, &trigger(), 4));
        assert!(!replayed(&one, &history, &trigger(), 3));
        // A window of 0 reads every event.
        assert!(replayed(&one, &history, &trigger(), 0));
    }

    /// A turn that started before the message this turn answers may keep
    /// its instruction after it: the kit replays no row from there on.
    #[test]
    fn a_snapshot_kept_after_the_turn_s_event_is_not_read() {
        let one = snapshot("ROOT_ONE");
        let history = vec![message(1), message(2), read(3, &format!("Mode.\n\n{one}"))];
        assert!(!replayed(&one, &history, &message(2), 50));
        assert!(replayed(&one, &history, &message(4), 50));
        // An event the store never numbered has nothing before it.
        assert!(replayed(&one, &history, &message(0), 50));
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
            50
        ));
        let mut deleted = read(2, &one);
        deleted.metadata.insert("deleted".into(), true.into());
        assert!(!replayed(
            &one,
            &[message(1), deleted, message(3)],
            &trigger(),
            50
        ));
        assert!(replayed(
            &one,
            &[message(1), read(2, &one), message(3)],
            &trigger(),
            50
        ));
    }
}
