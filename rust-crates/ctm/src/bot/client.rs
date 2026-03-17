//! TelegramBot struct and HTTP API methods.

use super::*;

/// Telegram Bot with rate limiting and message queue.
pub struct TelegramBot {
    pub(super) token: String,
    pub(super) chat_id: i64,
    pub(super) client: Client,
    pub(super) rate_limiter: Arc<
        RateLimiter<
            governor::state::NotKeyed,
            governor::state::InMemoryState,
            governor::clock::DefaultClock,
        >,
    >,
    pub(super) queue: Arc<Mutex<VecDeque<QueuedMessage>>>,
    pub(super) queue_processing: Arc<Mutex<bool>>,
    pub(super) chunk_size: usize,
    #[allow(dead_code)] // Library API
    pub(super) running: Arc<AtomicBool>,
}

impl TelegramBot {
    pub fn new(config: &Config) -> Result<Self> {
        // Rate limiter: use config.rate_limit msgs/sec (default 20 if 0)
        let rate = if config.rate_limit == 0 {
            20
        } else {
            config.rate_limit
        };
        let quota = Quota::per_second(NonZeroU32::new(rate).unwrap());
        let limiter = RateLimiter::direct(quota);

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| AppError::Telegram(format!("Failed to build reqwest Client: {e}")))?;

        Ok(Self {
            token: config.bot_token.clone(),
            chat_id: config.chat_id,
            client,
            rate_limiter: Arc::new(limiter),
            queue: Arc::new(Mutex::new(VecDeque::new())),
            queue_processing: Arc::new(Mutex::new(false)),
            chunk_size: config.chunk_size,
            running: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Get the configured chat ID.
    pub fn chat_id(&self) -> i64 {
        self.chat_id
    }

    /// Whether the bot is currently running (started and not yet stopped).
    #[allow(dead_code)] // Library API
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Mark the bot as running. Called when the daemon starts.
    #[allow(dead_code)] // Library API
    pub fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::Relaxed);
    }

    /// Get session state for a chat. Returns `None` — session state is managed
    /// by the daemon layer (`BotSessionState`), not the bot itself. This stub
    /// matches the TS `getSession()` interface for API compatibility.
    #[allow(dead_code)] // Library API
    pub fn get_session(&self, _chat_id: i64) -> Option<()> {
        None
    }

    /// Scrub the bot token from error messages.
    pub fn scrub_token(&self, text: &str) -> String {
        scrub_bot_token(text)
    }

    // -------------------------------------------------------------- API

