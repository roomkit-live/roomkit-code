//! Distinguish identical checkout paths on different client installations.
use crate::storage::document::Document;
use std::path::Path;

pub(super) fn remote(state: &Path, project: &Path) -> Result<String, String> {
    let path = state.join("memory-origin.json");
    let mut origin = Document::<String>::load(path.clone())?;
    if origin.revision().is_none() {
        use std::io::Write;
        // Publish once. Racing launches read whichever complete identity won.
        let mut new = tempfile::NamedTempFile::new_in(state).map_err(|e| e.to_string())?;
        write!(new, "\"{}\"", uuid::Uuid::new_v4()).map_err(|e| e.to_string())?;
        new.as_file().sync_all().map_err(|e| e.to_string())?;
        match new.persist_noclobber(&path) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
        origin = Document::load(path)?;
    }
    let uuid = uuid::Uuid::parse_str(&origin.value)
        .map_err(|_| "Invalid identity in memory-origin.json")?;
    let mut key = uuid.as_bytes().to_vec();
    key.extend_from_slice(project.as_os_str().as_encoded_bytes());
    Ok(crate::digest::sha256_hex(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_scope_distinguishes_installations_with_matching_paths() -> crate::Result<()> {
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let path = Path::new("/same/checkout/.git");
        let id = remote(first.path(), path)?;
        assert_eq!(id, remote(first.path(), path)?);
        assert_ne!(id, remote(second.path(), path)?);
        Ok(())
    }
}
