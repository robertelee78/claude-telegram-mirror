//! Hook pipeline integration tests.
//!
//! Tests JSON parsing of hook events and the generation of BridgeMessages
//! from those events. Since most hook.rs functions are private, we test
//! the public types (HookEvent, BridgeMessage) and their serialization.

use ctm::types::{BridgeMessage, HookEvent, MessageType};

#[test]
fn parse_pre_tool_use_event() {
    let json = r#"{
        "hook_event_name": "PreToolUse",
        "session_id": "sess-abc",
        "tool_name": "Bash",
        "tool_input": {"command": "ls -la"},
        "tool_use_id": "tu-123"
    }"#;

    let event: HookEvent = serde_json::from_str(json).expect("Should parse PreToolUse");
    match &event {
        HookEvent::PreToolUse(e) => {
            assert_eq!(e.base.session_id, "sess-abc");
            assert_eq!(e.tool_name, "Bash");
            assert_eq!(
                e.tool_input.get("command").and_then(|v| v.as_str()),
                Some("ls -la")
            );
            assert_eq!(e.tool_use_id.as_deref(), Some("tu-123"));
        }
        other => panic!("Expected PreToolUse, got: {:?}", other),
    }
}

#[test]
fn parse_post_tool_use_event() {
    let json = r#"{
        "hook_event_name": "PostToolUse",
        "session_id": "sess-def",
        "tool_name": "Write",
        "tool_input": {"file_path": "/tmp/test.txt", "content": "hello"},
        "tool_output": "File written successfully",
        "tool_error": null
    }"#;

    let event: HookEvent = serde_json::from_str(json).expect("Should parse PostToolUse");
    match &event {
        HookEvent::PostToolUse(e) => {
            assert_eq!(e.base.session_id, "sess-def");
            assert_eq!(e.tool_name, "Write");
            assert_eq!(e.tool_output.as_deref(), Some("File written successfully"));
            assert!(e.tool_error.is_none());
        }
        other => panic!("Expected PostToolUse, got: {:?}", other),
    }
}

#[test]
fn parse_stop_event() {
    let json = r#"{
        "hook_event_name": "Stop",
        "session_id": "sess-stop",
        "stop_hook_active": true,
        "transcript_summary": "Completed task successfully",
        "last_assistant_message": "Done!"
    }"#;

    let event: HookEvent = serde_json::from_str(json).expect("Should parse Stop");
    match &event {
        HookEvent::Stop(e) => {
            assert_eq!(e.base.session_id, "sess-stop");
            assert!(e.stop_hook_active);
            assert_eq!(
                e.transcript_summary.as_deref(),
                Some("Completed task successfully")
            );
            assert_eq!(e.last_assistant_message.as_deref(), Some("Done!"));
        }
        other => panic!("Expected Stop, got: {:?}", other),
    }
}

#[test]
fn parse_notification_event() {
    let json = r#"{
        "hook_event_name": "Notification",
        "session_id": "sess-notif",
        "message": "Something happened",
        "level": "error",
        "notification_type": "alert"
    }"#;

    let event: HookEvent = serde_json::from_str(json).expect("Should parse Notification");
    match &event {
        HookEvent::Notification(e) => {
            assert_eq!(e.base.session_id, "sess-notif");
            assert_eq!(e.message, "Something happened");
            assert_eq!(e.level.as_deref(), Some("error"));
            assert_eq!(e.notification_type.as_deref(), Some("alert"));
        }
        other => panic!("Expected Notification, got: {:?}", other),
    }
}

#[test]
fn parse_user_prompt_submit_event() {
    let json = r#"{
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-prompt",
        "prompt": "Fix the bug in main.rs"
    }"#;

    let event: HookEvent = serde_json::from_str(json).expect("Should parse UserPromptSubmit");
    match &event {
        HookEvent::UserPromptSubmit(e) => {
            assert_eq!(e.base.session_id, "sess-prompt");
            assert_eq!(e.prompt, "Fix the bug in main.rs");
        }
        other => panic!("Expected UserPromptSubmit, got: {:?}", other),
    }
}

#[test]
fn parse_pre_compact_event() {
    let json = r#"{
        "hook_event_name": "PreCompact",
        "session_id": "sess-compact"
    }"#;

    let event: HookEvent = serde_json::from_str(json).expect("Should parse PreCompact");
    match &event {
        HookEvent::PreCompact(e) => {
            assert_eq!(e.base.session_id, "sess-compact");
        }
        other => panic!("Expected PreCompact, got: {:?}", other),
    }
}

