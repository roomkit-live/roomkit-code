#![allow(clippy::unwrap_used)]
use super::*;
use axum::{Router, routing::get};

#[tokio::test]
async fn missing_release_is_not_an_error_when_the_repository_is_accessible() {
    use axum::{
        http::{HeaderMap, StatusCode, header::ACCEPT_ENCODING},
        response::IntoResponse,
    };
    // roomkit-tools enables reqwest's gzip: the answer is still asked uncompressed.
    let repository = |headers: HeaderMap| async move {
        if headers.contains_key(ACCEPT_ENCODING) {
            (StatusCode::BAD_REQUEST, "compressed").into_response()
        } else {
            "{}".into_response()
        }
    };
    let router = Router::new()
        .route("/repos/roomkit-live/roomkit-code", get(repository))
        .route(
            "/repos/roomkit-live/roomkit-code/releases/latest",
            get(|| async { StatusCode::NOT_FOUND }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let result = GitHub::with_source(url, None, FIXTURE_KEY)
        .unwrap()
        .latest()
        .await;
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
        let github = GitHub::with_source(url, None, FIXTURE_KEY).unwrap();
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

/// A key made for these tests alone, whose secret half was not kept: the
/// fixtures below were signed once with it.
const FIXTURE_KEY: &str = "RWQqrgpISR+AeoB/AdjguT4cwFZh8h7MmPQRPCsXrmfj7CfSag4GSA0p";

/// The fixture key's signature of the checksum file the fixture serves for
/// each platform, whose name the file holds.
fn fixture_signature() -> String {
    let (signature, global) = match TARGET {
        "x86_64-unknown-linux-gnu" => (
            "RUQqrgpISR+AevRPsntdnj1yhgFfETFnMN2kIUem1+bCtuei5vwA4qbu0iMAaYnb57yPhFpvzcPX41R3iylinyBZ3G3hFaRTWgM=",
            "tc7D8x31A2/IOjyZqz4vwojLXsWWE100gOWE1Ecx5TqQ56wUee1Qj/QRg0sHjYY61l/AW/eZsBLSSvpxEzQ6Bw==",
        ),
        "aarch64-unknown-linux-gnu" => (
            "RUQqrgpISR+AehN+jegkRdnm+6NNN+nXh/0GIyeUGu1LF78V7AaP0E5r05ucNT+E649M3voZ0X40dv0/zCC6688l3423b1ljCQI=",
            "jTSsFFkzOwDSFih1asvq/88NxtDR4ZOwL8HCdEyH2aCx1+DK+gf5ryLTuVa1lP299yl+pWuxn6R57YJCAtRGBA==",
        ),
        "x86_64-apple-darwin" => (
            "RUQqrgpISR+AejVFHotc6OviANXh1kAoAnyfDDlWBw6ttnR9X7Lx23VFmin8SoapiPFNZ0Uch5zAw9k3UXj4dBQ5jJQMqxY5lQs=",
            "XZdjIqTi0vCOg5tdxlGlzgJENjzmj1/LCTFtlVMQIuZ9PP+4aNl3CuZ/haiAoJLAxom9UO9V5ltSZY7OiZQRCw==",
        ),
        _ => (
            "RUQqrgpISR+AehIjgapwkIcqvHGdPiT76mRuj2dkGzXtZPXkIc9FD/pnPzDLXzdyMNJt7q1n0euWvqS6BHkhNgnBXVuIC3d22Ac=",
            "dW+y55e9MurAscbMGwW7/YWwzqppiiR0v4Q//EcKtZYd/6lGSra2bpfnK2Joc1pv/Bp56EjO+dWUtxsjRFFJCA==",
        ),
    };
    format!("untrusted comment: fixture\n{signature}\ntrusted comment: fixture\n{global}\n")
}

/// A release is installed only when the release key signed its checksum:
/// one that comes with the binary, from the same place, vouches for nothing.
#[test]
fn a_checksum_is_believed_only_when_the_release_key_signed_it() {
    let hash = crate::digest::sha256_hex(b"fixture release binary");
    let checksum = format!("{hash}  roomkit-code-v0.2.0-{TARGET}\n");
    let signature = fixture_signature();
    verify(FIXTURE_KEY, checksum.as_bytes(), &signature).unwrap();
    // Another binary's checksum, signed by nobody.
    let forged = format!("{}  roomkit-code-v0.2.0-{TARGET}\n", "0".repeat(64));
    let refused = verify(FIXTURE_KEY, forged.as_bytes(), &signature).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("signature verification failed")
    );
    // Another key, a file that is no signature, a build without a key.
    let other = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    assert!(verify(other, checksum.as_bytes(), &signature).is_err());
    assert!(verify(FIXTURE_KEY, checksum.as_bytes(), "not a signature").is_err());
    assert!(verify("UNSET", checksum.as_bytes(), &signature).is_err());
}

/// A release without its signature is no release to install.
#[test]
fn a_release_without_a_signature_is_not_valid() {
    let name = format!("roomkit-code-v0.2.0-{TARGET}");
    let asset = |id, suffix: &str, size| Asset {
        id,
        name: format!("{name}{suffix}"),
        size,
    };
    let mut release = Release {
        tag_name: "v0.2.0".into(),
        draft: false,
        prerelease: false,
        assets: vec![
            asset(1, "", 22),
            asset(2, ".sha256", 150),
            asset(3, ".sha256.minisig", 300),
        ],
    };
    release.validate().unwrap();
    release.assets[2].size = SIGNATURE_BYTES + 1;
    assert!(release.validate().is_err());
    release.assets.pop();
    let refused = release.validate().unwrap_err();
    assert!(refused.to_string().contains("no signature"));
}

/// The key this build verifies releases with is a minisign public key: a
/// build without one could install no update.
#[test]
fn the_embedded_release_key_is_a_minisign_public_key() {
    assert!(
        minisign_verify::PublicKey::from_base64(RELEASE_KEY.trim()).is_ok(),
        "src/updates/release-key.pub must hold the release signing key's public half"
    );
}

#[tokio::test]
async fn github_fixture_checks_pair_size_integrity_and_never_follows_untrusted_redirects() {
    let data = b"fixture release binary";
    let hash = crate::digest::sha256_hex(data);
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
            Asset {
                id: 5,
                name: format!("{name}.sha256.minisig"),
                size: 300,
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
            "/repos/roomkit-live/roomkit-code/releases/assets/5",
            get(|| async { fixture_signature() }),
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
    let github = GitHub::with_source(url, Some("fixture-token".into()), FIXTURE_KEY).unwrap();
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

/// Review finding: any path of github.com was accepted after a redirect,
/// where anyone can publish a file.
#[test]
fn a_download_follows_only_github_s_asset_stores_and_this_repository_s_releases() {
    let trusted = |url: &str| trusted_download(&url::Url::parse(url).unwrap());
    assert!(trusted(
        "https://release-assets.githubusercontent.com/github-production-release-asset/1/x"
    ));
    assert!(trusted("https://objects.githubusercontent.com/any/path"));
    assert!(trusted(
        "https://github.com/roomkit-live/roomkit-code/releases/download/v1.0.0/roomkit-code"
    ));
    for refused in [
        "https://github.com/someone/else/releases/download/v1.0.0/roomkit-code",
        "https://github.com/roomkit-live/roomkit-code/raw/main/file",
        "http://objects.githubusercontent.com/any/path",
        "https://user@objects.githubusercontent.com/any/path",
        "https://example.com/roomkit-live/roomkit-code/releases/download/v1/x",
    ] {
        assert!(!trusted(refused), "{refused}");
    }
}
