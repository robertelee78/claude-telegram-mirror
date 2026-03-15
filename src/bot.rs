use crate::error::{AppError, Result};
use crate::types::{InlineButton, SendOptions};
use governor::{Quota, RateLimiter};
use std::num::NonZeroU32;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{
    ChatId, InlineKeyboardButton, InlineKeyboardMarkup, MessageId, ParseMode, ThreadId,
};

/// Scrub bot token from error messages to prevent credential leakage.
/// Teloxide wraps reqwest errors that include the full API URL with token.
fn scrub_telegram_error(err: &teloxide::RequestError) -> String {
    let raw = err.to_string();
    // Telegram API URLs contain the token: /bot<TOKEN>/method
    // Scrub anything that looks like a bot token (numeric:alphanumeric)
    let re = regex::Regex::new(r"bot\d+:[A-Za-z0-9_-]+/").unwrap_or_else(|_| {
        regex::Regex::new(r"$^").unwrap() // fallback: match nothing
    });
    re.replace_all(&raw, "bot<REDACTED>/").to_string()
}

/// Telegram Bot wrapper with rate limiting and forum topic support
pub struct TelegramBot {
    bot: Bot,
    chat_id: ChatId,
    rate_limiter: Arc<
        RateLimiter<
            governor::state::NotKeyed,
            governor::state::InMemoryState,
            governor::clock::DefaultClock,
        >,
    >,
}

impl TelegramBot {
    pub fn new(token: &str, chat_id: i64) -> Self {
        let bot = Bot::new(token);
        // Telegram rate limit: ~30 messages/second per chat
        let quota = Quota::per_second(NonZeroU32::new(25).unwrap());
        let rate_limiter = Arc::new(RateLimiter::direct(quota));

        Self {
            bot,
            chat_id: ChatId(chat_id),
            rate_limiter,
        }
    }

    /// Get a clone of the underlying Bot for polling
    #[allow(dead_code)]
    pub fn bot(&self) -> Bot {
        self.bot.clone()
    }

    /// Get the chat ID
    #[allow(dead_code)]
    pub fn chat_id(&self) -> ChatId {
        self.chat_id
    }

    /// Send a text message with optional thread targeting
    pub async fn send_message(
        &self,
        text: &str,
        opts: &SendOptions,
        thread_id: Option<i32>,
    ) -> Result<Message> {
        self.rate_limiter.until_ready().await;

        let effective_thread = thread_id.or(opts.thread_id);
        let mut req = self.bot.send_message(self.chat_id, text);

        if let Some(pm) = &opts.parse_mode {
            req = req.parse_mode(match pm.as_str() {
                "MarkdownV2" => ParseMode::MarkdownV2,
                "HTML" => ParseMode::Html,
                _ => ParseMode::MarkdownV2,
            });
        }

        if let Some(tid) = effective_thread {
            req = req.message_thread_id(ThreadId(MessageId(tid)));
        }

        req.await
            .map_err(|e| AppError::Telegram(scrub_telegram_error(&e)))
    }

    /// Send a message with inline keyboard buttons
    pub async fn send_with_buttons(
        &self,
        text: &str,
        buttons: &[InlineButton],
        opts: &SendOptions,
        thread_id: Option<i32>,
    ) -> Result<Message> {
        self.rate_limiter.until_ready().await;

        let keyboard_buttons: Vec<InlineKeyboardButton> = buttons
            .iter()
            .map(|b| InlineKeyboardButton::callback(b.text.clone(), b.callback_data.clone()))
            .collect();

        // Arrange buttons in rows (max 3 per row)
        let rows: Vec<Vec<InlineKeyboardButton>> = keyboard_buttons
            .chunks(3)
            .map(|chunk| chunk.to_vec())
            .collect();

        let keyboard = InlineKeyboardMarkup::new(rows);
        let effective_thread = thread_id.or(opts.thread_id);

        let mut req = self
            .bot
            .send_message(self.chat_id, text)
            .reply_markup(keyboard);

        if let Some(pm) = &opts.parse_mode {
            req = req.parse_mode(match pm.as_str() {
                "MarkdownV2" => ParseMode::MarkdownV2,
                "HTML" => ParseMode::Html,
                _ => ParseMode::MarkdownV2,
            });
        }

        if let Some(tid) = effective_thread {
            req = req.message_thread_id(ThreadId(MessageId(tid)));
        }

        req.await
            .map_err(|e| AppError::Telegram(scrub_telegram_error(&e)))
    }

    /// Create a forum topic
    pub async fn create_forum_topic(&self, name: &str) -> Result<Option<i32>> {
        self.rate_limiter.until_ready().await;

        match self
            .bot
            .create_forum_topic(self.chat_id, name, 0x6FB9F0, "")
            .await
        {
            Ok(topic) => {
                let thread_id = topic.thread_id.0 .0;
                tracing::info!(%name, %thread_id, "Forum topic created");
                Ok(Some(thread_id))
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to create forum topic");
                Ok(None)
            }
        }
    }

