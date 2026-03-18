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
    SessionEnd(SessionEndEvent),
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
    /// GAP-8: Agent ID sent by Claude Code for sub-agent hooks.
    /// Present only when the hook fires from within a sub-agent context.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// GAP-8: Agent type sent by Claude Code for sub-agent hooks (e.g. "researcher").
    /// Present only when the hook fires from within a sub-agent context.
    #[serde(default)]
    pub agent_type: Option<String>,
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
    /// ADR-013 GAP-2: Agent type for the sub-agent (e.g. "Explore", "researcher").
    #[serde(default)]
    #[allow(dead_code)]
    pub agent_type: Option<String>,
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

/// Fires when the Claude Code session actually terminates (process exit,
/// /clear, logout, etc.). Unlike `Stop` which fires after every turn,
/// `SessionEnd` fires exactly once at the end of the session's lifetime.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionEndEvent {
    #[serde(flatten)]
    pub base: HookEventBase,
    /// Why the session ended: "clear", "logout", "prompt_input_exit",
    /// "bypass_permissions_disabled", or "other".
    #[serde(default)]
    pub reason: Option<String>,
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

impl BridgeMessage {
    /// Return a typed accessor for the metadata map.
    pub fn meta(&self) -> MessageMetadata<'_> {
        MessageMetadata(self.metadata.as_ref())
    }
}

/// Typed accessor for `BridgeMessage` metadata fields.
///
/// Wraps `Option<&serde_json::Map<String, serde_json::Value>>` and provides
/// named methods for every known metadata key, eliminating repetitive
/// `.metadata.as_ref().and_then(|m| m.get("key")).and_then(|v| v.as_str())`
/// chains throughout the daemon code.
///
/// Wire format and serialization are unchanged -- this is a read-only view.
pub struct MessageMetadata<'a>(Option<&'a serde_json::Map<String, serde_json::Value>>);

