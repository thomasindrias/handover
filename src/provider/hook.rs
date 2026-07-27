use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::model::Provider;

const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_NAME_BYTES: usize = 256;
const MAX_ID_BYTES: usize = 512;
const MAX_CWD_BYTES: usize = 16 * 1024;
const MAX_TOOL_INPUT_BYTES: usize = 1024 * 1024;
const MAX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_FAILURE_MESSAGE_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HookEvent {
    SessionStarted {
        native_session_id: String,
    },
    UserPromptSubmitted {
        native_session_id: String,
        prompt: String,
    },
    ToolRequested {
        native_session_id: String,
        tool_name: String,
        tool_use_id: String,
        command: Option<String>,
        file_path: Option<String>,
    },
    ToolCompleted {
        native_session_id: String,
        tool_name: String,
        tool_use_id: String,
        command: Option<String>,
        file_path: Option<String>,
        response: Option<String>,
        stdout: Option<String>,
        stderr: Option<String>,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
    ToolFailed {
        native_session_id: String,
        tool_name: String,
        tool_use_id: String,
        error: String,
    },
    Stopped {
        native_session_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedHook {
    pub cwd: PathBuf,
    pub event_name: String,
    pub event: HookEvent,
}

pub fn normalize(provider: Provider, bytes: &[u8]) -> Result<NormalizedHook> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(invalid(
            provider,
            format!("hook payload exceeds the {MAX_PAYLOAD_BYTES}-byte input limit"),
        ));
    }

    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| invalid(provider, format!("invalid hook JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid(provider, "hook payload must be a JSON object"))?;

    let event_name = required_string(provider, object, "hook_event_name", MAX_NAME_BYTES, true)?;
    let native_session_id = required_string(provider, object, "session_id", MAX_ID_BYTES, true)?;
    let cwd = PathBuf::from(required_string(
        provider,
        object,
        "cwd",
        MAX_CWD_BYTES,
        true,
    )?);

    let event = match event_name.as_str() {
        "SessionStart" => Ok(HookEvent::SessionStarted { native_session_id }),
        "UserPromptSubmit" => Ok(HookEvent::UserPromptSubmitted {
            native_session_id,
            prompt: required_string(provider, object, "prompt", MAX_CONTENT_BYTES, false)?,
        }),
        "PreToolUse" => Ok(HookEvent::ToolRequested {
            native_session_id,
            tool_name: required_string(provider, object, "tool_name", MAX_NAME_BYTES, true)?,
            tool_use_id: required_string(provider, object, "tool_use_id", MAX_ID_BYTES, true)?,
            command: optional_nested_string(
                provider,
                object,
                "tool_input",
                "command",
                MAX_TOOL_INPUT_BYTES,
            )?,
            file_path: optional_nested_string(
                provider,
                object,
                "tool_input",
                "file_path",
                MAX_TOOL_INPUT_BYTES,
            )?,
        }),
        "PostToolUse" => {
            let response = parse_tool_response(provider, object)?;
            Ok(HookEvent::ToolCompleted {
                native_session_id,
                tool_name: required_string(provider, object, "tool_name", MAX_NAME_BYTES, true)?,
                tool_use_id: required_string(provider, object, "tool_use_id", MAX_ID_BYTES, true)?,
                command: optional_nested_string(
                    provider,
                    object,
                    "tool_input",
                    "command",
                    MAX_TOOL_INPUT_BYTES,
                )?,
                file_path: optional_nested_string(
                    provider,
                    object,
                    "tool_input",
                    "file_path",
                    MAX_TOOL_INPUT_BYTES,
                )?,
                response: response.opaque,
                stdout: response.stdout,
                stderr: response.stderr,
                exit_code: response.exit_code,
                duration_ms: response.duration_ms,
            })
        }
        "PostToolUseFailure" => Ok(HookEvent::ToolFailed {
            native_session_id,
            tool_name: required_string(provider, object, "tool_name", MAX_NAME_BYTES, true)?,
            tool_use_id: required_string(provider, object, "tool_use_id", MAX_ID_BYTES, true)?,
            error: required_string(provider, object, "error", MAX_CONTENT_BYTES, false)?,
        }),
        "Stop" => Ok(HookEvent::Stopped { native_session_id }),
        other => Err(invalid(
            provider,
            format!("unsupported hook event {other:?}"),
        )),
    }?;
    Ok(NormalizedHook {
        cwd,
        event_name,
        event,
    })
}

pub fn session_start_output(handover: &str) -> HookOutput {
    HookOutput {
        stdout: serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": handover
            }
        })
        .to_string(),
        stderr: String::new(),
        exit_code: 0,
    }
}

pub fn stale_narrative_output(events_since: u64, has_narrative_checkpoint: bool) -> HookOutput {
    let message = if has_narrative_checkpoint {
        format!(
            "Handover: {events_since} events since the last narrative checkpoint. \
             Ask the agent to checkpoint, or run `handover checkpoint`."
        )
    } else {
        format!(
            "Handover: {events_since} events and no narrative checkpoint yet. \
             Ask the agent to checkpoint, or run `handover checkpoint`."
        )
    };
    HookOutput {
        stdout: serde_json::json!({ "systemMessage": message }).to_string(),
        stderr: String::new(),
        exit_code: 0,
    }
}

