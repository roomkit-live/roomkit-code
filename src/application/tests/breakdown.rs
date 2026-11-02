//! The context breakdown estimates what the request to the model carries.
use super::*;
use roomkit_tui::protocol::{MeterSegment, SidebarContent};

fn tokens(bytes: usize) -> u64 {
    u64::try_from(bytes.div_ceil(4)).unwrap()
}

async fn parts(app: &Application) -> Vec<MeterSegment> {
    let (sidebar, _) = app.profiles.sidebar(&app.view).await;
    let context = sidebar
        .sections
        .into_iter()
        .find(|section| section.title == "Context")
        .unwrap();
    let SidebarContent::ContextBreakdown { parts, remainder } = context.content else {
        panic!("Expected a context breakdown");
    };
    assert_eq!(remainder, "Messages");
    parts
}

/// The parts of every request before its messages add up to the system
/// message and the tools that `request` carried, within each part's rounding
/// and 2 %: skills, MCP servers and the engine's own texts included.
pub(super) async fn fixed_parts_match(app: &Application, request: &Value) {
    let parts = parts(app).await;
    let estimated: u64 = parts
        .iter()
        .filter(|part| part.label != "Instructions")
        .map(|part| part.value)
        .sum();
    let system: usize = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "system")
        .filter_map(|message| message["content"].as_str())
        .map(str::len)
        .sum();
    let tools: usize = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool.to_string().len())
        .sum();
    let sent = tokens(system) + tokens(tools);
    let slack = u64::try_from(parts.len()).unwrap() + 2;
    assert!(
        estimated.abs_diff(sent) <= slack,
        "{estimated} estimated, {sent} sent: {parts:?}"
    );
}

#[tokio::test]
async fn the_context_breakdown_follows_the_request_sent_to_the_model() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("AGENTS.md"), "Always run the tests.").unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn(&app).await;
    let parts = parts(&app).await;
    let part = |label: &str| {
        parts
            .iter()
            .find(|part| part.label == label)
            .map(|part| part.value)
            .unwrap()
    };
    let request = mock.requests.lock().unwrap()[0].clone();
    fixed_parts_match(&app, &request).await;
    // Within rounding of each part, plus 2 % for what is not estimated.
    let close = |estimate: u64, sent: u64| estimate.abs_diff(sent) <= 4 + sent / 50;
    // The system message is the system prompt, then the skills context.
    let system = tokens(request["messages"][0]["content"].as_str().unwrap().len());
    let estimated = part("System prompt") + part("Skills");
    assert!(
        close(estimated, system),
        "{system} system tokens for {parts:?}"
    );
    // The tools as sent, with their intent property and function wrapper.
    let sent: usize = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool.to_string().len())
        .sum();
    let sent = tokens(sent);
    assert!(
        close(part("Tools"), sent),
        "{sent} tool tokens for {parts:?}"
    );
    // Each turn carries the mode instruction and the project's AGENTS.md.
    let instruction = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_str())
        .find(|content| content.contains("Always run the tests."))
        .unwrap();
    let delivered = tokens(instruction.len());
    assert!(
        close(part("Instructions"), delivered),
        "{delivered} instruction tokens for {parts:?}"
    );
    app.close().await.unwrap();
}

/// The application sends no temperature: the endpoint's default applies, and
/// a model that refuses one, behind any base URL, is never sent it.
#[tokio::test]
async fn the_request_leaves_the_temperature_to_the_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn(&app).await;
    let request = mock.requests.lock().unwrap()[0].clone();
    assert!(request.get("temperature").is_none(), "{request}");
    app.close().await.unwrap();
}

/// No output limit is sent unless the agent sets one: the endpoint decides,
/// as each model has its own. A limit set in `/agents` goes with the next
/// request; zero is refused rather than read as no limit, and a profile
/// without a limit is stored without the field.
#[tokio::test]
async fn the_request_carries_an_output_limit_only_when_the_agent_sets_one() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let limits = |request: &Value| {
        (
            request.get("max_tokens").cloned(),
            request.get("max_completion_tokens").cloned(),
        )
    };
    let stored = || {
        let project = std::fs::read_dir(state.path().join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let saved: Value =
            serde_json::from_slice(&std::fs::read(project.join("agents.json")).unwrap()).unwrap();
        saved["agents"]["code"].clone()
    };
    turn(&app).await;
    let first = mock.requests.lock().unwrap()[0].clone();
    assert_eq!(limits(&first), (None, None));
    assert!(stored().get("max_output_tokens").is_none());

    let refused = submit_profile(&app, "agents", "edit:code", &[("output", "0")])
        .await
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        refused,
        "Max output tokens must be a positive number of tokens, or empty"
    );
    edit_profile(&mut app, "agents", "edit:code", &[("output", "2048")]).await;
    assert_eq!(stored()["max_output_tokens"], 2048);
    turn_operation(&app, "with-a-limit").await;
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(limits(&requests[1]), (Some(json!(2048)), None));
    app.close().await.unwrap();
}
