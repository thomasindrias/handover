mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

/// Fake claude that completes a session and writes one narrative checkpoint,
/// so the rendered handover has narrative to carry.
fn fake_claude(bin: &std::path::Path) {
    let body = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
printf '%s' '{"objective":"Ship arm","summary":"Armed and ready.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Claim it"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#;
    write_executable(&bin.join("claude"), body);
}

/// A finished `handover run claude` session: temp dir, cwd, and state root.
/// The `TempDir` must stay bound in the caller — dropping it deletes the repo.
fn finished_session() -> (
    TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::ffi::OsString,
) {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_claude(&bin);
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    (temp, cwd, state, path)
}

#[test]
fn arm_records_the_target_and_an_expiry_without_launching_anything() {
    let (_temp, cwd, state, path) = finished_session();

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex", "--ttl", "15m", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["to"], "codex");
    assert_eq!(value["surface"], "auto");
    assert!(value["armed_sequence"].as_u64().unwrap() > 0);
    assert!(value["expires_at"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn arm_refuses_a_second_pending_arm() {
    let (_temp, cwd, state, path) = finished_session();

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex"])
        .assert()
        .success();

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("already armed"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
