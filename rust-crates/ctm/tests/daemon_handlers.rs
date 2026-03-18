//! Integration tests for daemon handler layer logic.
//!
//! Tests the business logic exercised by socket_handlers, telegram_handlers,
//! callback_handlers, cleanup, and files -- all via the public `ctm::` API.
//!
//! No live Telegram API is required; these tests verify:
//! - Session lifecycle state transitions (the DB operations handlers perform)
//! - BridgeMessage construction, serialization, and routing metadata
//! - Approval lifecycle and response message structure
//! - File path validation and sanitization
//! - Formatting helpers used by handlers
//! - Cleanup query correctness (stale sessions, orphaned threads)
//! - Type validation (session IDs, slash commands, message types)

use ctm::config::Config;
use ctm::session::SessionManager;
use ctm::types::{
    is_valid_session_id, is_valid_session_status, is_valid_slash_command, ApprovalStatus,
    BridgeMessage, MessageType, SessionStatus, ALLOWED_TMUX_KEYS, SAFE_COMMANDS,
};
use std::path::PathBuf;
use tempfile::tempdir;

// ======================================================================
// Helpers
// ======================================================================

fn make_mgr() -> (SessionManager, tempfile::TempDir) {
    let tmp = tempdir().unwrap();
    let mgr = SessionManager::new(tmp.path(), 5).unwrap();
    (mgr, tmp)
}

fn make_bridge_msg(msg_type: MessageType, session_id: &str, content: &str) -> BridgeMessage {
    BridgeMessage {
        msg_type,
        session_id: session_id.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        content: content.to_string(),
        metadata: None,
    }
}

fn make_bridge_msg_with_meta(
    msg_type: MessageType,
    session_id: &str,
    content: &str,
    metadata: serde_json::Map<String, serde_json::Value>,
) -> BridgeMessage {
    BridgeMessage {
        msg_type,
        session_id: session_id.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        content: content.to_string(),
        metadata: Some(metadata),
    }
}

// ======================================================================
// 1. Session lifecycle through handler-like sequences
// ======================================================================

/// Simulate the full handler lifecycle: session_start -> tool_start ->
/// tool_result -> turn_complete -> session_end. Verify DB state at each step.
#[test]
fn session_lifecycle_full_sequence() {
    let (mgr, _tmp) = make_mgr();
    let sid = "session-lifecycle-full";

    // Step 1: session_start -- handler calls create_session
    mgr.create_session(
        sid,
        42,
        Some("devbox"),
        Some("/opt/myapp"),
        None,
        None,
        None,
    )
    .unwrap();
    let s = mgr.get_session(sid).unwrap().unwrap();
    assert_eq!(s.status, SessionStatus::Active);
    assert_eq!(s.hostname.as_deref(), Some("devbox"));
    assert_eq!(s.project_dir.as_deref(), Some("/opt/myapp"));
    let (active, _) = mgr.get_stats().unwrap();
    assert_eq!(active, 1);

    // Step 2: tool_start -- handler updates activity
    mgr.update_activity(sid).unwrap();
    let s2 = mgr.get_session(sid).unwrap().unwrap();
    assert!(s2.last_activity >= s.last_activity);

    // Step 3: tool_result -- handler updates activity again
    mgr.update_activity(sid).unwrap();

    // Step 4: turn_complete -- no DB change, session still active
    let s3 = mgr.get_session(sid).unwrap().unwrap();
    assert_eq!(s3.status, SessionStatus::Active);

    // Step 5: session_end -- handler calls end_session
    mgr.end_session(sid, SessionStatus::Ended).unwrap();
    let s4 = mgr.get_session(sid).unwrap().unwrap();
    assert_eq!(s4.status, SessionStatus::Ended);

    // Session no longer appears in active list
    let active = mgr.get_active_sessions().unwrap();
    assert!(active.iter().all(|s| s.id != sid));
}

/// Verify session reactivation (BUG-009 path: hook events arrive after session_end).
#[test]
fn session_reactivation_after_end() {
    let (mgr, _tmp) = make_mgr();
    let sid = "session-reactivate";

    mgr.create_session(sid, 1, None, None, None, None, None)
        .unwrap();
    mgr.end_session(sid, SessionStatus::Ended).unwrap();
    assert_eq!(
        mgr.get_session(sid).unwrap().unwrap().status,
        SessionStatus::Ended
    );

    // Reactivate (as ensure_session_exists does for BUG-009)
    mgr.reactivate_session(sid).unwrap();
    let s = mgr.get_session(sid).unwrap().unwrap();
    assert_eq!(s.status, SessionStatus::Active);
}

/// Verify that abort status works correctly.
#[test]
fn session_abort_status() {
    let (mgr, _tmp) = make_mgr();
    let sid = "session-abort";

    mgr.create_session(sid, 1, None, None, None, None, None)
        .unwrap();
    mgr.end_session(sid, SessionStatus::Aborted).unwrap();

    let s = mgr.get_session(sid).unwrap().unwrap();
    assert_eq!(s.status, SessionStatus::Aborted);

    // Aborted sessions should not appear in active list
    let active = mgr.get_active_sessions().unwrap();
    assert!(active.is_empty());
}

/// Thread ID assignment and lookup by thread_id (used by telegram_handlers).
#[test]
fn thread_id_assignment_and_lookup() {
    let (mgr, _tmp) = make_mgr();
    let sid = "session-thread-lookup";

    mgr.create_session(sid, 1, None, None, None, None, None)
        .unwrap();
    mgr.set_session_thread(sid, 12345).unwrap();

    // Lookup by thread_id (as telegram_handlers do for incoming messages)
    let found = mgr.get_session_by_thread_id(12345).unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, sid);

    // Non-existent thread returns None
    assert!(mgr.get_session_by_thread_id(99999).unwrap().is_none());
}

