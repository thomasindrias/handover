mod support;

use std::os::unix::fs::PermissionsExt;

use assert_cmd::cargo::cargo_bin_cmd;
use sesh::model::EventEnvelope;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

#[test]
fn status_log_and_inspect_are_verified_stable_json_projections() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$SESH_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test"},"tool_use_id":"tool-1"}'
large=$(printf '%09000d' 0)
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test"},"tool_response":{"stdout":"'"$large"'","stderr":"","exit_code":0},"tool_use_id":"tool-1"}'
printf '%s\n' 'dirty' > changed.txt
printf '%s' '{"objective":"Inspect session","summary":"Read surfaces are ready","decisions":[],"assumptions":[],"constraints":[],"completed":["capture"],"in_progress":[],"blockers":[],"next_steps":["Inspect output"],"related_event_sequences":[]}' | "$SESH_HOOK_BIN" checkpoint --format json --from-provider
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
    let state = temp.path().join("state");
    let path = path_with(&bin);
    cargo_bin_cmd!("sesh")
        .current_dir(&cwd)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    let status_output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(status_output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status_output.stdout).unwrap();
    assert!(status["session_id"].as_str().is_some());
    assert_eq!(status["provider"], "claude");
    assert_eq!(status["branch"], "main");
    assert!(status["head"].as_str().unwrap().len() >= 40);
    assert_eq!(
        status["cwd"].as_str().unwrap(),
        cwd.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(status["worktree"].as_str().unwrap().ends_with("/repo"));
    assert!(
        status["dirty"]["untracked"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path["path"] == "apps/web/changed.txt")
    );
    assert_eq!(status["latest_checkpoint"]["kind"], "narrative");
    assert!(status["latest_checkpoint"]["sequence"].as_u64().is_some());
    assert!(status["capture_gaps"].is_array());

    let log_output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["log", "--json"])
        .output()
        .unwrap();
    assert!(log_output.status.success());
    let envelopes: Vec<EventEnvelope> = String::from_utf8(log_output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(envelopes.len() > 5);
    assert!(envelopes.iter().all(|envelope| envelope.verify().is_ok()));
    assert!(
        envelopes
            .windows(2)
            .all(|pair| pair[0].event.sequence + 1 == pair[1].event.sequence)
    );

    let from_output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["log", "--from", "5"])
        .output()
        .unwrap();
    assert!(from_output.status.success());
    for line in String::from_utf8(from_output.stdout).unwrap().lines() {
        let sequence: u64 = line.split_whitespace().next().unwrap().parse().unwrap();
        assert!(sequence >= 5);
    }

    let inspect_output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["inspect", "--json"])
        .output()
        .unwrap();
    assert!(inspect_output.status.success());
    let inspect: serde_json::Value = serde_json::from_slice(&inspect_output.stdout).unwrap();
    assert_eq!(
        inspect["state_root"],
        state.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(
        inspect["session_dir"]
            .as_str()
            .unwrap()
            .contains("/sessions/")
    );
    assert_eq!(
        inspect["event_count"].as_u64().unwrap() as usize,
        envelopes.len()
    );
    assert!(inspect["checkpoint_files"].as_array().unwrap().len() >= 2);
    assert!(!inspect["blob_references"].as_array().unwrap().is_empty());
    assert!(inspect["permissions"].is_array());
    assert!(inspect["active_lease"].is_null());
    for file in inspect["checkpoint_files"].as_array().unwrap() {
        assert_eq!(file["mode"], 0o600);
    }
    let session_dir = std::path::Path::new(inspect["session_dir"].as_str().unwrap());
    assert_eq!(
        std::fs::metadata(session_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );

    let journal = session_dir.join("events.jsonl");
    let before_boundary_checks = std::fs::read(&journal).unwrap();
    for arguments in [
        vec!["status", "--json"],
        vec!["log", "--json"],
        vec!["inspect", "--json"],
        vec!["delete", "--yes"],
        vec!["switch", "codex"],
        vec!["handoff", "codex"],
        vec!["run", "codex"],
        vec!["checkpoint", "--format", "json"],
    ] {
        let output = cargo_bin_cmd!("sesh")
            .current_dir(&repo)
            .env("SESH_HOME", &state)
            .env("SESH_RUN_ID", "22222222-2222-4222-8222-222222222222")
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("attached provider"));
    }
    assert_eq!(std::fs::read(journal).unwrap(), before_boundary_checks);
}
