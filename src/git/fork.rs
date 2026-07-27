use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result, io};
use crate::fork::{ForkOperationStore, recover_fork_failure};
use crate::git::command::GitCommand;
use crate::git::fingerprint::{SubmoduleEntry, canonical_json, capture};
use crate::git::observe;
use crate::model::{
    ForkFingerprint, ForkOperation, ForkPhase, GitSnapshot, Provider, UntrackedEntry, UntrackedKind,
};
use crate::store::atomic::{read_private, sync_directory};

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationProof {
    pub fingerprint: ForkFingerprint,
    pub cleanup_inventory_sha256: String,
}

pub fn observe_target_proof(operation: &ForkOperation) -> Result<Option<MutationProof>> {
    let metadata = match std::fs::symlink_metadata(&operation.target_worktree) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io(&operation.target_worktree, source)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::InvalidState(format!(
            "fork target worktree {} is not a real directory",
            operation.target_worktree.display()
        )));
    }
    let target_root = operation
        .target_worktree
        .canonicalize()
        .map_err(|source| io(&operation.target_worktree, source))?;
    if target_root != operation.target_worktree {
        return Err(Error::InvalidState(format!(
            "fork target worktree {} no longer resolves to its recorded path",
            operation.target_worktree.display()
        )));
    }
    let command = GitCommand;
    let snapshot = observe::snapshot(&command, &target_root)?;
    if snapshot.identity.worktree != operation.target_worktree
        || snapshot.identity.common_git_dir != operation.source_worktree.common_git_dir
        || snapshot.head != operation.target_head
        || snapshot.branch.as_deref() != Some(operation.target_branch.as_str())
    {
        return Err(Error::InvalidState(format!(
            "fork target worktree {} is no longer registered at the recorded branch and HEAD",
            operation.target_worktree.display()
        )));
    }
    let inventory = capture_inventory(&target_root)?;
    Ok(Some(MutationProof {
        fingerprint: capture(&command, &target_root)?.fingerprint,
        cleanup_inventory_sha256: inventory.sha256,
    }))
}

pub fn remove_target_with_proof(operation: &ForkOperation, expected: &MutationProof) -> Result<()> {
    validate_target_with_proof(operation, expected)?;
    let command = GitCommand;
    command.output(
        &operation.source_worktree.worktree,
        [
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--force"),
            operation.target_worktree.as_os_str().to_os_string(),
        ],
    )?;
    remove_proven_branch(&command, operation)
}

pub fn validate_target_with_proof(
    operation: &ForkOperation,
    expected: &MutationProof,
) -> Result<()> {
    let fresh = observe_target_proof(operation)?.ok_or_else(|| {
        Error::InvalidState(format!(
            "fork target worktree {} is absent",
            operation.target_worktree.display()
        ))
    })?;
    if &fresh != expected {
        return Err(Error::InvalidState(format!(
            "fork target worktree {} changed after its last proven boundary",
            operation.target_worktree.display()
        )));
    }

    let command = GitCommand;
    require_registered_target(&command, operation)
}

pub fn remove_proven_branch(command: &GitCommand, operation: &ForkOperation) -> Result<()> {
    let reference = format!("refs/heads/{}", operation.target_branch);
    let head = command
        .optional_text_exit_one(
            &operation.source_worktree.worktree,
            ["rev-parse", "--verify", "--quiet", reference.as_str()],
        )?
        .ok_or_else(|| {
            Error::InvalidState(format!(
                "fork target branch {:?} is absent",
                operation.target_branch
            ))
        })?;
    if head != operation.target_head {
        return Err(Error::InvalidState(format!(
            "fork target branch {:?} changed from its recorded HEAD",
            operation.target_branch
        )));
    }
    if worktree_listing(&command.output(
        &operation.source_worktree.worktree,
        ["worktree", "list", "--porcelain", "-z"],
    )?)?
    .iter()
    .any(|entry| entry.branch.as_deref() == Some(reference.as_str()))
    {
        return Err(Error::InvalidState(format!(
            "fork target branch {:?} is still used by a worktree",
            operation.target_branch
        )));
    }
    command.output(
        &operation.source_worktree.worktree,
        [
            OsString::from("branch"),
            OsString::from("-D"),
            OsString::from("--"),
            OsString::from(&operation.target_branch),
        ],
    )?;
    Ok(())
}