pub fn capture_failure_output(_provider: Provider, event: &str, message: &str) -> HookOutput {
    let reason = format!(
        "Handover capture failed: {}",
        truncate_utf8(message, MAX_FAILURE_MESSAGE_BYTES)
    );
    let stdout = match event {
        "PreToolUse" => serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        }),
        "UserPromptSubmit" => serde_json::json!({
            "decision": "block",
            "reason": reason
        }),
        "PostToolUse" | "PostToolUseFailure" | "Stop" => serde_json::json!({
            "continue": false,
            "stopReason": reason
        }),
        _ => serde_json::json!({
            "systemMessage": reason
        }),
    };

    HookOutput {
        stdout: stdout.to_string(),
        stderr: String::new(),
        exit_code: 0,
    }
}

#[derive(Default)]
struct ParsedToolResponse {
    opaque: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
}

fn parse_tool_response(
    provider: Provider,
    object: &Map<String, Value>,
) -> Result<ParsedToolResponse> {
    let Some(value) = object.get("tool_response") else {
        return Ok(ParsedToolResponse::default());
    };
    if value.is_null() {
        return Ok(ParsedToolResponse::default());
    }

    let Some(response) = value.as_object() else {
        return Ok(ParsedToolResponse {
            opaque: Some(serialize_bounded_response(provider, value)?),
            ..ParsedToolResponse::default()
        });
    };

    let has_structured_fields = ["stdout", "stderr", "exit_code", "duration_ms"]
        .iter()
        .any(|field| response.contains_key(*field));
    if !has_structured_fields {
        return Ok(ParsedToolResponse {
            opaque: Some(serialize_bounded_response(provider, value)?),
            ..ParsedToolResponse::default()
        });
    }

    Ok(ParsedToolResponse {
        opaque: None,
        stdout: optional_string(
            provider,
            response,
            "tool_response.stdout",
            "stdout",
            MAX_CONTENT_BYTES,
        )?,
        stderr: optional_string(
            provider,
            response,
            "tool_response.stderr",
            "stderr",
            MAX_CONTENT_BYTES,
        )?,
        exit_code: optional_i32(provider, response, "exit_code")?,
        duration_ms: optional_u64(provider, response, "duration_ms")?,
    })
}

fn required_string(
    provider: Provider,
    object: &Map<String, Value>,
    field: &str,
    max_bytes: usize,
    reject_empty: bool,
) -> Result<String> {
    let value = object
        .get(field)
        .ok_or_else(|| invalid(provider, format!("hook payload is missing field {field}")))?;
    let value = value
        .as_str()
        .ok_or_else(|| invalid(provider, format!("hook field {field} must be a string")))?;
    validate_string(provider, field, value, max_bytes, reject_empty)?;
    Ok(value.to_owned())
}

fn optional_nested_string(
    provider: Provider,
    object: &Map<String, Value>,
    parent: &str,
    field: &str,
    max_bytes: usize,
) -> Result<Option<String>> {
    let Some(parent_value) = object.get(parent) else {
        return Ok(None);
    };
    if parent_value.is_null() {
        return Ok(None);
    }
    let parent_object = parent_value
        .as_object()
        .ok_or_else(|| invalid(provider, format!("hook field {parent} must be an object")))?;
    optional_string(
        provider,
        parent_object,
        &format!("{parent}.{field}"),
        field,
        max_bytes,
    )
}

fn optional_string(
    provider: Provider,
    object: &Map<String, Value>,
    display_name: &str,
    field: &str,
    max_bytes: usize,
) -> Result<Option<String>> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value.as_str().ok_or_else(|| {
        invalid(
            provider,
            format!("hook field {display_name} must be a string"),
        )
    })?;
    validate_string(provider, display_name, value, max_bytes, false)?;
    Ok(Some(value.to_owned()))
}

fn optional_i32(
    provider: Provider,
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<i32>> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value.as_i64().ok_or_else(|| {
        invalid(
            provider,
            format!("hook field tool_response.{field} must be an integer"),
        )
    })?;
    i32::try_from(value).map(Some).map_err(|_| {
        invalid(
            provider,
            format!("hook field tool_response.{field} is outside the i32 range"),
        )
    })
}

fn optional_u64(
    provider: Provider,
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<u64>> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value.as_u64().map(Some).ok_or_else(|| {
        invalid(
            provider,
            format!("hook field tool_response.{field} must be a non-negative integer"),
        )
    })
}

fn serialize_bounded_response(provider: Provider, value: &Value) -> Result<String> {
    let serialized = if let Some(text) = value.as_str() {
        text.to_owned()
    } else {
        serde_json::to_string(value).map_err(|error| {
            invalid(provider, format!("cannot serialize tool response: {error}"))
        })?
    };
    validate_string(
        provider,
        "tool_response",
        &serialized,
        MAX_CONTENT_BYTES,
        false,
    )?;
    Ok(serialized)
}

fn validate_string(
    provider: Provider,
    field: &str,
    value: &str,
    max_bytes: usize,
    reject_empty: bool,
) -> Result<()> {
    if reject_empty && value.is_empty() {
        return Err(invalid(
            provider,
            format!("hook field {field} must not be empty"),
        ));
    }
    if value.len() > max_bytes {
        return Err(invalid(
            provider,
            format!("hook field {field} exceeds the {max_bytes}-byte limit"),
        ));
    }
    Ok(())
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn invalid(provider: Provider, message: impl Into<String>) -> Error {
    Error::InvalidState(format!(
        "invalid {} hook payload: {}",
        provider.executable(),
        message.into()
    ))
}
