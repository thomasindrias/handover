mod support;

use std::os::unix::fs::PermissionsExt;

use assert_cmd::cargo::cargo_bin_cmd;
use handover::model::EventEnvelope;
use predicates::prelude::*;
use tempfile::TempDir;

use support::{
    add_linked_worktree, git, init_repo, path_with, repository_fingerprint, write_executable,
};

#[test]
fn switches_from_claude_to_codex_without_losing_the_session_or_worktree() {
    let temp = TempDir::new().unwrap();
    let repository = temp.path().join("platform");
    init_repo(&repository);
    let source_cwd = repository.join("apps/web");
    std::fs::create_dir_all(&source_cwd).unwrap();
    std::fs::write(source_cwd.join("README.md"), "web app\n").unwrap();
    git(&repository, &["add", "apps/web/README.md"]);
    git(&repository, &["commit", "-m", "add web app"]);

    let worktree = temp.path().join("oauth worktree");
    add_linked_worktree(&repository, &worktree, "feat/oauth");
    let nested_cwd = worktree.join("apps/web");
    let state = temp.path().join("state");
    let trace = temp.path().join("trace");
    std::fs::create_dir(&trace).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("handover"),
        "#!/usr/bin/env bash\nprintf '%s\\n' 'PATH-decoy handover was invoked' >&2\nexit 99\n",
    );
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
hook() { printf '%s' "$2" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook start '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"SessionStart","source":"startup"}'
hook prompt '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"UserPromptSubmit","prompt":"Implement OAuth callback with PKCE"}'
hook pre '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_unit"},"tool_use_id":"tool-pass"}'
hook post '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_unit"},"tool_response":{"stdout":"1 passed","stderr":"","exit_code":0},"tool_use_id":"tool-pass"}'
printf 'callback with pkce\n' > oauth_callback.rs
hook fail '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_integration"},"tool_response":{"stdout":"0 passed; 1 failed","stderr":"assertion failed: callback state","exit_code":101},"tool_use_id":"tool-fail"}'
printf '%s' '{"objective":"Implement OAuth callback with PKCE","summary":"Callback and PKCE are implemented","decisions":[{"statement":"Keep verifier in the session cookie","reason":"Avoid server-side state"}],"assumptions":[],"constraints":[],"completed":["OAuth callback","PKCE"],"in_progress":[],"blockers":["integration test failure"],"next_steps":["Fix callback integration test"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
hook stop '{"session_id":"claude-native","transcript_path":null,"cwd":"'"$cwd_json"'","permission_mode":"default","hook_event_name":"Stop"}'
exit 75
"#,
    );
    write_executable(
        &bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
printf '%s\n' "$@" > "$HANDOVER_TEST_TRACE/codex.args"
printf '%s\n' "$PWD" > "$HANDOVER_TEST_TRACE/codex.cwd"
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"codex-native","turn_id":"turn-1","transcript_path":null,"cwd":"'"$cwd_json"'","model":"gpt-test","permission_mode":"default","hook_event_name":"SessionStart","source":"startup"}' | "$HANDOVER_HOOK_BIN" __hook codex > "$HANDOVER_TEST_TRACE/codex.context.json"
exit 0
"#,
    );
    let path = path_with(&bin);

    cargo_bin_cmd!("handover")
        .current_dir(&nested_cwd)
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_TEST_TRACE", &trace)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .code(75)
        .stderr(predicate::str::contains("PATH-decoy").not());

    let before_switch = repository_fingerprint(&worktree);
    cargo_bin_cmd!("handover")
        .current_dir(&worktree)
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_TEST_TRACE", &trace)
        .env("PATH", &path)
        .args(["switch", "codex", "--", "--model", "gpt-test"])
        .assert()
        .success()
        .stderr(predicate::str::contains("PATH-decoy").not());
    let after_switch = repository_fingerprint(&worktree);

    assert_eq!(before_switch, after_switch);
    assert_eq!(
        std::fs::read_to_string(trace.join("codex.cwd"))
            .unwrap()
            .trim(),
        nested_cwd.canonicalize().unwrap().to_str().unwrap()
    );
    let hook_output: serde_json::Value =
        serde_json::from_slice(&std::fs::read(trace.join("codex.context.json")).unwrap()).unwrap();
    let handover = hook_output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let codex_args = std::fs::read_to_string(trace.join("codex.args")).unwrap();
    assert!(codex_args.lines().any(|argument| argument == "--model"));
    assert!(codex_args.lines().any(|argument| argument == "gpt-test"));
    assert_eq!(
        codex_args.lines().last(),
        Some(handover::handover::BOOTSTRAP)
    );
    for expected in [
        "Implement OAuth callback with PKCE",
        "Keep verifier in the session cookie",
        "apps/web/oauth_callback.rs",
        "cargo test oauth_unit",
        "status exit 0",
        "assertion failed: callback state",
        "Fix callback integration test",
    ] {
        assert!(
            handover.contains(expected),
            "handover is missing {expected:?}:\n{handover}"
        );
    }
    let session = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let envelopes: Vec<EventEnvelope> = std::fs::read_to_string(session.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(envelopes.iter().all(|envelope| envelope.verify().is_ok()));
    let codex_run = envelopes
        .iter()
        .rev()
        .find(|envelope| {
            envelope.event.provider == Some(handover::model::Provider::Codex)
                && matches!(
                    envelope.event.kind,
                    handover::model::EventKind::RunStarted { .. }
                )
        })
        .unwrap()
        .event
        .run_id
        .as_ref()
        .unwrap();
    let run_dir = session.join("runs").join(codex_run.to_string());
    let stored_handover = run_dir.join("inbox/handover.md");
    let recent_events = run_dir.join("inbox/recent-events.jsonl");
    assert_eq!(std::fs::read_to_string(&stored_handover).unwrap(), handover);
    for path in [&stored_handover, &recent_events] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    for line in std::fs::read_to_string(recent_events).unwrap().lines() {
        serde_json::from_str::<EventEnvelope>(line)
            .unwrap()
            .verify()
            .unwrap();
    }
    assert!(!session.join("refs/active-run.json").exists());
    assert!(
        !std::fs::read_dir(session.join("runs"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.'))
    );
    assert_eq!(
        git_text(&worktree, &["branch", "--show-current"]),
        "feat/oauth"
    );
    assert_no_provider_or_handover_state(&worktree);
}

fn git_text(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("--no-pager")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn assert_no_provider_or_handover_state(root: &std::path::Path) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(
                !matches!(name.as_ref(), ".handover" | ".claude" | ".codex"),
                "unexpected repository-local state at {}",
                path.display()
            );
            if name != ".git" && std::fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path);
            }
        }
    }
}