    /// Close a forum topic
    pub async fn close_forum_topic(&self, thread_id: i32) -> Result<bool> {
        self.rate_limiter.until_ready().await;

        match self
            .bot
            .close_forum_topic(self.chat_id, ThreadId(MessageId(thread_id)))
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::warn!(error = %e, %thread_id, "Failed to close forum topic");
                Ok(false)
            }
        }
    }

    /// Delete a forum topic
    pub async fn delete_forum_topic(&self, thread_id: i32) -> Result<bool> {
        self.rate_limiter.until_ready().await;

        match self
            .bot
            .delete_forum_topic(self.chat_id, ThreadId(MessageId(thread_id)))
            .await
        {
            Ok(_) => {
                tracing::info!(%thread_id, "Forum topic deleted");
                Ok(true)
            }
            Err(e) => {
                tracing::warn!(error = %e, %thread_id, "Failed to delete forum topic");
                Ok(false)
            }
        }
    }

    /// Unpin all messages in a forum topic
    pub async fn unpin_all_topic_messages(&self, thread_id: i32) -> Result<bool> {
        self.rate_limiter.until_ready().await;

        match self
            .bot
            .unpin_all_forum_topic_messages(self.chat_id, ThreadId(MessageId(thread_id)))
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::debug!(error = %e, "Failed to unpin topic messages");
                Ok(false)
            }
        }
    }

    /// Edit a message's text
    pub async fn edit_message_text(
        &self,
        message_id: i32,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<()> {
        self.rate_limiter.until_ready().await;

        let mut req = self
            .bot
            .edit_message_text(self.chat_id, MessageId(message_id), text);

        if let Some(pm) = parse_mode {
            req = req.parse_mode(pm);
        }

        req.await
            .map_err(|e| AppError::Telegram(scrub_telegram_error(&e)))?;
        Ok(())
    }

    /// Answer a callback query
    pub async fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<()> {
        self.rate_limiter.until_ready().await;

        let mut req = self.bot.answer_callback_query(callback_id);
        if let Some(t) = text {
            req = req.text(t);
        }

        req.await
            .map_err(|e| AppError::Telegram(scrub_telegram_error(&e)))?;
        Ok(())
    }

    /// Poll for updates (long polling)
    pub async fn get_updates(&self, offset: i64) -> Result<Vec<Update>> {
        let updates = self
            .bot
            .get_updates()
            .offset(offset as i32)
            .timeout(30)
            .await
            .map_err(|e| AppError::Telegram(scrub_telegram_error(&e)))?;

        Ok(updates)
    }
}

impl Clone for TelegramBot {
    fn clone(&self) -> Self {
        Self {
            bot: self.bot.clone(),
            chat_id: self.chat_id,
            rate_limiter: self.rate_limiter.clone(),
        }
    }
}

/// Check if text is an interrupt command (sends Escape to pause Claude)
pub fn is_interrupt_command(text: &str) -> bool {
    let normalized = text.trim().to_lowercase();
    matches!(
        normalized.as_str(),
        "stop"
            | "/stop"
            | "cancel"
            | "/cancel"
            | "abort"
            | "/abort"
            | "esc"
            | "/esc"
            | "escape"
            | "/escape"
    )
}

/// Check if text is a kill command (sends Ctrl-C to exit Claude entirely)
pub fn is_kill_command(text: &str) -> bool {
    let normalized = text.trim().to_lowercase();
    matches!(
        normalized.as_str(),
        "kill" | "/kill" | "exit" | "/exit" | "quit" | "/quit" | "ctrl+c" | "ctrl-c" | "^c"
    )
}

/// Check if text is a cc command prefix (e.g., "cc clear" -> "/clear")
pub fn parse_cc_command(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if lower.starts_with("cc ") {
        // UTF-8 safe: skip 3 chars (not bytes) to avoid panicking on multi-byte input
        let rest: String = text.chars().skip(3).collect();
        Some(format!("/{}", rest.trim()))
    } else {
        None
    }
}

/// Security fix #5: Validate chat ID on ALL updates including callbacks
pub fn is_authorized_chat(update: &Update, authorized_chat_id: i64) -> bool {
    use teloxide::types::UpdateKind;

    // Try update.chat() first (works for messages and most update types)
    if let Some(chat) = update.chat() {
        return chat.id.0 == authorized_chat_id;
    }

    // For callback queries, extract chat from the associated message
    if let UpdateKind::CallbackQuery(query) = &update.kind {
        if let Some(msg) = &query.message {
            return msg.chat().id.0 == authorized_chat_id;
        }
    }

    false
}
