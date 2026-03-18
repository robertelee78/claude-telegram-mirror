//! Message queue, rate limiting, and retry logic for TelegramBot.

use super::*;

/// RAII guard that resets `queue_processing` to `false` when dropped.
/// This ensures the flag is cleared even if the processing loop panics or
/// the future is cancelled.
struct ProcessingGuard(Arc<AtomicBool>);

impl Drop for ProcessingGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Three-tier priority message queue.
///
/// Messages are dequeued strictly in priority order: Critical first, then
/// Normal, then Low. Within each tier, ordering is FIFO.
///
/// Each tier has an independent overflow cap. When a tier overflows, the
/// oldest message in that tier is dropped — higher-priority tiers are never
/// affected by lower-priority overflows.
pub(super) struct PriorityMessageQueue {
    critical: VecDeque<QueuedMessage>,
    normal: VecDeque<QueuedMessage>,
    low: VecDeque<QueuedMessage>,
}

impl PriorityMessageQueue {
    /// Maximum critical messages queued. Should never be hit in normal operation.
    const MAX_CRITICAL: usize = 50;
    /// Maximum normal-priority messages queued.
    const MAX_NORMAL: usize = 300;
    /// Maximum low-priority messages queued.
    const MAX_LOW: usize = 150;

    pub(super) fn new() -> Self {
        Self {
            critical: VecDeque::new(),
            normal: VecDeque::new(),
            low: VecDeque::new(),
        }
    }

    /// Enqueue a message, routing to the correct tier by priority.
    /// If the tier is full, the oldest message in that tier is dropped.
    pub(super) fn enqueue(&mut self, msg: QueuedMessage) {
        let (queue, max) = match msg.priority {
            MessagePriority::Critical => (&mut self.critical, Self::MAX_CRITICAL),
            MessagePriority::Normal => (&mut self.normal, Self::MAX_NORMAL),
            MessagePriority::Low => (&mut self.low, Self::MAX_LOW),
        };
        if queue.len() >= max {
            let dropped = queue.pop_front();
            tracing::warn!(
                priority = ?msg.priority,
                dropped_text = dropped.as_ref().map(|d| d.text.chars().take(50).collect::<String>()),
                "Queue full for priority tier, dropping oldest message"
            );
        }
        queue.push_back(msg);
    }

    /// Pop the highest-priority available message.
    /// Drains Critical first, then Normal, then Low.
    pub(super) fn pop_next(&mut self) -> Option<QueuedMessage> {
        self.critical
            .pop_front()
            .or_else(|| self.normal.pop_front())
            .or_else(|| self.low.pop_front())
    }

    /// Push a message to the front of its priority tier's sub-queue.
    /// Used to re-enqueue a message for retry without changing its priority order.
    pub(super) fn push_front(&mut self, msg: QueuedMessage) {
        match msg.priority {
            MessagePriority::Critical => self.critical.push_front(msg),
            MessagePriority::Normal => self.normal.push_front(msg),
            MessagePriority::Low => self.low.push_front(msg),
        }
    }

    /// Total number of messages across all priority tiers.
    pub(super) fn total_len(&self) -> usize {
        self.critical.len() + self.normal.len() + self.low.len()
    }

    /// Whether all priority tiers are empty.
    pub(super) fn is_empty(&self) -> bool {
        self.critical.is_empty() && self.normal.is_empty() && self.low.is_empty()
    }
}

/// Deterministic jitter fraction in the range [0.0, 1.0), derived from the
/// current wall-clock nanoseconds. Avoids adding a `rand` crate dependency.
fn simple_jitter_fraction() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos % 1000) as f64 / 1000.0
}

impl TelegramBot {
    /// Enqueue a message and start processing if not already running.
    pub(super) async fn enqueue(&self, msg: QueuedMessage) {
        let mut q = self.queue.lock().await;
        q.enqueue(msg);
        drop(q);
        self.process_queue().await;
    }

