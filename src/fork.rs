use std::collections::BTreeSet;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result, io};
use crate::git::Git;
use crate::git::command::GitCommand;
use crate::git::fingerprint::{CapturedForkState, capture};
use crate::git::fork::{
    MutationProof, observe_target_proof, remove_proven_branch, remove_target_with_proof,
    validate_target_with_proof,
};
use crate::model::{EventKind, ForkOperation, ForkPhase, OperationId};
use crate::store::atomic::{create_private, sync_directory};
use crate::store::lease::SessionOperationLock;
use crate::store::refs::{read_json, write_json, write_json_create};
use crate::store::{SessionStore, StateLayout};

#[derive(Clone, Debug)]
pub struct ForkOperationStore {
    layout: StateLayout,
    id: OperationId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedChildProof {
    pub child_session_id: crate::model::SessionId,
    pub source_checkpoint_sequence: u64,
}

impl ForkOperationStore {
    pub fn create(layout: &StateLayout, operation: &ForkOperation) -> Result<Self> {
        operation.validate()?;
        layout.ensure()?;
        let layout = layout.canonicalized()?;
        let operation_dir = layout.operations().join(operation.id.to_string());
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
            .create(&operation_dir)
            .map_err(|source| io(&operation_dir, source))?;
        std::fs::set_permissions(&operation_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|source| io(&operation_dir, source))?;
        sync_directory(&layout.operations())?;
        if let Err(error) = write_json_create(&operation_dir.join("operation.json"), operation) {
            let _ = std::fs::remove_dir(&operation_dir);
            let _ = sync_directory(&layout.operations());
            return Err(error);
        }
        sync_directory(&operation_dir)?;
        Ok(Self {
            layout,
            id: operation.id.clone(),
        })
    }

    pub fn read(layout: &StateLayout, id: OperationId) -> Result<ForkOperation> {
        Self::open(layout, id)?.read_current()
    }

    pub fn open(layout: &StateLayout, id: OperationId) -> Result<Self> {
        validate_private_directory(layout.root(), "state root")?;
        validate_private_directory(&layout.operations(), "operations root")?;
        let layout = layout.canonicalized()?;
        let store = Self { layout, id };
        store.read_current()?;
        Ok(store)
    }

    pub fn transition(
        &self,
        expected: ForkPhase,
        next: ForkPhase,
        update: impl FnOnce(&mut ForkOperation),
    ) -> Result<ForkOperation> {
        validate_transition(expected, next)?;
        let _lock = SessionOperationLock::acquire(&self.operation_dir())?;
        let mut operation = self.read_current()?;
        if operation.phase != expected {
            return Err(Error::InvalidState(format!(
                "fork operation {} is {:?}, expected {:?}",
                self.id, operation.phase, expected
            )));
        }
        update(&mut operation);
        operation.phase = next;
        operation.updated_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| {
                Error::InvalidState(format!("cannot format fork operation time: {error}"))
            })?;
        operation.validate()?;
        write_json(&self.operation_dir().join("operation.json"), &operation)?;
        sync_directory(&self.operation_dir())?;
        Ok(operation)
    }

    pub fn operation_dir(&self) -> PathBuf {
        self.layout.operations().join(self.id.to_string())
    }

    pub fn id(&self) -> &OperationId {
        &self.id
    }

    pub fn operation(&self) -> Result<ForkOperation> {
        self.read_current()
    }

    pub fn layout(&self) -> &StateLayout {
        &self.layout
    }

    fn read_current(&self) -> Result<ForkOperation> {
        validate_private_operation_dir(&self.operation_dir(), &self.id)?;
        let operation: ForkOperation = read_json(&self.operation_dir().join("operation.json"))?;
        if operation.id != self.id {
            return Err(Error::InvalidState(format!(
                "fork operation directory {} contains record {}",
                self.id, operation.id
            )));
        }
        operation.validate()?;
        Ok(operation)
    }
}

pub fn recover_fork_failure(
    store: &ForkOperationStore,
    message: &str,
    live_proof: Option<&MutationProof>,
) -> Result<ForkOperation> {
    recover_fork_failure_with_live_child(store, message, live_proof, None)
}

