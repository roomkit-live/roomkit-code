//! When a conversation compacts outside its turns (HITL a2dfba34): `/compact`
//! starts at once when no turn is under way in the room, else once the room's
//! turns end; one compaction at a time per room, which a turn's history
//! waits for; and what the views show meanwhile.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use roomkit::model::RoomEvent;
use tokio::sync::watch;

/// What compacts an agent's conversation outside its turns.
pub(crate) trait Compactor: Send + Sync {
    /// Compacts `events`, `room`'s history since the agent's latest
    /// compaction, whatever its tier.
    fn compact_now<'a>(
        &'a self,
        room: &'a str,
        events: Vec<RoomEvent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// What a compaction asked in a room found.
pub(super) enum Asked {
    /// One is running: nothing more starts.
    Running,
    /// A turn is under way: it starts once the room's turns end.
    Queued,
    /// None is running and no turn is under way: it starts now.
    Start(Running),
}

/// One room's turns and compactions.
#[derive(Default)]
struct Room {
    /// The turns admitted and not finished yet.
    turns: usize,
    /// The agent whose compaction waits for the turns to end.
    queued: Option<String>,
    /// The compactions running, and since when.
    running: usize,
    since: Option<Instant>,
}

pub(super) struct Runs {
    rooms: Mutex<HashMap<String, Room>>,
    /// One per room: a compaction holds it, a turn reading its history waits.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// What compacts each agent's conversations, by agent ID.
    compactors: Mutex<HashMap<String, Arc<dyn Compactor>>>,
    changes: Arc<watch::Sender<()>>,
}

impl Runs {
    pub fn new(changes: Arc<watch::Sender<()>>) -> Self {
        Self {
            rooms: Mutex::default(),
            locks: Mutex::default(),
            compactors: Mutex::default(),
            changes,
        }
    }

    pub fn register(&self, agent: &str, compactor: Arc<dyn Compactor>) {
        crate::sync::lock(&self.compactors).insert(agent.into(), compactor);
    }

    pub fn compactor(&self, agent: &str) -> Option<Arc<dyn Compactor>> {
        crate::sync::lock(&self.compactors).get(agent).cloned()
    }

    /// `room`'s lock, which a compaction holds while it runs.
    pub fn lock(&self, room: &str) -> Arc<tokio::sync::Mutex<()>> {
        crate::sync::lock(&self.locks)
            .entry(room.into())
            .or_default()
            .clone()
    }

    /// Asks `agent`'s conversation in `room` to compact, decided under one
    /// lock with the turns that admit and finish.
    pub fn ask(self: &Arc<Self>, room: &str, agent: &str) -> Asked {
        let mut rooms = crate::sync::lock(&self.rooms);
        let state = rooms.entry(room.into()).or_default();
        if state.running > 0 {
            return Asked::Running;
        }
        if state.turns > 0 {
            state.queued = Some(agent.into());
            drop(rooms);
            self.changes.send_replace(());
            return Asked::Queued;
        }
        enter(state);
        drop(rooms);
        Asked::Start(self.started(room))
    }

    /// A turn was admitted in `room`.
    pub fn admitted(&self, room: &str) {
        crate::sync::lock(&self.rooms)
            .entry(room.into())
            .or_default()
            .turns += 1;
    }

    /// A turn of `room`'s finished: the agent whose compaction waited, once
    /// none is left, and the compaction that now runs.
    pub fn finished(self: &Arc<Self>, room: &str) -> Option<(String, Running)> {
        let mut rooms = crate::sync::lock(&self.rooms);
        let state = rooms.entry(room.into()).or_default();
        state.turns = state.turns.saturating_sub(1);
        if state.turns > 0 {
            return None;
        }
        let agent = state.queued.take()?;
        enter(state);
        drop(rooms);
        Some((agent, self.started(room)))
    }

    /// No compaction of `agent`'s waits in `room` any more: a turn ran it.
    pub fn unqueue(&self, room: &str, agent: &str) {
        let mut rooms = crate::sync::lock(&self.rooms);
        if let Some(state) = rooms.get_mut(room)
            && state.queued.as_deref() == Some(agent)
        {
            state.queued = None;
            drop(rooms);
            self.changes.send_replace(());
        }
    }

    /// A compaction runs in `room` until the returned value drops.
    pub fn running(self: &Arc<Self>, room: &str) -> Running {
        enter(
            crate::sync::lock(&self.rooms)
                .entry(room.into())
                .or_default(),
        );
        self.started(room)
    }

    pub fn compacting(&self, room: &str) -> bool {
        crate::sync::lock(&self.rooms)
            .get(room)
            .is_some_and(|state| state.running > 0)
    }

    /// What the views of `room` show while a compaction runs or waits.
    pub fn status(&self, room: &str) -> Option<String> {
        let rooms = crate::sync::lock(&self.rooms);
        let state = rooms.get(room)?;
        if let Some(since) = state.since.filter(|_| state.running > 0) {
            let seconds = since.elapsed().as_secs();
            return Some(format!("Compacting the conversation… {seconds} s"));
        }
        state
            .queued
            .as_ref()
            .map(|_| "Compaction queued: it runs when the agent's turn ends".to_owned())
    }

    fn started(self: &Arc<Self>, room: &str) -> Running {
        self.changes.send_replace(());
        Running {
            runs: self.clone(),
            room: room.into(),
        }
    }
}

fn enter(state: &mut Room) {
    state.running += 1;
    state.since.get_or_insert_with(Instant::now);
}

/// A compaction running in a room.
pub(crate) struct Running {
    runs: Arc<Runs>,
    room: String,
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut rooms = crate::sync::lock(&self.runs.rooms);
        if let Some(state) = rooms.get_mut(&self.room) {
            state.running = state.running.saturating_sub(1);
            if state.running == 0 {
                state.since = None;
            }
        }
        drop(rooms);
        self.runs.changes.send_replace(());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;

    fn runs() -> Arc<Runs> {
        Arc::new(Runs::new(Arc::new(watch::channel(()).0)))
    }

    /// Asked while a turn is under way, a compaction waits for the room's
    /// turns to end, the last of them included, then runs, alone.
    #[test]
    fn a_compaction_asked_during_turns_runs_once_they_end() {
        let runs = runs();
        runs.admitted("room");
        runs.admitted("room");
        assert!(matches!(runs.ask("room", "code"), Asked::Queued));
        assert_eq!(
            runs.status("room").as_deref(),
            Some("Compaction queued: it runs when the agent's turn ends")
        );
        assert!(runs.finished("room").is_none(), "a turn is left");
        let (agent, running) = runs.finished("room").unwrap();
        assert_eq!(agent, "code");
        assert!(runs.compacting("room"));
        assert!(matches!(runs.ask("room", "code"), Asked::Running));
        assert!(
            runs.status("room")
                .unwrap()
                .starts_with("Compacting the conversation…")
        );
        drop(running);
        assert!(!runs.compacting("room"));
        assert_eq!(runs.status("room"), None);
        assert!(matches!(runs.ask("room", "code"), Asked::Start(_)));
        assert!(!runs.compacting("other"));
    }

    /// A turn that compacted the conversation itself leaves nothing to run
    /// once it ends.
    #[test]
    fn a_compaction_a_turn_ran_no_longer_waits() {
        let runs = runs();
        runs.admitted("room");
        assert!(matches!(runs.ask("room", "code"), Asked::Queued));
        runs.unqueue("room", "code");
        assert!(runs.finished("room").is_none());
        assert_eq!(runs.status("room"), None);
    }
}
