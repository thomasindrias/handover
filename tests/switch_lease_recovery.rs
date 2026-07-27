mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use handover::model::{EventEnvelope, EventKind, Provider, RunId, SessionId};
use handover::store::lease::{LeaseStore, ProcessIdentity, RunLease};
use predicates::prelude::*;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

const FAKE_CLAUDE: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
exit 0
"#;

const FAKE_CODEX: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"codex-native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$HANDOVER_HOOK_BIN" __hook codex >/dev/null
exit 0
"#;

fn run_claude(repo: &std::path::Path, state: &std::path::Path, path: &std::ffi::OsStr) {
    cargo_bin_cmd!("handover")
        .current_dir(repo)
        .env("HANDOVER_HOME", state)
        .env("PATH", path)
        .args(["run", "claude"])
        .assert()
        .success();
}

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

fn journal(session: &std::path::Path) -> Vec<EventEnvelope> {
    std::fs::read_to_string(session.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn switch_refuses_a_live_or_foreign_host_lease_with_actionable_detail() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(&bin.join("claude"), FAKE_CLAUDE);
    let state = temp.path().join("state");
    let path = path_with(&bin);
    run_claude(&repo, &state, &path);

    let (session, session_id) = session_dir_and_id(&state);
    let leases = LeaseStore::new(&session);

    let live = RunLease::new(
        session_id.clone(),
        RunId::new(),
        Provider::Claude,
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .unwrap();
    leases.create(&live).unwrap();
    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["switch", "codex"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("claude")
                .and(predicate::str::contains(format!(
                    "pid {}",
                    std::process::id()
                )))
                .and(predicate::str::contains("retry the switch")),
        );
    leases.clear(&live.run_id).unwrap();

    let mut foreign = RunLease::new(
        session_id,
        RunId::new(),
        Provider::Claude,
        ProcessIdentity {
            pid: u32::MAX,
            start_token: "gone".into(),
        },
    )
    .unwrap();
    foreign.host = "different-host".into();
    leases.create(&foreign).unwrap();
    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["switch", "codex"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("different-host")
                .and(predicate::str::contains("liveness cannot be checked")),
        );
    leases.clear(&foreign.run_id).unwrap();
}

#[test]
fn switch_recovers_a_stale_lease_only_with_explicit_consent() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(&bin.join("claude"), FAKE_CLAUDE);
    write_executable(&bin.join("codex"), FAKE_CODEX);
    let state = temp.path().join("state");
    let path = path_with(&bin);
    run_claude(&repo, &state, &path);

    let (session, session_id) = session_dir_and_id(&state);
    let leases = LeaseStore::new(&session);
    let stale = RunLease::new(
        session_id,
        RunId::new(),
        Provider::Claude,
        ProcessIdentity {
            pid: u32::MAX,
            start_token: "gone".into(),
        },
    )
    .unwrap();
    leases.create(&stale).unwrap();

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["switch", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "handover switch codex --recover-lease",
        ));
    assert_eq!(leases.read().unwrap().unwrap().run_id, stale.run_id);
    assert!(
        !journal(&session)
            .iter()
            .any(|envelope| matches!(envelope.event.kind, EventKind::RunRecovered { .. }))
    );

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["switch", "codex", "--recover-lease"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Recovered stale claude lease"));
    assert!(leases.read().unwrap().is_none());

    let recovered = journal(&session)
        .into_iter()
        .find(|envelope| matches!(envelope.event.kind, EventKind::RunRecovered { .. }))
        .unwrap();
    match recovered.event.kind {
        EventKind::RunRecovered { reason, .. } => {
            assert!(reason.contains("--recover-lease"));
        }
        _ => unreachable!(),
    }
}