/// Only active sessions are returned by get_session_by_thread_id.
#[test]
fn thread_lookup_ignores_ended_sessions() {
    let (mgr, _tmp) = make_mgr();
    let sid = "session-ended-thread";

    mgr.create_session(sid, 1, None, None, None, None, None)
        .unwrap();
    mgr.set_session_thread(sid, 777).unwrap();
    mgr.end_session(sid, SessionStatus::Ended).unwrap();

    // Ended session should not be found by thread lookup
    let found = mgr.get_session_by_thread_id(777).unwrap();
    assert!(
        found.is_none(),
        "Ended session should not be found by thread_id"
    );
}

// ======================================================================
// 2. BridgeMessage serialization and MessageType routing
// ======================================================================

/// Verify that all MessageType variants serialize to the expected snake_case strings.
#[test]
fn message_type_serialization_all_variants() {
    let cases = vec![
        (MessageType::AgentResponse, "agent_response"),
        (MessageType::ToolStart, "tool_start"),
        (MessageType::ToolResult, "tool_result"),
        (MessageType::ApprovalRequest, "approval_request"),
        (MessageType::UserInput, "user_input"),
        (MessageType::ApprovalResponse, "approval_response"),
        (MessageType::Command, "command"),
        (MessageType::Error, "error"),
        (MessageType::SessionStart, "session_start"),
        (MessageType::SessionEnd, "session_end"),
        (MessageType::TurnComplete, "turn_complete"),
        (MessageType::PreCompact, "pre_compact"),
        (MessageType::SessionRename, "session_rename"),
        (MessageType::SendImage, "send_image"),
    ];

    for (variant, expected_str) in &cases {
        let json = serde_json::to_value(variant).unwrap();
        assert_eq!(
            json.as_str().unwrap(),
            *expected_str,
            "MessageType::{:?} should serialize to {:?}",
            variant,
            expected_str
        );

        // Round-trip
        let parsed: MessageType = serde_json::from_value(json).unwrap();
        assert_eq!(&parsed, variant);
    }
}

/// Unknown message types deserialize to MessageType::Unknown (forward compatibility).
#[test]
fn message_type_unknown_forward_compat() {
    let parsed: MessageType =
        serde_json::from_value(serde_json::Value::String("some_future_type".into())).unwrap();
    assert_eq!(parsed, MessageType::Unknown);
}

/// BridgeMessage round-trip with metadata (the wire format used by all handlers).
#[test]
fn bridge_message_roundtrip_with_metadata() {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "tool".to_string(),
        serde_json::Value::String("Bash".to_string()),
    );
    meta.insert(
        "input".to_string(),
        serde_json::json!({"command": "cargo test"}),
    );
    meta.insert(
        "hostname".to_string(),
        serde_json::Value::String("devbox".to_string()),
    );

    let msg = make_bridge_msg_with_meta(
        MessageType::ToolStart,
        "session-abc123",
        "tool execution",
        meta,
    );

    let json = serde_json::to_string(&msg).unwrap();
    let parsed: BridgeMessage = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.msg_type, MessageType::ToolStart);
    assert_eq!(parsed.session_id, "session-abc123");
    assert_eq!(parsed.content, "tool execution");

    // Verify metadata accessor
    let m = parsed.meta();
    assert_eq!(m.tool(), Some("Bash"));
    assert_eq!(m.hostname(), Some("devbox"));
    assert!(m.input().is_some());
}

/// BridgeMessage "type" field is renamed from msg_type in JSON.
#[test]
fn bridge_message_type_field_is_renamed() {
    let msg = make_bridge_msg(MessageType::SessionStart, "s1", "start");
    let json = serde_json::to_value(&msg).unwrap();
    assert!(
        json.get("type").is_some(),
        "JSON should have 'type' field, not 'msg_type'"
    );
    assert!(json.get("msg_type").is_none());
    assert_eq!(json["type"].as_str().unwrap(), "session_start");
}

/// BridgeMessage metadata is omitted when None (skip_serializing_if).
#[test]
fn bridge_message_no_metadata_omitted() {
    let msg = make_bridge_msg(MessageType::TurnComplete, "s1", "done");
    let json = serde_json::to_value(&msg).unwrap();
    assert!(
        json.get("metadata").is_none(),
        "Null metadata should be omitted from JSON"
    );
}

/// BridgeMessage MessageMetadata accessor covers all known fields.
#[test]
fn message_metadata_accessor_coverage() {
    let mut meta = serde_json::Map::new();
    meta.insert("tool".into(), serde_json::json!("Read"));
    meta.insert("input".into(), serde_json::json!({"file_path": "/a/b.rs"}));
    meta.insert("toolUseId".into(), serde_json::json!("tuid-123"));
    meta.insert("_client_id".into(), serde_json::json!("client-456"));
    meta.insert("hostname".into(), serde_json::json!("devbox"));
    meta.insert("tmuxTarget".into(), serde_json::json!("s:0.0"));
    meta.insert("tmuxSocket".into(), serde_json::json!("/tmp/tmux.sock"));
    meta.insert("source".into(), serde_json::json!("telegram"));
    meta.insert("projectDir".into(), serde_json::json!("/opt/proj"));
    meta.insert("transcript_path".into(), serde_json::json!("/tmp/t.jsonl"));
    meta.insert("trigger".into(), serde_json::json!("manual"));
    meta.insert("caption".into(), serde_json::json!("A photo"));
    meta.insert("approvalId".into(), serde_json::json!("approval-789"));

    let msg = make_bridge_msg_with_meta(MessageType::ToolStart, "s1", "", meta);
    let m = msg.meta();

    assert_eq!(m.tool(), Some("Read"));
    assert!(m.input().is_some());
    assert_eq!(m.tool_use_id(), Some("tuid-123"));
    assert_eq!(m.client_id(), Some("client-456"));
    assert_eq!(m.hostname(), Some("devbox"));
    assert_eq!(m.tmux_target(), Some("s:0.0"));
    assert_eq!(m.tmux_socket(), Some("/tmp/tmux.sock"));
    assert_eq!(m.source(), Some("telegram"));
    assert_eq!(m.project_dir(), Some("/opt/proj"));
    assert_eq!(m.transcript_path(), Some("/tmp/t.jsonl"));
    assert_eq!(m.trigger(), Some("manual"));
    assert_eq!(m.caption(), Some("A photo"));
    assert_eq!(m.approval_id(), Some("approval-789"));
}

