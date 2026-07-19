mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use sesh::model::{Provider, RunId, SessionId};
use sesh::store::lease::{LeaseStore, ProcessIdentity, RunLease};
use tempfile::TempDir;

use support::{init_repo, path_with, repository_fingerprint, write_executable};

#[test]
fn deletion_requires_confirmation_refuses_live_runs_and_preserves_the_repository() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$SESH_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
    );
    let state = temp.path().join("state");
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["run", "claude"])
        .assert()
        .success();
    let before = repository_fingerprint(&repo);
    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let session_id = SessionId::parse(session.file_name().unwrap().to_str().unwrap()).unwrap();
    let external = temp.path().join("must-survive");
    std::fs::write(&external, b"outside session\n").unwrap();
    std::os::unix::fs::symlink(&external, session.join("external-link")).unwrap();

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .arg("delete")
        .assert()
        .failure();
    assert!(session.exists());

    let leases = LeaseStore::new(&session);
    let live = RunLease::new(
        session_id.clone(),
        RunId::new(),
        Provider::Claude,
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .unwrap();
    leases.create(&live).unwrap();
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .failure();
    assert!(session.exists());
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
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .failure();
    assert!(session.exists());
    leases.clear(&foreign.run_id).unwrap();

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["delete", "--yes"])
        .assert()
        .success();

    assert!(!session.exists());
    assert_eq!(
        std::fs::read_dir(state.join("sessions")).unwrap().count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(state.join("refs/worktrees"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(repository_fingerprint(&repo), before);
    assert_eq!(std::fs::read(external).unwrap(), b"outside session\n");
}
