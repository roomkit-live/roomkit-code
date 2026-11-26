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
    /// Whether this release can be installed: [`Self::listed`], and signed.
    pub fn validate(&self) -> Result<()> {
        self.listed()?;
        self.signature()?;
        Ok(())
    }

    /// Whether this release can be the latest one: published, stable, with
    /// this platform's binary and checksum. Its signature only matters to
    /// a build it would update.
    fn listed(&self) -> Result<()> {
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
        let (suffix, most) = if checksum {
            (".sha256", 1024)
        } else {
            ("", MAX_BINARY)
        };
        self.named(suffix, most)
            .ok_or_else(|| "The release has no valid binary/checksum pair for this platform".into())
    }

    /// The signature of this platform's checksum file.
    fn signature(&self) -> Result<&Asset> {
        self.named(".sha256.minisig", SIGNATURE_BYTES)
            .ok_or_else(|| "The release has no signature for this platform".into())
    }

    /// The one asset of this platform's binary with `suffix`, of at most
    /// `most` bytes.
    fn named(&self, suffix: &str, most: u64) -> Option<&Asset> {
        let name = format!("roomkit-code-{}-{TARGET}{suffix}", self.tag_name);
        let mut found = self.assets.iter().filter(|a| a.name == name);
        let asset = found.next().filter(|_| found.next().is_none())?;
        (asset.id != 0 && asset.size != 0 && asset.size <= most).then_some(asset)
    }
}

/// The most bytes of a signature file read.
const SIGNATURE_BYTES: u64 = 4096;

/// The public key releases are signed for, in minisign's base64. Its secret
/// half signs each release's checksum files where the release is built, and
/// is in no release: a binary and a checksum that only come from the same
/// place prove nothing about that place.
const RELEASE_KEY: &str = include_str!("release-key.pub");

/// Checks that `signature`, a minisign signature file's text, is `key`'s
/// over `signed`. An error keeps minisign's cause: another key (after a
/// rotation), an old algorithm and a forged file read differently.
fn verify(key: &str, signed: &[u8], signature: &str) -> Result<()> {
    let key = minisign_verify::PublicKey::from_base64(key.trim()).map_err(|error| {
        format!("This build has no valid release key ({error}); it cannot verify an update")
    })?;
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|error| format!("Invalid release signature file ({error})"))?;
    key.verify(signed, &signature, false).map_err(|error| {
        format!("Release signature verification failed ({error}); installation unchanged").into()
    })
}

pub struct GitHub {
    client: reqwest::Client,
    token: Option<String>,
    base: String,
    /// The key a release's signature is checked with.
    key: String,
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
        Self::with_source("https://api.github.com".into(), token, RELEASE_KEY)
    }
    fn with_source(base: String, token: Option<String>, key: &str) -> Result<Self> {
        Ok(Self {
            key: key.to_owned(),
            base,
            token,
            client: crate::http_client::builder()?
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
            if !trusted_download(&url) {
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
        release.listed()?;
        // A release this build would not install is no update, signed or
        // not: one published before releases were signed included.
        if version(release.number())? > version(CURRENT)? {
            release.signature()?;
        }
        Ok(Some(release))
    }
    /// The asset `id`, of at most `most` bytes; `what` names it in an error.
    async fn small(&self, id: u64, most: u64, what: &str) -> Result<Vec<u8>> {
        let mut response = self.get(&format!("releases/assets/{id}"), true).await?;
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| format!("{what} download failed"))?
        {
            if (data.len() + chunk.len()) as u64 > most {
                return Err(format!("{what} file exceeds limit").into());
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }

    pub async fn download(&self, release: &Release) -> Result<(tempfile::NamedTempFile, String)> {
        release.validate()?;
        let binary = release.asset(false)?;
        let checksum = release.asset(true)?;
        let data = self.small(checksum.id, 1024, "Checksum").await?;
        // The checksum is believed once the release key signed it: it then
        // vouches for the binary it names.
        let signature = release.signature()?;
        let signature = self
            .small(signature.id, SIGNATURE_BYTES, "Signature")
            .await?;
        let signature =
            std::str::from_utf8(&signature).map_err(|_| "Invalid release signature file")?;
        verify(&self.key, &data, signature)?;
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
        if size != binary.size || crate::digest::hex(&hash.finalize()) != expected {
            return Err("Release checksum mismatch; installation unchanged".into());
        }
        file.as_file().sync_all()?;
        Ok((file, expected))
    }
}

/// Whether a release asset may be downloaded from `url`, where the API
/// redirected: GitHub's asset stores over HTTPS, or this repository's own
/// release downloads on github.com. Any other path of github.com serves
/// what anyone uploaded there.
fn trusted_download(url: &url::Url) -> bool {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.host_str() {
        Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com") => true,
        Some("github.com") => url
            .path()
            .starts_with("/roomkit-live/roomkit-code/releases/download/"),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