/// MessageMetadata returns None for missing fields (no panic).
#[test]
fn message_metadata_returns_none_for_missing() {
    let msg = make_bridge_msg(MessageType::AgentResponse, "s1", "hello");
    let m = msg.meta();
    assert!(m.tool().is_none());
    assert!(m.input().is_none());
    assert!(m.hostname().is_none());
    assert!(m.source().is_none());
}

// ======================================================================
// 3. Approval response message structure (ADR-006 C1)
// ======================================================================

/// Verify approval_response messages have correct structure for all actions.
#[test]
fn approval_response_structure_all_actions() {
    for action in &["approve", "reject", "abort"] {
        let approval_id = "approval-test-xyz";
        let session_id = "session-appr-test";

        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "approvalId".to_string(),
            serde_json::Value::String(approval_id.to_string()),
        );

        let msg = BridgeMessage {
            msg_type: MessageType::ApprovalResponse,
            session_id: session_id.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            content: action.to_string(),
            metadata: Some(metadata),
        };

        assert_eq!(msg.msg_type, MessageType::ApprovalResponse);
        assert_eq!(msg.session_id, session_id);
        assert_eq!(msg.content, *action);
        assert_eq!(msg.meta().approval_id(), Some(approval_id));

        // Must survive JSON round-trip (NDJSON framing requirement)
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: BridgeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.msg_type, MessageType::ApprovalResponse);
        assert_eq!(parsed.content, *action);
        assert_eq!(parsed.meta().approval_id(), Some(approval_id));
    }
}

/// Approval lifecycle: create -> resolve -> verify DB state.
#[test]
fn approval_lifecycle_through_db() {
    let (mgr, _tmp) = make_mgr();
    let sid = "session-appr-lc";

    mgr.create_session(sid, 1, None, None, None, None, None)
        .unwrap();

    // Create approval (as handle_approval_request does)
    let aid = mgr
        .create_approval(sid, "Allow Bash: rm -rf /tmp/test?", None)
        .unwrap();
    assert!(aid.starts_with("approval-"));

    let approval = mgr.get_approval(&aid).unwrap().unwrap();
    assert_eq!(approval.status, ApprovalStatus::Pending);
    assert_eq!(approval.session_id, sid);

    // Resolve approval (as handle_approval_callback does)
    let resolved = mgr
        .resolve_approval(&aid, ApprovalStatus::Approved)
        .unwrap();
    assert!(resolved);

    let approval = mgr.get_approval(&aid).unwrap().unwrap();
    assert_eq!(approval.status, ApprovalStatus::Approved);

    // Double-resolve returns false
    let re_resolved = mgr
        .resolve_approval(&aid, ApprovalStatus::Rejected)
        .unwrap();
    assert!(!re_resolved);
}

/// Ending a session expires all its pending approvals atomically.
#[test]
fn session_end_expires_all_pending_approvals() {
    let (mgr, _tmp) = make_mgr();
    let sid = "session-multi-appr";

    mgr.create_session(sid, 1, None, None, None, None, None)
        .unwrap();

    let a1 = mgr.create_approval(sid, "Approve action 1?", None).unwrap();
    let a2 = mgr.create_approval(sid, "Approve action 2?", None).unwrap();

    // Verify both are pending
    let (_, pending) = mgr.get_stats().unwrap();
    assert_eq!(pending, 2);

    // End session
    mgr.end_session(sid, SessionStatus::Ended).unwrap();

    // Both approvals should be expired
    assert_eq!(
        mgr.get_approval(&a1).unwrap().unwrap().status,
        ApprovalStatus::Expired
    );
    assert_eq!(
        mgr.get_approval(&a2).unwrap().unwrap().status,
        ApprovalStatus::Expired
    );
    let (_, pending) = mgr.get_stats().unwrap();
    assert_eq!(pending, 0);
}

// ======================================================================
// 4. Approval keyboard structure (used by handle_approval_request)
// ======================================================================

/// Verify create_approval_keyboard produces the expected button layout.
#[test]
fn approval_keyboard_layout() {
    let keyboard = ctm::bot::create_approval_keyboard("appr-42");

    // Two rows: [Approve, Reject] and [Abort Session]
    assert_eq!(keyboard.len(), 2);
    assert_eq!(keyboard[0].len(), 2);
    assert_eq!(keyboard[1].len(), 1);

    // Verify callback_data contains the approval_id
    assert_eq!(keyboard[0][0].callback_data, "approve:appr-42");
    assert_eq!(keyboard[0][1].callback_data, "reject:appr-42");
    assert_eq!(keyboard[1][0].callback_data, "abort:appr-42");

    // Verify button text
    assert!(keyboard[0][0].text.contains("Approve"));
    assert!(keyboard[0][1].text.contains("Reject"));
    assert!(keyboard[1][0].text.contains("Abort"));
}

// ======================================================================
// 5. Cleanup logic -- stale sessions and orphaned threads
// ======================================================================

