//! One durable sequence per endpoint/pane/source, independent of project storage.

use crate::storage::FileLease;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) struct Sequence {
    path: PathBuf,
    last: u64,
    _lease: FileLease,
}

impl Sequence {
    pub fn open(endpoint: &super::Endpoint) -> std::io::Result<Self> {
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(&endpoint.storage)?;
        let mut hash = Sha256::new();
        for value in [
            endpoint.socket.as_encoded_bytes(),
            endpoint.pane.as_bytes(),
            super::SOURCE.as_bytes(),
        ] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
        let key = crate::digest::hex(&hash.finalize());
        let path = endpoint.storage.join(format!("{key}.json"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lease =
            FileLease::exclusive(options.open(endpoint.storage.join(format!("{key}.lock")))?)?;
        let last = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        Ok(Self {
            path,
            last,
            _lease: lease,
        })
    }

    pub fn reserve(&mut self) -> std::io::Result<u64> {
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        self.reserve_at(now)
    }

    fn reserve_at(&mut self, now: u64) -> std::io::Result<u64> {
        let next = self
            .last
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("Sequence exhausted"))?
            .max(now);
        let directory = self
            .path
            .parent()
            .ok_or_else(|| std::io::Error::other("Missing sequence directory"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        write!(temporary, "{next}")?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        #[cfg(unix)]
        std::fs::File::open(directory)?.sync_all()?;
        self.last = next;
        Ok(next)
    }

    #[cfg(test)]
    pub(super) fn reserve_for_test(&mut self, now: u64) -> std::io::Result<u64> {
        self.reserve_at(now)
    }
}
