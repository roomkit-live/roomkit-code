#![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
use super::*;

mod shutdown;

fn compacted(from: u64) -> Compacted {
    Compacted {
        from,
        summary: "The user asked for tests.".into(),
        snapshot: Some("Project instruction snapshot".into()),
        summarized: 12,
        at: from + 9,
        recorded: Some("Recorded by the application: a plan.".into()),
        estimate: None,
        fixed: None,
    }
}

const TOOK: Duration = Duration::from_secs(3);

/// A record whose content does not read is set aside, not replaced by
/// the next compaction; one the system refused this time stays where it
/// is.
#[cfg(unix)]
#[test]
fn only_a_damaged_record_is_set_aside() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let compactions = Compactions::new(dir.path());
    let damaged = compactions.path("damaged");
    std::fs::write(&damaged, "{not a record").unwrap();
    assert_eq!(compactions.get("damaged", "code"), None);
    assert!(!damaged.exists());
    assert!(damaged.with_extension("json.unreadable").exists());
    assert!(compactions.notices("damaged")[0].contains("could not be read"));

    let refused = compactions.path("refused");
    std::fs::write(&refused, "{}").unwrap();
    std::fs::set_permissions(&refused, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&refused).is_ok() {
        // Permissions do not hold for this user, root for one.
        return;
    }
    assert_eq!(compactions.get("refused", "code"), None);
    assert!(refused.exists(), "a record the system refused stays");
    assert!(!refused.with_extension("json.unreadable").exists());
}

#[tokio::test]
async fn a_compaction_is_kept_on_disk_privately_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let compactions = Compactions::new(&dir.path().join("compactions"));
    assert_eq!(compactions.get("room", "code"), None);
    compactions
        .record(
            ("room", "code"),
            (compacted(40), 12),
            ((90_000, 31_000), TOOK, false),
        )
        .await;
    assert_eq!(compactions.get("room", "code"), Some(compacted(40)));
    // Another instance, as after a restart, reads the same.
    let reopened = Compactions::new(&dir.path().join("compactions"));
    assert_eq!(reopened.get("room", "code"), Some(compacted(40)));
    assert_eq!(reopened.get("room", "review"), None);
    assert_eq!(reopened.get("other", "code"), None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = compactions.path("room");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
        let dir = std::fs::metadata(dir.path().join("compactions")).unwrap();
        assert_eq!(dir.permissions().mode() & 0o077, 0);
    }
    let text = "Conversation compacted · 12 messages summarized · ≈90k → ≈31k tokens · 3 s";
    // Every view of the room is shown its line, each time it is sent its
    // events, as long as it is recent.
    for _ in 0..2 {
        assert!(
            matches!(compactions.lines("room"), Some(Item::Message(line)) if line.text == text)
        );
    }
    assert!(compactions.lines("other").is_none());
}

#[tokio::test]
async fn an_unreadable_record_is_none_and_the_next_compaction_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let compactions = Compactions::new(dir.path());
    std::fs::write(compactions.path("room"), "not json").unwrap();
    assert_eq!(compactions.get("room", "code"), None);
    compactions
        .record(("room", "code"), (compacted(7), 12), ((1, 1), TOOK, false))
        .await;
    let reopened = Compactions::new(dir.path());
    assert_eq!(reopened.get("room", "code"), Some(compacted(7)));
}

/// A second `/compact` while the first has not taken its flag yet leaves
/// it: the first still compacts.
#[tokio::test]
async fn a_compaction_asked_twice_keeps_the_first_one_s_flag() {
    let compactions = Arc::new(Compactions::new(Path::new("/nonexistent")));
    compactions.set_bounded("code", true);
    // The task it spawns runs once this test yields.
    assert_eq!(compactions.request("room", "code"), Requested::Started);
    assert_eq!(compactions.request("room", "code"), Requested::Running);
    assert!(compactions.take_forced("room", "code"));
}

#[test]
fn only_an_agent_whose_memory_compacts_can_be_asked_to() {
    let compactions = Compactions::new(Path::new("/nonexistent"));
    assert!(!compactions.force("room", "code"));
    compactions.set_bounded("code", true);
    assert!(compactions.force("room", "code"));
    assert!(compactions.take_forced("room", "code"));
    assert!(!compactions.take_forced("room", "code"));
    compactions.set_bounded("code", false);
    assert!(!compactions.bounded("room", "code"));
}
