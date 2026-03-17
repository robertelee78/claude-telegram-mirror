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
    pub timestamp: Option<String>,
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
    /// L3.10: The TypeScript type declares `subagent_id: string` (required), but
    /// the JSON payload may omit it in practice.  We keep `Option<String>` as an
    /// intentional relaxation for robustness — missing values deserialize as `None`
    /// instead of causing a parse failure.
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

/// L3.1: Inline keyboard button definition (domain-level type).
///
/// Note: `bot::InlineButton` is the same shape but lives in the bot module.
/// This type is provided for external consumers who want to construct buttons
/// without depending on the bot module internals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}

/// L3.1: Result returned by hook processing.
///
/// Represents the decision made by a hook (e.g. PreToolUse approval).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookResult {
    /// The permission decision: "allow", "deny", or "ask".
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
