//! Shared project persistence and exclusive ownership of open conversations.

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::Result;

pub(crate) struct Storage {
    pub database: PathBuf,
    pub sessions: Arc<SessionLeases>,
}

/// Release the lock explicitly: a concurrently spawned child's transient
/// pre-exec duplicate must not keep the parent's open-file-description locked.
pub(crate) struct FileLease(File);

impl FileLease {
    pub fn exclusive(file: File) -> std::io::Result<Self> {
        FileExt::try_lock_exclusive(&file)?;
        Ok(Self(file))
    }

    /// Bounded wait for a short synchronous file transaction, never held over an await.
    pub fn exclusive_wait(file: File) -> io::Result<Self> {
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(Self(file)),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Startup may briefly wait for another instance's schema/profile initialization.
    pub async fn exclusive_async(file: File) -> io::Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(Self(file)),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
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
        let sessions = directory.join("session-locks");
        builder.create(&sessions)?;
        // An existing directory keeps its mode through create: restore it.
        #[cfg(unix)]
        for private in [&directory, &sessions] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700))?;
        }
        let database = directory.join("history.sqlite3");
        private_file(&database)?;
        Ok(Self {
            database,
            sessions: Arc::new(SessionLeases {
                directory: sessions,
                leases: Mutex::new(Some(HashMap::new())),
            }),
        })
    }

    pub async fn initialization_lease(&self) -> io::Result<FileLease> {
        FileLease::exclusive_async(private_file(
            &self.database.with_file_name("initialize.lock"),
        )?)
        .await
    }
}

pub(crate) const SESSION_BUSY: &str = "This conversation is open in another RoomKit Code instance. Choose another session or close that instance";

pub(crate) struct SessionLeases {
    directory: PathBuf,
    // Sessions remain open across view changes, including their hooks and jobs.
    // None means shutdown finished: no stale handler may reclaim a session.
    leases: Mutex<Option<HashMap<String, FileLease>>>,
}

pub(crate) struct SessionClaim {
    id: String,
    lease: Option<FileLease>,
}

impl SessionLeases {
    /// Claim before reading state that can cause a write. None means occupied.
    /// An uncommitted claim is released on errors or task cancellation.
    pub fn claim(&self, id: &str) -> io::Result<Option<SessionClaim>> {
        let leases = self
            .leases
            .lock()
            .map_err(|_| io::Error::other("Session ownership unavailable"))?;
        let leases = leases
            .as_ref()
            .ok_or_else(|| io::Error::other("Application is closed"))?;
        if leases.contains_key(id) {
            return Ok(Some(SessionClaim {
                id: id.into(),
                lease: None,
            }));
        }
        let path = self
            .directory
            .join(format!("{:x}.lock", Sha256::digest(id.as_bytes())));
        match FileLease::exclusive(private_file(&path)?) {
            Ok(lease) => Ok(Some(SessionClaim {
                id: id.into(),
                lease: Some(lease),
            })),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn keep(&self, claim: SessionClaim) -> io::Result<()> {
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| io::Error::other("Session ownership unavailable"))?;
        let leases = leases
            .as_mut()
            .ok_or_else(|| io::Error::other("Application is closed"))?;
        if let Some(lease) = claim.lease {
            leases.insert(claim.id, lease);
        }
        Ok(())
    }

    pub fn busy(&self, id: &str) -> io::Result<bool> {
        Ok(self.claim(id)?.is_none())
    }

    pub fn close(&self) {
        crate::sync::lock(&self.leases).take();
    }
}

pub(crate) fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("Expected a regular private file"));
    }
    Ok(file)
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
        // Directories created loosely by another tool are made private again.
        let locks = storage.database.with_file_name("session-locks");
        std::fs::set_permissions(&locks, std::fs::Permissions::from_mode(0o755))?;
        let second = Storage::open(state.path(), root.path())?;
        assert_eq!(
            std::fs::metadata(&locks)?.permissions().mode() & 0o777,
            0o700
        );
        let claim = storage.sessions.claim("one")?.ok_or("Missing claim")?;
        storage.sessions.keep(claim)?;
        assert!(second.sessions.claim("one")?.is_none());
        assert!(second.sessions.claim("two")?.is_some());
        // Model the shared open-file-description inherited during process spawn.
        let transient_duplicate = storage
            .sessions
            .leases
            .lock()
            .map_err(|_| "poisoned")?
            .as_ref()
            .ok_or("closed")?
            .get("one")
            .ok_or("missing")?
            .0
            .try_clone()?;
        storage.sessions.close();
        assert!(second.sessions.claim("one")?.is_some());
        drop(transient_duplicate);
        Ok(())
    }
}
