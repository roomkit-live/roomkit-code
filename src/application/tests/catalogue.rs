use super::*;
use roomkit::model::{RoomId, ToolCall};
use roomkit_ai::{ToolContext, ToolOutput, Toolset};
use roomkit_tools_roomkit::{
    OutputForgetToolset, OutputReadToolset, ProcessReadToolset, ProcessStatusToolset,
};

fn context(room: &str, agent: &str) -> ToolContext {
    ToolContext::new(RoomId::new(room).unwrap(), ChannelId::new(agent).unwrap())
}

fn call(name: &str, args: Value) -> ToolCall {
    ToolCall::new("fixture", name, serde_json::from_value(args).unwrap())
}

fn data(output: ToolOutput) -> Value {
    let ToolOutput::Json(value) = output else {
        panic!("Expected structured output")
    };
    value["data"].clone()
}

fn tool_result(session: &Session, name: &str) -> Value {
    let tool = session
        .snapshot()
        .unwrap()
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::Tool { call, .. } if call.name == name => Some(call),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool.state, ToolState::Succeeded, "{}: {}", name, tool.text);
    serde_json::from_str::<Value>(&tool.text).unwrap()["data"].clone()
}

#[tokio::test]
async fn a_schema_4_project_moves_to_the_current_schema_with_its_selections() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let path = app._storage.database.with_file_name("agents.json");
    app.close().await.unwrap();
    drop(app);
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["schema"] = json!(4);
    let mut restricted = document["agents"]["code"].clone();
    restricted["id"] = json!("reader");
    restricted["tools"] = json!(["read"]);
    document["agents"]["reader"] = restricted;
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let app = Application::open(&settings, String::new()).await.unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["schema"], crate::profiles::model::PROJECT_SCHEMA);
    assert_eq!(
        app.profiles.tools("code").await.len(),
        crate::profiles::model::TOOLS.len() + crate::permissions::MODE_TOOLS.len()
    );
    assert_eq!(saved["agents"]["reader"]["tools"], json!(["read"]));
    app.close().await.unwrap();
    drop(app);
    // An earlier schema is refused rather than guessed.
    let mut older = saved.clone();
    older["schema"] = json!(3);
    std::fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
    assert!(Application::open(&settings, String::new()).await.is_err());
}

