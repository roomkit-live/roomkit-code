#![expect(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

use roomkit_tui::protocol::SidebarContent;
use serde_json::json;

use std::time::Duration;

use roomkit::model::{Metadata, RoomId};
use roomkit_ai::{AiChannelOptions, AiTool, Tool, ToolFuture};

use super::record::{Record, merge};
use super::snapshot::Digests;
use super::view::{SIDEBAR_ENTRIES, body, status};
use super::*;

fn effect(path: &str, change: Change, shell: bool) -> Effect {
    Effect {
        path: path.into(),
        change,
        shell,
    }
}

#[test]
fn the_list_keeps_the_net_change_since_the_conversation_started() {
    use Change::{Created, Deleted, Modified};
    assert_eq!(merge(None, Modified), Some(Modified));
    assert_eq!(merge(Some(Created), Modified), Some(Created));
    assert_eq!(merge(Some(Created), Deleted), None);
    assert_eq!(merge(Some(Modified), Deleted), Some(Deleted));
    assert_eq!(merge(Some(Deleted), Created), Some(Modified));

    let mut record = Record::default();
    record.apply(effect("new.txt", Created, false));
    record.apply(effect("new.txt", Modified, true));
    record.apply(effect("old.txt", Modified, true));
    record.apply(effect("gone.txt", Created, false));
    record.apply(effect("gone.txt", Deleted, false));
    record.read("old.txt".into());
    record.read("notes.md".into());
    let changes: Vec<_> = record
        .changes()
        .iter()
        .map(|c| (c.path.as_str(), status(c)))
        .collect();
    // Newest first; a tool's report outranks a later comparison of the same file.
    assert_eq!(
        changes,
        [
            ("old.txt", "Modified · shell".into()),
            ("new.txt", "Created".into())
        ]
    );
    assert_eq!(record.reads(), ["notes.md"]);
}

#[test]
fn file_tools_report_effects_previews_report_nothing() {
    let (effects, read) = tools::reported(
        "write",
        &json!({"path":"a.txt","created":true,"changed":true}),
    );
    assert_eq!(effects, [effect("a.txt", Change::Created, false)]);
    assert_eq!(read, None);
    let (effects, _) = tools::reported("edit", &json!({"path":"a.txt","changed":false}));
    assert!(effects.is_empty(), "An edit that changed nothing");
    let applied = json!({
        "dry_run": false, "completed": true, "previews": [], "failure": null,
        "applied": [
            {"index": 1, "operation": "add", "path": "to.txt"},
            {"index": 1, "operation": "delete", "path": "from.txt"},
            {"index": 2, "operation": "update", "path": "b.txt"}
        ]
    });
    let (effects, _) = tools::reported("patch", &applied);
    assert_eq!(
        effects,
        [
            effect("to.txt", Change::Created, false),
            effect("from.txt", Change::Deleted, false),
            effect("b.txt", Change::Modified, false),
        ]
    );
    let preview =
        json!({"dry_run": true, "completed": true, "previews": [], "applied": [], "failure": null});
    assert!(tools::reported("patch", &preview).0.is_empty());
    assert_eq!(
        tools::reported("read", &json!({"path":"src/main.rs","lines":[]})).1,
        Some("src/main.rs".into())
    );
}

fn put(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Date a file in the past, as one that existed well before a command.
fn age(root: &Path, path: &str) {
    let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(root.join(path))
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();
}

#[test]
fn a_shell_comparison_honors_ignore_rules_and_skips_repository_metadata() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    put(root, ".gitignore", "dist/\n");
    put(root, "src/keep.rs", "same");
    put(root, "src/edit.rs", "before");
    put(root, "src/remove.rs", "x");
    let mut digests = Digests::default();
    let before = snapshot::take(root, &mut digests).unwrap();
    put(root, "src/edit.rs", "after, longer");
    std::fs::remove_file(root.join("src/remove.rs")).unwrap();
    put(root, ".env", "TOKEN=1");
    put(root, "dist/bundle.js", "ignored");
    put(root, ".git/index", "metadata");
    let after = snapshot::take(root, &mut digests).unwrap();
    assert_eq!(
        snapshot::compare(root, &before, &after).unwrap(),
        [
            (".env".into(), Change::Created),
            ("src/edit.rs".into(), Change::Modified),
            ("src/remove.rs".into(), Change::Deleted),
        ]
    );
}

