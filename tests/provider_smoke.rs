use std::process::Command;

use sesh::model::Provider;
use sesh::provider::adapter;
use sesh::store::StateLayout;
use tempfile::TempDir;

#[test]
#[ignore = "requires the provider CLI to be installed"]
fn claude_validates_the_materialized_sesh_plugin_without_opening_a_session() {
    let temp = TempDir::new().unwrap();
    let layout = StateLayout::new(temp.path().join("state"));
    layout.ensure().unwrap();
    adapter(Provider::Claude)
        .setup(&layout.integrations())
        .unwrap();
    let plugin = layout.integrations().join("claude/1");

    let output = Command::new("claude")
        .env("SESH_HOME", layout.root())
        .args(["plugin", "validate"])
        .arg(&plugin)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "claude plugin validate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires the provider CLI to be installed and authenticated"]
fn codex_accepts_the_materialized_hooks_file_without_opening_a_session() {
    let temp = TempDir::new().unwrap();
    let layout = StateLayout::new(temp.path().join("state"));
    layout.ensure().unwrap();
    adapter(Provider::Codex)
        .setup(&layout.integrations())
        .unwrap();
    let codex_home = temp.path().join("codex_home");
    std::fs::create_dir(&codex_home).unwrap();
    std::os::unix::fs::symlink(
        layout.integrations().join("codex/1/hooks.json"),
        codex_home.join("hooks.json"),
    )
    .unwrap();
    // `codex doctor` treats missing auth as a hard failure independent of
    // hooks.json, so mirror what a real launch does (best-effort symlink of
    // the real auth.json) rather than testing against an artificially
    // incomplete CODEX_HOME.
    if let Some(real_auth) = real_codex_home().map(|home| home.join("auth.json"))
        && real_auth.exists()
    {
        std::os::unix::fs::symlink(real_auth, codex_home.join("auth.json")).unwrap();
    }

    let output = Command::new("codex")
        .env("CODEX_HOME", &codex_home)
        .arg("doctor")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "codex doctor failed with the materialized hooks.json present: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn real_codex_home() -> Option<std::path::PathBuf> {
    if let Ok(home) = std::env::var("CODEX_HOME") {
        return Some(std::path::PathBuf::from(home));
    }
    std::env::var("HOME")
        .ok()
        .map(|home| std::path::PathBuf::from(home).join(".codex"))
}
