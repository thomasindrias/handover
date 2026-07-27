use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::Serialize;

use crate::checkpoint::load_verified_checkpoint;
use crate::error::{Error, Result, io};
use crate::fork::{ForkOperationStore, lineage_commit_evidence, recover_committed_fork};
use crate::git::fork::{MutationProof, observe_target_proof};
use crate::model::{
    CheckpointKind, EventEnvelope, EventKind, ForkOperation, ForkPhase, OperationId, Provider,
    SessionId,
};
use crate::provider::adapter;
use crate::store::StateLayout;
use crate::store::atomic::{create_private, read_private, sync_directory};
use crate::store::lease::{LeaseStore, SessionOperationLock, host_name};
use crate::store::refs::write_json;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_argv: Option<Vec<String>>,
}

impl Diagnostic {
    fn error(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity: "error".into(),
            message: message.into(),
            repair_command: None,
            command: None,
            command_argv: None,
        }
    }

    fn warning(code: &str, message: impl Into<String>, repairable: bool) -> Self {
        Self {
            code: code.into(),
            severity: "warning".into(),
            message: message.into(),
            repair_command: repairable.then(|| "sesh doctor --repair".into()),
            command: None,
            command_argv: None,
        }
    }

    fn repaired(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity: "repaired".into(),
            message: message.into(),
            repair_command: None,
            command: None,
            command_argv: None,
        }
    }
}

pub fn check_format(layout: &StateLayout) -> Vec<Diagnostic> {
    let path = layout.root().join("FORMAT");
    match read_private(&path) {
        Ok(bytes) if bytes == b"sesh-state 1\n" => Vec::new(),
        Ok(_) => vec![Diagnostic::error(
            "format.unsupported",
            format!("{} does not contain `sesh-state 1`", path.display()),
        )],
        Err(error) => vec![Diagnostic::error(
            "format.unavailable",
            format!("cannot read {}: {error}", path.display()),
        )],
    }
}

pub fn check_permissions(layout: &StateLayout) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let root = layout.root();
    check_permission_path(root, root, &mut diagnostics);
    diagnostics
}

/// A provider's private home holds files the provider writes with its own
/// permissions, so Sesh guarantees the `0700` container and stops there.
fn is_provider_owned_home(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let Some(parts) = relative
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    matches!(
        parts.as_slice(),
        ["sessions", _, "runs", _, "codex_home"] | ["integrations", "codex", _, "review"]
    )
}

fn check_permission_path(root: &Path, path: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                "permissions.unavailable",
                format!("cannot inspect {}: {error}", path.display()),
            ));
            return;
        }
    };
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    // Provider adapters deliberately link private files into the state root, so a
    // symlink is judged by its target and never followed into a directory.
    if metadata.file_type().is_symlink() {
        let target = match std::fs::metadata(path) {
            Ok(target) => target,
            Err(error) => {
                diagnostics.push(Diagnostic::error(
                    "permissions.unavailable",
                    format!("cannot resolve {}: {error}", path.display()),
                ));
                return;
            }
        };
        if !target.file_type().is_file()
            || target.uid() != effective_uid
            || target.permissions().mode() & 0o777 != 0o600
        {
            let resolved = std::fs::read_link(path).unwrap_or_else(|_| path.to_path_buf());
            diagnostics.push(Diagnostic::error(
                "permissions.insecure",
                format!(
                    "{} links to {}, which must be a regular file owned by the current user with mode 0600",
                    path.display(),
                    resolved.display()
                ),
            ));
        }
        return;
    }
    let mode = metadata.permissions().mode() & 0o777;
    let expected = if metadata.is_dir() { 0o700 } else { 0o600 };
    if metadata.uid() != effective_uid || mode != expected {
        diagnostics.push(Diagnostic::error(
            "permissions.insecure",
            format!(
                "{} must be owned by the current user with mode {expected:04o}",
                path.display()
            ),
        ));
        return;
    }
    if metadata.is_dir() {
        if is_provider_owned_home(root, path) {
            return;
        }
        match std::fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => check_permission_path(root, &entry.path(), diagnostics),
                        Err(error) => diagnostics.push(Diagnostic::error(
                            "permissions.unavailable",
                            format!("cannot inspect {}: {error}", path.display()),
                        )),
                    }
                }
            }
            Err(error) => diagnostics.push(Diagnostic::error(
                "permissions.unavailable",
                format!("cannot list {}: {error}", path.display()),
            )),
        }
    }
}

