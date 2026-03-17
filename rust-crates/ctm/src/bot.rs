//! Telegram Bot API client using reqwest.
//!
//! Thin wrapper around the Bot API with:
//! - Rate limiting via `governor`
//! - Message queue with retry + exponential backoff (3 retries, 1s/2s/4s)
//! - TOPIC_CLOSED recovery (reopen + retry)
//! - Entity parse error fallback (strip formatting, retry as plain text)
//! - Bot token scrubbing on all error messages
//!
//! Ported from `telegram.ts`.

use crate::config::Config;
use crate::error::{AppError, Result};
use crate::formatting::chunk_message;
use governor::{Quota, RateLimiter};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use std::sync::{Arc, LazyLock};
use tokio::sync::Mutex;

static BOT_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"bot\d+:[A-Za-z0-9_-]+/").unwrap());

// ---------------------------------------------------------------- types

#[derive(Debug, Clone, Serialize)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}

/// Options for sending messages.
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    pub parse_mode: Option<String>,
    pub disable_notification: Option<bool>,
}

/// A queued message waiting to be sent.
#[derive(Debug, Clone)]
struct QueuedMessage {
    chat_id: i64,
    text: String,
    thread_id: Option<i64>,
    buttons: Option<Vec<InlineButton>>,
    parse_mode: Option<String>,
    disable_notification: Option<bool>,
    retries: u32,
}

/// Telegram API response wrapper.
#[derive(Debug, Deserialize)]
struct TgResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
    error_code: Option<i32>,
}

/// Forum topic creation result.
#[derive(Debug, Deserialize)]
struct ForumTopicResult {
    message_thread_id: i64,
}