    pub(super) fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }

    /// Call a Telegram Bot API method.
    pub(super) async fn api_call<T: serde::de::DeserializeOwned>(
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
    ///
    /// If `options.reply_to_message_id` is set, the first chunk will be sent as a
    /// reply to that message.  Subsequent chunks (if the message is split) are sent
    /// without the reply parameter to avoid cluttering the thread.
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
        let reply_to_message_id = options.and_then(|o| o.reply_to_message_id);

        for (idx, chunk) in chunks.into_iter().enumerate() {
            // Only apply reply_to on the first chunk; subsequent parts read naturally.
            let reply_id = if idx == 0 { reply_to_message_id } else { None };
            self.enqueue(QueuedMessage {
                chat_id: self.chat_id,
                text: chunk,
                thread_id,
                buttons: None,
                parse_mode: parse_mode.clone(),
                disable_notification,
                reply_to_message_id: reply_id,
                retries: 0,
                created_at: epoch_millis(),
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
        let reply_to_message_id = options.and_then(|o| o.reply_to_message_id);

        self.enqueue(QueuedMessage {
            chat_id: self.chat_id,
            text: text.to_string(),
            thread_id,
            buttons: Some(buttons),
            parse_mode,
            disable_notification,
            reply_to_message_id,
            retries: 0,
            created_at: epoch_millis(),
        })
        .await;
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
    #[allow(dead_code)] // Library API
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

    // -------------------------------------------------------- file upload

    /// Upload a photo to Telegram.
    pub async fn send_photo(
        &self,
        path: &std::path::Path,
        caption: Option<&str>,
        thread_id: Option<i64>,
    ) -> Result<()> {
        self.rate_limiter.until_ready().await;

        let file_name = sanitize_upload_filename(path);
        let file_bytes = std::fs::read(path)?;
        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", self.chat_id.to_string())
            .part("photo", part);

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }
        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        let resp = self
            .client
            .post(self.api_url("sendPhoto"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AppError::Telegram(self.scrub_token(&e.to_string())))?;

        let tg: TgResponse<TgMessage> = resp
            .json()
            .await
            .map_err(|e| AppError::Telegram(self.scrub_token(&e.to_string())))?;

        if !tg.ok {
            return Err(AppError::Telegram(
                tg.description.unwrap_or_else(|| "sendPhoto failed".into()),
            ));
        }
        Ok(())
    }

    /// Upload a document to Telegram.
    pub async fn send_document(
        &self,
        path: &std::path::Path,
        caption: Option<&str>,
        thread_id: Option<i64>,
    ) -> Result<()> {
        self.rate_limiter.until_ready().await;

        let file_name = sanitize_upload_filename(path);
        let file_bytes = std::fs::read(path)?;
        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", self.chat_id.to_string())
            .part("document", part);

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }
        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        let resp = self
            .client
            .post(self.api_url("sendDocument"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AppError::Telegram(self.scrub_token(&e.to_string())))?;

        let tg: TgResponse<TgMessage> = resp
            .json()
            .await
            .map_err(|e| AppError::Telegram(self.scrub_token(&e.to_string())))?;

        if !tg.ok {
            return Err(AppError::Telegram(
                tg.description
                    .unwrap_or_else(|| "sendDocument failed".into()),
            ));
        }
        Ok(())
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
    ///
    /// H4.1: `show_alert` controls whether the response is shown as a toast
    /// notification (`false`) or a modal alert dialog (`true`).
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "callback_query_id": callback_query_id,
            "show_alert": show_alert,
        });
        if let Some(t) = text {
            body["text"] = serde_json::Value::String(t.to_string());
        }
        let _: TgResponse<bool> = self.api_call("answerCallbackQuery", &body).await?;
        Ok(())
    }
}

// ===================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::PathBuf;

    /// Build a minimal Config suitable for unit-testing TelegramBot construction.
    fn test_config() -> Config {
        Config {
            bot_token: "123456:ABC-DEF_test-token".to_string(),
            chat_id: -1001234567890,
            enabled: true,
            verbose: false,
            approvals: true,
            use_threads: true,
            chunk_size: 4000,
            rate_limit: 20,
            session_timeout: 30,
            stale_session_timeout_hours: 72,
            auto_delete_topics: true,
            topic_delete_delay_minutes: 1440,
            socket_path: PathBuf::from("/tmp/test.sock"),
            config_dir: PathBuf::from("/tmp"),
            config_path: PathBuf::from("/tmp/config.json"),
            forum_enabled: false,
        }
    }

    #[test]
    fn new_succeeds_with_valid_config() {
        let config = test_config();
        let bot = TelegramBot::new(&config).expect("TelegramBot::new should succeed");
        assert_eq!(bot.token, config.bot_token);
        assert_eq!(bot.chat_id, config.chat_id);
        assert_eq!(bot.chunk_size, 4000);
    }

    #[test]
    fn new_with_zero_rate_limit_defaults_to_20() {
        let mut config = test_config();
        config.rate_limit = 0;
        // Should not panic — zero rate_limit is treated as 20
        let bot = TelegramBot::new(&config).expect("rate_limit=0 should default to 20");
        // We can't directly inspect the governor quota, but construction succeeding
        // proves NonZeroU32::new(20) was used instead of NonZeroU32::new(0).
        assert_eq!(bot.chat_id, config.chat_id);
    }

    #[test]
    fn new_with_custom_rate_limit() {
        let mut config = test_config();
        config.rate_limit = 5;
        let bot = TelegramBot::new(&config).expect("rate_limit=5 should succeed");
        assert_eq!(bot.chat_id, config.chat_id);
    }

    #[test]
    fn chat_id_getter() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        assert_eq!(bot.chat_id(), -1001234567890);
    }

    #[test]
    fn is_running_default_false() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        assert!(!bot.is_running());
    }

    #[test]
    fn set_running_toggles_flag() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        assert!(!bot.is_running());

        bot.set_running(true);
        assert!(bot.is_running());

        bot.set_running(false);
        assert!(!bot.is_running());
    }

    #[test]
    fn get_session_always_returns_none() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        assert!(bot.get_session(12345).is_none());
        assert!(bot.get_session(bot.chat_id()).is_none());
    }

    #[test]
    fn api_url_construction() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        let url = bot.api_url("sendMessage");
        assert_eq!(
            url,
            "https://api.telegram.org/bot123456:ABC-DEF_test-token/sendMessage"
        );
    }

    #[test]
    fn api_url_get_me() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        let url = bot.api_url("getMe");
        assert!(url.ends_with("/getMe"));
        assert!(url.starts_with("https://api.telegram.org/bot"));
    }

    #[test]
    fn scrub_token_removes_token_from_text() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        let error_msg =
            "POST https://api.telegram.org/bot123456:ABC-DEF_test-token/sendMessage failed";
        let scrubbed = bot.scrub_token(error_msg);
        assert!(!scrubbed.contains("123456:ABC-DEF_test-token"));
        assert!(scrubbed.contains("[REDACTED]"));
    }

    #[test]
    fn scrub_token_preserves_safe_text() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        let safe_msg = "Network timeout after 30 seconds";
        assert_eq!(bot.scrub_token(safe_msg), safe_msg);
    }

    #[test]
    fn queue_starts_empty() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        // The queue is wrapped in Arc<Mutex>, so we verify via the type system
        // that it was initialized. The VecDeque should be empty.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let q = bot.queue.lock().await;
            assert!(q.is_empty());
        });
    }

    #[test]
    fn chunk_size_from_config() {
        let mut config = test_config();
        config.chunk_size = 2000;
        let bot = TelegramBot::new(&config).unwrap();
        assert_eq!(bot.chunk_size, 2000);
    }
}