pub fn recover_fork_failure_with_live_child(
    store: &ForkOperationStore,
    message: &str,
    live_proof: Option<&MutationProof>,
    live_child: Option<&StagedChildProof>,
) -> Result<ForkOperation> {
    let operation = store.operation()?;
    if operation.phase == ForkPhase::RolledBack {
        return Ok(operation);
    }
    if operation.phase == ForkPhase::Complete {
        return Ok(operation);
    }
    if lineage_commit_evidence(store.layout(), &operation)? {
        return recover_committed_fork(store);
    }
    if matches!(
        operation.phase,
        ForkPhase::LineageCommitted | ForkPhase::ChildBound | ForkPhase::RunLeased
    ) {
        return mark_manual(
            store,
            &operation,
            format!(
                "{message}; fork phase implies committed lineage but the parent session.forked event is absent"
            ),
        );
    }

    if let (Some(recorded), Some(live)) = (operation.child_session_id.as_ref(), live_child)
        && (recorded != &live.child_session_id
            || operation.source_checkpoint_sequence != Some(live.source_checkpoint_sequence))
    {
        return mark_manual(
            store,
            &operation,
            format!("{message}; live staged child proof conflicts with the operation record"),
        );
    }
    let child_cleanup_operation = if operation.child_session_id.is_some() {
        Some(operation.clone())
    } else {
        live_child.map(|proof| {
            let mut cleanup = operation.clone();
            cleanup.child_session_id = Some(proof.child_session_id.clone());
            cleanup.source_checkpoint_sequence = Some(proof.source_checkpoint_sequence);
            cleanup
        })
    };
    let child_exists = if let Some(cleanup) = child_cleanup_operation.as_ref() {
        let child_id = cleanup
            .child_session_id
            .as_ref()
            .expect("child cleanup operation has an ID");
        let child_dir = store.layout().sessions().join(child_id.to_string());
        if std::fs::symlink_metadata(&child_dir).is_ok() {
            if let Err(error) = validate_staged_child(store.layout(), cleanup) {
                return mark_manual(
                    store,
                    &operation,
                    format!(
                        "{message}; staged child session {} cannot be proven: {error}",
                        child_dir.display()
                    ),
                );
            }
            true
        } else {
            false
        }
    } else {
        false
    };

    let target_exists = match std::fs::symlink_metadata(&operation.target_worktree) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => {
            return mark_manual(
                store,
                &operation,
                format!(
                    "{message}; cannot inspect target worktree {}: {source}",
                    operation.target_worktree.display()
                ),
            );
        }
    };
    let target_cleanup_proof = if target_exists {
        let expected = live_proof.cloned().or_else(|| operation_proof(&operation));
        let Some(expected) = expected else {
            return mark_manual(
                store,
                &operation,
                format!(
                    "{message}; target worktree {} has no durable or live mutation proof",
                    operation.target_worktree.display()
                ),
            );
        };
        if let Err(error) = validate_target_with_proof(&operation, &expected) {
            return mark_manual(
                store,
                &operation,
                format!(
                    "{message}; target worktree {} cannot be proven: {error}",
                    operation.target_worktree.display()
                ),
            );
        }
        Some(expected)
    } else {
        None
    };

    if child_exists
        && let Some(cleanup) = child_cleanup_operation.as_ref()
        && let Err(error) = remove_staged_child(store.layout(), cleanup)
    {
        let child_id = cleanup
            .child_session_id
            .as_ref()
            .expect("child cleanup operation has an ID");
        return mark_manual(
            store,
            &operation,
            format!(
                "{message}; staged child session {} was not removed: {error}",
                store
                    .layout()
                    .sessions()
                    .join(child_id.to_string())
                    .display()
            ),
        );
    }
    if let Some(expected) = target_cleanup_proof {
        if let Err(error) = remove_target_with_proof(&operation, &expected) {
            return mark_manual(
                store,
                &operation,
                format!(
                    "{message}; target worktree {} was not removed: {error}",
                    operation.target_worktree.display()
                ),
            );
        }
    } else if operation.branch_created
        && branch_exists(&operation)?
        && let Err(error) = remove_proven_branch(&GitCommand, &operation)
    {
        return mark_manual(
            store,
            &operation,
            format!(
                "{message}; target branch {:?} was not removed: {error}",
                operation.target_branch
            ),
        );
    }

    store.transition(operation.phase, ForkPhase::RolledBack, |record| {
        record.target_created = false;
        record.branch_created = false;
        record.error = Some(message.to_owned());
    })
}

