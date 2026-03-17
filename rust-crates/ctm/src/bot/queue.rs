//! Message queue, rate limiting, and retry logic for TelegramBot.

use super::*;

impl TelegramBot {
    /// Enqueue a message and start processing if not already running.
    pub(super) async fn enqueue(&self, msg: QueuedMessage) {
        self.queue.lock().await.push_back(msg);
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
                match q.pop_front() {
                    Some(m) => m,
                    None => break,
                }
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
                        q.push_front(retry);
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
        if let Some(reply_id) = item.reply_to_message_id {
            body["reply_parameters"] = serde_json::json!({ "message_id": reply_id });
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
}

// ===================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config {
            bot_token: "999:FAKE-TOKEN-for-queue-tests".to_string(),
            chat_id: -100999,
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

    #[tokio::test]
    async fn queue_drains_fifo_order() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();

        // Manually push items into the raw queue (bypassing enqueue which
        // triggers process_queue and would attempt HTTP calls).
        {
            let mut q = bot.queue.lock().await;
            for i in 0..5 {
                q.push_back(QueuedMessage {
                    chat_id: -100999,
                    text: format!("msg-{}", i),
                    thread_id: None,
                    buttons: None,
                    parse_mode: None,
                    disable_notification: None,
                    reply_to_message_id: None,
                    retries: 0,
                    created_at: 0,
                });
            }
        }

        // Drain and verify FIFO order
        let mut q = bot.queue.lock().await;
        for i in 0..5 {
            let item = q.pop_front().expect("queue should have items");
            assert_eq!(item.text, format!("msg-{}", i));
        }
        assert!(q.pop_front().is_none(), "queue should be empty after drain");
    }

    #[tokio::test]
    async fn empty_queue_pop_returns_none() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();

        let mut q = bot.queue.lock().await;
        assert!(q.is_empty());
        assert!(q.pop_front().is_none(), "pop_front on empty queue returns None, no panic");
    }

    #[tokio::test]
    async fn queue_push_back_and_push_front() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();

        let make_msg = |text: &str| QueuedMessage {
            chat_id: -100999,
            text: text.to_string(),
            thread_id: None,
            buttons: None,
            parse_mode: None,
            disable_notification: None,
            reply_to_message_id: None,
            retries: 0,
            created_at: 0,
        };

        {
            let mut q = bot.queue.lock().await;
            q.push_back(make_msg("first"));
            q.push_back(make_msg("second"));
            // Simulate retry: push_front puts it at head
            q.push_front(make_msg("retry"));
        }

        let mut q = bot.queue.lock().await;
        assert_eq!(q.pop_front().unwrap().text, "retry");
        assert_eq!(q.pop_front().unwrap().text, "first");
        assert_eq!(q.pop_front().unwrap().text, "second");
        assert!(q.pop_front().is_none());
    }

    #[tokio::test]
    async fn concurrent_push_pop_safety() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        let queue = Arc::clone(&bot.queue);

        let make_msg = |text: String| QueuedMessage {
            chat_id: -100999,
            text,
            thread_id: None,
            buttons: None,
            parse_mode: None,
            disable_notification: None,
            reply_to_message_id: None,
            retries: 0,
            created_at: 0,
        };

        // Spawn 10 tasks that each push a message
        let mut handles = Vec::new();
        for i in 0..10 {
            let q = Arc::clone(&queue);
            handles.push(tokio::spawn(async move {
                let mut locked = q.lock().await;
                locked.push_back(make_msg(format!("concurrent-{}", i)));
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // All 10 messages should be present
        let mut q = queue.lock().await;
        assert_eq!(q.len(), 10);

        // Drain all
        let mut texts: Vec<String> = Vec::new();
        while let Some(item) = q.pop_front() {
            texts.push(item.text);
        }
        assert_eq!(texts.len(), 10);
        // All should start with "concurrent-"
        for t in &texts {
            assert!(t.starts_with("concurrent-"));
        }
    }

    #[tokio::test]
    async fn queue_processing_flag_starts_false() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        let processing = bot.queue_processing.lock().await;
        assert!(!*processing);
    }
}
