mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use sesh::model::{Provider, RunId, SessionId};
use sesh::store::lease::{LeaseStore, ProcessIdentity, RunLease};
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

/// Fake claude: SessionStart, `cycles` recognized tool cycles, optionally a
/// provider checkpoint, then Stop.
fn fake_claude(bin: &std::path::Path, cycles: u32, checkpoint_before_stop: bool) {
    let checkpoint_line = if checkpoint_before_stop {
        r#"printf '%s' '{"objective":"Ship it","summary":"On track.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Finish"],"related_event_sequences":[]}' | "$SESH_HOOK_BIN" checkpoint --format json --from-provider"#.to_owned()
    } else {
        String::new()
    };
    let body = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${{1:-}} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() {{ printf '%s' "$1" | "$SESH_HOOK_BIN" __hook claude >/dev/null; }}
hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}}'
for i in $(seq 1 {cycles}); do
  hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":"cargo test case-'"$i"'"}},"tool_use_id":"tool-'"$i"'"}}'
  hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{{"command":"cargo test case-'"$i"'"}},"tool_response":{{"stdout":"ok","stderr":"","exit_code":0}},"tool_use_id":"tool-'"$i"'"}}'
done
{checkpoint_line}
hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}}'
exit 0
"#
    );
    write_executable(&bin.join("claude"), &body);
}

fn run_fake_claude(
    cycles: u32,
    checkpoint_before_stop: bool,
) -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_claude(&bin, cycles, checkpoint_before_stop);
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("sesh")
        .current_dir(&cwd)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    (temp, repo, state)
}

fn status_json(repo: &std::path::Path, state: &std::path::Path) -> serde_json::Value {
    let output = cargo_bin_cmd!("sesh")
        .current_dir(repo)
        .env("SESH_HOME", state)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn a_fresh_checkpointed_session_reports_ready() {
    let (_temp, repo, state) = run_fake_claude(1, true);
    let status = status_json(&repo, &state);
    let readiness = &status["switch_readiness"];
    assert_eq!(readiness["ready"], true);
    assert_eq!(readiness["lease"], "free");
    assert!(readiness["lease_reason"].is_null());
    assert_eq!(readiness["checkpoint_fresh"], true);
    assert_eq!(readiness["handoff_renderable"], true);
    assert!(readiness["handoff_error"].is_null());
}

#[test]
fn a_stale_narrative_checkpoint_is_advisory_and_does_not_block_readiness() {
    let (_temp, repo, state) = run_fake_claude(7, false);
    let status = status_json(&repo, &state);
    let readiness = &status["switch_readiness"];
    assert!(status["events_since_narrative"].as_u64().unwrap() >= 20);
    assert_eq!(readiness["checkpoint_fresh"], false);
    assert_eq!(readiness["ready"], true);
}

#[test]
fn a_recoverable_lease_blocks_readiness_and_says_so() {
    let (_temp, repo, state) = run_fake_claude(1, true);
    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let session_id = SessionId::parse(session.file_name().unwrap().to_str().unwrap()).unwrap();
    let leases = LeaseStore::new(&session);
    let stale = RunLease::new(
        session_id,
        RunId::new(),
        Provider::Claude,
        ProcessIdentity {
            pid: u32::MAX,
            start_token: "gone".into(),
        },
    )
    .unwrap();
    leases.create(&stale).unwrap();

    let status = status_json(&repo, &state);
    let readiness = &status["switch_readiness"];
    assert_eq!(readiness["lease"], "recoverable");
    assert!(
        readiness["lease_reason"]
            .as_str()
            .unwrap()
            .contains("--recover-lease")
    );
    assert_eq!(readiness["ready"], false);
}

#[test]
fn a_live_lease_blocks_readiness_and_says_so() {
    let (_temp, repo, state) = run_fake_claude(1, true);
    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let session_id = SessionId::parse(session.file_name().unwrap().to_str().unwrap()).unwrap();
    let leases = LeaseStore::new(&session);
    let live = RunLease::new(
        session_id,
        RunId::new(),
        Provider::Claude,
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .unwrap();
    leases.create(&live).unwrap();

    let status = status_json(&repo, &state);
    let readiness = &status["switch_readiness"];
    assert_eq!(readiness["lease"], "blocked");
    assert_eq!(readiness["ready"], false);
}