pub fn recover_committed_fork(store: &ForkOperationStore) -> Result<ForkOperation> {
    let mut operation = store.operation()?;
    if !lineage_commit_evidence(store.layout(), &operation)? {
        return Err(Error::InvalidState(format!(
            "fork operation {} has no canonical parent lineage commit evidence",
            operation.id
        )));
    }
    if operation.phase == ForkPhase::Complete {
        return Ok(operation);
    }
    if !matches!(
        operation.phase,
        ForkPhase::ChildStaged
            | ForkPhase::LineageCommitted
            | ForkPhase::ChildBound
            | ForkPhase::RunLeased
    ) {
        return mark_manual(
            store,
            &operation,
            format!(
                "parent lineage is committed but fork operation {} is in incompatible phase {:?}",
                operation.id, operation.phase
            ),
        );
    }

    let Some(expected) = operation_proof(&operation) else {
        return mark_manual(
            store,
            &operation,
            "committed fork target has no durable fingerprint".into(),
        );
    };
    match observe_target_proof(&operation) {
        Ok(Some(fresh)) if fresh == expected => {}
        Ok(Some(_)) => {
            return mark_manual(
                store,
                &operation,
                format!(
                    "committed fork target worktree {} changed before forward recovery",
                    operation.target_worktree.display()
                ),
            );
        }
        Ok(None) => {
            return mark_manual(
                store,
                &operation,
                format!(
                    "committed fork target worktree {} is absent",
                    operation.target_worktree.display()
                ),
            );
        }
        Err(error) => {
            return mark_manual(
                store,
                &operation,
                format!(
                    "committed fork target worktree {} cannot be proven: {error}",
                    operation.target_worktree.display()
                ),
            );
        }
    }

    let child_id = operation
        .child_session_id
        .clone()
        .expect("committed fork phase validated child ID");
    let child = match SessionStore::open(store.layout(), child_id) {
        Ok(child) => child,
        Err(error) => {
            return mark_manual(
                store,
                &operation,
                format!("committed fork child session cannot be opened: {error}"),
            );
        }
    };
    if child.meta().parent_session_id.as_ref() != Some(&operation.source_session_id)
        || child.meta().parent_checkpoint_sequence != operation.source_checkpoint_sequence
        || child.meta().worktree.worktree != operation.target_worktree
        || child.meta().worktree.common_git_dir != operation.source_worktree.common_git_dir
    {
        return mark_manual(
            store,
            &operation,
            "committed fork child metadata conflicts with the operation".into(),
        );
    }

    if operation.phase == ForkPhase::ChildStaged {
        operation =
            store.transition(ForkPhase::ChildStaged, ForkPhase::LineageCommitted, |_| {})?;
    }
    if operation.phase == ForkPhase::LineageCommitted {
        if let Err(error) = child.bind_worktree() {
            return mark_manual(
                store,
                &operation,
                format!("committed child worktree binding conflicts: {error}"),
            );
        }
        operation = store.transition(ForkPhase::LineageCommitted, ForkPhase::ChildBound, |_| {})?;
    } else if let Err(error) = child.bind_worktree() {
        return mark_manual(
            store,
            &operation,
            format!("committed child worktree binding conflicts: {error}"),
        );
    }
    if operation.phase == ForkPhase::RunLeased {
        operation = store.transition(ForkPhase::RunLeased, ForkPhase::Complete, |_| {})?;
    }
    Ok(operation)
}

