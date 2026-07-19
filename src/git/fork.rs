use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::command::GitCommand;
use crate::git::observe;
use crate::model::{GitSnapshot, Provider};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkRequest {
    pub provider: Provider,
    pub branch: Option<String>,
    pub worktree: Option<PathBuf>,
    pub provider_args: Vec<OsString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkTarget {
    pub branch: String,
    pub worktree: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkPreflight {
    pub source: GitSnapshot,
    pub target: ForkTarget,
    pub source_head: String,
}

pub fn default_target(source_worktree: &Path, operation_id: &str) -> Result<ForkTarget> {
    let short_id = operation_id
        .get(..8)
        .filter(|value| value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| Error::InvalidState("fork operation ID is malformed".into()))?
        .to_ascii_lowercase();
    let basename = source_worktree
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::InvalidState("source worktree has no repository name".into()))?;
    let basename_utf8 = basename
        .to_str()
        .ok_or_else(|| Error::InvalidState("source worktree name must be valid UTF-8".into()))?;
    let branch_component = sanitize_branch_component(basename_utf8);
    let parent = source_worktree
        .parent()
        .ok_or_else(|| Error::InvalidState("source worktree has no parent directory".into()))?;
    let worktree = parent.join(format!("{basename_utf8}-sesh-{short_id}"));

    Ok(ForkTarget {
        branch: format!("sesh/{branch_component}-{short_id}"),
        worktree,
    })
}

pub(crate) fn preflight(
    command: &GitCommand,
    source_cwd: &Path,
    caller_cwd: &Path,
    request: &ForkRequest,
    operation_id: &str,
) -> Result<ForkPreflight> {
    let source = observe::snapshot(command, source_cwd)?;
    let defaults = default_target(&source.identity.worktree, operation_id)?;
    let target = resolve_target(caller_cwd, request, defaults)?;

    require_utf8_path(&target.worktree, "target worktree")?;
    refuse_existing_target(&target.worktree)?;
    validate_branch(command, &source.identity.worktree, &target.branch)?;
    refuse_nested_target(command, &source.identity.worktree, &target.worktree)?;
    refuse_sparse_checkout(command, &source.identity.worktree)?;
    refuse_unmerged(command, &source.identity.worktree)?;
    refuse_intent_to_add(command, &source.identity.worktree)?;
    refuse_staged_gitlink(command, &source.identity.worktree)?;
    if !source.dirty_submodules.is_empty() {
        return Err(Error::InvalidState(
            "fork refuses dirty submodules in V1".into(),
        ));
    }
    validate_untracked_types(&source)?;
    refuse_unignored_special_nodes(command, &source.identity.worktree)?;

    Ok(ForkPreflight {
        source_head: source.head.clone(),
        source,
        target,
    })
}

fn resolve_target(
    caller_cwd: &Path,
    request: &ForkRequest,
    defaults: ForkTarget,
) -> Result<ForkTarget> {
    let branch = request.branch.clone().unwrap_or(defaults.branch);
    let requested = request.worktree.clone().unwrap_or(defaults.worktree);
    let requested = if requested.is_absolute() {
        requested
    } else {
        caller_cwd.join(requested)
    };
    if requested.file_name().is_none() {
        return Err(Error::InvalidState(
            "target worktree must name a path beneath an existing parent".into(),
        ));
    }
    let parent = requested
        .parent()
        .ok_or_else(|| Error::InvalidState("target worktree has no parent directory".into()))?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        Error::InvalidState(format!(
            "cannot canonicalize target parent {}: {error}",
            parent.display()
        ))
    })?;
    let file_name = requested.file_name().expect("checked above");
    require_utf8_path(&canonical_parent, "target parent")?;
    if file_name.to_str().is_none() {
        return Err(Error::InvalidState(
            "target worktree name must be valid UTF-8".into(),
        ));
    }
    Ok(ForkTarget {
        branch,
        worktree: canonical_parent.join(file_name),
    })
}

