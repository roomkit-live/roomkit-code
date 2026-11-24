use super::*;

#[tokio::test]
async fn concurrent_calls_preserve_the_branch_index_and_untracked_files() -> crate::Result<()> {
    let repo = tempfile::tempdir()?;
    let root = repo.path();
    let sub = root.join("crate");
    tokio::fs::create_dir(&sub).await?;
    let file = sub.join("note");
    tokio::fs::write(&file, "committed").await?;
    assert!(Checkout::sample(root).await.0.is_none());
    for args in [
        vec!["init", "-b", "topic"],
        vec!["add", "crate"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "commit",
            "-m",
            "target",
        ],
    ] {
        assert!(value(root, &args).await.is_some());
    }
    let facts = Checkout::sample(&sub)
        .await
        .1
        .ok_or("missing Git metadata")?;
    assert_eq!(facts.branch, "topic");
    assert!(!facts.dirty);
    let sha = &facts.head;
    assert!(value(root, &["switch", "-c", "ongoing"]).await.is_some());
    tokio::fs::write(&file, "staged").await?;
    assert!(value(root, &["add", "crate/note"]).await.is_some());
    tokio::fs::write(&file, "local").await?;
    tokio::fs::write(sub.join("untracked"), "local").await?;
    let status = value(root, &["status", "--porcelain=v1"]).await;
    let (first, second) =
        tokio::join!(TestCheckout::open(&sub, sha), TestCheckout::open(&sub, sha));
    let (first, second) = (first?, second?);
    assert_ne!(first.root, second.root);
    assert_eq!(&first.head, sha);
    assert_eq!(
        tokio::fs::read_to_string(first.root.join("note")).await?,
        "committed"
    );
    assert!(!first.root.join("untracked").exists());
    let temporary = first.root.clone();
    first.close().await?;
    second.close().await?;
    assert!(!temporary.exists());
    assert_eq!(tokio::fs::read_to_string(file).await?, "local");
    assert_eq!(value(root, &["status", "--porcelain=v1"]).await, status);
    let after = Checkout::sample(root).await.1.ok_or("missing checkout")?;
    assert_eq!(after.branch, "ongoing");
    assert!(after.dirty);
    assert!(TestCheckout::open(root, &"0".repeat(40)).await.is_err());
    assert!(TestCheckout::open(root, "HEAD").await.is_err());
    Ok(())
}