/// getMe result.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct BotUser {
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
#[allow(dead_code)]
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
    pub file_unique_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TgDocument {
    pub file_id: String,
    pub file_unique_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct CallbackQuery {
    pub id: String,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub message: Option<TgMessage>,
    #[serde(default)]
    pub from: Option<TgUser>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TgUser {
    pub id: i64,
}

#[derive(Debug, Deserialize)]
struct TgFile {
    #[serde(default)]
    file_path: Option<String>,
}

// Valid forum topic icon colors (Telegram API requirement)
const TOPIC_COLORS: [i64; 6] = [
    0x6FB9F0, // Blue
    0xFFD67E, // Yellow
    0xCB86DB, // Purple
    0x8EEE98, // Green
    0xFF93B2, // Pink
    0xFB6F5F, // Red
];

// ---------------------------------------------------------------- bot

/// Telegram Bot with rate limiting and message queue.
pub struct TelegramBot {
    token: String,
    chat_id: i64,
    client: Client,
    rate_limiter: Arc<
        RateLimiter<
            governor::state::NotKeyed,
            governor::state::InMemoryState,
            governor::clock::DefaultClock,
        >,
    >,
    queue: Arc<Mutex<Vec<QueuedMessage>>>,
    queue_processing: Arc<Mutex<bool>>,
    chunk_size: usize,
}

#[allow(dead_code)]
impl TelegramBot {
    pub fn new(config: &Config) -> Self {
        // Rate limiter: 25 msgs/sec bucket
        let quota = Quota::per_second(NonZeroU32::new(25).unwrap());
        let limiter = RateLimiter::direct(quota);

        Self {
            token: config.bot_token.clone(),
            chat_id: config.chat_id,
            client: Client::new(),
            rate_limiter: Arc::new(limiter),
            queue: Arc::new(Mutex::new(Vec::new())),
            queue_processing: Arc::new(Mutex::new(false)),
            chunk_size: config.chunk_size,
        }
    }

    /// Get the configured chat ID.
    pub fn chat_id(&self) -> i64 {
        self.chat_id
    }

    /// Scrub the bot token from error messages.
    pub fn scrub_token(&self, text: &str) -> String {
        scrub_bot_token(text)
    }

    // -------------------------------------------------------------- API

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }

    /// Call a Telegram Bot API method.
    async fn api_call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<TgResponse<T>> {
        self.rate_limiter.until_ready().await;
        let resp = self
            .client
            .post(self.api_url(method))
            .json(body)
            .send()
            .await
            .map_err(|e| AppError::Telegram(self.scrub_token(&e.to_string())))?;

        let tg: TgResponse<T> = resp
            .json()
            .await
            .map_err(|e| AppError::Telegram(self.scrub_token(&e.to_string())))?;

        Ok(tg)
    }

    /// Verify bot connectivity.
    pub async fn get_me(&self) -> Result<BotUser> {
        let resp: TgResponse<BotUser> = self.api_call("getMe", &serde_json::json!({})).await?;
        resp.result.ok_or_else(|| {
            AppError::Telegram(resp.description.unwrap_or_else(|| "getMe failed".into()))
        })
    }

    // ---------------------------------------------------------- sending

    /// Send a message (queued with rate limiting + chunking).
    pub async fn send_message(
        &self,
        text: &str,
        options: Option<&SendOptions>,
        thread_id: Option<i64>,
    ) {
        let chunks = chunk_message(text, self.chunk_size);
        let parse_mode = options
            .and_then(|o| o.parse_mode.clone())
            .or_else(|| Some("Markdown".into()));
        let disable_notification = options.and_then(|o| o.disable_notification);

        for chunk in chunks {
            self.enqueue(QueuedMessage {
                chat_id: self.chat_id,
                text: chunk,
                thread_id,
                buttons: None,
                parse_mode: parse_mode.clone(),
                disable_notification,
                retries: 0,
            })
            .await;
        }
    }

    /// Send a message with inline keyboard buttons.
    pub async fn send_with_buttons(
        &self,
        text: &str,
        buttons: Vec<InlineButton>,
        options: Option<&SendOptions>,
        thread_id: Option<i64>,
    ) {
        let parse_mode = options
            .and_then(|o| o.parse_mode.clone())
            .or_else(|| Some("Markdown".into()));
        let disable_notification = options.and_then(|o| o.disable_notification);

        self.enqueue(QueuedMessage {
            chat_id: self.chat_id,
            text: text.to_string(),
            thread_id,
            buttons: Some(buttons),
            parse_mode,
            disable_notification,
            retries: 0,
        })
        .await;
    }

    /// Enqueue a message and start processing if not already running.
    async fn enqueue(&self, msg: QueuedMessage) {
        self.queue.lock().await.push(msg);
        self.process_queue().await;
    }

    /// Process the message queue with retry logic.
    async fn process_queue(&self) {
        {
            let mut processing = self.queue_processing.lock().await;
            if *processing {
                return;
            }
            *processing = true;
        }

        loop {
            let item = {
                let mut q = self.queue.lock().await;
                if q.is_empty() {
                    break;
                }
                q.remove(0)
            };

            match self.send_item(&item).await {
                Ok(()) => {}
                Err(e) => {
                    let err_str = self.scrub_token(&e.to_string());
                    if item.retries < 3 {
                        let mut retry = item.clone();
                        retry.retries += 1;
                        let delay_ms = 1000 * (1u64 << retry.retries);
                        tracing::warn!(
                            retries = retry.retries,
                            delay_ms,
                            error = %err_str,
                            "Message send failed, retrying"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                        let mut q = self.queue.lock().await;
                        q.insert(0, retry);
                    } else {
                        tracing::error!(error = %err_str, "Failed to send message after 3 retries");
                    }
                }
            }
        }

        *self.queue_processing.lock().await = false;
    }

    /// Actually send a single queued message to Telegram.
    async fn send_item(&self, item: &QueuedMessage) -> Result<()> {
        let mut body = serde_json::json!({
            "chat_id": item.chat_id,
            "text": item.text,
        });

        if let Some(pm) = &item.parse_mode {
            body["parse_mode"] = serde_json::Value::String(pm.clone());
        }
        if let Some(dn) = item.disable_notification {
            body["disable_notification"] = serde_json::Value::Bool(dn);
        }
        if let Some(tid) = item.thread_id {
            body["message_thread_id"] = serde_json::Value::Number(tid.into());
        }
        if let Some(buttons) = &item.buttons {
            let keyboard = build_inline_keyboard(buttons);
            body["reply_markup"] = keyboard;
        }

        let resp: TgResponse<TgMessage> = self.api_call("sendMessage", &body).await?;

        if resp.ok {
            return Ok(());
        }

        let desc = resp.description.unwrap_or_default();
        let code = resp.error_code.unwrap_or(0);

        // TOPIC_CLOSED: reopen topic, retry send
        if code == 400 && desc.contains("TOPIC_CLOSED") {
            if let Some(tid) = item.thread_id {
                tracing::info!(thread_id = tid, "Topic was closed, attempting to reopen");
                if self.reopen_forum_topic(tid).await? {
                    // Send reopened notification
                    let _ = self
                        .api_call::<TgMessage>(
                            "sendMessage",
                            &serde_json::json!({
                                "chat_id": item.chat_id,
                                "text": "Topic reopened",
                                "message_thread_id": tid,
                                "disable_notification": true,
                            }),
                        )
                        .await;

                    // Retry the original message
                    let retry_resp: TgResponse<TgMessage> =
                        self.api_call("sendMessage", &body).await?;
                    if retry_resp.ok {
                        return Ok(());
                    }
                }
                tracing::error!(thread_id = tid, "Failed to reopen topic");
            }
        }

        // Entity parse error: strip formatting, retry as plain text
        if code == 400 && desc.contains("can't parse entities") {
            tracing::warn!("Markdown parsing failed, retrying as plain text");
            let plain_text = strip_markdown(&item.text);
            let mut plain_body = serde_json::json!({
                "chat_id": item.chat_id,
                "text": plain_text,
            });
            if let Some(dn) = item.disable_notification {
                plain_body["disable_notification"] = serde_json::Value::Bool(dn);
            }
            if let Some(tid) = item.thread_id {
                plain_body["message_thread_id"] = serde_json::Value::Number(tid.into());
            }
            if let Some(buttons) = &item.buttons {
                plain_body["reply_markup"] = build_inline_keyboard(buttons);
            }

            let plain_resp: TgResponse<TgMessage> =
                self.api_call("sendMessage", &plain_body).await?;
            if plain_resp.ok {
                return Ok(());
            }
        }

        Err(AppError::Telegram(self.scrub_token(&desc)))
    }

    // -------------------------------------------------------- forum topics

    /// Create a forum topic. Returns the thread_id, or None if not supported.
    pub async fn create_forum_topic(&self, name: &str, color_index: usize) -> Result<Option<i64>> {
        let icon_color = TOPIC_COLORS[color_index % TOPIC_COLORS.len()];
        let resp: TgResponse<ForumTopicResult> = self
            .api_call(
                "createForumTopic",
                &serde_json::json!({
                    "chat_id": self.chat_id,
                    "name": name,
                    "icon_color": icon_color,
                }),
            )
            .await?;

        if let Some(result) = resp.result {
            tracing::info!(
                name,
                thread_id = result.message_thread_id,
                "Created forum topic"
            );
            Ok(Some(result.message_thread_id))
        } else {
            tracing::debug!("Forum topics not supported");
            Ok(None)
        }
    }

    /// Close a forum topic.
    pub async fn close_forum_topic(&self, thread_id: i64) -> Result<bool> {
        let resp: TgResponse<bool> = self
            .api_call(
                "closeForumTopic",
                &serde_json::json!({
                    "chat_id": self.chat_id,
                    "message_thread_id": thread_id,
                }),
            )
            .await?;
        Ok(resp.ok)
    }

    /// Reopen a closed forum topic.
    pub async fn reopen_forum_topic(&self, thread_id: i64) -> Result<bool> {
        let resp: TgResponse<bool> = self
            .api_call(
                "reopenForumTopic",
                &serde_json::json!({
                    "chat_id": self.chat_id,
                    "message_thread_id": thread_id,
                }),
            )
            .await?;
        Ok(resp.ok)
    }

    /// Delete a forum topic entirely.
    pub async fn delete_forum_topic(&self, thread_id: i64) -> Result<bool> {
        let resp: TgResponse<bool> = self
            .api_call(
                "deleteForumTopic",
                &serde_json::json!({
                    "chat_id": self.chat_id,
                    "message_thread_id": thread_id,
                }),
            )
            .await?;
        Ok(resp.ok)
    }

    /// Edit a forum topic's name.
    pub async fn edit_forum_topic(&self, thread_id: i64, name: &str) -> Result<bool> {
        let resp: TgResponse<bool> = self
            .api_call(
                "editForumTopic",
                &serde_json::json!({
                    "chat_id": self.chat_id,
                    "message_thread_id": thread_id,
                    "name": name,
                }),
            )
            .await?;
        Ok(resp.ok)
    }

    /// Unpin all messages in a forum topic.
    pub async fn unpin_all_topic_messages(&self, thread_id: i64) -> Result<bool> {
        let resp: TgResponse<bool> = self
            .api_call(
                "unpinAllForumTopicMessages",
                &serde_json::json!({
                    "chat_id": self.chat_id,
                    "message_thread_id": thread_id,
                }),
            )
            .await?;
        Ok(resp.ok)
    }

    // -------------------------------------------------------- edit/remove

    /// Edit a message's text.
    ///
    /// `parse_mode` — pass `Some("Markdown")` to keep formatting, `None` for plain text
    /// (safer when the edited content may contain underscores or other Markdown special chars).
    pub async fn edit_message(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        parse_mode: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
        });
        if let Some(pm) = parse_mode {
            body["parse_mode"] = serde_json::Value::String(pm.to_string());
        }
        let _: TgResponse<TgMessage> = self.api_call("editMessageText", &body).await?;
        Ok(())
    }

    /// Remove inline keyboard from a message.
    pub async fn remove_keyboard(&self, message_id: i64, _thread_id: Option<i64>) -> Result<()> {
        let _: TgResponse<TgMessage> = self
            .api_call(
                "editMessageReplyMarkup",
                &serde_json::json!({
                    "chat_id": self.chat_id,
                    "message_id": message_id,
                }),
            )
            .await?;
        Ok(())
    }

    /// Edit a message's inline keyboard (reply markup).
    pub async fn edit_message_reply_markup(
        &self,
        message_id: i64,
        buttons: &[InlineButton],
    ) -> Result<()> {
        let keyboard = build_inline_keyboard(buttons);
        let _: TgResponse<TgMessage> = self
            .api_call(
                "editMessageReplyMarkup",
                &serde_json::json!({
                    "chat_id": self.chat_id,
                    "message_id": message_id,
                    "reply_markup": keyboard,
                }),
            )
            .await?;
        Ok(())
    }

    /// Edit a message's text and remove keyboard (for "Selected" / "Submitted" feedback).
    pub async fn edit_message_text_no_markup(
        &self,
        message_id: i64,
        text: &str,
        thread_id: Option<i64>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "chat_id": self.chat_id,
            "message_id": message_id,
            "text": text,
        });
        if let Some(tid) = thread_id {
            body["message_thread_id"] = serde_json::Value::Number(tid.into());
        }
        let _: TgResponse<TgMessage> = self.api_call("editMessageText", &body).await?;
        Ok(())
    }

    /// Send a message and return the sent message (for ping latency measurement).
    pub async fn send_message_returning(
        &self,
        text: &str,
        options: Option<&SendOptions>,
        thread_id: Option<i64>,
    ) -> Result<TgMessage> {
        let parse_mode = options
            .and_then(|o| o.parse_mode.clone())
            .or_else(|| Some("Markdown".into()));

        let mut body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
        });
        if let Some(pm) = parse_mode {
            body["parse_mode"] = serde_json::Value::String(pm);
        }
        if let Some(tid) = thread_id {
            body["message_thread_id"] = serde_json::Value::Number(tid.into());
        }

        let resp: TgResponse<TgMessage> = self.api_call("sendMessage", &body).await?;
        resp.result.ok_or_else(|| {
            AppError::Telegram(
                resp.description
                    .unwrap_or_else(|| "sendMessage failed".into()),
            )
        })
    }

    /// Send a message as a reply to another message.
    pub async fn send_message_reply_to(
        &self,
        text: &str,
        reply_to_message_id: i64,
        options: Option<&SendOptions>,
        thread_id: Option<i64>,
    ) -> Result<()> {
        let parse_mode = options
            .and_then(|o| o.parse_mode.clone())
            .or_else(|| Some("Markdown".into()));

        let mut body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "reply_parameters": {
                "message_id": reply_to_message_id,
            },
        });
        if let Some(pm) = parse_mode {
            body["parse_mode"] = serde_json::Value::String(pm);
        }
        if let Some(tid) = thread_id {
            body["message_thread_id"] = serde_json::Value::Number(tid.into());
        }

        let _: TgResponse<TgMessage> = self.api_call("sendMessage", &body).await?;
        Ok(())
    }

    // -------------------------------------------------------- file download

    /// Download a file from Telegram to a local path.
    pub async fn download_file(&self, file_id: &str, dest_path: &str) -> Result<Option<String>> {
        // Step 1: getFile
        let resp: TgResponse<TgFile> = self
            .api_call("getFile", &serde_json::json!({"file_id": file_id}))
            .await?;

        let tg_file = match resp.result {
            Some(f) => f,
            None => return Ok(None),
        };

        let file_path = match &tg_file.file_path {
            Some(p) => p,
            None => return Ok(None),
        };

        // Step 2: Download from Telegram file server
        let url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.token, file_path
        );
        let download_resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::Telegram(self.scrub_token(&e.to_string())))?;

        if !download_resp.status().is_success() {
            tracing::warn!(
                file_id,
                status = %download_resp.status(),
                "File download HTTP error"
            );
            return Ok(None);
        }

        let bytes = download_resp
            .bytes()
            .await
            .map_err(|e| AppError::Telegram(self.scrub_token(&e.to_string())))?;

        // Step 3: Write to disk with restrictive permissions
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(dest_path)?;

        use std::io::Write;
        file.write_all(&bytes)?;
        tracing::info!(file_id, dest_path, size = bytes.len(), "File downloaded");
        Ok(Some(dest_path.to_string()))
    }

    // -------------------------------------------------------- long polling

    /// Get updates via long polling.
    pub async fn get_updates(&self, offset: i64) -> Result<Vec<Update>> {
        let resp: TgResponse<Vec<Update>> = self
            .api_call(
                "getUpdates",
                &serde_json::json!({
                    "offset": offset,
                    "timeout": 30,
                    "allowed_updates": ["message", "callback_query"],
                }),
            )
            .await?;

        Ok(resp.result.unwrap_or_default())
    }

    /// Answer a callback query (dismiss the loading spinner).
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({"callback_query_id": callback_query_id});
        if let Some(t) = text {
            body["text"] = serde_json::Value::String(t.to_string());
        }
        let _: TgResponse<bool> = self.api_call("answerCallbackQuery", &body).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------- helpers

