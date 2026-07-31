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

/// A fake claude that arms a switch to codex, then moves the session's saved
/// cwd to a directory it deletes on the way out. The run itself succeeds; the
/// handover that follows it cannot resolve a saved cwd that is gone.
fn fake_claude_that_arms_then_loses_the_saved_cwd(
    bin: &std::path::Path,
    handover: &std::path::Path,
    state: &std::path::Path,
) {
    let body = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${{1:-}} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
json() {{ printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }}
hook() {{ printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }}
hook '{{"session_id":"native","cwd":"'"$(json "$PWD")"'","hook_event_name":"SessionStart"}}'
env -u HANDOVER_RUN_ID -u HANDOVER_SESSION_ID HANDOVER_HOME="{state}" "{handover}" arm codex >/dev/null
scratch="$(dirname "$PWD")/scratch"
mkdir -p "$scratch"
hook '{{"session_id":"native","cwd":"'"$(json "$scratch")"'","hook_event_name":"Stop"}}'
rmdir "$scratch"
exit 0
"#,
        state = state.display(),
        handover = handover.display()
    );
    write_executable(&bin.join("claude"), &body);
}

/// A fake codex that records the argv it was launched with, so the test can
/// read what the successor of a claimed arm actually received.
fn fake_codex_recording_argv(bin: &std::path::Path, argv_log: &std::path::Path) {
    let body = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${{1:-}} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
printf '%s\n' "$@" > "{argv_log}"
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() {{ printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook codex >/dev/null; }}
hook '{{"session_id":"native-codex","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}}'
printf 'codex-continued\n'
hook '{{"session_id":"native-codex","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}}'
exit 0
"#,
        argv_log = argv_log.display()
    );
    write_executable(&bin.join("codex"), &body);
}

/// A codex that cannot report a version. `handover arm` never probes its
/// target, so an arm for a provider that will not start is accepted — and a
/// provider that is not on PATH at all fails the same `probe()` this one does,
/// which is the gate under test.
fn fake_codex_that_cannot_start(bin: &std::path::Path) {
    let body = r#"#!/usr/bin/env bash
printf '%s\n' 'codex: command not found' >&2
exit 127
"#;
    write_executable(&bin.join("codex"), body);
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

#[test]
fn a_handover_that_cannot_render_keeps_the_finished_run_s_exit_code() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let state = temp.path().join("state");
    let handover = assert_cmd::cargo::cargo_bin("handover");

    fake_claude_that_arms_then_loses_the_saved_cwd(&bin, &handover, &state);
    fake_codex(&bin);
    let path = path_with(&bin);

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .output()
        .unwrap();

    // The run did its work and exited 0. The handover is a separate, still
    // recoverable fact, so it must not turn that into a failure.
    assert_eq!(
        output.status.code(),
        Some(0),
        "the finished run's exit code must survive a handover that cannot \
         render; stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Handover did not complete"),
        "the failure must be reported on stderr; stderr was: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("codex-continued"),
        "codex must not launch when the handover to it did not happen"
    );

    // Restore the saved cwd so the session is readable again, and prove the arm
    // survived: nothing was claimed, and `handover claim` still has one to take.
    std::fs::create_dir_all(repo.join("apps/scratch")).unwrap();
    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&log.stdout);
    assert!(text.contains("switch.armed"), "the arm must be recorded");
    assert!(
        !text.contains("switch.claimed"),
        "a handover that did not render must leave the arm unclaimed; \
         journal was: {text}"
    );
    assert!(
        !text.contains("checkpoint.created"),
        "nothing may be half-committed by a refused handover; journal was: {text}"
    );

    let claim = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["claim"])
        .output()
        .unwrap();
    assert!(
        claim.status.success(),
        "the arm must still be claimable by hand; stderr was: {}",
        String::from_utf8_lossy(&claim.stderr)
    );
}

/// The gate that precedes the claim has to cover the successor's ability to
/// start, not only the document's ability to render.
///
/// Otherwise the overwhelmingly likely failure — an arm for a provider that is
/// not installed, which `arm` accepts because it never probes — is spent: the
/// claim commits, the launch then fails, a session that did its work exits
/// non-zero, and the recovery the error would have suggested (`handover
/// claim`) is no longer available, because the arm is gone.
#[test]
fn a_successor_that_cannot_launch_keeps_the_exit_code_and_the_arm() {
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
    fake_codex_that_cannot_start(&bin);
    let path = path_with(&bin);

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "the finished run's exit code must survive a successor that cannot \
         start; stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Handover did not complete"),
        "the failure must be reported on stderr; stderr was: {stderr}"
    );

    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&log.stdout);
    assert!(text.contains("switch.armed"), "the arm must be recorded");
    assert!(
        !text.contains("switch.claimed"),
        "a successor that cannot start must not consume the arm; \
         journal was: {text}"
    );
    assert!(
        !text.contains("\"provider\":\"codex\""),
        "codex must have no run in the journal; journal was: {text}"
    );

    // The advice the loop printed has to be true: the arm is still there to
    // take, and `handover claim` does not need codex to run.
    let claim = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["claim"])
        .output()
        .unwrap();
    assert!(
        claim.status.success(),
        "the arm must still be claimable by hand; stderr was: {}",
        String::from_utf8_lossy(&claim.stderr)
    );
}

#[test]
fn a_claimed_hop_does_not_inherit_the_finished_run_s_provider_args() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let state = temp.path().join("state");
    let argv_log = temp.path().join("codex-argv");
    let handover = assert_cmd::cargo::cargo_bin("handover");

    fake_claude_that_arms(&bin, &handover, &state);
    fake_codex_recording_argv(&bin, &argv_log);
    let path = path_with(&bin);

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude", "--", "--claude-only-flag"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("codex-continued"),
        "the armed target must launch when the provider exits; stdout was: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    // Codex was launched by the claim, not by the user, so it gets the args a
    // `handover switch codex` with no trailing args would have given it.
    let argv = std::fs::read_to_string(&argv_log).unwrap();
    assert!(
        !argv.contains("--claude-only-flag"),
        "claude's flags must not reach codex's argv; argv was: {argv}"
    );

    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let journal = String::from_utf8_lossy(&log.stdout).into_owned();
    let codex_started: Vec<&str> = journal
        .lines()
        .filter(|line| line.contains("\"run.started\"") && line.contains("\"provider\":\"codex\""))
        .collect();
    assert_eq!(
        codex_started.len(),
        1,
        "codex must have exactly one run.started"
    );
    assert!(
        codex_started[0].contains("\"args\":[]"),
        "the successor's run.started must record no args; event was: {}",
        codex_started[0]
    );
}
