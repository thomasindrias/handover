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

// There is deliberately no Codex equivalent of the Claude smoke test above.
// Unlike `claude plugin validate`, no Codex CLI subcommand inspects a
// hooks.json file without starting a real session: `codex doctor` was
// tried and empirically ignores the file entirely (verified by deleting it
// and by feeding it corrupted content — both produced an identical,
// successful `doctor` report). Asserting `doctor` succeeds would only ever
// test whether Codex is authenticated, which is misleading to keep as a
// "smoke test" for the hooks mechanism. `CodexAdapter`'s own unit tests
// (src/provider/codex.rs) cover the materialized file's shape and content
// without needing the real CLI at all. Proof that Codex actually reads and
// fires the hooks requires a real session — that's a manual, maintainer-run
// check (see the design doc / plan's "Manual verification" section), not
// something this suite can assert without spending model quota.