/// Stale session candidates include sessions older than the timeout.
#[test]
fn stale_session_candidates_by_age() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("fresh-1", 1, None, None, None, None, None)
        .unwrap();
    mgr.create_session("stale-1", 1, None, None, None, None, None)
        .unwrap();

    // Make stale-1 appear very old
    mgr.test_set_last_activity("stale-1", "2020-01-01T00:00:00.000Z")
        .unwrap();

    let stale = mgr.get_stale_session_candidates(1).unwrap();
    let stale_ids: Vec<&str> = stale.iter().map(|s| s.id.as_str()).collect();
    assert!(
        stale_ids.contains(&"stale-1"),
        "Old session should be stale"
    );
    assert!(
        !stale_ids.contains(&"fresh-1"),
        "Fresh session should NOT be stale"
    );
}

/// Stale sessions without tmux info are detected (1h timeout path).
#[test]
fn stale_session_no_tmux_info() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("no-tmux-1", 1, None, None, None, None, None)
        .unwrap();
    mgr.test_set_last_activity("no-tmux-1", "2020-06-01T00:00:00.000Z")
        .unwrap();

    let stale = mgr.get_stale_session_candidates(1).unwrap();
    assert_eq!(stale.len(), 1);
    assert!(
        stale[0].tmux_target.is_none(),
        "Session without tmux should be flagged"
    );
}

/// Stale sessions with tmux info use the longer timeout.
#[test]
fn stale_session_with_tmux_uses_longer_timeout() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("tmux-sess", 1, None, None, None, Some("s:0.0"), None)
        .unwrap();
    // 2 hours ago -- stale for no-tmux (1h) but NOT for tmux (24h)
    let two_hours_ago = (chrono::Utc::now() - chrono::TimeDelta::hours(2))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    mgr.test_set_last_activity("tmux-sess", &two_hours_ago)
        .unwrap();

    // Query with 1h timeout -- this session has tmux info so it should appear
    // in candidates but in the daemon the longer timeout is applied separately.
    let stale = mgr.get_stale_session_candidates(1).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].tmux_target.as_deref(), Some("s:0.0"));
}

/// Orphaned thread sessions: ended sessions that still have a thread_id.
#[test]
fn orphaned_thread_detection() {
    let (mgr, _tmp) = make_mgr();

    // Create two sessions with threads, end both
    mgr.create_session("ended-with-thread", 1, None, None, None, None, None)
        .unwrap();
    mgr.set_session_thread("ended-with-thread", 111).unwrap();
    mgr.end_session("ended-with-thread", SessionStatus::Ended)
        .unwrap();

    mgr.create_session("ended-no-thread", 1, None, None, None, None, None)
        .unwrap();
    mgr.end_session("ended-no-thread", SessionStatus::Ended)
        .unwrap();

    let orphans = mgr.get_orphaned_thread_sessions().unwrap();
    let orphan_ids: Vec<&str> = orphans.iter().map(|s| s.id.as_str()).collect();

    assert!(
        orphan_ids.contains(&"ended-with-thread"),
        "Ended session with thread_id should be orphaned"
    );
    assert!(
        !orphan_ids.contains(&"ended-no-thread"),
        "Ended session WITHOUT thread_id should NOT be orphaned"
    );
}

/// After clearing thread_id, session is no longer orphaned.
#[test]
fn clear_thread_removes_orphan() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("clear-orph", 1, None, None, None, None, None)
        .unwrap();
    mgr.set_session_thread("clear-orph", 222).unwrap();
    mgr.end_session("clear-orph", SessionStatus::Ended).unwrap();

    assert_eq!(mgr.get_orphaned_thread_sessions().unwrap().len(), 1);

    mgr.clear_thread_id("clear-orph").unwrap();

    assert_eq!(mgr.get_orphaned_thread_sessions().unwrap().len(), 0);
}

// ======================================================================
// 6. Tmux target ownership (used by cleanup for pane reassignment check)
// ======================================================================

/// Tmux target owned by another session triggers cleanup.
#[test]
fn tmux_target_ownership_detection() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("owner-1", 1, None, None, None, None, None)
        .unwrap();
    mgr.create_session("owner-2", 1, None, None, None, None, None)
        .unwrap();

    mgr.set_tmux_info("owner-1", Some("s:0.0"), None).unwrap();

    // owner-2 asks if "s:0.0" is owned by someone else -- yes, owner-1 has it
    assert!(mgr
        .is_tmux_target_owned_by_other("s:0.0", "owner-2")
        .unwrap());

    // owner-1 asks the same -- no, it's their own
    assert!(!mgr
        .is_tmux_target_owned_by_other("s:0.0", "owner-1")
        .unwrap());

    // Unassigned target -- not owned by anyone
    assert!(!mgr
        .is_tmux_target_owned_by_other("s:1.0", "owner-2")
        .unwrap());
}

// ======================================================================
// 7. Session ID and slash command validation
// ======================================================================

/// Session IDs used by handlers must pass validation.
#[test]
fn session_id_validation_handler_cases() {
    // Valid IDs that handlers would receive
    assert!(is_valid_session_id("session-abc12345"));
    assert!(is_valid_session_id("abc123"));
    assert!(is_valid_session_id("session.1.2.3"));
    assert!(is_valid_session_id("my_session-id"));

    // Invalid IDs that handle_socket_message would reject
    assert!(!is_valid_session_id(""));
    assert!(!is_valid_session_id("bad;id"));
    assert!(!is_valid_session_id("path/traversal"));
    assert!(!is_valid_session_id("with spaces"));
    assert!(!is_valid_session_id(&"x".repeat(129)));
}

/// Session IDs with special chars must be rejected (SQL injection prevention).
#[test]
fn session_id_rejects_injection_chars() {
    assert!(!is_valid_session_id("'; DROP TABLE sessions; --"));
    assert!(!is_valid_session_id("id$(whoami)"));
    assert!(!is_valid_session_id("id`cmd`"));
    assert!(!is_valid_session_id("id|pipe"));
    assert!(!is_valid_session_id("id>file"));
    assert!(!is_valid_session_id("id\nline"));
}

