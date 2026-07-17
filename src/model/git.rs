use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorktreeIdentity {
    pub common_git_dir: PathBuf,
    pub git_dir: PathBuf,
    pub worktree: PathBuf,
    pub cwd_relative: PathBuf,
    pub key: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirtyPath {
    pub path: PathBuf,
    pub sha256: Option<String>,
    pub executable: bool,
    pub symlink_target: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitSnapshot {
    pub identity: WorktreeIdentity,
    pub branch: Option<String>,
    pub head: String,
    pub staged: Vec<DirtyPath>,
    pub unstaged: Vec<DirtyPath>,
    pub untracked: Vec<DirtyPath>,
    pub dirty_submodules: Vec<PathBuf>,
}
