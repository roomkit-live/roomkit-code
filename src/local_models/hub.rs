//! Public Hugging Face GGUF repositories and immutable model selections.
use serde::Deserialize;
use std::time::Duration;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelFile {
    pub repo: String,
    pub revision: String,
    pub name: String,
}

impl ModelFile {
    pub fn parse(id: &str) -> Option<Self> {
        let (repo, tail) = id.strip_prefix("hf://")?.split_once('@')?;
        let (revision, name) = tail.split_once('/')?;
        Self::new(repo, revision, name)
    }

    pub fn new(repo: &str, revision: &str, name: &str) -> Option<Self> {
        (valid_repo(repo)
            && digest(revision, 40)
            && name.ends_with(".gguf")
            && name.split('/').all(component))
        .then(|| Self {
            repo: repo.into(),
            revision: revision.into(),
            name: name.into(),
        })
    }

    pub fn id(&self) -> String {
        format!("hf://{}@{}/{}", self.repo, self.revision, self.name)
    }
}

pub(super) fn digest(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn component(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 200
        && !part.starts_with('.')
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.+ ".contains(&b))
}

pub(crate) fn valid_repo(repo: &str) -> bool {
    repo.len() <= 128
        && repo.split('/').count() == 2
        && repo
            .split('/')
            .all(|p| component(p) && !p.contains(' ') && !p.contains("--"))
}

pub(crate) fn shard(name: &str) -> Option<(&str, u32, u32)> {
    let stem = name.strip_suffix(".gguf")?;
    let (prefix, count) = stem.rsplit_once("-of-")?;
    let (base, index) = prefix.rsplit_once('-')?;
    if index.len() != 5 || count.len() != 5 {
        return None;
    }
    Some((base, index.parse().ok()?, count.parse().ok()?))
}

#[derive(Clone)]
pub(crate) struct Asset {
    pub file: ModelFile,
    pub size: u64,
    pub sha: String,
}

pub(crate) struct Variant {
    pub files: Vec<Asset>,
    pub first: ModelFile,
    pub size: u64,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
}
#[derive(Deserialize)]
struct Repository {
    sha: String,
    siblings: Vec<Sibling>,
}
#[derive(Deserialize)]
struct Sibling {
    rfilename: String,
    lfs: Option<Lfs>,
}
#[derive(Deserialize)]
struct Lfs {
    sha256: String,
    size: u64,
}

/// Which GGUF repositories to request from the Hub before its result limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchScope {
    TextGeneration,
    All,
}

pub(crate) struct Hub {
    root: Url,
    pub(super) client: reqwest::Client,
}

impl Hub {
    pub fn public() -> Result<Self, String> {
        #[cfg(test)]
        if let Ok(url) = super::TEST_HUB.try_with(|(url, _)| url.clone()) {
            return Self::at(url);
        }
        Self::at(Url::parse("https://huggingface.co/").map_err(|e| e.to_string())?)
    }

    pub(super) fn at(root: Url) -> Result<Self, String> {
        let client = crate::http_client::builder()
            .map_err(|e| e.to_string())?
            .connect_timeout(Duration::from_secs(12))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client, root })
    }

    pub(super) fn url(&self, segments: &[&str]) -> Result<Url, String> {
        let mut url = self.root.clone();
        url.path_segments_mut()
            .map_err(|_| "Invalid Hub endpoint")?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    async fn metadata<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, String> {
        let mut response = self
            .client
            .get(url)
            .timeout(Duration::from_secs(25))
            .send()
            .await
            .map_err(|_| "Hugging Face is unreachable; cached models are available offline")?;
        if !response.status().is_success() {
            return Err(format!("Hugging Face returned HTTP {}", response.status()));
        }
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Could not read Hugging Face metadata")?
        {
            if data.len() + chunk.len() > 8 * 1024 * 1024 {
                return Err("Hugging Face metadata is too large".into());
            }
            data.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&data).map_err(|_| "Invalid Hugging Face metadata".into())
    }

    pub(super) async fn is_main(&self, file: &ModelFile) -> Result<bool, String> {
        #[derive(Deserialize)]
        struct Head {
            sha: String,
        }
        let mut segments = vec!["api", "models"];
        segments.extend(file.repo.split('/'));
        let head: Head = self.metadata(self.url(&segments)?).await?;
        Ok(head.sha == file.revision)
    }

    pub async fn search(&self, query: &str, scope: SearchScope) -> Result<Vec<String>, String> {
        if query.len() > 128 || query.chars().any(char::is_control) {
            return Err("Model search is too long or contains a control character".into());
        }
        let mut url = self.url(&["api", "models"])?;
        url.query_pairs_mut().extend_pairs([
            ("filter", "gguf"),
            ("search", query),
            ("limit", "30"),
            ("sort", "downloads"),
        ]);
        if scope == SearchScope::TextGeneration {
            url.query_pairs_mut()
                .append_pair("pipeline_tag", "text-generation");
        }
        Ok(self
            .metadata::<Vec<Entry>>(url)
            .await?
            .into_iter()
            .map(|e| e.id)
            .filter(|r| valid_repo(r))
            .take(30)
            .collect())
    }

    pub async fn variants(
        &self,
        repo: &str,
        revision: Option<&str>,
    ) -> Result<Vec<Variant>, String> {
        if !valid_repo(repo) {
            return Err("Invalid Hugging Face repository".into());
        }
        let mut pieces = vec!["api", "models"];
        pieces.extend(repo.split('/'));
        if let Some(commit) = revision {
            if !digest(commit, 40) {
                return Err("Invalid model revision".into());
            }
            pieces.extend(["revision", commit]);
        }
        let mut url = self.url(&pieces)?;
        url.query_pairs_mut().append_pair("blobs", "true");
        let result: Repository = self.metadata(url).await?;
        if revision.is_some_and(|r| r != result.sha) {
            return Err("The Hub returned a different model revision".into());
        }
        let mut files = Vec::new();
        for sibling in result.siblings {
            let Some(lfs) = sibling.lfs else { continue };
            let Some(file) = ModelFile::new(repo, &result.sha, &sibling.rfilename) else {
                continue;
            };
            if file.name.to_ascii_lowercase().contains("mmproj")
                || !digest(&lfs.sha256, 64)
                || lfs.size == 0
            {
                continue;
            }
            files.push(Asset {
                file,
                size: lfs.size,
                sha: lfs.sha256,
            });
        }
        Ok(variants(files))
    }
}

fn variants(files: Vec<Asset>) -> Vec<Variant> {
    let mut groups = std::collections::BTreeMap::<String, Vec<Asset>>::new();
    for file in files {
        let group = shard(&file.file.name).map_or(file.file.name.as_str(), |(base, _, _)| base);
        groups.entry(group.to_owned()).or_default().push(file);
    }
    groups
        .into_values()
        .filter_map(|mut files| {
            files.sort_by(|a, b| a.file.name.cmp(&b.file.name));
            let first = files.first()?.file.clone();
            if let Some((_, _, total)) = shard(&first.name)
                && (usize::try_from(total).ok()? != files.len()
                    || !files.iter().enumerate().all(|(i, f)| {
                        shard(&f.file.name)
                            .is_some_and(|(_, n, count)| n as usize == i + 1 && count == total)
                    }))
            {
                return None;
            }
            let size = files
                .iter()
                .try_fold(0u64, |sum, f| sum.checked_add(f.size))?;
            Some(Variant { first, files, size })
        })
        .collect()
}