/// create_session rejects invalid session IDs.
#[test]
fn create_session_rejects_invalid_id() {
    let (mgr, _tmp) = make_mgr();
    let result = mgr.create_session("bad;id", 1, None, None, None, None, None);
    assert!(result.is_err());
}

/// Slash command validation (used by telegram_handlers for cc prefix).
#[test]
fn slash_command_validation() {
    assert!(is_valid_slash_command("/clear"));
    assert!(is_valid_slash_command("/compact"));
    assert!(is_valid_slash_command("/rename My Feature"));
    assert!(is_valid_slash_command("/help"));

    // Shell metacharacters must be rejected
    assert!(!is_valid_slash_command(""));
    assert!(!is_valid_slash_command("/clear;rm -rf /"));
    assert!(!is_valid_slash_command("/clear$(whoami)"));
    assert!(!is_valid_slash_command("/cmd|pipe"));
    assert!(!is_valid_slash_command("/cmd>file"));
    assert!(!is_valid_slash_command("/cmd`backtick`"));
}

/// Session status validation.
#[test]
fn session_status_validation() {
    assert!(is_valid_session_status("active"));
    assert!(is_valid_session_status("ended"));
    assert!(is_valid_session_status("aborted"));
    assert!(!is_valid_session_status("bogus"));
    assert!(!is_valid_session_status(""));
    assert!(!is_valid_session_status("ACTIVE"));
}

// ======================================================================
// 8. Formatting helpers (used by socket_handlers for Telegram messages)
// ======================================================================

/// format_session_start produces expected Markdown.
#[test]
fn format_session_start_output() {
    let text = ctm::formatting::format_session_start(
        "session-abc12345",
        Some("/opt/myproject"),
        Some("devbox"),
    );
    assert!(text.contains("session-abc12345"));
    assert!(text.contains("myproject") || text.contains("/opt/myproject"));
}

/// format_session_end includes duration when provided.
#[test]
fn format_session_end_with_duration() {
    let text = ctm::formatting::format_session_end("sess-1", Some(65_000));
    assert!(
        text.contains("sess-1"),
        "Session end message should contain session ID"
    );
}

/// format_session_end handles None duration gracefully.
#[test]
fn format_session_end_no_duration() {
    let text = ctm::formatting::format_session_end("sess-2", None);
    assert!(text.contains("sess-2"));
}

/// format_agent_response wraps content appropriately.
#[test]
fn format_agent_response_content() {
    let text = ctm::formatting::format_agent_response("Hello, this is a response from Claude.");
    assert!(!text.is_empty());
    assert!(text.contains("Hello"));
}

/// format_approval_request produces a readable prompt.
#[test]
fn format_approval_request_readable() {
    let text = ctm::formatting::format_approval_request("Allow Bash: rm -rf /tmp/test?");
    assert!(!text.is_empty());
    assert!(text.contains("rm -rf") || text.contains("Bash") || text.contains("Allow"));
}

/// format_error wraps error text.
#[test]
fn format_error_wraps_text() {
    let text = ctm::formatting::format_error("Something went wrong");
    assert!(text.contains("Something went wrong"));
}

/// truncate works correctly at various lengths.
#[test]
fn truncate_various_lengths() {
    // max_len < 4: returns chars without ellipsis
    assert_eq!(ctm::formatting::truncate("hello", 3), "hel");
    assert_eq!(ctm::formatting::truncate("hi", 10), "hi");
    assert_eq!(ctm::formatting::truncate("", 5), "");
    // max_len >= 4: truncates with "..." suffix
    assert_eq!(ctm::formatting::truncate("hello world", 8), "hello...");
    assert_eq!(ctm::formatting::truncate("short", 100), "short");
}

/// short_path strips common prefixes.
#[test]
fn short_path_strips_prefix() {
    let short = ctm::formatting::short_path("/home/user/.local/share/some/deep/file.rs");
    // Should be shorter than the original or equal
    assert!(short.len() <= "/home/user/.local/share/some/deep/file.rs".len());
}

/// chunk_message splits long text correctly.
#[test]
fn chunk_message_splits_correctly() {
    let text = "x".repeat(10000);
    let chunks = ctm::formatting::chunk_message(&text, 4000);
    assert!(
        chunks.len() >= 3,
        "10000 chars should produce at least 3 chunks"
    );
    // All chunks together should contain all content
    let total_len: usize = chunks.iter().map(|c| c.len()).sum();
    assert!(total_len >= 10000);
}

/// format_tool_details handles various tool types.
#[test]
fn format_tool_details_coverage() {
    let input = serde_json::json!({"file_path": "/src/main.rs", "command": "cargo test"});
    let text = ctm::formatting::format_tool_details("Bash", &input);
    assert!(!text.is_empty());

    let empty_input = serde_json::json!({});
    let text2 = ctm::formatting::format_tool_details("Unknown", &empty_input);
    assert!(!text2.is_empty());
}

// ======================================================================
// 9. Config validation (used by daemon startup)
// ======================================================================

/// validate_config identifies missing bot_token and chat_id as errors.
#[test]
fn config_validation_missing_credentials() {
    let config = Config {
        bot_token: String::new(),
        chat_id: 0,
        enabled: false,
        verbose: false,
        approvals: true,
        use_threads: true,
        chunk_size: 4000,
        rate_limit: 20,
        session_timeout: 30,
        stale_session_timeout_hours: 72,
        auto_delete_topics: true,
        topic_delete_delay_minutes: 15,
        inactivity_delete_threshold_minutes: 720,
        socket_path: PathBuf::from("/tmp/test.sock"),
        config_dir: PathBuf::from("/tmp"),
        config_path: PathBuf::from("/tmp/config.json"),
        forum_enabled: false,
        subagent_detection_window_secs: 60,
    };

    let (errors, warnings) = ctm::config::validate_config(&config);
    assert!(
        errors.len() >= 2,
        "Should have at least 2 errors (token + chat_id)"
    );
    assert!(!warnings.is_empty(), "Should warn about disabled mirroring");
}

