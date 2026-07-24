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

fn send(
    repo: &std::path::Path,
    state: &std::path::Path,
    body: &str,
) -> (bool, Vec<serde_json::Value>) {
    let output = cargo_bin_cmd!("sesh")
        .current_dir(repo)
        .env("SESH_HOME", state)
        .args(["mcp-server"])
        .write_stdin(body)
        .output()
        .unwrap();
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (output.status.success(), responses)
}

#[test]
fn initialize_echoes_the_requested_protocol_version_and_reports_server_info() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = temp.path().join("state");

    let (success, responses) = send(
        &repo,
        &state,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\"}}\n",
    );

    assert!(success);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "sesh");
    assert_eq!(
        responses[0]["result"]["capabilities"]["tools"],
        serde_json::json!({})
    );
}

#[test]
fn notifications_initialized_receives_no_response() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = temp.path().join("state");

    let (success, responses) = send(
        &repo,
        &state,
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    );

    assert!(success);
    assert!(responses.is_empty());
}

#[test]
fn tools_list_reports_the_three_tools_with_their_schemas() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = temp.path().join("state");

    let (success, responses) = send(
        &repo,
        &state,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n",
    );

    assert!(success);
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["list", "handoff", "status"]);
    assert_eq!(
        tools[1]["inputSchema"]["required"],
        serde_json::json!(["provider"])
    );
}

#[test]
fn an_unknown_method_returns_a_json_rpc_error() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = temp.path().join("state");

    let (success, responses) = send(
        &repo,
        &state,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"resources/list\"}\n",
    );

    assert!(success);
    assert_eq!(responses[0]["error"]["code"], -32601);
}

#[test]
fn a_malformed_line_gets_a_parse_error_and_the_stream_continues() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = temp.path().join("state");

    let (success, responses) = send(
        &repo,
        &state,
        "not json at all\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n",
    );

    assert!(success);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert!(responses[0]["id"].is_null());
    assert!(responses[1]["result"]["tools"].is_array());
}
