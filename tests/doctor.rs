mod support;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use sesh::model::Provider;
use sesh::provider::adapter;
use sesh::store::StateLayout;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

#[test]
fn setup_is_inspectable_noninteractive_and_refuses_asset_drift() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    install_capable_providers(&bin);
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["setup", "claude"])
        .assert()
        .code(2)
        .stdout(predicates::str::contains("claude --plugin-dir"));
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["setup", "codex"])
        .assert()
        .code(2)
        .stdout(predicates::str::contains("hooks.SessionStart"))
        .stdout(predicates::str::contains("dangerously-bypass-hook-trust").not());

    cargo_bin_cmd!("sesh")
        .env_remove("SESH_HOME")
        .env_remove("SESH_SESSION_ID")
        .env_remove("SESH_RUN_ID")
        .args(["__hook", "claude"])
        .write_stdin(r#"{"hook_event_name":"SessionStart"}"#)
        .assert()
        .success()
        .stdout("");

    let plugin = state.join("integrations/claude/1/.claude-plugin/plugin.json");
    std::fs::write(&plugin, b"drift").unwrap();
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["setup", "claude"])
        .assert()
        .failure();
}

#[test]
fn doctor_reports_layered_diagnostics_as_stable_json_without_mutation() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");
    let layout = StateLayout::new(state.clone());
    layout.ensure().unwrap();
    adapter(Provider::Claude)
        .setup(&layout.integrations())
        .unwrap();
    adapter(Provider::Codex)
        .setup(&layout.integrations())
        .unwrap();
    std::fs::write(
        state.join("integrations/claude/1/hooks/hooks.json"),
        b"drift",
    )
    .unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o750)).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"${1:-}\" = --help ]; then echo no-supported-flags; exit 0; fi\nexit 0\n",
    );
    write_executable(
        &bin.join("codex"),
        "#!/bin/sh\nif [ \"${1:-}\" = --help ]; then echo '--config --add-dir --cd'; exit 0; fi\nif [ \"${1:-}\" = features ]; then echo 'hooks experimental false'; exit 0; fi\nexit 0\n",
    );
    let before = std::fs::read(state.join("FORMAT")).unwrap();
    let before_modified = std::fs::metadata(state.join("FORMAT"))
        .unwrap()
        .modified()
        .unwrap();

    let output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &bin)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let diagnostics: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    for diagnostic in &diagnostics {
        assert!(diagnostic["code"].as_str().is_some());
        assert!(diagnostic["severity"].as_str().is_some());
        assert!(diagnostic["message"].as_str().is_some());
    }
    for code in [
        "git.missing",
        "provider.capability_missing",
        "codex.hooks_unstable",
        "integration.invalid",
        "permissions.insecure",
    ] {
        assert!(
            diagnostics.iter().any(|item| item["code"] == code),
            "missing {code}: {diagnostics:?}"
        );
    }
    assert_eq!(std::fs::read(state.join("FORMAT")).unwrap(), before);
    assert_eq!(
        std::fs::metadata(state.join("FORMAT"))
            .unwrap()
            .modified()
            .unwrap(),
        before_modified
    );

    let empty_path = temp.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let missing = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &empty_path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let missing: Vec<serde_json::Value> = serde_json::from_slice(&missing.stdout).unwrap();
    assert!(
        missing
            .iter()
            .any(|item| item["code"] == "provider.missing")
    );
}

#[test]
fn doctor_repairs_only_partial_tail_refs_and_capture_sentinel() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    install_capable_providers(&bin);
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--help" ]]; then printf '%s\n' '--plugin-dir --add-dir'; exit 0; fi
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake 1'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$SESH_HOOK_BIN" __hook claude >/dev/null
printf '%s' '{"objective":"Repair","summary":"Checkpoint","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Continue"],"related_event_sequences":[]}' | "$SESH_HOOK_BIN" checkpoint --format json --from-provider
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}' | "$SESH_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
    );
    let state = temp.path().join("state");
    let path = path_with(&bin);
    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();
    adapter(Provider::Codex)
        .setup(&state.join("integrations"))
        .unwrap();
    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let journal = session.join("events.jsonl");
    let committed = std::fs::read(&journal).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(b"{invalid partial")
        .unwrap();
    std::fs::remove_file(session.join("refs/latest-checkpoint")).unwrap();
    let run = std::fs::read_dir(session.join("runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(run.join("capture-failed.json"), b"{}\n").unwrap();
    std::fs::set_permissions(
        run.join("capture-failed.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let before_plain = std::fs::read(&journal).unwrap();
    let before_modified = std::fs::metadata(&journal).unwrap().modified().unwrap();

    let plain = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let diagnostics: Vec<serde_json::Value> = serde_json::from_slice(&plain.stdout).unwrap();
    assert!(diagnostics.iter().any(|item| {
        item["code"] == "journal.partial_tail"
            && item["repair_command"] == "sesh doctor --repair"
            && item["message"].as_str().unwrap().contains("16")
    }));
    assert_eq!(std::fs::read(&journal).unwrap(), before_plain);
    assert_eq!(
        std::fs::metadata(&journal).unwrap().modified().unwrap(),
        before_modified
    );

    let repaired = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["doctor", "--json", "--repair"])
        .output()
        .unwrap();
    let repaired_diagnostics: Vec<serde_json::Value> =
        serde_json::from_slice(&repaired.stdout).unwrap();
    assert!(
        repaired_diagnostics
            .iter()
            .any(|item| item["code"] == "journal.tail_repaired")
    );
    assert!(
        repaired_diagnostics
            .iter()
            .any(|item| item["code"] == "checkpoint.ref_rebuilt")
    );
    assert!(
        repaired_diagnostics
            .iter()
            .any(|item| item["code"] == "capture.sentinel_removed")
    );
    assert_eq!(std::fs::read(&journal).unwrap(), committed);
    assert!(session.join("refs/latest-checkpoint").exists());
    assert!(!run.join("capture-failed.json").exists());

    let mut corrupt = committed;
    let checksum = corrupt
        .windows(b"sha256:".len())
        .position(|window| window == b"sha256:")
        .unwrap()
        + b"sha256:".len();
    corrupt[checksum] = if corrupt[checksum] == b'a' {
        b'b'
    } else {
        b'a'
    };
    std::fs::write(&journal, corrupt).unwrap();
    let output = cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("PATH", &path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let diagnostics: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|item| item["code"] == "journal.corrupt" && item["severity"] == "error")
    );
}

fn install_capable_providers(bin: &std::path::Path) {
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"${1:-}\" = --help ]; then echo '--plugin-dir --add-dir'; exit 0; fi\nif [ \"${1:-}\" = --version ]; then echo 'fake 1'; exit 0; fi\nexit 0\n",
    );
    write_executable(
        &bin.join("codex"),
        "#!/bin/sh\nif [ \"${1:-}\" = --help ]; then echo '--config --add-dir --cd'; exit 0; fi\nif [ \"${1:-}\" = features ]; then echo 'hooks stable true'; exit 0; fi\nif [ \"${1:-}\" = --version ]; then echo 'fake 1'; exit 0; fi\nexit 0\n",
    );
}
