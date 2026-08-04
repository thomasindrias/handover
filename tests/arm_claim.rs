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

/// Fake codex that completes a session, so `switch`/`claim` have a second
/// provider on PATH to launch into.
fn fake_codex(bin: &std::path::Path) {
    let body = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook codex >/dev/null; }
hook '{"session_id":"codex-native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
hook '{"session_id":"codex-native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#;
    write_executable(&bin.join("codex"), body);
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
    fake_codex(&bin);
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

/// A process identity that cannot be live: `u32::MAX` is above every
/// reachable pid, and the start token would not match even if it were.
fn dead_identity() -> ProcessIdentity {
    ProcessIdentity {
        pid: u32::MAX,
        start_token: "gone".into(),
    }
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

/// The lone run directory under a session, and its id.
///
/// `finished_session()` runs exactly one provider, and run directories outlive
/// the run that made them — only `handover delete` removes them. That is what
/// lets a test speak as a real, finished run.
fn run_dir_and_id(session: &std::path::Path) -> (std::path::PathBuf, RunId) {
    let dir = std::fs::read_dir(session.join("runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let id = RunId::parse(dir.file_name().unwrap().to_str().unwrap()).unwrap();
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

    // And the claim points at that checkpoint, so the journal alone answers
    // "what did the switching provider receive?". The pointer is the
    // checkpoint's own event sequence, not the committed prefix -- the prefix
    // is one lower, and lives on the checkpoint asserted above.
    let claim_event = journal
        .iter()
        .map(|envelope| &envelope["event"])
        .find(|event| event["type"] == "switch.claimed")
        .expect("the claim must be journaled");
    assert_eq!(claim_event["payload"]["armed_sequence"], armed_sequence);
    assert_eq!(
        claim_event["payload"]["transition_checkpoint_sequence"],
        transition
    );
    assert_ne!(
        claim_event["payload"]["transition_checkpoint_sequence"],
        event["payload"]["through_sequence"],
        "a pointer to the checkpoint is not the checkpoint's committed prefix"
    );
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

/// The one privilege an arm carries: releasing a *dead* lease belonging to the
/// run that armed it, with no `[y/N]` prompt. Reaching it requires the caller
/// to *be* that run, so this arms from inside the run, against a lease that run
/// left behind — a supervisor killed before the teardown that clears it.
#[test]
fn claim_clears_a_dead_lease_left_by_the_arming_run_without_prompting() {
    let (_temp, cwd, state, path) = finished_session();
    let (session, session_id) = session_dir_and_id(&state);
    let (run_dir, run_id) = run_dir_and_id(&session);
    let leases = LeaseStore::new(&session);

    let dead = RunLease::new(
        session_id.clone(),
        run_id.clone(),
        Provider::Claude,
        dead_identity(),
    )
    .unwrap();
    leases.create(&dead).unwrap();

    let armed = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .env("HANDOVER_SESSION_ID", session_id.to_string())
        .env("HANDOVER_RUN_ID", run_id.to_string())
        .env(
            "HANDOVER_CHECKPOINT_INBOX",
            run_dir.join("inbox/checkpoints"),
        )
        .args(["arm", "codex", "--from-provider", "--json"])
        .output()
        .unwrap();
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let armed: serde_json::Value = serde_json::from_slice(&armed.stdout).unwrap();
    let armed_sequence = armed["armed_sequence"].as_u64().unwrap();

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

    // Clearing a lease is a recovery, and recoveries are recorded: neither
    // the journal nor the user is left to infer that it happened.
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Released the stale claude lease"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let recovered = String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|envelope| envelope["event"]["type"] == "run.recovered")
        .expect("releasing the arming run's lease must be journaled");
    assert_eq!(recovered["event"]["run_id"], run_id.to_string());
    assert_eq!(
        recovered["event"]["payload"]["reason"],
        format!("released by claim of the switch armed at sequence {armed_sequence}")
    );
}

/// The narrowing. An arm recorded from *outside* a run adopts nothing, so it
/// carries no authority over the lease it happened to find. Before this,
/// `arm` + `claim` from any terminal was an unprompted
/// `switch --recover-lease` — the consent gate reachable by two commands that
/// never advertised that power.
#[test]
fn an_arm_from_outside_a_run_cannot_release_the_lease_it_found() {
    let (_temp, cwd, state, path) = finished_session();
    let (session, session_id) = session_dir_and_id(&state);
    let leases = LeaseStore::new(&session);

    let dead = RunLease::new(session_id, RunId::new(), Provider::Claude, dead_identity()).unwrap();
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
        !output.status.success(),
        "an arm recorded outside a run must not carry the release privilege"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("was not created by the run that armed the switch"),
        "stderr was: {stderr}"
    );

    // Untouched: recovering it stays the consent-gated path.
    assert_eq!(leases.read().unwrap().unwrap().run_id, dead.run_id);
    leases.clear(&dead.run_id).unwrap();
}

/// A live lease always says "quit it", whoever armed the switch. Liveness is
/// checked before ownership because that advice is true and actionable either
/// way, and a live lease could not be released by anyone — so ownership only
/// becomes the interesting question once the lease is dead.
#[test]
fn claim_refuses_while_a_provider_is_still_live() {
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
    // Liveness must be the refusal that fires, not merely a diagnostic tacked
    // onto the ownership one. `classify_lease` appends "is still running" to
    // that refusal too, so asserting only the phrase above cannot tell the two
    // orderings apart -- but a live lease must never be told the ownership
    // story, which is true-but-useless advice about a lease nobody may release.
    assert!(
        !stderr.contains("was not created by the run that armed the switch"),
        "a live lease must be told to quit the provider, not who armed the switch; \
         stderr was: {stderr}"
    );

    // Refused, not consumed: the live lease is untouched.
    assert_eq!(leases.read().unwrap().unwrap().run_id, live.run_id);

    leases.clear(&live.run_id).unwrap();
}

/// The ownership refusal with a *non-`None`* `armed_run`: `Some(A) != Some(B)`,
/// the case its message literally describes — the arming run died, and a
/// different run took the session's lease before the claim landed. Arming from
/// inside the run is what makes `armed_run` `Some(A)` at all; an arm typed in a
/// plain terminal records `None` and is covered separately by
/// `an_arm_from_outside_a_run_cannot_release_the_lease_it_found`.
#[test]
fn claim_refuses_when_a_different_run_holds_the_lease() {
    let (_temp, cwd, state, path) = finished_session();
    let (session, session_id) = session_dir_and_id(&state);
    let (run_dir, run_id) = run_dir_and_id(&session);
    let leases = LeaseStore::new(&session);

    // Run A holds the lease while it arms, so the arm adopts A.
    let arming_run = RunLease::new(
        session_id.clone(),
        run_id.clone(),
        Provider::Claude,
        dead_identity(),
    )
    .unwrap();
    leases.create(&arming_run).unwrap();

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .env("HANDOVER_SESSION_ID", session_id.to_string())
        .env("HANDOVER_RUN_ID", run_id.to_string())
        .env(
            "HANDOVER_CHECKPOINT_INBOX",
            run_dir.join("inbox/checkpoints"),
        )
        .args(["arm", "codex", "--from-provider"])
        .assert()
        .success();

    // Before the claim lands, a different run takes the session's lease --
    // e.g. a fresh `handover run` started after the arm. It is dead too, so
    // the liveness rung above cannot be what refuses; only ownership can.
    leases.clear(&arming_run.run_id).unwrap();
    let foreign_run =
        RunLease::new(session_id, RunId::new(), Provider::Claude, dead_identity()).unwrap();
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
    use handover::store::lease::{LeaseStore, RunLease};

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
        dead_identity(),
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

/// The flag is not the authorization — the run proof is. A provider process
/// stays refused without the flag, and refused *with* it when the environment
/// does not describe a real active run. `attach` has no such flag at all in
/// this slice: it is worktree-scoped and only reachable over MCP.
#[test]
fn attached_provider_processes_cannot_arm_claim_or_attach_without_a_real_run() {
    let (_temp, cwd, state, path) = finished_session();

    for (args, expected) in [
        (
            vec!["arm", "codex"],
            "an attached provider may only invoke Handover hooks",
        ),
        (
            vec!["claim"],
            "an attached provider may only invoke Handover hooks",
        ),
        (
            vec!["attach", "codex"],
            "an attached provider may only invoke Handover hooks",
        ),
        // With the flag, the outer guard lets these through, so the refusal has
        // to come from the run proof itself. Without this assertion, deleting
        // that gate leaves the test green.
        (
            vec!["arm", "codex", "--from-provider"],
            "HANDOVER_SESSION_ID is required",
        ),
        (
            vec!["claim", "--from-provider"],
            "HANDOVER_SESSION_ID is required",
        ),
    ] {
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
            "{args:?} must be refused inside a provider run with no active-run proof"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{args:?} stderr was: {stderr}");
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

#[test]
fn arm_records_intent_and_switch_events_are_provider_neutral() {
    let (_temp, cwd, state, path) = finished_session();

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex"])
        .assert()
        .success();

    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let journal: Vec<serde_json::Value> = String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    let requested = journal
        .iter()
        .map(|envelope| &envelope["event"])
        .find(|event| event["type"] == "switch.requested")
        .expect("arm must record the intent");
    assert_eq!(requested["payload"]["to"], "codex");

    let armed = journal
        .iter()
        .map(|envelope| &envelope["event"])
        .find(|event| event["type"] == "switch.armed")
        .expect("arm must record the capability");

    // The intent precedes the capability.
    assert!(requested["sequence"].as_u64().unwrap() < armed["sequence"].as_u64().unwrap());

    // Switch events are session-level facts: `from`/`to` live in the payload,
    // so the envelope attributes them to no provider.
    for event in [requested, armed] {
        assert_eq!(
            event["provider"],
            serde_json::Value::Null,
            "switch events must be provider-neutral, got {event}"
        );
    }
}

#[test]
fn switch_refuses_when_a_different_provider_is_already_armed() {
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
        .args(["switch", "claude"])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "switch must refuse a conflicting arm"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("codex"),
        "must name the armed provider: {stderr}"
    );
    assert!(
        stderr.contains("claim"),
        "must say what to do about it: {stderr}"
    );
}

#[test]
fn switch_journals_the_same_arm_and_claim_a_two_step_switch_does() {
    let (_temp, cwd, state, path) = finished_session();

    cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["switch", "codex"])
        .assert()
        .success();

    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&log.stdout);

    // One command, but the journal records the same three facts a manual
    // `arm` + `claim` would leave behind.
    for kind in ["switch.requested", "switch.armed", "switch.claimed"] {
        assert!(text.contains(kind), "switch must journal {kind}");
    }
}

/// A `switch` that cannot render its handover must arm nothing.
///
/// `arm` and `claim` both prove the document renders before they touch the
/// journal; the porcelain that composes them has to do the same, or a failed
/// command leaves a capability the user never asked for — and, for the next
/// fifteen minutes, refuses every switch to any other provider.
///
/// The gate is broken by corrupting the narrative checkpoint's rendered
/// Markdown, which `load_verified_checkpoint` checks against its canonical
/// JSON. The saved cwd still resolves, so this lands where the arm used to be
/// already durable rather than before it.
#[test]
fn a_switch_that_cannot_render_its_handover_arms_nothing() {
    let (_temp, cwd, state, path) = finished_session();
    let (session, _) = session_dir_and_id(&state);

    let handover = |args: &[&str]| {
        cargo_bin_cmd!("handover")
            .current_dir(&cwd)
            .env("HANDOVER_HOME", &state)
            .env("PATH", &path)
            .args(args)
            .output()
            .unwrap()
    };
    let journal = || String::from_utf8_lossy(&handover(&["log", "--json"]).stdout).into_owned();

    let markdown = std::fs::read_dir(session.join("checkpoints"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "md"))
        .expect("the finished run wrote a narrative checkpoint");
    let canonical = std::fs::read(&markdown).unwrap();
    std::fs::write(&markdown, b"not the canonical rendering\n").unwrap();

    let refused = handover(&["switch", "codex"]);
    assert!(
        !refused.status.success(),
        "switch must fail when its handover will not render"
    );

    let text = journal();
    for kind in ["switch.requested", "switch.armed", "switch.claimed"] {
        assert!(
            !text.contains(kind),
            "a switch that failed its render gate must journal no {kind}; \
             journal was: {text}"
        );
    }

    // "Nothing happened" is retryable: with the cause fixed, the same command
    // works, and it is not fighting a capability the failure left behind.
    std::fs::write(&markdown, &canonical).unwrap();
    let retried = handover(&["switch", "codex"]);
    assert!(
        retried.status.success(),
        "the switch must succeed once the cause is fixed; stderr was: {}",
        String::from_utf8_lossy(&retried.stderr)
    );
    assert!(journal().contains("switch.claimed"));
}

/// `switch` finding a pending arm that already targets the same provider
/// must claim that one rather than arming a second time. The exit status
/// alone cannot tell the two apart -- both a reuse and a regressed
/// double-arm would leave `switch` exiting 0 -- so this pins the journal:
/// the `switch.claimed` event must point at the sequence `handover arm`
/// produced, and there must be exactly one `switch.armed` and one
/// `switch.requested` event for the whole arm-then-switch sequence.
#[test]
fn switch_reuses_a_pending_arm_for_the_same_provider() {
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

    let switched = run(&["switch", "codex"]);
    assert!(
        switched.status.success(),
        "{}",
        String::from_utf8_lossy(&switched.stderr)
    );

    let log = run(&["log", "--json"]);
    let journal: Vec<serde_json::Value> = String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    let claimed = journal
        .iter()
        .map(|envelope| &envelope["event"])
        .find(|event| event["type"] == "switch.claimed")
        .expect("switch must claim the pending arm");
    assert_eq!(
        claimed["payload"]["armed_sequence"], armed_sequence,
        "switch must claim the arm `handover arm` made, not a second one"
    );

    let armed_events = journal
        .iter()
        .map(|envelope| &envelope["event"])
        .filter(|event| event["type"] == "switch.armed")
        .count();
    assert_eq!(
        armed_events, 1,
        "switch must reuse the existing arm rather than recording a second one"
    );

    let requested_events = journal
        .iter()
        .map(|envelope| &envelope["event"])
        .filter(|event| event["type"] == "switch.requested")
        .count();
    assert_eq!(
        requested_events, 1,
        "switch must reuse the existing intent rather than recording a second one"
    );
}

#[test]
fn arm_replace_supersedes_a_pending_arm_and_journals_the_one_it_retired() {
    let (_temp, cwd, state, path) = finished_session();

    let first = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex", "--json"])
        .output()
        .unwrap();
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let superseded = first["armed_sequence"].as_u64().unwrap();

    // Without the flag this is the established refusal.
    let refused = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "claude"])
        .output()
        .unwrap();
    assert!(!refused.status.success(), "bare arm must still refuse");

    let replaced = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "claude", "--replace", "--json"])
        .output()
        .unwrap();
    assert!(
        replaced.status.success(),
        "{}",
        String::from_utf8_lossy(&replaced.stderr)
    );
    let replaced: serde_json::Value = serde_json::from_slice(&replaced.stdout).unwrap();
    assert_eq!(replaced["to"], "claude");
    assert!(replaced["armed_sequence"].as_u64().unwrap() > superseded);

    // The retired arm is accounted for, not silently dropped.
    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let expired = String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|envelope| envelope["event"]["type"] == "switch.expired")
        .expect("superseding an arm must retire it in the journal");
    assert_eq!(expired["event"]["payload"]["armed_sequence"], superseded);

    // And exactly one arm is pending afterwards: claiming gets claude.
    let claimed = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["claim", "--json"])
        .output()
        .unwrap();
    let claimed: serde_json::Value = serde_json::from_slice(&claimed.stdout).unwrap();
    assert_eq!(claimed["to_provider"], "claude");
}

