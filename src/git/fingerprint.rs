use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::git::command::GitCommand;
use crate::model::{ForkFingerprint, UntrackedEntry, UntrackedKind};

#[derive(Clone, Debug)]
pub(crate) struct CapturedForkState {
    pub fingerprint: ForkFingerprint,
    pub staged_patch: Vec<u8>,
    pub unstaged_patch: Vec<u8>,
    pub untracked_manifest: Vec<UntrackedEntry>,
    pub untracked_manifest_json: Vec<u8>,
    pub untracked_blobs: BTreeMap<String, Vec<u8>>,
    pub submodule_manifest_json: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmoduleEntry {
    path: PathBuf,
    expected_object: String,
    initialized: bool,
    parent: Option<PathBuf>,
}

struct CapturedUntracked {
    manifest: Vec<UntrackedEntry>,
    blobs: BTreeMap<String, Vec<u8>>,
}

pub(crate) fn capture(command: &GitCommand, cwd: &Path) -> Result<CapturedForkState> {
    let head = command.text(cwd, ["rev-parse", "HEAD"])?;
    require_object_id(&head)?;
    let branch = command
        .optional_text_exit_one(cwd, ["symbolic-ref", "--quiet", "--short", "HEAD"])?
        .filter(|value| !value.is_empty());
    let index_entries = command.output(cwd, ["ls-files", "--stage", "-z"])?;
    let staged_patch = command.output(
        cwd,
        [
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
        ],
    )?;
    let unstaged_patch = command.output(
        cwd,
        [
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
        ],
    )?;
    let untracked_paths =
        paths(command.output(cwd, ["ls-files", "--others", "--exclude-standard", "-z"])?)?;
    let worktree = canonical_worktree(command, cwd)?;
    let untracked = untracked(&worktree, untracked_paths)?;
    let untracked_manifest = untracked.manifest;
    let untracked_blobs = untracked.blobs;
    let untracked_manifest_json = canonical_json(&untracked_manifest)?;
    let mut visited = HashSet::new();
    let mut submodules = Vec::new();
    collect_submodules(
        command,
        &worktree,
        Path::new(""),
        None,
        &mut visited,
        &mut submodules,
    )?;
    submodules.sort_by(|left, right| left.path.cmp(&right.path));
    let submodule_manifest_json = canonical_json(&submodules)?;
    let fingerprint = ForkFingerprint {
        head,
        branch,
        index_entries_sha256: sha256(&index_entries),
        staged_patch_sha256: sha256(&staged_patch),
        unstaged_patch_sha256: sha256(&unstaged_patch),
        untracked_manifest_sha256: sha256(&untracked_manifest_json),
        submodule_manifest_sha256: sha256(&submodule_manifest_json),
    };
    fingerprint.validate()?;

    Ok(CapturedForkState {
        fingerprint,
        staged_patch,
        unstaged_patch,
        untracked_manifest,
        untracked_manifest_json,
        untracked_blobs,
        submodule_manifest_json,
    })
}

fn untracked(worktree: &Path, paths: Vec<PathBuf>) -> Result<CapturedUntracked> {
    let mut manifest = Vec::new();
    let mut blobs = BTreeMap::new();
    for path in paths {
        let absolute = worktree.join(&path);
        let metadata = std::fs::symlink_metadata(&absolute).map_err(|error| {
            Error::InvalidState(format!(
                "cannot inspect untracked path {}: {error}",
                absolute.display()
            ))
        })?;
        let entry = if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&absolute).map_err(|error| {
                Error::InvalidState(format!(
                    "cannot read untracked symlink {}: {error}",
                    absolute.display()
                ))
            })?;
            if target.to_str().is_none() {
                return Err(Error::InvalidState(format!(
                    "untracked symlink target at {} must be valid UTF-8",
                    path.display()
                )));
            }
            let bytes = target.as_os_str().as_encoded_bytes();
            UntrackedEntry {
                path,
                kind: UntrackedKind::Symlink,
                sha256: sha256(bytes),
                bytes: bytes.len() as u64,
                executable: false,
                symlink_target: Some(target),
                artifact: None,
            }
        } else if metadata.is_file() {
            let bytes = std::fs::read(&absolute).map_err(|error| {
                Error::InvalidState(format!(
                    "cannot read untracked file {}: {error}",
                    absolute.display()
                ))
            })?;
            let digest = sha256(&bytes);
            let artifact = PathBuf::from("untracked/blobs/sha256")
                .join(&digest[..2])
                .join(&digest[2..]);
            blobs.entry(digest.clone()).or_insert(bytes);
            UntrackedEntry {
                path,
                kind: UntrackedKind::Regular,
                sha256: digest,
                bytes: metadata.len(),
                executable: metadata.permissions().mode() & 0o111 != 0,
                symlink_target: None,
                artifact: Some(artifact),
            }
        } else {
            return Err(Error::InvalidState(format!(
                "unsupported untracked path type at {}",
                path.display()
            )));
        };
        entry.validate()?;
        manifest.push(entry);
    }
    manifest.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(CapturedUntracked { manifest, blobs })
}

