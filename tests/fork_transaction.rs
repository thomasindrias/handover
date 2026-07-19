mod support;

use std::os::unix::fs::{PermissionsExt, symlink};

use sesh::error::Error;
use sesh::fork::{
    ForkOperationStore, StagedChildProof, capture_fork_artifacts, recover_fork_failure,
    recover_fork_failure_with_live_child,
};
use sesh::git::Git;
use sesh::git::fork::{materialize, observe_target_proof};
use sesh::model::{
    EventKind, ForkOperation, ForkPhase, OperationId, RunId, SessionId, UntrackedEntry,
};
use sesh::runtime::Runtime;
use sesh::store::{SessionStore, StateLayout};
use support::{git, init_repo, repository_fingerprint};
use tempfile::TempDir;

#[test]
fn operation_record_round_trips_and_rejects_unknown_schema() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let layout = StateLayout::new(temp.path().join("state"));
    let operation = prepared_operation(&repo, temp.path().join("target"));

    let store = ForkOperationStore::create(&layout, &operation).unwrap();
    assert_eq!(
        ForkOperationStore::read(&layout, operation.id.clone()).unwrap(),
        operation
    );
    let record_path = store.operation_dir().join("operation.json");
    let unchanged = std::fs::read(&record_path).unwrap();
    assert!(
        store
            .transition(
                ForkPhase::ArtifactsCaptured,
                ForkPhase::WorktreeCreated,
                |_| {}
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&record_path).unwrap(), unchanged);
    assert!(
        store
            .transition(ForkPhase::Prepared, ForkPhase::ArtifactsCaptured, |_| {})
            .is_err()
    );
    assert_eq!(std::fs::read(&record_path).unwrap(), unchanged);

    let mut unsupported = operation.clone();
    unsupported.schema_version = 2;
    sesh::store::refs::write_json(
        &layout
            .operations()
            .join(unsupported.id.to_string())
            .join("operation.json"),
        &unsupported,
    )
    .unwrap();
    assert!(ForkOperationStore::read(&layout, unsupported.id).is_err());
}

#[test]
fn capture_rolls_back_if_source_changes_at_the_artifact_boundary() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("README.md"), "staged\n").unwrap();
    git(&repo, &["add", "README.md"]);
    std::fs::write(repo.join("README.md"), "unstaged\n").unwrap();

    let target = temp.path().join("target");
    let layout = StateLayout::new(temp.path().join("state"));
    let operation = prepared_operation(&repo, target.clone());
    let store = ForkOperationStore::create(&layout, &operation).unwrap();

    let error = capture_fork_artifacts(&store, &repo, || {
        std::fs::write(repo.join("README.md"), "changed during capture\n").unwrap();
        Ok(())
    })
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("source changed during fork capture")
    );
    assert!(!target.exists());
    let retained = ForkOperationStore::read(&layout, operation.id).unwrap();
    assert_eq!(retained.phase, ForkPhase::RolledBack);
    assert!(!retained.target_created);
    assert_eq!(
        retained.error.as_deref(),
        Some("source changed during fork capture")
    );
}

