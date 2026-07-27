mod support;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

#[test]
fn capture_failure_denies_work_until_doctor_repairs_the_private_sentinel() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let trace = temp.path().join("trace");
    std::fs::create_dir(&trace).unwrap();
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--help" ]]; then printf '%s\n' '--plugin-dir --add-dir'; exit 0; fi
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake 1'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
sleep 0.5
journal="$HANDOVER_HOME/sessions/$HANDOVER_SESSION_ID/events.jsonl"
chmod 0400 "$journal"
first=$(printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"touch intended"},"tool_use_id":"tool-1"}' | "$HANDOVER_HOOK_BIN" __hook claude)
printf '%s' "$first" > "$HANDOVER_TEST_TRACE/first.json"
if [[ $first != *permissionDecision*deny* ]]; then touch "$HANDOVER_TEST_TRACE/intended"; fi
chmod 0600 "$journal"
touch "$HANDOVER_TEST_TRACE/ready"
while [[ ! -e "$HANDOVER_TEST_TRACE/continue" ]]; do sleep 0.05; done
second=$(printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"touch intended"},"tool_use_id":"tool-2"}' | "$HANDOVER_HOOK_BIN" __hook claude)
printf '%s' "$second" > "$HANDOVER_TEST_TRACE/second.json"
if [[ -z $second ]]; then touch "$HANDOVER_TEST_TRACE/intended"; fi
exit 0
"#,
    );
    write_executable(
        &bin.join("codex"),
        "#!/bin/sh\nif [ \"${1:-}\" = --help ]; then echo '--config --add-dir --cd'; exit 0; fi\nif [ \"${1:-}\" = features ]; then echo 'hooks stable true'; exit 0; fi\nexit 0\n",
    );
    let state = temp.path().join("state");
    let path = path_with(&bin);
    let mut run = Command::new(env!("CARGO_BIN_EXE_handover"))
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_TEST_TRACE", &trace)
        .env("PATH", &path)
        .args(["run", "claude"])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    wait_for(&trace.join("ready"));

    assert!(
        std::fs::read_to_string(trace.join("first.json"))
            .unwrap()
            .contains("permissionDecision")
    );
    assert!(!trace.join("intended").exists());
    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let run_dir = std::fs::read_dir(session.join("runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(run_dir.join("capture-failed.json").exists());

    let repair = Command::new(env!("CARGO_BIN_EXE_handover"))
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["doctor", "--json", "--repair"])
        .output()
        .unwrap();
    let diagnostics: Vec<serde_json::Value> = serde_json::from_slice(&repair.stdout).unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|item| item["code"] == "capture.sentinel_removed")
    );
    assert!(!run_dir.join("capture-failed.json").exists());

    std::fs::write(trace.join("continue"), b"go\n").unwrap();
    assert!(run.wait().unwrap().success());
    assert_eq!(std::fs::read(trace.join("second.json")).unwrap(), b"");
    assert!(trace.join("intended").exists());
}

fn wait_for(path: &std::path::Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
}
