//! Project-scoped persistence outside the source tree, with exclusive ownership.

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::Result;

pub(crate) struct Storage {
    pub database: PathBuf,
    _lease: FileLease,
}

/// Release the lock explicitly: a concurrently spawned child's transient
/// pre-exec duplicate must not keep the parent's open-file-description locked.
pub(crate) struct FileLease(File);

impl FileLease {
    pub fn exclusive(file: File) -> std::io::Result<Self> {
        FileExt::try_lock_exclusive(&file)?;
        Ok(Self(file))
    }
}

impl Drop for FileLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

impl Storage {
    pub fn open(state_dir: &Path, workspace: &Path) -> Result<Self> {
        let key = format!(
            "{:x}",
            Sha256::digest(workspace.as_os_str().as_encoded_bytes())
        );
        let directory = state_dir.join("projects").join(key);
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory)?;
        let lease = private_file(&directory.join("session.lock"))?;
        let lease = FileLease::exclusive(lease).map_err(
            |_| "This project's storage is in use; close its other RoomKit Code instance",
        )?;
        let database = directory.join("history.sqlite3");
        private_file(&database)?;
        Ok(Self {
            database,
            _lease: lease,
        })
    }
}

fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn persistence_is_private_and_lock_releases_with_owner() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let storage = Storage::open(state.path(), root.path())?;
        assert_eq!(
            std::fs::metadata(&storage.database)?.permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(storage.database.parent().ok_or("Missing parent")?)?
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(Storage::open(state.path(), root.path()).is_err());
        // Model the shared open-file-description inherited during process spawn.
        let transient_duplicate = storage._lease.0.try_clone()?;
        drop(storage);
        Storage::open(state.path(), root.path())?;
        drop(transient_duplicate);
        Ok(())
    }
}
