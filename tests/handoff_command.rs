mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

fn fake_claude_with_narrative(bin: &std::path::Path) {
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$SESH_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_callback"},"tool_use_id":"tool-1"}'
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_callback"},"tool_response":{"stdout":"1 passed","stderr":"","exit_code":0},"tool_use_id":"tool-1"}'
printf '%s\n' 'dirty' > changed.txt
printf '%s' '{"objective":"Implement OAuth callback","summary":"PKCE support is complete; one integration test still fails.","decisions":[],"assumptions":[],"constraints":[],"completed":["capture"],"in_progress":[],"blockers":[],"next_steps":["Fix callback integration test"],"related_event_sequences":[]}' | "$SESH_HOOK_BIN" checkpoint --format json --from-provider
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
}

fn fake_claude_without_narrative(bin: &std::path::Path) {
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$SESH_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
}

#[test]
fn handoff_previews_switch_markdown_without_mutating_the_session() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_claude_with_narrative(&bin);
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("sesh")
        .current_dir(&cwd)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    let inspect_output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["inspect", "--json"])
        .output()
        .unwrap();
    assert!(inspect_output.status.success());
    let inspect: serde_json::Value = serde_json::from_slice(&inspect_output.stdout).unwrap();
    let session_dir = std::path::PathBuf::from(inspect["session_dir"].as_str().unwrap());
    let journal_path = session_dir.join("events.jsonl");
    let journal_before = std::fs::read(&journal_path).unwrap();
    let mut checkpoints_before: Vec<_> = std::fs::read_dir(session_dir.join("checkpoints"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    checkpoints_before.sort();

    let handoff_output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["handoff", "codex"])
        .output()
        .unwrap();
    assert!(handoff_output.status.success());
    let markdown = String::from_utf8(handoff_output.stdout).unwrap();
    assert!(markdown.starts_with("# Sesh handoff"));
    assert!(markdown.contains("`claude` \u{2192} `codex`"));
    assert!(markdown.contains("Implement OAuth callback"));
    assert!(markdown.contains("Fix callback integration test"));
    assert!(markdown.contains("cargo test oauth_callback"));

    let journal_after = std::fs::read(&journal_path).unwrap();
    let mut checkpoints_after: Vec<_> = std::fs::read_dir(session_dir.join("checkpoints"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    checkpoints_after.sort();
    assert_eq!(journal_before, journal_after);
    assert_eq!(checkpoints_before, checkpoints_after);
}

#[test]
fn handoff_reports_missing_narrative_checkpoint_like_a_real_switch_would() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_claude_without_narrative(&bin);
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("sesh")
        .current_dir(&cwd)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    let handoff_output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["handoff", "codex"])
        .output()
        .unwrap();
    assert!(handoff_output.status.success());
    let markdown = String::from_utf8(handoff_output.stdout).unwrap();
    assert!(markdown.contains(
        "No narrative checkpoint exists. Objective, decisions, assumptions, and next steps were not checkpointed."
    ));
}

#[test]
fn handoff_json_exposes_narrative_freshness_and_the_rendered_markdown() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_claude_with_narrative(&bin);
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("sesh")
        .current_dir(&cwd)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    let handoff_output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["handoff", "codex", "--json"])
        .output()
        .unwrap();
    assert!(handoff_output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&handoff_output.stdout).unwrap();

    assert_eq!(value["schema_version"], 1);
    assert!(value["session_id"].as_str().is_some());
    assert_eq!(value["from_provider"], "claude");
    assert_eq!(value["to_provider"], "codex");
    let sequence = value["transition"]["sequence"].as_u64().unwrap();
    let through_sequence = value["transition"]["through_sequence"].as_u64().unwrap();
    assert_eq!(sequence, through_sequence + 1);
    assert!(value["narrative_checkpoint"]["sequence"].as_u64().is_some());
    assert!(
        value["narrative_checkpoint"]["events_since"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(value["narrative_checkpoint"]["author"]["kind"], "provider");
    assert_eq!(
        value["narrative_checkpoint"]["author"]["provider"],
        "claude"
    );
    assert!(value["capture_gaps"].as_array().unwrap().is_empty());
    assert_eq!(value["omitted"], false);
    let markdown = value["markdown"].as_str().unwrap();
    assert!(markdown.starts_with("# Sesh handoff"));
    assert_eq!(
        value["markdown_bytes"].as_u64().unwrap() as usize,
        markdown.len()
    );
}

#[test]
fn handoff_json_reports_null_narrative_checkpoint_when_none_exists() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let cwd = repo.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_claude_without_narrative(&bin);
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("sesh")
        .current_dir(&cwd)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    let handoff_output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .args(["handoff", "codex", "--json"])
        .output()
        .unwrap();
    assert!(handoff_output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&handoff_output.stdout).unwrap();
    assert!(value["narrative_checkpoint"].is_null());
}