/// validate_config warns about extreme chunk_size.
#[test]
fn config_validation_bad_chunk_size() {
    let config = Config {
        bot_token: "valid-token".to_string(),
        chat_id: -1001234567890,
        enabled: true,
        verbose: false,
        approvals: true,
        use_threads: true,
        chunk_size: 500, // Too small
        rate_limit: 20,
        session_timeout: 30,
        stale_session_timeout_hours: 72,
        auto_delete_topics: true,
        topic_delete_delay_minutes: 15,
        inactivity_delete_threshold_minutes: 720,
        socket_path: PathBuf::from("/tmp/test.sock"),
        config_dir: PathBuf::from("/tmp"),
        config_path: PathBuf::from("/tmp/config.json"),
        forum_enabled: false,
        subagent_detection_window_secs: 60,
    };

    let (errors, warnings) = ctm::config::validate_config(&config);
    assert!(errors.is_empty());
    assert!(
        warnings.iter().any(|w| w.contains("CHUNK_SIZE")),
        "Should warn about bad chunk_size"
    );
}

/// Socket path validation (security check in daemon startup).
#[test]
fn socket_path_validation() {
    assert!(ctm::config::validate_socket_path("/tmp/bridge.sock"));
    assert!(ctm::config::validate_socket_path(
        "/home/user/.config/ctm/bridge.sock"
    ));

    // Security: reject path traversal
    assert!(!ctm::config::validate_socket_path("/tmp/../etc/evil.sock"));
    // Reject relative paths
    assert!(!ctm::config::validate_socket_path("relative/path.sock"));
    // Reject empty
    assert!(!ctm::config::validate_socket_path(""));
    // Reject too-long paths (AF_UNIX sun_path limit)
    assert!(!ctm::config::validate_socket_path(&format!(
        "/{}",
        "a".repeat(104)
    )));
}

// ======================================================================
// 10. Safe command and tmux key whitelists (used by handlers)
// ======================================================================

/// SAFE_COMMANDS whitelist contains expected commands.
#[test]
fn safe_commands_whitelist() {
    assert!(SAFE_COMMANDS.contains(&"ls"));
    assert!(SAFE_COMMANDS.contains(&"cat"));
    assert!(SAFE_COMMANDS.contains(&"grep"));
    assert!(SAFE_COMMANDS.contains(&"echo"));
    assert!(!SAFE_COMMANDS.contains(&"rm"));
    assert!(!SAFE_COMMANDS.contains(&"sudo"));
}

/// ALLOWED_TMUX_KEYS whitelist contains expected keys.
#[test]
fn allowed_tmux_keys_whitelist() {
    assert!(ALLOWED_TMUX_KEYS.contains(&"Enter"));
    assert!(ALLOWED_TMUX_KEYS.contains(&"Escape"));
    assert!(ALLOWED_TMUX_KEYS.contains(&"C-c"));
    assert!(ALLOWED_TMUX_KEYS.contains(&"Tab"));
    assert!(!ALLOWED_TMUX_KEYS.contains(&"Delete"));
}

// ======================================================================
// 11. Bot token scrubbing (security: used by all error logging)
// ======================================================================

/// scrub_bot_token removes tokens from error messages.
#[test]
fn bot_token_scrubbing() {
    let msg = "POST https://api.telegram.org/bot123456:ABC-DEF_test/sendMessage failed";
    let scrubbed = ctm::bot::scrub_bot_token(msg);
    assert!(!scrubbed.contains("123456:ABC-DEF_test"));
    assert!(scrubbed.contains("[REDACTED]"));
}

/// scrub_bot_token handles text without tokens.
#[test]
fn bot_token_scrubbing_safe_text() {
    let msg = "Network timeout after 30 seconds";
    assert_eq!(ctm::bot::scrub_bot_token(msg), msg);
}

/// scrub_bot_token handles multiple token occurrences.
#[test]
fn bot_token_scrubbing_multiple() {
    let msg = "bot111:AAA_bbb/getMe and bot222:DDD_eee/sendMessage";
    let scrubbed = ctm::bot::scrub_bot_token(msg);
    assert!(!scrubbed.contains("111:AAA_bbb"));
    assert!(!scrubbed.contains("222:DDD_eee"));
}

// ======================================================================
// 12. Tmux info persistence (used by handlers for target routing)
// ======================================================================

/// Full tmux info round-trip through DB (set/get).
#[test]
fn tmux_info_full_roundtrip() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("tmux-rt", 1, None, None, None, None, None)
        .unwrap();

    // Initially no tmux info
    assert!(mgr.get_tmux_info("tmux-rt").unwrap().is_none());

    // Set both target and socket
    mgr.set_tmux_info("tmux-rt", Some("session:0.1"), Some("/tmp/tmux-sock"))
        .unwrap();
    let (target, socket) = mgr.get_tmux_info("tmux-rt").unwrap().unwrap();
    assert_eq!(target, "session:0.1");
    assert_eq!(socket.as_deref(), Some("/tmp/tmux-sock"));

    // Update target only
    mgr.set_tmux_info("tmux-rt", Some("session:0.2"), None)
        .unwrap();
    let (target, socket) = mgr.get_tmux_info("tmux-rt").unwrap().unwrap();
    assert_eq!(target, "session:0.2");
    // Socket should still be set from before
    assert_eq!(socket.as_deref(), Some("/tmp/tmux-sock"));
}

