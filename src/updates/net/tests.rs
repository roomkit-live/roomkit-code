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

/// What the `minisign` command itself writes is accepted: this signature
/// was made by minisign 0.12 with a key made for this test and kept nowhere,
/// the way `scripts/release.py sign` makes a release"s.
#[test]
fn a_signature_written_by_the_minisign_command_is_accepted() {
    let key = "RWT0+SB5Nl4wisBnW8yRjQ7OBFVyWr1UxxsTauhwkHtFo3s2IaiCQUsX";
    let checksum = "abababababababababababababababababababababababababababababababab  roomkit-code-v9.9.9-aarch64-apple-darwin\n";
    let signature = "untrusted comment: signature from minisign secret key\nRUT0+SB5Nl4wilSzwHSiobiBt1HCvvUWfoGSaCcDlOgc/2qX2y783+X8+48qaj/2va6EGKmjhYei0zegevEcX5h9S4f8iy4HaAw=\ntrusted comment: RoomKit Code v9.9.9 roomkit-code-v9.9.9-aarch64-apple-darwin.sha256\nXnCFH5wsJddscPt/6+vzEGON4v6uj9vhIYRaH++88Q21XCM4AhrLLZ8+2ARnYMFyKoKmJcg3E1oqzrJfIV8TBQ==\n";
    verify(key, checksum.as_bytes(), signature).unwrap();
    assert!(verify(key, b"another checksum", signature).is_err());
    assert!(verify(RELEASE_KEY, checksum.as_bytes(), signature).is_err());
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

/// A second key made for one test, whose secret half was not kept either.
const OTHER_KEY: &str = "RWQDL1d+eln49WUATnip+g2D+MUheyt+OGaggNiJBLef+mzOC6qmpwMY";

/// `OTHER_KEY`'s signature of a checksum file that names a binary of
/// another version and platform, beside the fixture binary's digest.
const OTHER_SIGNATURE: &str = "untrusted comment: fixture\nRUQDL1d+eln49W6nocLncIn4QFMg/IdRHuiaJ3D9ANY0WSgunW2jzR9HOQMQBwg1L9w17zFxlw20fHAh7VmPJqPJV2CcqACnKg0=\ntrusted comment: fixture\n1VfpPNNfK56aNLBnjfS8pabm1tp5VOxjLfZ/4RxmlwvrW7OdWDYK93UW2aRPvpgu3TnNtHYadrYrzI53CH34BQ==\n";

/// Review finding: no download test failed once the signature check or the
/// check of the name the signed checksum holds was removed. Each case here
/// serves a binary whose digest its checksum file gives, so only those
/// checks stand between it and an installation.
#[tokio::test]
async fn a_download_needs_a_checksum_its_signature_covers_naming_this_binary() {
    let data = b"fixture release binary";
    let forged = b"forged release binary";
    let name = format!("roomkit-code-v0.2.0-{TARGET}");
    let checksum =
        |binary: &[u8], named: &str| format!("{}  {named}\n", crate::digest::sha256_hex(binary));
    let (unsigned, other) = (
        checksum(forged, &name),
        checksum(data, "roomkit-code-v0.1.0-other-target"),
    );
    let router = Router::new()
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/1",
            get(|| async { &b"fixture release binary"[..] }),
        )
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/5",
            get(|| async { fixture_signature() }),
        )
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/6",
            get(move || async { unsigned }),
        )
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/7",
            get(|| async { &b"forged release binary"[..] }),
        )
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/8",
            get(move || async { other }),
        )
        .route(
            "/repos/roomkit-live/roomkit-code/releases/assets/9",
            get(|| async { OTHER_SIGNATURE }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let release = |(binary, size): (u64, usize), checksum, signature| Release {
        tag_name: "v0.2.0".into(),
        draft: false,
        prerelease: false,
        assets: vec![
            Asset {
                id: binary,
                name: name.clone(),
                size: size as u64,
            },
            Asset {
                id: checksum,
                name: format!("{name}.sha256"),
                size: 150,
            },
            Asset {
                id: signature,
                name: format!("{name}.sha256.minisig"),
                size: 300,
            },
        ],
    };
    // A checksum file and a binary replaced together, the signature kept.
    let github = GitHub::with_source(url.clone(), None, FIXTURE_KEY).unwrap();
    let replaced = release((7, forged.len()), 6, 5);
    let refused = github.download(&replaced).await.unwrap_err().to_string();
    assert!(
        refused.contains("signature verification failed"),
        "{refused}"
    );
    // A checksum file the key did sign, for another binary.
    let github = GitHub::with_source(url, None, OTHER_KEY).unwrap();
    let elsewhere = release((1, data.len()), 8, 9);
    let refused = github.download(&elsewhere).await.unwrap_err().to_string();
    assert_eq!(refused, "Invalid release checksum file");
    task.abort();
}

/// Review finding: the latest release had to be signed before its version
/// was compared, so a build newer than the latest release, or one whose
/// latest release predates signing, reported an error at each check
/// instead of no update. A release that would update this build still has
/// to be signed.
#[tokio::test]
async fn only_a_release_that_would_update_this_build_must_be_signed() {
    for (tag, newer) in [("v0.0.1", false), ("v999.0.0", true)] {
        let name = format!("roomkit-code-{tag}-{TARGET}");
        let release = Release {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
            assets: vec![
                Asset {
                    id: 1,
                    name: name.clone(),
                    size: 22,
                },
                Asset {
                    id: 2,
                    name: format!("{name}.sha256"),
                    size: 150,
                },
            ],
        };
        let json = serde_json::to_string(&release).unwrap();
        let router = Router::new().route(
            "/repos/roomkit-live/roomkit-code/releases/latest",
            get(move || async { json }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let github = GitHub::with_source(url, None, FIXTURE_KEY).unwrap();
        let latest = github.latest().await;
        if newer {
            let refused = latest.unwrap_err().to_string();
            assert!(refused.contains("no signature"), "{refused}");
        } else {
            assert_eq!(latest.unwrap().unwrap().tag_name, tag);
        }
        task.abort();
    }
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
