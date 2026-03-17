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
                q.pop_front().unwrap()
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
