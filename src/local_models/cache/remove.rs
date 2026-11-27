//! Remove one installed quantization, keeping blobs linked by other snapshots.
use super::{Cache, ModelFile, io, shard};
use crate::local_models::hub::digest;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

impl Cache {
    // Called while the transfer still owns every blob lock. A transport error or
    // application shutdown leaves these files resumable; explicit cancellation frees them.
    pub(super) fn discard_partial(&self, model: &super::Variant) -> Result<(), String> {
        let root = self.checked_repository(&model.first.repo)?;
        let dir = root.join("blobs");
        real_directory(&dir)?;
        let all: BTreeSet<_> = model
            .files
            .iter()
            .map(|asset| dir.join(&asset.sha))
            .collect();
        for path in &all {
            remove_if_present(&path.with_extension("incomplete"))?;
        }
        let mut unshared = all;
        let snapshots = root.join("snapshots");
        if snapshots.try_exists().map_err(io)? {
            real_directory(&snapshots)?;
            keep_referenced(&snapshots, &BTreeSet::new(), &mut unshared)?;
        }
        for path in unshared {
            remove_if_present(&path)?;
        }
        Ok(())
    }

    pub fn remove(&self, file: &ModelFile) -> Result<(), String> {
        let repo = self.checked_repository(&file.repo)?;
        let snapshots = repo.join("snapshots");
        let snapshot = snapshots.join(&file.revision);
        let paths = selected_paths(&snapshot, &file.name)?;
        // Resolve the complete plan before unlinking anything. Only paths in this
        // repository's blobs directory, with a Hub digest name, can be reclaimed.
        let mut blobs = linked_blobs(&paths, &repo.join("blobs"))?;
        let _locks = self.reserve_for_removal(&file.repo, &blobs)?;
        if !blobs.is_empty() {
            keep_referenced(&snapshots, &paths, &mut blobs)?;
        }
        for path in paths.iter().rev() {
            fs::remove_file(path).map_err(io)?;
        }
        for blob in blobs {
            fs::remove_file(blob).map_err(io)?;
        }
        Ok(())
    }
    fn checked_repository(&self, name: &str) -> Result<PathBuf, String> {
        let repo = Cache(self.0.canonicalize().map_err(io)?).repository(name);
        real_directory(&repo)?;
        Ok(repo)
    }

    fn reserve_for_removal(
        &self,
        repo: &str,
        blobs: &BTreeSet<PathBuf>,
    ) -> Result<Vec<fs::File>, String> {
        // Also respect a writer using the standard Hub lock outside Code.
        let mut locks = Vec::new();
        for path in blobs {
            let hash = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("Invalid blob name")?;
            let lock = self.blob_lock(repo, hash)?;
            fs2::FileExt::try_lock_exclusive(&lock).map_err(
                |_| "A Hub client is using this model. Try again after its transfer finishes.",
            )?;
            locks.push(lock);
        }
        Ok(locks)
    }
}

fn linked_blobs(paths: &BTreeSet<PathBuf>, directory: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut blobs = BTreeSet::new();
    for path in paths {
        let parent = path.parent().ok_or("Invalid GGUF path")?;
        real_directory(parent)?;
        let meta = path.symlink_metadata().map_err(io)?;
        if meta.is_symlink() {
            let target = path.canonicalize().map_err(io)?;
            if target.parent() == Some(directory)
                && target
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| digest(s, 64))
            {
                blobs.insert(target);
            }
        } else if !meta.is_file() {
            return Err("Expected a GGUF file in the snapshot".into());
        }
    }
    Ok(blobs)
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(e)),
    }
}

fn real_directory(path: &Path) -> Result<(), String> {
    if path.canonicalize().map_err(io)? != path || !path.is_dir() {
        return Err("Refusing to remove a model through a redirected cache directory".into());
    }
    Ok(())
}

fn selected_paths(snapshot: &Path, name: &str) -> Result<BTreeSet<PathBuf>, String> {
    match shard(name) {
        Some((base, 1, count)) if (1..=1024).contains(&count) => Ok((1..=count)
            .map(|n| snapshot.join(format!("{base}-{n:05}-of-{count:05}.gguf")))
            .collect()),
        Some(_) => Err("Select the first shard of an installed model".into()),
        None => Ok(BTreeSet::from([snapshot.join(name)])),
    }
}

fn keep_referenced(
    root: &Path,
    selected: &BTreeSet<PathBuf>,
    blobs: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let mut directories = vec![root.to_owned()];
    while let Some(dir) = directories.pop() {
        for entry in fs::read_dir(dir).map_err(io)? {
            let entry = entry.map_err(io)?;
            let path = entry.path();
            if selected.contains(&path) {
                continue;
            }
            let kind = entry.file_type().map_err(io)?;
            if kind.is_dir() {
                directories.push(path);
            } else if kind.is_symlink() {
                match path.canonicalize() {
                    Ok(target) if target.is_dir() => {
                        // Another client may follow this alias to a snapshot. Keep its data.
                        blobs.clear();
                        return Ok(());
                    }
                    Ok(target) => {
                        blobs.remove(&target);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io(e)),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "cache fixtures must be created successfully"
    )]
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn gguf_removal_refuses_redirected_directories_and_busy_weights() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache(dir.path().to_owned());
        let file = ModelFile::new("fixture/model", &"a".repeat(40), "weights.gguf").unwrap();
        let repo = cache.repository(&file.repo);
        let elsewhere = tempfile::tempdir().unwrap();
        symlink(elsewhere.path(), &repo).unwrap();
        assert!(cache.remove(&file).is_err());

        fs::remove_file(&repo).unwrap();
        let blobs = repo.join("blobs");
        fs::create_dir_all(&blobs).unwrap();
        let sha = "b".repeat(64);
        let blob = blobs.join(&sha);
        fs::write(&blob, b"weights").unwrap();
        let snapshot = cache.path(&file);
        fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        symlink(&blob, &snapshot).unwrap();
        let lock = cache.blob_lock(&file.repo, &sha).unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        assert!(cache.remove(&file).unwrap_err().contains("Hub client"));
        assert!(snapshot.is_file() && blob.is_file());
        drop(lock);
        cache.remove(&file).unwrap();
        assert!(!snapshot.exists() && !blob.exists());
    }
}