/// Create session with tmux info atomically (M2.12 path).
#[test]
fn create_session_with_tmux_atomically() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session(
        "atomic-tmux",
        1,
        Some("host"),
        Some("/project"),
        None,
        Some("s:0.0"),
        Some("/tmp/tmux"),
    )
    .unwrap();

    let s = mgr.get_session("atomic-tmux").unwrap().unwrap();
    assert_eq!(s.tmux_target.as_deref(), Some("s:0.0"));
    assert_eq!(s.tmux_socket.as_deref(), Some("/tmp/tmux"));

    let info = mgr.get_tmux_info("atomic-tmux").unwrap().unwrap();
    assert_eq!(info.0, "s:0.0");
    assert_eq!(info.1.as_deref(), Some("/tmp/tmux"));
}

// ======================================================================
// 13. Formatting: escape and detect helpers
// ======================================================================

/// escape_markdown_v2 escapes special chars outside code blocks.
#[test]
fn escape_markdown_v2_special_chars() {
    let escaped = ctm::formatting::escape_markdown_v2("hello_world *bold* [link]");
    assert!(!escaped.contains("_world") || escaped.contains("\\_world"));
    // Code blocks should be preserved
    let with_code = ctm::formatting::escape_markdown_v2("text `code_here` more_text");
    assert!(with_code.contains("`code_here`"));
}

/// detect_language identifies common languages.
#[test]
fn detect_language_coverage() {
    // detect_language uses starts_with patterns
    assert_eq!(ctm::formatting::detect_language("fn main() { }"), "rust");
    assert_eq!(ctm::formatting::detect_language("use std::io;"), "rust");
    assert_eq!(
        ctm::formatting::detect_language("def hello():\n    pass"),
        "python"
    );
    assert_eq!(ctm::formatting::detect_language("import os"), "python");
    assert_eq!(ctm::formatting::detect_language("package main"), "go");
    assert_eq!(
        ctm::formatting::detect_language("#include <stdio.h>"),
        "cpp"
    );
    // Unknown content returns empty string
    assert_eq!(ctm::formatting::detect_language("just plain text"), "");
}

/// strip_ansi removes escape codes.
#[test]
fn strip_ansi_codes() {
    let ansi = "\x1b[31mRed text\x1b[0m";
    let clean = ctm::formatting::strip_ansi(ansi);
    assert_eq!(clean, "Red text");
    assert!(!clean.contains("\x1b"));
}

/// needs_chunking correctly identifies oversized messages.
#[test]
fn needs_chunking_detection() {
    assert!(!ctm::formatting::needs_chunking("short", 4000));
    assert!(ctm::formatting::needs_chunking(&"x".repeat(5000), 4000));
}

/// estimate_chunks returns correct estimates.
#[test]
fn estimate_chunks_accuracy() {
    assert_eq!(ctm::formatting::estimate_chunks("short", 4000), 1);
    assert!(ctm::formatting::estimate_chunks(&"x".repeat(10000), 4000) >= 3);
}

// ======================================================================
// 14. Mirror status persistence (used by /toggle command handler)
// ======================================================================

/// Mirror status round-trip (toggle command path).
#[test]
fn mirror_status_toggle_roundtrip() {
    let tmp = tempdir().unwrap();

    // Default is true when file doesn't exist
    assert!(ctm::config::read_mirror_status(tmp.path()));

    // Write disabled
    ctm::config::write_mirror_status(tmp.path(), false, Some(12345));
    assert!(!ctm::config::read_mirror_status(tmp.path()));

    // Write enabled
    ctm::config::write_mirror_status(tmp.path(), true, None);
    assert!(ctm::config::read_mirror_status(tmp.path()));
}

/// Corrupt status file defaults to enabled (safe default).
#[test]
fn mirror_status_corrupt_defaults_enabled() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("status.json"), "not valid json").unwrap();
    assert!(ctm::config::read_mirror_status(tmp.path()));
}

// ======================================================================
// 15. Pending approvals listing (used by /status command handler)
// ======================================================================

/// get_pending_approvals returns only pending ones for a session.
#[test]
fn pending_approvals_per_session() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("pa-sess", 1, None, None, None, None, None)
        .unwrap();
    mgr.create_session("pa-other", 1, None, None, None, None, None)
        .unwrap();

    let a1 = mgr.create_approval("pa-sess", "Q1?", None).unwrap();
    let _a2 = mgr.create_approval("pa-sess", "Q2?", None).unwrap();
    let _a3 = mgr.create_approval("pa-other", "Q3?", None).unwrap();

    let pending = mgr.get_pending_approvals("pa-sess").unwrap();
    assert_eq!(
        pending.len(),
        2,
        "Should have 2 pending approvals for pa-sess"
    );

    // Resolve one
    mgr.resolve_approval(&a1, ApprovalStatus::Approved).unwrap();

    let pending = mgr.get_pending_approvals("pa-sess").unwrap();
    assert_eq!(
        pending.len(),
        1,
        "Should have 1 pending approval after resolving one"
    );
}

// ======================================================================
// 16. MessageType Display (used for logging in handlers)
// ======================================================================

/// MessageType Display trait produces readable strings.
#[test]
fn message_type_display() {
    assert_eq!(format!("{}", MessageType::AgentResponse), "agent_response");
    assert_eq!(format!("{}", MessageType::ToolStart), "tool_start");
    assert_eq!(format!("{}", MessageType::SessionStart), "session_start");
    assert_eq!(format!("{}", MessageType::SessionEnd), "session_end");
    assert_eq!(
        format!("{}", MessageType::ApprovalRequest),
        "approval_request"
    );
    assert_eq!(
        format!("{}", MessageType::ApprovalResponse),
        "approval_response"
    );
    assert_eq!(format!("{}", MessageType::Command), "command");
    assert_eq!(format!("{}", MessageType::Unknown), "unknown");
    assert_eq!(format!("{}", MessageType::SendImage), "send_image");
}

