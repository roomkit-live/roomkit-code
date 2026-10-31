#![allow(clippy::unwrap_used)]
use super::*;
use axum::{Router, routing::get};

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
    let found = github.latest().await.unwrap();
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
