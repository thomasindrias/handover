use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::git::command::GitCommand;
use crate::model::{DirtyPath, GitSnapshot, WorktreeIdentity};

pub fn snapshot(command: &GitCommand, cwd: &Path) -> Result<GitSnapshot> {
    let worktree = canonical(command.text(cwd, ["rev-parse", "--show-toplevel"])?)?;
    let git_dir =
        canonical(command.text(cwd, ["rev-parse", "--path-format=absolute", "--git-dir"])?)?;
    let common_git_dir = canonical(command.text(
        cwd,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)?;
    let canonical_cwd = cwd.canonicalize().map_err(|error| {
        Error::Command(format!("cannot canonicalize {}: {error}", cwd.display()))
    })?;
    require_utf8([&worktree, &git_dir, &common_git_dir, &canonical_cwd])?;
    let cwd_relative = canonical_cwd
        .strip_prefix(&worktree)
        .map_err(|_| Error::Command("cwd is outside discovered worktree".into()))?
        .to_path_buf();
    let key = WorktreeIdentity::derive_key(&common_git_dir, &git_dir);
    let identity = WorktreeIdentity {
        common_git_dir,
        git_dir,
        worktree: worktree.clone(),
        cwd_relative,
        key,
    };
    identity.validate()?;

    let head = command.text(&worktree, ["rev-parse", "HEAD"])?;
    require_object_id(&head)?;
    let branch = command
        .optional_text_exit_one(&worktree, ["symbolic-ref", "--quiet", "--short", "HEAD"])?
        .filter(|value| !value.is_empty());
    let staged = paths(command.output(
        &worktree,
        [
            "diff",
            "--cached",
            "--name-only",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "-z",
        ],
    )?)?
    .into_iter()
    .map(|path| staged_path(command, &worktree, path))
    .collect::<Result<Vec<_>>>()?;
    let unstaged = paths(command.output(
        &worktree,
        [
            "diff",
            "--name-only",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "-z",
        ],
    )?)?
    .into_iter()
    .map(|path| unstaged_path(command, &worktree, &worktree, path))
    .collect::<Result<Vec<_>>>()?;
    let untracked = paths(command.output(
        &worktree,
        ["ls-files", "--others", "--exclude-standard", "-z"],
    )?)?
    .into_iter()
    .map(|path| worktree_path(&worktree, path))
    .collect::<Result<Vec<_>>>()?;
    let dirty_submodules = dirty_submodules(command, &worktree)?;

    Ok(GitSnapshot {
        identity,
        branch,
        head,
        staged,
        unstaged,
        untracked,
        dirty_submodules,
    })
}

fn canonical(value: String) -> Result<PathBuf> {
    PathBuf::from(value)
        .canonicalize()
        .map_err(|error| Error::Command(format!("cannot canonicalize Git path: {error}")))
}

fn require_utf8<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> Result<()> {
    if paths.into_iter().any(|path| path.to_str().is_none()) {
        return Err(Error::InvalidState(
            "Handover V1 requires Git paths that are valid UTF-8; no path was recorded lossily"
                .into(),
        ));
    }
    Ok(())
}

fn require_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidState(format!(
            "Git emitted a non-relative repository path {}",
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
    require_utf8(paths.iter())?;
    for path in &paths {
        require_relative(path)?;
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[derive(Debug)]
struct IndexEntry {
    mode: String,
    object: String,
    stage: String,
    path: PathBuf,
}

fn index_entries(bytes: &[u8]) -> Result<Vec<IndexEntry>> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(Error::InvalidState(
            "Git index list was not NUL-terminated".into(),
        ));
    }
    let mut entries = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| Error::InvalidState("malformed Git index entry".into()))?;
        let header = std::str::from_utf8(&record[..tab])
            .map_err(|_| Error::InvalidState("non-ASCII Git index header".into()))?;
        let fields = header.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(Error::InvalidState("malformed Git index header".into()));
        }
        if !matches!(fields[0], "100644" | "100755" | "120000" | "160000") {
            return Err(Error::InvalidState(format!(
                "unsupported Git index mode {}",
                fields[0]
            )));
        }
        require_object_id(fields[1])?;
        if !matches!(fields[2], "0" | "1" | "2" | "3") {
            return Err(Error::InvalidState(format!(
                "invalid Git index stage {}",
                fields[2]
            )));
        }
        let path = PathBuf::from(OsString::from_vec(record[tab + 1..].to_vec()));
        require_utf8([&path])?;
        require_relative(&path)?;
        entries.push(IndexEntry {
            mode: fields[0].to_owned(),
            object: fields[1].to_owned(),
            stage: fields[2].to_owned(),
            path,
        });
    }
    Ok(entries)
}

fn entries_for_path(command: &GitCommand, cwd: &Path, path: &Path) -> Result<Vec<IndexEntry>> {
    let bytes = command.output(
        cwd,
        [
            OsString::from("ls-files"),
            OsString::from("--stage"),
            OsString::from("-z"),
            OsString::from("--"),
            path.as_os_str().to_os_string(),
        ],
    )?;
    let entries = index_entries(&bytes)?;
    if entries.iter().any(|entry| entry.path != path) {
        return Err(Error::InvalidState(format!(
            "Git index lookup returned an unexpected path for {}",
            path.display()
        )));
    }
    Ok(entries)
}