impl<'a> MessageMetadata<'a> {
    /// Helper: get a string value by key.
    fn str_field(&self, key: &str) -> Option<&'a str> {
        self.0?.get(key)?.as_str()
    }

    /// Helper: get a `serde_json::Value` reference by key.
    fn value_field(&self, key: &str) -> Option<&'a serde_json::Value> {
        self.0?.get(key)
    }

    /// Tool name (`"tool"`).
    pub fn tool(&self) -> Option<&'a str> {
        self.str_field("tool")
    }

    /// Tool input object (`"input"`).
    pub fn input(&self) -> Option<&'a serde_json::Value> {
        self.value_field("input")
    }

    /// Tool use identifier (`"toolUseId"`).
    pub fn tool_use_id(&self) -> Option<&'a str> {
        self.str_field("toolUseId")
    }

    /// Socket client identifier (`"_client_id"`).
    pub fn client_id(&self) -> Option<&'a str> {
        self.str_field("_client_id")
    }

    /// Hostname of the machine running the session (`"hostname"`).
    pub fn hostname(&self) -> Option<&'a str> {
        self.str_field("hostname")
    }

    /// Tmux target pane (`"tmuxTarget"`).
    pub fn tmux_target(&self) -> Option<&'a str> {
        self.str_field("tmuxTarget")
    }

    /// Tmux socket path (`"tmuxSocket"`).
    pub fn tmux_socket(&self) -> Option<&'a str> {
        self.str_field("tmuxSocket")
    }

    /// Message source (`"source"`, e.g. `"telegram"` or `"cli"`).
    pub fn source(&self) -> Option<&'a str> {
        self.str_field("source")
    }

    /// Project directory (`"projectDir"`).
    pub fn project_dir(&self) -> Option<&'a str> {
        self.str_field("projectDir")
    }

    /// Transcript file path (`"transcript_path"`).
    pub fn transcript_path(&self) -> Option<&'a str> {
        self.str_field("transcript_path")
    }

    /// Compact trigger (`"trigger"`, e.g. `"auto"` or `"manual"`).
    pub fn trigger(&self) -> Option<&'a str> {
        self.str_field("trigger")
    }

    /// Image/document caption (`"caption"`).
    pub fn caption(&self) -> Option<&'a str> {
        self.str_field("caption")
    }

    /// ADR-013: Sub-agent identifier (`"agentId"`).
    pub fn agent_id(&self) -> Option<&'a str> {
        self.str_field("agentId")
    }

    /// ADR-013 GAP-2: Sub-agent type label (`"agentType"`).
    pub fn agent_type(&self) -> Option<&'a str> {
        self.str_field("agentType")
    }

    /// Approval identifier (`"approvalId"`).
    #[allow(dead_code)] // Public API — used in tests and available for callers
    pub fn approval_id(&self) -> Option<&'a str> {
        self.str_field("approvalId")
    }

    /// GAP-9: Whether this session is running in headless mode (CLAUDE_CODE_HEADLESS).
    pub fn headless(&self) -> bool {
        self.0
            .and_then(|m| m.get("headless"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
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
    "Enter", "Escape", "Tab", "Space", "C-c", "C-u", "C-d", "C-l", "Up", "Down", "Left", "Right",
    "BSpace",
    // Digit keys for Claude Code's multi-select TUI (toggle option by 1-based index).
    "1", "2", "3", "4", "5", "6", "7", "8", "9",
];

/// Maximum session ID length
pub const MAX_SESSION_ID_LEN: usize = 128;

/// Valid status values for sessions.
///
/// Kept for backward compatibility with external callers. Prefer
/// [`SessionStatus`] for compile-time safety.
#[allow(dead_code)] // Backward-compat public API
pub const VALID_SESSION_STATUSES: &[&str] = &["active", "ended", "aborted"];

/// Valid status values for approvals.
///
/// Kept for backward compatibility with external callers. Prefer
/// [`ApprovalStatus`] for compile-time safety.
#[allow(dead_code)] // Backward-compat public API
pub const VALID_APPROVAL_STATUSES: &[&str] =
    &["pending", "approved", "denied", "rejected", "expired"];

/// Returns true if `s` is a recognized session status.
///
/// Kept for backward compatibility. Prefer `SessionStatus::try_from(s)`.
#[allow(dead_code)] // Backward-compat public API
pub fn is_valid_session_status(s: &str) -> bool {
    VALID_SESSION_STATUSES.contains(&s)
}

/// Returns true if `s` is a recognized approval status.
///
/// Kept for backward compatibility. Prefer `ApprovalStatus::try_from(s)`.
#[allow(dead_code)] // Backward-compat public API
pub fn is_valid_approval_status(s: &str) -> bool {
    VALID_APPROVAL_STATUSES.contains(&s)
}

/// Typed session status for compile-time safety.
///
/// The SQLite layer still stores these as TEXT strings; use `.as_str()` when
/// writing to the database and `TryFrom<&str>` when reading back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Ended,
    Aborted,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Ended => "ended",
            Self::Aborted => "aborted",
        }
    }
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl TryFrom<&str> for SessionStatus {
    type Error = String;

    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        match s {
            "active" => Ok(Self::Active),
            "ended" => Ok(Self::Ended),
            "aborted" => Ok(Self::Aborted),
            other => Err(format!("invalid session status: {other}")),
        }
    }
}

/// Typed approval status for compile-time safety.
///
/// The SQLite layer still stores these as TEXT strings; use `.as_str()` when
/// writing to the database and `TryFrom<&str>` when reading back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Rejected,
    Expired,
}

impl ApprovalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }
}

impl std::fmt::Display for ApprovalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl TryFrom<&str> for ApprovalStatus {
    type Error = String;

    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        match s {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "denied" => Ok(Self::Denied),
            "rejected" => Ok(Self::Rejected),
            "expired" => Ok(Self::Expired),
            other => Err(format!("invalid approval status: {other}")),
        }
    }
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

