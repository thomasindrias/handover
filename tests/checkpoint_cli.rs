mod support;

use std::os::unix::fs::PermissionsExt;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, write_executable};

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const RUN_ID: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn provider_submission_is_private_and_does_not_touch_the_journal() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    let session = state.join("sessions").join(SESSION_ID);
    let inbox = session.join("runs").join(RUN_ID).join("inbox/checkpoints");
    std::fs::create_dir_all(&inbox).unwrap();
    make_private_dirs(&state, &inbox);
    let events = session.join("events.jsonl");
    std::fs::write(&events, b"unchanged\n").unwrap();
    std::fs::set_permissions(&events, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = std::fs::read(&events).unwrap();

    cargo_bin_cmd!("handover")
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_SESSION_ID", SESSION_ID)
        .env("HANDOVER_RUN_ID", RUN_ID)
        .env("HANDOVER_CHECKPOINT_INBOX", &inbox)
        .args(["checkpoint", "--format", "json", "--from-provider"])
        .write_stdin(narrative_json())
        .assert()
        .success();

    assert_eq!(std::fs::read(&events).unwrap(), before);
    let submissions: Vec<_> = std::fs::read_dir(&inbox)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0].extension().unwrap(), "json");
    assert_eq!(
        std::fs::metadata(&submissions[0])
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&submissions[0]).unwrap()).unwrap();
    assert_eq!(value["objective"], "Implement OAuth");
}

#[test]
fn provider_submission_cannot_redirect_to_an_arbitrary_inbox() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    let expected = state
        .join("sessions")
        .join(SESSION_ID)
        .join("runs")
        .join(RUN_ID)
        .join("inbox/checkpoints");
    std::fs::create_dir_all(&expected).unwrap();
    make_private_dirs(&state, &expected);
    let redirected = temp.path().join("redirected");
    std::fs::create_dir(&redirected).unwrap();
    std::fs::set_permissions(&redirected, std::fs::Permissions::from_mode(0o700)).unwrap();

    cargo_bin_cmd!("handover")
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_SESSION_ID", SESSION_ID)
        .env("HANDOVER_RUN_ID", RUN_ID)
        .env("HANDOVER_CHECKPOINT_INBOX", &redirected)
        .args(["checkpoint", "--format", "json", "--from-provider"])
        .write_stdin(narrative_json())
        .assert()
        .failure();

    assert_eq!(std::fs::read_dir(redirected).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(expected).unwrap().count(), 0);
}

#[test]
fn human_submission_commits_an_event_artifacts_and_both_refs() {
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
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
    );
    let state = temp.path().join("state");
    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["run", "claude"])
        .assert()
        .success();

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env_remove("HANDOVER_RUN_ID")
        .args(["checkpoint", "--format", "json"])
        .write_stdin(narrative_json())
        .assert()
        .success();

    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let lines = std::fs::read_to_string(session.join("events.jsonl")).unwrap();
    let last: serde_json::Value = serde_json::from_str(lines.lines().last().unwrap()).unwrap();
    assert_eq!(last["event"]["type"], "checkpoint.created");
    assert_eq!(last["event"]["payload"]["checkpoint_kind"], "narrative");
    let sequence = last["event"]["sequence"].as_u64().unwrap();
    for extension in ["json", "md"] {
        let path = session
            .join("checkpoints")
            .join(format!("{sequence:012}.{extension}"));
        assert!(path.exists());
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    for reference in ["latest-checkpoint", "latest-narrative-checkpoint"] {
        let value: u64 =
            serde_json::from_slice(&std::fs::read(session.join("refs").join(reference)).unwrap())
                .unwrap();
        assert_eq!(value, sequence);
    }
    let checkpoint: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            session
                .join("checkpoints")
                .join(format!("{sequence:012}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(checkpoint["author"]["kind"], "human");

    let before = std::fs::read(session.join("events.jsonl")).unwrap();
    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_RUN_ID", RUN_ID)
        .args(["checkpoint", "--format", "json"])
        .write_stdin(narrative_json())
        .assert()
        .failure();
    assert_eq!(std::fs::read(session.join("events.jsonl")).unwrap(), before);
}

#[test]
fn provider_submission_is_promoted_by_the_next_hook() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("claude"),
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${{1:-}} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
printf '%s' '{}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
printf '%s' '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
            narrative_json()
        ),
    );
    let state = temp.path().join("state");

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["run", "claude"])
        .assert()
        .success();

    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let lines = std::fs::read_to_string(session.join("events.jsonl")).unwrap();
    let checkpoint_event: serde_json::Value = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .find(|value: &serde_json::Value| value["event"]["type"] == "checkpoint.created")
        .unwrap();
    let sequence = checkpoint_event["event"]["sequence"].as_u64().unwrap();
    let checkpoint: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            session
                .join("checkpoints")
                .join(format!("{sequence:012}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(checkpoint["author"]["kind"], "provider");
    assert_eq!(checkpoint["author"]["provider"], "claude");
    let run = std::fs::read_dir(session.join("runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read_dir(run.join("inbox/checkpoints"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn symlinked_provider_submission_is_refused_and_retained() {
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
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
printf '%s' 'not trusted' > "$HANDOVER_CHECKPOINT_INBOX/payload.txt"
chmod 600 "$HANDOVER_CHECKPOINT_INBOX/payload.txt"
ln -s payload.txt "$HANDOVER_CHECKPOINT_INBOX/forged.json"
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
    );
    let state = temp.path().join("state");

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", path_with(&bin))
        .args(["run", "claude"])
        .assert()
        .failure();

    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let run = std::fs::read_dir(session.join("runs"))
        .unwrap()
        .find_map(|entry| {
            let path = entry.unwrap().path();
            path.is_dir().then_some(path)
        })
        .unwrap();
    let forged = run.join("inbox/checkpoints/forged.json");
    assert!(
        std::fs::symlink_metadata(forged)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

fn narrative_json() -> &'static str {
    r#"{"objective":"Implement OAuth","summary":"PKCE is done","decisions":[],"assumptions":[],"constraints":[],"completed":["PKCE"],"in_progress":[],"blockers":[],"next_steps":["Fix callback test"],"related_event_sequences":[]}"#
}

fn make_private_dirs(root: &std::path::Path, leaf: &std::path::Path) {
    let mut current = Some(leaf);
    while let Some(path) = current {
        if path.starts_with(root) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        if path == root {
            break;
        }
        current = path.parent();
    }
}

fn path_with(bin: &std::path::Path) -> std::ffi::OsString {
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).unwrap()
}