fn staged_path(command: &GitCommand, cwd: &Path, path: PathBuf) -> Result<DirtyPath> {
    let entries = entries_for_path(command, cwd, &path)?;
    if entries.is_empty() {
        return Ok(missing_path(path));
    }
    if entries.len() != 1 || entries[0].stage != "0" {
        return Err(Error::InvalidState(format!(
            "unmerged index entry at {}",
            path.display()
        )));
    }
    let entry = &entries[0];
    let content = if entry.mode == "160000" {
        entry.object.as_bytes().to_vec()
    } else {
        command.output(
            cwd,
            [
                OsString::from("cat-file"),
                OsString::from("blob"),
                OsString::from(&entry.object),
            ],
        )?
    };
    let symlink_target =
        (entry.mode == "120000").then(|| PathBuf::from(OsString::from_vec(content.clone())));
    if let Some(target) = symlink_target.as_ref() {
        require_utf8([target])?;
    }
    Ok(DirtyPath {
        path,
        sha256: Some(hex::encode(Sha256::digest(&content))),
        executable: entry.mode == "100755",
        symlink_target,
    })
}

fn unstaged_path(
    command: &GitCommand,
    cwd: &Path,
    worktree: &Path,
    path: PathBuf,
) -> Result<DirtyPath> {
    let entries = entries_for_path(command, cwd, &path)?;
    if entries.len() > 1 || entries.first().is_some_and(|entry| entry.stage != "0") {
        return Err(Error::InvalidState(format!(
            "unmerged index entry at {}",
            path.display()
        )));
    }
    if entries.first().is_some_and(|entry| entry.mode == "160000") {
        return submodule_path(command, worktree, path);
    }
    worktree_path(worktree, path)
}

fn submodule_path(command: &GitCommand, worktree: &Path, path: PathBuf) -> Result<DirtyPath> {
    let absolute = worktree.join(&path);
    match std::fs::symlink_metadata(&absolute) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(missing_path(path)),
        Err(error) => Err(Error::Command(format!(
            "cannot inspect {}: {error}",
            absolute.display()
        ))),
        Ok(metadata) if metadata.is_dir() => {
            if std::fs::symlink_metadata(absolute.join(".git")).is_err() {
                return Ok(missing_path(path));
            }
            let head = command.text(&absolute, ["rev-parse", "HEAD"])?;
            require_object_id(&head)?;
            Ok(DirtyPath {
                path,
                sha256: Some(hex::encode(Sha256::digest(head.as_bytes()))),
                executable: false,
                symlink_target: None,
            })
        }
        Ok(metadata) => Err(Error::Command(format!(
            "unsupported submodule file type at {} (mode {:o})",
            absolute.display(),
            metadata.mode()
        ))),
    }
}

fn worktree_path(worktree: &Path, path: PathBuf) -> Result<DirtyPath> {
    require_relative(&path)?;
    let absolute = worktree.join(&path);
    match std::fs::symlink_metadata(&absolute) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = std::fs::read_link(&absolute).map_err(|error| {
                Error::Command(format!("cannot read {}: {error}", absolute.display()))
            })?;
            require_utf8([&target])?;
            let digest = Sha256::digest(target.as_os_str().as_encoded_bytes());
            Ok(DirtyPath {
                path,
                sha256: Some(hex::encode(digest)),
                executable: false,
                symlink_target: Some(target),
            })
        }
        Ok(metadata) if metadata.is_file() => {
            let bytes = std::fs::read(&absolute).map_err(|error| {
                Error::Command(format!("cannot read {}: {error}", absolute.display()))
            })?;
            Ok(DirtyPath {
                path,
                sha256: Some(hex::encode(Sha256::digest(bytes))),
                executable: metadata.permissions().mode() & 0o111 != 0,
                symlink_target: None,
            })
        }
        Ok(metadata) => Err(Error::Command(format!(
            "unsupported dirty file type at {} (mode {:o})",
            absolute.display(),
            metadata.mode()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(missing_path(path)),
        Err(error) => Err(Error::Command(format!(
            "cannot inspect {}: {error}",
            absolute.display()
        ))),
    }
}

fn missing_path(path: PathBuf) -> DirtyPath {
    DirtyPath {
        path,
        sha256: None,
        executable: false,
        symlink_target: None,
    }
}

fn dirty_submodules(command: &GitCommand, cwd: &Path) -> Result<Vec<PathBuf>> {
    let index = command.output(cwd, ["ls-files", "--stage", "-z"])?;
    let mut dirty = Vec::new();
    for entry in index_entries(&index)? {
        if entry.mode != "160000" || entry.stage != "0" {
            continue;
        }
        let status = command.output(
            cwd,
            [
                OsString::from("status"),
                OsString::from("--porcelain=v2"),
                OsString::from("-z"),
                OsString::from("--untracked-files=all"),
                OsString::from("--ignore-submodules=none"),
                OsString::from("--"),
                entry.path.as_os_str().to_os_string(),
            ],
        )?;
        if submodule_status_is_dirty(&status)? {
            dirty.push(entry.path);
        }
    }
    dirty.sort();
    dirty.dedup();
    Ok(dirty)
}

fn submodule_status_is_dirty(bytes: &[u8]) -> Result<bool> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(Error::InvalidState(
            "Git status was not NUL-terminated".into(),
        ));
    }
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        if !record.starts_with(b"1 ") {
            continue;
        }
        let fields = record.splitn(9, |byte| *byte == b' ').collect::<Vec<_>>();
        if fields.len() != 9 {
            return Err(Error::InvalidState(
                "malformed Git porcelain v2 record".into(),
            ));
        }
        let xy = fields[1];
        let sub = fields[2];
        if sub.len() == 4
            && sub[0] == b'S'
            && (xy.get(1).is_some_and(|status| *status != b'.')
                || sub[1..].iter().any(|status| *status != b'.'))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
