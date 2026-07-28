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

/// The handover is the product, and every sequence it names is
/// independently checkable against the journal. `claim` crosses a provider
/// boundary, so it must commit a real transition checkpoint and describe
/// that one -- never a number that no event answers to.
#[test]
fn the_handover_claim_emits_names_a_transition_checkpoint_that_exists() {
    let (_temp, cwd, state, path) = finished_session();

    let run = |args: &[&str]| {
        cargo_bin_cmd!("handover")
            .current_dir(&cwd)
            .env("HANDOVER_HOME", &state)
            .env("PATH", &path)
            .args(args)
            .output()
            .unwrap()
    };

    let armed: serde_json::Value =
        serde_json::from_slice(&run(&["arm", "codex", "--json"]).stdout).unwrap();
    let armed_sequence = armed["armed_sequence"].as_u64().unwrap();

    let claimed = run(&["claim", "--json"]);
    assert!(
        claimed.status.success(),
        "{}",
        String::from_utf8_lossy(&claimed.stderr)
    );
    let claimed: serde_json::Value = serde_json::from_slice(&claimed.stdout).unwrap();
    let transition = claimed["transition"]["sequence"].as_u64().unwrap();

    // What the emitted document tells the next provider.
    let markdown = claimed["markdown"].as_str().unwrap();
    assert!(
        markdown.contains(&format!("- Transition event sequence: {transition}\n")),
        "handover was: {markdown}"
    );
    assert!(
        markdown.contains(&format!(
            "## Transition checkpoint\n\n- Event sequence: {transition}\n- Includes committed facts through sequence: {}\n",
            transition - 1
        )),
        "handover was: {markdown}"
    );

    // What the journal says about that same sequence.
    let log = run(&["log", "--json"]);
    let journal: Vec<serde_json::Value> = String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let event = journal
        .iter()
        .map(|envelope| &envelope["event"])
        .find(|event| event["sequence"] == transition)
        .unwrap_or_else(|| {
            panic!("the handover names transition {transition}, absent from the journal")
        });
    assert_eq!(event["type"], "checkpoint.created");
    assert_eq!(event["payload"]["checkpoint_kind"], "transition");
    assert_eq!(event["payload"]["through_sequence"], transition - 1);

    // The checkpoint the event points at is on disk too.
    let (session, _) = session_dir_and_id(&state);
    assert!(
        session
            .join(event["payload"]["path"].as_str().unwrap())
            .is_file()
    );

    // And the claim records which committed prefix it handed over, so the
    // journal alone answers "what did the switching provider receive?".
    let claim_event = journal
        .iter()
        .map(|envelope| &envelope["event"])
        .find(|event| event["type"] == "switch.claimed")
        .expect("the claim must be journaled");
    assert_eq!(claim_event["payload"]["armed_sequence"], armed_sequence);
    assert_eq!(claim_event["payload"]["through_sequence"], transition);
    assert!(claim_event["sequence"].as_u64().unwrap() > transition);
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

#[test]
fn attach_binds_a_fresh_worktree_and_is_idempotent() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");

    let first = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["attach", "claude", "--json"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["created"], true);
    assert_eq!(first["provider"], "claude");

    let second = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["attach", "codex", "--json"])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();

    assert_eq!(second["created"], false);
    assert_eq!(
        second["session_id"], first["session_id"],
        "attach must resolve to the existing session, never create a second one"
    );
}

