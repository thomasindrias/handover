use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result, io};

struct TempPath {
    path: PathBuf,
    armed: bool,
}

impl TempPath {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn replace_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = private_parent(path)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        uuid::Uuid::new_v4()
    ));
    let mut cleanup = TempPath::new(temp.clone());
    write_new_private(&temp, contents)?;

    std::fs::rename(&temp, path).map_err(|source| io(path, source))?;
    cleanup.disarm();
    sync_directory(parent)?;
    Ok(())
}

pub fn create_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = private_parent(path)?;
    let temp = parent.join(format!(".create.{}.tmp", uuid::Uuid::new_v4()));
    let mut cleanup = TempPath::new(temp.clone());
    write_new_private(&temp, contents)?;

    std::fs::hard_link(&temp, path).map_err(|source| io(path, source))?;
    sync_directory(parent)?;
    std::fs::remove_file(&temp).map_err(|source| io(&temp, source))?;
    cleanup.disarm();
    sync_directory(parent)?;
    Ok(())
}

pub fn read_private(path: &Path) -> Result<Vec<u8>> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io(path, source))?;
    let metadata = file.metadata().map_err(|source| io(path, source))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(Error::InvalidState(format!(
            "refusing insecure private file {}",
            path.display(),
        )));
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io(path, source))?;
    Ok(bytes)
}

fn private_parent(path: &Path) -> Result<&Path> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::InvalidState(format!("{} has no parent", path.display())))?;
    super::ensure_private_dir(parent)?;
    Ok(parent)
}

fn write_new_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| io(path, source))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|source| io(path, source))?;
    file.write_all(contents)
        .map_err(|source| io(path, source))?;
    file.sync_all().map_err(|source| io(path, source))?;
    Ok(())
}

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    let directory = std::fs::File::open(path).map_err(|source| io(path, source))?;
    directory.sync_all().map_err(|source| io(path, source))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::{create_private, read_private, replace_private};

    #[test]
    fn replacement_is_complete_and_private() {
        let temp = TempDir::new().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("ref.json");

        replace_private(&path, b"first").unwrap();
        replace_private(&path, b"second").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn immutable_create_never_replaces_an_existing_file() {
        let temp = TempDir::new().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("checkpoint.json");

        create_private(&path, b"first").unwrap();
        assert!(create_private(&path, b"second").is_err());

        assert_eq!(read_private(&path).unwrap(), b"first");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn private_reads_refuse_symlinks_and_insecure_modes() {
        let temp = TempDir::new().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = temp.path().join("target");
        create_private(&target, b"secret").unwrap();
        let link = temp.path().join("link");
        symlink(&target, &link).unwrap();

        assert!(read_private(&link).is_err());

        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(read_private(&target).is_err());
    }
}
