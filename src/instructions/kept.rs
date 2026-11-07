//! What an agent's history still holds of the project's instructions.
//!
//! The kit keeps the instruction each turn read as the agent's own row and
//! replays it where that turn read it in the agent's later turns (roomkit-rs
//! RMK-1407). A turn therefore sends the project's snapshot again only when
//! the latest one the agent read differs, or no longer replays: its row
//! precedes every message the agent's memory still reads.

use roomkit::model::{ChannelId, EventDirection, EventType, RoomEvent};
use roomkit_core::rules::{edits::is_deleted, transcoding::text_of};

use super::GUIDANCE;

/// Whether the turn of `channel` that reads `history`, through a memory
/// keeping its latest `window` events (0 keeping every one), still replays
/// `snapshot` as the latest project snapshot the agent read.
pub(crate) fn still_read(
    snapshot: &str,
    history: &[RoomEvent],
    channel: &ChannelId,
    window: usize,
) -> bool {
    let latest = history
        .iter()
        .filter(|event| is_kept_instruction(event, channel))
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
    use roomkit::model::{ChannelType, EventSource, RoomId, TextContent};

    const AGENT: &str = "code";

    fn event(index: u64, kind: EventType, channel: &str, text: &str) -> RoomEvent {
        let mut source = EventSource::new(ChannelId::new(channel).unwrap(), ChannelType::AI);
        source.direction = EventDirection::Outbound;
        let mut event = RoomEvent::new(RoomId::new("room").unwrap(), source, TextContent::new(text));
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
        assert!(still_read(&one, &history, &agent(), 50));
        // Another agent's turn reads none of this agent's rows.
        assert!(!still_read(&one, &history, &ChannelId::new("other").unwrap(), 50));
        // A turn told something by the application ends with the snapshot.
        let told = vec![message(1), read(2, &format!("Mode.\n\nAnswer it.\n\n{one}")), message(3)];
        assert!(still_read(&one, &told, &agent(), 50));
    }

    #[test]
    fn a_changed_snapshot_or_a_first_turn_reads_none() {
        let one = snapshot("ROOT_ONE");
        let two = snapshot("ROOT_TWO");
        assert!(!still_read(&one, &[message(1)], &agent(), 50));
        let history = vec![message(1), read(2, &format!("Mode.\n\n{one}")), message(3)];
        assert!(!still_read(&two, &history, &agent(), 50));
        // Back to the first rules: the latest snapshot read is the second.
        let back = vec![
            message(1),
            read(2, &format!("Mode.\n\n{one}")),
            message(3),
            read(4, &format!("Mode.\n\n{two}")),
            message(5),
        ];
        assert!(!still_read(&one, &back, &agent(), 50));
        // A longer snapshot that only begins as the current one is another.
        let longer = format!("{one}\n\nSource: \"nested\"");
        let grown = vec![message(1), read(2, &format!("Mode.\n\n{longer}")), message(3)];
        assert!(!still_read(&one, &grown, &agent(), 50));
    }

    #[test]
    fn a_snapshot_before_every_message_the_window_reads_is_read_no_more() {
        let one = snapshot("ROOT_ONE");
        let mut history = vec![message(1), read(2, &format!("Mode.\n\n{one}"))];
        // The agent's own rows take none of the window.
        history.extend((3..=6).map(|index| event(index, EventType::ToolCallStart, AGENT, "")));
        history.extend((7..=9).map(message));
        assert!(still_read(&one, &history, &agent(), 4));
        assert!(!still_read(&one, &history, &agent(), 3));
        // A window of 0 reads every event.
        assert!(still_read(&one, &history, &agent(), 0));
    }

    #[test]
    fn a_deleted_or_an_inbound_row_is_no_snapshot_read() {
        let one = snapshot("ROOT_ONE");
        let mut inbound = read(2, &one);
        inbound.source.direction = EventDirection::Inbound;
        assert!(!still_read(&one, &[message(1), inbound, message(3)], &agent(), 50));
        let mut deleted = read(2, &one);
        deleted.metadata.insert("deleted".into(), true.into());
        assert!(!still_read(&one, &[message(1), deleted, message(3)], &agent(), 50));
        assert!(still_read(&one, &[message(1), read(2, &one), message(3)], &agent(), 50));
    }
}
