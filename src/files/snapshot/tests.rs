#![expect(clippy::unwrap_used, reason = "test fixtures and assertions")]

use super::*;

#[test]
fn changed_rules_are_read_even_when_their_size_and_modification_time_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::write(root.join(".gitignore"), "one.log\n").unwrap();
    std::fs::write(root.join("one.log"), "existing").unwrap();
    let original = std::fs::metadata(root.join(".gitignore"))
        .unwrap()
        .modified()
        .unwrap();
    let mut digests = Digests::default();
    let before = take(root, &mut digests).unwrap();
    std::fs::write(root.join(".gitignore"), "two.log\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(root.join(".gitignore"))
        .unwrap()
        .set_modified(original)
        .unwrap();
    let after = take(root, &mut digests).unwrap();
    assert_eq!(
        compare(root, &before, &after).unwrap(),
        [(".gitignore".into(), Change::Modified)]
    );
}

#[test]
fn changing_rules_distinguishes_existing_and_new_files_in_an_ignored_directory() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("generated")).unwrap();
    std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    std::fs::write(root.join("generated/existing"), "existing").unwrap();
    std::fs::write(root.join("generated/edited"), "before").unwrap();
    let mut digests = Digests::default();
    let before = take(root, &mut digests).unwrap();
    std::fs::remove_file(root.join(".gitignore")).unwrap();
    std::fs::write(root.join("generated/edited"), "after, longer").unwrap();
    let new = std::fs::File::create(root.join("generated/new")).unwrap();
    new.set_modified(SystemTime::UNIX_EPOCH).unwrap();
    let after = take(root, &mut digests).unwrap();
    assert_eq!(
        compare(root, &before, &after).unwrap(),
        [
            (".gitignore".into(), Change::Deleted),
            ("generated/edited".into(), Change::Modified),
            ("generated/new".into(), Change::Created),
        ]
    );
}

#[test]
fn an_incomplete_inventory_only_prevents_comparison_when_ignore_rules_change() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("generated")).unwrap();
    std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    std::fs::write(root.join("visible"), "before").unwrap();
    std::fs::write(root.join("generated/ignored"), "ignored").unwrap();
    let mut digests = Digests::default();
    let mut before = take(root, &mut digests).unwrap();
    before.known = inventory(root, 2);
    assert!(
        before.known.is_none(),
        "An ignored tree also counts toward the limit"
    );
    // Rewriting the same rules is still an ordinary comparison, even when its
    // metadata changes and the inventory could not cover a large ignored tree.
    std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(root.join(".gitignore"))
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH)
        .unwrap();
    std::fs::write(root.join("visible"), "after, longer").unwrap();
    let after = take(root, &mut digests).unwrap();
    assert_eq!(
        compare(root, &before, &after).unwrap(),
        [("visible".into(), Change::Modified)]
    );
    std::fs::remove_file(root.join(".gitignore")).unwrap();
    let after = take(root, &mut digests).unwrap();
    assert!(compare(root, &before, &after).is_none());
}

#[test]
fn an_inventory_excludes_metadata_staging_and_symlinked_trees() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::write(root.path().join(".git/index"), "metadata").unwrap();
    std::fs::write(root.path().join(".roomkit-write-temporary"), "staged").unwrap();
    std::fs::write(outside.path().join("outside"), "external").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
    assert!(inventory(root.path(), LIMIT).unwrap().is_empty());
    assert!(inventory(&root.path().join("absent"), LIMIT).is_none());
}

/// Review finding: a command that rewrote a file with as many bytes and put
/// its modification time back was reported as changing nothing. A digest was
/// reused on the file's size and time alone, and a file past the digest limit
/// was compared on them alone; its inode change time moved all the same.
#[test]
fn a_rewrite_restoring_size_and_modification_time_is_still_a_change() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let large = usize::try_from(DIGESTED).unwrap() + 1;
    std::fs::write(root.join("small"), "before").unwrap();
    std::fs::write(root.join("large"), vec![b'a'; large]).unwrap();
    let mut digests = Digests::default();
    let before = take(root, &mut digests).unwrap();
    assert!(before.files["small"].digest.is_some());
    assert!(before.files["large"].digest.is_none());
    // A kernel without fine-grained timestamps moves the change time once a
    // tick, up to 4 ms: the rewrite comes in a later one.
    std::thread::sleep(std::time::Duration::from_millis(20));
    for (name, content) in [("small", b"after!".to_vec()), ("large", vec![b'b'; large])] {
        let path = root.join(name);
        let original = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, content).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(original)
            .unwrap();
    }
    let after = take(root, &mut digests).unwrap();
    assert_eq!(
        compare(root, &before, &after).unwrap(),
        [
            ("large".into(), Change::Modified),
            ("small".into(), Change::Modified)
        ]
    );
}