fn collect_submodules(
    command: &GitCommand,
    repository: &Path,
    prefix: &Path,
    parent: Option<&Path>,
    visited: &mut HashSet<PathBuf>,
    manifest: &mut Vec<SubmoduleEntry>,
) -> Result<()> {
    let canonical_repository = repository.canonicalize().map_err(|error| {
        Error::InvalidState(format!(
            "cannot canonicalize repository {}: {error}",
            repository.display()
        ))
    })?;
    if !visited.insert(canonical_repository.clone()) {
        return Err(Error::InvalidState(
            "recursive submodule identity was visited twice".into(),
        ));
    }
    let listed_root = canonical_worktree(command, repository)?;
    if listed_root != canonical_repository {
        return Err(Error::InvalidState(format!(
            "submodule Git identity does not match {}",
            repository.display()
        )));
    }
    let entries = index_entries(command.output(repository, ["ls-files", "--stage", "-z"])?)?;
    for entry in entries.into_iter().filter(|entry| entry.mode == "160000") {
        if entry.stage != "0" {
            return Err(Error::InvalidState(format!(
                "unmerged submodule index entry at {}",
                entry.path.display()
            )));
        }
        let absolute = repository.join(&entry.path);
        let relative = prefix.join(&entry.path);
        let initialized = match std::fs::symlink_metadata(&absolute) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(Error::InvalidState(format!(
                    "cannot inspect submodule {}: {error}",
                    absolute.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => {
                match std::fs::symlink_metadata(absolute.join(".git")) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => {
                        return Err(Error::InvalidState(format!(
                            "cannot inspect submodule Git metadata {}: {error}",
                            absolute.display()
                        )));
                    }
                    Ok(_) => {
                        let submodule_root = canonical_worktree(command, &absolute)?;
                        let canonical_absolute = absolute.canonicalize().map_err(|error| {
                            Error::InvalidState(format!(
                                "cannot canonicalize submodule {}: {error}",
                                absolute.display()
                            ))
                        })?;
                        if submodule_root != canonical_absolute {
                            return Err(Error::InvalidState(format!(
                                "submodule Git identity does not match {}",
                                relative.display()
                            )));
                        }
                        let head = command.text(&absolute, ["rev-parse", "HEAD"])?;
                        if head != entry.object {
                            return Err(Error::InvalidState(format!(
                                "submodule HEAD differs from gitlink at {}",
                                relative.display()
                            )));
                        }
                        let status = command.output(
                            &absolute,
                            [
                                "status",
                                "--porcelain=v2",
                                "-z",
                                "--untracked-files=all",
                                "--ignore-submodules=none",
                            ],
                        )?;
                        if !status.is_empty() {
                            return Err(Error::InvalidState(format!(
                                "submodule is dirty at {}",
                                relative.display()
                            )));
                        }
                        true
                    }
                }
            }
            Ok(_) => {
                return Err(Error::InvalidState(format!(
                    "unsupported submodule file type at {}",
                    relative.display()
                )));
            }
        };
        manifest.push(SubmoduleEntry {
            path: relative.clone(),
            expected_object: entry.object,
            initialized,
            parent: parent.map(Path::to_path_buf),
        });
        if initialized {
            collect_submodules(
                command,
                &absolute,
                &relative,
                Some(&relative),
                visited,
                manifest,
            )?;
        }
    }
    Ok(())
}

fn canonical_worktree(command: &GitCommand, cwd: &Path) -> Result<PathBuf> {
    let value = command.text(cwd, ["rev-parse", "--show-toplevel"])?;
    let path = PathBuf::from(value);
    if path.to_str().is_none() {
        return Err(Error::InvalidState(
            "Git worktree path must be valid UTF-8".into(),
        ));
    }
    path.canonicalize().map_err(|error| {
        Error::InvalidState(format!(
            "cannot canonicalize Git worktree {}: {error}",
            path.display()
        ))
    })
}

#[derive(Debug)]
struct IndexEntry {
    mode: String,
    object: String,
    stage: String,
    path: PathBuf,
}

fn index_entries(bytes: Vec<u8>) -> Result<Vec<IndexEntry>> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(Error::InvalidState(
            "Git index list was not NUL-terminated".into(),
        ));
    }
    bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| Error::InvalidState("malformed Git index entry".into()))?;
            let fields = std::str::from_utf8(&record[..tab])
                .map_err(|_| Error::InvalidState("non-ASCII Git index header".into()))?
                .split_ascii_whitespace()
                .collect::<Vec<_>>();
            if fields.len() != 3 {
                return Err(Error::InvalidState("malformed Git index header".into()));
            }
            require_object_id(fields[1])?;
            let path = PathBuf::from(OsString::from_vec(record[tab + 1..].to_vec()));
            require_relative_utf8(&path)?;
            Ok(IndexEntry {
                mode: fields[0].into(),
                object: fields[1].into(),
                stage: fields[2].into(),
                path,
            })
        })
        .collect()
}

fn paths(bytes: Vec<u8>) -> Result<Vec<PathBuf>> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(Error::InvalidState(
            "Git path list was not NUL-terminated".into(),
        ));
    }
    let mut paths = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| PathBuf::from(OsString::from_vec(part.to_vec())))
        .collect::<Vec<_>>();
    for path in &paths {
        require_relative_utf8(path)?;
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn require_relative_utf8(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.to_str().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidState(format!(
            "Git emitted invalid repository-relative path {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_object_id(value: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::InvalidState(format!(
            "Git emitted malformed object ID {value:?}"
        )));
    }
    Ok(())
}

fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        Error::InvalidState(format!("cannot encode fork artifact JSON: {error}"))
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
