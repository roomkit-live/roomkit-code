#![allow(clippy::unwrap_used)]
use super::*;

fn write(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[tokio::test]
async fn global_root_and_nested_precedence_never_load_siblings_or_outside_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    write(root.path(), "AGENTS.md", "OUTSIDE");
    let project = root.path().join("project");
    write(&project, "AGENTS.md", "ROOT");
    write(&project, "src/AGENTS.md", "SHADOWED");
    write(&project, "src/AGENTS.override.md", "OVERRIDE");
    write(&project, "src/deep/AGENTS.md", "DEEP");
    write(&project, "other/AGENTS.md", "SIBLING");
    write(global.path(), "AGENTS.md", "GLOBAL");
    let loader = Instructions::new(&project, global.path());
    let base = loader.load(vec![], true).await;
    assert_eq!(
        base.sources
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>(),
        ["GLOBAL", "ROOT"]
    );
    let report = loader
        .load(
            vec!["src/deep/new.rs".into(), "src/deep/other.rs".into()],
            true,
        )
        .await;
    assert_eq!(
        report
            .sources
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>(),
        ["GLOBAL", "ROOT", "OVERRIDE", "DEEP"]
    );
    assert!(report.problems.is_empty());
    assert_eq!(report.sources[3].scope, "src/deep");
    assert!(!report.render().contains("SIBLING"));
    assert!(!report.render().contains("OUTSIDE"));
    assert_eq!(
        loader.load(vec!["other".into()], false).await.sources[0].text,
        "SIBLING"
    );
}

#[tokio::test]
async fn refreshed_snapshots_report_changes_removals_and_empty_override_fallback() {
    let root = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    write(root.path(), "src/AGENTS.md", "VERSION_ONE");
    write(root.path(), "src/AGENTS.override.md", " \n");
    let loader = Instructions::new(root.path(), global.path());
    let before = loader.load(vec!["src/new/file.rs".into()], false).await;
    assert_eq!(before.sources[0].text, "VERSION_ONE");
    write(root.path(), "src/AGENTS.md", "VERSION_TWO");
    let after = loader.load(vec!["src/new/file.rs".into()], false).await;
    assert_ne!(before.sources[0].revision, after.sources[0].revision);
    std::fs::remove_file(root.path().join("src/AGENTS.md")).unwrap();
    let removed = loader.load(vec!["src/new/file.rs".into()], false).await;
    assert!(removed.sources.is_empty());
    assert_eq!(removed.checked_scopes, ["src"]);
    assert!(removed.scoped().contains("including removals"));
}

#[tokio::test]
async fn invalid_oversize_and_symlink_files_are_visible_without_disclosure_or_fallback() {
    let root = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "AGENTS.md", "OUTSIDE_SECRET");
    write(root.path(), "AGENTS.md", "DO_NOT_FALL_BACK");
    std::os::unix::fs::symlink(
        outside.path().join("AGENTS.md"),
        root.path().join("AGENTS.override.md"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
    let loader = Instructions::new(root.path(), global.path());
    let report = loader
        .load(
            vec![
                "linked/file".into(),
                "../elsewhere".into(),
                "/absolute".into(),
            ],
            true,
        )
        .await;
    assert!(report.sources.is_empty());
    assert!(!report.render().contains("OUTSIDE_SECRET"));
    assert!(!report.render().contains("DO_NOT_FALL_BACK"));
    assert!(report.problems.len() >= 3);
    std::fs::remove_file(root.path().join("AGENTS.override.md")).unwrap();
    std::fs::write(root.path().join("AGENTS.override.md"), [0xff, 0xfe]).unwrap();
    assert!(loader.load(vec![], true).await.problems[0].contains("UTF-8"));
    write(
        root.path(),
        "AGENTS.override.md",
        &"x".repeat(16 * 1024 + 1),
    );
    assert!(loader.load(vec![], true).await.problems[0].contains("16 KiB"));
    std::fs::remove_file(root.path().join("AGENTS.override.md")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        root.path().join("AGENTS.override.md"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR,
        0,
    )
    .unwrap();
    let fifo = tokio::time::timeout(std::time::Duration::from_secs(2), loader.load(vec![], true))
        .await
        .unwrap();
    assert!(fifo.problems[0].contains("regular file"));
}

#[tokio::test]
async fn aggregate_budget_reports_omissions_instead_of_truncating_instructions() {
    let root = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    for name in ["AGENTS.md", "src/AGENTS.md", "src/api/AGENTS.md"] {
        write(root.path(), name, &"x".repeat(15 * 1024));
    }
    let loader = Instructions::new(root.path(), global.path());
    let report = loader.load(vec!["src/api".into()], true).await;
    assert_eq!(report.sources.len(), 2);
    assert!(report.sources.iter().all(|s| s.text.len() == 15 * 1024));
    assert!(report.problems[0].contains("32 KiB"));
    assert!(report.render().len() < 65_536);
}

#[tokio::test]
async fn scoped_tool_disclosure_is_isolated_by_turn_room_and_agent() {
    use roomkit::model::{ChannelId, RoomId, ToolCall};
    use roomkit_ai::{ToolContext, ToolOutput, Toolset};
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    write(root.path(), "src/AGENTS.md", "RULES_ONE");
    write(root.path(), "src/file", "contents");
    let loader = Instructions::new(root.path(), state.path());
    let tool = loader.wrap(roomkit_tools_roomkit::ReadToolset::new(
        roomkit_tools_fs::ReadTool::new(root.path()).await.unwrap(),
    ));
    let read = |room: &str, agent: &str| {
        tool.call(
            ToolCall::new(
                "call",
                "read",
                serde_json::from_value(serde_json::json!({"path":"src/file"})).unwrap(),
            ),
            ToolContext::new(RoomId::new(room).unwrap(), ChannelId::new(agent).unwrap()),
        )
    };
    let contains = |output: ToolOutput| matches!(output, ToolOutput::Json(value) if value.get("project_instructions").is_some());
    loader.begin("one", "agent");
    assert!(contains(read("one", "agent").await.unwrap()));
    assert!(!contains(read("one", "agent").await.unwrap()));
    loader.begin("two", "agent");
    assert!(contains(read("two", "agent").await.unwrap()));
    loader.begin("one", "other");
    assert!(contains(read("one", "other").await.unwrap()));
    loader.begin("one", "agent");
    assert!(contains(read("one", "agent").await.unwrap()));
    write(root.path(), "src/AGENTS.md", "RULES_TWO");
    assert!(contains(read("one", "agent").await.unwrap()));
    write(root.path(), "src/AGENTS.md", "RULES_ONE");
    assert!(
        contains(read("one", "agent").await.unwrap()),
        "reverting a file is also a new snapshot"
    );
}
