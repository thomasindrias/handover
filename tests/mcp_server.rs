mod support;

use assert_cmd::cargo::cargo_bin_cmd;
use handover::model::{RunId, SessionId};
use tempfile::TempDir;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

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
                "an attached provider may only invoke Handover hooks",
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

/// The two fake providers a run and a claim need on `PATH`.
fn fake_providers(bin: &std::path::Path) {
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
printf '%s' '{"objective":"Be armed","summary":"Ready.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Continue"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
    write_executable(
        &bin.join("codex"),
        "#!/usr/bin/env bash\nif [[ ${1:-} == \"--version\" ]]; then printf '%s\\n' 'fake-codex 1.0'; exit 0; fi\nexit 0\n",
    );
}

/// Initialize `repo`, run a provider in it to completion, and arm a switch to
/// codex from the terminal.
///
/// The run is over, so its lease is cleared while its run directory — and the
/// checkpoint inbox inside it — survives. That is precisely the leftover run
/// environment `authorize_run_scoped_write`'s lease requirement exists to
/// refuse, and the only way to reach it without leaving a real provider
/// running for the length of the test.
///
/// The pending arm matters as much as the run does: without one, a `claim`
/// would fail at "no switch is armed for this session" whether or not
/// authorization ran at all, and a regression that dropped run-scoping would
/// ship green.
fn armed_run_in(repo: &std::path::Path, state: &std::path::Path, path: &std::ffi::OsStr) {
    init_repo(repo);
    cargo_bin_cmd!("handover")
        .current_dir(repo)
        .env("HANDOVER_HOME", state)
        .env("PATH", path)
        .args(["run", "claude"])
        .assert()
        .success();
    // Armed through the ordinary CLI, which needs no run environment.
    cargo_bin_cmd!("handover")
        .current_dir(repo)
        .env("HANDOVER_HOME", state)
        .env("PATH", path)
        .args(["arm", "codex"])
        .assert()
        .success();
}

/// One state root holding one such session.
fn armed_session_with_a_finished_run() -> (
    TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::ffi::OsString,
) {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    let state = temp.path().join("state");
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_providers(&bin);
    let path = path_with(&bin);
    armed_run_in(&repo, &state, &path);
    (temp, repo, state, path)
}

/// The session directory bound to `worktree`, and its id.
///
/// Resolved through the CLI's own `status` rather than by picking the lone
/// entry under `<state>/sessions`: one state root can hold two sessions, and
/// the mismatch test below depends on naming the right one.
fn session_dir_and_id(
    state: &std::path::Path,
    worktree: &std::path::Path,
    path: &std::ffi::OsStr,
) -> (std::path::PathBuf, SessionId) {
    let output = cargo_bin_cmd!("handover")
        .current_dir(worktree)
        .env("HANDOVER_HOME", state)
        .env("PATH", path)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = SessionId::parse(status["session_id"].as_str().unwrap()).unwrap();
    (state.join("sessions").join(id.to_string()), id)
}

