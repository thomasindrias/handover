use std::path::PathBuf;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeIdentity {
    pub common_git_dir: PathBuf,
    pub git_dir: PathBuf,
    pub worktree: PathBuf,
    pub cwd_relative: PathBuf,
    pub key: String,
}

impl WorktreeIdentity {
    pub fn derive_key(common_git_dir: &Path, git_dir: &Path) -> String {
        let mut digest = Sha256::new();
        digest.update(common_git_dir.as_os_str().as_encoded_bytes());
        digest.update([0]);
        digest.update(git_dir.as_os_str().as_encoded_bytes());
        hex::encode(digest.finalize())
    }

    pub fn validate(&self) -> Result<()> {
        for path in [&self.common_git_dir, &self.git_dir, &self.worktree] {
            if !is_normal_absolute(path) || path.to_str().is_none() {
                return Err(Error::InvalidState(
                    "worktree identity paths must be normalized absolute valid UTF-8".into(),
                ));
            }
        }
        if self.cwd_relative.is_absolute()
            || self
                .cwd_relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(Error::InvalidState(
                "saved cwd must be a normalized worktree-relative path".into(),
            ));
        }
        if self.cwd_relative.to_str().is_none() {
            return Err(Error::InvalidState("saved cwd must be valid UTF-8".into()));
        }
        let expected = Self::derive_key(&self.common_git_dir, &self.git_dir);
        if self.key != expected {
            return Err(Error::InvalidState(
                "worktree identity key does not match its Git directories".into(),
            ));
        }
        Ok(())
    }

    pub fn same_worktree_as(&self, other: &Self) -> bool {
        self.key == other.key
            && self.common_git_dir == other.common_git_dir
            && self.git_dir == other.git_dir
            && self.worktree == other.worktree
    }
}

fn is_normal_absolute(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::RootDir))
        && components.all(|component| matches!(component, Component::Normal(_)))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirtyPath {
    pub path: PathBuf,
    pub sha256: Option<String>,
    pub executable: bool,
    pub symlink_target: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSnapshot {
    pub identity: WorktreeIdentity,
    pub branch: Option<String>,
    pub head: String,
    pub staged: Vec<DirtyPath>,
    pub unstaged: Vec<DirtyPath>,
    pub untracked: Vec<DirtyPath>,
    pub dirty_submodules: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::WorktreeIdentity;

    fn identity() -> WorktreeIdentity {
        let common_git_dir = PathBuf::from("/repo/.git");
        let git_dir = PathBuf::from("/repo/.git/worktrees/oauth");
        WorktreeIdentity {
            key: WorktreeIdentity::derive_key(&common_git_dir, &git_dir),
            common_git_dir,
            git_dir,
            worktree: PathBuf::from("/work/oauth"),
            cwd_relative: PathBuf::from("apps/web"),
        }
    }

    #[test]
    fn identity_rejects_lexical_escape_and_key_mismatch() {
        let mut invalid = identity();
        invalid.cwd_relative = PathBuf::from("../outside");
        assert!(invalid.validate().is_err());

        let mut invalid = identity();
        invalid.worktree = PathBuf::from("/work/../other");
        assert!(invalid.validate().is_err());

        let mut invalid = identity();
        invalid.key = "not-the-derived-key".into();
        assert!(invalid.validate().is_err());
    }
}
