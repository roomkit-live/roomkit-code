//! The exit report reads the conversation's whole history and the live processes.
use super::*;
use crate::exit_summary::collect;

fn report(prompt: u64, completion: u64, cached: u64) -> Option<Value> {
    Some(json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "prompt_tokens_details": {"cached_tokens": cached}
    }))
}

#[tokio::test]
async fn exit_report_sums_every_turn_and_lists_processes_closing_will_stop() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    // Requests 0-1 form the first turn, request 2 the second one.
    let mock = Mock::with_reports(
        vec![("process_start", json!({"command":"cat"}))],
        "Done.",
        vec![
            report(200, 20, 100),
            report(300, 30, 200),
            report(400, 40, 300),
        ],
    )
    .await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn_operation(&app, "first").await;
    turn_operation(&app, "second").await;
    let processes = &app.profiles.resources.processes;
    let command = vec!["roomkit-code".to_owned()];
    let summary = collect(&app.kit, processes, Some(("main".into(), Some(command)))).await;
    assert_eq!(summary.session.as_deref(), Some("main"));
    let usage = summary.usage.as_ref().unwrap();
    // Input excludes cached tokens: 900 prompt tokens, 600 of them cached.
    assert_eq!(
        (usage.input, usage.cached, usage.output, usage.turns),
        (300, 600, 90, 2)
    );
    assert!(usage.complete);
    assert_eq!(summary.running.len(), 1);
    assert_eq!(summary.running[0].command, "cat");

    // A fresh draft has nothing to resume, but processes are still reported.
    let draft = crate::sessions::create(
        &app.kit,
        &app.services.sessions,
        app.channel.id(),
        &app.view.agent,
        None,
        true,
    )
    .await
    .unwrap();
    let summary = collect(&app.kit, processes, Some((draft.to_string(), None))).await;
    assert!(summary.session.is_none() && summary.usage.is_none());
    assert_eq!(summary.running.len(), 1);
    app.close().await.unwrap();
    assert!(collect(&app.kit, processes, None).await.running.is_empty());
}

/// RMK-1457: since roomkit-rs f86cbb1 a Chat Completions usage reports the
/// tokens a prompt wrote to the cache apart from its `input_tokens`, and the
/// exit line counted them nowhere.
#[tokio::test]
async fn exit_report_counts_a_prompt_s_cache_writes_in_its_input() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let written = Some(json!({
        "prompt_tokens": 1000,
        "completion_tokens": 10,
        "prompt_tokens_details": {"cached_tokens": 200, "cache_write_tokens": 300}
    }));
    let mock = Mock::with_reports(Vec::new(), "Done.", vec![written]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn_operation(&app, "first").await;
    let processes = &app.profiles.resources.processes;
    let summary = collect(&app.kit, processes, Some(("main".into(), None))).await;
    let usage = summary.usage.as_ref().unwrap();
    // 1000 prompt tokens: 200 read from the cache, 800 not, 300 of them written there.
    assert_eq!((usage.input, usage.cached, usage.output), (800, 200, 10));
    app.close().await.unwrap();
}
