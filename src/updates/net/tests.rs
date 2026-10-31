#![allow(clippy::unwrap_used)]
use super::*;
use axum::{Router, routing::get};

#[tokio::test]
async fn missing_release_is_not_an_error_when_the_repository_is_accessible() {
    use axum::http::StatusCode;
    let router = Router::new()
        .route("/repos/roomkit-live/roomkit-code", get(|| async { "{}" }))
        .route(
            "/repos/roomkit-live/roomkit-code/releases/latest",
            get(|| async { StatusCode::NOT_FOUND }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let result = GitHub::with_source(url, None).unwrap().latest().await;
    task.abort();
    assert!(
        result.is_ok(),
        "no published release must be a normal result: {result:?}"
    );
    assert!(result.unwrap().is_none());
}

#[tokio::test]
async fn unavailable_service_is_not_reported_as_no_update() {
    use axum::http::StatusCode;
    for (latest, repository) in [
        (401, 200),
        (403, 200),
        (429, 200),
        (500, 200),
        (404, 404),
        (404, 403),
    ] {
        let router = Router::new()
            .route(
                "/repos/roomkit-live/roomkit-code",
                get(move || async move { StatusCode::from_u16(repository).unwrap() }),
            )
            .route(
                "/repos/roomkit-live/roomkit-code/releases/latest",
                get(move || async move { StatusCode::from_u16(latest).unwrap() }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let github = GitHub::with_source(url, None).unwrap();
        let error = github.latest().await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "Could not check for updates right now. Please try again later."
        );
        assert_eq!(
            error.downcast_ref::<ServiceError>().unwrap().status,
            Some(if latest == 404 { repository } else { latest })
        );
        let download = github.get("releases/assets/999", true).await.unwrap_err();
        assert_eq!(
            download.to_string(),
            "Could not download the update. Please try again later."
        );
        assert_eq!(
            download.downcast_ref::<ServiceError>().unwrap().status,
            Some(404)
        );
        task.abort();
    }
}

#[tokio::test]
async fn github_fixture_checks_pair_size_integrity_and_never_follows_untrusted_redirects() {
    let data = b"fixture release binary";
    let hash = format!("{:x}", Sha256::digest(data));
    let name = format!("roomkit-code-v0.2.0-{TARGET}");
    let release = Release {
        tag_name: "v0.2.0".into(),
        draft: false,
        prerelease: false,
        assets: vec![
            Asset {
                id: 1,
                name: name.clone(),
                size: data.len() as u64,
            },
            Asset {
                id: 2,
                name: format!("{name}.sha256"),
                size: 150,
            },
        ],
    };
    let json = serde_json::to_string(&release).unwrap();
    let checksum = format!("{hash}  {name}\n");
    let router = Router::new()
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/4",
            get(|| async { &b"corrupt release binary"[..] }),
        )
        .route(
            "/repos/roomkit-live/roomkit-code/releases/latest",
            get(move || async { json }),
        )
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/1",
            get(|| async { &b"fixture release binary"[..] }),
        )
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/2",
            get(move || async { checksum }),
        )
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/3",
            get(|| async {
                (
                    axum::http::StatusCode::FOUND,
                    [("location", "https://attacker.invalid/token")],
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let github = GitHub::with_source(url, Some("fixture-token".into())).unwrap();
    let found = github.latest().await.unwrap().unwrap();
    let (file, got) = github.download(&found).await.unwrap();
    assert_eq!(got, hash);
    assert_eq!(std::fs::read(file.path()).unwrap(), data);
    let mut wrong = found.clone();
    wrong.assets[0].size += 1;
    assert!(github.download(&wrong).await.is_err());
    wrong.assets[0].size = data.len() as u64;
    wrong.assets[0].id = 4;
    assert!(
        github
            .download(&wrong)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("checksum mismatch")
    );
    wrong.assets[0].id = 3;
    assert!(
        github
            .download(&wrong)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("Untrusted")
    );
    wrong.draft = true;
    assert!(wrong.validate().is_err());
    let mut missing = found;
    missing.assets.remove(1);
    assert!(missing.validate().is_err());
    task.abort();
}
