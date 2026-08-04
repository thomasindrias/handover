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

/// A fake claude that checkpoints and exits cleanly, arming nothing. The arm
/// under test is recorded from the terminal afterwards, so what claims it is
/// `handover switch` rather than this run's exit.
fn fake_claude(bin: &std::path::Path) {
    let body = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
printf '%s' '{"objective":"Hand over","summary":"Ready to switch.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Continue in the codex app"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#;
    write_executable(&bin.join("claude"), body);
}

struct Fixture {
    temp: TempDir,
    cwd: std::path::PathBuf,
    state: std::path::PathBuf,
    path: std::ffi::OsString,
}

impl Fixture {
    fn command(&self, args: &[&str]) -> std::process::Output {
        cargo_bin_cmd!("handover")
            .current_dir(&self.cwd)
            .env("HANDOVER_HOME", &self.state)
            .env("PATH", &self.path)
            .args(args)
            .output()
            .unwrap()
    }

    fn status(&self) -> serde_json::Value {
        let output = self.command(&["status", "--json"]);
        assert!(
            output.status.success(),
            "status must be readable after a desktop hop; stderr was: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
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

/// A finished `handover run claude` session with a switch to codex armed on
/// `surface` from the terminal — the arm `handover switch codex` then finds
/// pending and reuses.
fn armed_session(surface: &str) -> Fixture {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let state = temp.path().join("state");

    fake_claude(&bin);
    fake_codex(&bin);
    let path = path_with(&bin);
    let fixture = Fixture {
        temp,
        cwd,
        state,
        path,
    };

    let run = fixture.command(&["run", "claude"]);
    assert!(
        run.status.success(),
        "the arming run must finish; stderr was: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let arm = fixture.command(&["arm", "codex", "--surface", surface, "--json"]);
    assert!(
        arm.status.success(),
        "arming on the {surface} surface must be accepted; stderr was: {}",
        String::from_utf8_lossy(&arm.stderr)
    );
    let armed: serde_json::Value = serde_json::from_slice(&arm.stdout).unwrap();
    assert_eq!(
        armed["surface"], surface,
        "the arm records the surface asked for"
    );

    fixture
}

/// What a desktop hop must leave behind, whichever command claimed the arm.
///
/// Nothing is supervised, so the session must hold no lease and no supervised
/// binding for the target — that state is exactly what tier reporting reads,
/// and it is what lets the application that just opened attach over MCP and
/// read as attach tier. A lease created on this path would leave the session
/// looking supervised by a run that does not exist, so the assertion is on
/// `status`, not on the journal alone.
fn assert_the_target_is_attachable_rather_than_supervised(fixture: &Fixture) {
    let status = fixture.status();
    assert_eq!(
        status["switch_readiness"]["lease"], "free",
        "a desktop hop supervises nothing, so it must leave no lease; status was: {status}"
    );
    assert_ne!(
        status["binding"]["provider"], "codex",
        "the desktop target must not be bound as a supervised run; status was: {status}"
    );

    // The hop over: what the opened application does over MCP, done here
    // through the same projection's CLI form. It can only succeed against a
    // session nothing holds.
    let attach = fixture.command(&["attach", "codex", "--json"]);
    assert!(
        attach.status.success(),
        "the opened application must be able to attach; stderr was: {}",
        String::from_utf8_lossy(&attach.stderr)
    );
    let status = fixture.status();
    assert_eq!(
        status["binding"]["tier"], "attached",
        "a desktop session reads as attach tier; status was: {status}"
    );
    assert_eq!(status["binding"]["provider"], "codex");
    assert_eq!(status["binding"]["detached"], false);
    assert_eq!(
        status["switch_readiness"]["lease"], "free",
        "attaching supervises nothing either; status was: {status}"
    );
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
    assert_the_target_is_attachable_rather_than_supervised(&fixture);
}

/// The same arm, claimed by the porcelain instead of by a provider's exit, must
/// take the same transport. `switch` reuses a pending arm rather than recording
/// a second intent, and a reused arm's surface is the arm's to decide.
#[test]
fn switch_takes_the_desktop_transport_when_the_arm_it_reuses_asked_for_it() {
    let fixture = armed_session("desktop");
    let launches = fixture.temp.path().join("desktop-launches");

    let output = cargo_bin_cmd!("handover")
        .current_dir(&fixture.cwd)
        .env("HANDOVER_HOME", &fixture.state)
        .env("PATH", &fixture.path)
        .env(TEST_LAUNCH_LOG_ENV, &launches)
        .args(["switch", "codex"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains("codex-continued"),
        "a desktop arm must not be supervised in this terminal; stdout was: {stdout}"
    );
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
    // Nothing was supervised, so there is no child exit to relay: what this
    // reports is whether the switch it was asked for happened, and it did.
    assert_eq!(
        output.status.code(),
        Some(0),
        "an opened desktop switch is this command's whole job; stderr was: {stderr}"
    );

    let journal = String::from_utf8_lossy(&fixture.command(&["log", "--json"]).stdout).into_owned();
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
    assert_the_target_is_attachable_rather_than_supervised(&fixture);
}

/// `switch`'s third transport row. It supervised nothing, so it has no run's
/// exit code to keep — and no reason to claim success for an application that
/// never came up. It must not fall back to the terminal either: the handover is
/// already committed, and the arm that chose the desktop is spent.
#[test]
fn a_switch_whose_application_will_not_open_says_so_and_does_not_report_success() {
    let fixture = armed_session("desktop");
    let unwritable = fixture
        .temp
        .path()
        .join("no-such-directory/desktop-launches");

    let output = cargo_bin_cmd!("handover")
        .current_dir(&fixture.cwd)
        .env("HANDOVER_HOME", &fixture.state)
        .env("PATH", &fixture.path)
        .env(TEST_LAUNCH_LOG_ENV, &unwritable)
        .args(["switch", "codex"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_ne!(
        output.status.code(),
        Some(0),
        "an application that did not open is not a switch that landed; stderr was: {stderr}"
    );
    assert!(
        stderr.contains("Could not open codex's desktop application"),
        "the failure must be reported; stderr was: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("codex-continued"),
        "a failed desktop launch must not fall back to supervising a successor"
    );
    // No finished run to account for here, so the line must not invent one --
    // and the arm is spent, so the recovery is the rendered handover.
    assert!(
        !stderr.contains("exited with"),
        "nothing exited: this command supervised nothing; stderr was: {stderr}"
    );
    assert!(
        !stderr.contains("handover claim"),
        "the arm is already claimed, so nothing may point at `handover claim`; \
         stderr was: {stderr}"
    );
    assert!(
        stderr.contains("handover preview codex"),
        "the rendered handover must be named; stderr was: {stderr}"
    );
    assert_the_target_is_attachable_rather_than_supervised(&fixture);
}

/// The other half of the mapping: an arm that explicitly asked for the CLI
/// surface is supervised in this terminal, and opens no application.
///
/// `Surface::Auto` behaves this way too, but only `Auto` is pinned elsewhere —
/// so a `Cli` arm routed to the desktop transport would otherwise go unnoticed.
#[test]
fn an_arm_that_asked_for_the_cli_surface_is_supervised_in_this_terminal() {
    let fixture = armed_session("cli");
    let launches = fixture.temp.path().join("desktop-launches");

    let output = cargo_bin_cmd!("handover")
        .current_dir(&fixture.cwd)
        .env("HANDOVER_HOME", &fixture.state)
        .env("PATH", &fixture.path)
        .env(TEST_LAUNCH_LOG_ENV, &launches)
        .args(["switch", "codex"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("codex-continued"),
        "an arm for the CLI surface is supervised here; stdout was: {stdout}, stderr: {stderr}"
    );
    // The seam was armed, so anything that opened an application would have
    // left a line here.
    assert!(
        !launches.exists(),
        "the CLI surface must open no application; the launch log holds: {}",
        std::fs::read_to_string(&launches).unwrap_or_default()
    );
    assert!(
        !stderr.contains("desktop application"),
        "nothing may claim to have opened an application; stderr was: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "the supervised successor's exit is what this returns; stderr was: {stderr}"
    );

    let journal = String::from_utf8_lossy(&fixture.command(&["log", "--json"]).stdout).into_owned();
    assert!(
        journal
            .lines()
            .any(|line| line.contains("\"run.started\"") && line.contains("\"codex\"")),
        "the successor ran here, so it has a supervised run; journal was: {journal}"
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
