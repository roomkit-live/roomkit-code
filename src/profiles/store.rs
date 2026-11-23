//! Coordinate edits that span several profile documents.
use crate::storage::FileLease;
use std::{fs::OpenOptions, path::Path};

/// Serialize cross-document profile edits, including provider reference checks.
pub(super) fn edit_lease(root: &Path) -> Result<FileLease, String> {
    FileLease::exclusive(edit_file(root)?)
        .map_err(|_| "Profiles are being edited elsewhere; retry shortly".into())
}

pub(super) async fn load_lease(root: &Path) -> Result<FileLease, String> {
    FileLease::exclusive_async(edit_file(root)?)
        .await
        .map_err(|_| "Profiles are being edited elsewhere; retry shortly".into())
}

fn edit_file(root: &Path) -> Result<std::fs::File, String> {
    let path = root.join("profiles.lock");
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
        return Err("Profile edit lock must be a regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| "Cannot open profile edit lock".into())
}
