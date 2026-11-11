//! Immutable versions and atomic activation, confined to an explicitly managed install.
use super::*;
use crate::storage::FileLease;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Installation {
    pub bin: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    version: String,
    target: String,
    sha256: String,
}

pub fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_BINARY {
        return Err("Invalid release binary size or type".into());
    }
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    let mut size = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_BINARY {
            return Err("Release binary exceeds size limit".into());
        }
        hash.update(&buf[..n]);
    }
    Ok(crate::digest::hex(&hash.finalize()))
}

fn regular_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(()),
        Ok(_) => Err("Managed installation directory must not be a symlink".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn pointer(root: &Path, name: &str, target: &Path) -> Result<()> {
    let temp = root.join(format!(".link-{}", uuid::Uuid::new_v4()));
    symlink(target, &temp)?;
    let result = fs::rename(&temp, root.join(name));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}

impl Installation {
    pub fn root(&self) -> PathBuf {
        self.bin.join(".roomkit-code")
    }
    pub fn detect(exe: &Path) -> Option<Self> {
        let exe = exe.canonicalize().ok()?;
        let root = exe.parent()?.parent()?.parent()?;
        if root.file_name()? != ".roomkit-code" {
            return None;
        }
        let bin = root.parent()?.to_path_buf();
        let install = Self { bin };
        if fs::read_link(install.bin.join("roomkit-code")).ok()?
            != Path::new(".roomkit-code/current/roomkit-code")
        {
            return None;
        }
        install.version_at(exe.parent()?).ok()?;
        Some(install)
    }
    fn lock(&self) -> Result<FileLease> {
        regular_dir(&self.root())?;
        regular_dir(&self.root().join("versions"))?;
        let path = self.root().join("install.lock");
        // Do not truncate or follow a link supplied as a lock file.
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err("Invalid installation lock".into());
        }
        // Explicit unlock also releases transient copies inherited by a child
        // between fork and exec, just like the application's storage lease.
        FileLease::exclusive(file).map_err(|_| "Another installation or update is running".into())
    }
    fn version_at(&self, dir: &Path) -> Result<Receipt> {
        if fs::symlink_metadata(dir)?.file_type().is_symlink()
            || dir.parent() != Some(self.root().join("versions").as_path())
        {
            return Err("Invalid managed version directory".into());
        }
        let receipt = dir.join("release.json");
        if fs::symlink_metadata(&receipt)?.file_type().is_symlink()
            || fs::metadata(&receipt)?.len() > 4096
        {
            return Err("Invalid installation receipt".into());
        }
        let r: Receipt = serde_json::from_slice(&fs::read(receipt)?)?;
        version(&r.version)?;
        if r.target != TARGET
            || r.sha256.len() != 64
            || !r.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Installation receipt has an invalid target or checksum".into());
        }
        let exe = dir.join("roomkit-code");
        if fs::symlink_metadata(&exe)?.file_type().is_symlink() || digest(&exe)? != r.sha256 {
            return Err("Installed binary checksum does not match its receipt".into());
        }
        Ok(r)
    }
    fn resolve(&self, name: &str) -> Result<PathBuf> {
        let link = fs::read_link(self.root().join(name))?;
        let parts = link.components().collect::<Vec<_>>();
        if parts.len() != 2
            || parts[0] != Component::Normal("versions".as_ref())
            || !matches!(parts[1], Component::Normal(_))
        {
            return Err("Invalid managed installation pointer".into());
        }
        let dir = self.root().join(link);
        self.version_at(&dir)?;
        Ok(dir)
    }
    pub fn previous(&self) -> Result<String> {
        let dir = self
            .resolve("previous")
            .map_err(|_| "No verified previous version is available for rollback")?;
        if self.resolve("current")? == dir {
            return Err("No different previous version is available".into());
        }
        Ok(self.version_at(&dir)?.version)
    }
    pub fn ensure_current(&self, exe: &Path) -> Result<()> {
        const RESTART: &str =
            "Another version was activated. Restart RoomKit Code before updating again";
        // A later update may have removed this process's version directory.
        let running = exe.canonicalize().map_err(|_| RESTART)?;
        // The configured bin directory can contain an alias such as macOS's
        // /var -> /private/var. Resolve both sides after validating the receipt.
        if self
            .resolve("current")?
            .join("roomkit-code")
            .canonicalize()?
            != running
        {
            return Err(RESTART.into());
        }
        Ok(())
    }
    pub fn activate(
        &self,
        source: &Path,
        release: &str,
        checksum: &str,
        expected_exe: Option<&Path>,
    ) -> Result<()> {
        version(release)?;
        fs::create_dir_all(&self.bin)?;
        let _lock = self.lock()?;
        let entry = self.bin.join("roomkit-code");
        let exists = fs::symlink_metadata(&entry).is_ok();
        if exists
            && fs::read_link(&entry).ok().as_deref()
                != Some(Path::new(".roomkit-code/current/roomkit-code"))
        {
            return Err(
                "roomkit-code already exists here and is not managed; choose another --install-dir"
                    .into(),
            );
        }
        if let Some(exe) = expected_exe {
            self.ensure_current(exe)?;
        }
        let old = if exists {
            Some(self.resolve("current")?)
        } else {
            None
        };
        if digest(source)? != checksum {
            return Err("Downloaded binary checksum mismatch; installation unchanged".into());
        }
        let name = format!("{release}-{checksum}");
        let dir = self.root().join("versions").join(&name);
        if !dir.exists() {
            let staging = tempfile::Builder::new()
                .prefix(".stage-")
                .tempdir_in(self.root().join("versions"))?;
            let binary = staging.path().join("roomkit-code");
            fs::copy(source, &binary)?;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
            if digest(&binary)? != checksum {
                return Err("Staged binary checksum mismatch".into());
            }
            fs::File::open(&binary)?.sync_all()?;
            let mut meta = fs::File::create(staging.path().join("release.json"))?;
            meta.write_all(&serde_json::to_vec(&Receipt {
                version: release.into(),
                target: TARGET.into(),
                sha256: checksum.into(),
            })?)?;
            meta.sync_all()?;
            fs::rename(staging.path(), &dir)?;
            fs::File::open(self.root().join("versions"))?.sync_all()?;
        }
        self.version_at(&dir)?;
        if old.as_ref() != Some(&dir) {
            if let Some(old) = old {
                pointer(&self.root(), "previous", old.strip_prefix(self.root())?)?;
            }
            // Activation is last. Before it, crashes leave the old current version intact.
            pointer(
                &self.root(),
                "current",
                Path::new("versions").join(name).as_path(),
            )?;
            if !exists {
                symlink(".roomkit-code/current/roomkit-code", &entry)?;
            }
            fs::File::open(&self.bin)?.sync_all()?;
        }
        self.prune();
        Ok(())
    }

    /// Remove every version but the current and previous ones, and staging
    /// left by an interrupted installation: each update adds up to
    /// `MAX_BINARY`. Runs under the installation lock, once the pointers
    /// moved. A running process keeps its binary open; a version that cannot
    /// be removed now is removed by a later update.
    fn prune(&self) {
        let versions = self.root().join("versions");
        let kept: Vec<PathBuf> = ["current", "previous"]
            .iter()
            .filter_map(|name| fs::read_link(self.root().join(name)).ok())
            .map(|link| self.root().join(link))
            .collect();
        let Ok(entries) = fs::read_dir(&versions) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let directory = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if directory && !kept.contains(&path) {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }
    pub fn rollback(&self, exe: &Path, expected: &str) -> Result<()> {
        let _lock = self.lock()?;
        self.ensure_current(exe)?;
        let old = self.resolve("current")?;
        let previous = self.resolve("previous")?;
        if previous == old || self.version_at(&previous)?.version != expected {
            return Err("The previous version changed; review rollback again".into());
        }
        pointer(&self.root(), "previous", old.strip_prefix(self.root())?)?;
        pointer(&self.root(), "current", previous.strip_prefix(self.root())?)
    }
}