/// ADR-013 GAP-1: Validate an agent ID from user-controlled callback_data.
///
/// Rejects strings containing path-traversal characters (`/`, `\`, `..`) and
/// only allows ASCII alphanumerics, hyphens, underscores, and dots. This
/// prevents path traversal when the agent_id is used to construct file paths
/// such as `/tmp/ctm-subagent-{agent_id}.md`.
#[allow(dead_code)] // Library API — used by callback_handlers and socket_handlers (ADR-013)
pub fn is_valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
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

/// ADR-013: Extract the parent session ID from a sub-agent transcript path.
///
/// Sub-agent transcripts follow the pattern:
/// `~/.claude/projects/{project}/{parentSessionId}/subagents/agent-{agentId}.jsonl`
///
/// Splits on `/subagents/`, takes the left side, and returns the last path component
/// (which is the parent session ID).
///
/// Returns `None` if the path does not contain `/subagents/` or has no parent component.
#[allow(dead_code)] // Library API — used by daemon routing (ADR-013)
pub fn extract_parent_session_id(transcript_path: &str) -> Option<String> {
    let (parent_part, _) = transcript_path.split_once("/subagents/")?;
    let last_component = parent_part.rsplit('/').next()?;
    if last_component.is_empty() {
        return None;
    }
    Some(last_component.to_string())
}