/// Build an inline keyboard JSON structure from our button list.
/// Layout: two buttons per row, matching TypeScript's `if (idx + 1) % 2 === 0 keyboard.row()`.
fn build_inline_keyboard(buttons: &[InlineButton]) -> serde_json::Value {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut current_row: Vec<serde_json::Value> = Vec::new();

    for (idx, btn) in buttons.iter().enumerate() {
        current_row.push(serde_json::json!({
            "text": btn.text,
            "callback_data": btn.callback_data,
        }));
        // Start a new row after every 2nd button
        if (idx + 1) % 2 == 0 {
            rows.push(serde_json::Value::Array(current_row));
            current_row = Vec::new();
        }
    }
    // Flush any remaining button(s) in the last row
    if !current_row.is_empty() {
        rows.push(serde_json::Value::Array(current_row));
    }

    serde_json::json!({"inline_keyboard": rows})
}

/// Strip markdown formatting for plain text fallback.
fn strip_markdown(text: &str) -> String {
    text.replace("**", "")
        .replace('*', "")
        .replace('`', "'")
        .replace('_', "")
}

/// Scrub bot token from log messages to prevent leaking.
///
/// Applies a regex `bot\d+:[A-Za-z0-9_-]+/` globally, matching any Telegram
/// API URL token regardless of whether the runtime token is known. This matches
/// the TypeScript winston format pipeline behavior.
pub fn scrub_bot_token(text: &str) -> String {
    BOT_TOKEN_REGEX
        .replace_all(text, "bot[REDACTED]/")
        .into_owned()
}