pub fn lineage_commit_evidence(layout: &StateLayout, operation: &ForkOperation) -> Result<bool> {
    let Some(child_id) = operation.child_session_id.as_ref() else {
        return Ok(false);
    };
    let parent_dir = layout
        .sessions()
        .join(operation.source_session_id.to_string());
    match std::fs::symlink_metadata(&parent_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io(&parent_dir, source)),
        Ok(_) => {}
    }
    let parent = SessionStore::open(layout, operation.source_session_id.clone())?;
    let mut matched = false;
    for envelope in read_committed_envelopes(&parent)? {
        if let EventKind::SessionForked {
            operation_id,
            child_session_id,
            parent_checkpoint_sequence,
            target_worktree,
            target_branch,
        } = &envelope.event.kind
            && operation_id == &operation.id
        {
            let exact = child_session_id == child_id
                && Some(*parent_checkpoint_sequence) == operation.source_checkpoint_sequence
                && target_worktree == &operation.target_worktree
                && target_branch == &operation.target_branch;
            if !exact || matched {
                return Err(Error::InvalidState(format!(
                    "parent session contains conflicting fork lineage for operation {}",
                    operation.id
                )));
            }
            matched = true;
        }
    }
    Ok(matched)
}

fn read_committed_envelopes(parent: &SessionStore) -> Result<Vec<crate::model::EventEnvelope>> {
    let bytes = crate::store::atomic::read_private(&parent.session_dir().join("events.jsonl"))?;
    let committed = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let mut envelopes = Vec::new();
    for line in bytes[..committed].split_inclusive(|byte| *byte == b'\n') {
        let envelope: crate::model::EventEnvelope = serde_json::from_slice(&line[..line.len() - 1])
            .map_err(|error| {
                Error::InvalidState(format!("invalid parent journal line: {error}"))
            })?;
        envelope.verify()?;
        if envelope.event.session_id != *parent.id()
            || envelope.event.sequence != envelopes.len() as u64 + 1
            || envelope.line()? != line
        {
            return Err(Error::InvalidState(
                "parent journal contains a noncanonical or out-of-sequence event".into(),
            ));
        }
        envelopes.push(envelope);
    }
    Ok(envelopes)
}

fn operation_proof(operation: &ForkOperation) -> Option<MutationProof> {
    Some(MutationProof {
        fingerprint: operation.target_fingerprint.clone()?,
        cleanup_inventory_sha256: operation.target_cleanup_inventory_sha256.clone()?,
    })
}

fn validate_staged_child(layout: &StateLayout, operation: &ForkOperation) -> Result<SessionStore> {
    let child_id = operation.child_session_id.as_ref().ok_or_else(|| {
        Error::InvalidState("fork operation has no staged child session ID".into())
    })?;
    let child = SessionStore::open(layout, child_id.clone())?;
    let checkpoint = operation.source_checkpoint_sequence.ok_or_else(|| {
        Error::InvalidState("fork operation has no parent checkpoint sequence".into())
    })?;
    if child.meta().parent_session_id.as_ref() != Some(&operation.source_session_id)
        || child.meta().parent_checkpoint_sequence != Some(checkpoint)
        || child.meta().worktree.worktree != operation.target_worktree
        || child.meta().worktree.common_git_dir != operation.source_worktree.common_git_dir
    {
        return Err(Error::InvalidState(
            "staged child metadata does not match the fork operation".into(),
        ));
    }
    let binding = layout
        .worktree_refs()
        .join(format!("{}.json", child.meta().worktree.key));
    match std::fs::symlink_metadata(&binding) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(io(&binding, source)),
        Ok(_) => {
            return Err(Error::InvalidState(format!(
                "staged child already has a global worktree ref at {}",
                binding.display()
            )));
        }
    }

    let journal = child.session_dir().join("events.jsonl");
    let journal_bytes = crate::store::atomic::read_private(&journal)?;
    if journal_bytes.last() != Some(&b'\n') {
        return Err(Error::InvalidState(
            "staged child journal has an uncommitted partial tail".into(),
        ));
    }
    let envelopes = read_committed_envelopes(&child)?;
    if envelopes.len() != 2
        || !matches!(
            &envelopes[0].event.kind,
            EventKind::SessionCreated { worktree } if worktree == &child.meta().worktree
        )
        || !matches!(
            &envelopes[1].event.kind,
            EventKind::GitSnapshot { snapshot }
                if snapshot.identity == child.meta().worktree
                    && snapshot.head == operation.target_head
                    && snapshot.branch.as_deref() == Some(operation.target_branch.as_str())
        )
    {
        return Err(Error::InvalidState(
            "staged child journal contains events outside child creation".into(),
        ));
    }
    validate_staged_child_inventory(&child.session_dir())?;

    Ok(child)
}

