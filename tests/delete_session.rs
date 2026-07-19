mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use sesh::fork::ForkOperationStore;
use sesh::git::Git;
use sesh::model::{ForkOperation, ForkPhase, OperationId, Provider, RunId, SessionId, SessionMeta};
use sesh::store::StateLayout;
use sesh::store::lease::{LeaseStore, ProcessIdentity, RunLease};
use tempfile::TempDir;

use support::{init_repo, path_with, repository_fingerprint, write_executable};

#[test]
fn deletion_requires_confirmation_refuses_live_runs_and_preserves_the_repository() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$SESH_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
    );
    let state = temp.path().join("state");
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["run", "claude"])
        .assert()
        .success();
    let before = repository_fingerprint(&repo);
    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let session_id = SessionId::parse(session.file_name().unwrap().to_str().unwrap()).unwrap();
    let external = temp.path().join("must-survive");
    std::fs::write(&external, b"outside session\n").unwrap();
    std::os::unix::fs::symlink(&external, session.join("external-link")).unwrap();

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .arg("delete")
        .assert()
        .failure();
    assert!(session.exists());

    let leases = LeaseStore::new(&session);
    let live = RunLease::new(
        session_id.clone(),
        RunId::new(),
        Provider::Claude,
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .unwrap();
    leases.create(&live).unwrap();
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .failure();
    assert!(session.exists());
    leases.clear(&live.run_id).unwrap();

    let mut foreign = RunLease::new(
        session_id,
        RunId::new(),
        Provider::Claude,
        ProcessIdentity {
            pid: u32::MAX,
            start_token: "gone".into(),
        },
    )
    .unwrap();
    foreign.host = "different-host".into();
    leases.create(&foreign).unwrap();
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .failure();
    assert!(session.exists());
    leases.clear(&foreign.run_id).unwrap();

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .success();

    assert!(!session.exists());
    assert_eq!(
        std::fs::read_dir(state.join("sessions")).unwrap().count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(state.join("refs/worktrees"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(repository_fingerprint(&repo), before);
    assert_eq!(std::fs::read(external).unwrap(), b"outside session\n");
}

#[test]
fn deletion_orders_children_and_removes_terminal_fork_artifacts_without_touching_git() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let target = temp.path().canonicalize().unwrap().join("forked worktree");
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    install_fork_providers(&bin);
    let state = temp.path().join("state");
    let path = path_with(&bin);
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args([
            "fork",
            "codex",
            "--branch",
            "sesh/delete-child",
            "--worktree",
            target.to_str().unwrap(),
        ])
        .assert()
        .success();

    let mut sessions = session_metas(&state);
    sessions.sort_by_key(|meta| meta.parent_session_id.is_some());
    let parent = sessions[0].clone();
    let child = sessions[1].clone();
    assert_eq!(child.parent_session_id.as_ref(), Some(&parent.id));
    let child_dir = state.join("sessions").join(child.id.to_string());
    assert!(
        std::fs::read_dir(child_dir.join("runs"))
            .unwrap()
            .any(|entry| entry.unwrap().path().join("inbox/handoff.md").exists())
    );
    let source_before = repository_fingerprint(&repo);
    let target_before = repository_fingerprint(&target);

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(child.id.to_string()));
    assert!(state.join("sessions").join(parent.id.to_string()).exists());
    assert!(child_dir.exists());

    cargo_bin_cmd!("sesh")
        .current_dir(&target)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .success();
    assert!(!child_dir.exists());
    assert!(target.exists());
    assert_eq!(
        git_text(&target, &["branch", "--show-current"]),
        "sesh/delete-child"
    );

    let layout = StateLayout::new(state.clone());
    let snapshot = Git::new().snapshot(&repo).unwrap();
    let unfinished = ForkOperation {
        schema_version: 1,
        id: OperationId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap(),
        phase: ForkPhase::Prepared,
        source_session_id: parent.id.clone(),
        source_worktree: snapshot.identity,
        source_checkpoint_sequence: None,
        source_fingerprint: None,
        target_branch: "sesh/unfinished-delete-test".into(),
        target_worktree: temp
            .path()
            .canonicalize()
            .unwrap()
            .join("unfinished-target"),
        target_head: snapshot.head,
        child_session_id: None,
        target_fingerprint: None,
        target_cleanup_inventory_sha256: None,
        branch_created: false,
        target_created: false,
        error: None,
        updated_at: "2026-07-19T10:00:00Z".into(),
    };
    let unfinished_store = ForkOperationStore::create(&layout, &unfinished).unwrap();
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(unfinished.id.to_string()));
    unfinished_store
        .transition(ForkPhase::Prepared, ForkPhase::RolledBack, |record| {
            record.error = Some("test completed".into());
        })
        .unwrap();

    let terminal_operations = std::fs::read_dir(state.join("operations"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(terminal_operations.len(), 2);

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_dir(state.join("sessions")).unwrap().count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(state.join("operations")).unwrap().count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(state.join("refs/worktrees"))
            .unwrap()
            .count(),
        0
    );
    assert!(repo.exists());
    assert!(target.exists());
    assert_eq!(repository_fingerprint(&repo), source_before);
    assert_eq!(repository_fingerprint(&target), target_before);
    assert_eq!(
        git_text(&target, &["branch", "--show-current"]),
        "sesh/delete-child"
    );
}

fn session_metas(state: &std::path::Path) -> Vec<SessionMeta> {
    std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.unwrap().path().join("meta.json")).unwrap())
                .unwrap()
        })
        .collect()
}

fn git_text(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn install_fork_providers(bin: &std::path::Path) {
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"claude-native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$SESH_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
    );
    write_executable(
        &bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"codex-native","turn_id":"turn-delete","cwd":"'"$cwd_json"'","model":"test","hook_event_name":"SessionStart","source":"startup"}' | "$SESH_HOOK_BIN" __hook codex >/dev/null
exit 0
"#,
    );
}