fn validate_branch(command: &GitCommand, source: &Path, branch: &str) -> Result<()> {
    command.output(source, ["check-ref-format", "--branch", branch])?;
    let reference = format!("refs/heads/{branch}");
    if command
        .optional_text_exit_one(source, ["show-ref", "--verify", "--quiet", &reference])?
        .is_some()
    {
        return Err(Error::InvalidState(format!(
            "target branch {branch:?} already exists"
        )));
    }
    Ok(())
}

fn refuse_existing_target(target: &Path) -> Result<()> {
    match std::fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::InvalidState(format!(
            "cannot inspect target worktree {}: {error}",
            target.display()
        ))),
        Ok(_) => Err(Error::InvalidState(format!(
            "target worktree {} already exists",
            target.display()
        ))),
    }
}

fn refuse_sparse_checkout(command: &GitCommand, source: &Path) -> Result<()> {
    for key in ["core.sparseCheckout", "core.sparseCheckoutCone"] {
        if command
            .optional_text_exit_one(source, ["config", "--bool", "--get", key])?
            .is_some_and(|value| value == "true")
        {
            return Err(Error::InvalidState(
                "fork refuses sparse checkout in V1".into(),
            ));
        }
    }
    Ok(())
}

fn refuse_unmerged(command: &GitCommand, source: &Path) -> Result<()> {
    if !command
        .output(source, ["ls-files", "--unmerged", "-z"])?
        .is_empty()
    {
        return Err(Error::InvalidState(
            "fork refuses unmerged index entries".into(),
        ));
    }
    Ok(())
}

fn refuse_intent_to_add(command: &GitCommand, source: &Path) -> Result<()> {
    let status = command.output(
        source,
        ["status", "--porcelain=v2", "-z", "--untracked-files=no"],
    )?;
    for record in status
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
    {
        let mut fields = record.split(|byte| *byte == b' ');
        let kind = fields.next();
        let xy = fields.next();
        if matches!(kind, Some(b"1" | b"2")) && xy == Some(b".A") {
            return Err(Error::InvalidState(
                "fork refuses intent-to-add index entries".into(),
            ));
        }
    }
    Ok(())
}