pub fn check_git(cwd: &Path) -> Vec<Diagnostic> {
    let version = std::process::Command::new("git").arg("--version").output();
    match version {
        Err(error) => {
            return vec![Diagnostic::error(
                "git.missing",
                format!("cannot execute git: {error}"),
            )];
        }
        Ok(output) if !output.status.success() => {
            return vec![Diagnostic::error("git.unusable", "git --version failed")];
        }
        Ok(_) => {}
    }
    match std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
    {
        Ok(output) if output.status.success() && output.stdout.starts_with(b"true") => Vec::new(),
        Ok(output) => vec![Diagnostic::error(
            "git.not_worktree",
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        )],
        Err(error) => vec![Diagnostic::error(
            "git.unusable",
            format!("cannot inspect worktree: {error}"),
        )],
    }
}

pub fn check_provider(provider: Provider) -> Vec<Diagnostic> {
    let executable = provider.executable();
    let help = match std::process::Command::new(executable)
        .arg("--help")
        .output()
    {
        Err(error) => {
            return vec![Diagnostic::error(
                "provider.missing",
                format!("cannot execute {executable}: {error}"),
            )];
        }
        Ok(output) if !output.status.success() => {
            return vec![Diagnostic::error(
                "provider.help_failed",
                format!("{executable} --help exited with {}", output.status),
            )];
        }
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
    };
    let required: &[&str] = match provider {
        Provider::Claude => &["--plugin-dir", "--add-dir"],
        Provider::Codex => &["--config", "--add-dir", "--cd"],
    };
    let mut diagnostics = Vec::new();
    for flag in required {
        if !help.contains(flag) {
            diagnostics.push(Diagnostic::error(
                "provider.capability_missing",
                format!("{executable} --help does not advertise {flag}"),
            ));
        }
    }
    if provider == Provider::Codex {
        match std::process::Command::new(executable)
            .args(["features", "list"])
            .output()
        {
            Ok(output)
                if output.status.success()
                    && String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                        let fields: Vec<_> = line.split_whitespace().collect();
                        fields == ["hooks", "stable", "true"]
                    }) => {}
            Ok(_) => diagnostics.push(Diagnostic::error(
                "codex.hooks_unstable",
                "codex features list does not report `hooks stable true`",
            )),
            Err(error) => diagnostics.push(Diagnostic::error(
                "codex.features_failed",
                format!("cannot inspect Codex features: {error}"),
            )),
        }
    }
    diagnostics
}

pub fn check_integrations(layout: &StateLayout) -> Vec<Diagnostic> {
    [Provider::Claude, Provider::Codex]
        .into_iter()
        .filter_map(|provider| {
            let integrations = layout.integrations();
            adapter(provider).verify(&integrations).err().map(|error| {
                let executable = provider.executable();
                // Never running setup is the ordinary first-run state, not corruption.
                if !integrations.join(executable).exists() {
                    let setup = format!("sesh setup {executable}");
                    return Diagnostic {
                        code: "integration.missing".into(),
                        severity: "error".into(),
                        message: format!("{executable} integration is not set up; run `{setup}`"),
                        repair_command: Some(setup),
                        command: None,
                        command_argv: None,
                    };
                }
                Diagnostic::error(
                    "integration.invalid",
                    format!("{executable} integration: {error}"),
                )
            })
        })
        .collect()
}

