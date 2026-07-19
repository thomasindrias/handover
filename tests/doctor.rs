mod support;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use sesh::fork::{ForkOperationStore, capture_fork_artifacts};
use sesh::git::Git;
use sesh::git::fork::materialize;
use sesh::model::{EventKind, ForkOperation, ForkPhase, OperationId, Provider, RunId, SessionId};
use sesh::provider::adapter;
use sesh::runtime::Runtime;
use sesh::store::{SessionStore, StateLayout};
use tempfile::TempDir;

use support::{git, init_repo, path_with, write_executable};

#[test]
fn setup_is_inspectable_noninteractive_and_refuses_asset_drift() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    install_capable_providers(&bin);
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["setup", "claude"])
        .assert()
        .code(2)
        .stdout(predicates::str::contains("claude --plugin-dir"));
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["setup", "codex"])
        .assert()
        .code(2)
        .stdout(predicates::str::contains("hooks.SessionStart"))
        .stdout(predicates::str::contains("dangerously-bypass-hook-trust").not());

    cargo_bin_cmd!("sesh")
        .env_remove("SESH_HOME")
        .env_remove("SESH_SESSION_ID")
        .env_remove("SESH_RUN_ID")
        .args(["__hook", "claude"])
        .write_stdin(r#"{"hook_event_name":"SessionStart"}"#)
        .assert()
        .success()
        .stdout("");

    let plugin = state.join("integrations/claude/1/.claude-plugin/plugin.json");
    std::fs::write(&plugin, b"drift").unwrap();
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["setup", "claude"])
        .assert()
        .failure();
}

#[test]
fn doctor_reports_layered_diagnostics_as_stable_json_without_mutation() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");
    let layout = StateLayout::new(state.clone());
    layout.ensure().unwrap();
    adapter(Provider::Claude)
        .setup(&layout.integrations())
        .unwrap();
    adapter(Provider::Codex)
        .setup(&layout.integrations())
        .unwrap();
    std::fs::write(
        state.join("integrations/claude/1/hooks/hooks.json"),
        b"drift",
    )
    .unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o750)).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"${1:-}\" = --help ]; then echo no-supported-flags; exit 0; fi\nexit 0\n",
    );
    write_executable(
        &bin.join("codex"),
        "#!/bin/sh\nif [ \"${1:-}\" = --help ]; then echo '--config --add-dir --cd'; exit 0; fi\nif [ \"${1:-}\" = features ]; then echo 'hooks experimental false'; exit 0; fi\nexit 0\n",
    );
    let before = std::fs::read(state.join("FORMAT")).unwrap();
    let before_modified = std::fs::metadata(state.join("FORMAT"))
        .unwrap()
        .modified()
        .unwrap();

    let output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &bin)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let diagnostics: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    for diagnostic in &diagnostics {
        assert!(diagnostic["code"].as_str().is_some());
        assert!(diagnostic["severity"].as_str().is_some());
        assert!(diagnostic["message"].as_str().is_some());
    }
    for code in [
        "git.missing",
        "provider.capability_missing",
        "codex.hooks_unstable",
        "integration.invalid",
        "permissions.insecure",
    ] {
        assert!(
            diagnostics.iter().any(|item| item["code"] == code),
            "missing {code}: {diagnostics:?}"
        );
    }
    assert_eq!(std::fs::read(state.join("FORMAT")).unwrap(), before);
    assert_eq!(
        std::fs::metadata(state.join("FORMAT"))
            .unwrap()
            .modified()
            .unwrap(),
        before_modified
    );

    let empty_path = temp.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let missing = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &empty_path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let missing: Vec<serde_json::Value> = serde_json::from_slice(&missing.stdout).unwrap();
    assert!(
        missing
            .iter()
            .any(|item| item["code"] == "provider.missing")
    );
}

