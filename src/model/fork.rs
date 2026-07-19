use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::model::{OperationId, SessionId, WorktreeIdentity};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkFingerprint {
    pub head: String,
    pub branch: Option<String>,
    pub index_entries_sha256: String,
    pub staged_patch_sha256: String,
    pub unstaged_patch_sha256: String,
    pub untracked_manifest_sha256: String,
    pub submodule_manifest_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UntrackedKind {
    Regular,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UntrackedEntry {
    pub path: PathBuf,
    pub kind: UntrackedKind,
    pub sha256: String,
    pub bytes: u64,
    pub executable: bool,
    pub symlink_target: Option<PathBuf>,
    pub artifact: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPhase {
    Prepared,
    ArtifactsCaptured,
    WorktreeCreated,
    StagedApplied,
    UnstagedApplied,
    UntrackedCopied,
    Verified,
    ChildStaged,
    LineageCommitted,
    ChildBound,
    RunLeased,
    Complete,
    RolledBack,
    NeedsManualRecovery,
}

impl ForkPhase {
    pub(crate) fn ordinal(self) -> Option<u8> {
        match self {
            Self::Prepared => Some(0),
            Self::ArtifactsCaptured => Some(1),
            Self::WorktreeCreated => Some(2),
            Self::StagedApplied => Some(3),
            Self::UnstagedApplied => Some(4),
            Self::UntrackedCopied => Some(5),
            Self::Verified => Some(6),
            Self::ChildStaged => Some(7),
            Self::LineageCommitted => Some(8),
            Self::ChildBound => Some(9),
            Self::RunLeased => Some(10),
            Self::Complete => Some(11),
            Self::RolledBack | Self::NeedsManualRecovery => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkOperation {
    pub schema_version: u32,
    pub id: OperationId,
    pub phase: ForkPhase,
    pub source_session_id: SessionId,
    pub source_worktree: WorktreeIdentity,
    pub source_checkpoint_sequence: Option<u64>,
    pub source_fingerprint: Option<ForkFingerprint>,
    pub target_branch: String,
    pub target_worktree: PathBuf,
    pub target_head: String,
    pub child_session_id: Option<SessionId>,
    pub target_fingerprint: Option<ForkFingerprint>,
    pub target_cleanup_inventory_sha256: Option<String>,
    pub branch_created: bool,
    pub target_created: bool,
    pub error: Option<String>,
    pub updated_at: String,
}

impl ForkOperation {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(Error::InvalidState(format!(
                "unsupported fork operation schema {}",
                self.schema_version
            )));
        }
        self.source_worktree.validate()?;
        require_absolute_utf8(&self.target_worktree, "target worktree")?;
        if self.target_branch.is_empty() || self.target_branch.as_bytes().contains(&0) {
            return Err(Error::InvalidState(
                "fork operation target branch is invalid".into(),
            ));
        }
        require_object_id(&self.target_head, "target HEAD")?;
        OffsetDateTime::parse(&self.updated_at, &Rfc3339).map_err(|error| {
            Error::InvalidState(format!("fork operation timestamp is invalid: {error}"))
        })?;
        if let Some(fingerprint) = self.source_fingerprint.as_ref() {
            fingerprint.validate()?;
        }
        if let Some(fingerprint) = self.target_fingerprint.as_ref() {
            fingerprint.validate()?;
        }
        if let Some(hash) = self.target_cleanup_inventory_sha256.as_deref() {
            require_sha256(hash, "target cleanup inventory")?;
        }

        let normal = self.phase.ordinal();
        if normal.is_some_and(|phase| phase == 0) && self.source_fingerprint.is_some() {
            return Err(Error::InvalidState(
                "prepared fork operation cannot have a source fingerprint".into(),
            ));
        }
        if normal.is_some_and(|phase| phase >= 1) && self.source_fingerprint.is_none() {
            return Err(Error::InvalidState(
                "captured fork operation is missing its source fingerprint".into(),
            ));
        }
        if normal.is_some_and(|phase| phase < 2) && (self.target_created || self.branch_created) {
            return Err(Error::InvalidState(
                "fork target flags precede worktree creation".into(),
            ));
        }
        if normal.is_some_and(|phase| phase >= 2) && (!self.target_created || !self.branch_created)
        {
            return Err(Error::InvalidState(
                "fork worktree phase is missing target creation flags".into(),
            ));
        }
        if self.target_created
            && (self.target_fingerprint.is_none() || self.target_cleanup_inventory_sha256.is_none())
        {
            return Err(Error::InvalidState(
                "created fork target is missing cleanup fingerprints".into(),
            ));
        }
        if normal.is_some_and(|phase| phase >= 7) {
            if self.child_session_id.is_none() || self.source_checkpoint_sequence.is_none() {
                return Err(Error::InvalidState(
                    "staged fork child is missing lineage identifiers".into(),
                ));
            }
        } else if normal.is_some()
            && (self.child_session_id.is_some() || self.source_checkpoint_sequence.is_some())
        {
            return Err(Error::InvalidState(
                "fork lineage identifiers precede child staging".into(),
            ));
        }
        match self.phase {
            ForkPhase::RolledBack | ForkPhase::NeedsManualRecovery if self.error.is_none() => {
                return Err(Error::InvalidState(
                    "failed fork operation is missing its error".into(),
                ));
            }
            phase if phase.ordinal().is_some() && self.error.is_some() => {
                return Err(Error::InvalidState(
                    "nonterminal fork operation contains an error".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

impl ForkFingerprint {
    pub fn validate(&self) -> Result<()> {
        require_object_id(&self.head, "fingerprint HEAD")?;
        if self
            .branch
            .as_ref()
            .is_some_and(|branch| branch.is_empty() || branch.as_bytes().contains(&0))
        {
            return Err(Error::InvalidState(
                "fingerprint branch is malformed".into(),
            ));
        }
        for (label, hash) in [
            ("index entries", &self.index_entries_sha256),
            ("staged patch", &self.staged_patch_sha256),
            ("unstaged patch", &self.unstaged_patch_sha256),
            ("untracked manifest", &self.untracked_manifest_sha256),
            ("submodule manifest", &self.submodule_manifest_sha256),
        ] {
            require_sha256(hash, label)?;
        }
        Ok(())
    }
}

impl UntrackedEntry {
    pub fn validate(&self) -> Result<()> {
        require_relative_utf8(&self.path, "untracked path")?;
        require_sha256(&self.sha256, "untracked content")?;
        if let Some(target) = self.symlink_target.as_ref() {
            if target.as_os_str().is_empty() || target.to_str().is_none() {
                return Err(Error::InvalidState(
                    "untracked symlink target must be valid UTF-8".into(),
                ));
            }
        }
        if let Some(artifact) = self.artifact.as_ref() {
            require_relative_utf8(artifact, "untracked artifact")?;
        }
        match self.kind {
            UntrackedKind::Regular => {
                let expected = PathBuf::from("untracked/blobs/sha256")
                    .join(&self.sha256[..2])
                    .join(&self.sha256[2..]);
                if self.symlink_target.is_some() || self.artifact.as_ref() != Some(&expected) {
                    return Err(Error::InvalidState(
                        "regular untracked entry has inconsistent artifact metadata".into(),
                    ));
                }
                Ok(())
            }
            UntrackedKind::Symlink
                if self.symlink_target.is_none() || self.artifact.is_some() || self.executable =>
            {
                Err(Error::InvalidState(
                    "symlink untracked entry has inconsistent metadata".into(),
                ))
            }
            UntrackedKind::Symlink
                if self.symlink_target.as_ref().is_some_and(|target| {
                    target.as_os_str().as_encoded_bytes().len() as u64 != self.bytes
                }) =>
            {
                Err(Error::InvalidState(
                    "symlink untracked entry byte length is inconsistent".into(),
                ))
            }
            _ => Ok(()),
        }
    }
}

fn require_absolute_utf8(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute()
        || path.to_str().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::InvalidState(format!(
            "fork operation {label} must be a normalized absolute valid UTF-8 path"
        )));
    }
    Ok(())
}

fn require_relative_utf8(path: &Path, label: &str) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.to_str().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidState(format!(
            "{label} must be a normalized relative valid UTF-8 path"
        )));
    }
    Ok(())
}

fn require_object_id(value: &str, label: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::InvalidState(format!("{label} is malformed")));
    }
    Ok(())
}

fn require_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(Error::InvalidState(format!("{label} SHA-256 is malformed")));
    }
    Ok(())
}