fn remove_staged_child(layout: &StateLayout, operation: &ForkOperation) -> Result<()> {
    let child = validate_staged_child(layout, operation)?;
    let child_id = operation
        .child_session_id
        .as_ref()
        .expect("validated staged child has an ID");

    let deleting = layout
        .sessions()
        .join(format!(".deleting-{}-{}", child_id, operation.id));
    if std::fs::symlink_metadata(&deleting).is_ok() {
        return Err(Error::InvalidState(format!(
            "staged child deletion path already exists at {}",
            deleting.display()
        )));
    }
    std::fs::rename(child.session_dir(), &deleting)
        .map_err(|source| io(child.session_dir(), source))?;
    sync_directory(&layout.sessions())?;
    std::fs::remove_dir_all(&deleting).map_err(|source| io(&deleting, source))?;
    sync_directory(&layout.sessions())
}

fn validate_staged_child_inventory(root: &Path) -> Result<()> {
    let allowed = BTreeSet::from([
        PathBuf::from("blobs"),
        PathBuf::from("blobs/sha256"),
        PathBuf::from("checkpoints"),
        PathBuf::from("events.jsonl"),
        PathBuf::from("lock"),
        PathBuf::from("meta.json"),
        PathBuf::from("operation.lock"),
        PathBuf::from("refs"),
        PathBuf::from("runs"),
    ]);
    let mut observed = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| io(&directory, source))? {
            let entry = entry.map_err(|source| io(&directory, source))?;
            let absolute = entry.path();
            let relative = absolute
                .strip_prefix(root)
                .map_err(|_| Error::InvalidState("staged child inventory escaped its root".into()))?
                .to_path_buf();
            let metadata =
                std::fs::symlink_metadata(&absolute).map_err(|source| io(&absolute, source))?;
            if metadata.file_type().is_symlink()
                || (!metadata.is_dir() && !metadata.is_file())
                || !allowed.contains(&relative)
            {
                return Err(Error::InvalidState(format!(
                    "unexpected staged child path {}",
                    relative.display()
                )));
            }
            observed.insert(relative);
            if metadata.is_dir() {
                pending.push(absolute);
            }
        }
    }
    let required = allowed
        .iter()
        .filter(|path| path.as_path() != Path::new("operation.lock"))
        .cloned()
        .collect::<BTreeSet<_>>();
    if !required.is_subset(&observed) {
        return Err(Error::InvalidState(
            "staged child inventory is missing an expected path".into(),
        ));
    }
    Ok(())
}

fn branch_exists(operation: &ForkOperation) -> Result<bool> {
    let reference = format!("refs/heads/{}", operation.target_branch);
    Ok(GitCommand
        .optional_text_exit_one(
            &operation.source_worktree.worktree,
            ["rev-parse", "--verify", "--quiet", reference.as_str()],
        )?
        .is_some())
}

fn mark_manual(
    store: &ForkOperationStore,
    operation: &ForkOperation,
    message: String,
) -> Result<ForkOperation> {
    match store.transition(operation.phase, ForkPhase::NeedsManualRecovery, |record| {
        record.error = Some(message.clone())
    }) {
        Ok(_) => Err(Error::InvalidState(message)),
        Err(transition_error) => Err(Error::InvalidState(format!(
            "{message}; cannot record manual recovery: {transition_error}"
        ))),
    }
}