#[test]
fn doctor_repairs_only_partial_tail_refs_and_capture_sentinel() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    install_capable_providers(&bin);
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--help" ]]; then printf '%s\n' '--plugin-dir --add-dir'; exit 0; fi
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake 1'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$SESH_HOOK_BIN" __hook claude >/dev/null
printf '%s' '{"objective":"Repair","summary":"Checkpoint","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Continue"],"related_event_sequences":[]}' | "$SESH_HOOK_BIN" checkpoint --format json --from-provider
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}' | "$SESH_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
    );
    let state = temp.path().join("state");
    let path = path_with(&bin);
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();
    adapter(Provider::Codex)
        .setup(&state.join("integrations"))
        .unwrap();
    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let journal = session.join("events.jsonl");
    let committed = std::fs::read(&journal).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(b"{invalid partial")
        .unwrap();
    std::fs::remove_file(session.join("refs/latest-checkpoint")).unwrap();
    let run = std::fs::read_dir(session.join("runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(run.join("capture-failed.json"), b"{}\n").unwrap();
    std::fs::set_permissions(
        run.join("capture-failed.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let before_plain = std::fs::read(&journal).unwrap();
    let before_modified = std::fs::metadata(&journal).unwrap().modified().unwrap();

    let plain = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let diagnostics: Vec<serde_json::Value> = serde_json::from_slice(&plain.stdout).unwrap();
    assert!(diagnostics.iter().any(|item| {
        item["code"] == "journal.partial_tail"
            && item["repair_command"] == "sesh doctor --repair"
            && item["message"].as_str().unwrap().contains("16")
    }));
    assert_eq!(std::fs::read(&journal).unwrap(), before_plain);
    assert_eq!(
        std::fs::metadata(&journal).unwrap().modified().unwrap(),
        before_modified
    );

    let repaired = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["doctor", "--json", "--repair"])
        .output()
        .unwrap();
    let repaired_diagnostics: Vec<serde_json::Value> =
        serde_json::from_slice(&repaired.stdout).unwrap();
    assert!(
        repaired_diagnostics
            .iter()
            .any(|item| item["code"] == "journal.tail_repaired")
    );
    assert!(
        repaired_diagnostics
            .iter()
            .any(|item| item["code"] == "checkpoint.ref_rebuilt")
    );
    assert!(
        repaired_diagnostics
            .iter()
            .any(|item| item["code"] == "capture.sentinel_removed")
    );
    assert_eq!(std::fs::read(&journal).unwrap(), committed);
    assert!(session.join("refs/latest-checkpoint").exists());
    assert!(!run.join("capture-failed.json").exists());

    let mut corrupt = committed;
    let checksum = corrupt
        .windows(b"sha256:".len())
        .position(|window| window == b"sha256:")
        .unwrap()
        + b"sha256:".len();
    corrupt[checksum] = if corrupt[checksum] == b'a' {
        b'b'
    } else {
        b'a'
    };
    std::fs::write(&journal, corrupt).unwrap();
    let output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let diagnostics: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|item| item["code"] == "journal.corrupt" && item["severity"] == "error")
    );
}

#[test]
fn doctor_reports_precommit_forks_without_mutating_or_deleting_them() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("README.md"), "staged\n").unwrap();
    git(&repo, &["add", "README.md"]);
    std::fs::write(repo.join("README.md"), "unstaged\n").unwrap();
    std::fs::write(repo.join("untracked.txt"), "untracked\n").unwrap();
    let state = temp.path().join("state");
    let layout = StateLayout::new(state.clone());
    layout.ensure().unwrap();
    adapter(Provider::Claude)
        .setup(&layout.integrations())
        .unwrap();
    adapter(Provider::Codex)
        .setup(&layout.integrations())
        .unwrap();
    let target = temp
        .path()
        .canonicalize()
        .unwrap()
        .join("target with spaces");
    let operation = prepared_operation(&repo, target.clone());
    let store = ForkOperationStore::create(&layout, &operation).unwrap();
    capture_fork_artifacts(&store, &repo, || Ok(())).unwrap();
    materialize(&store, &repo, |_| Ok(())).unwrap();
    let record = store.operation_dir().join("operation.json");
    let before_bytes = std::fs::read(&record).unwrap();
    let before_mtime = std::fs::metadata(&record).unwrap().modified().unwrap();
    let branch_before = git_output(
        &repo,
        &["rev-parse", "--verify", "refs/heads/sesh/doctor-test"],
    );
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    install_capable_providers(&bin);

    let plain = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let diagnostics: Vec<serde_json::Value> = serde_json::from_slice(&plain.stdout).unwrap();
    let diagnostic = diagnostics
        .iter()
        .find(|item| item["code"] == "fork_precommit_crash")
        .unwrap();
    assert_eq!(diagnostic["command_argv"][0], "git");
    assert_eq!(diagnostic["command_argv"][2], target.to_str().unwrap());
    let display = diagnostic["command"].as_str().unwrap();
    assert!(display.contains("target with spaces"));
    assert!(display.contains('\''));
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("sesh/doctor-test")
    );
    assert_eq!(std::fs::read(&record).unwrap(), before_bytes);
    assert_eq!(
        std::fs::metadata(&record).unwrap().modified().unwrap(),
        before_mtime
    );

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["doctor", "--json", "--repair"])
        .assert()
        .success();
    assert!(target.exists());
    assert_eq!(store.operation().unwrap().phase, ForkPhase::Verified);
    assert_eq!(
        git_output(
            &repo,
            &["rev-parse", "--verify", "refs/heads/sesh/doctor-test"]
        ),
        branch_before
    );

    std::fs::write(target.join("changed-after-crash.txt"), "preserve me\n").unwrap();
    let changed = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["doctor", "--json", "--repair"])
        .output()
        .unwrap();
    let diagnostics: Vec<serde_json::Value> = serde_json::from_slice(&changed.stdout).unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|item| item["code"] == "fork_target_changed")
    );
    assert!(target.join("changed-after-crash.txt").exists());
    assert_eq!(store.operation().unwrap().phase, ForkPhase::Verified);
}

