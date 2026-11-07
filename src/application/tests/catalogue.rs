use super::*;
use roomkit_ai::{Tool, ToolResult};
use roomkit_tools_roomkit::{
    OutputForget, OutputRead, ProcessRead, ProcessStart, ProcessStatus, ProcessStop,
};

/// What `tool` answers the call of `name` with `args`, made by the agent
/// `agent` in the room `room`.
async fn call(
    tool: &Arc<dyn Tool>,
    (room, agent): (&str, &str),
    name: &str,
    args: Value,
) -> crate::testing::Answer {
    crate::testing::call(tool, room, agent, name, args).await
}

fn data(output: ToolResult) -> Value {
    let ToolResult::Json(value) = output else {
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
    let resources = &app.profiles.resources;
    let outputs: Arc<dyn Tool> = Arc::new(OutputRead::new(resources.outputs.clone()));
    let processes: Arc<dyn Tool> = Arc::new(ProcessRead::new(resources.processes.clone()));
    for caller in [("other", "code"), ("main", "other")] {
        let read = json!({"archive_id":archive});
        assert!(call(&outputs, caller, "output_read", read).await.is_err());
        let read = json!({"process_id":"p1"});
        assert!(
            call(&processes, caller, "process_read", read)
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
    let observer: Arc<dyn Tool> =
        Arc::new(ProcessStatus::new(app.profiles.resources.processes.clone()));
    let status = call(&observer, ("main", "observer"), "process_status", json!({}));
    let status = data(status.await.unwrap());
    assert!(status["processes"].as_array().unwrap().is_empty());
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
        tool_result(&session, "process_status")["processes"][0]["process_id"],
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
    assert!(shown.contains("1 retained processes"), "{shown}");
    assert!(!shown.contains("\"archive_id\"") && !shown.contains("\"bytes\":["));
    let completed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let read = json!({"process_id":"p1"});
            let result = data(
                call(&processes, ("main", "code"), "process_read", read)
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
    let code = ("main", "code");
    let forget: Arc<dyn Tool> = Arc::new(OutputForget::new(app.profiles.resources.outputs.clone()));
    let forgotten = call(
        &forget,
        code,
        "output_forget",
        json!({"archive_id":archive}),
    );
    assert_eq!(data(forgotten.await.unwrap())["forgotten"], true);
    let read = json!({"archive_id":archive});
    assert!(call(&outputs, code, "output_read", read).await.is_err());
    // Leave another live process for the application's shutdown path to reap.
    let processes = &app.profiles.resources.processes;
    let start: Arc<dyn Tool> = Arc::new(ProcessStart::new(processes.clone()));
    let stop: Arc<dyn Tool> = Arc::new(ProcessStop::new(processes.clone()));
    let cat = || json!({"command":"cat"});
    call(&start, code, "process_start", cat()).await.unwrap();
    let stopping = json!({"process_id":"p2","forget":true});
    let stopped = data(call(&stop, code, "process_stop", stopping).await.unwrap());
    assert_eq!(stopped["forgotten"], true);
    call(&start, code, "process_start", cat()).await.unwrap();
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
    let status = call(&observer, ("main", "code"), "process_status", json!({}));
    assert!(status.await.is_err());
    assert!(
        archives.iter().all(|path| !path.exists()),
        "Temporary output archives are removed at shutdown"
    );
}

/// The engine's `read_stored_result`, which every agent declares to page an
/// outcome the engine stored, runs without an approval in Ask, as the other
/// reads do: the engine answers it. The gate refused it, with no rule for it.
/// The engine reports an unknown result as a refusal of its own.
#[tokio::test]
async fn the_engine_s_stored_results_are_read_without_an_approval() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![(
        "read_stored_result",
        json!({"result_id":"evicted_unknown"}),
    )])
    .await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    assert_eq!(settings.mode, crate::permissions::Mode::Ask);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    let snapshot = session.snapshot().unwrap();
    let call = snapshot
        .items
        .iter()
        .find_map(|item| match item {
            Item::Tool { call, .. } if call.name == "read_stored_result" => Some(call),
            _ => None,
        })
        .unwrap();
    assert!(!call.text.contains("permission policy"), "{}", call.text);
    assert!(
        call.text.contains("Result 'evicted_unknown' not found"),
        "{}",
        call.text
    );
    app.close().await.unwrap();
}

/// A `timeout_ms` above the host maximum the prompt states is refused with
/// that maximum, so the model can correct it, instead of being capped.
#[tokio::test]
async fn a_timeout_above_the_host_maximum_is_refused_with_the_maximum() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let shell = crate::timeouts::shell_max_seconds(false) * 1000;
    let process = crate::agent::process_limits().max_timeout_ms;
    let mock = Mock::start(vec![
        ("shell", json!({"command":"true","timeout_ms":shell + 1})),
        (
            "process_start",
            json!({"command":"true","timeout_ms":process + 1}),
        ),
    ])
    .await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    let snapshot = session.snapshot().unwrap();
    for (name, maximum) in [("shell", shell), ("process_start", process)] {
        let call = snapshot
            .items
            .iter()
            .find_map(|item| match item {
                Item::Tool { call, .. } if call.name == name => Some(call),
                _ => None,
            })
            .unwrap();
        assert_eq!(call.state, ToolState::Denied, "{name}: {}", call.text);
        let message = format!("timeout_ms exceeds the host maximum of {maximum} ms");
        assert!(call.text.contains(&message), "{name}: {}", call.text);
    }
    assert!(app.profiles.resources.processes.observe().await.is_empty());
    app.close().await.unwrap();
    // The terminal keeps two minutes, which its prompt states.
    assert_eq!(shell, 120_000);
    let request = mock.requests.lock().unwrap()[0].clone();
    let prompt = request["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains("long command (maximum 120000)."));
}

/// HITL 884d7bf5: in an unattended launch the shell runs a command up to an
/// hour, and the prompt states that maximum; past it, the call is refused
/// with it.
#[tokio::test]
async fn an_unattended_shell_runs_a_command_up_to_an_hour() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let hour = 3_600_000;
    let mock = Mock::start(vec![
        (
            "shell",
            json!({"command":"printf ran > long","timeout_ms":hour}),
        ),
        ("shell", json!({"command":"true","timeout_ms":hour + 1})),
    ])
    .await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    settings.unattended = true;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    app.close().await.unwrap();
    let calls: Vec<_> = session
        .snapshot()
        .unwrap()
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Tool { call, .. } if call.name == "shell" => Some(call.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(calls[0].state, ToolState::Succeeded, "{}", calls[0].text);
    let written = std::fs::read_to_string(root.path().join("long")).unwrap();
    assert_eq!(written, "ran");
    assert_eq!(calls[1].state, ToolState::Denied, "{}", calls[1].text);
    let message = format!("timeout_ms exceeds the host maximum of {hour} ms");
    assert!(calls[1].text.contains(&message), "{}", calls[1].text);
    let request = mock.requests.lock().unwrap()[0].clone();
    let prompt = request["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains(&format!("long command (maximum {hour}).")));
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
    let mut resources = crate::agent::Resources::new(root.path(), state.path(), state.path(), &[])
        .await
        .unwrap();
    let config = SearchHttpConfig {
        endpoint: Some(format!("{origin}/search")),
        private_origins: vec![origin.clone()],
        ..SearchHttpConfig::default()
    };
    resources.search = WebSearchTool::new(Arc::new(ExaSearch::new(config).unwrap()));
    let policy = WebFetchPolicy {
        private_origins: vec![origin],
        ..WebFetchPolicy::default()
    };
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
