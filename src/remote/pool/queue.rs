//! Owner-local waiting order. Redis arbitrates capacity across independent owners.
use super::{
    Requirements,
    registry::{Row, Worker},
};
use crate::sync::lock;
use roomkit::turns::TurnRequest;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub(super) struct Queue {
    pub admission: tokio::sync::Mutex<super::inventory::Snapshot>,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    next: u64,
    active: BTreeMap<u64, Entry>,
    recent: VecDeque<Entry>,
}
struct Entry {
    room: String,
    parent: String,
    task: String,
    delegated_turn: Option<String>,
    closed: tokio_util::sync::CancellationToken,
    title: String,
    req: Requirements,
    pinned: Option<String>,
    peer: Option<super::consultation::PeerTurn>,
    reason: String,
    placement: Option<String>,
}

/// Only the original delegation can be addressed from the parent. Later
/// private messages use the same child room and task metadata.
pub(super) fn delegated_turn(request: &TurnRequest) -> bool {
    let status = request
        .context
        .room
        .metadata
        .get("task_status")
        .and_then(serde_json::Value::as_str);
    matches!(status, Some("pending" | "in_progress"))
        && request.event.source.channel_id == roomkit::model::ChannelId::system()
}

impl Entry {
    fn accepts(&self, w: &Worker) -> bool {
        self.pinned.as_ref().is_none_or(|name| name == &w.name) && self.req.matches(w)
    }
    fn describe(&self, active: bool) -> String {
        let placement = match &self.placement {
            Some(w) if active => format!("running on {w}"),
            Some(w) => format!("assigned to {w}"),
            None => match &self.req.profile {
                Some(profile) => format!("queued · {} · profile {profile}", self.reason),
                None => format!("queued · {}", self.reason),
            },
        };
        format!("{} · {placement} · {}", self.task, self.title)
    }
}
pub(super) struct Ticket {
    queue: Arc<Queue>,
    id: u64,
}
impl Queue {
    pub fn enter(self: &Arc<Self>, r: &TurnRequest, req: Requirements) -> Ticket {
        let metadata = &r.context.room.metadata;
        let field = |k| {
            metadata
                .get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let pinned = r
            .previous_turn
            .as_ref()
            .and_then(|t| t.executor_id.as_ref())
            .and_then(|id| id.split_once('~'))
            .map(|(name, _)| name.into());
        let pinned = super::post_target(r)
            .or_else(|| {
                metadata
                    .get("task_context")
                    .and_then(|c| c.get("executor_target"))
            })
            .and_then(|target| target.get("worker"))
            .and_then(|w| w.as_str())
            .map(str::to_owned)
            .or(pinned);
        let entry = Entry {
            room: r.room_id.to_string(),
            parent: field("parent_room_id"),
            task: field("task_id"),
            closed: Default::default(),
            delegated_turn: delegated_turn(r).then(|| r.turn_id.clone()),
            title: field("task_input")
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect(),
            peer: None,
            req,
            pinned,
            reason: "checking workers".into(),
            placement: None,
        };
        let mut state = lock(&self.state);
        let id = state.next;
        state.next += 1;
        state.active.insert(id, entry);
        Ticket {
            queue: self.clone(),
            id,
        }
    }
    /// Only a placed turn can receive a question. Its token ends with its slot.
    pub(super) fn peer(&self, room: &str) -> Option<super::consultation::PeerTurn> {
        lock(&self.state)
            .active
            .values()
            .find(|e| e.room == room && e.placement.is_some())
            .and_then(|e| e.peer.clone())
    }

    /// Resolve the exact admitted turn in the calling conversation. A task
    /// queued for a worker or already handed back cannot be steered.
    pub(super) fn task_turn(
        &self,
        parent: &str,
        task: &str,
    ) -> Option<(String, String, tokio_util::sync::CancellationToken)> {
        let state = lock(&self.state);
        let entry = state.active.values().find(|e| {
            e.parent == parent
                && e.task == task
                && e.placement.is_some()
                && e.delegated_turn.is_some()
        })?;
        Some((
            entry.room.clone(),
            entry.delegated_turn.clone()?,
            entry.closed.clone(),
        ))
    }

    pub(super) fn placement(&self, room: &str) -> Option<String> {
        lock(&self.state)
            .active
            .values()
            .find(|e| e.room == room)?
            .placement
            .clone()
    }

    pub fn report(&self, room: &str) -> Vec<String> {
        let state = lock(&self.state);
        state
            .active
            .values()
            .map(|e| (e, true))
            .chain(state.recent.iter().rev().map(|e| (e, false)))
            .filter(|(e, _)| e.room == room || (e.parent == room && e.delegated_turn.is_some()))
            .map(|(e, active)| e.describe(active))
            .collect()
    }
}
impl Ticket {
    pub fn candidates<'a>(&self, rows: &'a [Row]) -> Vec<&'a Worker> {
        let mut state = lock(&self.queue.state);
        let Some(entry) = state.active.get(&self.id) else {
            return vec![];
        };
        let mut matched = false;
        let mut enough_ram = false;
        let mut ranked = Vec::new();
        for r in rows
            .iter()
            .filter(|r| r.online() && entry.accepts(&r.worker))
        {
            matched = true;
            let w = &r.worker;
            if w.metrics.available_mib < entry.req.min_ram_mib {
                continue;
            }
            enough_ram = true;
            if r.used >= w.slots
                || state.active.range(..self.id).any(|(_, e)| {
                    e.placement.is_none()
                        && e.accepts(w)
                        && e.req.min_ram_mib <= w.metrics.available_mib
                })
            {
                continue;
            }
            ranked.push(w);
        }
        ranked.sort_by(|a, b| {
            a.metrics
                .compare_load(&b.metrics)
                .then_with(|| a.name.cmp(&b.name))
        });
        if let Some(e) = state.active.get_mut(&self.id) {
            e.reason = if enough_ram {
                "all matching slots busy or reserved for an earlier task"
            } else if matched {
                "insufficient available RAM"
            } else {
                if e.req.consultation {
                    "no online worker matches placement and the required consultation tools"
                } else if e.req.test_commit.is_some() {
                    "no online worker matches the requested placement and supports cargo_test at a commit on this profile"
                } else {
                    "no online worker matches OS, profile, labels or workspace"
                }
            }
            .into();
        }
        ranked
    }
    pub fn assign(&self, placement: &str, worker: &str, turn: &str) {
        if let Some(e) = lock(&self.queue.state).active.get_mut(&self.id) {
            e.placement = Some(placement.into());
            e.peer = e.req.consultation.then(|| super::consultation::PeerTurn {
                worker: worker.into(),
                id: turn.into(),
                closed: Default::default(),
            });
        }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = lock(&self.queue.state);
        let Some(mut entry) = state.active.remove(&self.id) else {
            return;
        };
        entry.closed.cancel();
        if let Some(peer) = entry.peer.take() {
            peer.closed.cancel();
        }
        if entry.placement.is_some() {
            state.recent.push_back(entry);
            if state.recent.len() > 32 {
                state.recent.pop_front();
            }
        }
    }
}
