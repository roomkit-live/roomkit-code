//! What a shell command changed: the project's files before and after it ran.

use std::{collections::HashMap, path::Path, time::SystemTime};

use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

use super::Change;

/// Files compared around one command. A larger project is not compared at all,
/// rather than reporting a partial difference as complete.
const LIMIT: usize = 50_000;

/// Files up to this size are compared by content when only their time changed,
/// so a command rewriting identical bytes (`sed -i`, `touch`) changes nothing.
const DIGESTED: u64 = 1024 * 1024;

/// Bytes read for new digests by one snapshot. A large project's digests fill
/// over several commands instead of delaying the first one.
const BUDGET: u64 = 64 * 1024 * 1024;

/// Version-control metadata is never project content, even when not ignored.
const METADATA: [&str; 4] = [".git", ".hg", ".svn", ".jj"];

/// Files whose change can hide or reveal others that nobody touched.
const RULES: [&str; 2] = [".gitignore", ".ignore"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stat {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct File {
    stat: Stat,
    /// When the file itself last changed (its inode change time on Unix).
    /// Copying, unpacking or moving sets it to now, whatever their mtime.
    changed: Option<SystemTime>,
    digest: Option<[u8; 32]>,
}

#[cfg(unix)]
fn changed(metadata: &std::fs::Metadata) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt;
    let seconds = u64::try_from(metadata.ctime()).ok()?;
    let nanos = u32::try_from(metadata.ctime_nsec()).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::new(seconds, nanos))
}

#[cfg(not(unix))]
fn changed(metadata: &std::fs::Metadata) -> Option<SystemTime> {
    metadata.created().ok()
}

/// The project's files, and when the walk started.
pub(super) struct Snapshot {
    files: HashMap<String, File>,
    taken: SystemTime,
}

/// Content digests from earlier snapshots, reused while a file's size and time
/// are unchanged: only the first snapshot reads every small file.
#[derive(Default)]
pub(super) struct Digests(HashMap<String, (Stat, [u8; 32])>);

impl Digests {
    /// The file's digest, read only while `budget` allows it.
    fn of(&mut self, path: &str, stat: Stat, file: &Path, budget: &mut u64) -> Option<[u8; 32]> {
        if stat.len > DIGESTED {
            return None;
        }
        if let Some((known, digest)) = self.0.get(path)
            && *known == stat
        {
            return Some(*digest);
        }
        *budget = budget.checked_sub(stat.len)?;
        let digest: [u8; 32] = Sha256::digest(std::fs::read(file).ok()?).into();
        self.0.insert(path.to_owned(), (stat, digest));
        Some(digest)
    }
}

/// Walk `root`, honoring `.gitignore`, `.ignore` and global excludes even
/// outside a repository. Hidden files are included: `.env` or `.github` are
/// project files. `None` when the project holds more than `LIMIT` files.
pub(super) fn take(root: &Path, digests: &mut Digests) -> Option<Snapshot> {
    let taken = SystemTime::now();
    let walk = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| !METADATA.iter().any(|name| entry.file_name() == *name))
        .build();
    // Count files before reading any: a project over the limit costs a walk.
    let mut found = Vec::new();
    for entry in walk.flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let (Ok(metadata), Ok(relative)) = (entry.metadata(), entry.path().strip_prefix(root))
        else {
            continue;
        };
        found.push((display(relative), metadata, entry.into_path()));
        if found.len() > LIMIT {
            return None;
        }
    }
    let mut budget = BUDGET;
    let files: HashMap<_, _> = found
        .into_iter()
        .map(|(path, metadata, absolute)| {
            let stat = Stat {
                len: metadata.len(),
                modified: metadata.modified().ok(),
            };
            let digest = digests.of(&path, stat, &absolute, &mut budget);
            let file = File {
                stat,
                changed: changed(&metadata),
                digest,
            };
            (path, file)
        })
        .collect();
    digests.0.retain(|path, _| files.contains_key(path));
    Some(Snapshot { files, taken })
}

/// Workspace-relative, `/`-separated, as the file tools report paths.
pub(super) fn display(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Created, modified and deleted files, sorted by path.
pub(super) fn compare(root: &Path, before: &Snapshot, after: &Snapshot) -> Vec<(String, Change)> {
    // A changed ignore rule hides files that still exist and reveals files that
    // existed before the command: neither was touched by it.
    let rules_changed = before
        .files
        .keys()
        .chain(after.files.keys())
        .filter(|path| {
            RULES
                .iter()
                .any(|rule| path.rsplit('/').next() == Some(*rule))
        })
        .any(|path| before.files.get(path) != after.files.get(path));
    let mut changes: Vec<_> = after
        .files
        .iter()
        .filter_map(|(path, now)| match before.files.get(path) {
            None if rules_changed && now.changed.is_some_and(|c| c < before.taken) => None,
            None => Some((path.clone(), Change::Created)),
            Some(then) if modified(then, now) => Some((path.clone(), Change::Modified)),
            Some(_) => None,
        })
        .chain(
            before
                .files
                .keys()
                .filter(|path| !after.files.contains_key(*path))
                .filter(|path| std::fs::symlink_metadata(root.join(path)).is_err())
                .map(|path| (path.clone(), Change::Deleted)),
        )
        .collect();
    changes.sort_by(|a, b| a.0.cmp(&b.0));
    changes
}

/// A new size or time, unless the content is known to be identical.
fn modified(then: &File, now: &File) -> bool {
    then.stat != now.stat && (then.digest.is_none() || then.digest != now.digest)
}
