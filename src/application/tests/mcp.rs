use super::*;
use roomkit_tui::{
    Client,
    protocol::{Decision, FormValues},
};
use roomkit_tui_engine::CommandResult;

fn request(app: &Application, command: Command) -> Request {
    Request::new(
        RequestId::new(80),
        "mcp-configuration",
        app.view.room.as_str(),
        command,
    )
}
async fn invoke(app: &Application, arguments: &str) -> std::result::Result<CommandResult, String> {
    app.profiles
        .mcp(
            &app.kit,
            request(
                app,
                Command::Invoke {
                    name: "mcp".into(),
                    arguments: arguments.into(),
                },
            ),
            app.view.clone(),
        )
        .await
}
async fn stdio_form(
    app: &Application,
    root: &std::path::Path,
    id: &str,
    mode: &str,
    timeout: &str,
    edit: bool,
) -> Command {
    let script = root.join("fake_mcp.py");
    std::fs::write(&script, include_str!("mcp_server.py")).unwrap();
    let CommandResult::Events(events) = invoke(
        app,
        &if edit {
            format!("edit {id}")
        } else {
            "add".into()
        },
    )
    .await
    .unwrap() else {
        panic!("Expected form")
    };
    let Event::Form { form, .. } = &events[0] else {
        panic!("Expected form")
    };
    let args = json!([
        script,
        mode,
        root.join(format!("{id}.pid")),
        root.join(format!("{id}.trace"))
    ])
    .to_string();
    let values = [
        ("id", id),
        ("program", "python3"),
        ("args", &args),
        ("environment", "{}"),
        ("timeout", timeout),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect::<FormValues>();
    Command::SubmitForm {
        name: "mcp".into(),
        form: form.id.clone(),
        values: Box::new(values),
    }
}
async fn configure(
    app: &Application,
    root: &std::path::Path,
    id: &str,
    mode: &str,
    timeout: &str,
) -> (RunOutcome, Vec<Event>) {
    let form = stdio_form(app, root, id, mode, timeout, false).await;
    assert!(
        !root.join(format!("{id}.pid")).exists(),
        "Opening the form must not start a server"
    );
    execute_command(app, root, form).await
}

async fn attach(
    app: &Application,
    root: &std::path::Path,
) -> (
    Client,
    tokio::task::JoinHandle<std::result::Result<(), roomkit_tui_engine::AdapterError>>,
) {
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: root.into(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    };
    let task = tokio::spawn(app.session().run_with_commands(host, handler));
    assert!(matches!(event(&client).await, Event::Snapshot(_)));
    (client, task)
}
async fn finish(client: &Client, id: u64) -> (RunOutcome, Vec<Event>) {
    let mut events = Vec::new();
    loop {
        let next = event(client).await;
        assert!(
            !matches!(&next, Event::Reply { error: Some(_), .. }),
            "{next:?}"
        );
        if let Event::OperationFinished {
            operation, outcome, ..
        } = &next
            && operation.as_str() == format!("operation-{id}")
        {
            return (*outcome, events);
        }
        events.push(next);
    }
}
async fn execute(
    app: &Application,
    root: &std::path::Path,
    argument: &str,
) -> (RunOutcome, Vec<Event>) {
    execute_command(
        app,
        root,
        Command::Invoke {
            name: "mcp".into(),
            arguments: argument.into(),
        },
    )
    .await
}
async fn execute_command(
    app: &Application,
    root: &std::path::Path,
    value: Command,
) -> (RunOutcome, Vec<Event>) {
    let (client, task) = attach(app, root).await;
    command(&client, 81, app.view.room.as_str(), value);
    let result = finish(&client, 81).await;
    drop(client);
    task.await.unwrap().unwrap();
    result
}

fn alive(root: &std::path::Path, id: &str) -> bool {
    let pid = std::fs::read_to_string(root.join(format!("{id}.pid"))).unwrap();
    std::process::Command::new("kill")
        .args(["-0", &pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn saving_mcp_connects_and_enables_tools_with_permissions_and_durable_selection() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        ("mcp__demo__read", json!({})),
        ("mcp__demo__read", json!({})),
        ("mcp__demo__danger", json!({})),
    ])
    .await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    configure(&app, root.path(), "demo", "normal", "5").await;
    assert!(alive(root.path(), "demo"));
    let sidebar = app.profiles.sidebar(&app.view).await.0;
    let encoded = serde_json::to_string(&sidebar).unwrap();
    assert!(encoded.contains("Connected") && encoded.contains("active tools"));
    assert!(encoded.contains("show:demo"));
    assert!(!encoded.contains("fake_mcp.py"));
    let mut ui = roomkit_tui::Session::default();
    ui.apply(Event::Snapshot(app.session().snapshot().await.unwrap()));
    ui.apply(Event::SidebarChanged {
        conversation: app.view.room.as_str().into(),
        sidebar: Some(sidebar),
    });
    assert!(
        ui.sidebar().is_some(),
        "Application sidebar must satisfy the generic protocol"
    );
    assert!(mock.requests.lock().unwrap().is_empty());
    assert!(
        app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "mcp__demo__read")
    );
    assert!(
        app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "read")
    );
    assert!(invoke(&app, "delete demo").await.is_err());
    let (client, task) = attach(&app, root.path()).await;
    command(
        &client,
        82,
        "main",
        Command::Submit {
            agent: app.view.agent.as_str().into(),
            text: "Use the selected MCP tools".into(),
        },
    );
    let first = approval(&client).await;
    assert!(first.title.contains("mcp__demo__read"));
    assert!(!root.path().join("demo.trace").exists());
    command(
        &client,
        83,
        "main",
        Command::Decide {
            permission: first.id,
            decision: Decision::Deny,
        },
    );
    let second = approval(&client).await;
    assert!(!root.path().join("demo.trace").exists());
    app.policy.set("main", "yolo").unwrap();
    command(
        &client,
        84,
        "main",
        Command::Decide {
            permission: second.id,
            decision: Decision::Allow,
        },
    );
    let (outcome, events) = finish(&client, 82).await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::PermissionRequested { .. }))
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("demo.trace")).unwrap(),
        "read\ndanger\n"
    );
    let request = mock.requests.lock().unwrap()[0].clone();
    assert!(request.to_string().contains("MCP-SERVER-INSTRUCTIONS"));
    assert!(request.to_string().contains("server_instructions"));
    // The breakdown counts the server's instructions and schemas as sent.
    super::breakdown::fixed_parts_match(&app, &request).await;
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
    assert!(!alive(root.path(), "demo"));
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(!alive(root.path(), "demo"));
    assert!(
        !app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name.starts_with("mcp__"))
    );
    assert_eq!(
        execute(&app, root.path(), "connect demo").await.0,
        RunOutcome::Completed
    );
    assert!(
        app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "mcp__demo__read")
    );
    invoke(&app, "disable demo").await.unwrap();
    assert!(
        alive(root.path(), "demo"),
        "Disabling exposure does not kill a shared server"
    );
    assert_eq!(
        execute(&app, root.path(), "disconnect demo").await.0,
        RunOutcome::Completed
    );
    assert!(!alive(root.path(), "demo"));
    let encoded = serde_json::to_string(&app.profiles.sidebar(&app.view).await.0).unwrap();
    assert!(encoded.contains("Disconnected") && encoded.contains("0 active tools"));
    app.close().await.unwrap();
}

