//! The arithmetic of a compaction by tiers (HITL a2daedfe): when a history
//! compacts, and where it is cut.
//!
//! A history stays as it is, and its prompt's prefix with it, while it fits
//! under `TRIGGER` of its ceiling. Past it, a summary stands for its oldest
//! messages and the latest are kept whole within `KEPT` of the ceiling: the
//! history grows again from there, as it is, up to the next tier.

/// The share of the ceiling past which a history compacts.
pub(crate) const TRIGGER: f64 = 0.8;
/// The share of the ceiling the messages a compaction keeps whole may take,
/// with what their turns replay.
pub(crate) const KEPT: f64 = 0.3;
/// The fewest messages a compaction keeps whole.
pub(crate) const MIN_KEPT: usize = 4;
/// The share of the ceiling a summary's request may read.
const SUMMARY_READS: f64 = 0.5;

/// The tokens a history compacts under: the model's window, or the input
/// past which its price rises when that is smaller (HITL db06dd3d). None
/// without a known window: the agent keeps its window of messages.
pub(crate) fn ceiling(window: Option<u64>, long_context: Option<u64>) -> Option<u64> {
    let window = window.filter(|tokens| *tokens > 0)?;
    Some(
        long_context
            .filter(|tokens| *tokens > 0)
            .map_or(window, |price| window.min(price)),
    )
}

/// `ceiling * share`, in tokens.
fn share(ceiling: u64, share: f64) -> usize {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "a share of a token count, cut toward zero"
    )]
    let tokens = (ceiling as f64 * share) as usize;
    tokens
}

/// Whether a request of `tokens` passes its ceiling's tier.
pub(crate) fn due(tokens: usize, ceiling: u64) -> bool {
    tokens > share(ceiling, TRIGGER)
}

/// The characters a summary's request under `ceiling` may read, at four a
/// token: what it summarizes never overflows the model.
pub(crate) fn summary_chars(ceiling: u64) -> usize {
    share(ceiling, SUMMARY_READS).saturating_mul(4)
}

/// What a compaction under `ceiling` keeps whole.
pub(crate) fn kept_budget(ceiling: u64) -> usize {
    share(ceiling, KEPT)
}

/// What each event of `events`, `(index, tokens)` oldest first, costs with
/// what the turn replays of its agent's earlier turns, `replayed`: each piece
/// at its index (roomkit-ai's `TurnFootprint::replayed`), charged to the
/// latest event before it, which belongs to the piece's turn. A cut falls on
/// a turn's start, so that a turn's pieces stay with it, kept or summarized:
/// the kit charges them to the turn's answer, which comes to the same total.
/// A piece before every event replays with none and costs nothing.
pub(crate) fn costs(events: &[(u64, usize)], replayed: &[(u64, usize)]) -> Vec<usize> {
    let mut costs: Vec<usize> = events.iter().map(|(_, tokens)| *tokens).collect();
    for (index, tokens) in replayed {
        let before = events.partition_point(|(event, _)| event < index);
        if let Some(at) = before.checked_sub(1) {
            costs[at] += tokens;
        }
    }
    costs
}

/// Which of a history's events start a turn, from which ones its agent
/// wrote, oldest first: a message it did not write that follows one it
/// wrote, or opens the history. A message steered into a running turn, or
/// written while it ran, follows another person's: it opens no turn.
pub(crate) fn turn_starts(written: &[bool]) -> Vec<bool> {
    written
        .iter()
        .enumerate()
        .map(|(at, own)| !own && (at == 0 || written[at - 1]))
        .collect()
}

/// The tokens a turn's request takes as last measured: `last`, what the
/// provider reported of the agent's latest request, then what came since,
/// the answer that request wrote among it, `since`.
pub(crate) fn measured(last: Option<usize>, since: &[usize]) -> Option<usize> {
    last.map(|last| last + since.iter().sum::<usize>())
}

/// Where a compaction cuts a history whose events cost `costs`, oldest
/// first: the first of the latest events kept whole within `budget`,
/// `MIN_KEPT` of them at least, moved to the next event that `starts` a turn
/// ([`turn_starts`]), so that a turn is kept or summarized whole. 0 when
/// nothing would be summarized.
pub(crate) fn cut(costs: &[usize], starts: &[bool], budget: usize) -> usize {
    let mut running = 0;
    let mut cut = 0;
    for (at, cost) in costs.iter().enumerate().rev() {
        if running + cost > budget && costs.len() - at > MIN_KEPT {
            cut = at + 1;
            break;
        }
        running += cost;
    }
    if cut == 0 {
        return 0;
    }
    let turn = |at: &usize| starts.get(*at).copied().unwrap_or(false);
    (cut..costs.len())
        .find(turn)
        .or_else(|| (1..cut).rev().find(turn))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ceiling_is_the_window_or_the_price_s_threshold_below_it() {
        assert_eq!(ceiling(None, Some(272_000)), None);
        assert_eq!(ceiling(Some(0), None), None);
        assert_eq!(ceiling(Some(131_072), None), Some(131_072));
        assert_eq!(ceiling(Some(1_050_000), Some(272_000)), Some(272_000));
        assert_eq!(ceiling(Some(128_000), Some(272_000)), Some(128_000));
        assert!(!due(80_000, 100_000) && due(80_001, 100_000));
        assert_eq!(kept_budget(100_000), 30_000);
        assert_eq!(summary_chars(100_000), 200_000);
    }

    #[test]
    fn what_a_turn_replays_is_charged_to_the_event_before_it() {
        let events = [(10, 5), (20, 7), (30, 1)];
        // A round at 25 follows the event at 20; one at 5 comes before all.
        let replayed = [(5, 100), (25, 40), (31, 2), (21, 3)];
        assert_eq!(costs(&events, &replayed), vec![5, 50, 3]);
        assert_eq!(costs(&[], &replayed), Vec::<usize>::new());
    }

    #[test]
    fn a_turn_starts_with_the_first_message_after_the_agent_s() {
        // A question, its answer; a question, a message steered in, the
        // answer; a question.
        let written = [false, true, false, false, true, false];
        assert_eq!(
            turn_starts(&written),
            vec![true, false, true, false, false, true]
        );
        assert_eq!(turn_starts(&[true, false]), vec![false, true]);
        assert!(turn_starts(&[]).is_empty());
    }

    #[test]
    fn the_last_request_measured_counts_what_came_since() {
        assert_eq!(measured(Some(50_000), &[4_000, 10]), Some(54_010));
        assert_eq!(measured(None, &[4_000]), None);
    }

    #[test]
    fn a_cut_keeps_the_latest_within_the_budget_and_whole_turns() {
        let costs = [10; 10];
        let every = [true; 10];
        // 40 tokens keep the last four.
        assert_eq!(cut(&costs, &every, 40), 6);
        // Never fewer than MIN_KEPT, nothing to summarize when all fit.
        assert_eq!(cut(&costs, &every, 5), 6);
        assert_eq!(cut(&costs, &every, 100), 0);
        // The cut moves on to the next event that starts a turn...
        let turns = [
            true, false, true, false, true, false, true, false, true, false,
        ];
        assert_eq!(cut(&costs, &turns, 50), 6);
        assert_eq!(cut(&costs, &turns, 40), 6);
        assert_eq!(cut(&costs, &turns, 30), 6);
        // ...or back to the one before when none follows.
        let late = [
            true, false, false, true, false, false, false, false, false, false,
        ];
        assert_eq!(cut(&costs, &late, 40), 3);
        assert_eq!(cut(&costs, &[false; 10], 40), 0);
    }
}
