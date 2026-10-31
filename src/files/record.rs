//! One conversation's net file changes, and the rules that merge them.

use serde::{Deserialize, Serialize};

/// Paths kept per conversation, for changes and for reads; the oldest go first.
pub(super) const RETAINED: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Change {
    Created,
    Modified,
    Deleted,
}

/// One observed effect on a workspace-relative path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Effect {
    pub path: String,
    pub change: Change,
    /// Found by comparing the project around a shell command.
    pub shell: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Changed {
    pub path: String,
    pub change: Change,
    #[serde(default)]
    pub shell: bool,
    seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Read {
    path: String,
    seq: u64,
}

/// One conversation's net effects since it started.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Record {
    #[serde(default)]
    changed: Vec<Changed>,
    #[serde(default)]
    read: Vec<Read>,
    #[serde(default)]
    seq: u64,
    /// A shell command ran in a project too large to compare.
    #[serde(default)]
    pub untracked: bool,
    /// Entries beyond the retained count were dropped, oldest first.
    #[serde(default)]
    pub trimmed: bool,
    /// The last save failed: the list may not survive a restart.
    #[serde(skip)]
    pub unsaved: bool,
    /// The stored list could not be read. This one only holds what happened
    /// since, and is never saved over it.
    #[serde(skip)]
    pub offline: bool,
}

/// The net change since the conversation started, given the previous one.
pub(super) fn merge(previous: Option<Change>, observed: Change) -> Option<Change> {
    use Change::{Created, Deleted, Modified};
    match (previous, observed) {
        (None, change) => Some(change),
        // Created, then removed: nothing of it remains.
        (Some(Created), Deleted) => None,
        (Some(Created), Created | Modified) => Some(Created),
        (Some(Modified | Deleted), Deleted) => Some(Deleted),
        // It existed before, and it exists again.
        (Some(Modified | Deleted), Created | Modified) => Some(Modified),
    }
}

impl Record {
    /// A record for a list that could not be read: it is never saved over it.
    pub fn offline() -> Self {
        Self {
            offline: true,
            ..Self::default()
        }
    }

    pub fn apply(&mut self, effect: Effect) {
        self.seq += 1;
        let index = self.changed.iter().position(|c| c.path == effect.path);
        let previous = index.map(|i| self.changed[i].clone());
        let Some(change) = merge(previous.as_ref().map(|c| c.change), effect.change) else {
            if let Some(index) = index {
                self.changed.remove(index);
            }
            return;
        };
        // A tool's own report outranks a comparison that saw the same file.
        let shell = previous.map_or(effect.shell, |p| p.shell && effect.shell);
        let entry = Changed {
            path: effect.path,
            change,
            shell,
            seq: self.seq,
        };
        match index {
            Some(index) => self.changed[index] = entry,
            None => self.changed.push(entry),
        }
        self.trimmed |= retain_newest(&mut self.changed, |c| c.seq);
    }

    pub fn read(&mut self, path: String) {
        self.seq += 1;
        self.read.retain(|r| r.path != path);
        self.read.push(Read {
            path,
            seq: self.seq,
        });
        self.trimmed |= retain_newest(&mut self.read, |r| r.seq);
    }

    /// Changes, newest first.
    pub fn changes(&self) -> Vec<&Changed> {
        let mut changes: Vec<_> = self.changed.iter().collect();
        changes.sort_by_key(|c| std::cmp::Reverse(c.seq));
        changes
    }

    /// Files read and never changed, newest first.
    pub fn reads(&self) -> Vec<&str> {
        let mut reads: Vec<_> = self
            .read
            .iter()
            .filter(|r| !self.changed.iter().any(|c| c.path == r.path))
            .collect();
        reads.sort_by_key(|r| std::cmp::Reverse(r.seq));
        reads.into_iter().map(|r| r.path.as_str()).collect()
    }
}

/// Whether older items were dropped.
fn retain_newest<T>(items: &mut Vec<T>, seq: impl Fn(&T) -> u64) -> bool {
    if items.len() <= RETAINED {
        return false;
    }
    items.sort_by_key(|item| std::cmp::Reverse(seq(item)));
    items.truncate(RETAINED);
    true
}