/// `--replace` retires the pending arm, so it must not do so before the gate
/// that can still refuse the command.
///
/// Reproduced by the reviewer with a corrupted checkpoint blob: the journal
/// ended at `switch.expired` with no replacement, and the user had just been
/// told on stderr that the supersede succeeded. That is the "half happened"
/// state `claim_core` and `next_launch_from_pending_arm` both go out of their
/// way to prevent, and it violates what `commit_transition_handover`'s own
/// contract states -- prove the document renders before mutating.
///
/// The gate is broken the same way `a_switch_that_cannot_render_its_handover_\
/// arms_nothing` breaks it: the narrative checkpoint's rendered Markdown is
/// corrupted, which `load_verified_checkpoint` catches against its canonical
/// JSON. The saved cwd still resolves, so the failure lands at the render
/// rather than earlier.
#[test]
fn arm_replace_that_fails_its_render_gate_leaves_the_pending_arm_intact() {
    let (_temp, cwd, state, path) = finished_session();
    let (session, _) = session_dir_and_id(&state);

    let handover = |args: &[&str]| {
        cargo_bin_cmd!("handover")
            .current_dir(&cwd)
            .env("HANDOVER_HOME", &state)
            .env("PATH", &path)
            .args(args)
            .output()
            .unwrap()
    };
    let journal = || String::from_utf8_lossy(&handover(&["log", "--json"]).stdout).into_owned();

    let first = handover(&["arm", "codex", "--json"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let pending = first["armed_sequence"].as_u64().unwrap();

    let markdown = std::fs::read_dir(session.join("checkpoints"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "md"))
        .expect("the finished run wrote a narrative checkpoint");
    let canonical = std::fs::read(&markdown).unwrap();
    std::fs::write(&markdown, b"not the canonical rendering\n").unwrap();

    let refused = handover(&["arm", "claude", "--replace"]);
    let stderr = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(
        !refused.status.success(),
        "--replace must fail when its handover will not render; stderr was: {stderr}"
    );
    // The command failed, so it must not have announced a supersede that has
    // to be undone by reading the journal to find out.
    assert!(
        !stderr.contains("Superseded"),
        "a command that failed must not report a supersede; stderr was: {stderr}"
    );

    // The half that matters: the arm the user had is still theirs.
    let text = journal();
    assert!(
        !text.contains("switch.expired"),
        "a --replace that failed its gate must retire nothing; journal was: {text}"
    );

    // With the cause fixed, `status` can render again -- and it still reports
    // the arm the failed command was supposed to have superseded. Asserted
    // after the repair rather than before it because `status` renders the same
    // handover the gate does, so a corrupted checkpoint refuses it too; the
    // repair changes no event, and `switch.expired` is checked above.
    std::fs::write(&markdown, &canonical).unwrap();
    let status: serde_json::Value =
        serde_json::from_slice(&handover(&["status", "--json"]).stdout).unwrap();
    assert_eq!(
        status["switch_readiness"]["armed"]["sequence"], pending,
        "the original arm must still be pending; status was: {status}"
    );
    assert_eq!(status["switch_readiness"]["armed"]["to"], "codex");

    // "Nothing happened" is retryable: with the cause fixed the same command
    // works, and only then is the original arm retired.
    let retried = handover(&["arm", "claude", "--replace", "--json"]);
    assert!(
        retried.status.success(),
        "the supersede must work once the cause is fixed; stderr was: {}",
        String::from_utf8_lossy(&retried.stderr)
    );
    let retried: serde_json::Value = serde_json::from_slice(&retried.stdout).unwrap();
    assert_eq!(retried["to"], "claude");
    let expired = journal()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|envelope| envelope["event"]["type"] == "switch.expired")
        .expect("the successful supersede retires the arm it replaced");
    assert_eq!(expired["event"]["payload"]["armed_sequence"], pending);
}

/// `--replace` with nothing pending must be a plain no-op: it arms, and it
/// must not journal `switch.expired` for an arm that never existed. That
/// event is a claim about the past that would be a lie -- and, being
/// append-only and checksummed, a permanent one. A regression such as
/// branching on `replace` instead of on whether `crate::arm::pending` found
/// something would pass every other test here while planting exactly that
/// lie in the journal.
#[test]
fn arm_replace_with_nothing_pending_is_a_plain_no_op() {
    let (_temp, cwd, state, path) = finished_session();

    let armed = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["arm", "codex", "--replace", "--json"])
        .output()
        .unwrap();
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let armed: serde_json::Value = serde_json::from_slice(&armed.stdout).unwrap();
    assert_eq!(armed["to"], "codex");
    assert!(armed["armed_sequence"].as_u64().unwrap() > 0);

    // Nothing was pending, so nothing was retired.
    let log = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let has_expired = String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .any(|envelope| envelope["event"]["type"] == "switch.expired");
    assert!(
        !has_expired,
        "--replace with nothing pending must not journal switch.expired; journal was: {}",
        String::from_utf8_lossy(&log.stdout)
    );
}
