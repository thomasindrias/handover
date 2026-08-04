use std::io::{BufRead, Write};

use crate::error::{Error, Result, io};
use crate::model::{Provider, Surface};
use crate::runtime::Runtime;
use crate::store::Environment;

pub fn mcp_server_command(environment: &Environment, runtime: &dyn Runtime) -> Result<i32> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                // Invalid UTF-8 is a bad message (recoverable).
                // Other I/O errors must propagate (not recoverable).
                if err.kind() == std::io::ErrorKind::InvalidData {
                    let response = error_response(serde_json::Value::Null, -32700, "Parse error");
                    writeln!(stdout, "{response}").map_err(|source| io("stdout", source))?;
                    stdout.flush().map_err(|source| io("stdout", source))?;
                    continue;
                } else {
                    return Err(io("stdin", err));
                }
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle_message(&line, environment, runtime) {
            writeln!(stdout, "{response}").map_err(|source| io("stdout", source))?;
            stdout.flush().map_err(|source| io("stdout", source))?;
        }
    }
    Ok(0)
}

fn handle_message(line: &str, environment: &Environment, runtime: &dyn Runtime) -> Option<String> {
    let request: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => {
            return Some(error_response(
                serde_json::Value::Null,
                -32700,
                "Parse error",
            ));
        }
    };
    // A JSON-RPC notification has no "id" member and gets no response at all.
    // `?` on the Option here returns None (no response) exactly for that case.
    let id = request.get("id").cloned()?;
    let method = request
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let outcome: std::result::Result<serde_json::Value, String> = match method {
        "initialize" => Ok(initialize_result(&request)),
        "tools/list" => Ok(tools_list_result()),
        "tools/call" => tools_call_result(&request, environment, runtime),
        "ping" => Ok(serde_json::json!({})),
        _ => Err(format!("unknown method: {method}")),
    };
    Some(match outcome {
        Ok(result) => success_response(id, result),
        Err(message) => error_response(id, -32601, &message),
    })
}

fn success_response(id: serde_json::Value, result: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
    .to_string()
}

fn error_response(id: serde_json::Value, code: i64, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

fn initialize_result(request: &serde_json::Value) -> serde_json::Value {
    let protocol_version = request
        .get("params")
        .and_then(|params| params.get("protocolVersion"))
        .cloned()
        .unwrap_or_else(|| serde_json::Value::String("2025-06-18".into()));
    serde_json::json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "handover",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

fn tools_list_result() -> serde_json::Value {
    // Built rather than hardcoded: this string is a contract advertised to
    // agents, and a changed default must not be able to ship a false one.
    let ttl_description = format!(
        "How long the arm stays claimable, e.g. `{default}`. Defaults to {default}.",
        default = crate::arm::DEFAULT_TTL
    );
    serde_json::json!({
        "tools": [
            {
                "name": "list",
                "description": "List all Handover sessions across every repository, with last provider, last activity, and narrative-checkpoint freshness.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "preview",
                "description": "Preview the exact handover `handover switch` would produce right now, without switching.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "provider": { "type": "string", "enum": ["claude", "codex"] } },
                    "required": ["provider"],
                },
            },
            {
                "name": "status",
                "description": "Report this session's current state and switch readiness, including the exact command to run to switch.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "arm",
                "description": "Record a pending switch to another provider without launching anything. The switch completes when this session's provider exits, and the target comes up in the same terminal holding the handover. Scoped to the run this process is attached to when it has one, and otherwise to the worktree it is called from — an attached session, such as a desktop application Handover opened, has no run.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "enum": ["claude", "codex"] },
                        "surface": { "type": "string", "enum": ["auto", "cli", "desktop"] },
                        "ttl": { "type": "string", "description": ttl_description },
                        "replace": { "type": "boolean", "description": "Supersede the pending arm instead of refusing, whether it names a different provider or the same one — refreshing an arm for the same provider with a new surface or TTL is the same operation. The superseded arm is retired in the journal as `switch.expired`. Defaults to false." },
                    },
                    "required": ["provider"],
                },
            },
            {
                "name": "claim",
                "description": "Consume this session's pending arm, commit the transition checkpoint, and return the handover. Refuses while a provider still holds the run lease. Scoped to the run this process is attached to when it has one, and otherwise to the worktree it is called from — an attached session, such as a desktop application Handover opened, has no run.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "arm": { "type": "integer", "description": "Assert which arm is being consumed, by the sequence of its switch.armed event." },
                    },
                },
            },
            {
                "name": "attach",
                "description": "Bind this worktree to a Handover session for a provider Handover did not launch. Resolves to the existing session when one exists rather than forking a second history beside it.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "provider": { "type": "string", "enum": ["claude", "codex"] } },
                    "required": ["provider"],
                },
            },
        ],
    })
}