    /// Process the message queue with retry logic.
    async fn process_queue(&self) {
        // Atomically set processing = true only if it was false.
        // If it was already true, another task is processing — return immediately.
        if self
            .queue_processing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // Hold the guard for the entire duration of the loop so that
        // cancellation or a panic always resets the flag via Drop.
        let _guard = ProcessingGuard(Arc::clone(&self.queue_processing));

        loop {
            let item = {
                let mut q = self.queue.lock().await;
                if q.is_empty() {
                    break;
                }
                match q.pop_next() {
                    Some(m) => m,
                    None => break,
                }
            };

            match self.send_item(&item).await {
                Ok(()) => {
                    // Additive increase: successful send, rate can grow.
                    self.aimd.lock().await.on_success();
                }
                Err(AppError::RateLimited {
                    retry_after_secs,
                    adaptive_retry_ms,
                }) => {
                    // Telegram 429 — the entire bot token is globally blocked.
                    // No API calls of any type succeed during the retry_after window.
                    // The entire queue must pause; continuing to send other messages
                    // extends the penalty and risks a 900-second IP ban.

                    // Multiplicative decrease: halve the effective send rate.
                    self.aimd.lock().await.on_rate_limit(retry_after_secs);

                    // Compute wait duration: prefer ms-precision adaptive_retry when
                    // available (Bot API 8.0+), fall back to retry_after in seconds.
                    let wait_ms = adaptive_retry_ms.unwrap_or(retry_after_secs * 1000);
                    // Add ~10% jitter to prevent thundering herd if multiple bots recover
                    // simultaneously. Jitter is deterministic (no rand crate).
                    let jitter_ms = (wait_ms as f64 * 0.1 * simple_jitter_fraction()) as u64;
                    let total_wait = tokio::time::Duration::from_millis(wait_ms + jitter_ms);

                    let q_depth = self.queue.lock().await.total_len();
                    tracing::warn!(
                        retry_after_secs,
                        adaptive_retry_ms = ?adaptive_retry_ms,
                        total_wait_ms = total_wait.as_millis(),
                        queue_depth = q_depth,
                        "429 rate limited — pausing entire queue (bot token globally blocked)"
                    );

                    // Sleep before re-enqueuing so the queue is unlocked during the wait.
                    tokio::time::sleep(total_wait).await;

                    // Push the message back to the front of its priority tier WITHOUT
                    // incrementing retries — rate limiting is flow control, not a failure.
                    let mut q = self.queue.lock().await;
                    q.push_front(item);
                }
                Err(e) => {
                    let err_str = self.scrub_token(&e.to_string());
                    if item.retries < 3 {
                        let mut retry = item.clone();
                        retry.retries += 1;
                        let delay_ms = 1000u64.saturating_mul(1u64 << retry.retries.min(10));
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
        // _guard is dropped here, resetting queue_processing to false.
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

        let desc = resp.description.as_deref().unwrap_or("").to_string();
        let code = resp.error_code.unwrap_or(0);

        // 429 Too Many Requests — honor retry_after and pause the entire queue.
        // Rate limiting is NOT a retry-worthy failure; it is flow control.
        if code == 429 {
            let retry_after_secs = resp
                .parameters
                .as_ref()
                .and_then(|p| p.retry_after)
                .unwrap_or(30); // Conservative fallback per ADR-011.
            let adaptive_retry_ms = resp.parameters.as_ref().and_then(|p| p.adaptive_retry);

            tracing::warn!(
                retry_after_secs,
                adaptive_retry_ms = ?adaptive_retry_ms,
                "Telegram rate limited (429), honoring retry_after"
            );

            return Err(AppError::RateLimited {
                retry_after_secs,
                adaptive_retry_ms,
            });
        }

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

                    // Retry the original message; surface any new error directly.
                    let retry_resp: TgResponse<TgMessage> =
                        self.api_call("sendMessage", &body).await?;
                    if retry_resp.ok {
                        return Ok(());
                    }
                    let retry_desc = retry_resp.description.unwrap_or_default();
                    return Err(AppError::Telegram(self.scrub_token(&retry_desc)));
                }
                tracing::error!(thread_id = tid, "Failed to reopen topic");
                return Err(AppError::Telegram(self.scrub_token(&desc)));
            }
        }

        // TOPIC_ID_INVALID: topic has been permanently deleted, don't retry.
        if code == 400 && desc.contains("TOPIC_ID_INVALID") {
            tracing::warn!(
                thread_id = ?item.thread_id,
                "Topic no longer exists (TOPIC_ID_INVALID), dropping message"
            );
            return Err(AppError::Telegram("Topic deleted".into()));
        }

        // "message thread not found": stale thread_id or Telegram state inconsistency.
        // Retrying with the same thread_id will fail identically — don't retry.
        // Let ensure_session_exists create a new topic on the next message.
        if code == 400 && desc.contains("message thread not found") {
            tracing::warn!(
                thread_id = ?item.thread_id,
                "Topic not found (stale thread_id or Telegram state inconsistency), dropping message"
            );
            return Err(AppError::Telegram("Topic not found".into()));
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
    use crate::bot::client::AimdState;
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
            topic_delete_delay_minutes: 15,
            inactivity_delete_threshold_minutes: 720,
            socket_path: PathBuf::from("/tmp/test.sock"),
            config_dir: PathBuf::from("/tmp"),
            config_path: PathBuf::from("/tmp/config.json"),
            forum_enabled: false,
            subagent_detection_window_secs: 60,
        }
    }

    /// Build a minimal QueuedMessage for tests.
    fn make_msg(text: &str, priority: MessagePriority) -> QueuedMessage {
        QueuedMessage {
            chat_id: -100999,
            text: text.to_string(),
            thread_id: None,
            buttons: None,
            parse_mode: None,
            disable_notification: None,
            reply_to_message_id: None,
            retries: 0,
            created_at: 0,
            priority,
        }
    }

    // ---------------------------------------------------------------- PriorityMessageQueue

    #[test]
    fn priority_queue_starts_empty() {
        let q = PriorityMessageQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.total_len(), 0);
    }

