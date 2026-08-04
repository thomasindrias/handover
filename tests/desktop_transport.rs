mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use handover::launch::TEST_LAUNCH_LOG_ENV;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

/// A fake claude that checkpoints, arms a switch to codex on the *desktop*
/// surface from outside the run, and exits non-zero. The exit code is the point
/// as much as the arm is: a desktop hop supervises nothing, so what the command
/// returns can only be this run's own exit.
fn fake_claude_that_arms_a_desktop_switch(
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
printf '%s' '{{"objective":"Hand over","summary":"Ready to switch.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Continue in the codex app"],"related_event_sequences":[]}}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
env -u HANDOVER_RUN_ID -u HANDOVER_SESSION_ID HANDOVER_HOME="{state}" "{handover}" arm codex --surface desktop >/dev/null
hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}}'
exit 7
"#,
        state = state.display(),
        handover = handover.display()
    );
    write_executable(&bin.join("claude"), &body);
}

/// A codex that answers `--version` — the claim gate probes it — and prints a
/// marker when it is *run*. The marker is the tripwire: a desktop hop must
/// never supervise it in this terminal.
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

struct Fixture {
    temp: TempDir,
    cwd: std::path::PathBuf,
    state: std::path::PathBuf,
    path: std::ffi::OsString,
}

fn fixture() -> Fixture {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let state = temp.path().join("state");
    let handover = assert_cmd::cargo::cargo_bin("handover");

    fake_claude_that_arms_a_desktop_switch(&bin, &handover, &state);
    fake_codex(&bin);
    let path = path_with(&bin);

    Fixture {
        temp,
        cwd,
        state,
        path,
    }
}

/// The transport `--surface desktop` selects: the application is opened, and
/// nothing is supervised in the terminal the finished run was using.
#[test]
fn an_armed_desktop_switch_opens_the_application_instead_of_supervising_a_successor() {
    let fixture = fixture();
    let launches = fixture.temp.path().join("desktop-launches");

    let output = cargo_bin_cmd!("handover")
        .current_dir(&fixture.cwd)
        .env("HANDOVER_HOME", &fixture.state)
        .env("PATH", &fixture.path)
        .env(TEST_LAUNCH_LOG_ENV, &launches)
        .args(["run", "claude"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Nothing came up in this terminal.
    assert!(
        !stdout.contains("codex-continued"),
        "a desktop switch must not supervise a successor here; stdout was: {stdout}"
    );
    // What would have been opened, recorded instead of opened. The path is the
    // saved cwd a supervised successor would have started in, not the target's
    // default and not this process's own.
    assert_eq!(
        std::fs::read_to_string(&launches).unwrap_or_else(|error| panic!(
            "the desktop launch was never attempted ({error}); stderr was: {stderr}"
        )),
        format!(
            "codex app {}\n",
            fixture.cwd.canonicalize().unwrap().display()
        )
    );
    assert!(
        stderr.contains("Opened codex's desktop application"),
        "the user must be told what was opened; stderr was: {stderr}"
    );
    // Supervising nothing, the command can only return the finished run's exit.
    assert_eq!(
        output.status.code(),
        Some(7),
        "the finished run's exit code must survive a desktop hop; stderr was: {stderr}"
    );

    let log = cargo_bin_cmd!("handover")
        .current_dir(&fixture.cwd)
        .env("HANDOVER_HOME", &fixture.state)
        .env("PATH", &fixture.path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let journal = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(
        journal.contains("switch.claimed"),
        "the arm must be claimed; journal was: {journal}"
    );
    assert!(
        !journal
            .lines()
            .any(|line| line.contains("\"run.started\"") && line.contains("\"codex\"")),
        "a desktop target must have no supervised run; journal was: {journal}"
    );
}

/// The third transport row: nothing was opened, and the command must neither
/// fail nor lie about what is left to do.
///
/// The failure is injected through the same seam the test above observes — the
/// capture log is pointed at a directory that does not exist — because the only
/// other way to make a launch fail is to attempt a real one.
#[test]
fn a_desktop_application_that_will_not_open_keeps_the_exit_code_and_names_the_handover() {
    let fixture = fixture();
    let unwritable = fixture
        .temp
        .path()
        .join("no-such-directory/desktop-launches");

    let output = cargo_bin_cmd!("handover")
        .current_dir(&fixture.cwd)
        .env("HANDOVER_HOME", &fixture.state)
        .env("PATH", &fixture.path)
        .env(TEST_LAUNCH_LOG_ENV, &unwritable)
        .args(["run", "claude"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(7),
        "a launch that did not happen must not fail a session that did its work; \
         stderr was: {stderr}"
    );
    assert!(
        stderr.contains("Could not open codex's desktop application"),
        "the failure must be reported; stderr was: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("codex-continued"),
        "a failed desktop launch must not fall back to supervising a successor"
    );

    // The advice has to be true of the state it is given in. The arm is spent,
    // so `handover claim` is not the recovery -- the rendered handover is.
    assert!(
        !stderr.contains("handover claim"),
        "the arm is already claimed, so nothing may point at `handover claim`; \
         stderr was: {stderr}"
    );
    assert!(
        stderr.contains("handover preview codex"),
        "the rendered handover must be named; stderr was: {stderr}"
    );

    let claim = cargo_bin_cmd!("handover")
        .current_dir(&fixture.cwd)
        .env("HANDOVER_HOME", &fixture.state)
        .env("PATH", &fixture.path)
        .args(["claim"])
        .output()
        .unwrap();
    assert!(
        !claim.status.success(),
        "the claim happened before the launch, so there must be nothing left to claim"
    );

    let preview = cargo_bin_cmd!("handover")
        .current_dir(&fixture.cwd)
        .env("HANDOVER_HOME", &fixture.state)
        .env("PATH", &fixture.path)
        .args(["preview", "codex"])
        .output()
        .unwrap();
    assert!(
        preview.status.success(),
        "the advice names `handover preview codex`, so it must work; stderr was: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
}
