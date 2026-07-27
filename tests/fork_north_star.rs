mod support;

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use handover::git::Git;
use handover::model::{
    EventEnvelope, EventKind, ForkOperation, ForkPhase, SessionMeta, WorktreeRef,
};
use support::{
    add_linked_worktree, git, init_repo, path_with, repository_fingerprint, write_executable,
};
use tempfile::TempDir;

#[test]
fn forks_from_claude_to_codex_with_explicit_child_lineage_and_exact_state() {
    let temp = TempDir::new().unwrap();
    let repository = temp.path().join("platform");
    init_repo(&repository);
    std::fs::create_dir_all(repository.join("apps/web")).unwrap();
    std::fs::write(repository.join("apps/web/tracked.rs"), "base\n").unwrap();
    git(&repository, &["add", "apps/web/tracked.rs"]);
    git(&repository, &["commit", "-m", "add web app"]);
    let source = temp.path().join("oauth worktree");
    add_linked_worktree(&repository, &source, "feat/oauth");
    let source_cwd = source.join("apps/web");
    let state = temp.path().join("state");
    let trace = temp.path().join("trace");
    std::fs::create_dir(&trace).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook '{"session_id":"claude-native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
hook '{"session_id":"claude-native","cwd":"'"$cwd_json"'","hook_event_name":"UserPromptSubmit","prompt":"Implement OAuth callback with PKCE"}'
hook '{"session_id":"claude-native","cwd":"'"$cwd_json"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_unit"},"tool_use_id":"pass"}'
hook '{"session_id":"claude-native","cwd":"'"$cwd_json"'","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_unit"},"tool_response":{"stdout":"1 passed","stderr":"","exit_code":0},"tool_use_id":"pass"}'
printf 'staged callback\n' > staged_callback.rs
git add staged_callback.rs
printf 'unstaged verifier\n' >> tracked.rs
printf 'untracked state\n' > oauth_state.txt
hook '{"session_id":"claude-native","cwd":"'"$cwd_json"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_integration"},"tool_use_id":"fail"}'
hook '{"session_id":"claude-native","cwd":"'"$cwd_json"'","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test oauth_integration"},"tool_response":{"stdout":"0 passed; 1 failed","stderr":"assertion failed: callback state","exit_code":101},"tool_use_id":"fail"}'
printf '%s' '{"objective":"Implement OAuth callback with PKCE","summary":"Callback is implemented; integration remains","decisions":[{"statement":"Keep verifier in the session cookie","reason":"Avoid server-side state"}],"assumptions":[],"constraints":[],"completed":["OAuth callback"],"in_progress":["PKCE integration"],"blockers":["integration test failure"],"next_steps":["Fix callback integration test"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
hook '{"session_id":"claude-native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 75
"#,
    );
    write_executable(
        &bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
printf '%s\n' "$PWD" > "$HANDOVER_TEST_TRACE/codex.cwd"
printf '%s\n' "$HANDOVER_SESSION_ID" > "$HANDOVER_TEST_TRACE/codex.session"
git branch --show-current > "$HANDOVER_TEST_TRACE/codex.branch"
printf '%s\n' "$@" > "$HANDOVER_TEST_TRACE/codex.args"
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"codex-native","turn_id":"turn-1","cwd":"'"$cwd_json"'","model":"gpt-test","hook_event_name":"SessionStart","source":"startup"}' | "$HANDOVER_HOOK_BIN" __hook codex > "$HANDOVER_TEST_TRACE/codex.context.json"
exit 0
"#,
    );
    let path = path_with(&bin);

    cargo_bin_cmd!("handover")
        .current_dir(&source_cwd)
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_TEST_TRACE", &trace)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .code(75);
    let source_before = repository_fingerprint(&source);
    let source_snapshot = Git::new().snapshot(&source_cwd).unwrap();

    cargo_bin_cmd!("handover")
        .current_dir(&source)
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_TEST_TRACE", &trace)
        .env("PATH", &path)
        .args(["fork", "codex", "--", "--model", "gpt-test"])
        .assert()
        .success();

    assert_eq!(repository_fingerprint(&source), source_before);
    let codex_cwd = PathBuf::from(
        std::fs::read_to_string(trace.join("codex.cwd"))
            .unwrap()
            .trim(),
    );
    assert_eq!(codex_cwd.file_name().unwrap(), "web");
    assert_eq!(codex_cwd.parent().unwrap().file_name().unwrap(), "apps");
    let target = codex_cwd.parent().unwrap().parent().unwrap().to_path_buf();
    assert_ne!(target, source);
    let branch = std::fs::read_to_string(trace.join("codex.branch"))
        .unwrap()
        .trim()
        .to_owned();
    assert!(branch.starts_with("handover/oauth-worktree-"));
    let child_id = std::fs::read_to_string(trace.join("codex.session"))
        .unwrap()
        .trim()
        .to_owned();
    let hook_output: serde_json::Value =
        serde_json::from_slice(&std::fs::read(trace.join("codex.context.json")).unwrap()).unwrap();
    let handover = hook_output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    for expected in [
        "Forked from session",
        child_id.as_str(),
        branch.as_str(),
        "Implement OAuth callback with PKCE",
        "Keep verifier in the session cookie",
        "Fix callback integration test",
        "cargo test oauth_unit",
        "cargo test oauth_integration",
        "assertion failed: callback state",
        "apps/web/staged_callback.rs",
        "apps/web/tracked.rs",
        "apps/web/oauth_state.txt",
    ] {
        assert!(
            handover.contains(expected),
            "child handover is missing {expected:?}:\n{handover}"
        );
    }
    let target_snapshot = Git::new().snapshot(&codex_cwd).unwrap();
    assert_eq!(target_snapshot.head, source_snapshot.head);
    assert_eq!(target_snapshot.staged, source_snapshot.staged);
    assert_eq!(target_snapshot.unstaged, source_snapshot.unstaged);
    assert_eq!(target_snapshot.untracked, source_snapshot.untracked);

    let mut metas = std::fs::read_dir(state.join("sessions"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let meta: SessionMeta =
                serde_json::from_slice(&std::fs::read(path.join("meta.json")).unwrap()).unwrap();
            (path, meta)
        })
        .collect::<Vec<_>>();
    metas.sort_by_key(|item| item.1.id.to_string());
    assert_eq!(metas.len(), 2);
    let (_, child) = metas
        .iter()
        .find(|(_, meta)| meta.id.to_string() == child_id)
        .unwrap();
    let (parent_dir, parent) = metas
        .iter()
        .find(|(_, meta)| meta.parent_session_id.is_none())
        .unwrap();
    assert_eq!(child.parent_session_id.as_ref(), Some(&parent.id));
    assert!(child.parent_checkpoint_sequence.unwrap() > 0);
    assert_eq!(child.worktree.worktree, target);
    let parent_events = read_events(parent_dir);
    assert!(parent_events.iter().any(|event| matches!(
        event.event.kind,
        EventKind::SessionForked {
            ref child_session_id,
            parent_checkpoint_sequence,
            ref target_branch,
            ..
        } if child_session_id == &child.id
            && parent_checkpoint_sequence == child.parent_checkpoint_sequence.unwrap()
            && target_branch == &branch
    )));
    let child_dir = state.join("sessions").join(&child_id);
    let child_events = read_events(&child_dir);
    assert!(matches!(
        child_events[0].event.kind,
        EventKind::SessionCreated { .. }
    ));
    assert!(
        child_events
            .iter()
            .all(|event| event.event.session_id == child.id)
    );
    assert!(
        !child_events
            .iter()
            .any(|event| matches!(event.event.kind, EventKind::SessionForked { .. }))
    );
    assert!(!child_dir.join("refs/active-run.json").exists());

    let operation_path = std::fs::read_dir(state.join("operations"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("operation.json");
    let operation: ForkOperation =
        serde_json::from_slice(&std::fs::read(operation_path).unwrap()).unwrap();
    assert_eq!(operation.phase, ForkPhase::Complete);
    assert_eq!(operation.child_session_id.as_ref(), Some(&child.id));

    let refs = std::fs::read_dir(state.join("refs/worktrees"))
        .unwrap()
        .map(|entry| {
            serde_json::from_slice::<WorktreeRef>(&std::fs::read(entry.unwrap().path()).unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(refs.len(), 2);
    assert!(
        refs.iter()
            .any(|reference| reference.session_id == parent.id)
    );
    assert!(
        refs.iter()
            .any(|reference| reference.session_id == child.id)
    );
    assert_eq!(
        git_text(&source, &["branch", "--show-current"]),
        "feat/oauth"
    );
    assert_eq!(git_text(&target, &["branch", "--show-current"]), branch);
    assert_no_provider_state(&source);
    assert_no_provider_state(&target);
}

fn read_events(session: &Path) -> Vec<EventEnvelope> {
    std::fs::read_to_string(session.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<EventEnvelope>(line).unwrap())
        .inspect(|envelope| envelope.verify().unwrap())
        .collect()
}

fn git_text(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn assert_no_provider_state(root: &Path) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(!matches!(name.as_ref(), ".handover" | ".claude" | ".codex"));
            if name != ".git" && std::fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path);
            }
        }
    }
}