pub fn check_sessions(layout: &StateLayout) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let entries = match std::fs::read_dir(layout.sessions()) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                "sessions.unavailable",
                format!("cannot list sessions: {error}"),
            ));
            return diagnostics;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            diagnostics.push(Diagnostic::error(
                "session.invalid_name",
                format!("invalid session path {}", path.display()),
            ));
            continue;
        };
        let Ok(id) = SessionId::parse(name) else {
            diagnostics.push(Diagnostic::error(
                "session.invalid_name",
                format!("invalid session directory {name:?}"),
            ));
            continue;
        };
        let scan = scan_journal(&path.join("events.jsonl"), &id);
        match scan {
            Ok(scan) => {
                if scan.partial_bytes > 0 {
                    diagnostics.push(Diagnostic::warning(
                        "journal.partial_tail",
                        format!(
                            "session {id} has {} uncommitted final journal bytes",
                            scan.partial_bytes
                        ),
                        true,
                    ));
                }
                check_handshake_timeout(&scan.envelopes, &mut diagnostics);
                check_orphan_artifacts(&path, &scan.envelopes, &mut diagnostics);
            }
            Err(error) => diagnostics.push(Diagnostic::error(
                "journal.corrupt",
                format!("session {id}: {error}"),
            )),
        }
        match LeaseStore::new(&path).read() {
            Ok(Some(lease)) => {
                if lease.host != host_name().unwrap_or_default() {
                    diagnostics.push(Diagnostic::error(
                        "lease.foreign_host",
                        format!("run {} belongs to host {}", lease.run_id, lease.host),
                    ));
                } else {
                    let supervisor_live = lease.supervisor.is_live().unwrap_or(false);
                    let child_live = lease
                        .child
                        .as_ref()
                        .and_then(|child| child.is_live().ok())
                        .unwrap_or(false);
                    let (code, severity) = match (supervisor_live, child_live) {
                        (false, false) => ("lease.dead", "warning"),
                        (false, true) => ("lease.live_orphan_child", "error"),
                        _ => ("lease.live", "info"),
                    };
                    diagnostics.push(Diagnostic {
                        code: code.into(),
                        severity: severity.into(),
                        message: format!("run {} lease is present", lease.run_id),
                        repair_command: None,
                        command: None,
                        command_argv: None,
                    });
                }
            }
            Ok(None) => {}
            Err(error) => diagnostics.push(Diagnostic::error("lease.invalid", error.to_string())),
        }
    }
    diagnostics
}

pub fn repair(layout: &StateLayout) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let Ok(entries) = std::fs::read_dir(layout.sessions()) else {
        return diagnostics;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(id) = SessionId::parse(name) else {
            continue;
        };
        let Ok(_operation) = SessionOperationLock::acquire(&path) else {
            continue;
        };
        let journal = path.join("events.jsonl");
        if let Ok(scan) = scan_journal(&journal, &id)
            && scan.partial_bytes > 0
        {
            let committed = scan.committed_bytes;
            let result = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&journal)
                .and_then(|file| {
                    file.set_len(committed as u64)?;
                    file.sync_all()
                });
            match result {
                Ok(()) => diagnostics.push(Diagnostic::repaired(
                    "journal.tail_repaired",
                    format!(
                        "removed {} final bytes from session {id}",
                        scan.partial_bytes
                    ),
                )),
                Err(error) => diagnostics.push(Diagnostic::error(
                    "journal.repair_failed",
                    format!("cannot repair session {id}: {error}"),
                )),
            }
        }
        let scan = match scan_journal(&journal, &id) {
            Ok(scan) => scan,
            Err(_) => continue,
        };
        rebuild_checkpoint_refs(&path, &scan.envelopes, &mut diagnostics);
        repair_capture_sentinels(&path, &mut diagnostics);
    }
    diagnostics.extend(repair_forks(layout));
    diagnostics
}

