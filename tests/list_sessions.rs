mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

#[test]
fn an_empty_state_lists_no_sessions_from_any_directory() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    let cwd = temp.path().join("anywhere");
    std::fs::create_dir_all(&cwd).unwrap();

    let output = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["sessions"].as_array().unwrap().len(), 0);

    let pretty = cargo_bin_cmd!("handover")
        .current_dir(&cwd)
        .env("HANDOVER_HOME", &state)
        .args(["list"])
        .output()
        .unwrap();
    assert!(pretty.status.success());
    let pretty_value: serde_json::Value = serde_json::from_slice(&pretty.stdout).unwrap();
    assert_eq!(pretty_value, value);
}

#[test]
fn an_attached_provider_cannot_list_sessions() {
    let temp = TempDir::new().unwrap();
    let output = cargo_bin_cmd!("handover")
        .current_dir(temp.path())
        .env("HANDOVER_HOME", temp.path().join("state"))
        .env("HANDOVER_RUN_ID", "22222222-2222-4222-8222-222222222222")
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("attached provider"));
}

fn fake_claude(bin: &std::path::Path, checkpoint_json: Option<&str>) {
    let checkpoint_line = checkpoint_json.map_or(String::new(), |json| {
        format!(
            "printf '%s' '{json}' | \"$HANDOVER_HOOK_BIN\" checkpoint --format json --from-provider\n"
        )
    });
    write_executable(
        &bin.join("claude"),
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${{1:-}} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() {{ printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }}
hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}}'
{checkpoint_line}hook '{{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}}'
exit 0
"#
        ),
    );
}

const ALPHA_CHECKPOINT: &str = r#"{"objective":"List sessions","summary":"Alpha work is captured","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Continue"],"related_event_sequences":[]}"#;

fn run_fake_session(repo: &std::path::Path, state: &std::path::Path, bin: &std::path::Path) {
    cargo_bin_cmd!("handover")
        .current_dir(repo)
        .env("HANDOVER_HOME", state)
        .env("PATH", path_with(bin))
        .args(["run", "claude"])
        .assert()
        .success();
}

#[test]
fn every_session_is_listed_with_facts_newest_first() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();

    let alpha = temp.path().join("alpha");
    init_repo(&alpha);
    fake_claude(&bin, Some(ALPHA_CHECKPOINT));
    run_fake_session(&alpha, &state, &bin);

    let beta = temp.path().join("beta");
    init_repo(&beta);
    fake_claude(&bin, None);
    run_fake_session(&beta, &state, &bin);

    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let output = cargo_bin_cmd!("handover")
        .current_dir(&outside)
        .env("HANDOVER_HOME", &state)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = value["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 2);

    let newest = &rows[0];
    let oldest = &rows[1];
    assert!(newest["worktree"].as_str().unwrap().ends_with("/beta"));
    assert!(oldest["worktree"].as_str().unwrap().ends_with("/alpha"));
    assert!(newest["last_activity"].as_str().unwrap() > oldest["last_activity"].as_str().unwrap());

    for row in rows {
        assert_eq!(row["degraded"], false);
        assert_eq!(row["bound"], true);
        assert_eq!(row["branch"], "main");
        assert_eq!(row["last_provider"], "claude");
        assert!(row["session_id"].as_str().unwrap().len() == 36);
        assert!(row["repository"].as_str().unwrap().ends_with("/.git"));
        assert!(row["diagnostics"].as_array().unwrap().is_empty());
    }

    assert!(oldest["latest_narrative_checkpoint"].as_u64().is_some());
    assert!(oldest["events_since_narrative"].as_u64().unwrap() >= 1);
    assert!(newest["latest_narrative_checkpoint"].is_null());
    assert!(newest["events_since_narrative"].as_u64().unwrap() >= 1);
}

fn session_dirs(state: &std::path::Path) -> std::collections::BTreeSet<String> {
    let sessions = state.join("sessions");
    std::fs::read_dir(sessions)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn newest_session_dir(
    state: &std::path::Path,
    before: &std::collections::BTreeSet<String>,
) -> std::path::PathBuf {
    let after = session_dirs(state);
    let new: Vec<_> = after.difference(before).collect();
    assert_eq!(new.len(), 1);
    state.join("sessions").join(new[0])
}

#[test]
fn corrupt_sessions_degrade_to_diagnostic_rows_without_failing_the_listing() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_claude(&bin, None);

    let alpha = temp.path().join("alpha");
    init_repo(&alpha);
    run_fake_session(&alpha, &state, &bin);
    let healthy_dirs = session_dirs(&state);

    let beta = temp.path().join("beta");
    init_repo(&beta);
    run_fake_session(&beta, &state, &bin);
    let beta_dir = newest_session_dir(&state, &healthy_dirs);
    std::fs::write(beta_dir.join("meta.json"), b"not json").unwrap();

    let with_beta = session_dirs(&state);
    let gamma = temp.path().join("gamma");
    init_repo(&gamma);
    run_fake_session(&gamma, &state, &bin);
    let gamma_dir = newest_session_dir(&state, &with_beta);
    let journal = gamma_dir.join("events.jsonl");
    let mut bytes = std::fs::read(&journal).unwrap();
    bytes.extend_from_slice(b"not a valid envelope\n");
    std::fs::write(&journal, bytes).unwrap();

    std::fs::create_dir(state.join("sessions/not-a-session")).unwrap();

    let output = cargo_bin_cmd!("handover")
        .current_dir(temp.path())
        .env("HANDOVER_HOME", &state)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = value["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 4);

    let healthy: Vec<_> = rows.iter().filter(|row| row["degraded"] == false).collect();
    assert_eq!(healthy.len(), 1);
    assert!(healthy[0]["worktree"].as_str().unwrap().ends_with("/alpha"));
    assert_eq!(healthy[0]["bound"], true);
    assert_eq!(healthy[0]["last_provider"], "claude");

    let degraded: Vec<_> = rows.iter().filter(|row| row["degraded"] == true).collect();
    assert_eq!(degraded.len(), 3);
    for row in &degraded {
        let diagnostics = row["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .as_str()
                .unwrap()
                .contains("run handover doctor")
        );
        assert!(row["last_activity"].is_null());
        assert_eq!(row["bound"], false);
    }

    let degraded_ids: Vec<&str> = degraded
        .iter()
        .map(|row| row["session_id"].as_str().unwrap())
        .collect();
    let beta_id = beta_dir.file_name().unwrap().to_string_lossy().into_owned();
    let gamma_id = gamma_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(degraded_ids.contains(&beta_id.as_str()));
    assert!(degraded_ids.contains(&gamma_id.as_str()));
    assert!(degraded_ids.contains(&"not-a-session"));
}