#[test]
fn doctor_repairs_only_the_missing_binding_after_lineage_commit() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");
    let layout = StateLayout::new(state.clone());
    let runtime = FixedRuntime;
    let parent =
        SessionStore::create(&layout, &runtime, Git::new().snapshot(&repo).unwrap()).unwrap();
    adapter(Provider::Claude)
        .setup(&layout.integrations())
        .unwrap();
    adapter(Provider::Codex)
        .setup(&layout.integrations())
        .unwrap();
    let target = temp.path().canonicalize().unwrap().join("target");
    let operation = prepared_operation(&repo, target.clone());
    let store = ForkOperationStore::create(&layout, &operation).unwrap();
    capture_fork_artifacts(&store, &repo, || Ok(())).unwrap();
    materialize(&store, &repo, |_| Ok(())).unwrap();
    let (transition, _) = parent
        .create_transition_checkpoint(&runtime, None, None, None)
        .unwrap();
    let child_id = SessionId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    let child = SessionStore::stage_child(
        &layout,
        &runtime,
        Git::new().snapshot(&target).unwrap(),
        parent.id().clone(),
        transition.sequence,
        child_id.clone(),
    )
    .unwrap();
    store
        .transition(ForkPhase::Verified, ForkPhase::ChildStaged, |record| {
            record.child_session_id = Some(child_id.clone());
            record.source_checkpoint_sequence = Some(transition.sequence);
        })
        .unwrap();
    parent
        .append(
            &runtime,
            None,
            None,
            EventKind::SessionForked {
                operation_id: operation.id,
                child_session_id: child_id,
                parent_checkpoint_sequence: transition.sequence,
                target_worktree: target.clone(),
                target_branch: "sesh/doctor-test".into(),
            },
        )
        .unwrap();
    assert!(
        SessionStore::find_for_worktree(&layout, &child.meta().worktree)
            .unwrap()
            .is_none()
    );
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    install_capable_providers(&bin);

    let plain = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let plain: Vec<serde_json::Value> = serde_json::from_slice(&plain.stdout).unwrap();
    assert!(
        plain
            .iter()
            .any(|item| item["code"] == "fork_postcommit_incomplete")
    );
    assert_eq!(store.operation().unwrap().phase, ForkPhase::ChildStaged);
    assert!(target.exists());

    let repaired = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["doctor", "--json", "--repair"])
        .output()
        .unwrap();
    let repaired: Vec<serde_json::Value> = serde_json::from_slice(&repaired.stdout).unwrap();
    assert!(
        repaired
            .iter()
            .any(|item| item["code"] == "fork.forward_repaired")
    );
    assert_eq!(store.operation().unwrap().phase, ForkPhase::ChildBound);
    assert_eq!(
        SessionStore::find_for_worktree(&layout, &child.meta().worktree)
            .unwrap()
            .unwrap()
            .id(),
        child.id()
    );
    assert!(target.exists());
}

fn prepared_operation(repo: &std::path::Path, target: std::path::PathBuf) -> ForkOperation {
    let source = Git::new().snapshot(repo).unwrap();
    ForkOperation {
        schema_version: 1,
        id: OperationId::parse("99999999-9999-4999-8999-999999999999").unwrap(),
        phase: ForkPhase::Prepared,
        source_session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        source_worktree: source.identity,
        source_checkpoint_sequence: None,
        source_fingerprint: None,
        target_branch: "sesh/doctor-test".into(),
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

fn git_output(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
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

fn install_capable_providers(bin: &std::path::Path) {
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"${1:-}\" = --help ]; then echo '--plugin-dir --add-dir'; exit 0; fi\nif [ \"${1:-}\" = --version ]; then echo 'fake 1'; exit 0; fi\nexit 0\n",
    );
    write_executable(
        &bin.join("codex"),
        "#!/bin/sh\nif [ \"${1:-}\" = --help ]; then echo '--config --add-dir --cd'; exit 0; fi\nif [ \"${1:-}\" = features ]; then echo 'hooks stable true'; exit 0; fi\nif [ \"${1:-}\" = --version ]; then echo 'fake 1'; exit 0; fi\nexit 0\n",
    );
}
