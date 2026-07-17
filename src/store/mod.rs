pub mod atomic;
pub mod journal;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result, io};

#[derive(Clone, Debug, Default)]
pub struct Environment {
    values: HashMap<OsString, OsString>,
}

impl Environment {
    pub fn capture() -> Self {
        Self {
            values: std::env::vars_os().collect(),
        }
    }

    pub fn from_pairs(values: HashMap<&str, OsString>) -> Self {
        Self {
            values: values
                .into_iter()
                .map(|(key, value)| (OsString::from(key), value))
                .collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.values.get(OsStr::new(key)).map(OsString::as_os_str)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateLayout {
    root: PathBuf,
}

impl StateLayout {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_environment(env: &Environment) -> Result<Self> {
        let cwd = std::env::current_dir().map_err(|source| io(".", source))?;
        Self::from_environment_at(env, &cwd)
    }

    pub fn from_environment_at(env: &Environment, cwd: &Path) -> Result<Self> {
        if let Some(root) = env.get("SESH_HOME") {
            if root.is_empty() {
                return Err(Error::InvalidState("SESH_HOME must not be empty".into()));
            }
            return Ok(Self::new(resolve_from(cwd, PathBuf::from(root))));
        }
        if let Some(root) = env.get("XDG_STATE_HOME").filter(|root| !root.is_empty()) {
            return Ok(Self::new(
                resolve_from(cwd, PathBuf::from(root)).join("sesh"),
            ));
        }
        let home = env
            .get("HOME")
            .filter(|home| !home.is_empty())
            .ok_or(Error::StateHomeUnavailable)?;
        Ok(Self::new(
            resolve_from(cwd, PathBuf::from(home)).join(".local/state/sesh"),
        ))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn worktree_refs(&self) -> PathBuf {
        self.root.join("refs/worktrees")
    }

    pub fn ensure(&self) -> Result<()> {
        let paths = [
            self.root.clone(),
            self.sessions(),
            self.root.join("refs"),
            self.worktree_refs(),
        ];
        for path in &paths {
            ensure_private_dir(path)?;
        }

        let format = self.root.join("FORMAT");
        if format.exists() {
            let bytes = atomic::read_private(&format)?;
            if bytes != b"sesh-state 1\n" {
                return Err(Error::InvalidState(
                    "unsupported Sesh state format; expected 1".into(),
                ));
            }
        } else {
            atomic::create_private(&format, b"sesh-state 1\n")?;
        }
        Ok(())
    }

    pub fn canonicalized(&self) -> Result<Self> {
        let root = self
            .root
            .canonicalize()
            .map_err(|source| io(&self.root, source))?;
        Ok(Self::new(root))
    }
}

fn resolve_from(cwd: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    let existed = match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => return Err(io(path, source)),
    };
    if !existed {
        std::fs::create_dir_all(path).map_err(|source| io(path, source))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|source| io(path, source))?;
    }

    let metadata = std::fs::symlink_metadata(path).map_err(|source| io(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::InvalidState(format!(
            "private state path {} is not a real directory",
            path.display(),
        )));
    }

    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(Error::InvalidState(format!(
            "private state path {} has unexpected owner {}",
            path.display(),
            metadata.uid(),
        )));
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(Error::InvalidState(format!(
            "private state directory {} must have mode 0700",
            path.display(),
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::{Environment, StateLayout};

    #[test]
    fn sesh_home_wins_over_xdg_and_home() {
        let env = Environment::from_pairs(HashMap::from([
            ("SESH_HOME", OsString::from("/state/explicit")),
            ("XDG_STATE_HOME", OsString::from("/state/xdg")),
            ("HOME", OsString::from("/home/dev")),
        ]));

        assert_eq!(
            StateLayout::from_environment(&env).unwrap().root(),
            std::path::Path::new("/state/explicit")
        );
    }

    #[test]
    fn xdg_then_home_are_the_fallbacks() {
        let xdg = Environment::from_pairs(HashMap::from([
            ("XDG_STATE_HOME", OsString::from("/state/xdg")),
            ("HOME", OsString::from("/home/dev")),
        ]));
        let home = Environment::from_pairs(HashMap::from([("HOME", OsString::from("/home/dev"))]));

        assert_eq!(
            StateLayout::from_environment(&xdg).unwrap().root(),
            std::path::Path::new("/state/xdg/sesh")
        );
        assert_eq!(
            StateLayout::from_environment(&home).unwrap().root(),
            std::path::Path::new("/home/dev/.local/state/sesh")
        );
    }

    #[test]
    fn empty_environment_roots_are_never_treated_as_the_launch_directory() {
        let explicit = Environment::from_pairs(HashMap::from([
            ("SESH_HOME", OsString::new()),
            ("HOME", OsString::from("/home/dev")),
        ]));
        let xdg = Environment::from_pairs(HashMap::from([
            ("XDG_STATE_HOME", OsString::new()),
            ("HOME", OsString::from("/home/dev")),
        ]));

        assert!(StateLayout::from_environment(&explicit).is_err());
        assert_eq!(
            StateLayout::from_environment(&xdg).unwrap().root(),
            std::path::Path::new("/home/dev/.local/state/sesh")
        );
    }

    #[test]
    fn ensure_creates_a_user_only_root() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));

        layout.ensure().unwrap();

        let mode = std::fs::metadata(layout.root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        for directory in [
            layout.sessions(),
            layout.root().join("refs"),
            layout.worktree_refs(),
        ] {
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            std::fs::metadata(layout.root().join("FORMAT"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn ensure_refuses_a_symlinked_state_root() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let root = temp.path().join("state");
        symlink(&target, &root).unwrap();

        assert!(StateLayout::new(root).ensure().is_err());
    }

    #[test]
    fn ensure_refuses_an_existing_group_readable_state_root() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o750)).unwrap();

        assert!(StateLayout::new(root).ensure().is_err());
    }

    #[test]
    fn relative_sesh_home_is_resolved_once_against_the_launch_cwd() {
        let temp = TempDir::new().unwrap();
        let cwd = temp.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let env =
            Environment::from_pairs(HashMap::from([("SESH_HOME", OsString::from("../state"))]));

        let layout = StateLayout::from_environment_at(&env, &cwd).unwrap();
        layout.ensure().unwrap();
        let canonical = layout.canonicalized().unwrap();

        assert_eq!(
            canonical.root(),
            temp.path().join("state").canonicalize().unwrap()
        );
    }

    #[test]
    fn ensure_refuses_an_unknown_state_format() {
        let temp = TempDir::new().unwrap();
        let layout = StateLayout::new(temp.path().join("state"));
        layout.ensure().unwrap();
        std::fs::write(layout.root().join("FORMAT"), b"sesh-state 999\n").unwrap();

        assert!(layout.ensure().is_err());
    }
}