#[tokio::test]
async fn coding_tools_apply_structured_multifile_patch_and_create_parent_directories() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    for (name, contents) in [
        ("update", "before"),
        ("remove", "obsolete"),
        ("move", "source"),
    ] {
        std::fs::write(root.path().join(name), contents).unwrap();
    }
    let changes = json!([
        {"op":"add","path":"new","content":"added"},
        {"op":"update","path":"update","old_content":"before","new_content":"after"},
        {"op":"delete","path":"remove","old_content":"obsolete"},
        {"op":"move","from":"move","to":"moved","old_content":"source"}
    ]);
    let mock = Mock::start(vec![
        ("patch", json!({"changes":changes,"dry_run":false})),
        (
            "write",
            json!({"path":"src/nested/module.rs","content":"pub fn example() {}\n"}),
        ),
    ])
    .await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    assert_eq!(tool_result(&session, "patch")["completed"], true);
    assert_eq!(
        std::fs::read_to_string(root.path().join("update")).unwrap(),
        "after"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("moved")).unwrap(),
        "source"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("new")).unwrap(),
        "added"
    );
    assert!(!root.path().join("remove").exists() && !root.path().join("move").exists());
    assert_eq!(
        tool_result(&session, "write")["created_directories"],
        json!(["src", "src/nested"])
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn processes_and_archives_survive_reconfiguration_are_scoped_and_close_with_application() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let first = Mock::start(vec![
        ("process_start", json!({"command":"cat"})),
        ("shell", json!({"command":"i=0; while [ $i -lt 6000 ]; do printf 'archive-line\n'; i=$((i+1)); done"})),
    ]).await;
    let mut settings = settings(root.path(), state.path(), &first.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    assert_eq!(tool_result(&session, "process_start")["state"], "running");
    let shell = tool_result(&session, "shell");
    assert_eq!(shell["stdout"]["truncated"], true);
    let archive = shell["stdout"]["archive"]["archive_id"].as_str().unwrap();
    let outputs = OutputReadToolset::new(app.profiles.resources.outputs.clone());
    let processes = ProcessReadToolset::new(app.profiles.resources.processes.clone());
    for ctx in [context("other", "code"), context("main", "other")] {
        assert!(
            outputs
                .call(
                    call("output_read", json!({"archive_id":archive})),
                    ctx.clone()
                )
                .await
                .is_err()
        );
        assert!(
            processes
                .call(call("process_read", json!({"process_id":"p1"})), ctx)
                .await
                .is_err()
        );
    }
    let second = Mock::start(vec![
        ("process_status", json!({})),
        (
            "process_write",
            json!({"process_id":"p1","text":"hello\n","close_stdin":true}),
        ),
        (
            "output_read",
            json!({"archive_id":archive,"offset":36000,"max_bytes":120}),
        ),
    ])
    .await;
    let provider = saved_provider_id(&settings);
    edit_profile(
        &mut app,
        "providers",
        &format!("edit:{provider}"),
        &[("url", &second.base_url)],
    )
    .await;
    // Selecting another participant must not terminate the original participant's jobs.
    edit_profile(
        &mut app,
        "agents",
        "add",
        &[
            ("id", "observer"),
            ("provider", &provider),
            ("model", "fixture-model"),
            ("tools", "process_status"),
        ],
    )
    .await;
    let observer = ProcessStatusToolset::new(app.profiles.resources.processes.clone());
    assert!(
        data(
            observer
                .call(
                    call("process_status", json!({})),
                    context("main", "observer")
                )
                .await
                .unwrap()
        )
        .as_array()
        .unwrap()
        .is_empty()
    );
    let roomkit_tui_engine::CommandResult::Switch(view) = profile_command(
        &app,
        Command::Invoke {
            name: "agents".into(),
            arguments: "select:code".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected selected agent")
    };
    app.view = *view;
    let session = turn_operation(&app, "continue-process").await;
    assert_eq!(
        tool_result(&session, "process_status")[0]["process_id"],
        "p1"
    );
    assert_eq!(tool_result(&session, "process_write")["complete"], true);
    assert_eq!(
        tool_result(&session, "output_read")["text"],
        "archive-line\n".repeat(6000)[36000..36120]
    );
    let frame = roomkit_tui::render::screen(
        &session,
        160,
        90,
        &crate::tool_views::renderers(root.path()),
    );
    let shown = frame.lines.join("\n");
    assert!(
        shown.contains("stdout archive") && shown.contains("bytes 36000–36120"),
        "{shown}"
    );
    assert!(!shown.contains("\"archive_id\"") && !shown.contains("\"bytes\":["));
    let completed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let result = data(
                processes
                    .call(
                        call("process_read", json!({"process_id":"p1"})),
                        context("main", "code"),
                    )
                    .await
                    .unwrap(),
            );
            if result["status"]["state"] == "finished" {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(completed["stdout"]["text"], "hello\n");
    assert_eq!(completed["status"]["exit_code"], 0);
    let forget = OutputForgetToolset::new(app.profiles.resources.outputs.clone());
    assert_eq!(
        data(
            forget
                .call(
                    call("output_forget", json!({"archive_id":archive})),
                    context("main", "code")
                )
                .await
                .unwrap()
        )["forgotten"],
        true
    );
    assert!(
        outputs
            .call(
                call("output_read", json!({"archive_id":archive})),
                context("main", "code")
            )
            .await
            .is_err()
    );
    // Leave another live process for the application's shutdown path to reap.
    let start =
        roomkit_tools_roomkit::ProcessStartToolset::new(app.profiles.resources.processes.clone());
    start
        .call(
            call("process_start", json!({"command":"cat"})),
            context("main", "code"),
        )
        .await
        .unwrap();
    let stop =
        roomkit_tools_roomkit::ProcessStopToolset::new(app.profiles.resources.processes.clone());
    let stopped = data(
        stop.call(
            call("process_stop", json!({"process_id":"p2","forget":true})),
            context("main", "code"),
        )
        .await
        .unwrap(),
    );
    assert_eq!(stopped["forgotten"], true);
    start
        .call(
            call("process_start", json!({"command":"cat"})),
            context("main", "code"),
        )
        .await
        .unwrap();
    let state_path = app._storage.database.parent().unwrap().to_owned();
    let archives: Vec<_> = std::fs::read_dir(state_path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("roomkit-output-")
        })
        .collect();
    assert!(
        !archives.is_empty(),
        "Output archive storage exists before shutdown"
    );
    app.close().await.unwrap();
    assert!(
        observer
            .call(call("process_status", json!({})), context("main", "code"))
            .await
            .is_err()
    );
    assert!(
        archives.iter().all(|path| !path.exists()),
        "Temporary output archives are removed at shutdown"
    );
}

#[tokio::test]
async fn web_uses_anonymous_exa_and_fetch_without_reusing_the_model_credential() {
    use roomkit_tools_search::{ExaSearch, SearchHttpConfig, WebSearchTool};
    use roomkit_tools_web::{WebFetchPolicy, WebFetchTool};
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let seen = observed.clone();
    let page = format!("{origin}/page");
    let hit = page.clone();
    let router = Router::new().route("/search", post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| { let seen=seen.clone(); let hit=hit.clone(); async move {
        assert!(!headers.contains_key("authorization") && !headers.contains_key("x-api-key"));
        seen.lock().unwrap().push(body);
        Json(json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":format!("Title: Rust guide\nURL: {hit}\nText: A public fixture guide.")}]}}))
    }})).route("/page", axum::routing::get(|headers: axum::http::HeaderMap| async move {
        assert!(!headers.contains_key("authorization"));
        ([("content-type","text/html")], "<h1>Rust guide</h1><p>Useful public documentation.</p>")
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mock = Mock::start(vec![
        ("web_search", json!({"query":"Rust guide"})),
        ("web_fetch", json!({"url":page})),
    ])
    .await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let mut resources = crate::agent::Resources::new(root.path(), state.path(), state.path())
        .await
        .unwrap();
    let mut config = SearchHttpConfig::default();
    config.endpoint = Some(format!("{origin}/search"));
    config.policy.private_origins = vec![origin.clone()];
    resources.search = WebSearchTool::new(Arc::new(ExaSearch::new(None, config).unwrap()));
    let mut policy = WebFetchPolicy::default();
    policy.private_origins = vec![origin];
    resources.fetch = WebFetchTool::new().with_policy(policy).unwrap();
    let app = Application::open_with_tools(&settings, "fixture-model-key".into(), Some(resources))
        .await
        .unwrap();
    let session = turn(&app).await;
    assert_eq!(
        tool_result(&session, "web_search")["payload"]["results"][0]["url"],
        page
    );
    assert!(
        tool_result(&session, "web_fetch")["content"]
            .as_str()
            .unwrap()
            .contains("Useful public documentation")
    );
    let frame = roomkit_tui::render::screen(
        &session,
        140,
        40,
        &crate::tool_views::renderers(root.path()),
    );
    let shown = frame.lines.join("\n");
    assert!(shown.contains("Rust guide") && shown.contains("HTTP 200"));
    assert!(!shown.contains("\"payload\"") && !shown.contains("\"content_bytes\""));
    assert_eq!(
        observed.lock().unwrap()[0]["params"]["name"],
        "web_search_exa"
    );
    assert_eq!(
        observed.lock().unwrap()[0]["params"]["arguments"]["query"],
        "Rust guide"
    );
    assert!(
        mock.headers
            .lock()
            .unwrap()
            .iter()
            .all(|header| header == "Bearer fixture-model-key")
    );
    app.close().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn ask_refuses_network_and_process_effects_before_execution() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        (
            "process_start",
            json!({"command":"printf should-not-run > denied"}),
        ),
        (
            "process_write",
            json!({"process_id":"p1","text":"denied\n"}),
        ),
        ("web_search", json!({"query":"fixture query"})),
        ("web_fetch", json!({"url":"https://example.org/fixture"})),
    ])
    .await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Test permissions".into(),
        },
    );
    for (index, (name, detail)) in [
        ("process_start", "not sandboxed"),
        ("process_write", "Process input"),
        ("web_search", "Exa"),
        ("web_fetch", "public URL"),
    ]
    .iter()
    .enumerate()
    {
        let permission = approval(&client).await;
        assert_eq!(permission.title, format!("Allow {name}?"));
        assert!(permission.detail.contains(detail));
        assert!(!root.path().join("denied").exists());
        command(
            &client,
            index as u64 + 2,
            "main",
            Command::Decide {
                permission: permission.id,
                decision: roomkit_tui::protocol::Decision::Deny,
            },
        );
    }
    loop {
        if matches!(event(&client).await, Event::RunFinished { .. }) {
            break;
        }
    }
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}
