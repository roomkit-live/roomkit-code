use super::*;

#[test]
#[allow(clippy::panic, reason = "The reader fails midway on purpose")]
fn a_reload_that_fails_midway_keeps_the_previous_catalogue() -> crate::Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let private = tempfile::tempdir()?;
    std::fs::write(
        state.path().join("hooks.json"),
        serde_json::json!({"hooks": {"PreToolUse": [{"hooks": [
            {"type":"command", "command":"exit 2"}
        ]}]}})
        .to_string(),
    )?;
    let mut catalogue = Catalogue::load(workspace.path(), state.path(), private.path());
    let id = catalogue.entries[0].id.clone();
    catalogue.set_enabled(&id, true)?;
    // The trust file reads; the first source fails.
    let trust = catalogue.trust.clone();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        catalogue.reload_with(|path, limit| {
            if path == trust {
                read(path, limit)
            } else {
                panic!("source unreadable")
            }
        });
    }));
    assert!(failed.is_err());
    assert_eq!(catalogue.entries.len(), 1);
    assert!(catalogue.enabled.contains(&id));
    assert!(catalogue.problems.is_empty());
    Ok(())
}

/// Review finding: trust covered the command's text, so `./check.sh`
/// stayed trusted whatever the agent later wrote in the script.
#[test]
fn a_review_covers_the_project_scripts_the_command_names() -> crate::Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let private = tempfile::tempdir()?;
    std::fs::write(workspace.path().join("check.sh"), "exit 0\n")?;
    // A file the command reads or writes is no script of its own.
    std::fs::write(workspace.path().join("seen.log"), "")?;
    std::fs::create_dir(workspace.path().join("tools"))?;
    std::fs::write(workspace.path().join("tools/run.sh"), "exit 0\n")?;
    std::fs::write(
        state.path().join("hooks.json"),
        serde_json::json!({"hooks": {"PreToolUse": [{"hooks": [
            {"type":"command", "command":"sh \"$ROOMKIT_PROJECT_DIR/check.sh\" --strict"},
            {"type":"command", "command":"cat >/dev/null; MODE=strict ./check.sh"},
            {"type":"command", "command":"cat check.sh >> seen.log; /bin/echo unrelated"},
            {"type":"command", "command":"timeout 5 ./check.sh"},
            {"type":"command", "command":"exec env MODE=x \"$ROOMKIT_PROJECT_DIR\"/check.sh"},
            {"type":"command", "command":"cd tools && ./run.sh", "timeout": 5},
            {"type":"command", "command":"(cd tools && ./run.sh); ./check.sh", "timeout": 5}
        ]}]}})
        .to_string(),
    )?;
    let mut catalogue = Catalogue::load(workspace.path(), state.path(), private.path());
    let reviewed: Vec<Entry> = catalogue.entries.clone();
    let pinned: Vec<Vec<&str>> = reviewed.iter().map(Entry::pinned).collect();
    assert_eq!(
        pinned,
        [
            vec!["check.sh"],
            vec!["check.sh"],
            vec![],
            vec!["check.sh"],
            vec!["check.sh"],
            vec!["tools/run.sh"],
            // A `cd` in a subshell ends with it.
            vec!["tools/run.sh", "check.sh"]
        ]
    );
    // A hook that launches no project script keeps the identifier it had
    // before scripts were read: the trust already given it holds.
    let unpinned = &reviewed[2];
    let before = serde_json::to_vec(&(
        &unpinned.source,
        unpinned.event,
        &unpinned.group.matcher,
        &unpinned.hook,
    ))?;
    assert_eq!(unpinned.id, crate::digest::sha256_hex(before));
    catalogue.set_enabled(&reviewed[0].id, true)?;
    assert!(reviewed.iter().all(Entry::scripts_unchanged));
    std::fs::write(workspace.path().join("check.sh"), "rm -rf .\n")?;
    // The entries a turn holds no longer run the script, and the
    // catalogue read anew asks for another review.
    assert!(!reviewed[0].scripts_unchanged() && !reviewed[1].scripts_unchanged());
    assert!(reviewed[2].scripts_unchanged());
    catalogue.reload();
    assert_ne!(catalogue.entries[0].id, reviewed[0].id);
    assert_eq!(catalogue.entries[2].id, reviewed[2].id);
    assert!(catalogue.selected().is_empty());
    Ok(())
}

#[test]
fn every_unreadable_file_is_reported_after_a_reload() -> crate::Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let private = tempfile::tempdir()?;
    // A damaged trust file disables every hook: the user must be told.
    std::fs::write(private.path().join("hooks-trust.json"), "{not json")?;
    let catalogue = Catalogue::load(workspace.path(), state.path(), private.path());
    assert_eq!(
        catalogue.problems,
        ["Invalid hook trust file; all hooks require review"]
    );
    std::fs::remove_file(private.path().join("hooks-trust.json"))?;
    std::fs::create_dir(private.path().join("hooks-trust.json"))?;
    std::fs::write(state.path().join("hooks.json"), "{not json")?;
    let catalogue = Catalogue::load(workspace.path(), state.path(), private.path());
    assert_eq!(catalogue.problems.len(), 2, "{:?}", catalogue.problems);
    Ok(())
}

#[test]
fn simultaneous_trust_edits_preserve_both_choices() -> crate::Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let private = tempfile::tempdir()?;
    std::fs::write(
        state.path().join("hooks.json"),
        serde_json::json!({"hooks": {
            "TurnStart": [{"hooks": [
                {"type":"command", "command":"echo first"},
                {"type":"command", "command":"echo second"}
            ]}]
        }})
        .to_string(),
    )?;
    let one = Catalogue::load(workspace.path(), state.path(), private.path());
    let two = Catalogue::load(workspace.path(), state.path(), private.path());
    assert_eq!(one.entries.len(), 2);
    let first = one.entries[0].id.clone();
    let second = two.entries[1].id.clone();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|threads| {
        let jobs = [(one, first.clone()), (two, second.clone())]
            .into_iter()
            .map(|(mut catalogue, id)| {
                let barrier = &barrier;
                threads.spawn(move || {
                    barrier.wait();
                    catalogue.set_enabled(&id, true)
                })
            })
            .collect::<Vec<_>>();
        for job in jobs {
            assert!(matches!(job.join(), Ok(Ok(()))));
        }
    });
    let actual = Catalogue::load(workspace.path(), state.path(), private.path());
    assert!(actual.enabled.contains(&first) && actual.enabled.contains(&second));
    Ok(())
}
