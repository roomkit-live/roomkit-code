//! GitHub release metadata and bounded, integrity-checked binary downloads.
use super::*;
use sha2::{Digest, Sha256};
use std::io::Write;

#[derive(Debug)]
pub(super) struct ServiceError {
    pub asset: bool,
    pub status: Option<u16>,
}
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.asset {
            "Could not download the update. Please try again later."
        } else {
            "Could not check for updates right now. Please try again later."
        })
    }
}
impl std::error::Error for ServiceError {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub id: u64,
    pub name: String,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}
impl Release {
    pub fn validate(&self) -> Result<()> {
        if self.draft || self.prerelease {
            return Err("Only published stable releases are supported".into());
        }
        version(&self.tag_name)?;
        self.asset(false)?;
        self.asset(true)?;
        Ok(())
    }
    pub fn number(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }
    pub fn asset(&self, checksum: bool) -> Result<&Asset> {
        let name = format!(
            "roomkit-code-{}-{TARGET}{}",
            self.tag_name,
            if checksum { ".sha256" } else { "" }
        );
        let assets: Vec<_> = self.assets.iter().filter(|a| a.name == name).collect();
        if assets.len() != 1
            || assets[0].id == 0
            || assets[0].size == 0
            || assets[0].size > if checksum { 1024 } else { MAX_BINARY }
        {
            return Err("The release has no valid binary/checksum pair for this platform".into());
        }
        Ok(assets[0])
    }
}

pub struct GitHub {
    client: reqwest::Client,
    token: Option<String>,
    base: String,
}
impl GitHub {
    pub async fn new() -> Result<Self> {
        let token = std::env::var("ROOMKIT_UPDATE_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        // Reuse the user's GitHub CLI login for this private repository, never write it to disk.
        let token = match token {
            Some(t) => Some(t),
            None => {
                let mut command = tokio::process::Command::new("gh");
                command
                    .args(["auth", "token", "--hostname", "github.com"])
                    .kill_on_drop(true);
                match tokio::time::timeout(std::time::Duration::from_secs(2), command.output())
                    .await
                {
                    Ok(Ok(out)) if out.status.success() => String::from_utf8(out.stdout)
                        .ok()
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty()),
                    _ => None,
                }
            }
        };
        Self::with_source("https://api.github.com".into(), token)
    }
    fn with_source(base: String, token: Option<String>) -> Result<Self> {
        Ok(Self {
            base,
            token,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("roomkit-code/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(120))
                .build()?,
        })
    }
    async fn get(&self, path: &str, asset: bool) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .get(
                format!("{}/repos/roomkit-live/roomkit-code/{path}", self.base)
                    .trim_end_matches('/'),
            )
            .header(
                "Accept",
                if asset {
                    "application/octet-stream"
                } else {
                    "application/vnd.github+json"
                },
            )
            .header("X-GitHub-Api-Version", "2022-11-28");
        if !asset {
            request = request.timeout(std::time::Duration::from_secs(10));
        }
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let mut response = request.send().await.map_err(|_| ServiceError {
            asset,
            status: None,
        })?;
        if asset && response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or("Missing release download location")?;
            let url = url::Url::parse(location).map_err(|_| "Invalid release download location")?;
            if url.scheme() != "https"
                || !matches!(
                    url.host_str(),
                    Some(
                        "release-assets.githubusercontent.com"
                            | "objects.githubusercontent.com"
                            | "github.com"
                    )
                )
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("Untrusted release download location".into());
            }
            // Credentials stay on api.github.com; signed asset URLs need no Authorization header.
            response = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|_| ServiceError {
                    asset,
                    status: None,
                })?;
        }
        match response.status().as_u16() {
            200 => Ok(response),
            // Only the latest-release endpoint may legitimately have no result.
            404 if !asset && path == "releases/latest" => Ok(response),
            status => Err(ServiceError {
                asset,
                status: Some(status),
            }
            .into()),
        }
    }
    pub async fn latest(&self) -> Result<Option<Release>> {
        let mut response = self.get("releases/latest", false).await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            // GitHub also hides inaccessible repositories behind 404. Confirm
            // access before telling the user that no release is available.
            self.get("", false).await?;
            return Ok(None);
        }
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Incomplete release metadata")?
        {
            if data.len() + chunk.len() > 1024 * 1024 {
                return Err("Release metadata exceeds limit".into());
            }
            data.extend_from_slice(&chunk);
        }
        let release: Release =
            serde_json::from_slice(&data).map_err(|_| "Invalid release metadata")?;
        release.validate()?;
        Ok(Some(release))
    }
    pub async fn download(&self, release: &Release) -> Result<(tempfile::NamedTempFile, String)> {
        release.validate()?;
        let binary = release.asset(false)?;
        let checksum = release.asset(true)?;
        let mut response = self
            .get(&format!("releases/assets/{}", checksum.id), true)
            .await?;
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Checksum download failed")?
        {
            if data.len() + chunk.len() > 1024 {
                return Err("Checksum file exceeds limit".into());
            }
            data.extend_from_slice(&chunk);
        }
        let line = std::str::from_utf8(&data).map_err(|_| "Invalid checksum encoding")?;
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 2
            || parts[1] != binary.name
            || parts[0].len() != 64
            || !parts[0].bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("Invalid release checksum file".into());
        }
        let expected = parts[0].to_ascii_lowercase();
        let mut response = self
            .get(&format!("releases/assets/{}", binary.id), true)
            .await?;
        let mut file = tempfile::NamedTempFile::new()?;
        let mut hash = Sha256::new();
        let mut size = 0;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Binary download interrupted; installation unchanged")?
        {
            size += chunk.len() as u64;
            if size > MAX_BINARY || size > binary.size {
                return Err("Release binary exceeds expected size".into());
            }
            hash.update(&chunk);
            file.write_all(&chunk)?;
        }
        if size != binary.size || format!("{:x}", hash.finalize()) != expected {
            return Err("Release checksum mismatch; installation unchanged".into());
        }
        file.as_file().sync_all()?;
        Ok((file, expected))
    }
}

#[cfg(test)]
mod tests;
