//! Resumable downloads under the Hugging Face per-blob lock.
use super::{
    cache::io,
    hub::{Asset, Hub},
};
use reqwest::{StatusCode, header};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) async fn download(
    hub: &Hub,
    asset: &Asset,
    target: &Path,
    progress: impl Fn(u64),
) -> Result<(), String> {
    let partial = target.with_extension("incomplete");
    let mut offset = match tokio::fs::metadata(&partial).await {
        Ok(meta) if meta.len() <= asset.size => meta.len(),
        Ok(_) => {
            tokio::fs::remove_file(&partial).await.map_err(io)?;
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(io(e)),
    };
    if offset < asset.size {
        let mut parts: Vec<_> = asset.file.repo.split('/').collect();
        parts.extend(["resolve", &asset.file.revision]);
        parts.extend(asset.file.name.split('/'));
        let url = hub.url(&parts)?;
        let mut request = hub.client.get(url);
        if offset != 0 {
            request = request.header(header::RANGE, format!("bytes={offset}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Could not download the model from Hugging Face")?;
        match response.status() {
            StatusCode::OK => offset = 0,
            StatusCode::PARTIAL_CONTENT if valid_range(&response, offset, asset.size) => {}
            StatusCode::PARTIAL_CONTENT | StatusCode::RANGE_NOT_SATISFIABLE => {
                if offset != 0 {
                    tokio::fs::remove_file(&partial).await.map_err(io)?;
                }
                return Err("Unexpected model byte range. Select the file again to retry.".into());
            }
            status => return Err(format!("Model download failed (HTTP {status})")),
        }
        receive(response, &partial, offset, asset.size, progress).await?;
    }
    // Re-read the prefix too: an interrupted transfer may have been made by another client.
    if checksum(&partial).await? != asset.sha {
        tokio::fs::remove_file(&partial).await.map_err(io)?;
        return Err("Model SHA-256 differs from the Hub metadata".into());
    }
    tokio::fs::rename(partial, target).await.map_err(io)
}

fn valid_range(response: &reqwest::Response, offset: u64, size: u64) -> bool {
    let expected = format!("bytes {offset}-{}/{size}", size - 1);
    response
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|h| h.to_str().ok())
        == Some(&expected)
}

async fn receive(
    mut response: reqwest::Response,
    path: &Path,
    mut received: u64,
    size: u64,
    progress: impl Fn(u64),
) -> Result<(), String> {
    let mut output = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(received != 0)
        .truncate(received == 0)
        .open(path)
        .await
        .map_err(io)?;
    progress(received);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Model download interrupted. Select the file again to resume.")?
    {
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or("Model is too large")?;
        if received > size {
            return Err("Model exceeds its declared size".into());
        }
        output.write_all(&chunk).await.map_err(io)?;
        progress(received);
    }
    output.flush().await.map_err(io)?;
    if received != size {
        return Err("Incomplete model download. Select the file again to resume.".into());
    }
    Ok(())
}

async fn checksum(path: &Path) -> Result<String, String> {
    let mut file = tokio::fs::File::open(path).await.map_err(io)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 256 * 1024];
    loop {
        let count = file.read(&mut buf).await.map_err(io)?;
        if count == 0 {
            break;
        }
        hash.update(&buf[..count]);
    }
    Ok(crate::digest::hex(&hash.finalize()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "fixture failures should stop the test")]
    use super::super::hub::ModelFile;
    use super::*;
    use axum::{Router, http::HeaderMap, routing::get};

    #[tokio::test]
    async fn gguf_resumes_an_interrupted_transfer_or_restarts_on_full_response() {
        let dir = tempfile::tempdir().unwrap();
        let file = ModelFile::new("example/coder", &"a".repeat(40), "model.gguf").unwrap();
        let data = b"example weights";
        let asset = Asset {
            file,
            size: data.len() as u64,
            sha: crate::digest::sha256_hex(data),
        };
        for partial in [true, false] {
            let server = Router::new().fallback(get(move |headers: HeaderMap| async move {
                assert_eq!(headers[header::RANGE], "bytes=7-");
                if partial {
                    (
                        StatusCode::PARTIAL_CONTENT,
                        [(header::CONTENT_RANGE, "bytes 7-14/15")],
                        &data[7..],
                    )
                } else {
                    (StatusCode::OK, [(header::ACCEPT_RANGES, "none")], &data[..])
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let hub = Hub::at(
                format!("http://{}", listener.local_addr().unwrap())
                    .parse()
                    .unwrap(),
            )
            .unwrap();
            let handle = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
            let target = dir.path().join(format!("weights-{partial}"));
            std::fs::write(target.with_extension("incomplete"), &data[..7]).unwrap();
            download(&hub, &asset, &target, |_| ()).await.unwrap();
            assert_eq!(std::fs::read(target).unwrap(), data);
            handle.abort();
        }
    }
}
