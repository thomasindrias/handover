mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

fn fake_codex(bin: &std::path::Path) {
    write_executable(
        &bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
printf '%s\n' "$@" > "$SESH_TEST_TRACE/codex.args"
printf '%s\n' "${CODEX_HOME:-unset}" > "$SESH_TEST_TRACE/codex.home"
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$SESH_HOOK_BIN" __hook codex >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
}

#[test]
fn codex_launch_uses_a_private_codex_home_with_no_config_overlay_flags() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_codex(&bin);
    let state = temp.path().join("state");
    let trace = temp.path().join("trace");
    std::fs::create_dir(&trace).unwrap();
    let path = path_with(&bin);

    cargo_bin_cmd!("sesh")
        .current_dir(&repo)
        .env("SESH_HOME", &state)
        .env("SESH_TEST_TRACE", &trace)
        .env("PATH", &path)
        .args(["run", "codex"])
        .assert()
        .success();

    let args = std::fs::read_to_string(trace.join("codex.args")).unwrap();
    assert!(
        !args.lines().any(|arg| arg == "-c") && !args.contains("hooks."),
        "expected no config overlay flags, got: {args}"
    );
    assert!(
        args.contains("--dangerously-bypass-hook-trust"),
        "expected the trust bypass flag, got: {args}"
    );

    let codex_home = std::fs::read_to_string(trace.join("codex.home"))
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(codex_home, "unset", "CODEX_HOME was not set for the child");
    let codex_home = std::path::PathBuf::from(codex_home);
    let hooks_json_link = codex_home.join("hooks.json");
    assert!(
        std::fs::symlink_metadata(&hooks_json_link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "expected codex_home/hooks.json to be a symlink"
    );
    let expected = std::fs::read(state.join("integrations/codex/1/hooks.json")).unwrap();
    assert_eq!(std::fs::read(&hooks_json_link).unwrap(), expected);
}
