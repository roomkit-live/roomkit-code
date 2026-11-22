//! Versioned private JSON documents. No values are copied into conversation storage.

use crate::storage::FileLease;
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

pub(crate) struct Document<T> {
    pub value: T,
    path: PathBuf,
    revision: Option<Vec<u8>>,
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read local settings file".into()),
    };
    if !metadata.is_file() || metadata.len() > 2_097_152 {
        return Err("Settings file must be a regular file smaller than 2 MiB".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Settings file permissions must be private (chmod 600)".into());
        }
    }
    std::fs::read(path)
        .map(Some)
        .map_err(|_| "Cannot read local settings file".into())
}

impl<T: Serialize + DeserializeOwned + Default> Document<T> {
    pub fn revision(&self) -> Option<Vec<u8>> {
        self.revision.clone()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file still holds what was loaded, for a change that depends
    /// on it and writes other files first.
    pub fn is_current(&self) -> Result<bool, String> {
        Ok(read(&self.path)?.map(|b| Sha256::digest(b).to_vec()) == self.revision)
    }
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let bytes = read(&path)?;
        let value = bytes
            .as_ref()
            .map(|bytes| {
                serde_json::from_slice(bytes).map_err(|_| {
                    "Invalid local settings file; its contents were not printed".to_string()
                })
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            value,
            path,
            revision: bytes.map(|b| Sha256::digest(b).to_vec()),
        })
    }

    pub fn reload(&mut self) -> Result<(), String> {
        *self = Self::load(self.path.clone())?;
        Ok(())
    }

    /// Optimistic revision check under a short cross-process lock. A conflicting
    /// edit is never silently overwritten, even from another project instance.
    pub fn save(&mut self, value: T) -> Result<(), String> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock_path = self.path.with_extension("lock");
        if std::fs::symlink_metadata(&lock_path).is_ok_and(|m| !m.is_file()) {
            return Err("Settings lock must be a regular file".into());
        }
        let lock = options
            .open(lock_path)
            .map_err(|_| "Cannot open settings lock")?;
        let _lease = FileLease::exclusive(lock)
            .map_err(|_| "Settings are being edited elsewhere; retry shortly")?;
        if !self.is_current()? {
            return Err("Settings changed in another instance; cancel and reopen the form".into());
        }
        let bytes = serde_json::to_vec_pretty(&value).map_err(|_| "Cannot encode settings")?;
        if bytes.len() > 2_097_152 {
            return Err("Settings storage exceeds 2 MiB".into());
        }
        let parent = self.path.parent().ok_or("Invalid settings location")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| "Cannot create private settings file")?;
        temporary
            .write_all(&bytes)
            .map_err(|_| "Cannot write settings")?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| "Cannot flush settings")?;
        temporary
            .persist(&self.path)
            .map_err(|_| "Cannot replace settings atomically")?;
        self.revision = Some(Sha256::digest(bytes).to_vec());
        self.value = value;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "A fixture filesystem failure should fail the test"
    )]
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn concurrent_project_edit_cannot_overwrite_a_newer_shared_connection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("connections.json");
        let mut first = Document::<Value>::load(path.clone()).unwrap();
        let mut second = Document::<Value>::load(path.clone()).unwrap();
        first.save(json!({"first": true})).unwrap();
        assert!(second.save(json!({"second": true})).is_err());
        assert_eq!(
            Document::<Value>::load(path).unwrap().value,
            json!({"first": true})
        );
        second.reload().unwrap();
        second.save(json!({"second": true})).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn credential_files_are_private_and_symlinks_are_refused() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("connections.json");
        let mut doc = Document::<Value>::load(path.clone()).unwrap();
        doc.save(json!({"secret": "fixture-secret"})).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = temp.path().join("link.json");
        symlink(&path, &link).unwrap();
        assert!(Document::<Value>::load(link).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Document::<Value>::load(path).is_err());
    }
}
