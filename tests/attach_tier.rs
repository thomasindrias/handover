mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

/// A worktree adopted by `handover attach` — no run, no hooks, no lease.
fn adopted_worktree() -> (
    TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::ffi::OsString,
) {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // `attach` probes nothing, but `status` renders a handover for the other
    // provider, so both names must resolve.
    for name in ["claude", "codex"] {
        write_executable(
            &bin.join(name),
            "#!/usr/bin/env bash\nif [[ ${1:-} == \"--version\" ]]; then printf '%s\\n' 'fake 1.0'; exit 0; fi\nexit 0\n",
        );
    }
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["attach", "claude"])
        .assert()
        .success();

    (temp, repo, state, path)
}

#[test]
fn status_reports_an_adopted_session_as_attach_tier_with_its_provider() {
    let (_temp, repo, state, path) = adopted_worktree();

    let output = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(status["binding"]["tier"], "attached");
    assert_eq!(status["binding"]["detached"], false);
    // The session is bound to a provider and Handover knows which one;
    // reporting null here would be a lie the journal can disprove.
    assert_eq!(status["binding"]["provider"], "claude");
    assert_eq!(status["provider"], "claude");
    // The adopted session is on claude, so the useful next step is the other
    // provider -- not the old fallback, which named claude and told the user
    // to switch to the provider they were already on.
    assert_eq!(
        status["switch_readiness"]["suggested_switch_command"],
        "handover switch codex"
    );
}

/// After a claim moves an adopted session on, the old desktop window is still
/// on screen but nothing it does is journaled, and Handover cannot make it
/// quit. `status` must report that honestly: nothing is bound right now, and
/// the binding block names what was last attached -- claude -- as detached.
///
/// This single test pins both halves of the wiring `build_status_value` added:
/// the `binding` block reflects `Binding` field-for-field (not a hardcoded
/// stand-in), and the top-level `provider` goes to `null` once `previous_provider`
/// recognises a detached binding as nothing being bound.
#[test]
fn status_reports_a_detached_binding_as_unbound_with_the_last_provider_named() {
    let (_temp, repo, state, path) = adopted_worktree();

    let handover = |args: &[&str]| {
        cargo_bin_cmd!("handover")
            .current_dir(&repo)
            .env("HANDOVER_HOME", &state)
            .env("PATH", &path)
            .args(args)
            .output()
            .unwrap()
    };

    // The sequence of the `session.attached` event that bound this session --
    // read from the journal, not assumed, since `arm` and `claim` each append
    // events of their own first.
    let log = handover(&["log", "--json"]);
    assert!(
        log.status.success(),
        "{}",
        String::from_utf8_lossy(&log.stderr)
    );
    let attached_sequence = String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|envelope| envelope["event"]["type"] == "session.attached")
        .expect("attach recorded a session.attached event")["event"]["sequence"]
        .as_u64()
        .unwrap();

    let arm = handover(&["arm", "codex"]);
    assert!(
        arm.status.success(),
        "{}",
        String::from_utf8_lossy(&arm.stderr)
    );

    let claim = handover(&["claim"]);
    assert!(
        claim.status.success(),
        "{}",
        String::from_utf8_lossy(&claim.stderr)
    );

    let output = handover(&["status", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(status["binding"]["tier"], "attached");
    assert_eq!(status["binding"]["provider"], "claude");
    assert_eq!(status["binding"]["sequence"], attached_sequence);
    assert_eq!(status["binding"]["detached"], true);
    // Nothing is bound right now -- the claim moved the session to codex, and
    // naming claude here would assert a binding that no longer exists. Read
    // together with the fields above: the last attachment was claude, and it
    // is detached.
    assert_eq!(status["provider"], serde_json::Value::Null);
}

#[test]
fn status_reports_a_supervised_session_as_supervised_tier() {
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
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
    write_executable(
        &bin.join("codex"),
        "#!/usr/bin/env bash\nif [[ ${1:-} == \"--version\" ]]; then printf '%s\\n' 'fake-codex 1.0'; exit 0; fi\nexit 0\n",
    );
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    let output = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(status["binding"]["tier"], "supervised");
    assert_eq!(status["binding"]["detached"], false);
    assert_eq!(status["provider"], "claude");
}