#[test]
fn rollback_removes_only_targets_matching_each_durable_precommit_phase() {
    for phase in [
        ForkPhase::Prepared,
        ForkPhase::ArtifactsCaptured,
        ForkPhase::WorktreeCreated,
        ForkPhase::StagedApplied,
        ForkPhase::UnstagedApplied,
        ForkPhase::UntrackedCopied,
        ForkPhase::Verified,
        ForkPhase::ChildStaged,
    ] {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        prepare_capture_matrix(&repo);
        let source_before = repository_fingerprint(&repo);
        let target = temp.path().join("target");
        let layout = StateLayout::new(temp.path().join("state"));
        let operation = prepared_operation(&repo, target.clone());
        let store = ForkOperationStore::create(&layout, &operation).unwrap();

        if phase != ForkPhase::Prepared {
            capture_fork_artifacts(&store, &repo, || Ok(())).unwrap();
        }
        if matches!(
            phase,
            ForkPhase::WorktreeCreated
                | ForkPhase::StagedApplied
                | ForkPhase::UnstagedApplied
                | ForkPhase::UntrackedCopied
                | ForkPhase::Verified
                | ForkPhase::ChildStaged
        ) {
            let injected = materialize(&store, &repo, |durable| {
                if durable == phase {
                    Err(Error::InvalidState(format!(
                        "injected failure after {durable:?}"
                    )))
                } else {
                    Ok(())
                }
            });
            if phase == ForkPhase::ChildStaged {
                injected.unwrap();
                store
                    .transition(ForkPhase::Verified, ForkPhase::ChildStaged, |record| {
                        record.child_session_id =
                            Some(SessionId::parse("22222222-2222-4222-8222-222222222222").unwrap());
                        record.source_checkpoint_sequence = Some(1);
                    })
                    .unwrap();
            } else {
                injected.unwrap_err();
            }
        }

        let recovered = recover_fork_failure(&store, "injected precommit failure", None).unwrap();
        assert_eq!(recovered.phase, ForkPhase::RolledBack, "phase {phase:?}");
        assert!(!target.exists(), "phase {phase:?}");
        assert!(
            git_optional(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/heads/sesh/fork-test"
                ]
            )
            .is_none(),
            "phase {phase:?}"
        );
        assert_eq!(
            repository_fingerprint(&repo),
            source_before,
            "phase {phase:?}"
        );
    }
}

#[test]
fn rollback_preserves_a_target_changed_after_the_last_durable_phase() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    prepare_capture_matrix(&repo);
    let source_before = repository_fingerprint(&repo);
    let target = temp.path().join("target");
    let layout = StateLayout::new(temp.path().join("state"));
    let operation = prepared_operation(&repo, target.clone());
    let store = ForkOperationStore::create(&layout, &operation).unwrap();
    capture_fork_artifacts(&store, &repo, || Ok(())).unwrap();
    materialize(&store, &repo, |_| Ok(())).unwrap();
    std::fs::write(target.join("changed-after-phase.txt"), "must survive\n").unwrap();

    let error = recover_fork_failure(&store, "injected failure", None).unwrap_err();

    assert!(error.to_string().contains("target worktree"));
    assert!(target.join("changed-after-phase.txt").exists());
    assert_eq!(
        git_optional(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/sesh/fork-test"
            ]
        )
        .as_deref(),
        Some(operation.target_head.as_str())
    );
    assert_eq!(repository_fingerprint(&repo), source_before);
    assert_eq!(
        store.operation().unwrap().phase,
        ForkPhase::NeedsManualRecovery
    );
}

#[test]
fn live_mutation_proof_recovers_a_git_change_not_written_to_the_operation_record() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let target = temp.path().canonicalize().unwrap().join("target");
    let layout = StateLayout::new(temp.path().join("state"));
    let operation = prepared_operation(&repo, target.clone());
    let store = ForkOperationStore::create(&layout, &operation).unwrap();
    capture_fork_artifacts(&store, &repo, || Ok(())).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "sesh/fork-test",
            target.to_str().unwrap(),
            operation.target_head.as_str(),
        ],
    );
    let proof = observe_target_proof(&operation).unwrap().unwrap();
    assert_eq!(
        store.operation().unwrap().phase,
        ForkPhase::ArtifactsCaptured
    );

    let recovered =
        recover_fork_failure(&store, "operation record write failed", Some(&proof)).unwrap();

    assert_eq!(recovered.phase, ForkPhase::RolledBack);
    assert!(!target.exists());
    assert!(
        git_optional(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/sesh/fork-test"
            ]
        )
        .is_none()
    );
}

