mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

const CHECKPOINT_JSON: &str = r#"{"objective":"Implement OAuth callback","summary":"PKCE support is complete.","decisions":[],"assumptions":[],"constraints":[],"completed":["capture"],"in_progress":[],"blockers":[],"next_steps":["Fix callback integration test"],"related_event_sequences":[]}"#;

/// Fake claude: SessionStart, `cycles` recognized tool cycles, optionally a
/// provider checkpoint, then Stop with its hook stdout captured to
/// $NUDGE_CAPTURE.
fn fake_claude(bin: &std::path::Path, cycles: u32, checkpoint_before_stop: bool) {
    let checkpoint_line = if checkpoint_before_stop {
        format!(
            r#"printf '%s' '{CHECKPOINT_JSON}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider"#
        )
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
printf '%s' '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}}' | "$HANDOVER_HOOK_BIN" __hook claude > "$NUDGE_CAPTURE"
exit 0
"#
    );
    write_executable(&bin.join("claude"), &body);
}

struct NudgeRun {
    _temp: TempDir,
    repo: std::path::PathBuf,
    state: std::path::PathBuf,
    capture: std::path::PathBuf,
}

fn run_fake_claude(cycles: u32, checkpoint_before_stop: bool) -> NudgeRun {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_claude(&bin, cycles, checkpoint_before_stop);
    let state = temp.path().join("state");
    let capture = temp.path().join("stop-output.json");
    let path = path_with(&bin);

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .env("NUDGE_CAPTURE", &capture)
        .args(["run", "claude"])
        .assert()
        .success();

    NudgeRun {
        _temp: temp,
        repo,
        state,
        capture,
    }
}

fn status_json(run: &NudgeRun) -> serde_json::Value {
    let output = cargo_bin_cmd!("handover")
        .current_dir(&run.repo)
        .env("HANDOVER_HOME", &run.state)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn stop_warns_with_a_single_system_message_once_twenty_events_are_stale() {
    let run = run_fake_claude(7, false);

    let stdout = std::fs::read_to_string(&run.capture).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["systemMessage"]);
    let message = value["systemMessage"].as_str().unwrap();
    assert!(
        message.contains("events and no narrative checkpoint yet"),
        "unexpected message: {message}"
    );
    assert!(message.starts_with("Handover: "));
    assert!(message.ends_with("run `handover checkpoint`."));

    let status = status_json(&run);
    assert!(status["latest_narrative_checkpoint"].is_null());
    assert!(status["events_since_narrative"].as_u64().unwrap() >= 20);
}

#[test]
fn a_fresh_narrative_checkpoint_suppresses_the_stop_warning() {
    let run = run_fake_claude(7, true);

    let stdout = std::fs::read_to_string(&run.capture).unwrap();
    assert!(
        stdout.is_empty(),
        "expected empty Stop output, got: {stdout}"
    );

    let status = status_json(&run);
    assert!(status["latest_narrative_checkpoint"].as_u64().is_some());
    assert!(status["events_since_narrative"].as_u64().unwrap() < 20);
}

#[test]
fn below_threshold_activity_keeps_the_stop_output_empty() {
    let run = run_fake_claude(1, false);

    let stdout = std::fs::read_to_string(&run.capture).unwrap();
    assert!(
        stdout.is_empty(),
        "expected empty Stop output, got: {stdout}"
    );

    let status = status_json(&run);
    assert!(status["latest_narrative_checkpoint"].is_null());
    assert!(status["events_since_narrative"].as_u64().unwrap() < 20);
}
