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
