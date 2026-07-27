use handover::model::Provider;
use handover::provider::hook::{
    HookEvent, capture_failure_output, normalize, session_start_output, stale_narrative_output,
};

#[test]
fn normalizes_claude_prompt_without_persisting_transcript_path() {
    let normalized = normalize(
        Provider::Claude,
        include_bytes!("fixtures/hooks/claude-user-prompt.json"),
    )
    .unwrap();
    assert_eq!(normalized.cwd, std::path::Path::new("/work/oauth"));
    assert_eq!(normalized.event_name, "UserPromptSubmit");

    assert_eq!(
        normalized.event,
        HookEvent::UserPromptSubmitted {
            native_session_id: "claude-native-1".into(),
            prompt: "Implement the OAuth callback".into(),
        }
    );
}

#[test]
fn normalizes_claude_session_start() {
    let normalized = normalize(
        Provider::Claude,
        include_bytes!("fixtures/hooks/claude-session-start.json"),
    )
    .unwrap();

    assert_eq!(
        normalized.event,
        HookEvent::SessionStarted {
            native_session_id: "claude-native-1".into(),
        }
    );
}

#[test]
fn normalizes_claude_tool_result() {
    let normalized = normalize(
        Provider::Claude,
        include_bytes!("fixtures/hooks/claude-post-tool.json"),
    )
    .unwrap();

    assert!(matches!(
        normalized.event,
        HookEvent::ToolCompleted {
            tool_name,
            command: Some(command),
            exit_code: Some(101),
            ..
        } if tool_name == "Bash" && command == "cargo test oauth_callback"
    ));
}

#[test]
fn normalizes_codex_tool_request() {
    let normalized = normalize(
        Provider::Codex,
        include_bytes!("fixtures/hooks/codex-pre-tool.json"),
    )
    .unwrap();

    assert!(matches!(
        normalized.event,
        HookEvent::ToolRequested {
            command: Some(command),
            tool_use_id,
            ..
        } if command == "cargo test oauth_callback" && tool_use_id == "tool-2"
    ));
}

#[test]
fn normalizes_codex_tool_result_with_unknown_fields_allowed() {
    let mut value: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/hooks/codex-post-tool.json")).unwrap();
    value["future_field"] = serde_json::json!({"ignored": true});
    let bytes = serde_json::to_vec(&value).unwrap();

    let normalized = normalize(Provider::Codex, &bytes).unwrap();

    assert!(matches!(
        normalized.event,
        HookEvent::ToolCompleted {
            response: Some(response),
            exit_code: None,
            ..
        } if response.contains("Final output")
    ));
}

#[test]
fn missing_required_prompt_is_an_error() {
    let payload = br#"{"session_id":"native","cwd":"/work","hook_event_name":"UserPromptSubmit"}"#;
    assert!(normalize(Provider::Claude, payload).is_err());
}

#[test]
fn capture_failure_blocks_before_work_for_both_providers() {
    for provider in [Provider::Claude, Provider::Codex] {
        let pre_tool = capture_failure_output(provider, "PreToolUse", "disk full");
        assert_eq!(pre_tool.exit_code, 0);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pre_tool.stdout).unwrap(),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "Handover capture failed: disk full"
                }
            })
        );

        let prompt = capture_failure_output(provider, "UserPromptSubmit", "disk full");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&prompt.stdout).unwrap(),
            serde_json::json!({
                "decision": "block",
                "reason": "Handover capture failed: disk full"
            })
        );

        let post_tool = capture_failure_output(provider, "PostToolUse", "disk full");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&post_tool.stdout).unwrap(),
            serde_json::json!({
                "continue": false,
                "stopReason": "Handover capture failed: disk full"
            })
        );
    }
}

#[test]
fn normalizes_failure_and_stop_events() {
    let failure = serde_json::json!({
        "session_id": "native",
        "cwd": "/work",
        "hook_event_name": "PostToolUseFailure",
        "tool_name": "Bash",
        "tool_use_id": "tool-3",
        "error": "permission denied"
    });
    assert_eq!(
        normalize(Provider::Claude, &serde_json::to_vec(&failure).unwrap())
            .unwrap()
            .event,
        HookEvent::ToolFailed {
            native_session_id: "native".into(),
            tool_name: "Bash".into(),
            tool_use_id: "tool-3".into(),
            error: "permission denied".into(),
        }
    );

    let stopped = br#"{"session_id":"native","cwd":"/work","hook_event_name":"Stop"}"#;
    assert_eq!(
        normalize(Provider::Codex, stopped).unwrap().event,
        HookEvent::Stopped {
            native_session_id: "native".into()
        }
    );
}

#[test]
fn rejects_empty_and_oversized_identifiers() {
    let empty_session = br#"{"session_id":"","cwd":"/work","hook_event_name":"Stop"}"#;
    assert!(normalize(Provider::Claude, empty_session).is_err());

    let empty_tool_id = pre_tool_payload("Bash", "", "true");
    assert!(normalize(Provider::Codex, &empty_tool_id).is_err());

    let maximum_session = serde_json::json!({
        "session_id": "s".repeat(512),
        "cwd": "/work",
        "hook_event_name": "Stop"
    });
    assert!(
        normalize(
            Provider::Claude,
            &serde_json::to_vec(&maximum_session).unwrap()
        )
        .is_ok()
    );

    let oversized_session = serde_json::json!({
        "session_id": "s".repeat(513),
        "cwd": "/work",
        "hook_event_name": "Stop"
    });
    assert!(
        normalize(
            Provider::Claude,
            &serde_json::to_vec(&oversized_session).unwrap()
        )
        .is_err()
    );

    let oversized_tool_id = pre_tool_payload("Bash", &"t".repeat(513), "true");
    assert!(normalize(Provider::Codex, &oversized_tool_id).is_err());
}

