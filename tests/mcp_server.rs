mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

use support::{init_repo, path_with, write_executable};

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const RUN_ID: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn mcp_server_exits_cleanly_on_an_empty_stream() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");

    cargo_bin_cmd!("handover")
        .env("HANDOVER_HOME", &state)
        .args(["mcp-server"])
        .write_stdin("")
        .assert()
        .success();
}

#[test]
fn mcp_server_starts_under_an_attached_provider_while_other_commands_stay_refused() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");

    // The server itself starts, and a real tool call inside it succeeds —
    // not just an empty stream exiting cleanly. `list` works without a bound
    // session (it reports an empty session array), so no fixture session is
    // needed here.
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "list", "arguments": {} },
    });
    let output = cargo_bin_cmd!("handover")
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_SESSION_ID", SESSION_ID)
        .env("HANDOVER_RUN_ID", RUN_ID)
        .args(["mcp-server"])
        .write_stdin(format!("{request}\n"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let responses: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 1);
    assert!(responses[0].get("error").is_none());
    assert_eq!(responses[0]["result"]["isError"], false);

    // Meanwhile the same commands invoked as ordinary CLI subprocesses under
    // the same attached-provider environment stay refused: `list`, `status`,
    // and `handover` are the three commands the MCP server re-exposes.
    for args in [
        vec!["list", "--json"],
        vec!["status", "--json"],
        vec!["preview", "codex", "--json"],
    ] {
        cargo_bin_cmd!("handover")
            .env("HANDOVER_HOME", &state)
            .env("HANDOVER_SESSION_ID", SESSION_ID)
            .env("HANDOVER_RUN_ID", RUN_ID)
            .args(&args)
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "an attached provider may only invoke Handover hooks or submit provider checkpoints",
            ));
    }

    // A refused agent has to be able to correct itself, so the refusal names
    // the flag that makes a checkpoint the one mutation it may perform.
    cargo_bin_cmd!("handover")
        .env("HANDOVER_HOME", &state)
        .env("HANDOVER_SESSION_ID", SESSION_ID)
        .env("HANDOVER_RUN_ID", RUN_ID)
        .args(["checkpoint"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--from-provider"));
}

fn send(
    repo: &std::path::Path,
    state: &std::path::Path,
    body: &str,
) -> (bool, Vec<serde_json::Value>) {
    let output = cargo_bin_cmd!("handover")
        .current_dir(repo)
        .env("HANDOVER_HOME", state)
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
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "handover");
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
    assert_eq!(names, vec!["list", "preview", "status"]);
    assert_eq!(
        tools[1]["inputSchema"]["required"],
        serde_json::json!(["provider"])
    );
}

#[test]
fn ping_returns_an_empty_result_not_an_error() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = temp.path().join("state");

    let (success, responses) = send(
        &repo,
        &state,
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
    );

    assert!(success);
    assert_eq!(responses.len(), 1);
    assert!(responses[0].get("error").is_none());
    assert_eq!(responses[0]["result"], serde_json::json!({}));
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

#[test]
fn an_invalid_utf8_line_gets_a_parse_error_and_the_stream_continues() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = temp.path().join("state");

    // Send invalid UTF-8 bytes followed by a valid JSON-RPC request
    let mut input = Vec::new();
    input.extend_from_slice(b"\xff\xfe\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n");

    let output = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["mcp-server"])
        .write_stdin(input)
        .output()
        .unwrap();

    let responses: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    // Process must succeed
    assert!(output.status.success());
    // Should get two responses: parse error for invalid UTF-8, then success for tools/list
    assert_eq!(responses.len(), 2);
    // First response: parse error with code -32700 for the bad line
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert!(responses[0]["id"].is_null());
    // Second response: successful tools/list
    assert!(responses[1]["result"]["tools"].is_array());
}

fn fake_claude_with_narrative(bin: &std::path::Path) {
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
printf '%s' '{"objective":"Ship it","summary":"On track.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Finish"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
}

fn run_fake_claude() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_claude_with_narrative(&bin);
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["run", "claude"])
        .assert()
        .success();

    (temp, repo, state)
}

fn call_tool(
    repo: &std::path::Path,
    state: &std::path::Path,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    });
    let (success, responses) = send(repo, state, &format!("{request}\n"));
    assert!(success);
    assert_eq!(responses.len(), 1);
    responses[0]["result"].clone()
}

#[test]
fn list_tool_matches_the_cli_json_projection() {
    let (_temp, repo, state) = run_fake_claude();
    let cli_output = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["list", "--json"])
        .output()
        .unwrap();
    let cli_value: serde_json::Value = serde_json::from_slice(&cli_output.stdout).unwrap();

    let result = call_tool(&repo, &state, "list", serde_json::json!({}));
    let text = result["content"][0]["text"].as_str().unwrap();
    let tool_value: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(tool_value, cli_value);
    assert_eq!(result["isError"], false);
}

#[test]
fn handover_tool_matches_the_cli_json_projection() {
    let (_temp, repo, state) = run_fake_claude();
    let cli_output = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["preview", "codex", "--json"])
        .output()
        .unwrap();
    let cli_value: serde_json::Value = serde_json::from_slice(&cli_output.stdout).unwrap();

    let result = call_tool(
        &repo,
        &state,
        "preview",
        serde_json::json!({"provider": "codex"}),
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    let tool_value: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(tool_value, cli_value);
    assert_eq!(result["isError"], false);
}

#[test]
fn status_tool_matches_the_cli_json_projection() {
    let (_temp, repo, state) = run_fake_claude();
    let cli_output = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let cli_value: serde_json::Value = serde_json::from_slice(&cli_output.stdout).unwrap();

    let result = call_tool(&repo, &state, "status", serde_json::json!({}));
    let text = result["content"][0]["text"].as_str().unwrap();
    let tool_value: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(tool_value, cli_value);
    assert_eq!(result["isError"], false);
}

#[test]
fn handover_tool_reports_a_domain_error_when_no_session_is_bound() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");

    let result = call_tool(
        &repo,
        &state,
        "preview",
        serde_json::json!({"provider": "codex"}),
    );
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("this worktree has no Handover session")
    );
}

#[test]
fn status_tool_reports_a_domain_error_when_no_session_is_bound() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");

    let result = call_tool(&repo, &state, "status", serde_json::json!({}));
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("this worktree has no Handover session")
    );
}

#[test]
fn handover_tool_rejects_a_missing_provider_argument_as_a_domain_error() {
    let (_temp, repo, state) = run_fake_claude();
    let result = call_tool(&repo, &state, "preview", serde_json::json!({}));
    assert_eq!(result["isError"], true);
}

#[test]
fn an_unknown_tool_name_returns_a_json_rpc_error_not_a_tool_result() {
    let (_temp, repo, state) = run_fake_claude();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "switch", "arguments": {} },
    });
    let (success, responses) = send(&repo, &state, &format!("{request}\n"));
    assert!(success);
    assert_eq!(responses[0]["error"]["code"], -32601);
}