#[test]
fn bridge_message_type_field_serializes_correctly() {
    let msg = BridgeMessage {
        msg_type: MessageType::ToolStart,
        session_id: "s1".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        content: "Bash".to_string(),
        metadata: None,
    };

    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        json.get("type").and_then(|v| v.as_str()),
        Some("tool_start"),
        "msg_type should serialize as 'type' field with snake_case value"
    );

    // Round-trip
    let parsed: BridgeMessage = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.msg_type, MessageType::ToolStart);
    assert_eq!(parsed.session_id, "s1");
}

#[test]
fn bridge_message_with_metadata() {
    let mut meta = serde_json::Map::new();
    meta.insert("tool".into(), serde_json::Value::String("Bash".into()));
    meta.insert(
        "hostname".into(),
        serde_json::Value::String("builder-01".into()),
    );

    let msg = BridgeMessage {
        msg_type: MessageType::ToolResult,
        session_id: "s2".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        content: "command output here".to_string(),
        metadata: Some(meta),
    };

    let json_str = serde_json::to_string(&msg).unwrap();
    let parsed: BridgeMessage = serde_json::from_str(&json_str).unwrap();

    assert_eq!(parsed.msg_type, MessageType::ToolResult);
    let meta = parsed.metadata.unwrap();
    assert_eq!(meta.get("tool").and_then(|v| v.as_str()), Some("Bash"));
    assert_eq!(
        meta.get("hostname").and_then(|v| v.as_str()),
        Some("builder-01")
    );
}

#[test]
fn message_type_for_notification_error_is_error() {
    // Notification events with level=error should map to MessageType::Error
    // (this tests the type mapping logic conceptually)
    let json = serde_json::json!({
        "type": "error",
        "sessionId": "s1",
        "timestamp": "2024-01-01T00:00:00Z",
        "content": "Something failed"
    });

    let msg: BridgeMessage = serde_json::from_value(json).unwrap();
    assert_eq!(msg.msg_type, MessageType::Error);
}

#[test]
fn message_type_for_approval_request() {
    let json = serde_json::json!({
        "type": "approval_request",
        "sessionId": "s1",
        "timestamp": "2024-01-01T00:00:00Z",
        "content": "Allow Bash: rm -rf /tmp?"
    });

    let msg: BridgeMessage = serde_json::from_value(json).unwrap();
    assert_eq!(msg.msg_type, MessageType::ApprovalRequest);
}

#[test]
fn message_type_unknown_forward_compat() {
    let json = serde_json::json!({
        "type": "some_future_type",
        "sessionId": "s1",
        "timestamp": "2024-01-01T00:00:00Z",
        "content": ""
    });

    let msg: BridgeMessage = serde_json::from_value(json).unwrap();
    assert_eq!(
        msg.msg_type,
        MessageType::Unknown,
        "Unknown message types should deserialize as MessageType::Unknown"
    );
}

#[test]
fn hook_event_with_optional_fields_missing() {
    // PreToolUse with minimal fields (only required ones)
    let json = r#"{
        "hook_event_name": "PreToolUse",
        "session_id": "sess-minimal"
    }"#;

    let event: HookEvent = serde_json::from_str(json).expect("Should parse minimal PreToolUse");
    match &event {
        HookEvent::PreToolUse(e) => {
            assert_eq!(e.base.session_id, "sess-minimal");
            assert_eq!(e.tool_name, ""); // default
            assert!(e.tool_use_id.is_none());
        }
        other => panic!("Expected PreToolUse, got: {:?}", other),
    }
}

#[test]
fn hook_event_with_base_fields() {
    let json = r#"{
        "hook_event_name": "PreToolUse",
        "session_id": "sess-full-base",
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": "/home/user/project",
        "permission_mode": "default",
        "hook_id": "hook-abc-123",
        "tool_name": "Read"
    }"#;

    let event: HookEvent = serde_json::from_str(json).expect("Should parse with all base fields");
    match &event {
        HookEvent::PreToolUse(e) => {
            assert_eq!(e.base.session_id, "sess-full-base");
            assert_eq!(
                e.base.transcript_path.as_deref(),
                Some("/tmp/transcript.jsonl")
            );
            assert_eq!(e.base.cwd.as_deref(), Some("/home/user/project"));
            assert_eq!(e.base.permission_mode.as_deref(), Some("default"));
            assert_eq!(e.base.hook_id.as_deref(), Some("hook-abc-123"));
        }
        other => panic!("Expected PreToolUse, got: {:?}", other),
    }
}