#[test]
fn a_crash_loses_ephemeral_mutation_proof_and_preserves_the_unrecorded_target() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let target = temp.path().canonicalize().unwrap().join("target");
    let layout = StateLayout::new(temp.path().join("state"));
    let operation = prepared_operation(&repo, target.clone());
    let store = ForkOperationStore::create(&layout, &operation).unwrap();
    capture_fork_artifacts(&store, &repo, || Ok(())).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "sesh/fork-test",
            target.to_str().unwrap(),
            operation.target_head.as_str(),
        ],
    );

    let error = recover_fork_failure(&store, "recovery after process death", None).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("no durable or live mutation proof")
    );
    assert!(target.exists());
    assert_eq!(
        store.operation().unwrap().phase,
        ForkPhase::NeedsManualRecovery
    );
}

#[test]
fn rollback_removes_an_exact_staged_child_before_its_proven_target() {
    let fixture = staged_child_fixture();

    let recovered =
        recover_fork_failure(&fixture.store, "injected child staging failure", None).unwrap();

    assert_eq!(recovered.phase, ForkPhase::RolledBack);
    assert!(!fixture.child_dir.exists());
    assert!(!fixture.target.exists());
}

#[test]
fn live_child_proof_recovers_staging_before_the_operation_record_write() {
    let fixture = staged_child_fixture_before_record();
    let proof = StagedChildProof {
        child_session_id: fixture.child_id.clone(),
        source_checkpoint_sequence: 3,
    };

    let recovered = recover_fork_failure_with_live_child(
        &fixture.store,
        "child phase record write failed",
        None,
        Some(&proof),
    )
    .unwrap();

    assert_eq!(recovered.phase, ForkPhase::RolledBack);
    assert!(!fixture.child_dir.exists());
    assert!(!fixture.target.exists());
}

#[test]
fn rollback_preserves_all_artifacts_when_the_staged_child_inventory_changed() {
    let fixture = staged_child_fixture();
    let unexpected = fixture.child_dir.join("unexpected.txt");
    std::fs::write(&unexpected, "not created by the fork transaction\n").unwrap();

    let error =
        recover_fork_failure(&fixture.store, "injected child staging failure", None).unwrap_err();

    assert!(error.to_string().contains("staged child session"));
    assert!(unexpected.exists());
    assert!(fixture.target.exists());
    assert_eq!(
        fixture.store.operation().unwrap().phase,
        ForkPhase::NeedsManualRecovery
    );
}

#[test]
fn rollback_preserves_the_staged_child_when_target_proof_fails() {
    let fixture = staged_child_fixture();
    std::fs::write(
        fixture.target.join("changed-after-child-stage.txt"),
        "must preserve the whole transaction\n",
    )
    .unwrap();

    recover_fork_failure(&fixture.store, "injected child staging failure", None).unwrap_err();

    assert!(fixture.child_dir.exists());
    assert!(
        fixture
            .target
            .join("changed-after-child-stage.txt")
            .exists()
    );
    assert_eq!(
        fixture.store.operation().unwrap().phase,
        ForkPhase::NeedsManualRecovery
    );
}