pub fn check_forks(layout: &StateLayout) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let entries = match std::fs::read_dir(layout.operations()) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                "fork_record_corrupt",
                format!("cannot list fork operations: {error}"),
            ));
            return diagnostics;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                diagnostics.push(Diagnostic::error(
                    "fork_record_corrupt",
                    format!("cannot inspect a fork operation entry: {error}"),
                ));
                continue;
            }
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            diagnostics.push(Diagnostic::error(
                "fork_record_corrupt",
                format!(
                    "fork operation path {} is not UTF-8",
                    entry.path().display()
                ),
            ));
            continue;
        };
        let id = match OperationId::parse(&name) {
            Ok(id) => id,
            Err(error) => {
                diagnostics.push(Diagnostic::error(
                    "fork_record_corrupt",
                    format!("invalid fork operation directory {name:?}: {error}"),
                ));
                continue;
            }
        };
        let operation = match ForkOperationStore::read(layout, id) {
            Ok(operation) => operation,
            Err(error) => {
                diagnostics.push(Diagnostic::error(
                    "fork_record_corrupt",
                    format!("fork operation {name} is invalid: {error}"),
                ));
                continue;
            }
        };
        if matches!(operation.phase, ForkPhase::Complete | ForkPhase::RolledBack) {
            continue;
        }
        let committed = match lineage_commit_evidence(layout, &operation) {
            Ok(committed) => committed,
            Err(error) => {
                diagnostics.push(fork_diagnostic(
                    "fork_record_corrupt",
                    "error",
                    &operation,
                    format!("parent lineage evidence is invalid: {error}"),
                    false,
                ));
                continue;
            }
        };
        if matches!(
            operation.phase,
            ForkPhase::LineageCommitted | ForkPhase::ChildBound | ForkPhase::RunLeased
        ) && !committed
        {
            diagnostics.push(fork_diagnostic(
                "fork_record_corrupt",
                "error",
                &operation,
                "phase claims committed lineage but the parent event is absent".into(),
                false,
            ));
            continue;
        }

        let target = observe_target_proof(&operation);
        let expected = operation_target_proof(&operation);
        let changed = match (&target, &expected) {
            (Ok(Some(fresh)), Some(expected)) => fresh != expected,
            (Ok(None), _) => operation.target_created,
            (Err(_), _) => true,
            (Ok(Some(_)), None) => false,
        };
        if changed || operation.phase == ForkPhase::NeedsManualRecovery {
            let detail = match target {
                Err(error) => format!("target cannot be proven: {error}"),
                Ok(None) => "recorded target worktree is absent".into(),
                Ok(Some(_)) => "target differs from its recorded fingerprint or inventory".into(),
            };
            diagnostics.push(fork_diagnostic(
                "fork_target_changed",
                "error",
                &operation,
                detail,
                false,
            ));
        } else if committed {
            diagnostics.push(fork_diagnostic(
                "fork_postcommit_incomplete",
                "warning",
                &operation,
                "parent lineage is committed; forward repair is available".into(),
                true,
            ));
        } else if operation.target_created || matches!(target, Ok(Some(_))) {
            let detail = if operation.target_created {
                "lineage is uncommitted; the target still matches the last durable boundary"
            } else {
                "lineage is uncommitted and the target has no crash-durable mutation proof"
            };
            diagnostics.push(fork_diagnostic(
                "fork_precommit_crash",
                "warning",
                &operation,
                detail.into(),
                false,
            ));
        } else {
            diagnostics.push(fork_diagnostic(
                "fork_in_progress",
                "warning",
                &operation,
                "operation has not created a target and may still be active".into(),
                false,
            ));
        }
    }
    diagnostics
}

fn repair_forks(layout: &StateLayout) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let Ok(entries) = std::fs::read_dir(layout.operations()) else {
        return diagnostics;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(id) = OperationId::parse(&name) else {
            continue;
        };
        let Ok(store) = ForkOperationStore::open(layout, id) else {
            continue;
        };
        let Ok(before) = store.operation() else {
            continue;
        };
        if !lineage_commit_evidence(layout, &before).unwrap_or(false)
            || matches!(before.phase, ForkPhase::Complete | ForkPhase::RolledBack)
        {
            continue;
        }
        match recover_committed_fork(&store) {
            Ok(after) => diagnostics.push(Diagnostic::repaired(
                "fork.forward_repaired",
                format!(
                    "advanced fork operation {} from {:?} to {:?} without changing its target",
                    after.id, before.phase, after.phase
                ),
            )),
            Err(error) => diagnostics.push(fork_diagnostic(
                "fork_target_changed",
                "error",
                &before,
                format!("forward repair stopped: {error}"),
                false,
            )),
        }
    }
    diagnostics
}