#[tokio::test]
async fn mcp_failed_startup_timeout_and_cancel_do_not_leak_or_leave_processes() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    for (id, mode) in [("failure", "fail"), ("timeout", "hang")] {
        let (outcome, events) = configure(&app, root.path(), id, mode, "1").await;
        assert_eq!(outcome, RunOutcome::Failed);
        assert!(!format!("{events:?}").contains("FIXTURE-PRIVATE-CREDENTIAL"));
        assert!(!alive(root.path(), id));
    }
    let form = stdio_form(&app, root.path(), "cancel", "hang", "120", false).await;
    let (client, task) = attach(&app, root.path()).await;
    command(&client, 90, "main", form);
    let run = loop {
        if let Event::RunStarted { run, .. } = event(&client).await {
            break run;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("cancel.pid").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    command(&client, 91, "main", Command::Interrupt { run });
    assert_eq!(finish(&client, 90).await.0, RunOutcome::Interrupted);
    assert!(!alive(root.path(), "cancel"));
    assert!(mock.requests.lock().unwrap().is_empty());
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
    let log =
        std::fs::read_to_string(app._storage.database.with_file_name("diagnostics.jsonl")).unwrap();
    assert!(!log.contains("FIXTURE-PRIVATE-CREDENTIAL"));
    assert!(log.contains("mcp_connection"));
}

#[tokio::test]
async fn automatic_mcp_activation_is_bounded_and_does_not_affect_other_agents() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let (outcome, events) = configure(&app, root.path(), "catalogue", "large", "5").await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(format!("{events:?}").contains("64 of 75"));
    assert_eq!(
        app.profiles
            .tools("code")
            .await
            .iter()
            .filter(|t| t.name.starts_with("mcp__"))
            .count(),
        64
    );
    assert!(
        invoke(&app, "enable catalogue")
            .await
            .unwrap_err()
            .contains("64")
    );
    invoke(&app, "disable catalogue").await.unwrap();
    invoke(&app, "toggle:catalogue:tool-000").await.unwrap();
    let selected = app
        .profiles
        .tools("code")
        .await
        .into_iter()
        .filter(|t| t.name.starts_with("mcp__"))
        .map(|t| t.name)
        .collect::<Vec<_>>();
    assert_eq!(selected, vec!["mcp__catalogue__tool-000"]);
    let provider = saved_provider_id(&settings);
    edit_profile(
        &mut app,
        "agents",
        "add",
        &[
            ("id", "reviewer"),
            ("label", "Reviewer"),
            ("provider", &provider),
            ("model", "fixture-model"),
            ("tools", ""),
        ],
    )
    .await;
    assert!(
        !app.profiles
            .tools("reviewer")
            .await
            .iter()
            .any(|t| t.name.starts_with("mcp__"))
    );
    app.close().await.unwrap();
    assert!(!alive(root.path(), "catalogue"));
}