fn validate_transition(expected: ForkPhase, next: ForkPhase) -> Result<()> {
    let allowed = match (expected.ordinal(), next) {
        (Some(current), phase) if phase.ordinal().is_some() => phase.ordinal() == Some(current + 1),
        (Some(current), ForkPhase::RolledBack) => {
            current < ForkPhase::LineageCommitted.ordinal().expect("normal phase")
        }
        (Some(_), ForkPhase::NeedsManualRecovery) => true,
        (None, phase) if phase.ordinal().is_some() => expected == ForkPhase::NeedsManualRecovery,
        _ => false,
    };
    if !allowed {
        return Err(Error::InvalidState(format!(
            "invalid fork operation transition {expected:?} -> {next:?}"
        )));
    }
    Ok(())
}

fn validate_private_operation_dir(path: &Path, id: &OperationId) -> Result<()> {
    if path.file_name().and_then(|name| name.to_str()) != Some(&id.to_string()) {
        return Err(Error::InvalidState(
            "fork operation directory basename does not match its ID".into(),
        ));
    }
    validate_private_directory(path, "fork operation directory")
}

fn validate_private_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io(path, source))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(Error::InvalidState(format!(
            "refusing insecure {label} {}",
            path.display()
        )));
    }
    Ok(())
}

pub fn capture_fork_artifacts(
    store: &ForkOperationStore,
    source_cwd: &Path,
    boundary: impl FnOnce() -> Result<()>,
) -> Result<ForkOperation> {
    let operation = store.read_current()?;
    if operation.phase != ForkPhase::Prepared {
        return Err(Error::InvalidState(format!(
            "fork artifact capture requires prepared phase, found {:?}",
            operation.phase
        )));
    }
    let observed = Git::new().snapshot(source_cwd)?;
    if !observed
        .identity
        .same_worktree_as(&operation.source_worktree)
    {
        return rollback_capture(
            store,
            "fork capture source does not match the operation worktree".into(),
        );
    }

    let command = GitCommand;
    let captured = match capture(&command, source_cwd).and_then(|captured| {
        write_capture_artifacts(store, &captured)?;
        boundary()?;
        Ok(captured)
    }) {
        Ok(captured) => captured,
        Err(error) => return rollback_capture(store, error.to_string()),
    };
    let fresh = match capture(&command, source_cwd) {
        Ok(fresh) => fresh,
        Err(error) => return rollback_capture(store, error.to_string()),
    };
    if fresh.fingerprint != captured.fingerprint {
        return rollback_capture(store, "source changed during fork capture".into());
    }

    store.transition(
        ForkPhase::Prepared,
        ForkPhase::ArtifactsCaptured,
        |operation| {
            operation.source_fingerprint = Some(captured.fingerprint);
        },
    )
}

fn write_capture_artifacts(store: &ForkOperationStore, captured: &CapturedForkState) -> Result<()> {
    for entry in &captured.untracked_manifest {
        entry.validate()?;
    }
    let directory = store.operation_dir();
    create_private(&directory.join("staged.patch"), &captured.staged_patch)?;
    create_private(&directory.join("unstaged.patch"), &captured.unstaged_patch)?;
    create_private(
        &directory.join("untracked/manifest.json"),
        &captured.untracked_manifest_json,
    )?;
    for (hash, bytes) in &captured.untracked_blobs {
        let artifact = directory
            .join("untracked/blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..]);
        create_private(&artifact, bytes)?;
    }
    create_private(
        &directory.join("submodules.json"),
        &captured.submodule_manifest_json,
    )?;
    sync_directory(&directory)
}

fn rollback_capture(store: &ForkOperationStore, message: String) -> Result<ForkOperation> {
    let transition = store.transition(ForkPhase::Prepared, ForkPhase::RolledBack, |operation| {
        operation.error = Some(message.clone());
    });
    match transition {
        Ok(_) => Err(Error::InvalidState(message)),
        Err(transition_error) => Err(Error::InvalidState(format!(
            "{message}; cannot record capture rollback: {transition_error}"
        ))),
    }
}