fn operation_target_proof(operation: &ForkOperation) -> Option<MutationProof> {
    Some(MutationProof {
        fingerprint: operation.target_fingerprint.clone()?,
        cleanup_inventory_sha256: operation.target_cleanup_inventory_sha256.clone()?,
    })
}

fn fork_diagnostic(
    code: &str,
    severity: &str,
    operation: &ForkOperation,
    detail: String,
    repairable: bool,
) -> Diagnostic {
    let inspect_root = if std::fs::symlink_metadata(&operation.target_worktree).is_ok() {
        &operation.target_worktree
    } else {
        &operation.source_worktree.worktree
    };
    let argv = vec![
        "git".to_owned(),
        "-C".to_owned(),
        inspect_root.to_string_lossy().into_owned(),
        "status".to_owned(),
        "--short".to_owned(),
        "--branch".to_owned(),
        "--untracked-files=all".to_owned(),
    ];
    let display = argv
        .iter()
        .map(|argument| shell_words::quote(argument).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    Diagnostic {
        code: code.into(),
        severity: severity.into(),
        message: format!(
            "fork {} phase {:?}; source session {}; target {}; branch {:?}: {detail}; inspect with {display}",
            operation.id,
            operation.phase,
            operation.source_session_id,
            operation.target_worktree.display(),
            operation.target_branch,
        ),
        repair_command: repairable.then(|| "sesh doctor --repair".into()),
        command: Some(display),
        command_argv: Some(argv),
    }
}

struct JournalScan {
    envelopes: Vec<EventEnvelope>,
    committed_bytes: usize,
    partial_bytes: usize,
}

fn scan_journal(path: &Path, expected_session: &SessionId) -> Result<JournalScan> {
    let bytes = read_private(path)?;
    let committed_bytes = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let mut envelopes = Vec::new();
    let mut offset = 0usize;
    for line in bytes[..committed_bytes].split_inclusive(|byte| *byte == b'\n') {
        let envelope: EventEnvelope = serde_json::from_slice(&line[..line.len() - 1])
            .map_err(|error| Error::InvalidState(format!("invalid journal line: {error}")))?;
        envelope.verify()?;
        if envelope.event.session_id != *expected_session
            || envelope.event.sequence != envelopes.len() as u64 + 1
            || envelope.line()? != line
        {
            return Err(Error::InvalidState(
                "journal contains a noncanonical or out-of-sequence event".into(),
            ));
        }
        offset += line.len();
        envelopes.push(envelope);
    }
    Ok(JournalScan {
        envelopes,
        committed_bytes: offset,
        partial_bytes: bytes.len() - committed_bytes,
    })
}

fn check_handshake_timeout(envelopes: &[EventEnvelope], diagnostics: &mut Vec<Diagnostic>) {
    let Some(started) = envelopes
        .iter()
        .rev()
        .find(|item| matches!(item.event.kind, EventKind::RunStarted { .. }))
    else {
        return;
    };
    let run_id = started.event.run_id.as_ref();
    let handshook = envelopes.iter().any(|item| {
        item.event.run_id.as_ref() == run_id
            && matches!(item.event.kind, EventKind::RunHandshake { .. })
    });
    let stopped = envelopes.iter().any(|item| {
        item.event.run_id.as_ref() == run_id
            && matches!(item.event.kind, EventKind::RunStopped { .. })
    });
    if !handshook && stopped {
        diagnostics.push(Diagnostic::error(
            "run.session_start_timeout",
            format!("run {run_id:?} stopped without a SessionStart handshake"),
        ));
    }
}

fn check_orphan_artifacts(
    session_dir: &Path,
    envelopes: &[EventEnvelope],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let committed: std::collections::BTreeSet<_> = envelopes
        .iter()
        .filter_map(|item| {
            matches!(item.event.kind, EventKind::CheckpointCreated { .. })
                .then_some(item.event.sequence)
        })
        .collect();
    if let Ok(entries) = std::fs::read_dir(session_dir.join("checkpoints")) {
        for entry in entries.flatten() {
            let path = entry.path();
            let sequence = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<u64>().ok());
            if sequence.is_none_or(|sequence| !committed.contains(&sequence)) {
                diagnostics.push(Diagnostic::warning(
                    "checkpoint.orphan_artifact",
                    format!("orphan checkpoint artifact {}", path.display()),
                    false,
                ));
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(session_dir.join("runs")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name.ends_with(".tmp") {
                diagnostics.push(Diagnostic::warning(
                    "temporary.orphan",
                    format!("orphan temporary path {}", entry.path().display()),
                    false,
                ));
            }
        }
    }
}

fn rebuild_checkpoint_refs(
    session_dir: &Path,
    envelopes: &[EventEnvelope],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut latest = None;
    let mut narrative = None;
    for envelope in envelopes {
        if let EventKind::CheckpointCreated {
            checkpoint_kind, ..
        } = &envelope.event.kind
        {
            if load_verified_checkpoint(session_dir, envelope.event.sequence).is_err() {
                diagnostics.push(Diagnostic::error(
                    "checkpoint.invalid",
                    format!("checkpoint {} is invalid", envelope.event.sequence),
                ));
                return;
            }
            latest = Some(envelope.event.sequence);
            if checkpoint_kind == &CheckpointKind::Narrative {
                narrative = Some(envelope.event.sequence);
            }
        }
    }
    for (name, value) in [
        ("latest-checkpoint", latest),
        ("latest-narrative-checkpoint", narrative),
    ] {
        let path = session_dir.join("refs").join(name);
        match value {
            Some(value) => {
                let current = crate::store::refs::read_json::<u64>(&path).ok();
                if current != Some(value) {
                    match write_json(&path, &value) {
                        Ok(()) => diagnostics.push(Diagnostic::repaired(
                            "checkpoint.ref_rebuilt",
                            format!("rebuilt {name} as event {value}"),
                        )),
                        Err(error) => diagnostics.push(Diagnostic::error(
                            "checkpoint.ref_repair_failed",
                            format!("cannot rebuild {name}: {error}"),
                        )),
                    }
                }
            }
            None if std::fs::symlink_metadata(&path).is_ok() => {
                match std::fs::remove_file(&path)
                    .map_err(|source| io(&path, source))
                    .and_then(|()| sync_directory(&session_dir.join("refs")))
                {
                    Ok(()) => diagnostics.push(Diagnostic::repaired(
                        "checkpoint.ref_removed",
                        format!("removed stale {name}"),
                    )),
                    Err(error) => diagnostics.push(Diagnostic::error(
                        "checkpoint.ref_repair_failed",
                        format!("cannot remove {name}: {error}"),
                    )),
                }
            }
            None => {}
        }
    }
}

fn repair_capture_sentinels(session_dir: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let runs = session_dir.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs) else {
        return;
    };
    for entry in entries.flatten() {
        let run_dir = entry.path();
        let sentinel = run_dir.join("capture-failed.json");
        if std::fs::symlink_metadata(&sentinel).is_err() {
            continue;
        }
        let probe = session_dir.join(format!(".doctor-probe-{}", uuid::Uuid::new_v4()));
        let probed = create_private(&probe, b"probe\n")
            .and_then(|()| std::fs::remove_file(&probe).map_err(|source| io(&probe, source)))
            .and_then(|()| sync_directory(session_dir));
        if probed.is_ok()
            && std::fs::remove_file(&sentinel).is_ok()
            && sync_directory(&run_dir).is_ok()
        {
            diagnostics.push(Diagnostic::repaired(
                "capture.sentinel_removed",
                format!(
                    "removed {} after a successful private write probe",
                    sentinel.display()
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::{check_integrations, check_permissions, check_sessions};
    use crate::model::{EventKind, GitSnapshot, Provider, RunId, SessionId, WorktreeIdentity};
    use crate::runtime::Runtime;
    use crate::store::lease::{LeaseStore, ProcessIdentity, RunLease};
    use crate::store::{SessionStore, StateLayout};

    struct FixedRuntime;

    impl Runtime for FixedRuntime {
        fn now(&self) -> crate::error::Result<String> {
            Ok("2026-07-19T10:00:00Z".into())
        }

        fn session_id(&self) -> SessionId {
            SessionId::new()
        }

        fn run_id(&self) -> RunId {
            RunId::new()
        }

        fn operation_id(&self) -> crate::model::OperationId {
            crate::model::OperationId::parse("33333333-3333-4333-8333-333333333333").unwrap()
        }
    }

    #[test]
    fn sessions_distinguish_dead_foreign_and_live_orphan_leases_and_missing_handshake() {
        let (_temp, layout, store) = fixture();
        let run_id = RunId::new();
        store
            .append(
                &FixedRuntime,
                Some(run_id.clone()),
                Some(Provider::Claude),
                EventKind::RunStarted {
                    cwd: "/repo".into(),
                    args: Vec::new(),
                    supervisor_pid: u32::MAX,
                },
            )
            .unwrap();
        store
            .append(
                &FixedRuntime,
                Some(run_id),
                Some(Provider::Claude),
                EventKind::RunStopped {
                    exit_code: None,
                    signal: Some(libc::SIGKILL),
                },
            )
            .unwrap();
        assert!(
            check_sessions(&layout)
                .iter()
                .any(|item| item.code == "run.session_start_timeout")
        );

        let leases = LeaseStore::new(&store.session_dir());
        let dead = RunLease::new(
            store.id().clone(),
            RunId::new(),
            Provider::Claude,
            ProcessIdentity {
                pid: u32::MAX,
                start_token: "gone".into(),
            },
        )
        .unwrap();
        leases.create(&dead).unwrap();
        assert!(
            check_sessions(&layout)
                .iter()
                .any(|item| item.code == "lease.dead")
        );
        leases.clear(&dead.run_id).unwrap();

        let mut foreign = dead.clone();
        foreign.run_id = RunId::new();
        foreign.host = "foreign-host".into();
        leases.create(&foreign).unwrap();
        assert!(
            check_sessions(&layout)
                .iter()
                .any(|item| item.code == "lease.foreign_host")
        );
        leases.clear(&foreign.run_id).unwrap();

        let mut orphan = dead;
        orphan.run_id = RunId::new();
        orphan.child = Some(ProcessIdentity::capture(std::process::id()).unwrap());
        leases.create(&orphan).unwrap();
        assert!(
            check_sessions(&layout)
                .iter()
                .any(|item| item.code == "lease.live_orphan_child")
        );
    }

    #[test]
    fn integrations_name_the_setup_command_when_a_provider_was_never_set_up() {
        let (_temp, layout, _store) = fixture();
        crate::provider::adapter(Provider::Codex)
            .setup(&layout.integrations())
            .unwrap();

        let diagnostics = check_integrations(&layout);
        let missing = diagnostics
            .iter()
            .find(|item| item.code == "integration.missing")
            .expect("claude was never set up");
        assert!(missing.message.contains("sesh setup claude"));
        assert_eq!(missing.repair_command.as_deref(), Some("sesh setup claude"));
        assert!(
            !diagnostics
                .iter()
                .any(|item| item.message.starts_with("codex integration")),
            "a provider that was set up is healthy"
        );
    }

    #[test]
    fn permissions_accept_private_symlink_targets_and_reject_exposed_ones() {
        let (temp, layout, _store) = fixture();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        let private_target = outside.join("auth.json");
        std::fs::write(&private_target, b"{}").unwrap();
        std::fs::set_permissions(&private_target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = layout.root().join("auth.json");
        std::os::unix::fs::symlink(&private_target, &link).unwrap();
        assert!(
            check_permissions(&layout).is_empty(),
            "a symlink to a user-owned 0600 file is how the Codex adapter wires CODEX_HOME"
        );

        std::fs::set_permissions(&private_target, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            check_permissions(&layout)
                .iter()
                .any(|item| item.code == "permissions.insecure"),
            "a symlink to a world-readable file must still fail closed"
        );
        std::fs::remove_file(&link).unwrap();

        let directory_target = outside.join("nested");
        std::fs::create_dir(&directory_target).unwrap();
        std::os::unix::fs::symlink(&directory_target, layout.root().join("nested")).unwrap();
        assert!(
            check_permissions(&layout)
                .iter()
                .any(|item| item.code == "permissions.insecure"),
            "only regular files may be linked, so traversal cannot escape the state root"
        );
    }

    #[test]
    fn permissions_accept_a_materialized_codex_home_under_the_state_root() {
        let (temp, layout, store) = fixture();
        crate::provider::adapter(Provider::Codex)
            .setup(&layout.integrations())
            .unwrap();
        let provider_home = temp.path().join("dot-codex");
        std::fs::create_dir(&provider_home).unwrap();
        for name in ["config.toml", "auth.json"] {
            let file = provider_home.join(name);
            std::fs::write(&file, b"x").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        crate::provider::codex::materialize_codex_home(
            &store.session_dir().join("runs/run-1/codex_home"),
            &layout.integrations().join("codex/1/hooks.json"),
            Some(&provider_home),
        )
        .unwrap();

        assert_eq!(
            check_permissions(&layout),
            Vec::new(),
            "a Codex run must not leave `sesh doctor` reporting its own state as insecure"
        );
    }

    #[test]
    fn permissions_leave_the_contents_of_a_provider_owned_home_to_the_provider() {
        let (_temp, layout, store) = fixture();
        let codex_home = store.session_dir().join("runs/run-1/codex_home");
        crate::store::ensure_private_dir(&codex_home).unwrap();
        // Real Codex writes these itself, with its own permissions.
        let database = codex_home.join("state_5.sqlite");
        std::fs::write(&database, b"x").unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o644)).unwrap();
        let scratch = codex_home.join("tmp");
        std::fs::create_dir(&scratch).unwrap();
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            check_permissions(&layout),
            Vec::new(),
            "Sesh guarantees the 0700 container, not files the provider writes inside it"
        );

        std::fs::set_permissions(&codex_home, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            check_permissions(&layout)
                .iter()
                .any(|item| item.code == "permissions.insecure"),
            "the container itself must still be private"
        );
    }

    #[test]
    fn permissions_reject_a_dangling_symlink() {
        let (temp, layout, _store) = fixture();
        std::os::unix::fs::symlink(
            temp.path().join("missing"),
            layout.root().join("dangling.json"),
        )
        .unwrap();
        assert!(
            check_permissions(&layout)
                .iter()
                .any(|item| item.code == "permissions.unavailable")
        );
    }

    fn fixture() -> (TempDir, StateLayout, SessionStore) {
        let temp = TempDir::new().unwrap();
        let worktree = temp.path().join("repo");
        let git_dir = worktree.join(".git");
        let snapshot = GitSnapshot {
            identity: WorktreeIdentity {
                key: WorktreeIdentity::derive_key(&git_dir, &git_dir),
                common_git_dir: git_dir.clone(),
                git_dir,
                worktree,
                cwd_relative: PathBuf::new(),
            },
            branch: Some("main".into()),
            head: "deadbeef".into(),
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
            dirty_submodules: Vec::new(),
        };
        let layout = StateLayout::new(temp.path().join("state"));
        let store = SessionStore::create(&layout, &FixedRuntime, snapshot).unwrap();
        (temp, layout, store)
    }
}