    #[test]
    fn priority_queue_drains_critical_before_normal_before_low() {
        let mut q = PriorityMessageQueue::new();
        q.enqueue(make_msg("low-1", MessagePriority::Low));
        q.enqueue(make_msg("normal-1", MessagePriority::Normal));
        q.enqueue(make_msg("critical-1", MessagePriority::Critical));
        q.enqueue(make_msg("low-2", MessagePriority::Low));
        q.enqueue(make_msg("normal-2", MessagePriority::Normal));
        q.enqueue(make_msg("critical-2", MessagePriority::Critical));

        // Critical drained first (FIFO within tier)
        assert_eq!(q.pop_next().unwrap().text, "critical-1");
        assert_eq!(q.pop_next().unwrap().text, "critical-2");
        // Then normal
        assert_eq!(q.pop_next().unwrap().text, "normal-1");
        assert_eq!(q.pop_next().unwrap().text, "normal-2");
        // Then low
        assert_eq!(q.pop_next().unwrap().text, "low-1");
        assert_eq!(q.pop_next().unwrap().text, "low-2");
        // Empty
        assert!(q.pop_next().is_none());
    }

    #[test]
    fn priority_queue_total_len_counts_all_tiers() {
        let mut q = PriorityMessageQueue::new();
        q.enqueue(make_msg("a", MessagePriority::Critical));
        q.enqueue(make_msg("b", MessagePriority::Normal));
        q.enqueue(make_msg("c", MessagePriority::Low));
        assert_eq!(q.total_len(), 3);
    }

    #[test]
    fn priority_queue_overflow_drops_oldest_in_tier() {
        let mut q = PriorityMessageQueue::new();
        // Fill the low-priority tier to its cap
        for i in 0..PriorityMessageQueue::MAX_LOW {
            q.enqueue(make_msg(&format!("low-{i}"), MessagePriority::Low));
        }
        assert_eq!(q.low.len(), PriorityMessageQueue::MAX_LOW);

        // Adding one more should drop the oldest (low-0) and add the new one
        q.enqueue(make_msg("low-overflow", MessagePriority::Low));
        assert_eq!(q.low.len(), PriorityMessageQueue::MAX_LOW);
        // The oldest "low-0" should have been dropped; "low-1" is now front
        assert_eq!(q.low.front().unwrap().text, "low-1");
        // The new message is at the back
        assert_eq!(q.low.back().unwrap().text, "low-overflow");
    }

