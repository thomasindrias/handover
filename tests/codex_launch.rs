mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

fn fake_codex(bin: &std::path::Path) {
    write_executable(
        &bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
printf '%s\n' "$@" > "$HANDOVER_TEST_TRACE/codex.args"
printf '%s\n' "${CODEX_HOME:-unset}" > "$HANDOVER_TEST_TRACE/codex.home"
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook codex >/dev/null; }
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

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_TEST_TRACE", &trace)
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

    // The launched session can actually reach the skill: it is inside the
    // CODEX_HOME the child was handed, and it carries the frontmatter Codex
    // scans for.
    let skill = codex_home.join("skills/handover-switch/SKILL.md");
    let text = std::fs::read_to_string(&skill)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", skill.display()));
    assert!(text.contains("name: handover-switch"));
    assert!(text.contains("--from-provider"));
}

/// Design decision 2 (see Task 4) is that Handover's own `handover-switch`
/// skill wins a name collision with a user skill of the same name, with one
/// stderr warning so the shadowing is visible rather than silent. This test
/// asserts both halves where a user would actually see them: the process's
/// own stderr, and the private `CODEX_HOME` the child process is actually
/// handed.
#[test]
fn a_colliding_user_skill_is_shadowed_with_a_visible_warning() {
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

    // The user's own, real Codex home -- resolve_provider_home prefers the
    // CODEX_HOME environment variable, so pointing it here is how a real
    // shell session would have it set, too.
    let real_codex_home = temp.path().join("real-codex-home");
    let user_skill = real_codex_home.join("skills/handover-switch");
    std::fs::create_dir_all(&user_skill).unwrap();
    std::fs::write(
        user_skill.join("SKILL.md"),
        "---\nname: handover-switch\ndescription: user skill\n---\nTOTALLY-NOT-HANDOVERS-CONTENT\n",
    )
    .unwrap();

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_TEST_TRACE", &trace)
        .env("PATH", &path)
        .env("CODEX_HOME", &real_codex_home)
        .args(["run", "codex"])
        .assert()
        .success()
        .stderr(predicate::str::contains("shadowed"));

    // The private CODEX_HOME the child was actually handed still serves
    // Handover's own skill, not the user's -- recovered the same way the
    // test above recovers it, from the fake codex's trace file.
    let private_codex_home = std::fs::read_to_string(trace.join("codex.home"))
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(
        private_codex_home, "unset",
        "CODEX_HOME was not set for the child"
    );
    let private_codex_home = std::path::PathBuf::from(private_codex_home);
    let skill = private_codex_home.join("skills/handover-switch/SKILL.md");
    let text = std::fs::read_to_string(&skill)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", skill.display()));
    assert!(
        text.contains("--from-provider"),
        "the private home must serve Handover's own skill, got: {text}"
    );
    assert!(
        !text.contains("TOTALLY-NOT-HANDOVERS-CONTENT"),
        "the user's colliding skill content must not leak through, got: {text}"
    );
}
