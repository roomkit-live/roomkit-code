#![expect(
    clippy::unwrap_used,
    reason = "Failures on temporary repositories and memory calls fail the test"
)]
use super::*;
use roomkit_ai::{
    AiContentPart, AiContext, AiMessage, AiMessageContent, AiRole, AiToolResultPart, Tool,
    memory::estimate_context_tokens,
};
use roomkit_tools_memory::MemoryInput;
use serde_json::json;

fn git(path: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn memory_scope_follows_git_worktrees_and_keeps_separate_projects_apart() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("one");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.org",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    );
    git(&repo, &["worktree", "add", "--detach", "../two", "HEAD"]);
    std::fs::create_dir(repo.join("sub")).unwrap();
    let linked = project_root(&temp.path().join("two")).await.unwrap();
    assert_eq!(linked, project_root(&repo.join("sub")).await.unwrap());
    assert_ne!(linked, project_root(temp.path()).await.unwrap());
}

#[tokio::test]
async fn memory_search_reaches_a_note_beyond_the_index_and_reads_share_one_budget() {
    let temp = tempfile::tempdir().unwrap();
    let store = MemoryTool::new(Arc::new(FileMemory::new(temp.path().to_path_buf(), QUOTA)));
    let cancel = CancellationToken::new();
    for i in 0..32 {
        let id = format!("runner-{i:02}");
        let input = MemoryInput {
            action: "save".into(),
            id: Some(id.clone()),
            title: Some(format!("Runner specification {i}")),
            text: Some(format!("Machine {id} has {}", "hardware ".repeat(64))),
            ..Default::default()
        };
        store.call(input, &cancel).await.unwrap();
    }
    let view = View::new(store, Some(2048), None);
    let empty = AiContext::default();
    let indexed = view.prepare(&empty).await;
    assert!(indexed.messages[0].text().contains("runner-00"));
    assert!(!indexed.messages[0].text().contains("runner-31"));
    assert!(estimate_context_tokens(&indexed) <= 64);
    let search = view
        .tool(None)
        .handler(
            "memory".into(),
            json!({"action":"search","query":"runner-31"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .unwrap();
    assert!(matches!(search,roomkit_ai::ToolResult::Text(ref s) if s.contains("runner-31")));
    let read = view
        .tool(None)
        .handler(
            "memory".into(),
            json!({"action":"read","id":"runner-31"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .unwrap();
    let message = |value| {
        AiMessage::new(
            AiRole::Tool,
            AiMessageContent::Parts(vec![AiContentPart::ToolResult(AiToolResultPart::new(
                "call", "memory", value,
            ))]),
        )
    };
    let input = AiContext {
        messages: vec![message(search), message(read.clone()), message(read)],
        ..Default::default()
    };
    let baseline = AiContext {
        messages: vec![message("".into()); 3],
        ..Default::default()
    };
    let prepared = view.prepare(&input).await;
    assert!(estimate_context_tokens(&prepared) <= estimate_context_tokens(&baseline) + 64);
    assert!(
        prepared
            .messages
            .last()
            .unwrap()
            .text()
            .contains("Machine runner-31")
    );
    let full = AiContext {
        max_tokens: Some(2048),
        ..input
    };
    assert_eq!(
        estimate_context_tokens(view.prepare(&full).await.as_ref()),
        estimate_context_tokens(&baseline)
    );
}