/// The lone run directory under a session, and its id. Run directories outlive
/// the run that made them, which is what lets a test speak as a real, finished
/// run.
fn run_dir_and_id(session: &std::path::Path) -> (std::path::PathBuf, RunId) {
    let dir = std::fs::read_dir(session.join("runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let id = RunId::parse(dir.file_name().unwrap().to_str().unwrap()).unwrap();
    (dir, id)
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
fn tools_list_reports_each_tools_input_schema() {
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
    // The advertised roster is pinned by
    // `the_tool_list_advertises_the_three_reads_and_the_three_writes`. This
    // test's job is the schema shape, so it looks each tool up by name and
    // says nothing about which tools exist or in what order.
    let schema = |name: &str| -> serde_json::Value {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not advertised at all"))["inputSchema"]
            .clone()
    };

    // Every provider-targeted tool must say so, or a client will call it with
    // no provider and get a domain error it could have avoided.
    for name in ["preview", "arm", "attach"] {
        assert_eq!(schema(name)["type"], "object", "{name}");
        assert_eq!(
            schema(name)["required"],
            serde_json::json!(["provider"]),
            "{name} must declare provider as required"
        );
    }

    // `claim` takes no required argument: its only input is the optional
    // assertion of which arm is being consumed.
    assert_eq!(schema("claim")["type"], "object");
    assert!(schema("claim").get("required").is_none());
    assert_eq!(schema("claim")["properties"]["arm"]["type"], "integer");

    // `arm`'s two optional arguments are what the tool layer parses by hand.
    assert_eq!(schema("arm")["properties"]["surface"]["type"], "string");
    assert_eq!(schema("arm")["properties"]["ttl"]["type"], "string");
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

/// A caller carrying run credentials takes the strict, run-scoped path, and
/// that path is unchanged: a run whose lease has been cleared cannot arm or
/// claim, however genuine its environment is.
///
/// This is the security-relevant half of deriving `from_provider` instead of
/// pinning it. A launched provider always has `HANDOVER_RUN_ID` set, so it can
/// never take the worktree-scoped path by accident — and the run directory it
/// names outlives the run, which is exactly the leftover environment the lease
/// requirement exists to close off.
///
/// The environment here is real, not fabricated: session id, run id, and
/// checkpoint inbox all come from a run that genuinely happened, so
/// `active_run` accepts it and the refusal can only come from the lease.
#[test]
fn the_write_tools_still_refuse_a_run_that_no_longer_holds_the_lease() {
    let (temp, repo, state, path) = armed_session_with_a_finished_run();
    let (session, session_id) = session_dir_and_id(&state, &repo, &path);
    let (run, run_id) = run_dir_and_id(&session);

    for (name, arguments) in [
        ("arm", serde_json::json!({ "provider": "codex" })),
        ("claim", serde_json::json!({})),
    ] {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let output = cargo_bin_cmd!("handover")
            .current_dir(&repo)
            .env("HANDOVER_HOME", &state)
            .env("PATH", &path)
            .env("HANDOVER_SESSION_ID", session_id.to_string())
            .env("HANDOVER_RUN_ID", run_id.to_string())
            .env("HANDOVER_CHECKPOINT_INBOX", run.join("inbox/checkpoints"))
            .args(["mcp-server"])
            .write_stdin(format!("{request}\n"))
            .output()
            .unwrap();
        assert!(output.status.success(), "the server itself must not die");
        let responses: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 1);
        assert_eq!(
            responses[0]["result"]["isError"], true,
            "{name} must still refuse a run that no longer holds the lease: {:?}",
            responses[0]
        );
        // The refusal must come from the run-scoped authorization and nowhere
        // else. Drop that check and `arm` refuses with "already armed" while
        // `claim` succeeds outright; neither says this.
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(
            text.contains("no longer holds this session's lease"),
            "{name} must refuse at the run proof, not somewhere further in: {text}"
        );
    }

    let log = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&log.stdout).contains("switch.claimed"),
        "a refused write tool must leave no claim behind"
    );
    drop(temp);
}

/// The other refusal the strict path owes: a provider process can `cd`
/// anywhere, so a run attached to one session must not be able to walk into
/// another worktree and write the session it finds there.
///
/// Checked before the lease, so this is what a genuine, *live* run of another
/// session would hit too — the state a fabricated fixture cannot reach without
/// leaving a real provider running for the length of the test.
#[test]
fn the_write_tools_still_refuse_a_run_belonging_to_another_session() {
    // Both sessions share one `HANDOVER_HOME`, which is what a provider that
    // walks into a second worktree actually sees — and what lets the run proof
    // get past `active_run` and reach the session comparison itself.
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_providers(&bin);
    let path = path_with(&bin);
    let own_repo = temp.path().join("own");
    let other_repo = temp.path().join("other");
    armed_run_in(&own_repo, &state, &path);
    armed_run_in(&other_repo, &state, &path);
    let (other_session, other_session_id) = session_dir_and_id(&state, &other_repo, &path);
    let (other_run, other_run_id) = run_dir_and_id(&other_session);

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "arm", "arguments": { "provider": "codex" } },
    });
    // The credentials name the other session's run; the cwd resolves to this
    // one. Nothing here is fabricated: the run existed, its inbox is where the
    // environment says, and `active_run` accepts it — so the refusal can only
    // come from the session comparison.
    let output = cargo_bin_cmd!("handover")
        .current_dir(&own_repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .env("HANDOVER_SESSION_ID", other_session_id.to_string())
        .env("HANDOVER_RUN_ID", other_run_id.to_string())
        .env(
            "HANDOVER_CHECKPOINT_INBOX",
            other_run.join("inbox/checkpoints"),
        )
        .args(["mcp-server"])
        .write_stdin(format!("{request}\n"))
        .output()
        .unwrap();
    assert!(output.status.success(), "the server itself must not die");
    let responses: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        responses[0]["result"]["isError"], true,
        "arm must still refuse a run attached to another session: {:?}",
        responses[0]
    );
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("run the command from your own session's worktree"),
        "arm must refuse at the session comparison, not somewhere further in: {text}"
    );

    let log = cargo_bin_cmd!("handover")
        .current_dir(&own_repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&log.stdout).contains("switch.claimed"),
        "a refused write tool must leave no claim behind"
    );
}

