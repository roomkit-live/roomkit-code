//! Whose memory compacts: the one answer to "does this agent compact
//! here?", which the agents' assembly states and every request reads.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// The agents whose memory compacts, and where one runs with a
/// configuration of its own that says otherwise.
#[derive(Default)]
pub(super) struct Bounds {
    /// The agents with a known window, by ID.
    agents: Mutex<HashSet<String>>,
    /// What a conversation's own configuration of an agent says, by room
    /// and agent: it stands before the agent's own.
    fitted: Mutex<HashMap<(String, String), bool>>,
}

impl Bounds {
    /// States whether `agent`'s memory compacts, as its agent is assembled.
    pub(super) fn set(&self, agent: &str, bounded: bool) {
        let mut agents = crate::sync::lock(&self.agents);
        if bounded {
            agents.insert(agent.into());
        } else {
            agents.remove(agent);
        }
    }

    /// States whether `agent`'s memory compacts in `room`, where it runs
    /// with a configuration of its own.
    pub(super) fn set_in(&self, room: &str, agent: &str, bounded: bool) {
        crate::sync::lock(&self.fitted).insert((room.into(), agent.into()), bounded);
    }

    /// `agent` runs in `room` with its own configuration again.
    pub(super) fn forget_in(&self, room: &str, agent: &str) {
        crate::sync::lock(&self.fitted).remove(&(room.to_owned(), agent.to_owned()));
    }

    /// Whether `agent`'s memory compacts in `room`.
    pub(super) fn compacts(&self, room: &str, agent: &str) -> bool {
        let fitted = crate::sync::lock(&self.fitted)
            .get(&(room.to_owned(), agent.to_owned()))
            .copied();
        fitted.unwrap_or_else(|| crate::sync::lock(&self.agents).contains(agent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_s_own_configuration_stands_before_the_agent_s() {
        let bounds = Bounds::default();
        assert!(!bounds.compacts("room", "code"));
        bounds.set("code", true);
        assert!(bounds.compacts("room", "code"));
        bounds.set_in("room", "code", false);
        assert!(!bounds.compacts("room", "code") && bounds.compacts("other", "code"));
        bounds.forget_in("room", "code");
        assert!(bounds.compacts("room", "code"));
        bounds.set("code", false);
        assert!(!bounds.compacts("room", "code"));
    }
}