/// Review finding: `sed -i` rewriting identical bytes listed the file as
/// modified, and a command changing `.gitignore` listed every newly ignored
/// file as deleted and every newly visible one as created.
#[test]
fn identical_rewrites_and_changed_ignore_rules_are_not_changes() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    put(root, ".gitignore", "dist/\n");
    for (path, text) in [
        ("same.txt", "abc"),
        ("build/out.js", "old"),
        ("dist/kept.js", "old"),
    ] {
        put(root, path, text);
        age(root, path);
    }
    let mut digests = Digests::default();
    let before = snapshot::take(root, &mut digests).unwrap();
    // The command rewrites identical bytes and swaps the ignored directory.
    put(root, "same.txt", "abc");
    put(root, ".gitignore", "build/\n");
    let after = snapshot::take(root, &mut digests).unwrap();
    assert_eq!(
        snapshot::compare(root, &before, &after).unwrap(),
        [(".gitignore".into(), Change::Modified)]
    );
}

/// Light review finding: unpacking or copying files that keep their old mtime,
/// a `.gitignore` among them, listed nothing.
#[test]
fn unpacked_files_with_old_times_are_still_created() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    put(root, "README.md", "existing");
    let mut digests = Digests::default();
    let before = snapshot::take(root, &mut digests).unwrap();
    // `tar xzf starter.tgz` restores the archive's modification times.
    for path in [".gitignore", "src/lib.rs"] {
        put(root, path, "from the archive");
        age(root, path);
    }
    let after = snapshot::take(root, &mut digests).unwrap();
    assert_eq!(
        snapshot::compare(root, &before, &after).unwrap(),
        [
            (".gitignore".into(), Change::Created),
            ("src/lib.rs".into(), Change::Created),
        ]
    );
}

/// Found in use: a command run in the same step as a `write` or an `edit`
/// saw the file they stage beside their destination, then no longer, and the
/// list held it as deleted.
#[test]
fn a_write_being_staged_is_not_a_file_a_command_changed() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    put(root, "src/lib.rs", "before");
    // A write is staged as the command starts, and renamed before it ends.
    put(root, "src/.roomkit-write-a1B2c3", "after");
    let mut digests = Digests::default();
    let before = snapshot::take(root, &mut digests).unwrap();
    let staged = root.join("src/.roomkit-write-a1B2c3");
    std::fs::rename(staged, root.join("src/lib.rs")).unwrap();
    // Another one is staged as the command ends.
    put(root, ".roomkit-write-d4E5f6", "next");
    let after = snapshot::take(root, &mut digests).unwrap();
    assert_eq!(
        snapshot::compare(root, &before, &after).unwrap(),
        [("src/lib.rs".into(), Change::Modified)]
    );
}

/// A shell that writes a file, then runs until the call is dropped.
struct Hanging(std::path::PathBuf);

impl Tool for Hanging {
    fn definition(&self) -> AiTool {
        AiTool {
            name: "shell".into(),
            description: "Run a command".into(),
            parameters: json!({"type": "object"}),
            tags: Vec::new(),
            changes_state: false,
            defer_loading: false,
        }
    }

    fn handler(&self, _: String, _: Metadata) -> ToolFuture {
        let path = self.0.join("partial.txt");
        Box::pin(async move {
            std::fs::write(path, "written before the cancel").unwrap();
            std::future::pending().await
        })
    }
}