fn refuse_staged_gitlink(command: &GitCommand, source: &Path) -> Result<()> {
    let diff = command.output(source, ["diff", "--cached", "--raw", "--no-renames", "-z"])?;
    for record in diff
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
    {
        if record.first() == Some(&b':') {
            let header = std::str::from_utf8(record)
                .map_err(|_| Error::InvalidState("Git emitted a non-ASCII raw diff".into()))?;
            let mut fields = header[1..].split_ascii_whitespace();
            if fields.next() == Some("160000") || fields.next() == Some("160000") {
                return Err(Error::InvalidState(
                    "fork refuses staged gitlink changes in V1".into(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_untracked_types(source: &GitSnapshot) -> Result<()> {
    for path in &source.untracked {
        let absolute = source.identity.worktree.join(&path.path);
        let metadata = std::fs::symlink_metadata(&absolute).map_err(|error| {
            Error::InvalidState(format!(
                "cannot inspect untracked path {}: {error}",
                absolute.display()
            ))
        })?;
        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            return Err(Error::InvalidState(format!(
                "fork refuses unsupported untracked path type at {}",
                path.path.display()
            )));
        }
    }
    Ok(())
}

fn refuse_nested_target(command: &GitCommand, source: &Path, target: &Path) -> Result<()> {
    if target.starts_with(source) {
        return Err(Error::InvalidState(
            "target worktree must not be inside the source worktree".into(),
        ));
    }
    let listing = command.output(source, ["worktree", "list", "--porcelain", "-z"])?;
    for record in listing
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
    {
        let Some(raw_path) = record.strip_prefix(b"worktree ") else {
            continue;
        };
        let worktree = PathBuf::from(OsString::from_vec(raw_path.to_vec()));
        require_utf8_path(&worktree, "registered worktree")?;
        let registered = worktree.canonicalize().map_err(|error| {
            Error::InvalidState(format!(
                "cannot canonicalize registered worktree {}: {error}",
                worktree.display()
            ))
        })?;
        if target.starts_with(&registered) {
            return Err(Error::InvalidState(format!(
                "target worktree must not be nested inside registered worktree {}",
                registered.display()
            )));
        }
    }
    Ok(())
}

fn refuse_unignored_special_nodes(command: &GitCommand, source: &Path) -> Result<()> {
    let gitlinks = gitlink_paths(command, source)?;
    let mut pending = vec![source.to_path_buf()];
    while !pending.is_empty() {
        let mut candidates = Vec::new();
        let mut directories = Vec::new();
        let mut special = Vec::new();
        for directory in std::mem::take(&mut pending) {
            let entries = std::fs::read_dir(&directory).map_err(|error| {
                Error::InvalidState(format!("cannot inspect {}: {error}", directory.display()))
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    Error::InvalidState(format!("cannot inspect {}: {error}", directory.display()))
                })?;
                let path = entry.path();
                let relative = path.strip_prefix(source).map_err(|_| {
                    Error::InvalidState("metadata walk escaped source worktree".into())
                })?;
                require_utf8_path(relative, "source metadata")?;
                if relative == Path::new(".git") || gitlinks.iter().any(|item| item == relative) {
                    continue;
                }
                let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                    Error::InvalidState(format!("cannot inspect {}: {error}", path.display()))
                })?;
                if metadata.is_dir() {
                    candidates.push(relative.to_path_buf());
                    directories.push((relative.to_path_buf(), path));
                } else if !metadata.is_file() && !metadata.file_type().is_symlink() {
                    candidates.push(relative.to_path_buf());
                    special.push(relative.to_path_buf());
                }
            }
        }
        let ignored = ignored_paths(command, source, &candidates)?;
        for (relative, absolute) in directories {
            if !ignored.contains(&relative) {
                pending.push(absolute);
            }
        }
        if let Some(path) = special.into_iter().find(|path| !ignored.contains(path)) {
            return Err(Error::InvalidState(format!(
                "fork refuses unignored special node {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn gitlink_paths(command: &GitCommand, source: &Path) -> Result<Vec<PathBuf>> {
    let entries = command.output(source, ["ls-files", "--stage", "-z"])?;
    let mut paths = Vec::new();
    for record in entries
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
    {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            return Err(Error::InvalidState("malformed Git index entry".into()));
        };
        let header = std::str::from_utf8(&record[..tab])
            .map_err(|_| Error::InvalidState("non-ASCII Git index header".into()))?;
        if header.split_ascii_whitespace().next() == Some("160000") {
            let path = PathBuf::from(OsString::from_vec(record[tab + 1..].to_vec()));
            require_utf8_path(&path, "gitlink")?;
            paths.push(path);
        }
    }
    Ok(paths)
}

fn ignored_paths(
    command: &GitCommand,
    source: &Path,
    candidates: &[PathBuf],
) -> Result<std::collections::HashSet<PathBuf>> {
    if candidates.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let mut input = Vec::new();
    for candidate in candidates {
        input.extend_from_slice(candidate.as_os_str().as_encoded_bytes());
        input.push(0);
    }
    let output =
        command.output_with_input_exit_one(source, ["check-ignore", "-z", "--stdin"], &input)?;
    if !output.is_empty() && !output.ends_with(&[0]) {
        return Err(Error::InvalidState(
            "Git ignore output was not NUL-terminated".into(),
        ));
    }
    output
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
        .map(|item| {
            let path = PathBuf::from(OsString::from_vec(item.to_vec()));
            require_utf8_path(&path, "ignored metadata")?;
            Ok(path)
        })
        .collect()
}

fn require_utf8_path(path: &Path, label: &str) -> Result<()> {
    if path.to_str().is_none() {
        return Err(Error::InvalidState(format!("{label} must be valid UTF-8")));
    }
    Ok(())
}

fn sanitize_branch_component(value: &str) -> String {
    let mut sanitized = String::new();
    let mut invalid_run = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            sanitized.push(character.to_ascii_lowercase());
            invalid_run = false;
        } else if !invalid_run && !sanitized.is_empty() {
            sanitized.push('-');
            invalid_run = true;
        }
    }
    while sanitized.ends_with('-') {
        sanitized.pop();
    }
    if sanitized.is_empty() {
        "repo".into()
    } else {
        sanitized
    }
}
