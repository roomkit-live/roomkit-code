//! Exact model files in the Hugging Face Hub cache shared with other clients.
use super::hub::{Asset, Hub, ModelFile, Variant, shard};
use std::io::Write;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) struct Cache(pub PathBuf);

impl Cache {
    pub fn locate() -> Result<Self, String> {
        #[cfg(test)]
        if let Ok(path) = super::TEST_HUB.try_with(|(_, path)| path.clone()) {
            return Ok(Self(path));
        }
        let absolute = |name| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let path = absolute("HF_HUB_CACHE")
            .or_else(|| absolute("HUGGINGFACE_HUB_CACHE"))
            .or_else(|| absolute("HF_HOME").map(|p| p.join("hub")))
            .or_else(|| absolute("XDG_CACHE_HOME").map(|p| p.join("huggingface/hub")))
            .or_else(|| std::env::home_dir().map(|p| p.join(".cache/huggingface/hub")))
            .ok_or("Cannot locate the Hugging Face cache")?;
        Ok(Self(path))
    }

    fn repository(&self, repo: &str) -> PathBuf {
        self.0.join(format!("models--{}", repo.replace('/', "--")))
    }

    pub fn path(&self, file: &ModelFile) -> PathBuf {
        self.repository(&file.repo)
            .join("snapshots")
            .join(&file.revision)
            .join(&file.name)
    }

    pub fn installed(&self, file: &ModelFile) -> bool {
        let path = self.path(file);
        if !path.is_file() {
            return false;
        }
        match shard(&file.name) {
            Some((base, 1, count)) if (1..=1024).contains(&count) => (2..=count).all(|n| {
                self.path(&ModelFile {
                    name: format!("{base}-{n:05}-of-{count:05}.gguf"),
                    ..file.clone()
                })
                .is_file()
            }),
            Some(_) => false,
            None => true,
        }
    }

    /// Resolve a shorthand against a cached quantization before delegating to llama-server.
    pub fn reference(&self, value: &str) -> Option<PathBuf> {
        let (repo, quant) = value.split_once(':').unwrap_or((value, "Q4_K_M"));
        if !super::hub::valid_repo(repo) || quant.is_empty() {
            return None;
        }
        let root = self.repository(repo);
        let main = fs::read_to_string(root.join("refs/main")).unwrap_or_default();
        let mut snapshots: Vec<_> = directories(&root.join("snapshots")).collect();
        snapshots.sort_by_key(|p| {
            (
                p.file_name().and_then(|n| n.to_str()) != Some(main.trim()),
                p.clone(),
            )
        });
        let wanted = quant.to_ascii_lowercase();
        for snapshot in snapshots {
            let rev = snapshot.file_name()?.to_str()?;
            let mut models = Vec::new();
            self.scan(repo, rev, &snapshot, &snapshot, &mut models);
            models.sort_by(|a, b| a.name.cmp(&b.name));
            for model in models {
                let name = shard(&model.name)
                    .map_or_else(|| model.name.trim_end_matches(".gguf"), |(base, _, _)| base);
                if name
                    .to_ascii_lowercase()
                    .strip_suffix(&wanted)
                    .is_some_and(|prefix| {
                        prefix.is_empty() || prefix.ends_with(['-', '_', '.', '/'])
                    })
                {
                    return Some(self.path(&model));
                }
            }
        }
        None
    }

    pub fn models(&self) -> Vec<ModelFile> {
        let mut found = Vec::new();
        for dir in directories(&self.0) {
            let Some(repo) = dir
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|n| n.strip_prefix("models--"))
                .map(|n| n.replace("--", "/"))
            else {
                continue;
            };
            if !super::hub::valid_repo(&repo) {
                continue;
            }
            for snapshot in directories(&dir.join("snapshots")) {
                let Some(rev) = snapshot.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                self.scan(&repo, rev, &snapshot, &snapshot, &mut found);
            }
        }
        found.sort_by_key(ModelFile::id);
        found
    }

    fn scan(&self, repo: &str, rev: &str, root: &Path, dir: &Path, found: &mut Vec<ModelFile>) {
        if found.len() >= 256 {
            return;
        }
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if rel.components().count() < 8 {
                    self.scan(repo, rev, root, &path, found);
                }
            } else if let Some(file) = rel.to_str().and_then(|n| ModelFile::new(repo, rev, n))
                && !file.name.to_ascii_lowercase().contains("mmproj")
                && self.installed(&file)
                && file.id().len() <= 256
            {
                found.push(file);
            }
            if found.len() >= 256 {
                break;
            }
        }
    }

    pub async fn download(
        &self,
        hub: &Hub,
        model: &Variant,
        progress: impl Fn(u64),
    ) -> Result<(), String> {
        let root = self.repository(&model.first.repo);
        let blobs = root.join("blobs");
        fs::create_dir_all(&blobs).map_err(io)?;
        let mut done = 0;
        for file in &model.files {
            self.blob(hub, &blobs, file, |bytes| progress(done + bytes))
                .await?;
            done += file.size;
            progress(done);
        }
        // Publish the first shard last: a model is selectable only once all its files exist.
        for file in model.files.iter().rev() {
            let path = self.path(&file.file);
            publish_link(&path, file)?;
        }
        if hub.is_main(&model.first).await.unwrap_or(false) {
            let refs = root.join("refs");
            fs::create_dir_all(&refs).map_err(io)?;
            let mut temp = tempfile::NamedTempFile::new_in(&refs).map_err(io)?;
            temp.write_all(model.first.revision.as_bytes())
                .map_err(io)?;
            temp.persist(refs.join("main")).map_err(|e| io(e.error))?;
        }
        Ok(())
    }

    async fn blob(
        &self,
        hub: &Hub,
        dir: &Path,
        asset: &Asset,
        progress: impl Fn(u64),
    ) -> Result<(), String> {
        let target = dir.join(&asset.sha);
        if target.metadata().is_ok_and(|m| m.len() == asset.size) {
            return Ok(());
        }
        let locks = self
            .0
            .join(".locks")
            .join(format!("models--{}", asset.file.repo.replace('/', "--")));
        fs::create_dir_all(&locks).map_err(io)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(locks.join(format!("{}.lock", asset.sha)))
            .map_err(io)?;
        loop {
            match fs2::FileExt::try_lock_exclusive(&lock) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_millis(200)).await
                }
                Err(e) => return Err(io(e)),
            }
        }
        if target.metadata().is_ok_and(|m| m.len() == asset.size) {
            return Ok(());
        }
        super::transfer::download(hub, asset, &target, progress).await
    }
}

fn publish_link(path: &Path, asset: &Asset) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid snapshot path")?;
    fs::create_dir_all(parent).map_err(io)?;
    // Snapshots remain usable if the whole Hub cache moves.
    let depth = Path::new(&asset.file.name).components().count() + 1;
    let target: PathBuf = std::iter::repeat_n("..", depth)
        .chain(["blobs", asset.sha.as_str()])
        .collect();
    if fs::read_link(path).is_ok_and(|old| old == target) {
        return Ok(());
    }
    let stage = tempfile::tempdir_in(parent).map_err(io)?;
    let link = stage.path().join("link");
    std::os::unix::fs::symlink(target, &link).map_err(io)?;
    fs::rename(link, path).map_err(io)
}

fn directories(path: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
}
pub(super) fn io(error: std::io::Error) -> String {
    format!("Hugging Face cache: {error}")
}
