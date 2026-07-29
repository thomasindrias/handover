mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

/// A fake claude that writes a checkpoint, then arms a switch to codex from
/// *outside* the run (a second terminal — an attached provider cannot arm in
/// this slice), then exits. This is the in-session handover moment.
fn fake_claude_that_arms(
    bin: &std::path::Path,
    handover: &std::path::Path,
    state: &std::path::Path,
) {
    let body = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${{1:-}} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() {{ printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }}
hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}}'
printf '%s' '{{"objective":"Hand over","summary":"Ready to switch.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Continue in codex"],"related_event_sequences":[]}}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
env -u HANDOVER_RUN_ID -u HANDOVER_SESSION_ID HANDOVER_HOME="{state}" "{handover}" arm codex >/dev/null
hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}}'
exit 0
"#,
        state = state.display(),
        handover = handover.display()
    );
    write_executable(&bin.join("claude"), &body);
}

fn fake_codex(bin: &std::path::Path) {
    let body = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook codex >/dev/null; }
hook '{"session_id":"native-codex","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
printf 'codex-continued\n'
hook '{"session_id":"native-codex","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#;
    write_executable(&bin.join("codex"), body);
}

#[test]
fn arming_during_a_run_hands_over_to_the_target_when_the_provider_exits() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let state = temp.path().join("state");
    let handover = assert_cmd::cargo::cargo_bin("handover");

    fake_claude_that_arms(&bin, &handover, &state);
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

    // Codex ran in the same terminal, without a second `handover` invocation.
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("codex-continued"),
        "the armed target must launch when the provider exits; stdout was: {}",
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
    assert!(text.contains("switch.claimed"), "the arm must be claimed");
    assert!(
        text.contains("\"provider\":\"codex\""),
        "codex must have a run in the journal"
    );
}
