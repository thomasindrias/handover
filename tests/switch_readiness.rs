mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use handover::model::{Provider, RunId, SessionId};
use handover::store::lease::{LeaseStore, ProcessIdentity, RunLease};
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

/// Fake claude: SessionStart, `cycles` recognized tool cycles, optionally a
/// provider checkpoint, then Stop.
fn fake_claude(bin: &std::path::Path, cycles: u32, checkpoint_before_stop: bool) {
    let checkpoint_line = if checkpoint_before_stop {
        r#"printf '%s' '{"objective":"Ship it","summary":"On track.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Finish"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider"#.to_owned()
    } else {
        String::new()
    };
    let body = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${{1:-}} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() {{ printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }}
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

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    (temp, repo, state)
}

fn status_json(repo: &std::path::Path, state: &std::path::Path) -> serde_json::Value {
    let output = cargo_bin_cmd!("handover")
        .current_dir(repo)
        .env("HANDOVER_HOME", state)
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
    assert_eq!(readiness["handover_renderable"], true);
    assert!(readiness["handover_error"].is_null());
    assert!(readiness["armed"].is_null());
}

#[test]
fn status_includes_the_exact_switch_command_to_run() {
    let (_temp, repo, state) = run_fake_claude(1, true);
    let status = status_json(&repo, &state);
    let readiness = &status["switch_readiness"];
    assert_eq!(
        readiness["suggested_switch_command"],
        "handover switch codex"
    );
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

fn session_dir(state: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

fn arm(repo: &std::path::Path, state: &std::path::Path, args: &[&str]) {
    cargo_bin_cmd!("handover")
        .current_dir(repo)
        .env("HANDOVER_HOME", state)
        .args(args)
        .assert()
        .success();
}

/// A pending arm is a refusal this block could not see: `switch` rejects any
/// target that is not the armed one, so reporting `ready: true` alongside a
/// suggestion for the other provider sends the user at a command that cannot
/// run. Arming the provider that just ran makes the two differ, so the
/// suggestion has to follow the arm rather than the previous provider.
#[test]
fn a_pending_arm_blocks_readiness_and_the_suggestion_follows_it() {
    let (_temp, repo, state) = run_fake_claude(1, true);
    arm(&repo, &state, &["arm", "claude"]);

    let status = status_json(&repo, &state);
    let readiness = &status["switch_readiness"];
    assert_eq!(readiness["lease"], "free");
    assert_eq!(readiness["handover_renderable"], true);
    assert_eq!(
        readiness["ready"], false,
        "an armed switch is the only one that will be accepted"
    );
    assert_eq!(readiness["armed"]["to"], "claude");
    assert!(readiness["armed"]["sequence"].as_u64().unwrap() > 0);
    assert!(
        readiness["armed"]["expires_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(
        readiness["suggested_switch_command"],
        "handover switch claude"
    );
}

/// `status` holds no `SessionOperationLock`, so it must read a pending arm
/// without retiring an expired one — `crate::arm::pending` would append
/// `switch.expired`, which is a write from a command that promises none.
#[test]
fn status_sees_through_an_expired_arm_without_journaling_its_expiry() {
    let (_temp, repo, state) = run_fake_claude(1, true);
    arm(&repo, &state, &["arm", "codex", "--ttl", "1s"]);

    let journal = session_dir(&state).join("events.jsonl");
    let before = std::fs::read(&journal).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1_500));

    let status = status_json(&repo, &state);
    let readiness = &status["switch_readiness"];
    assert!(
        readiness["armed"].is_null(),
        "an expired arm is not pending"
    );
    assert_eq!(readiness["ready"], true);
    assert_eq!(
        readiness["suggested_switch_command"],
        "handover switch codex"
    );
    assert_eq!(
        std::fs::read(&journal).unwrap(),
        before,
        "status must append nothing, least of all switch.expired"
    );
}

#[test]
fn a_recoverable_lease_blocks_readiness_and_says_so() {
    let (_temp, repo, state) = run_fake_claude(1, true);
    let session = session_dir(&state);
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
    let session = session_dir(&state);
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
