//! The local picker accepts a download only after every shard verifies.
use super::*;
use axum::{Json, Router, routing::get};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const REV: &str = "1234567890123456789012345678901234567890";
const FIRST: &str = "quant/example-Q6_K-00001-of-00002.gguf";
const LAST: &str = "quant/example-Q6_K-00002-of-00002.gguf";

#[derive(Default)]
struct Gate {
    requests: std::sync::atomic::AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

async fn hub(
    bad: Arc<AtomicBool>,
    gate: Option<Arc<Gate>>,
) -> (url::Url, tokio::task::JoinHandle<()>) {
    let repo = || async {
        let siblings: Vec<_> = [(FIRST, b"one"), (LAST, b"two")]
            .iter()
            .map(|(name, data)| {
                serde_json::json!({
                    "rfilename":name, "lfs":{"sha256": crate::digest::sha256_hex(data), "size":3 }
                })
            })
            .collect();
        Json(serde_json::json!({"sha":REV,"siblings":siblings}))
    };
    let routes = Router::new()
        .route(
            "/api/models",
            get(
                |axum::extract::Query(query): axum::extract::Query<
                    std::collections::HashMap<String, String>,
                >| async move {
                    assert_eq!(query["filter"], "gguf");
                    let text = serde_json::json!({"id":"example/coder","downloads":100});
                    let vision = serde_json::json!({"id":"example/vision","downloads":50});
                    let found = match query.get("pipeline_tag").map(String::as_str) {
                        Some("text-generation") => vec![text],
                        Some("image-text-to-text") => vec![vision],
                        _ => vec![text, vision, serde_json::json!({"id":"example/voice"})],
                    };
                    Json(found)
                },
            ),
        )
        .route("/api/models/example/coder", get(repo))
        .route(
            "/api/models/example/empty",
            get(|| async {
                Json(serde_json::json!({"sha":REV,"siblings":[{"rfilename":"README.md"}]}))
            }),
        )
        .route(
            &format!("/api/models/example/coder/revision/{REV}"),
            get(repo),
        )
        .route(
            &format!("/example/coder/resolve/{REV}/{FIRST}"),
            get(|| async { "one" }),
        )
        .route(
            &format!("/example/coder/resolve/{REV}/{LAST}"),
            get(move || async move {
                if let Some(gate) = gate {
                    gate.requests.fetch_add(1, Ordering::Relaxed);
                    gate.entered.notify_one();
                    gate.release.notified().await;
                }
                if bad.load(Ordering::Relaxed) {
                    "bad"
                } else {
                    "two"
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    (
        url,
        tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        }),
    )
}

#[tokio::test]
async fn gguf_search_download_retry_select_and_offline_resume() {
    let bad = Arc::new(AtomicBool::new(true));
    let (url, server) = hub(bad.clone(), None).await;
    let cache = tempfile::tempdir().unwrap();
    let cache_dir = cache.path().join("hub");
    crate::local_models::TEST_HUB
        .scope((url.clone(), cache_dir.clone()), async {
            let workspace = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            let initial = workspace.path().join("initial.gguf");
            std::fs::write(&initial, b"fixture").unwrap();
            let mut config = settings(workspace.path(), state.path(), "http://localhost");
            config.provider = crate::providers::ProviderKind::LlamaCpp;
            config.model = initial.display().to_string();
            config.base_url.clear();
            let mut app = Application::open(&config, String::new()).await.unwrap();
            let picker = model(&app, "").await;
            let search = &offered(&picker).1[0].value;
            let result = submit_profile(&app, "model", search, &[("query", "coder")])
                .await
                .unwrap();
            assert!(!offered(&result).1.iter().any(|c| c.label == "example/voice"));
            assert_eq!(offered(&result).1[1].label, "example/vision");
            let all = &offered(&picker).1.iter().find(|c| c.label == "Search all GGUF models").unwrap().value;
            let any = submit_profile(&app, "model", all, &[("query", "coder")]).await.unwrap();
            assert!(offered(&any).1.iter().any(|c| c.label == "example/voice"));
            let repo = &offered(&result).1[0].value;
            let files = model(&app, repo).await;
            assert!(matches!(&files, CommandResult::Events(events)
                if matches!(&events[..], [Event::Choices { title, .. }] if title == "Choose a GGUF file · example/coder")));
            let choice = &offered(&files).1[0];
            assert_eq!(choice.label, FIRST);
            let selected = &choice.value;
            let mut changes = app.profiles.local_models.downloads.changes.subscribe();
            let _ = model(&app, selected).await;
            background::wait_status(&app, &mut changes, "SHA-256").await;
            assert_eq!(
                offered(&model(&app, "").await).1.len(),
                3,
                "no partial model"
            );
            bad.store(false, Ordering::Relaxed);
            let _ = model(&app, selected).await;
            background::wait_status(&app, &mut changes, "ready to select").await;
            let CommandResult::Switch(view) = model(&app, selected).await else {
                panic!("Expected model switch")
            };
            let id = format!("hf://example/coder@{REV}/{FIRST}");
            assert_eq!(view.model.as_ref().unwrap().model, id);
            assert_eq!(view.model.as_ref().unwrap().label(), "example · Q6_K");
            assert_eq!(
                view.model.as_ref().unwrap().context_limit_hint.as_deref(),
                Some("/model → Agent context capacity")
            );
            app.view = *view;
            let path = cache_dir.join("models--example--coder/snapshots").join(REV);
            assert_eq!(std::fs::read(path.join(LAST)).unwrap(), b"two");
            assert_eq!(
                std::fs::read_link(path.join(LAST)).unwrap(),
                std::path::Path::new("../../../blobs").join(crate::digest::sha256_hex(b"two"))
            );
            let refs = cache_dir.join("models--example--coder/refs/main");
            assert_eq!(std::fs::read_to_string(refs).unwrap(), REV);
            check_agent_forms(&app, &id).await;
            server.abort();
            app.close().await.unwrap();
            drop(app);
            let moved = cache.path().join("moved-cache");
            std::fs::rename(&cache_dir, &moved).unwrap();
            crate::local_models::TEST_HUB
                .scope((url, moved), async {
                    let app = Application::open(&config, String::new()).await.unwrap();
                    assert_eq!(app.view.model.as_ref().unwrap().model, id);
                    let offline = model(&app, "").await;
                    assert!(
                        offered(&offline)
                            .1
                            .iter()
                            .any(|c| c.value == *selected && c.description.contains(&REV[..12]))
                    );
                    assert!(matches!(
                        model(&app, selected).await,
                        CommandResult::Switch(_)
                    ));
                    app.close().await.unwrap();
                })
                .await;
        })
        .await;
}

async fn check_agent_forms(app: &Application, id: &str) {
    let add = submit_profile(
        app,
        "providers",
        "add",
        &[
            ("id", "offline"),
            ("kind", "llamacpp"),
            ("auth", "none"),
            ("url", ""),
        ],
    )
    .await
    .unwrap();
    let cached = &offered(&add)
        .1
        .iter()
        .find(|c| c.label == FIRST)
        .unwrap()
        .value;
    let new = opened(&invoke(app, "providers", cached).await);
    assert_eq!(new["model"], id);
    assert_eq!(new["provider"], "offline");
    let agents = invoke(app, "agents", "models:code").await;
    let choice = &offered(&agents)
        .1
        .iter()
        .find(|c| c.label == FIRST)
        .unwrap()
        .value;
    let edit = opened(&invoke(app, "agents", choice).await);
    assert_eq!(edit["id"], "code");
    assert_eq!(edit["model"], id);
}

#[tokio::test]
async fn gguf_search_retries_from_the_same_form_and_explains_an_empty_repository() {
    let (url, server) = hub(Arc::new(AtomicBool::new(false)), None).await;
    let cache = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    crate::local_models::TEST_HUB.scope((url, cache.path().into()), async {
        let app = open_local(workspace.path(), state.path()).await;
        let CommandResult::Events(events) = model(&app, "gguf - search").await else {
            panic!("Expected a search form")
        };
        let [Event::Form { form, .. }] = &events[..] else { panic!("Expected a search form") };
        let search = |query: &str| {
            let mut values = roomkit_tui::protocol::FormValues::new();
            values.insert("query", query);
            Command::SubmitForm { name: form.command.clone(), form: form.id.clone(), values: Box::new(values) }
        };
        let refused = profile_command(&app, search("example/missing")).await.unwrap_err();
        assert!(refused.to_string().contains("404"));
        let result = profile_command(&app, search("example/empty")).await.unwrap();
        assert!(matches!(result, CommandResult::Events(events)
            if matches!(&events[..], [Event::Choices { title, choices, .. }]
                if title.contains("No installable GGUF files") && choices.len() == 1 && choices[0].label == "Search again")));
        app.close().await.unwrap();
    }).await;
    server.abort();
}

async fn open_local(root: &std::path::Path, state: &std::path::Path) -> Application {
    std::fs::write(root.join("initial.gguf"), b"fixture").unwrap();
    let mut config = settings(root, state, "http://localhost");
    config.base_url.clear();
    config.provider = crate::providers::ProviderKind::LlamaCpp;
    config.model = "initial.gguf".into();
    Application::open(&config, String::new()).await.unwrap()
}

mod background;
mod manage;

#[tokio::test]
async fn gguf_local_model_switch_keeps_the_agent_context_and_allows_editing_it() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    for name in ["original.gguf", "another.gguf"] {
        std::fs::write(root.path().join(name), b"fixture").unwrap();
    }
    let mut config = settings(root.path(), state.path(), "http://localhost");
    config.provider = crate::providers::ProviderKind::LlamaCpp;
    config.base_url.clear();
    config.model = "original.gguf".into();
    config.context_window = Some(131_072);
    let mut app = Application::open(&config, String::new()).await.unwrap();
    let CommandResult::Switch(next) = model(&app, "another.gguf").await else {
        panic!("Expected switch")
    };
    app.view = *next;
    assert_eq!(app.view.context_capacity, Some(131_072));
    let menu = model(&app, "").await;
    let context = &offered(&menu)
        .1
        .iter()
        .find(|c| c.label == "Agent context capacity")
        .unwrap()
        .value;
    let CommandResult::Switch(next) = submit_profile(&app, "model", context, &[("context", "96K")])
        .await
        .unwrap()
    else {
        panic!("Expected refreshed capacity")
    };
    assert_eq!(next.model.as_ref().unwrap().model, "another.gguf");
    assert_eq!(next.context_capacity, Some(98_304));
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&config, String::new()).await.unwrap();
    assert_eq!(app.view.model.as_ref().unwrap().model, "another.gguf");
    assert_eq!(app.view.context_capacity, Some(98_304));
    app.close().await.unwrap();
}