#[test]
fn enforces_name_cwd_and_tool_input_byte_limits() {
    let maximum_name = pre_tool_payload(&"n".repeat(256), "tool", "true");
    assert!(normalize(Provider::Claude, &maximum_name).is_ok());

    let oversized_name = pre_tool_payload(&"n".repeat(257), "tool", "true");
    assert!(normalize(Provider::Claude, &oversized_name).is_err());

    let maximum_cwd = serde_json::json!({
        "session_id": "native",
        "cwd": "c".repeat(16 * 1024),
        "hook_event_name": "Stop"
    });
    assert!(normalize(Provider::Claude, &serde_json::to_vec(&maximum_cwd).unwrap()).is_ok());

    let oversized_cwd = serde_json::json!({
        "session_id": "native",
        "cwd": "c".repeat(16 * 1024 + 1),
        "hook_event_name": "Stop"
    });
    assert!(
        normalize(
            Provider::Claude,
            &serde_json::to_vec(&oversized_cwd).unwrap()
        )
        .is_err()
    );

    let maximum_command = pre_tool_payload("Bash", "tool", &"c".repeat(1024 * 1024));
    assert!(normalize(Provider::Codex, &maximum_command).is_ok());

    let oversized_command = pre_tool_payload("Bash", "tool", &"c".repeat(1024 * 1024 + 1));
    assert!(normalize(Provider::Codex, &oversized_command).is_err());
}

#[test]
fn enforces_prompt_and_response_byte_limits() {
    let maximum_prompt = serde_json::json!({
        "session_id": "native",
        "cwd": "/work",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "p".repeat(4 * 1024 * 1024)
    });
    assert!(
        normalize(
            Provider::Claude,
            &serde_json::to_vec(&maximum_prompt).unwrap()
        )
        .is_ok()
    );

    let oversized_prompt = serde_json::json!({
        "session_id": "native",
        "cwd": "/work",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "p".repeat(4 * 1024 * 1024 + 1)
    });
    assert!(
        normalize(
            Provider::Claude,
            &serde_json::to_vec(&oversized_prompt).unwrap()
        )
        .is_err()
    );

    let maximum_response = post_tool_payload(serde_json::json!("r".repeat(4 * 1024 * 1024)));
    assert!(normalize(Provider::Codex, &maximum_response).is_ok());

    let oversized_response = post_tool_payload(serde_json::json!("r".repeat(4 * 1024 * 1024 + 1)));
    assert!(normalize(Provider::Codex, &oversized_response).is_err());
}

#[test]
fn rejects_payloads_over_the_outer_input_limit() {
    let oversized = vec![b' '; 8 * 1024 * 1024 + 1];
    let error = normalize(Provider::Claude, &oversized)
        .unwrap_err()
        .to_string();
    assert!(error.contains("input limit"));
}

#[test]
fn rejects_wrong_types_for_known_fields() {
    let cases = [
        serde_json::json!({
            "session_id": 7,
            "cwd": "/work",
            "hook_event_name": "Stop"
        }),
        serde_json::json!({
            "session_id": "native",
            "cwd": "/work",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_use_id": "tool",
            "tool_input": "cargo test"
        }),
        serde_json::json!({
            "session_id": "native",
            "cwd": "/work",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_use_id": "tool",
            "tool_input": {"command": false}
        }),
        post_tool_value(serde_json::json!({"stdout": ["not", "text"]})),
        post_tool_value(serde_json::json!({"exit_code": "zero"})),
        post_tool_value(serde_json::json!({"duration_ms": -1})),
    ];

    for value in cases {
        assert!(
            normalize(Provider::Codex, &serde_json::to_vec(&value).unwrap()).is_err(),
            "accepted wrong type in {value}"
        );
    }
}

#[test]
fn session_start_output_is_exact_and_provider_safe() {
    let output = session_start_output("Objective: fix OAuth\nNext: run the integration test");
    assert_eq!(output.exit_code, 0);
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": "Objective: fix OAuth\nNext: run the integration test"
            }
        })
    );
}

#[test]
fn stale_narrative_output_is_exactly_one_system_message() {
    let stale = stale_narrative_output(27, true);
    assert_eq!(stale.exit_code, 0);
    assert!(stale.stderr.is_empty());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stale.stdout).unwrap(),
        serde_json::json!({
            "systemMessage": "Handover: 27 events since the last narrative checkpoint. \
             Ask the agent to checkpoint, or run `handover checkpoint`."
        })
    );

    let never = stale_narrative_output(27, false);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&never.stdout).unwrap(),
        serde_json::json!({
            "systemMessage": "Handover: 27 events and no narrative checkpoint yet. \
             Ask the agent to checkpoint, or run `handover checkpoint`."
        })
    );

    for output in [&stale, &never] {
        let value: serde_json::Value = serde_json::from_str(&output.stdout).unwrap();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["systemMessage"]);
    }
}

#[test]
fn capture_failure_output_is_bounded_on_a_utf8_boundary() {
    let output = capture_failure_output(Provider::Claude, "PreToolUse", &"å".repeat(10_000));
    assert!(output.stdout.len() < 5_000);
    assert!(serde_json::from_str::<serde_json::Value>(&output.stdout).is_ok());
}

fn pre_tool_payload(tool_name: &str, tool_use_id: &str, command: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "session_id": "native",
        "cwd": "/work",
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_use_id": tool_use_id,
        "tool_input": {"command": command}
    }))
    .unwrap()
}

fn post_tool_value(response: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "session_id": "native",
        "cwd": "/work",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_use_id": "tool",
        "tool_response": response
    })
}

fn post_tool_payload(response: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&post_tool_value(response)).unwrap()
}
