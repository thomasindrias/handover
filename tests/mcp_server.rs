mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const RUN_ID: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn mcp_server_exits_cleanly_on_an_empty_stream() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");

    cargo_bin_cmd!("sesh")
        .env("SESH_HOME", &state)
        .args(["mcp-server"])
        .write_stdin("")
        .assert()
        .success();
}

#[test]
fn mcp_server_starts_under_an_attached_provider_while_other_commands_stay_refused() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");

    cargo_bin_cmd!("sesh")
        .env("SESH_HOME", &state)
        .env("SESH_SESSION_ID", SESSION_ID)
        .env("SESH_RUN_ID", RUN_ID)
        .args(["mcp-server"])
        .write_stdin("")
        .assert()
        .success();

    cargo_bin_cmd!("sesh")
        .env("SESH_HOME", &state)
        .env("SESH_SESSION_ID", SESSION_ID)
        .env("SESH_RUN_ID", RUN_ID)
        .args(["list", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "an attached provider may only invoke Sesh hooks or submit provider checkpoints",
        ));
}
