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
}

#[derive(Debug, Clone, Deserialize)]
pub struct StopEvent {
    #[serde(flatten)]
    pub base: HookEventBase,
    #[serde(default)]
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
    #[serde(default)]
    pub subagent_id: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// Bridge message sent over the Unix domain socket
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub session_id: String,
    pub timestamp: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Information about a connected socket client (mirrors TS `SocketClientInfo`).
#[derive(Debug, Clone)]
pub struct SocketClientInfo {
    /// Unique client identifier (assigned on connect).
    pub id: String,
    /// Epoch millis when the client connected.
    pub connected_at: u64,
    /// Session ID the client is associated with, if known.
    pub session_id: Option<String>,
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

/// Maximum stdin/NDJSON line size (1 MiB)
pub const MAX_LINE_BYTES: usize = 1_048_576;

/// Session ID validation pattern
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_SESSION_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Slash command character whitelist validation
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
}