/// A caller with no run credentials takes the worktree-scoped path — which is
/// exactly what the plain CLI `handover arm` already does for any process in
/// that worktree, and what the `attach` tool already did here.
///
/// This is the case the pinned `from_provider = true` made impossible. A
/// desktop session has no run environment at all — that is what attach tier
/// means — so it could pull its handover with `preview` and never arm its way
/// back out: the desktop leg was a one-way trip a human had to end from a
/// terminal.
#[test]
fn the_write_tools_serve_an_attached_session_that_has_no_run() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");

    // Adopt the worktree exactly as a desktop application would on its first
    // turn: over MCP, with no run environment.
    let attach = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "attach", "arguments": { "provider": "claude" } },
    });
    let (_, responses) = send(&repo, &state, &format!("{attach}\n"));
    assert_eq!(responses[0]["result"]["isError"], false);

    // And arm its way back out, over the same server, still with no run.
    let arm = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "arm", "arguments": { "provider": "codex" } },
    });
    let (success, responses) = send(&repo, &state, &format!("{arm}\n"));
    assert!(success);
    assert_eq!(
        responses[0]["result"]["isError"], false,
        "an attached session must be able to arm its own switch: {:?}",
        responses[0]
    );
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let armed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(armed["to"], "codex");
    let armed_sequence = armed["armed_sequence"].as_u64().unwrap();

    // `claim` completes it from the same place, which is what makes the leg a
    // round trip rather than a one-way one.
    let claim = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": { "name": "claim", "arguments": { "arm": armed_sequence } },
    });
    let (success, responses) = send(&repo, &state, &format!("{claim}\n"));
    assert!(success);
    assert_eq!(
        responses[0]["result"]["isError"], false,
        "an attached session must be able to claim its own arm: {:?}",
        responses[0]
    );
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let claimed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(claimed["to_provider"], "codex");
}

/// `attach` is the deliberate exception: it is scoped to the worktree its cwd
/// resolves to, because by definition no run exists yet.
#[test]
fn attach_is_worktree_scoped_rather_than_run_scoped() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = temp.path().join("state");

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "attach", "arguments": { "provider": "claude" } },
    });
    let (success, responses) = send(&repo, &state, &format!("{request}\n"));
    assert!(success);
    assert_eq!(
        responses[0]["result"]["isError"], false,
        "attach needs no run: {:?}",
        responses[0]
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["created"], true);
    assert_eq!(value["provider"], "claude");

    // Attaching again resolves to the same session rather than forking a
    // second history beside it.
    let (_, again) = send(&repo, &state, &format!("{request}\n"));
    let text = again[0]["result"]["content"][0]["text"].as_str().unwrap();
    let second: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(second["created"], false);
    assert_eq!(second["session_id"], value["session_id"]);
}