#[tokio::test]
async fn interrupting_an_mcp_tool_notifies_server_without_closing_the_shared_connection() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![("mcp__demo__slow", json!({}))]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    configure(&app, root.path(), "demo", "normal", "5").await;
    app.policy.set("main", "yolo").unwrap();
    let (client, task) = attach(&app, root.path()).await;
    command(
        &client,
        92,
        "main",
        Command::Submit {
            agent: app.view.agent.as_str().into(),
            text: "Call the slow fixture".into(),
        },
    );
    let run = loop {
        if let Event::RunStarted { run, .. } = event(&client).await {
            break run;
        }
    };
    let trace = root.path().join("demo.trace");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !trace.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    command(&client, 93, "main", Command::Interrupt { run });
    assert_eq!(finish(&client, 92).await.0, RunOutcome::Interrupted);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !std::fs::read_to_string(&trace)
            .unwrap()
            .contains("cancelled")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(alive(root.path(), "demo"));
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
    assert!(!alive(root.path(), "demo"));
}

#[tokio::test]
async fn application_exit_joins_a_pending_mcp_connection_after_view_detach() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let form = stdio_form(&app, root.path(), "pending", "hang", "120", false).await;
    let (client, task) = attach(&app, root.path()).await;
    command(&client, 94, "main", form);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("pending.pid").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(client);
    task.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), app.close())
        .await
        .unwrap()
        .unwrap();
    assert!(!alive(root.path(), "pending"));
}