#[derive(Default)]
struct WorktreeListingEntry {
    path: Option<PathBuf>,
    head: Option<String>,
    branch: Option<String>,
}

fn require_registered_target(command: &GitCommand, operation: &ForkOperation) -> Result<()> {
    let entries = worktree_listing(&command.output(
        &operation.source_worktree.worktree,
        ["worktree", "list", "--porcelain", "-z"],
    )?)?;
    let expected_branch = format!("refs/heads/{}", operation.target_branch);
    let matches = entries.iter().filter(|entry| {
        entry.path.as_deref() == Some(operation.target_worktree.as_path())
            && entry.head.as_deref() == Some(operation.target_head.as_str())
            && entry.branch.as_deref() == Some(expected_branch.as_str())
    });
    if matches.count() != 1 {
        return Err(Error::InvalidState(format!(
            "fork target worktree {} is not uniquely registered at the recorded branch and HEAD",
            operation.target_worktree.display()
        )));
    }
    Ok(())
}

fn worktree_listing(bytes: &[u8]) -> Result<Vec<WorktreeListingEntry>> {
    let mut entries = Vec::new();
    let mut current = WorktreeListingEntry::default();
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if current.path.is_some() {
                entries.push(std::mem::take(&mut current));
            }
            continue;
        }
        if let Some(raw) = field.strip_prefix(b"worktree ") {
            if current.path.is_some() {
                entries.push(std::mem::take(&mut current));
            }
            let path = PathBuf::from(OsString::from_vec(raw.to_vec()));
            require_utf8_path(&path, "registered worktree")?;
            current.path = Some(path);
        } else if let Some(raw) = field.strip_prefix(b"HEAD ") {
            current.head = Some(
                String::from_utf8(raw.to_vec())
                    .map_err(|_| Error::InvalidState("worktree HEAD is not UTF-8".into()))?,
            );
        } else if let Some(raw) = field.strip_prefix(b"branch ") {
            current.branch = Some(
                String::from_utf8(raw.to_vec())
                    .map_err(|_| Error::InvalidState("worktree branch is not UTF-8".into()))?,
            );
        }
    }
    if current.path.is_some() {
        entries.push(current);
    }
    Ok(entries)
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
    let worktree = parent.join(format!("{basename_utf8}-handover-{short_id}"));

    Ok(ForkTarget {
        branch: format!("handover/{branch_component}-{short_id}"),
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

pub fn materialize(
    store: &ForkOperationStore,
    source_cwd: &Path,
    mut boundary: impl FnMut(ForkPhase) -> Result<()>,
) -> Result<crate::model::ForkOperation> {
    let command = GitCommand;
    let operation = store.operation()?;
    if operation.phase != ForkPhase::ArtifactsCaptured {
        return Err(Error::InvalidState(format!(
            "fork materialization requires artifacts_captured, found {:?}",
            operation.phase
        )));
    }
    let source_fingerprint = operation.source_fingerprint.as_ref().ok_or_else(|| {
        Error::InvalidState("fork operation is missing its source fingerprint".into())
    })?;
    let current_source = capture(&command, source_cwd)?;
    if &current_source.fingerprint != source_fingerprint {
        return Err(Error::InvalidState(
            "source changed before fork materialization".into(),
        ));
    }
    let expected_source = observe::snapshot(&command, source_cwd)?;
    if !expected_source
        .identity
        .same_worktree_as(&operation.source_worktree)
    {
        return Err(Error::InvalidState(
            "fork materialization source does not match its operation".into(),
        ));
    }
    let (staged_patch, unstaged_patch) = verified_patches(store, source_fingerprint)?;
    let untracked = verified_untracked_manifest(store, source_fingerprint)?;
    let submodules = verified_submodule_manifest(store, source_fingerprint)?;

    command.output(
        &expected_source.identity.worktree,
        [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("-b"),
            OsString::from(&operation.target_branch),
            operation.target_worktree.as_os_str().to_os_string(),
            OsString::from(&operation.target_head),
        ],
    )?;
    let target_root = operation.target_worktree.canonicalize().map_err(|source| {
        Error::InvalidState(format!(
            "cannot canonicalize target worktree {}: {source}",
            operation.target_worktree.display()
        ))
    })?;
    verify_target_identity(&command, &expected_source, &operation, &target_root)?;
    let mut inventory = capture_inventory(&target_root)?;
    verify_allowed_inventory(&command, &target_root, &inventory, None, &[], &[])?;
    let mut target_fingerprint = capture(&command, &target_root)?.fingerprint;
    verify_semantic_layer(
        &command,
        &target_root,
        &operation.target_branch,
        &expected_source,
        SemanticLayer::Clean,
    )?;
    let proof = MutationProof {
        fingerprint: target_fingerprint.clone(),
        cleanup_inventory_sha256: inventory.sha256.clone(),
    };
    if let Err(error) = store.transition(
        ForkPhase::ArtifactsCaptured,
        ForkPhase::WorktreeCreated,
        |record| {
            record.branch_created = true;
            record.target_created = true;
            record.target_fingerprint = Some(target_fingerprint.clone());
            record.target_cleanup_inventory_sha256 = Some(inventory.sha256.clone());
        },
    ) {
        return materialization_error(store, error, Some(&proof));
    }
    if let Err(error) = boundary(ForkPhase::WorktreeCreated) {
        return materialization_error(store, error, None);
    }

    require_unchanged_target(&command, &target_root, &target_fingerprint, &inventory)?;
    apply_patch(
        &command,
        &target_root,
        &staged_patch,
        &source_fingerprint.staged_patch_sha256,
        true,
    )?;
    let next_inventory = capture_inventory(&target_root)?;
    verify_allowed_inventory(
        &command,
        &target_root,
        &next_inventory,
        Some(&inventory),
        &[],
        &[],
    )?;
    inventory = next_inventory;
    target_fingerprint = capture(&command, &target_root)?.fingerprint;
    verify_semantic_layer(
        &command,
        &target_root,
        &operation.target_branch,
        &expected_source,
        SemanticLayer::Staged,
    )?;
    let proof = MutationProof {
        fingerprint: target_fingerprint.clone(),
        cleanup_inventory_sha256: inventory.sha256.clone(),
    };
    if let Err(error) = store.transition(
        ForkPhase::WorktreeCreated,
        ForkPhase::StagedApplied,
        |record| {
            record.target_fingerprint = Some(target_fingerprint.clone());
            record.target_cleanup_inventory_sha256 = Some(inventory.sha256.clone());
        },
    ) {
        return materialization_error(store, error, Some(&proof));
    }
    if let Err(error) = boundary(ForkPhase::StagedApplied) {
        return materialization_error(store, error, None);
    }

    require_unchanged_target(&command, &target_root, &target_fingerprint, &inventory)?;
    apply_patch(
        &command,
        &target_root,
        &unstaged_patch,
        &source_fingerprint.unstaged_patch_sha256,
        false,
    )?;
    let next_inventory = capture_inventory(&target_root)?;
    verify_allowed_inventory(
        &command,
        &target_root,
        &next_inventory,
        Some(&inventory),
        &[],
        &[],
    )?;
    inventory = next_inventory;
    target_fingerprint = capture(&command, &target_root)?.fingerprint;
    verify_semantic_layer(
        &command,
        &target_root,
        &operation.target_branch,
        &expected_source,
        SemanticLayer::Unstaged,
    )?;
    let proof = MutationProof {
        fingerprint: target_fingerprint.clone(),
        cleanup_inventory_sha256: inventory.sha256.clone(),
    };
    if let Err(error) = store.transition(
        ForkPhase::StagedApplied,
        ForkPhase::UnstagedApplied,
        |record| {
            record.target_fingerprint = Some(target_fingerprint.clone());
            record.target_cleanup_inventory_sha256 = Some(inventory.sha256.clone());
        },
    ) {
        return materialization_error(store, error, Some(&proof));
    }
    if let Err(error) = boundary(ForkPhase::UnstagedApplied) {
        return materialization_error(store, error, None);
    }

    require_unchanged_target(&command, &target_root, &target_fingerprint, &inventory)?;
    if let Err(error) = restore_untracked(store, &target_root, &untracked) {
        let proof = observe_target_proof(&operation).ok().flatten();
        return materialization_error(store, error, proof.as_ref());
    }
    if let Err(error) = restore_submodules(
        &command,
        &expected_source.identity.worktree,
        &target_root,
        &submodules,
    ) {
        let proof = observe_target_proof(&operation).ok().flatten();
        return materialization_error(store, error, proof.as_ref());
    }
    let next_inventory = capture_inventory(&target_root)?;
    verify_allowed_inventory(
        &command,
        &target_root,
        &next_inventory,
        Some(&inventory),
        &untracked,
        &submodules,
    )?;
    inventory = next_inventory;
    target_fingerprint = capture(&command, &target_root)?.fingerprint;
    verify_semantic_layer(
        &command,
        &target_root,
        &operation.target_branch,
        &expected_source,
        SemanticLayer::Complete,
    )?;
    let proof = MutationProof {
        fingerprint: target_fingerprint.clone(),
        cleanup_inventory_sha256: inventory.sha256.clone(),
    };
    if let Err(error) = store.transition(
        ForkPhase::UnstagedApplied,
        ForkPhase::UntrackedCopied,
        |record| {
            record.target_fingerprint = Some(target_fingerprint.clone());
            record.target_cleanup_inventory_sha256 = Some(inventory.sha256.clone());
        },
    ) {
        return materialization_error(store, error, Some(&proof));
    }
    if let Err(error) = boundary(ForkPhase::UntrackedCopied) {
        return materialization_error(store, error, None);
    }

    let fresh_source = capture(&command, source_cwd)?;
    if &fresh_source.fingerprint != source_fingerprint {
        return Err(Error::InvalidState(
            "source changed during fork materialization".into(),
        ));
    }
    verify_saved_cwd(&target_root, &operation.source_worktree.cwd_relative)?;
    let fresh_target = capture(&command, &target_root)?.fingerprint;
    let final_inventory = capture_inventory(&target_root)?;
    if fresh_target != target_fingerprint || final_inventory != inventory {
        return Err(Error::InvalidState(
            "target changed during fork verification".into(),
        ));
    }
    let proof = MutationProof {
        fingerprint: fresh_target.clone(),
        cleanup_inventory_sha256: final_inventory.sha256.clone(),
    };
    let verified =
        match store.transition(ForkPhase::UntrackedCopied, ForkPhase::Verified, |record| {
            record.target_fingerprint = Some(fresh_target);
            record.target_cleanup_inventory_sha256 = Some(final_inventory.sha256);
        }) {
            Ok(verified) => verified,
            Err(error) => return materialization_error(store, error, Some(&proof)),
        };
    if let Err(error) = boundary(ForkPhase::Verified) {
        return materialization_error(store, error, None);
    }
    Ok(verified)
}

fn materialization_error<T>(
    store: &ForkOperationStore,
    error: Error,
    live_proof: Option<&MutationProof>,
) -> Result<T> {
    let message = error.to_string();
    match recover_fork_failure(store, &message, live_proof) {
        Ok(_) => Err(error),
        Err(recovery_error) => Err(Error::InvalidState(format!(
            "{message}; fork recovery failed: {recovery_error}"
        ))),
    }
}

fn verified_patches(
    store: &ForkOperationStore,
    fingerprint: &crate::model::ForkFingerprint,
) -> Result<(PathBuf, PathBuf)> {
    let staged = store.operation_dir().join("staged.patch");
    let unstaged = store.operation_dir().join("unstaged.patch");
    let staged_bytes = read_private(&staged)?;
    let unstaged_bytes = read_private(&unstaged)?;
    if sha256(&staged_bytes) != fingerprint.staged_patch_sha256
        || sha256(&unstaged_bytes) != fingerprint.unstaged_patch_sha256
    {
        return Err(Error::InvalidState(
            "immutable fork patch does not match its fingerprint".into(),
        ));
    }
    Ok((staged, unstaged))
}

fn verified_untracked_manifest(
    store: &ForkOperationStore,
    fingerprint: &crate::model::ForkFingerprint,
) -> Result<Vec<UntrackedEntry>> {
    let bytes = read_private(&store.operation_dir().join("untracked/manifest.json"))?;
    if sha256(&bytes) != fingerprint.untracked_manifest_sha256 {
        return Err(Error::InvalidState(
            "untracked manifest does not match its fingerprint".into(),
        ));
    }
    let entries: Vec<UntrackedEntry> = decode_canonical_json(&bytes, "untracked manifest")?;
    let mut previous: Option<&Path> = None;
    for entry in &entries {
        entry.validate()?;
        if previous.is_some_and(|path| path >= entry.path.as_path()) {
            return Err(Error::InvalidState(
                "untracked manifest is not strictly path-sorted".into(),
            ));
        }
        previous = Some(&entry.path);
    }
    Ok(entries)
}

fn verified_submodule_manifest(
    store: &ForkOperationStore,
    fingerprint: &crate::model::ForkFingerprint,
) -> Result<Vec<SubmoduleEntry>> {
    let bytes = read_private(&store.operation_dir().join("submodules.json"))?;
    if sha256(&bytes) != fingerprint.submodule_manifest_sha256 {
        return Err(Error::InvalidState(
            "submodule manifest does not match its fingerprint".into(),
        ));
    }
    let entries: Vec<SubmoduleEntry> = decode_canonical_json(&bytes, "submodule manifest")?;
    let mut previous: Option<&Path> = None;
    let mut initialized = BTreeSet::new();
    for entry in &entries {
        require_relative_path(&entry.path, "submodule path")?;
        require_object_id(&entry.expected_object)?;
        if previous.is_some_and(|path| path >= entry.path.as_path()) {
            return Err(Error::InvalidState(
                "submodule manifest is not strictly path-sorted".into(),
            ));
        }
        if let Some(parent) = entry.parent.as_ref() {
            require_relative_path(parent, "submodule parent")?;
            let suffix = entry.path.strip_prefix(parent).map_err(|_| {
                Error::InvalidState("nested submodule does not descend from its parent".into())
            })?;
            require_relative_path(suffix, "nested submodule path")?;
            if !initialized.contains(parent) {
                return Err(Error::InvalidState(
                    "nested submodule parent is not an earlier initialized entry".into(),
                ));
            }
        }
        if entry.initialized {
            initialized.insert(entry.path.clone());
        }
        previous = Some(&entry.path);
    }
    Ok(entries)
}

fn require_unchanged_target(
    command: &GitCommand,
    target_root: &Path,
    expected_fingerprint: &crate::model::ForkFingerprint,
    expected_inventory: &Inventory,
) -> Result<()> {
    let fingerprint = capture(command, target_root)?.fingerprint;
    let inventory = capture_inventory(target_root)?;
    if &fingerprint != expected_fingerprint || &inventory != expected_inventory {
        return Err(Error::InvalidState(
            "target changed after its last durable fork phase".into(),
        ));
    }
    Ok(())
}

fn apply_patch(
    command: &GitCommand,
    target: &Path,
    patch: &Path,
    expected_sha256: &str,
    staged: bool,
) -> Result<()> {
    let bytes = read_private(patch)?;
    if sha256(&bytes) != expected_sha256 {
        return Err(Error::InvalidState(format!(
            "immutable fork patch changed at {}",
            patch.display()
        )));
    }
    if bytes.is_empty() {
        return Ok(());
    }
    let mut args = vec![OsString::from("apply")];
    if staged {
        args.push(OsString::from("--index"));
    }
    args.extend([
        OsString::from("--binary"),
        OsString::from("--whitespace=nowarn"),
        OsString::from("--"),
        patch.as_os_str().to_os_string(),
    ]);
    command.output(target, args)?;
    Ok(())
}

fn verify_target_identity(
    command: &GitCommand,
    source: &GitSnapshot,
    operation: &crate::model::ForkOperation,
    target_root: &Path,
) -> Result<()> {
    let target = observe::snapshot(command, target_root)?;
    if target.head != operation.target_head
        || target.branch.as_deref() != Some(operation.target_branch.as_str())
        || target.identity.common_git_dir != source.identity.common_git_dir
        || target.identity.worktree != target_root
    {
        return Err(Error::InvalidState(
            "created target worktree identity does not match the fork operation".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum SemanticLayer {
    Clean,
    Staged,
    Unstaged,
    Complete,
}

fn verify_semantic_layer(
    command: &GitCommand,
    target_root: &Path,
    target_branch: &str,
    source: &GitSnapshot,
    layer: SemanticLayer,
) -> Result<()> {
    let target = observe::snapshot(command, target_root)?;
    let (staged, unstaged, untracked) = match layer {
        SemanticLayer::Clean => (&[][..], &[][..], &[][..]),
        SemanticLayer::Staged => (&source.staged[..], &[][..], &[][..]),
        SemanticLayer::Unstaged => (&source.staged[..], &source.unstaged[..], &[][..]),
        SemanticLayer::Complete => (
            &source.staged[..],
            &source.unstaged[..],
            &source.untracked[..],
        ),
    };
    if target.head != source.head
        || target.branch.as_deref() != Some(target_branch)
        || target.staged != staged
        || target.unstaged != unstaged
        || target.untracked != untracked
        || !target.dirty_submodules.is_empty()
    {
        return Err(Error::InvalidState(format!(
            "target semantic state does not match the allowed fork layer {layer:?}: staged {:?} expected {:?}; unstaged {:?} expected {:?}; untracked {:?} expected {:?}",
            target.staged, staged, target.unstaged, unstaged, target.untracked, untracked
        )));
    }
    Ok(())
}

fn restore_untracked(
    store: &ForkOperationStore,
    target_root: &Path,
    entries: &[UntrackedEntry],
) -> Result<()> {
    for entry in entries {
        entry.validate()?;
        let destination = secure_destination(target_root, &entry.path)?;
        match entry.kind {
            UntrackedKind::Regular => {
                let artifact = entry.artifact.as_ref().expect("validated regular artifact");
                let bytes = read_private(&store.operation_dir().join(artifact))?;
                if bytes.len() as u64 != entry.bytes || sha256(&bytes) != entry.sha256 {
                    return Err(Error::InvalidState(format!(
                        "untracked blob does not match manifest at {}",
                        entry.path.display()
                    )));
                }
                let mode = if entry.executable { 0o755 } else { 0o644 };
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode)
                    .open(&destination)
                    .map_err(|source| io(&destination, source))?;
                file.set_permissions(std::fs::Permissions::from_mode(mode))
                    .map_err(|source| io(&destination, source))?;
                file.write_all(&bytes)
                    .map_err(|source| io(&destination, source))?;
                file.sync_all().map_err(|source| io(&destination, source))?;
            }
            UntrackedKind::Symlink => {
                let link_target = entry
                    .symlink_target
                    .as_ref()
                    .expect("validated symlink target");
                if sha256(link_target.as_os_str().as_encoded_bytes()) != entry.sha256 {
                    return Err(Error::InvalidState(format!(
                        "untracked symlink target does not match manifest at {}",
                        entry.path.display()
                    )));
                }
                symlink(link_target, &destination).map_err(|source| io(&destination, source))?;
            }
        }
        sync_directory(destination.parent().expect("destination has parent"))?;
    }
    Ok(())
}

fn secure_destination(root: &Path, relative: &Path) -> Result<PathBuf> {
    require_relative_path(relative, "untracked destination")?;
    let parent = relative
        .parent()
        .ok_or_else(|| Error::InvalidState("untracked path has no parent".into()))?;
    let mut current = root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(component) = component else {
            return Err(Error::InvalidState(
                "untracked parent path is not normalized".into(),
            ));
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(Error::InvalidState(format!(
                    "untracked parent {} is not a real directory",
                    current.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o755);
                builder
                    .create(&current)
                    .map_err(|source| io(&current, source))?;
                std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o755))
                    .map_err(|source| io(&current, source))?;
                sync_directory(&current)?;
                sync_directory(current.parent().expect("created directory has parent"))?;
            }
            Err(source) => return Err(io(&current, source)),
        }
    }
    let destination = root.join(relative);
    if !destination.starts_with(root) {
        return Err(Error::InvalidState(
            "untracked destination escapes target worktree".into(),
        ));
    }
    match std::fs::symlink_metadata(&destination) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
        Err(source) => Err(io(&destination, source)),
        Ok(_) => Err(Error::InvalidState(format!(
            "untracked destination {} already exists",
            destination.display()
        ))),
    }
}

fn restore_submodules(
    command: &GitCommand,
    source_root: &Path,
    target_root: &Path,
    entries: &[SubmoduleEntry],
) -> Result<()> {
    for entry in entries.iter().filter(|entry| entry.initialized) {
        let source_submodule = source_root.join(&entry.path);
        let source_head = command.text(&source_submodule, ["rev-parse", "HEAD"])?;
        if source_head != entry.expected_object {
            return Err(Error::InvalidState(format!(
                "source submodule HEAD changed at {}",
                entry.path.display()
            )));
        }
        let (parent_repository, local_path) = match entry.parent.as_ref() {
            Some(parent) => (
                target_root.join(parent),
                entry.path.strip_prefix(parent).map_err(|_| {
                    Error::InvalidState("submodule path escaped its recorded parent".into())
                })?,
            ),
            None => (target_root.to_path_buf(), entry.path.as_path()),
        };
        seed_submodule_repository(command, &source_submodule, &parent_repository, local_path)?;
        command.output(
            &parent_repository,
            [
                OsString::from("-c"),
                OsString::from("protocol.allow=never"),
                OsString::from("submodule"),
                OsString::from("update"),
                OsString::from("--init"),
                OsString::from("--no-fetch"),
                OsString::from("--"),
                local_path.as_os_str().to_os_string(),
            ],
        )?;
        let target_submodule = target_root.join(&entry.path);
        let head = command.text(&target_submodule, ["rev-parse", "HEAD"])?;
        if head != entry.expected_object {
            return Err(Error::InvalidState(format!(
                "restored submodule HEAD differs at {}",
                entry.path.display()
            )));
        }
    }
    Ok(())
}

fn seed_submodule_repository(
    command: &GitCommand,
    source_submodule: &Path,
    target_parent_repository: &Path,
    local_path: &Path,
) -> Result<()> {
    let source_objects = PathBuf::from(command.text(
        source_submodule,
        [
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ],
    )?)
    .canonicalize()
    .map_err(|source| io(source_submodule, source))?;
    require_utf8_path(&source_objects, "source submodule object directory")?;
    let module_path = PathBuf::from("modules").join(local_path);
    let module_git_dir = PathBuf::from(command.text(
        target_parent_repository,
        [
            OsString::from("rev-parse"),
            OsString::from("--path-format=absolute"),
            OsString::from("--git-path"),
            module_path.into_os_string(),
        ],
    )?);
    match std::fs::symlink_metadata(&module_git_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(io(&module_git_dir, source)),
        Ok(_) => {
            return Err(Error::InvalidState(format!(
                "target submodule repository {} already exists",
                module_git_dir.display()
            )));
        }
    }
    command.output(
        target_parent_repository,
        [
            OsString::from("init"),
            OsString::from("--bare"),
            module_git_dir.as_os_str().to_os_string(),
        ],
    )?;
    let target_worktree = target_parent_repository.join(local_path);
    for (key, value) in [
        (OsString::from("core.bare"), OsString::from("false")),
        (
            OsString::from("core.worktree"),
            target_worktree.as_os_str().to_os_string(),
        ),
    ] {
        command.output(
            target_parent_repository,
            [
                OsString::from("--git-dir"),
                module_git_dir.as_os_str().to_os_string(),
                OsString::from("config"),
                key,
                value,
            ],
        )?;
    }
    let alternates = module_git_dir.join("objects/info/alternates");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o644)
        .open(&alternates)
        .map_err(|source| io(&alternates, source))?;
    file.write_all(source_objects.as_os_str().as_encoded_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|source| io(&alternates, source))?;
    file.sync_all().map_err(|source| io(&alternates, source))?;
    sync_directory(alternates.parent().expect("alternates has parent"))?;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InventoryKind {
    Directory,
    Regular,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryEntry {
    path: PathBuf,
    kind: InventoryKind,
    sha256: Option<String>,
    executable: bool,
    symlink_target: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Inventory {
    entries: Vec<InventoryEntry>,
    sha256: String,
}

fn capture_inventory(root: &Path) -> Result<Inventory> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let children = std::fs::read_dir(&directory).map_err(|source| io(&directory, source))?;
        for child in children {
            let child = child.map_err(|source| io(&directory, source))?;
            let absolute = child.path();
            let relative = absolute
                .strip_prefix(root)
                .map_err(|_| Error::InvalidState("target inventory escaped worktree".into()))?
                .to_path_buf();
            require_relative_path(&relative, "target inventory path")?;
            if relative == Path::new(".git") {
                continue;
            }
            let metadata =
                std::fs::symlink_metadata(&absolute).map_err(|source| io(&absolute, source))?;
            let executable = metadata.permissions().mode() & 0o111 != 0;
            let (kind, digest, link_target) = if metadata.file_type().is_symlink() {
                let target =
                    std::fs::read_link(&absolute).map_err(|source| io(&absolute, source))?;
                require_utf8_path(&target, "inventory symlink target")?;
                (
                    InventoryKind::Symlink,
                    Some(sha256(target.as_os_str().as_encoded_bytes())),
                    Some(target),
                )
            } else if metadata.is_file() {
                (
                    InventoryKind::Regular,
                    Some(sha256(
                        &std::fs::read(&absolute).map_err(|source| io(&absolute, source))?,
                    )),
                    None,
                )
            } else if metadata.is_dir() {
                pending.push(absolute);
                (InventoryKind::Directory, None, None)
            } else {
                return Err(Error::InvalidState(format!(
                    "unsupported target inventory node {}",
                    relative.display()
                )));
            };
            entries.push(InventoryEntry {
                path: relative,
                kind,
                sha256: digest,
                executable,
                symlink_target: link_target,
            });
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let digest = sha256(&canonical_json(&entries)?);
    Ok(Inventory {
        entries,
        sha256: digest,
    })
}

fn verify_allowed_inventory(
    command: &GitCommand,
    target_root: &Path,
    inventory: &Inventory,
    previous: Option<&Inventory>,
    untracked: &[UntrackedEntry],
    submodules: &[SubmoduleEntry],
) -> Result<()> {
    let mut allowed = BTreeSet::new();
    collect_allowed_repository(command, target_root, Path::new(""), &mut allowed)?;
    for entry in untracked {
        if std::fs::symlink_metadata(target_root.join(&entry.path)).is_ok() {
            insert_with_parents(&mut allowed, &entry.path);
        }
    }
    for entry in submodules.iter().filter(|entry| entry.initialized) {
        let git_metadata = entry.path.join(".git");
        if std::fs::symlink_metadata(target_root.join(&git_metadata)).is_ok() {
            insert_with_parents(&mut allowed, &git_metadata);
        }
        collect_allowed_repository(
            command,
            &target_root.join(&entry.path),
            &entry.path,
            &mut allowed,
        )?;
    }
    if let Some(previous) = previous {
        for entry in &previous.entries {
            if entry.kind == InventoryKind::Directory
                && std::fs::symlink_metadata(target_root.join(&entry.path))
                    .is_ok_and(|metadata| metadata.is_dir())
            {
                allowed.insert(entry.path.clone());
            }
        }
    }
    let actual = inventory
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>();
    if actual != allowed {
        let unexpected = actual.difference(&allowed).next();
        let missing = allowed.difference(&actual).next();
        return Err(Error::InvalidState(format!(
            "target inventory contains unexpected or missing paths (unexpected: {unexpected:?}, missing: {missing:?})"
        )));
    }
    Ok(())
}

fn collect_allowed_repository(
    command: &GitCommand,
    repository: &Path,
    prefix: &Path,
    allowed: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let paths = nul_paths(command.output(repository, ["ls-files", "--cached", "-z"])?)?;
    for path in paths {
        if std::fs::symlink_metadata(repository.join(&path)).is_ok() {
            insert_with_parents(allowed, &prefix.join(path));
        }
    }
    Ok(())
}

fn insert_with_parents(paths: &mut BTreeSet<PathBuf>, path: &Path) {
    let mut current = Some(path);
    while let Some(path) = current {
        if !path.as_os_str().is_empty() {
            paths.insert(path.to_path_buf());
        }
        current = path.parent();
    }
}

fn verify_saved_cwd(target_root: &Path, cwd_relative: &Path) -> Result<()> {
    let path = target_root.join(cwd_relative);
    let canonical = path.canonicalize().map_err(|source| io(&path, source))?;
    if !canonical.starts_with(target_root) || !canonical.is_dir() {
        return Err(Error::InvalidState(
            "saved cwd is not a real directory in the target worktree".into(),
        ));
    }
    Ok(())
}

fn decode_canonical_json<T>(bytes: &[u8], label: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let value = serde_json::from_slice(bytes)
        .map_err(|error| Error::InvalidState(format!("cannot decode {label}: {error}")))?;
    if canonical_json(&value)? != bytes {
        return Err(Error::InvalidState(format!(
            "{label} is not canonical JSON"
        )));
    }
    Ok(value)
}

fn nul_paths(bytes: Vec<u8>) -> Result<Vec<PathBuf>> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(Error::InvalidState(
            "Git path list was not NUL-terminated".into(),
        ));
    }
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = PathBuf::from(OsString::from_vec(path.to_vec()));
            require_relative_path(&path, "Git path")?;
            Ok(path)
        })
        .collect()
}

fn require_relative_path(path: &Path, label: &str) -> Result<()> {
    if path.as_os_str().is_empty()
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

fn require_object_id(value: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::InvalidState("malformed Git object ID".into()));
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
