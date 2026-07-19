use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result, io};
use crate::git::Git;
use crate::git::command::GitCommand;
use crate::git::fingerprint::{CapturedForkState, capture};
use crate::model::{ForkOperation, ForkPhase, OperationId};
use crate::store::StateLayout;
use crate::store::atomic::{create_private, sync_directory};
use crate::store::lease::SessionOperationLock;
use crate::store::refs::{read_json, write_json, write_json_create};

#[derive(Clone, Debug)]
pub struct ForkOperationStore {
    layout: StateLayout,
    id: OperationId,
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
        validate_private_directory(layout.root(), "state root")?;
        validate_private_directory(&layout.operations(), "operations root")?;
        let layout = layout.canonicalized()?;
        let store = Self { layout, id };
        store.read_current()
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