#[test]
fn committed_lineage_recovers_forward_without_removing_git_state() {
    for phase in [
        ForkPhase::ChildStaged,
        ForkPhase::LineageCommitted,
        ForkPhase::ChildBound,
        ForkPhase::RunLeased,
    ] {
        let fixture = staged_child_fixture();
        let operation = fixture.store.operation().unwrap();
        fixture
            .parent
            .append(
                &FixedRuntime,
                None,
                None,
                EventKind::SessionForked {
                    operation_id: operation.id.clone(),
                    child_session_id: operation.child_session_id.clone().unwrap(),
                    parent_checkpoint_sequence: 3,
                    target_worktree: operation.target_worktree.clone(),
                    target_branch: operation.target_branch.clone(),
                },
            )
            .unwrap();
        if phase != ForkPhase::ChildStaged {
            fixture
                .store
                .transition(ForkPhase::ChildStaged, ForkPhase::LineageCommitted, |_| {})
                .unwrap();
        }
        let child = SessionStore::open(&fixture.layout, fixture.child_id.clone()).unwrap();
        if matches!(phase, ForkPhase::ChildBound | ForkPhase::RunLeased) {
            child.bind_worktree().unwrap();
            fixture
                .store
                .transition(ForkPhase::LineageCommitted, ForkPhase::ChildBound, |_| {})
                .unwrap();
        }
        if phase == ForkPhase::RunLeased {
            fixture
                .store
                .transition(ForkPhase::ChildBound, ForkPhase::RunLeased, |_| {})
                .unwrap();
        }

        let recovered =
            recover_fork_failure(&fixture.store, "injected postcommit failure", None).unwrap();

        let expected = if phase == ForkPhase::RunLeased {
            ForkPhase::Complete
        } else {
            ForkPhase::ChildBound
        };
        assert_eq!(recovered.phase, expected, "phase {phase:?}");
        assert!(fixture.target.exists(), "phase {phase:?}");
        assert_eq!(
            git_optional(
                &fixture.parent.meta().worktree.worktree,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/heads/sesh/fork-test"
                ]
            )
            .as_deref(),
            Some(operation.target_head.as_str()),
            "phase {phase:?}"
        );
        assert!(
            SessionStore::find_for_worktree(&fixture.layout, &child.meta().worktree)
                .unwrap()
                .is_some(),
            "phase {phase:?}"
        );
    }
}

