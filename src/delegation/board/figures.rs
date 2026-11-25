//! What tells that a conversation's figures changed, and what was read of
//! them: its own agent's cost, and where the readings of each room stand.

use std::collections::HashMap;

use roomkit_tui::protocol::ConversationCost;

#[derive(Default)]
pub(super) struct Figures {
    /// Counts what changes a conversation's figures: a task started or
    /// ended, a turn recorded.
    version: u64,
    /// What each conversation's own agent cost, as of a `version`.
    own_costs: HashMap<String, (u64, Option<ConversationCost>)>,
    /// The turns recorded in each room since launch: a reading of a task's
    /// figures begun before one in its room is stale.
    readings: HashMap<String, u64>,
}

impl Figures {
    /// A task started or ended.
    pub(super) fn changed(&mut self) {
        self.version += 1;
    }

    /// A turn was recorded in `room`: the readings begun there before it
    /// are stale.
    pub(super) fn recorded_in(&mut self, room: &str) {
        self.version += 1;
        *self.readings.entry(room.to_owned()).or_default() += 1;
    }

    pub(super) fn version(&self) -> u64 {
        self.version
    }

    /// Where readings of `room`'s figures stand.
    pub(super) fn reading(&self, room: &str) -> u64 {
        self.readings.get(room).copied().unwrap_or_default()
    }

    /// What conversation `root`'s own agent cost, as read at `version`.
    pub(super) fn own_cost(&self, root: &str, version: u64) -> Option<Option<ConversationCost>> {
        let (kept, cost) = self.own_costs.get(root)?;
        (*kept == version).then(|| cost.clone())
    }

    pub(super) fn keep_own_cost(
        &mut self,
        root: &str,
        version: u64,
        cost: Option<ConversationCost>,
    ) {
        self.own_costs.insert(root.to_owned(), (version, cost));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cost_read_before_the_figures_changed_is_not_kept() {
        let mut figures = Figures::default();
        figures.keep_own_cost("main", figures.version(), None);
        assert_eq!(figures.own_cost("main", figures.version()), Some(None));
        figures.changed();
        assert_eq!(figures.own_cost("main", figures.version()), None);
        let before = (figures.version(), figures.reading("task-room"));
        figures.recorded_in("task-room");
        assert_eq!(figures.version(), before.0 + 1);
        assert_eq!(figures.reading("task-room"), before.1 + 1);
        assert_eq!(figures.reading("other"), 0);
    }
}
