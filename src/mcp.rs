use std::io::{BufRead, Write};

use crate::error::{Result, io};
use crate::store::Environment;

pub fn mcp_server_command(environment: &Environment) -> Result<i32> {
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
        if let Some(response) = handle_message(&line, environment) {
            writeln!(stdout, "{response}").map_err(|source| io("stdout", source))?;
            stdout.flush().map_err(|source| io("stdout", source))?;
        }
    }
    Ok(0)
}

fn handle_message(line: &str, _environment: &Environment) -> Option<String> {
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
            "name": "sesh",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

fn tools_list_result() -> serde_json::Value {
    serde_json::json!({
        "tools": [
            {
                "name": "list",
                "description": "List all Sesh sessions across every repository, with last provider, last activity, and narrative-checkpoint freshness.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "handoff",
                "description": "Preview the exact handoff sesh switch would produce right now, without switching.",
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
        ],
    })
}