/// ADR-013: Extract the agent ID from a sub-agent transcript path.
///
/// Sub-agent transcripts follow the pattern:
/// `~/.claude/projects/{project}/{parentSessionId}/subagents/agent-{agentId}.jsonl`
///
/// Splits on `/subagents/`, takes the right side, strips the `.jsonl` extension,
/// and returns the filename (e.g. `"agent-abc123"`).
///
/// Returns `None` if the path does not contain `/subagents/` or has no agent component.
#[allow(dead_code)] // Library API — used by daemon routing (ADR-013)
pub fn extract_agent_id(transcript_path: &str) -> Option<String> {
    let (_, agent_part) = transcript_path.split_once("/subagents/")?;
    // The agent_part may contain further path segments; take only the filename
    let filename = agent_part.rsplit('/').next()?;
    // Strip .jsonl extension if present
    let name = filename.strip_suffix(".jsonl").unwrap_or(filename);
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_agent_ids() {
        assert!(is_valid_agent_id("agent-abc123"));
        assert!(is_valid_agent_id("agent_abc_123"));
        assert!(is_valid_agent_id("agent.abc"));
        assert!(!is_valid_agent_id(""));
        assert!(!is_valid_agent_id("../etc/passwd"));
        assert!(!is_valid_agent_id("abc/../../etc"));
        assert!(!is_valid_agent_id("abc\\def"));
        assert!(!is_valid_agent_id("abc..def"));
    }

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

    #[test]
    fn test_session_status_as_str() {
        assert_eq!(SessionStatus::Active.as_str(), "active");
        assert_eq!(SessionStatus::Ended.as_str(), "ended");
        assert_eq!(SessionStatus::Aborted.as_str(), "aborted");
    }

    #[test]
    fn test_session_status_display() {
        assert_eq!(format!("{}", SessionStatus::Active), "active");
        assert_eq!(format!("{}", SessionStatus::Ended), "ended");
        assert_eq!(format!("{}", SessionStatus::Aborted), "aborted");
    }

    #[test]
    fn test_session_status_try_from() {
        assert_eq!(SessionStatus::try_from("active"), Ok(SessionStatus::Active));
        assert_eq!(SessionStatus::try_from("ended"), Ok(SessionStatus::Ended));
        assert_eq!(
            SessionStatus::try_from("aborted"),
            Ok(SessionStatus::Aborted)
        );
        assert!(SessionStatus::try_from("unknown").is_err());
    }

    #[test]
    fn test_session_status_serde_roundtrip() {
        let status = SessionStatus::Active;
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json, serde_json::Value::String("active".into()));
        let parsed: SessionStatus = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, SessionStatus::Active);
    }

    #[test]
    fn test_approval_status_as_str() {
        assert_eq!(ApprovalStatus::Pending.as_str(), "pending");
        assert_eq!(ApprovalStatus::Approved.as_str(), "approved");
        assert_eq!(ApprovalStatus::Denied.as_str(), "denied");
        assert_eq!(ApprovalStatus::Rejected.as_str(), "rejected");
        assert_eq!(ApprovalStatus::Expired.as_str(), "expired");
    }

    #[test]
    fn test_approval_status_display() {
        assert_eq!(format!("{}", ApprovalStatus::Pending), "pending");
        assert_eq!(format!("{}", ApprovalStatus::Approved), "approved");
    }

    #[test]
    fn test_approval_status_try_from() {
        assert_eq!(
            ApprovalStatus::try_from("pending"),
            Ok(ApprovalStatus::Pending)
        );
        assert_eq!(
            ApprovalStatus::try_from("approved"),
            Ok(ApprovalStatus::Approved)
        );
        assert_eq!(
            ApprovalStatus::try_from("denied"),
            Ok(ApprovalStatus::Denied)
        );
        assert_eq!(
            ApprovalStatus::try_from("rejected"),
            Ok(ApprovalStatus::Rejected)
        );
        assert_eq!(
            ApprovalStatus::try_from("expired"),
            Ok(ApprovalStatus::Expired)
        );
        assert!(ApprovalStatus::try_from("unknown").is_err());
    }

    #[test]
    fn test_approval_status_serde_roundtrip() {
        let status = ApprovalStatus::Approved;
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json, serde_json::Value::String("approved".into()));
        let parsed: ApprovalStatus = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, ApprovalStatus::Approved);
    }

    // ADR-013: extract_parent_session_id tests

    #[test]
    fn test_extract_parent_session_id_typical() {
        let path = "/home/user/.claude/projects/myproj/sess-abc123/subagents/agent-def456.jsonl";
        assert_eq!(
            extract_parent_session_id(path),
            Some("sess-abc123".to_string())
        );
    }

    #[test]
    fn test_extract_parent_session_id_no_subagents() {
        let path = "/home/user/.claude/projects/myproj/sess-abc123/transcript.jsonl";
        assert_eq!(extract_parent_session_id(path), None);
    }

    #[test]
    fn test_extract_parent_session_id_empty_parent() {
        // Edge case: /subagents/ at the start
        let path = "/subagents/agent-def456.jsonl";
        // parent_part = "", rsplit('/').next() = "" => None
        assert_eq!(extract_parent_session_id(path), None);
    }

    #[test]
    fn test_extract_parent_session_id_nested_path() {
        let path = "/home/user/.claude/projects/deep/nested/path/parent-id/subagents/agent-x.jsonl";
        assert_eq!(
            extract_parent_session_id(path),
            Some("parent-id".to_string())
        );
    }

    // ADR-013: extract_agent_id tests

    #[test]
    fn test_extract_agent_id_typical() {
        let path = "/home/user/.claude/projects/myproj/sess-abc123/subagents/agent-def456.jsonl";
        assert_eq!(
            extract_agent_id(path),
            Some("agent-def456".to_string())
        );
    }

    #[test]
    fn test_extract_agent_id_no_extension() {
        let path = "/some/path/sess/subagents/agent-xyz";
        assert_eq!(
            extract_agent_id(path),
            Some("agent-xyz".to_string())
        );
    }

    #[test]
    fn test_extract_agent_id_no_subagents() {
        let path = "/home/user/.claude/projects/myproj/transcript.jsonl";
        assert_eq!(extract_agent_id(path), None);
    }

    #[test]
    fn test_extract_agent_id_empty() {
        let path = "/some/path/subagents/";
        // agent_part = "", rsplit('/').next() = "" => stripped = "" => None
        assert_eq!(extract_agent_id(path), None);
    }
}
