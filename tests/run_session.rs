mod support;

use std::ffi::OsString;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::TempDir;

use support::{init_repo, write_executable};

#[test]
fn run_captures_hooks_and_returns_the_provider_exit_code() {
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
printf '%s' '{"session_id":"native-claude","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"SessionStart","source":"startup"}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
printf '%s' '{"session_id":"native-claude","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"UserPromptSubmit","prompt":"Implement OAuth"}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
printf '%s\n' 'provider stdout is inherited'
printf '%s\n' 'provider stderr is inherited' >&2
exit 23
"#,
    );
    write_executable(&bin.join("handover"), "#!/bin/sh\nexit 99\n");
    let path = path_with(&bin);
    let state = temp.path().join("state");

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", path)
        .arg("run")
        .arg("claude")
        .assert()
        .code(23)
        .stdout(predicate::str::contains("provider stdout is inherited"))
        .stderr(predicate::str::contains("provider stderr is inherited"));

    let sessions: Vec<_> = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(sessions.len(), 1);
    let lines = std::fs::read_to_string(sessions[0].join("events.jsonl")).unwrap();
    let types: Vec<_> = lines
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            value["event"]["type"].as_str().unwrap().to_owned()
        })
        .collect();
    for expected in [
        "session.created",
        "git.snapshot",
        "run.started",
        "run.handshake",
        "provider.prompt.submitted",
        "run.stopped",
    ] {
        assert!(
            types.iter().any(|kind| kind == expected),
            "missing {expected}"
        );
    }
    assert!(!sessions[0].join("refs/active-run.json").exists());
}

#[test]
fn duplicate_stable_hooks_append_one_event_and_one_followup_snapshot() {
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
start='{"session_id":"native","transcript_path":null,"cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
post='{"session_id":"native","transcript_path":null,"cwd":"'"$cwd_json"'","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"true"},"tool_response":{"stdout":"ok","stderr":"","exit_code":0},"tool_use_id":"tool-1"}'
printf '%s' "$start" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
printf '%s' "$start" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
printf '%s' "$post" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
printf '%s' "$post" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
    );
    let state = temp.path().join("state");

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["run", "claude"])
        .assert()
        .success();

    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let lines = std::fs::read_to_string(session.join("events.jsonl")).unwrap();
    let types: Vec<_> = lines
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            value["event"]["type"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(
        types.iter().filter(|kind| *kind == "run.handshake").count(),
        1
    );
    assert_eq!(
        types
            .iter()
            .filter(|kind| *kind == "provider.tool.completed")
            .count(),
        1
    );
    assert_eq!(
        types.iter().filter(|kind| *kind == "git.snapshot").count(),
        3
    );
}

fn path_with(bin: &std::path::Path) -> OsString {
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).unwrap()
}