// ===================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_markdown() {
        assert_eq!(strip_markdown("**bold**"), "bold");
        assert_eq!(strip_markdown("*italic*"), "italic");
        assert_eq!(strip_markdown("`code`"), "'code'");
        assert_eq!(strip_markdown("_under_"), "under");
    }

    #[test]
    fn test_scrub_bot_token() {
        let token = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";
        let msg = format!("Error: bot{}/sendMessage failed", token);
        let scrubbed = scrub_bot_token(&msg);
        assert!(!scrubbed.contains(token));
        assert!(scrubbed.contains("[REDACTED]"));
    }

    #[test]
    fn test_scrub_bot_token_regex_no_literal_needed() {
        // The regex approach scrubs any token pattern without needing the runtime token.
        let msg = "POST https://api.telegram.org/bot987654321:XyZ-abc_123DEF456/sendMessage";
        let scrubbed = scrub_bot_token(msg);
        assert!(!scrubbed.contains("987654321:XyZ-abc_123DEF456"));
        assert!(scrubbed.contains("bot[REDACTED]/sendMessage"));
    }

    #[test]
    fn test_scrub_bot_token_no_token_in_text() {
        let msg = "some error message without any token";
        assert_eq!(scrub_bot_token(msg), msg);
    }

    #[test]
    fn test_scrub_bot_token_multiple_occurrences() {
        let msg = "bot111:AAA_bbb-ccc/getMe and bot222:DDD_eee-fff/sendMessage";
        let scrubbed = scrub_bot_token(msg);
        assert!(!scrubbed.contains("111:AAA_bbb-ccc"));
        assert!(!scrubbed.contains("222:DDD_eee-fff"));
        assert_eq!(
            scrubbed,
            "bot[REDACTED]/getMe and bot[REDACTED]/sendMessage"
        );
    }

    #[test]
    fn test_build_inline_keyboard_two_per_row() {
        let buttons = vec![
            InlineButton {
                text: "Approve".into(),
                callback_data: "approve:1".into(),
            },
            InlineButton {
                text: "Reject".into(),
                callback_data: "reject:1".into(),
            },
        ];
        let kb = build_inline_keyboard(&buttons);
        let rows = kb["inline_keyboard"].as_array().unwrap();
        // Two buttons should be in a single row
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_build_inline_keyboard_three_buttons() {
        let buttons = vec![
            InlineButton {
                text: "A".into(),
                callback_data: "a".into(),
            },
            InlineButton {
                text: "B".into(),
                callback_data: "b".into(),
            },
            InlineButton {
                text: "C".into(),
                callback_data: "c".into(),
            },
        ];
        let kb = build_inline_keyboard(&buttons);
        let rows = kb["inline_keyboard"].as_array().unwrap();
        // First row has 2 buttons, second row has 1
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].as_array().unwrap().len(), 2);
        assert_eq!(rows[1].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_topic_colors() {
        assert_eq!(TOPIC_COLORS.len(), 6);
        // Blue first
        assert_eq!(TOPIC_COLORS[0], 0x6FB9F0);
    }
}
