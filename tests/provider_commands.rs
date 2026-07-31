mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

/// A fake claude that writes a checkpoint and then arms a switch to codex
/// **from inside its own run** — no `env -u` scrubbing, exactly as the
/// `/handover-switch` command will. This is the moment slice 3 exists for.
fn fake_claude_that_arms_from_inside_the_run(bin: &std::path::Path) {
    let body = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
printf '%s' '{"objective":"Hand over","summary":"Ready to switch.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Continue in codex"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
"$HANDOVER_HOOK_BIN" arm codex --from-provider >/dev/null
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#;
    write_executable(&bin.join("claude"), body);
}

fn fake_codex(bin: &std::path::Path) {
    let body = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook codex >/dev/null; }
hook '{"session_id":"codex-native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
printf 'codex-continued\n'
hook '{"session_id":"codex-native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#;
    write_executable(&bin.join("codex"), body);
}

#[test]
fn an_attached_provider_arms_its_own_switch_through_the_cli() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let state = temp.path().join("state");

    fake_claude_that_arms_from_inside_the_run(&bin);
    fake_codex(&bin);
    let path = path_with(&bin);

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The provider armed from inside its own run, and quitting handed over.
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("codex-continued"),
        "stdout was: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&log.stdout);
    assert!(
        text.contains("switch.armed"),
        "the provider's arm must be journaled"
    );
    assert!(
        text.contains("switch.claimed"),
        "the arm must be claimed on exit"
    );
}

/// The environment proves which run this process belongs to; the cwd decides
/// which session the command acts on. A provider that wanders into another
/// worktree must not be able to arm the session it finds there.
#[test]
fn a_provider_cannot_arm_a_session_its_run_is_not_attached_to() {
    let temp = TempDir::new().unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let state = temp.path().join("state");
    fake_codex(&bin);
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
printf '%s' '{"objective":"Stay put","summary":"Own session.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Nothing"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
# The neighbouring worktree has its own Handover session. Arming it from here
# must be refused: this run is not attached to it.
if (cd "$HANDOVER_TEST_OTHER" && "$HANDOVER_HOOK_BIN" arm claude --from-provider) >/dev/null 2>&1; then
  printf 'cross-session-arm-succeeded\n'
fi
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
    let path = path_with(&bin);

    // Two independent repositories, each with its own Handover session.
    let other = temp.path().join("other");
    init_repo(&other);
    cargo_bin_cmd!("handover")
        .current_dir(&other)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["run", "codex"])
        .assert()
        .success();

    let repo = temp.path().join("repo");
    init_repo(&repo);
    let output = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_TEST_OTHER", &other)
        .env("PATH", &path)
        .args(["run", "claude"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("cross-session-arm-succeeded"),
        "a provider armed a session its run is not attached to; stdout was: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