    #[test]
    fn priority_queue_overflow_does_not_affect_other_tiers() {
        let mut q = PriorityMessageQueue::new();
        // Pre-fill normal tier to its cap
        for i in 0..PriorityMessageQueue::MAX_NORMAL {
            q.enqueue(make_msg(&format!("normal-{i}"), MessagePriority::Normal));
        }
        // Add a critical message — should not be affected by normal overflow
        q.enqueue(make_msg("critical-safe", MessagePriority::Critical));
        assert_eq!(q.critical.len(), 1);
        assert_eq!(q.critical.front().unwrap().text, "critical-safe");
    }

    #[test]
    fn priority_queue_push_front_preserves_priority() {
        let mut q = PriorityMessageQueue::new();
        q.enqueue(make_msg("normal-first", MessagePriority::Normal));
        q.push_front(make_msg("normal-retry", MessagePriority::Normal));

        // push_front should place it at the head of the Normal tier
        assert_eq!(q.pop_next().unwrap().text, "normal-retry");
        assert_eq!(q.pop_next().unwrap().text, "normal-first");
        assert!(q.pop_next().is_none());
    }

    #[test]
    fn priority_queue_fifo_within_tier() {
        let mut q = PriorityMessageQueue::new();
        for i in 0..5 {
            q.enqueue(make_msg(&format!("msg-{i}"), MessagePriority::Normal));
        }
        for i in 0..5 {
            assert_eq!(q.pop_next().unwrap().text, format!("msg-{i}"));
        }
        assert!(q.pop_next().is_none());
    }

    // ---------------------------------------------------------------- TelegramBot queue

    #[tokio::test]
    async fn bot_queue_starts_empty() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        let q = bot.queue.lock().await;
        assert!(q.is_empty());
        assert_eq!(q.total_len(), 0);
    }

    #[tokio::test]
    async fn queue_processing_flag_starts_false() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        assert!(!bot.queue_processing.load(Ordering::Acquire));
    }

    // ---------------------------------------------------------------- simple_jitter_fraction

    #[test]
    fn jitter_fraction_is_in_range() {
        for _ in 0..100 {
            let f = simple_jitter_fraction();
            assert!(f >= 0.0 && f < 1.0, "jitter {f} out of [0, 1) range");
        }
    }

    // ---------------------------------------------------------------- AimdState

    #[test]
    fn aimd_on_success_increases_rate() {
        let mut aimd = AimdState::new(20.0);
        aimd.rate = 10.0; // Start below max
        aimd.on_success();
        assert_eq!(aimd.rate, 10.5);
    }

    #[test]
    fn aimd_on_success_clamps_to_max() {
        let mut aimd = AimdState::new(20.0);
        aimd.rate = 19.8;
        aimd.on_success();
        assert_eq!(aimd.rate, 20.0); // Clamped to max_rate
    }

    #[test]
    fn aimd_on_rate_limit_halves_rate() {
        let mut aimd = AimdState::new(20.0);
        aimd.rate = 20.0;
        aimd.on_rate_limit(30);
        assert_eq!(aimd.rate, 10.0);
    }

    #[test]
    fn aimd_on_rate_limit_clamps_to_min() {
        let mut aimd = AimdState::new(20.0);
        aimd.rate = 0.6; // Just above min
        aimd.on_rate_limit(30);
        assert_eq!(aimd.rate, 0.5); // Clamped to min_rate
    }

    #[test]
    fn aimd_inter_message_delay_at_max_rate() {
        let aimd = AimdState::new(20.0);
        let delay = aimd.inter_message_delay();
        // 1.0 / 20.0 = 50ms
        assert_eq!(delay.as_millis(), 50);
    }

    #[test]
    fn aimd_debounce_prevents_double_decrease() {
        let mut aimd = AimdState::new(20.0);
        aimd.rate = 20.0;
        // First decrease
        aimd.on_rate_limit(30);
        assert_eq!(aimd.rate, 10.0);
        // Second decrease immediately — should be debounced (no change)
        aimd.on_rate_limit(30);
        assert_eq!(aimd.rate, 10.0);
    }

    // ---------------------------------------------------------------- MessagePriority ordering

    #[test]
    fn message_priority_ordering() {
        assert!(MessagePriority::Critical < MessagePriority::Normal);
        assert!(MessagePriority::Normal < MessagePriority::Low);
        assert!(MessagePriority::Critical < MessagePriority::Low);
    }
}