fn tools_call_result(
    request: &serde_json::Value,
    environment: &Environment,
    runtime: &dyn Runtime,
) -> std::result::Result<serde_json::Value, String> {
    let params = request.get("params").cloned().unwrap_or_default();
    let name = params
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let outcome = match name {
        "list" => crate::app::mcp_list_value(environment),
        "status" => crate::app::build_status_value(environment, runtime),
        "preview" => handover_tool_value(environment, &arguments),
        "arm" => arm_tool_value(environment, runtime, &arguments),
        "claim" => claim_tool_value(environment, runtime, &arguments),
        "attach" => attach_tool_value(environment, runtime, &arguments),
        _ => return Err(format!("unknown tool: {name}")),
    };
    Ok(tool_call_content(outcome))
}

fn required_provider(arguments: &serde_json::Value) -> Result<Provider> {
    let provider_value = arguments
        .get("provider")
        .cloned()
        .ok_or_else(|| Error::InvalidState("missing required argument: provider".into()))?;
    serde_json::from_value(provider_value)
        .map_err(|error| Error::InvalidState(format!("invalid provider argument: {error}")))
}

fn handover_tool_value(
    environment: &Environment,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value> {
    crate::app::mcp_handover_value(required_provider(arguments)?, environment)
}

fn arm_tool_value(
    environment: &Environment,
    runtime: &dyn Runtime,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value> {
    let provider = required_provider(arguments)?;
    let surface = match arguments.get("surface") {
        None | Some(serde_json::Value::Null) => Surface::Auto,
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| Error::InvalidState(format!("invalid surface argument: {error}")))?,
    };
    let ttl = match arguments.get("ttl") {
        None | Some(serde_json::Value::Null) => crate::arm::DEFAULT_TTL.to_owned(),
        Some(value) => value
            .as_str()
            .ok_or_else(|| Error::InvalidState("ttl must be a string such as `15m`".into()))?
            .to_owned(),
    };
    let replace = match arguments.get("replace") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| Error::InvalidState("replace must be a boolean".into()))?,
    };
    crate::app::mcp_arm_value(provider, surface, &ttl, replace, environment, runtime)
}

fn claim_tool_value(
    environment: &Environment,
    runtime: &dyn Runtime,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value> {
    let arm = match arguments.get("arm") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or_else(|| {
            Error::InvalidState(
                "arm must be the sequence number of the armed switch, as an integer".into(),
            )
        })?),
    };
    crate::app::mcp_claim_value(arm, environment, runtime)
}

fn attach_tool_value(
    environment: &Environment,
    runtime: &dyn Runtime,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value> {
    crate::app::mcp_attach_value(required_provider(arguments)?, environment, runtime)
}

fn tool_call_content(outcome: Result<serde_json::Value>) -> serde_json::Value {
    match outcome {
        Ok(value) => serde_json::json!({
            "content": [{ "type": "text", "text": value.to_string() }],
            "isError": false,
        }),
        Err(error) => serde_json::json!({
            "content": [{ "type": "text", "text": error.to_string() }],
            "isError": true,
        }),
    }
}
