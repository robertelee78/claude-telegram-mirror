use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============ Bridge Message Types ============

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMessage {
    #[serde(rename = "type")]
    pub msg_type: MessageType,
    pub session_id: String,
    pub timestamp: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

impl BridgeMessage {
    pub fn new(msg_type: MessageType, session_id: &str, content: &str) -> Self {
        Self {
            msg_type,
            session_id: session_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            content: content.to_string(),
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Map<String, serde_json::Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn get_metadata_str(&self, key: &str) -> Option<&str> {
        self.metadata
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_str())
    }

    pub fn get_metadata_value(&self, key: &str) -> Option<&serde_json::Value> {
        self.metadata.as_ref().and_then(|m| m.get(key))
    }
}

// ============ Session Types ============

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    Ended,
    Aborted,
}

impl SessionStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Active => "active",
            Self::Ended => "ended",
            Self::Aborted => "aborted",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "ended" => Self::Ended,
            "aborted" => Self::Aborted,
            _ => Self::Ended,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub chat_id: i64,
    pub thread_id: Option<i64>,
    pub hostname: Option<String>,
    pub project_dir: Option<String>,
    pub tmux_target: Option<String>,
    pub tmux_socket: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub status: SessionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
}

impl ApprovalStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            "expired" => Self::Expired,
            _ => Self::Expired,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub id: String,
    pub session_id: String,
    pub prompt: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub status: ApprovalStatus,
}

// ============ Hook Event Types ============

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "hook_event_name")]
pub enum HookEvent {
    Stop {
        session_id: String,
        #[serde(default)]
        transcript_path: Option<String>,
        #[serde(default)]
        transcript_summary: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
    },
    SubagentStop {
        session_id: String,
    },
    PreToolUse {
        session_id: String,
        tool_name: String,
        #[serde(default)]
        tool_input: HashMap<String, serde_json::Value>,
        #[serde(default)]
        tool_use_id: Option<String>,
        #[serde(default)]
        hook_id: Option<String>,
        #[serde(default)]
        permission_mode: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
    },
    PostToolUse {
        session_id: String,
        tool_name: String,
        #[serde(default)]
        tool_input: HashMap<String, serde_json::Value>,
        #[serde(default)]
        tool_output: Option<String>,
        #[serde(default)]
        tool_error: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
    },
    Notification {
        session_id: String,
        message: String,
        #[serde(default)]
        level: Option<String>,
        #[serde(default)]
        notification_type: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
    },
    UserPromptSubmit {
        session_id: String,
        prompt: String,
        #[serde(default)]
        timestamp: Option<String>,
    },
    PreCompact {
        session_id: String,
        #[serde(default)]
        trigger: Option<String>,
        #[serde(default)]
        custom_instructions: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
    },
}

impl HookEvent {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Stop { session_id, .. }
            | Self::SubagentStop { session_id, .. }
            | Self::PreToolUse { session_id, .. }
            | Self::PostToolUse { session_id, .. }
            | Self::Notification { session_id, .. }
            | Self::UserPromptSubmit { session_id, .. }
            | Self::PreCompact { session_id, .. } => session_id,
        }
    }
}

// ============ Hook Output Types ============

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    pub hook_specific_output: HookSpecificOutput,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSpecificOutput {
    pub hook_event_name: String,
    pub permission_decision: String,
    pub permission_decision_reason: String,
}

// ============ Telegram Types ============

#[derive(Debug, Clone)]
pub struct SendOptions {
    pub parse_mode: Option<String>,
    pub thread_id: Option<i32>,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            parse_mode: Some("Markdown".to_string()),
            thread_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}

// ============ Allowed tmux keys (whitelist) ============

pub const ALLOWED_TMUX_KEYS: &[&str] = &[
    "Enter", "Escape", "Tab", "C-c", "C-u", "C-d", "C-l",
    "Up", "Down", "Left", "Right", "BSpace",
];