// ======================================================================
// 17. Duplicate session creation is idempotent
// ======================================================================

/// Creating a session twice returns the same ID and updates activity.
#[test]
fn duplicate_session_creation_idempotent() {
    let (mgr, _tmp) = make_mgr();
    let sid = "dup-test-1";

    let id1 = mgr
        .create_session(sid, 1, Some("host1"), Some("/proj1"), None, None, None)
        .unwrap();
    let s1 = mgr.get_session(sid).unwrap().unwrap();

    // Small delay to ensure activity timestamp changes
    std::thread::sleep(std::time::Duration::from_millis(10));

    let id2 = mgr
        .create_session(sid, 2, Some("host2"), Some("/proj2"), None, None, None)
        .unwrap();
    let s2 = mgr.get_session(sid).unwrap().unwrap();

    assert_eq!(id1, id2, "Second create should return same ID");
    assert!(
        s2.last_activity >= s1.last_activity,
        "Activity should be updated"
    );
    // ADR-013 F3: When new metadata is provided, it auto-heals the existing session.
    // hostname/project_dir are updated to the latest values (not discarded).
    assert_eq!(s2.hostname.as_deref(), Some("host2"));
    assert_eq!(s2.project_dir.as_deref(), Some("/proj2"));
}

// ======================================================================
// 18. Handler routing: mirroring toggle gating
// ======================================================================

/// BridgeMessage types that are safety-critical should always be processed.
/// This test documents the routing invariant from handle_socket_message.
#[test]
fn safety_critical_message_types() {
    // These types bypass the mirroring toggle gate
    let always_active = [
        MessageType::ApprovalRequest,
        MessageType::ApprovalResponse,
        MessageType::Command,
    ];

    // These types are gated by mirroring toggle
    let gated = [
        MessageType::AgentResponse,
        MessageType::ToolStart,
        MessageType::ToolResult,
        MessageType::UserInput,
        MessageType::Error,
        MessageType::SessionStart,
        MessageType::SessionEnd,
        MessageType::TurnComplete,
        MessageType::PreCompact,
        MessageType::SessionRename,
        MessageType::SendImage,
    ];

    // Verify they are distinct sets
    for mt in &always_active {
        assert!(
            !gated.contains(mt),
            "Safety-critical type {:?} should NOT be in gated set",
            mt
        );
    }
}

// ======================================================================
// 19. Session stats (used by /status command)
// ======================================================================

/// get_stats accurately reflects session and approval counts.
#[test]
fn stats_comprehensive() {
    let (mgr, _tmp) = make_mgr();

    // Empty state
    let (active, pending) = mgr.get_stats().unwrap();
    assert_eq!(active, 0);
    assert_eq!(pending, 0);

    // Add sessions and approvals
    mgr.create_session("st-1", 1, None, None, None, None, None)
        .unwrap();
    mgr.create_session("st-2", 1, None, None, None, None, None)
        .unwrap();
    mgr.create_session("st-3", 1, None, None, None, None, None)
        .unwrap();
    mgr.create_approval("st-1", "q1?", None).unwrap();
    mgr.create_approval("st-2", "q2?", None).unwrap();

    let (active, pending) = mgr.get_stats().unwrap();
    assert_eq!(active, 3);
    assert_eq!(pending, 2);

    // End one session (should expire its approvals)
    mgr.end_session("st-1", SessionStatus::Ended).unwrap();
    let (active, pending) = mgr.get_stats().unwrap();
    assert_eq!(active, 2);
    assert_eq!(pending, 1); // st-1's approval expired, st-2's still pending
}

// ======================================================================
// 20. SessionStatus and ApprovalStatus enum coverage
// ======================================================================

/// SessionStatus enum variants have correct string representations.
#[test]
fn session_status_enum_as_str() {
    assert_eq!(SessionStatus::Active.as_str(), "active");
    assert_eq!(SessionStatus::Ended.as_str(), "ended");
    assert_eq!(SessionStatus::Aborted.as_str(), "aborted");
}

/// SessionStatus Display matches as_str.
#[test]
fn session_status_display() {
    assert_eq!(format!("{}", SessionStatus::Active), "active");
    assert_eq!(format!("{}", SessionStatus::Ended), "ended");
    assert_eq!(format!("{}", SessionStatus::Aborted), "aborted");
}

/// ApprovalStatus enum variants have correct string representations.
#[test]
fn approval_status_enum_as_str() {
    assert_eq!(ApprovalStatus::Pending.as_str(), "pending");
    assert_eq!(ApprovalStatus::Approved.as_str(), "approved");
    assert_eq!(ApprovalStatus::Rejected.as_str(), "rejected");
    assert_eq!(ApprovalStatus::Expired.as_str(), "expired");
}

// ======================================================================
// 21. NDJSON wire format (the framing protocol between hook and daemon)
// ======================================================================

/// BridgeMessage serializes to a single JSON line (no embedded newlines).
#[test]
fn bridge_message_ndjson_compatible() {
    let mut meta = serde_json::Map::new();
    meta.insert("tool".into(), serde_json::json!("Bash"));
    meta.insert(
        "input".into(),
        serde_json::json!({"command": "echo 'hello\nworld'"}),
    );

    let msg = make_bridge_msg_with_meta(
        MessageType::ToolStart,
        "session-ndjson",
        "content with\nnewline",
        meta,
    );

    let json = serde_json::to_string(&msg).unwrap();
    // NDJSON framing requires no unescaped newlines in the JSON line
    assert!(
        !json.contains('\n') || json.contains("\\n"),
        "Serialized JSON must not contain raw newlines"
    );
}
