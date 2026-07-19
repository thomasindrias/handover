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
#[ignore = "requires the provider CLI to be installed"]
fn codex_accepts_every_static_hook_overlay_without_opening_a_session() {
    let temp = TempDir::new().unwrap();
    let layout = StateLayout::new(temp.path().join("state"));
    layout.ensure().unwrap();
    adapter(Provider::Codex)
        .setup(&layout.integrations())
        .unwrap();
    let overlays =
        std::fs::read_to_string(layout.integrations().join("codex/1/hooks.txt")).unwrap();
    let mut command = Command::new("codex");
    command
        .env("SESH_HOME", layout.root())
        .arg("--strict-config");
    for overlay in overlays.lines() {
        command.args(["-c", overlay]);
    }
    let output = command.args(["features", "list"]).output().unwrap();

    assert!(
        output.status.success(),
        "codex strict overlay validation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