/// Review finding: a cancelled or timed-out shell call lost what it changed.
#[tokio::test]
async fn a_cancelled_shell_call_still_reports_what_it_changed() {
    let root = tempfile::tempdir().unwrap();
    let files = Files::new(root.path());
    let tracked = files.wrap(Arc::new(Hanging(root.path().into())));
    // Cancelling the turn or the tool timeout drops the call mid-command.
    let options = AiChannelOptions {
        tool_timeout: Some(Duration::from_millis(200)),
        ..AiChannelOptions::default()
    };
    let answered = crate::testing::turn(&tracked, "room", "code", "shell", json!({}), options);
    assert!(answered.await.is_none());
    // Closing the application waits for the comparison the drop started.
    files.settle(Duration::from_secs(5)).await;
    let record = files.record("room").await;
    let changes: Vec<_> = record
        .changes()
        .iter()
        .map(|c| (c.path.clone(), status(c)))
        .collect();
    assert_eq!(changes, [("partial.txt".into(), "Created · shell".into())]);
}

/// Every agent slot settles the comparisons as the engine closes it: a
/// comparison that never ends holds up the close once, not once per slot.
#[tokio::test]
async fn closing_waits_once_for_a_comparison_that_never_ends() {
    let root = tempfile::tempdir().unwrap();
    let files = Files::new(root.path());
    files.comparisons.spawn(std::future::pending::<()>());
    let bound = Duration::from_millis(300);
    let first = tokio::time::Instant::now();
    files.settle(bound).await;
    assert!(first.elapsed() >= bound);
    let second = tokio::time::Instant::now();
    files.settle(bound).await;
    assert!(second.elapsed() < bound, "{:?}", second.elapsed());
}

/// Review finding: an unreadable saved list was replaced by an empty one,
/// then saved over the original on the next change.
#[tokio::test]
async fn an_unreadable_saved_list_is_never_overwritten() {
    let state = tempfile::tempdir().unwrap();
    let store = SqliteStore::new(roomkit_sqlite::SqliteStoreOptions {
        path: state.path().join("history.sqlite3"),
    });
    let kit = RoomKit::new(roomkit::RoomKitOptions::with_store(store));
    let room = kit
        .create_room(roomkit::CreateRoomOptions {
            room_id: Some(RoomId::new("room").unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    let unreadable: serde_json::Map<String, serde_json::Value> =
        [("changed".into(), json!(42))].into_iter().collect();
    kit.store()
        .set_private_state(&room.id, STATE_KEY, Some(unreadable.clone()))
        .await
        .unwrap();
    let files = Files::new(state.path());
    files.install(&kit);
    let effects = vec![effect("a.txt", Change::Created, false)];
    files.observed("room", effects, None).await;
    let record = files.record("room").await;
    assert!(record.offline && record.changes().len() == 1);
    assert!(body(&record).contains("could not be read"));
    let stored = kit.store().get_private_state(&room.id, STATE_KEY).await;
    assert_eq!(stored.unwrap(), Some(unreadable));
    kit.close().await.unwrap();
}

#[tokio::test]
async fn the_sidebar_lists_recent_changes_and_counts_files_only_read() {
    let files = Files::new(Path::new("."));
    let changes: Vec<_> = (0..10)
        .map(|i| effect(&format!("f{i}.txt"), Change::Created, false))
        .collect();
    files
        .observed("room", changes, Some("README.md".into()))
        .await;
    let section = files.sidebar("room").await;
    assert_eq!(section.title, "Files");
    let SidebarContent::Entries { entries } = section.content else {
        panic!("Expected entries");
    };
    let labels: Vec<_> = entries.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels[0], "f9.txt");
    assert_eq!(labels[SIDEBAR_ENTRIES], "2 more");
    assert_eq!(labels.last().unwrap(), &"1 file read");
    assert_eq!(entries[0].detail, "Created");
    assert!(files.sidebar("other").await.title == "Files");
    files.compared("room", None, None).await;
    let body = body(&files.record("room").await);
    assert!(body.contains("Changed (10)") && body.contains("Read only (1)"));
    assert!(body.contains("too large to compare"));
}
