mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use handover::model::{Provider, RunId, SessionId};
use handover::store::lease::{LeaseStore, ProcessIdentity, RunLease};
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

/// The lone session directory under `<state>/sessions`, and its id.
fn session_dir_and_id(state: &std::path::Path) -> (std::path::PathBuf, SessionId) {
    let dir = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let id = SessionId::parse(dir.file_name().unwrap().to_str().unwrap()).unwrap();
    (dir, id)
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

#[test]
fn claim_consumes_the_arm_and_prints_the_handover() {
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
        .args(["claim"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Ship arm"));

    // The arm is one-shot: a second claim finds nothing.
    let second = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["claim"])
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("no switch is armed"),
        "stderr was: {}",
        String::from_utf8_lossy(&second.stderr)
    );
}

#[test]
fn claim_refuses_when_the_asserted_arm_is_not_the_pending_one() {
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
        .args(["claim", "--arm", "999"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not 999"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_expired_arm_is_retired_lazily_and_cannot_be_claimed() {
    let (_temp, cwd, state, path) = finished_session();

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex", "--ttl", "1s"])
        .assert()
        .success();

    std::thread::sleep(std::time::Duration::from_millis(1_100));

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["claim"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no switch is armed"));

    // Expiry is journaled at the moment it is observed.
    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&log.stdout).contains("switch.expired"));
}

/// `arm` captures `armed_run` from whatever lease exists at arm time, so a
/// lease exercising `release_for_claim`'s "belongs to the arming run" branch
/// has to be planted *before* `arm` runs, with the run id `arm` will record.
#[test]
fn claim_clears_a_dead_lease_left_by_the_arming_run_without_prompting() {
    let (_temp, cwd, state, path) = finished_session();
    let (session, session_id) = session_dir_and_id(&state);
    let leases = LeaseStore::new(&session);

    let dead = RunLease::new(
        session_id,
        RunId::new(),
        Provider::Claude,
        ProcessIdentity {
            pid: u32::MAX,
            start_token: "gone".into(),
        },
    )
    .unwrap();
    leases.create(&dead).unwrap();

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
        .args(["claim"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Ship arm"));

    assert!(
        leases.read().unwrap().is_none(),
        "the dead lease left by the arming run should be cleared, not merely ignored"
    );
}

#[test]
fn claim_refuses_while_the_arming_runs_provider_is_still_live() {
    let (_temp, cwd, state, path) = finished_session();
    let (session, session_id) = session_dir_and_id(&state);
    let leases = LeaseStore::new(&session);

    // The current test process stands in for the still-running provider: it
    // is unquestionably live for the duration of this test.
    let live = RunLease::new(
        session_id,
        RunId::new(),
        Provider::Claude,
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .unwrap();
    leases.create(&live).unwrap();

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
        .args(["claim"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is still running"), "stderr was: {stderr}");
    assert!(stderr.contains("claude"), "stderr was: {stderr}");

    // Refused, not consumed: the live lease is untouched.
    assert_eq!(leases.read().unwrap().unwrap().run_id, live.run_id);

    leases.clear(&live.run_id).unwrap();
}

#[test]
fn claim_refuses_when_a_different_run_holds_the_lease() {
    let (_temp, cwd, state, path) = finished_session();
    let (session, session_id) = session_dir_and_id(&state);
    let leases = LeaseStore::new(&session);

    let arming_run = RunLease::new(
        session_id.clone(),
        RunId::new(),
        Provider::Claude,
        ProcessIdentity {
            pid: u32::MAX,
            start_token: "gone".into(),
        },
    )
    .unwrap();
    leases.create(&arming_run).unwrap();

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex"])
        .assert()
        .success();

    // Before the claim lands, a different run takes the session's lease --
    // e.g. a fresh `handover run` started after the arm. Its run id was
    // never seen by `arm`, so it cannot be the one that authorised the
    // switch.
    leases.clear(&arming_run.run_id).unwrap();
    let foreign_run = RunLease::new(
        session_id,
        RunId::new(),
        Provider::Claude,
        ProcessIdentity {
            pid: u32::MAX,
            start_token: "also gone".into(),
        },
    )
    .unwrap();
    leases.create(&foreign_run).unwrap();

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["claim"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("was not created by the run that armed the switch"),
        "stderr was: {stderr}"
    );
    // The classify_lease diagnostic for a dead, unrelated lease: it reports
    // it as recoverable rather than blocked.
    assert!(
        stderr.contains("stale") && stderr.contains("recover"),
        "expected the classify_lease diagnostic in stderr, got: {stderr}"
    );

    leases.clear(&foreign_run.run_id).unwrap();
}
