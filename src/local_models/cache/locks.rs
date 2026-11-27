//! Locks shared with other Hub clients, held until all shards have been published.
use super::{Cache, io};
use std::{collections::BTreeSet, fs, time::Duration};
use tokio_util::sync::CancellationToken;

impl Cache {
    pub(super) fn blob_lock(&self, repo: &str, sha: &str) -> Result<fs::File, String> {
        let dir = self
            .0
            .join(".locks")
            .join(format!("models--{}", repo.replace('/', "--")));
        fs::create_dir_all(&dir).map_err(io)?;
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{sha}.lock")))
            .map_err(io)
    }

    pub(super) async fn reserve_blobs(
        &self,
        model: &super::Variant,
        cancel: &CancellationToken,
    ) -> Result<Vec<fs::File>, String> {
        let mut locks = Vec::new();
        // Identical weights can be named in more than one variant. Every writer
        // reserves their digests once in the same order, including for cache hits.
        let hashes: BTreeSet<_> = model.files.iter().map(|a| &a.sha).collect();
        for hash in hashes {
            let lock = self.blob_lock(&model.first.repo, hash)?;
            loop {
                match fs2::FileExt::try_lock_exclusive(&lock) {
                    Ok(()) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => tokio::select! {
                        () = cancel.cancelled() => return Err("Download cancelled".into()),
                        () = tokio::time::sleep(Duration::from_millis(200)) => {}
                    },
                    Err(e) => return Err(io(e)),
                }
            }
            locks.push(lock);
        }
        Ok(locks)
    }
}
