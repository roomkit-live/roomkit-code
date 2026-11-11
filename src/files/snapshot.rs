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

/// How the file tools name the file they stage a write in, beside its
/// destination, until they rename it (roomkit-tools-fs). It is never a
/// project file: a command run beside a write saw it before and no longer
/// after, and listed it as deleted.
const STAGED: &str = ".roomkit-write-";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stat {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct File {
    stat: Stat,
    /// Compared between snapshots, never against the wall clock.
    changed: Option<SystemTime>,
    digest: Option<[u8; 32]>,
}

impl File {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            stat: Stat {
                len: metadata.len(),
                modified: metadata.modified().ok(),
            },
            changed: changed(metadata),
            digest: None,
        }
    }
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

/// Visible files and a bounded inventory that also includes ignored paths.
pub(super) struct Snapshot {
    files: HashMap<String, File>,
    /// An incomplete inventory cannot prove that a newly visible path is new.
    known: Option<HashMap<String, File>>,
}

/// Read names and metadata, never ignored contents. Filesystem timestamps can
/// be less precise than the clock, so they cannot prove whether a path existed.
/// Failure or a large ignored tree only disables comparisons of changed rules.
fn inventory(root: &Path, limit: usize) -> Option<HashMap<String, File>> {
    let walk = WalkBuilder::new(root)
        .standard_filters(false)
        .follow_links(false)
        .filter_entry(|entry| {
            !METADATA.iter().any(|name| entry.file_name() == *name)
                && !entry.file_name().to_string_lossy().starts_with(STAGED)
        })
        .build();
    let mut paths = HashMap::new();
    for (count, entry) in walk.enumerate() {
        if count > limit {
            return None;
        }
        let entry = entry.ok()?;
        if entry.file_type()?.is_file() {
            paths.insert(
                display(entry.path().strip_prefix(root).ok()?),
                File::from_metadata(&entry.metadata().ok()?),
            );
        }
    }
    Some(paths)
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
        // Rules decide which other paths are visible. Always read their bytes:
        // editors and archive tools can preserve both their size and mtime.
        if !is_rule(path)
            && let Some((known, digest)) = self.0.get(path)
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
/// project files; a write the file tools are staging is not. `None` when the
/// project holds more than `LIMIT` files.
pub(super) fn take(root: &Path, digests: &mut Digests) -> Option<Snapshot> {
    let known = inventory(root, LIMIT);
    let walk = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| !METADATA.iter().any(|name| entry.file_name() == *name))
        .build();
    // Count files before reading any: a project over the limit costs a walk.
    let mut found = Vec::new();
    for entry in walk.flatten() {
        let staged = entry.file_name().to_string_lossy().starts_with(STAGED);
        if staged || !entry.file_type().is_some_and(|kind| kind.is_file()) {
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
            let mut file = File::from_metadata(&metadata);
            file.digest = digests.of(&path, file.stat, &absolute, &mut budget);
            (path, file)
        })
        .collect();
    digests.0.retain(|path, _| files.contains_key(path));
    Some(Snapshot { files, known })
}

/// Workspace-relative, `/`-separated, as the file tools report paths.
pub(super) fn display(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// A changed rule can hide or reveal files that the command did not touch.
fn rules_changed(before: &Snapshot, after: &Snapshot) -> bool {
    before
        .files
        .keys()
        .chain(after.files.keys())
        .filter(|path| is_rule(path))
        .any(
            |path| match (before.files.get(path), after.files.get(path)) {
                (Some(then), Some(now)) => match (then.digest, now.digest) {
                    (Some(then), Some(now)) => then != now,
                    _ => then.stat != now.stat || then.changed != now.changed,
                },
                _ => true,
            },
        )
}

fn is_rule(path: &str) -> bool {
    RULES
        .iter()
        .any(|rule| path.rsplit('/').next() == Some(*rule))
}

/// Created, modified and deleted files, sorted by path. Unknown when ignore
/// rules changed and the earlier inventory could not establish which existed.
pub(super) fn compare(
    root: &Path,
    before: &Snapshot,
    after: &Snapshot,
) -> Option<Vec<(String, Change)>> {
    let known = if rules_changed(before, after) {
        Some(before.known.as_ref()?)
    } else {
        None
    };
    let mut changes: Vec<_> = after
        .files
        .iter()
        .filter_map(|(path, now)| {
            let change = match before.files.get(path) {
                None => revealed(known.and_then(|paths| paths.get(path)), now),
                Some(then) if modified(then, now) => Some(Change::Modified),
                Some(_) => None,
            };
            change.map(|change| (path.clone(), change))
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
    Some(changes)
}

/// A newly visible path may have existed, and been edited, while ignored.
fn revealed(known: Option<&File>, now: &File) -> Option<Change> {
    let Some(known) = known else {
        return Some(Change::Created);
    };
    (known.stat != now.stat || known.changed != now.changed).then_some(Change::Modified)
}

/// Compare known content first, falling back to the file's size and time.
fn modified(then: &File, now: &File) -> bool {
    match (then.digest, now.digest) {
        (Some(then), Some(now)) => then != now,
        _ => then.stat != now.stat,
    }
}

#[cfg(test)]
mod tests;
