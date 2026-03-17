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
}

/// Telegram API response wrapper.
#[derive(Debug, Deserialize)]
pub(super) struct TgResponse<T> {
    pub(super) ok: bool,
    pub(super) result: Option<T>,
    pub(super) description: Option<String>,
    pub(super) error_code: Option<i32>,
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