struct HttpFixture {
    child: std::process::Child,
    url: String,
}
impl HttpFixture {
    async fn start(root: &std::path::Path) -> Self {
        let script = root.join("mcp_http.py");
        std::fs::write(&script, include_str!("mcp_http_server.py")).unwrap();
        let port = root.join("http.port");
        let child = std::process::Command::new("python3")
            .arg(script)
            .arg(&port)
            .arg(root.join("http.trace"))
            .arg("PATH")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut fixture = Self {
            child,
            url: String::new(),
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !port.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        fixture.url = format!(
            "http://127.0.0.1:{}/mcp",
            std::fs::read_to_string(port).unwrap()
        );
        fixture
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
async fn configure_http(app: &Application, root: &std::path::Path, action: &str, url: &str) {
    let CommandResult::Events(events) = invoke(app, action).await.unwrap() else {
        panic!("Expected HTTP form")
    };
    let Event::Form { form, .. } = &events[0] else {
        panic!("Expected HTTP form")
    };
    assert!(form.title.contains("HTTP"));
    let values = [
        ("id", "remote"),
        ("url", url),
        ("bearer_env", "PATH"),
        ("headers", "{}"),
        ("timeout", "5"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect::<FormValues>();
    let command = Command::SubmitForm {
        name: "mcp".into(),
        form: form.id.clone(),
        values: Box::new(values),
    };
    assert_eq!(
        execute_command(app, root, command).await.0,
        RunOutcome::Completed
    );
}

#[tokio::test]
async fn http_mcp_uses_saved_references_permissions_and_closes_the_session() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let fixture = HttpFixture::start(root.path()).await;
    let mock = Mock::start(vec![("mcp__remote__read", json!({}))]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    configure_http(&app, root.path(), "add-http", &fixture.url).await;
    assert!(!root.path().join("http.trace").exists());
    assert!(
        app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "mcp__remote__read")
    );
    let (client, task) = attach(&app, root.path()).await;
    command(
        &client,
        91,
        "main",
        Command::Submit {
            agent: app.view.agent.as_str().into(),
            text: "Use remote read".into(),
        },
    );
    let permission = approval(&client).await;
    assert!(permission.title.contains("mcp__remote__read"));
    assert!(!root.path().join("http.trace").exists());
    command(
        &client,
        92,
        "main",
        Command::Decide {
            permission: permission.id,
            decision: Decision::Allow,
        },
    );
    assert_eq!(finish(&client, 91).await.0, RunOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(root.path().join("http.trace")).unwrap(),
        "read\n"
    );
    drop(client);
    task.await.unwrap().unwrap();
    configure_http(&app, root.path(), "edit remote", &fixture.url).await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("http.trace")).unwrap(),
        "read\ndelete\n"
    );
    assert!(
        app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "mcp__remote__read")
    );
    app.close().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("http.trace")).unwrap(),
        "read\ndelete\ndelete\n"
    );
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(
        !app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name.starts_with("mcp__"))
    );
    assert_eq!(
        execute(&app, root.path(), "connect remote").await.0,
        RunOutcome::Completed
    );
    assert!(
        app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "mcp__remote__read")
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn editing_a_live_mcp_server_reconnects_and_preserves_custom_or_disabled_tools() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(
        configure(&app, root.path(), "demo", "normal", "5").await.0,
        RunOutcome::Completed
    );
    invoke(&app, "toggle:demo:danger").await.unwrap();
    let previous = std::fs::read_to_string(root.path().join("demo.pid")).unwrap();
    let edit = stdio_form(&app, root.path(), "demo", "normal", "5", true).await;
    assert!(
        alive(root.path(), "demo"),
        "Opening an editor must keep the connection alive"
    );
    assert_eq!(
        execute_command(&app, root.path(), edit).await.0,
        RunOutcome::Completed
    );
    assert_ne!(
        previous,
        std::fs::read_to_string(root.path().join("demo.pid")).unwrap()
    );
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", &previous])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    let selected = app.profiles.tools("code").await;
    assert!(selected.iter().any(|t| t.name == "mcp__demo__read"));
    assert!(!selected.iter().any(|t| t.name == "mcp__demo__danger"));
    // A failed replacement is saved for correction, closes the old process,
    // and withdraws unavailable tools from the active agent.
    let edit = stdio_form(&app, root.path(), "demo", "fail", "5", true).await;
    assert_eq!(
        execute_command(&app, root.path(), edit).await.0,
        RunOutcome::Failed
    );
    assert!(!alive(root.path(), "demo"));
    assert!(
        !app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name.starts_with("mcp__"))
    );
    let edit = stdio_form(&app, root.path(), "demo", "normal", "5", true).await;
    assert_eq!(
        execute_command(&app, root.path(), edit).await.0,
        RunOutcome::Completed
    );
    assert!(
        !app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "mcp__demo__danger")
    );
    invoke(&app, "disable demo").await.unwrap();
    let edit = stdio_form(&app, root.path(), "demo", "normal", "5", true).await;
    assert_eq!(
        execute_command(&app, root.path(), edit).await.0,
        RunOutcome::Completed
    );
    assert!(
        !app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name.starts_with("mcp__"))
    );
    assert!(alive(root.path(), "demo"));
    assert!(mock.requests.lock().unwrap().is_empty());
    app.close().await.unwrap();
}

#[tokio::test]
async fn invalid_mcp_edits_keep_the_saved_configuration_and_live_connection() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    configure(&app, root.path(), "demo", "normal", "5").await;
    let file = app._storage.database.with_file_name("mcp.json");
    let before = std::fs::read(&file).unwrap();
    let pid = std::fs::read_to_string(root.path().join("demo.pid")).unwrap();
    let mut edit = stdio_form(&app, root.path(), "demo", "normal", "5", true).await;
    let Command::SubmitForm { values, .. } = &mut edit else {
        panic!("Expected form")
    };
    values.insert(
        "environment",
        json!({"API_KEY":"ROOMKIT_MCP_AUTOCONNECT_MISSING_KEY"}).to_string(),
    );
    let error = app
        .profiles
        .mcp(&app.kit, request(&app, edit), app.view.clone())
        .await
        .unwrap_err();
    assert!(error.contains("missing"));
    assert_eq!(before, std::fs::read(&file).unwrap());
    assert_eq!(
        pid,
        std::fs::read_to_string(root.path().join("demo.pid")).unwrap()
    );
    assert!(alive(root.path(), "demo"));
    invoke(&app, "close").await.unwrap();
    assert!(alive(root.path(), "demo"));
    assert!(
        app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "mcp__demo__read")
    );
    app.close().await.unwrap();
}
