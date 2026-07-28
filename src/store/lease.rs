use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io};
use crate::model::{Provider, RunId, SessionId};
use crate::store::atomic::{create_private, sync_directory};
use crate::store::refs::{read_json, write_json, write_json_create};

pub struct SessionOperationLock {
    file: std::fs::File,
}

impl SessionOperationLock {
    pub fn acquire(session_dir: &Path) -> Result<Self> {
        super::ensure_private_dir(session_dir)?;
        let path = session_dir.join("operation.lock");
        match create_private(&path, b"") {
            Ok(()) => {}
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            }
            Err(error) => return Err(error),
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|source| io(&path, source))?;
        validate_private_lock(&file, &path)?;
        file.lock_exclusive().map_err(|source| io(&path, source))?;
        Ok(Self { file })
    }
}

impl Drop for SessionOperationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_token: String,
}

impl ProcessIdentity {
    pub fn capture(pid: u32) -> Result<Self> {
        if pid == 0 {
            return Err(Error::Command("process ID must be positive".into()));
        }
        let output = Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .env("LC_ALL", "C")
            .output()
            .map_err(|error| Error::Command(format!("cannot inspect process {pid}: {error}")))?;
        if !output.status.success() {
            return Err(Error::Command(format!("process {pid} does not exist")));
        }
        let start_token = std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::Command(format!("process {pid} identity is not UTF-8")))?
            .trim()
            .to_owned();
        if start_token.is_empty() {
            return Err(Error::Command(format!(
                "process {pid} has no start identity"
            )));
        }
        Ok(Self { pid, start_token })
    }

    pub fn is_live(&self) -> Result<bool> {
        match Self::capture(self.pid) {
            Ok(current) => Ok(current == *self),
            Err(Error::Command(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn describe(&self) -> String {
        format!("pid {}, started {}", self.pid, self.start_token)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunLease {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub provider: Provider,
    pub host: String,
    pub supervisor: ProcessIdentity,
    pub child: Option<ProcessIdentity>,
}

impl RunLease {
    pub fn new(
        session_id: SessionId,
        run_id: RunId,
        provider: Provider,
        supervisor: ProcessIdentity,
    ) -> Result<Self> {
        Ok(Self {
            schema_version: 1,
            session_id,
            run_id,
            provider,
            host: host_name()?,
            supervisor,
            child: None,
        })
    }

    #[cfg(test)]
    fn fixture(supervisor: ProcessIdentity) -> Self {
        Self {
            schema_version: 1,
            session_id: SessionId::new(),
            run_id: RunId::new(),
            provider: Provider::Claude,
            host: "test-host".into(),
            supervisor,
            child: None,
        }
    }
}

/// The process still holding `lease`, or `None` when the lease is stale.
///
/// The child is asked first and wins when it is live, because it is the
/// provider the user would have to quit; the supervisor stands in for it
/// otherwise. Every caller that has to tell a live lease from a dead one asks
/// here, so a `RunLease` that grows another process identity changes liveness
/// in exactly one place instead of silently leaving some callers behind.
pub fn live_holder(lease: &RunLease) -> Result<Option<&ProcessIdentity>> {
    let child_live = lease
        .child
        .as_ref()
        .map(ProcessIdentity::is_live)
        .transpose()?
        .unwrap_or(false);
    if child_live {
        return Ok(lease.child.as_ref());
    }
    if lease.supervisor.is_live()? {
        return Ok(Some(&lease.supervisor));
    }
    Ok(None)
}

#[derive(Clone, Debug)]
pub struct LeaseStore {
    path: PathBuf,
}

impl LeaseStore {
    pub fn new(session_dir: &Path) -> Self {
        Self {
            path: session_dir.join("refs/active-run.json"),
        }
    }

    pub fn create(&self, lease: &RunLease) -> Result<()> {
        validate_lease(lease)?;
        if let Some(existing) = self.read()? {
            if live_holder(&existing)?.is_some() {
                return Err(Error::InvalidState(format!(
                    "session already has active provider {}",
                    existing.run_id
                )));
            }
            return Err(Error::InvalidState(format!(
                "session has stale lease {}; recover it before launching",
                existing.run_id
            )));
        }
        write_json_create(&self.path, lease)
    }

    pub fn update_child(&self, expected: &RunId, child: ProcessIdentity) -> Result<()> {
        let mut lease = self
            .read()?
            .ok_or_else(|| Error::InvalidState("active run lease disappeared".into()))?;
        if &lease.run_id != expected {
            return Err(Error::InvalidState("active run lease changed".into()));
        }
        if lease
            .child
            .as_ref()
            .is_some_and(|current| current != &child)
        {
            return Err(Error::InvalidState(
                "active run child identity changed".into(),
            ));
        }
        lease.child = Some(child);
        validate_lease(&lease)?;
        write_json(&self.path, &lease)
    }

    pub fn read(&self) -> Result<Option<RunLease>> {
        match std::fs::symlink_metadata(&self.path) {
            Ok(_) => {
                let lease = read_json(&self.path)?;
                validate_lease(&lease)?;
                Ok(Some(lease))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io(&self.path, source)),
        }
    }

    pub fn clear(&self, expected: &RunId) -> Result<()> {
        let lease = self
            .read()?
            .ok_or_else(|| Error::InvalidState("active run lease disappeared".into()))?;
        if &lease.run_id != expected {
            return Err(Error::InvalidState(
                "refusing to clear a different run lease".into(),
            ));
        }
        std::fs::remove_file(&self.path).map_err(|source| io(&self.path, source))?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Error::InvalidState("lease path has no parent".into()))?;
        sync_directory(parent)
    }
}

pub fn host_name() -> Result<String> {
    let output = Command::new("hostname")
        .output()
        .map_err(|error| Error::Command(format!("cannot read hostname: {error}")))?;
    if !output.status.success() {
        return Err(Error::Command(format!(
            "hostname exited with {}",
            output.status
        )));
    }
    let host = std::str::from_utf8(&output.stdout)
        .map_err(|_| Error::Command("hostname is not valid UTF-8".into()))?
        .trim()
        .to_owned();
    if host.is_empty() {
        return Err(Error::Command("hostname is empty".into()));
    }
    Ok(host)
}

fn validate_lease(lease: &RunLease) -> Result<()> {
    if lease.schema_version != 1
        || lease.host.trim().is_empty()
        || lease.supervisor.pid == 0
        || lease.supervisor.start_token.trim().is_empty()
        || lease
            .child
            .as_ref()
            .is_some_and(|child| child.pid == 0 || child.start_token.trim().is_empty())
    {
        return Err(Error::InvalidState(
            "active run lease is incomplete or has an unsupported schema".into(),
        ));
    }
    Ok(())
}

fn validate_private_lock(file: &std::fs::File, path: &Path) -> Result<()> {
    let metadata = file.metadata().map_err(|source| io(path, source))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(Error::InvalidState(format!(
            "refusing insecure session operation lock {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::{LeaseStore, ProcessIdentity, RunLease, SessionOperationLock};

    #[test]
    fn current_process_identity_is_live() {
        let identity = ProcessIdentity::capture(std::process::id()).unwrap();
        assert!(identity.is_live().unwrap());
    }

    #[test]
    fn describe_formats_pid_and_start_token() {
        let identity = ProcessIdentity {
            pid: 4821,
            start_token: "Tue Jul 21 09:14:02 2026".into(),
        };
        assert_eq!(
            identity.describe(),
            "pid 4821, started Tue Jul 21 09:14:02 2026"
        );
    }

    #[test]
    fn pid_reuse_is_not_treated_as_the_same_process() {
        let identity = ProcessIdentity {
            pid: std::process::id(),
            start_token: "definitely-not-this-process".into(),
        };
        assert!(!identity.is_live().unwrap());
    }

    #[test]
    fn live_lease_cannot_be_replaced() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = LeaseStore::new(temp.path());
        let lease = RunLease::fixture(ProcessIdentity::capture(std::process::id()).unwrap());
        store.create(&lease).unwrap();
        assert!(
            store
                .create(&lease)
                .unwrap_err()
                .to_string()
                .contains("active provider")
        );
    }

    #[test]
    fn operation_lock_serializes_lease_check_and_create() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = SessionOperationLock::acquire(temp.path()).unwrap();
        let path = temp.path().to_path_buf();
        let waiting = std::thread::spawn(move || {
            let _second = SessionOperationLock::acquire(&path).unwrap();
            42
        });
        std::thread::sleep(std::time::Duration::from_millis(25));
        assert!(!waiting.is_finished());
        drop(first);
        assert_eq!(waiting.join().unwrap(), 42);
    }

    #[test]
    fn operation_lock_is_private_and_refuses_symlinks() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let lock = SessionOperationLock::acquire(temp.path()).unwrap();
        drop(lock);
        let path = temp.path().join("operation.lock");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::remove_file(&path).unwrap();
        let target = temp.path().join("target");
        std::fs::write(&target, b"").unwrap();
        symlink(&target, &path).unwrap();
        assert!(SessionOperationLock::acquire(temp.path()).is_err());
    }

    #[test]
    fn stale_and_mismatched_leases_require_explicit_recovery() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = LeaseStore::new(temp.path());
        let lease = RunLease::fixture(ProcessIdentity {
            pid: u32::MAX,
            start_token: "gone".into(),
        });
        store.create(&lease).unwrap();

        let error = store.create(&lease).unwrap_err().to_string();
        assert!(error.contains("stale lease"));
        assert!(store.clear(&crate::model::RunId::new()).is_err());
        store.clear(&lease.run_id).unwrap();
        assert!(store.read().unwrap().is_none());
    }
}
