use serde::{Deserialize, Serialize};

/// Hook event types sent by Claude Code
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "hook_event_name")]
pub enum HookEvent {
    Stop(StopEvent),
    SubagentStop(SubagentStopEvent),
    PreToolUse(PreToolUseEvent),
    PostToolUse(PostToolUseEvent),
    Notification(NotificationEvent),
    UserPromptSubmit(UserPromptSubmitEvent),
    PreCompact(PreCompactEvent),
}

/// Base fields present on all hook events
#[derive(Debug, Clone, Deserialize)]
pub struct HookEventBase {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub hook_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // Deserialized from JSON
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StopEvent {
    #[serde(flatten)]
    pub base: HookEventBase,
    #[serde(default)]
    #[allow(dead_code)] // Deserialized from JSON
    pub stop_hook_active: bool,
    #[serde(default)]
    pub transcript_summary: Option<String>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubagentStopEvent {
    #[serde(flatten)]
    pub base: HookEventBase,
    /// L3.10: The TypeScript type declares `subagent_id: string` (required), but
    /// the JSON payload may omit it in practice.  We keep `Option<String>` as an
    /// intentional relaxation for robustness — missing values deserialize as `None`
    /// instead of causing a parse failure.
    #[serde(default)]
    #[allow(dead_code)] // Deserialized from JSON
    pub subagent_id: Option<String>,
    /// L6.2: Optional result text from the subagent, mirroring the TS type's
    /// `result?: string` field.
    #[serde(default)]
    #[allow(dead_code)] // Deserialized from JSON
    pub result: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreToolUseEvent {
    #[serde(flatten)]
    pub base: HookEventBase,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
    #[serde(default)]
    pub tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostToolUseEvent {
    #[serde(flatten)]
    pub base: HookEventBase,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
    #[serde(default)]
    pub tool_output: Option<String>,
    #[serde(default)]
    pub tool_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotificationEvent {
    #[serde(flatten)]
    pub base: HookEventBase,
    #[serde(default)]
    pub message: String,
    /// L6.3 (INTENTIONAL): `level` is a free-form `Option<String>` rather than a
    /// Rust enum.  The TypeScript type is `"info" | "warning" | "error"` but serde
    /// cannot enforce string enum unions from JSON without a custom deserializer.
    /// Runtime string comparison (`level.as_deref() == Some("error")`) is sufficient.
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub notification_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserPromptSubmitEvent {
    #[serde(flatten)]
    pub base: HookEventBase,
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreCompactEvent {
    #[serde(flatten)]
    pub base: HookEventBase,
}

/// Message types sent to the bridge daemon via Unix socket (NDJSON)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    AgentResponse,
    ToolStart,
    ToolResult,
    ApprovalRequest,
    UserInput,
    ApprovalResponse,
    Command,
    Error,
    SessionStart,
    SessionEnd,
    TurnComplete,
    PreCompact,
    SessionRename,
    SendImage,
    /// Forward-compatible catch-all for unknown message types.
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentResponse => write!(f, "agent_response"),
            Self::ToolStart => write!(f, "tool_start"),
            Self::ToolResult => write!(f, "tool_result"),
            Self::ApprovalRequest => write!(f, "approval_request"),
            Self::UserInput => write!(f, "user_input"),
            Self::ApprovalResponse => write!(f, "approval_response"),
            Self::Command => write!(f, "command"),
            Self::Error => write!(f, "error"),
            Self::SessionStart => write!(f, "session_start"),
            Self::SessionEnd => write!(f, "session_end"),
            Self::TurnComplete => write!(f, "turn_complete"),
            Self::PreCompact => write!(f, "pre_compact"),
            Self::SessionRename => write!(f, "session_rename"),
            Self::SendImage => write!(f, "send_image"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Bridge message sent over the Unix domain socket
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMessage {
    #[serde(rename = "type")]
    pub msg_type: MessageType,
    pub session_id: String,
    pub timestamp: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Information about a connected socket client (mirrors TS `SocketClientInfo`).
#[derive(Debug, Clone)]
#[allow(dead_code)] // Library API
pub struct SocketClientInfo {
    /// Unique client identifier (assigned on connect).
    pub id: String,
    /// Epoch millis when the client connected.
    pub connected_at: u64,
    /// Session ID the client is associated with, if known.
    pub session_id: Option<String>,
}

// L4.5: InlineButton is defined solely in bot.rs and re-exported from lib.rs.
// This eliminates the duplicate definition that previously lived here.
// External consumers should use `ctm::bot::InlineButton`.

/// L3.1: Result returned by hook processing.
///
/// Represents the decision made by a hook (e.g. PreToolUse approval).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Library API
pub struct HookResult {
    /// The permission decision: "allow", "deny", or "ask".
    ///
    /// L6.12 (INTENTIONAL): `decision` is a free-form `Option<String>` rather than
    /// a Rust enum.  The TypeScript type is `"allow" | "deny" | "ask"` but serde
    /// string matching works at runtime without a custom deserializer.  Hook callers
    /// compare via `decision.as_deref() == Some("allow")` etc.
    pub decision: Option<String>,
    /// Human-readable reason for the decision.
    pub reason: Option<String>,
    /// Modified input (for hooks that transform tool input before execution).
    pub modified_input: Option<serde_json::Value>,
}

/// Whitelist of safe Bash commands that auto-approve without Telegram confirmation
pub const SAFE_COMMANDS: &[&str] = &[
    "ls", "pwd", "cat", "head", "tail", "echo", "grep", "find", "which",
];

/// Whitelist of allowed tmux special keys
pub const ALLOWED_TMUX_KEYS: &[&str] = &[
    "Enter", "Escape", "Tab", "C-c", "C-u", "C-d", "C-l", "Up", "Down", "Left", "Right", "BSpace",
];

/// Maximum session ID length
pub const MAX_SESSION_ID_LEN: usize = 128;

/// Valid status values for sessions.
pub const VALID_SESSION_STATUSES: &[&str] = &["active", "ended", "aborted"];

/// Valid status values for approvals.
pub const VALID_APPROVAL_STATUSES: &[&str] =
    &["pending", "approved", "denied", "rejected", "expired"];

/// Returns true if `s` is a recognized session status.
pub fn is_valid_session_status(s: &str) -> bool {
    VALID_SESSION_STATUSES.contains(&s)
}

/// Returns true if `s` is a recognized approval status.
pub fn is_valid_approval_status(s: &str) -> bool {
    VALID_APPROVAL_STATUSES.contains(&s)
}

/// Maximum stdin/NDJSON line size (1 MiB)
pub const MAX_LINE_BYTES: usize = 1_048_576;

/// Session ID validation pattern.
///
/// A valid session ID is non-empty, at most [`MAX_SESSION_ID_LEN`] characters,
/// and contains only ASCII alphanumerics, hyphens, underscores, or dots.
///
/// # Examples
///
/// ```
/// use ctm::types::is_valid_session_id;
///
/// assert!(is_valid_session_id("session-42"));
/// assert!(is_valid_session_id("abc_def.123"));
///
/// // Empty or overly long IDs are rejected.
/// assert!(!is_valid_session_id(""));
///
/// // Special characters are not allowed.
/// assert!(!is_valid_session_id("bad;id"));
/// ```
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_SESSION_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Slash command character whitelist validation.
///
/// Only ASCII alphanumerics, underscores, hyphens, spaces, and forward
/// slashes are permitted. This prevents shell injection via crafted
/// command strings.
///
/// # Examples
///
/// ```
/// use ctm::types::is_valid_slash_command;
///
/// assert!(is_valid_slash_command("/clear"));
/// assert!(is_valid_slash_command("/rename My Feature"));
///
/// // Shell metacharacters are rejected.
/// assert!(!is_valid_slash_command("/cmd;rm -rf /"));
/// assert!(!is_valid_slash_command(""));
/// ```
pub fn is_valid_slash_command(command: &str) -> bool {
    !command.is_empty()
        && command
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' ' | '/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_session_ids() {
        assert!(is_valid_session_id("abc123"));
        assert!(is_valid_session_id("abc-def-123"));
        assert!(is_valid_session_id("abc_def_123"));
        assert!(is_valid_session_id(&"a".repeat(128)));
    }

    #[test]
    fn test_invalid_session_ids() {
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id(&"a".repeat(129)));
        assert!(!is_valid_session_id("abc;rm"));
        assert!(!is_valid_session_id("abc def"));
        assert!(!is_valid_session_id("abc/def"));
    }

    #[test]
    fn test_session_id_with_dots() {
        assert!(is_valid_session_id("abc.def.123"));
        assert!(is_valid_session_id("session.1.2.3"));
    }

    #[test]
    fn test_valid_slash_commands() {
        assert!(is_valid_slash_command("/clear"));
        assert!(is_valid_slash_command("/compact"));
        assert!(is_valid_slash_command("/rename My Feature"));
    }

    #[test]
    fn test_invalid_slash_commands() {
        assert!(!is_valid_slash_command(""));
        assert!(!is_valid_slash_command("/clear;rm -rf /"));
        assert!(!is_valid_slash_command("/clear$(whoami)"));
        assert!(!is_valid_slash_command("/clear`id`"));
        assert!(!is_valid_slash_command("/cmd|pipe"));
        assert!(!is_valid_slash_command("/cmd>file"));
        assert!(!is_valid_slash_command("/cmd&&other"));
    }

    #[test]
    fn test_message_type_serde_roundtrip() {
        // Known variants round-trip correctly
        let mt = MessageType::AgentResponse;
        let json = serde_json::to_value(&mt).unwrap();
        assert_eq!(json, serde_json::Value::String("agent_response".into()));
        let parsed: MessageType = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, MessageType::AgentResponse);
    }

    #[test]
    fn test_message_type_unknown_variant() {
        // Unknown string deserializes to Unknown (forward compat)
        let parsed: MessageType =
            serde_json::from_value(serde_json::Value::String("future_type".into())).unwrap();
        assert_eq!(parsed, MessageType::Unknown);
    }

    #[test]
    fn test_bridge_message_type_field_rename() {
        // BridgeMessage.msg_type serializes as "type" in JSON
        let msg = BridgeMessage {
            msg_type: MessageType::Command,
            session_id: "s1".into(),
            timestamp: "2024-01-01T00:00:00Z".into(),
            content: "test".into(),
            metadata: None,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some("command"),);
        // Round-trip
        let parsed: BridgeMessage = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.msg_type, MessageType::Command);
    }
}