#[test]
fn capture_is_deterministic_across_absolute_paths_and_never_mutates_the_source() {
    let temp = TempDir::new().unwrap();
    let repo_a = temp.path().join("first/repo-a");
    let repo_b = temp.path().join("elsewhere/deeper/repo-b");
    prepare_capture_matrix(&repo_a);
    prepare_capture_matrix(&repo_b);

    let before_a = repository_fingerprint(&repo_a);
    let before_b = repository_fingerprint(&repo_b);
    let index_a = git_bytes(&repo_a, &["ls-files", "--stage", "-z"]);
    let index_b = git_bytes(&repo_b, &["ls-files", "--stage", "-z"]);

    let layout_a = StateLayout::new(temp.path().join("state-a"));
    let operation_a = prepared_operation(&repo_a, temp.path().join("target-a"));
    let store_a = ForkOperationStore::create(&layout_a, &operation_a).unwrap();
    let captured_a = capture_fork_artifacts(&store_a, &repo_a, || Ok(())).unwrap();

    let layout_b = StateLayout::new(temp.path().join("state-b"));
    let operation_b = prepared_operation(&repo_b, temp.path().join("target-b"));
    let store_b = ForkOperationStore::create(&layout_b, &operation_b).unwrap();
    let captured_b = capture_fork_artifacts(&store_b, &repo_b, || Ok(())).unwrap();

    assert_eq!(captured_a.phase, ForkPhase::ArtifactsCaptured);
    assert_eq!(captured_b.phase, ForkPhase::ArtifactsCaptured);
    let fingerprint_a = captured_a.source_fingerprint.unwrap();
    let fingerprint_b = captured_b.source_fingerprint.unwrap();
    assert_eq!(
        fingerprint_a.staged_patch_sha256,
        fingerprint_b.staged_patch_sha256
    );
    assert_eq!(
        fingerprint_a.index_entries_sha256,
        fingerprint_b.index_entries_sha256
    );
    assert_eq!(
        fingerprint_a.unstaged_patch_sha256,
        fingerprint_b.unstaged_patch_sha256
    );
    assert_eq!(
        fingerprint_a.untracked_manifest_sha256,
        fingerprint_b.untracked_manifest_sha256
    );
    assert_eq!(
        fingerprint_a.submodule_manifest_sha256,
        fingerprint_b.submodule_manifest_sha256
    );
    assert_eq!(
        std::fs::read(store_a.operation_dir().join("untracked/manifest.json")).unwrap(),
        std::fs::read(store_b.operation_dir().join("untracked/manifest.json")).unwrap()
    );
    assert_eq!(
        std::fs::read(store_a.operation_dir().join("submodules.json")).unwrap(),
        std::fs::read(store_b.operation_dir().join("submodules.json")).unwrap()
    );

    let manifest: Vec<UntrackedEntry> = serde_json::from_slice(
        &std::fs::read(store_a.operation_dir().join("untracked/manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.len(), 4);
    assert_eq!(
        manifest
            .iter()
            .map(|entry| entry.path.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["nested/data.bin", "run-copy.sh", "run.sh", "shortcut"]
    );
    let run_artifacts = manifest
        .iter()
        .filter(|entry| matches!(entry.path.to_str(), Some("run.sh" | "run-copy.sh")))
        .map(|entry| entry.artifact.clone().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(run_artifacts.len(), 2);
    assert_eq!(run_artifacts[0], run_artifacts[1]);
    for entry in manifest {
        entry.validate().unwrap();
        if let Some(artifact) = entry.artifact {
            let metadata = std::fs::metadata(store_a.operation_dir().join(artifact)).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    assert_eq!(repository_fingerprint(&repo_a), before_a);
    assert_eq!(repository_fingerprint(&repo_b), before_b);
    assert_eq!(git_bytes(&repo_a, &["ls-files", "--stage", "-z"]), index_a);
    assert_eq!(git_bytes(&repo_b, &["ls-files", "--stage", "-z"]), index_b);
}

#[test]
fn capture_records_recursive_submodules_without_absolute_git_metadata() {
    let temp = TempDir::new().unwrap();
    let leaf = temp.path().join("leaf");
    init_repo(&leaf);
    let middle = temp.path().join("middle");
    init_repo(&middle);
    git_allow_file(
        &middle,
        &["submodule", "add"],
        &[&leaf, std::path::Path::new("nested/leaf")],
    );
    git(&middle, &["commit", "-am", "add nested submodule"]);

    let repo = temp.path().join("repo");
    init_repo(&repo);
    git_allow_file(
        &repo,
        &["submodule", "add"],
        &[&middle, std::path::Path::new("vendor/middle")],
    );
    git(&repo, &["commit", "-am", "add submodule"]);

    let layout = StateLayout::new(temp.path().join("state"));
    let operation = prepared_operation(&repo, temp.path().join("target"));
    let store = ForkOperationStore::create(&layout, &operation).unwrap();
    capture_fork_artifacts(&store, &repo, || Ok(())).unwrap();

    let bytes = std::fs::read(store.operation_dir().join("submodules.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(manifest.as_array().unwrap().len(), 2);
    assert_eq!(manifest[0]["path"], "vendor/middle");
    assert_eq!(manifest[0]["initialized"], true);
    assert_eq!(manifest[0]["parent"], serde_json::Value::Null);
    assert_eq!(manifest[1]["path"], "vendor/middle/nested/leaf");
    assert_eq!(manifest[1]["initialized"], false);
    assert_eq!(manifest[1]["parent"], "vendor/middle");
    assert!(
        !String::from_utf8(bytes)
            .unwrap()
            .contains(temp.path().to_str().unwrap())
    );
}

fn prepared_operation(repo: &std::path::Path, target: std::path::PathBuf) -> ForkOperation {
    let source = Git::new().snapshot(repo).unwrap();
    let target = target
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join(target.file_name().unwrap());
    ForkOperation {
        schema_version: 1,
        id: OperationId::parse("99999999-9999-4999-8999-999999999999").unwrap(),
        phase: ForkPhase::Prepared,
        source_session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        source_worktree: source.identity,
        source_checkpoint_sequence: None,
        source_fingerprint: None,
        target_branch: "sesh/fork-test".into(),
        target_worktree: target,
        target_head: source.head,
        child_session_id: None,
        target_fingerprint: None,
        target_cleanup_inventory_sha256: None,
        branch_created: false,
        target_created: false,
        error: None,
        updated_at: "2026-07-19T10:00:00Z".into(),
    }
}

struct StagedChildFixture {
    _temp: TempDir,
    layout: StateLayout,
    parent: SessionStore,
    store: ForkOperationStore,
    target: std::path::PathBuf,
    child_dir: std::path::PathBuf,
    child_id: SessionId,
}

fn staged_child_fixture() -> StagedChildFixture {
    let fixture = staged_child_fixture_before_record();
    fixture
        .store
        .transition(ForkPhase::Verified, ForkPhase::ChildStaged, |record| {
            record.child_session_id = Some(fixture.child_id.clone());
            record.source_checkpoint_sequence = Some(3);
        })
        .unwrap();
    fixture
}

fn staged_child_fixture_before_record() -> StagedChildFixture {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    prepare_capture_matrix(&repo);
    let target = temp.path().canonicalize().unwrap().join("target");
    let layout = StateLayout::new(temp.path().join("state"));
    let runtime = FixedRuntime;
    let parent =
        SessionStore::create(&layout, &runtime, Git::new().snapshot(&repo).unwrap()).unwrap();
    let operation = prepared_operation(&repo, target.clone());
    let store = ForkOperationStore::create(&layout, &operation).unwrap();
    capture_fork_artifacts(&store, &repo, || Ok(())).unwrap();
    materialize(&store, &repo, |_| Ok(())).unwrap();
    let child_id = SessionId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    let child = SessionStore::stage_child(
        &layout,
        &runtime,
        Git::new().snapshot(&target).unwrap(),
        parent.id().clone(),
        3,
        child_id.clone(),
    )
    .unwrap();
    StagedChildFixture {
        child_dir: child.session_dir(),
        _temp: temp,
        layout,
        parent,
        store,
        target,
        child_id,
    }
}

struct FixedRuntime;

impl Runtime for FixedRuntime {
    fn now(&self) -> sesh::error::Result<String> {
        Ok("2026-07-19T10:00:00Z".into())
    }

    fn session_id(&self) -> SessionId {
        SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap()
    }

    fn run_id(&self) -> RunId {
        RunId::parse("33333333-3333-4333-8333-333333333333").unwrap()
    }

    fn operation_id(&self) -> OperationId {
        OperationId::parse("99999999-9999-4999-8999-999999999999").unwrap()
    }
}

fn prepare_capture_matrix(repo: &std::path::Path) {
    init_repo(repo);
    std::fs::write(repo.join("delete.txt"), "delete me\n").unwrap();
    std::fs::write(repo.join("old-name.txt"), "rename me\n").unwrap();
    std::fs::write(repo.join("binary.bin"), [0, 1, 2, 0xff, 0, 4]).unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "capture fixtures"]);

    std::fs::write(repo.join("README.md"), "staged README\n").unwrap();
    git(repo, &["add", "README.md"]);
    std::fs::write(repo.join("README.md"), "unstaged README\n").unwrap();
    std::fs::remove_file(repo.join("delete.txt")).unwrap();
    git(repo, &["mv", "old-name.txt", "new-name.txt"]);
    std::fs::write(repo.join("binary.bin"), [0, 9, 8, 0xff, 0, 7]).unwrap();
    git(repo, &["add", "binary.bin"]);

    std::fs::create_dir_all(repo.join("nested")).unwrap();
    std::fs::write(repo.join("nested/data.bin"), [0, 3, 0xff, 4]).unwrap();
    std::fs::write(repo.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(repo.join("run-copy.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(repo.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink("nested/data.bin", repo.join("shortcut")).unwrap();
}

fn git_bytes(cwd: &std::path::Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .arg("--no-pager")
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_optional(cwd: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("--no-pager")
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap();
    match output.status.code() {
        Some(0) => Some(String::from_utf8(output.stdout).unwrap().trim().to_owned()),
        Some(1) => None,
        _ => panic!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn git_allow_file(cwd: &std::path::Path, args: &[&str], paths: &[&std::path::Path]) {
    let output = std::process::Command::new("git")
        .arg("-c")
        .arg("protocol.file.allow=always")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .args(paths)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
