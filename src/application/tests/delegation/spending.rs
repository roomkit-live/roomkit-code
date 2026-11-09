//! RMK-1536: what a delegated task consumed shows task by task, in a total
//! beside the conversation's own cost, agent by agent at exit, and again
//! once the application reopened.

use roomkit_tui::protocol::{ConversationCost, SidebarContent};
use roomkit_tui_engine::CommandHandler;

use super::tasks::{tasks, until};
use super::*;

/// A model server that reports what each request used: the worker answers at
/// once, 1,000 tokens in and 100 out; each of the delegating agent's requests
/// uses 2,000 and 200.
async fn reporting_server() -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(request.clone());
                let worker = request.to_string().contains(WORKER);
                let last = request["messages"].as_array().unwrap().last().unwrap();
                let delta = if worker {
                    json!({"content": RESULT})
                } else if last.to_string().contains(RESULT) {
                    json!({"content": RELAYED})
                } else if last["role"] == "tool" {
                    json!({"content": "Handed to the reviewer."})
                } else {
                    call("delegate_task", &task_for("review"))
                };
                let reason = if delta.get("tool_calls").is_some() {
                    "tool_calls"
                } else {
                    "stop"
                };
                let usage = if worker {
                    json!({"prompt_tokens": 1_000, "completion_tokens": 100})
                } else {
                    json!({"prompt_tokens": 2_000, "completion_tokens": 200})
                };
                let body = format!(
                    "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"index":0,"delta":delta}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}),
                    json!({"choices":[], "usage": usage}),
                );
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, requests, task)
}

fn sessions(app: &Application, project: &std::path::Path) -> crate::sessions::Sessions {
    crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: project.into(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    }
}

/// What the view's live events say the conversation's tasks cost.
fn related(event: &Event) -> Option<ConversationCost> {
    match event {
        Event::RelatedCostUpdated {
            cost: Some(cost), ..
        } => Some(cost.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn a_task_s_cost_shows_by_task_by_agent_and_in_the_total_and_after_a_restart() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = reporting_server().await;
    let mut settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let verified = roomkit_ai::Date::new(2026, 11, 6).unwrap();
    settings.pricing = Some(roomkit_ai::ModelPricing::new(2.0, 10.0, verified).unwrap());
    let app = Application::open(&settings, String::new()).await.unwrap();
    let handler = sessions(&app, project.path());
    let mut updates = handler.subscribe(app.view.clone()).unwrap();

    turn(&app).await;
    until_handed_back(&requests).await;
    until_relayed(&app).await;

    // 1,000 × 2.0 + 100 × 10.0 millionths of a dollar: the worker alone. The
    // view adds its own cost to it.
    let worker = ConversationCost::new(3_000, "USD", true);
    assert_eq!(until(&mut updates, related).await, worker);

    // Task by task, in the sidebar and in /tasks.
    let spent = "1.1k tokens · ≈ $0.0030";
    let detail = until(&mut updates, |event| match event {
        Event::SidebarChanged {
            sidebar: Some(sidebar),
            ..
        } => sidebar.sections.iter().find_map(|section| {
            match (&section.title[..], &section.content) {
                ("Tasks", SidebarContent::Entries { entries }) => entries
                    .iter()
                    .find(|entry| entry.detail.contains("tokens"))
                    .map(|entry| entry.detail.clone()),
                _ => None,
            }
        }),
        _ => None,
    })
    .await;
    assert!(detail.starts_with("completed in"), "{detail}");
    assert!(detail.ends_with(spent), "{detail}");
    let list = tasks(&handler, &app.view, "").await.unwrap();
    assert!(list.body.contains(spent), "{}", list.body);

    // At exit: the whole conversation, then each agent's part. The delegating
    // agent's two turns used three requests, 6,000 in and 600 out.
    let processes = &app.profiles.resources.processes;
    let shown = Some((app.view.room.to_string(), None));
    let mut summary =
        crate::exit_summary::collect(&app.kit, processes, shown, Some(app.view.clone())).await;
    super::super::add_delegated(&app, &app.view, &mut summary).await;
    let lines = summary.conversation_report();
    let report = lines.join("\n");
    assert!(
        report.contains("total=7700 input=7000 (+ 0 cached) output=700 · 3 turns · cost ≈ $0.02"),
        "{report}"
    );
    assert!(
        report.contains(": total=6600 · 2 turns · ≈ $0.02"),
        "{report}"
    );
    assert!(
        report.contains("  Review (1 task): total=1100 · 1 turn · ≈ $0.0030"),
        "{report}"
    );

    // Reopened, the conversation's tasks and their cost are still there,
    // read from the rooms they ran in; no notice says they just ran.
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(app.profiles.resources.tasks.of("main").is_empty());
    let handler = sessions(&app, project.path());
    let mut updates = handler.subscribe(app.view.clone()).unwrap();
    assert_eq!(until(&mut updates, related).await, worker);
    let list = tasks(&handler, &app.view, "").await.unwrap();
    assert!(list.body.contains("Review · completed in"), "{}", list.body);
    assert!(list.body.contains(spent), "{}", list.body);
    assert!(app.profiles.resources.tasks.notices("main").is_empty());
    app.close().await.unwrap();
    server.abort();
}