#[test]
fn attach_refuses_while_a_live_lease_holds_the_session() {
    use handover::model::{Provider, RunId, SessionId};
    use handover::store::lease::{LeaseStore, ProcessIdentity, RunLease};

    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");

    let created = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["attach", "claude", "--json"])
        .output()
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let session_id = SessionId::parse(created["session_id"].as_str().unwrap()).unwrap();

    let session_dir = state.join("sessions").join(session_id.to_string());
    let lease = RunLease::new(
        session_id,
        RunId::new(),
        Provider::Claude,
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .unwrap();
    LeaseStore::new(&session_dir).create(&lease).unwrap();

    let output = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["attach", "codex"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("still running"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A merely stale/dead lease must not make `attach` refuse: only `"blocked"`
/// refuses, and a dead lease classifies as `"recoverable"`. This is the
/// row of the design table the feature exists for.
#[test]
fn attach_succeeds_when_the_session_has_only_a_stale_lease() {
    use handover::model::{Provider, RunId, SessionId};
    use handover::store::lease::{LeaseStore, ProcessIdentity, RunLease};

    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");

    let created = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["attach", "claude", "--json"])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let session_id = SessionId::parse(created["session_id"].as_str().unwrap()).unwrap();

    let session_dir = state.join("sessions").join(session_id.to_string());
    let dead = RunLease::new(
        session_id.clone(),
        RunId::new(),
        Provider::Claude,
        ProcessIdentity {
            pid: u32::MAX,
            start_token: "gone".into(),
        },
    )
    .unwrap();
    LeaseStore::new(&session_dir).create(&dead).unwrap();

    let output = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["attach", "codex", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "a stale/dead lease is recoverable, not blocked; attach must not refuse: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["created"], false);
    assert_eq!(
        value["session_id"], created["session_id"],
        "attach must resolve to the existing session, not create a new one"
    );
}

/// `provider_command_allowed` in `src/app.rs` allows only `Hook`,
/// `Checkpoint { from_provider: true }`, and `McpServer` once
/// `HANDOVER_RUN_ID` marks the process as an attached provider. `Arm`,
/// `Claim`, and `Attach` mutate state and are deliberately not in that list
/// in this slice; MCP access is gated by run-scoping in a later slice. Pin
/// the refusal so nobody widens the allow-list by accident.
#[test]
fn attached_provider_processes_cannot_arm_claim_or_attach() {
    let (_temp, cwd, state, path) = finished_session();

    for args in [vec!["arm", "codex"], vec!["claim"], vec!["attach", "codex"]] {
        let output = cargo_bin_cmd!("handover")
            .current_dir(&cwd)
            .env("HANDOVER_HOME", &state)
            .env("PATH", &path)
            .env("HANDOVER_RUN_ID", "22222222-2222-4222-8222-222222222222")
            .args(&args)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{args:?} must be refused inside a provider run"
        );
    }
}

/// The layering contract from the spec: the experience layer holds no
/// logic, so a complete arm/claim cycle must work with nothing but the CLI
/// -- no MCP server configured anywhere in this test.
#[test]
fn the_full_cycle_runs_on_the_cli_alone_with_no_mcp_server() {
    let (_temp, cwd, state, path) = finished_session();

    let run = |args: &[&str]| {
        cargo_bin_cmd!("handover")
            .current_dir(&cwd)
            .env("HANDOVER_HOME", &state)
            .env("PATH", &path)
            .args(args)
            .output()
            .unwrap()
    };

    let armed: serde_json::Value =
        serde_json::from_slice(&run(&["arm", "codex", "--json"]).stdout).unwrap();
    let sequence = armed["armed_sequence"].as_u64().unwrap().to_string();

    let claimed = run(&["claim", "--arm", &sequence]);
    assert!(
        claimed.status.success(),
        "{}",
        String::from_utf8_lossy(&claimed.stderr)
    );
    assert!(String::from_utf8_lossy(&claimed.stdout).contains("Ship arm"));

    let log = String::from_utf8_lossy(&run(&["log", "--json"]).stdout).into_owned();
    assert!(log.contains("switch.armed"));
    assert!(log.contains("switch.claimed"));

    // The lease is left free for the next launcher.
    let status: serde_json::Value =
        serde_json::from_slice(&run(&["status", "--json"]).stdout).unwrap();
    assert_eq!(status["switch_readiness"]["lease"], "free");
}

/// `SessionOperationLock` is what makes claiming atomic. Spawning several
/// concurrent `handover claim` processes against one armed switch must
/// leave exactly one winner: zero means the lock deadlocked, more than one
/// means `crate::arm::pending` was read outside the lock in `claim_command`.
#[test]
fn concurrent_claims_produce_exactly_one_winner() {
    let (_temp, cwd, state, path) = finished_session();

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex"])
        .assert()
        .success();

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let cwd = cwd.clone();
            let state = state.clone();
            let path = path.clone();
            std::thread::spawn(move || {
                cargo_bin_cmd!("handover")
                    .current_dir(&cwd)
                    .env("HANDOVER_HOME", &state)
                    .env("PATH", &path)
                    .args(["claim"])
                    .output()
                    .unwrap()
                    .status
                    .success()
            })
        })
        .collect();

    let winners = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(|succeeded| *succeeded)
        .count();
    assert_eq!(
        winners, 1,
        "an arm is one-shot even under concurrent claims"
    );
}

/// Fake claude: SessionStart, `cycles` recognized tool cycles, then Stop --
/// no narrative checkpoint is ever written. Mirrors the configurable-cycles
/// fake provider idiom in `tests/switch_readiness.rs`, whose own
/// `a_stale_narrative_checkpoint_is_advisory_and_does_not_block_readiness`
/// test proves 7 cycles pushes `events_since` past
/// `STALE_NARRATIVE_EVENT_THRESHOLD` (20) with no checkpoint recorded.
fn fake_claude_without_narrative_checkpoint(bin: &std::path::Path, cycles: u32) {
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
hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}}'
exit 0
"#
    );
    write_executable(&bin.join("claude"), &body);
}

/// A finished `handover run claude` session with `cycles` tool cycles and no
/// narrative checkpoint: enough events accumulate that `arm` will observe a
/// stale narrative when it looks.
fn session_with_stale_narrative(
    cycles: u32,
) -> (
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
    fake_claude_without_narrative_checkpoint(&bin, cycles);
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

/// `arm` must not be stricter than the `switch` readiness gate it precedes:
/// a stale narrative checkpoint is advisory everywhere else in this
/// codebase (see `switch_readiness.rs`), so `arm` must warn on stderr and
/// proceed, never refuse, reporting `checkpoint_fresh: false` in its JSON.
#[test]
fn arm_warns_on_stderr_but_succeeds_past_a_stale_narrative_checkpoint() {
    let (_temp, cwd, state, path) = session_with_stale_narrative(7);

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "a stale narrative checkpoint must warn, not refuse arm: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("events since the last narrative checkpoint"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["checkpoint_fresh"], false);
}
