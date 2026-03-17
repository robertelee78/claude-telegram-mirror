//! All Telegram API response/request types.

use serde::{Deserialize, Serialize};

/// L4.5: Single canonical definition of InlineButton. Previously duplicated
/// in types.rs — now lives only here and is re-exported via lib.rs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}

/// Options for sending messages.
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    pub parse_mode: Option<String>,
    pub disable_notification: Option<bool>,
    /// L3.1: When set, the message is sent to this forum thread.
    /// This provides an alternative to passing `thread_id` as a separate parameter.
    #[allow(dead_code)] // Library API
    pub thread_id: Option<i64>,
    /// When set, the sent message will be a reply to this message ID.
    /// This allows callers to use `SendOptions` instead of the separate
    /// `send_message_reply_to` method for reply threading.
    pub reply_to_message_id: Option<i64>,
}

/// Message priority tier for the three-tier priority queue.
///
/// Critical messages are drained first; Low messages are drained last and
/// dropped first when their sub-queue overflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)] // Critical and Low are used in tests and future callers
pub(super) enum MessagePriority {
    /// User-blocking or safety-critical (approvals, session start/end, errors).
    /// Drained first. Should never hit the cap in normal operation.
    Critical = 0,
    /// User-visible output (agent responses, tool results). Drained second.
    Normal = 1,
    /// Informational only (tool previews, turn-complete). Drained last, dropped first.
    Low = 2,
}

/// A queued message waiting to be sent.
#[derive(Debug, Clone)]
pub(super) struct QueuedMessage {
    pub(super) chat_id: i64,
    pub(super) text: String,
    pub(super) thread_id: Option<i64>,
    pub(super) buttons: Option<Vec<InlineButton>>,
    pub(super) parse_mode: Option<String>,
    pub(super) disable_notification: Option<bool>,
    pub(super) reply_to_message_id: Option<i64>,
    pub(super) retries: u32,
    /// Epoch millis when this item was first enqueued (for staleness tracking).
    #[allow(dead_code)] // Tracked for future staleness-based queue eviction
    pub(super) created_at: u64,
    /// Priority tier for queue ordering. Defaults to Normal.
    pub(super) priority: MessagePriority,
}

/// Telegram API response parameters — present in certain error responses.
///
/// Included in 429 (rate-limited) responses since Bot API 8.0.
#[derive(Debug, Deserialize, Default)]
pub(super) struct ResponseParameters {
    /// Seconds to wait before retrying. Present on 429 responses.
    #[serde(default)]
    pub(super) retry_after: Option<u64>,
    /// Millisecond-precision adaptive retry (Bot API 8.0+, November 2025).
    /// More granular than `retry_after`. Use this when available.
    #[serde(default)]
    pub(super) adaptive_retry: Option<u64>,
    /// Chat migration target. Present when a group was upgraded to a supergroup.
    #[serde(default)]
    #[allow(dead_code)] // Present in Telegram API responses for future use
    pub(super) migrate_to_chat_id: Option<i64>,
}

/// Telegram API response wrapper.
#[derive(Debug, Deserialize)]
pub(super) struct TgResponse<T> {
    pub(super) ok: bool,
    pub(super) result: Option<T>,
    pub(super) description: Option<String>,
    pub(super) error_code: Option<i32>,
    /// Present in error responses when the API has additional retry metadata.
    #[serde(default)]
    pub(super) parameters: Option<ResponseParameters>,
}

/// Forum topic creation result.
#[derive(Debug, Deserialize)]
pub(super) struct ForumTopicResult {
    pub(super) message_thread_id: i64,
}

/// getMe result.
#[derive(Debug, Deserialize)]
pub struct BotUser {
    #[allow(dead_code)] // Deserialized from JSON
    pub id: i64,
    pub username: Option<String>,
}

/// Telegram Update.
#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TgMessage>,
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
}

/// Telegram Message.
#[derive(Debug, Deserialize)]
pub struct TgMessage {
    pub message_id: i64,
    pub chat: TgChat,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    #[serde(default)]
    pub photo: Option<Vec<PhotoSize>>,
    #[serde(default)]
    pub document: Option<TgDocument>,
}

#[derive(Debug, Deserialize)]
pub struct TgChat {
    pub id: i64,
}

#[derive(Debug, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    #[allow(dead_code)] // Deserialized from JSON
    pub file_unique_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct TgDocument {
    pub file_id: String,
    #[allow(dead_code)] // Deserialized from JSON
    pub file_unique_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub message: Option<TgMessage>,
    #[serde(default)]
    #[allow(dead_code)] // Deserialized from JSON
    pub from: Option<TgUser>,
}

#[derive(Debug, Deserialize)]
pub struct TgUser {
    #[allow(dead_code)] // Deserialized from JSON
    pub id: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct TgFile {
    #[serde(default)]
    pub(super) file_path: Option<String>,
}

// Valid forum topic icon colors (Telegram API requirement)
pub(super) const TOPIC_COLORS: [i64; 6] = [
    0x6FB9F0, // Blue
    0xFFD67E, // Yellow
    0xCB86DB, // Purple
    0x8EEE98, // Green
    0xFF93B2, // Pink
    0xFB6F5F, // Red
];