/// The write tools on their success path, in the only fixture that can reach
/// it: the MCP client is spawned *by* the provider Handover launched, so the
/// server inherits a genuine active run — the run environment and a live lease
/// — which is the one state `authorize_run_scoped_write` accepts.
///
/// Everything the tool layer parses by hand is exercised here: `arm`'s
/// `surface` and `ttl`, and `claim`'s `arm`.
#[test]
fn the_write_tools_succeed_inside_the_run_that_spawned_the_server() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let trace = temp.path().join("mcp-trace.jsonl");

    // The fake provider is its own MCP client: it pipes three tool calls into
    // `handover mcp-server` and parks the responses in a trace file. Both the
    // requests and `$HANDOVER_TEST_MCP_TRACE` reach the script through the
    // launched provider's environment, exactly as a real client would be
    // configured.
    write_executable(
        &bin.join("claude"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null; }
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
printf '%s' '{"objective":"Ship it","summary":"On track.","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["Finish"],"related_event_sequences":[]}' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"arm","arguments":{"provider":"codex","surface":"cli","ttl":"30m"}}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"claim","arguments":{}}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"claim","arguments":{"arm":999999}}}'
} | "$HANDOVER_HOOK_BIN" mcp-server > "$HANDOVER_TEST_MCP_TRACE"
hook '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
    // The arm made over MCP completes when claude exits, so the successor has
    // to be probeable.
    write_executable(
        &bin.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
hook() { printf '%s' "$1" | "$HANDOVER_HOOK_BIN" __hook codex >/dev/null; }
hook '{"session_id":"codex-native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}'
hook '{"session_id":"codex-native","cwd":"'"$cwd_json"'","hook_event_name":"Stop"}'
exit 0
"#,
    );
    let state = temp.path().join("state");
    let path = path_with(&bin);

    cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .env("HANDOVER_TEST_MCP_TRACE", &trace)
        .args(["run", "claude"])
        .assert()
        .success();

    let responses: Vec<serde_json::Value> = std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 3, "trace was: {responses:?}");

    // 1. `arm` succeeds, and both optional arguments reach the projection.
    let armed = &responses[0]["result"];
    assert_eq!(
        armed["isError"], false,
        "arm must succeed inside its own run: {:?}",
        responses[0]
    );
    let value: serde_json::Value =
        serde_json::from_str(armed["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(value["to"], "codex");
    assert_eq!(
        value["surface"], "cli",
        "the tool's surface argument must reach the projection, not the Auto default"
    );
    let expires = OffsetDateTime::parse(value["expires_at"].as_str().unwrap(), &Rfc3339).unwrap();
    let remaining = expires - OffsetDateTime::now_utc();
    assert!(
        remaining > Duration::minutes(20) && remaining <= Duration::minutes(30),
        "the tool's 30m ttl must reach the projection, not the 15m default; {remaining} remained"
    );
    let armed_sequence = value["armed_sequence"].as_u64().unwrap();

    // 2. `claim` refuses while the calling run's own lease is still live.
    // That is correct — the provider asking to claim is the provider that has
    // to quit first — so it is asserted rather than skipped.
    let live = &responses[1]["result"];
    assert_eq!(live["isError"], true, "{:?}", responses[1]);
    let text = live["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("claude is still running this session"),
        "claim must refuse on the live lease, not on authorization: {text}"
    );

    // 3. `claim`'s optional arm assertion is threaded through. A claim naming
    // the wrong sequence is refused by sequence, which is only reachable if
    // the argument was parsed — dropping it would produce the live-lease
    // refusal above instead.
    let mismatched = &responses[2]["result"];
    assert_eq!(mismatched["isError"], true, "{:?}", responses[2]);
    let text = mismatched["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains(&format!(
            "the armed switch is at sequence {armed_sequence}, not 999999"
        )),
        "claim's arm argument must reach the core: {text}"
    );

    // The arm survived all three calls and was claimed when claude exited, so
    // what the tool wrote was a real arm and not a projection over nothing.
    let log = cargo_bin_cmd!("handover")
        .current_dir(&repo)
        .env("HANDOVER_HOME", &state)
        .env("PATH", &path)
        .args(["log", "--json"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(log.contains("switch.armed"), "{log}");
    assert!(log.contains("switch.claimed"), "{log}");
}

/// A tool the server never advertised is a protocol error; a tool it did
/// advertise is not. Pin the advertised set so the two stay distinguishable.
#[test]
fn the_tool_list_advertises_the_three_reads_and_the_three_writes() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let state = temp.path().join("state");

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
    });
    let (_, responses) = send(&repo, &state, &format!("{request}\n"));
    let names: Vec<&str> = responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["list", "preview", "status", "arm", "claim", "attach"]
    );
}
