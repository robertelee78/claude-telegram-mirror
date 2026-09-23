//! The send path: the one task that drains the outbox into Telegram (ADR-023, ADR-024).
//!
//! Handlers never wait on Telegram: they put a message in the outbox and return
//! (ADR-023). This drainer is the only thing that waits — for its slot in the group's
//! 20-a-minute budget, for Telegram's `retry_after`, for the network to come back.
//!
//! **It never drops a message for load or for a transient failure** (ADR-024). Each
//! send takes the next *batch* — everything waiting for the most urgent topic, packed
//! into one message — so a backlog drains at dozens of lines per post. What happens
//! when a send fails:
//!
//! | Telegram said                 | The drainer                                       |
//! |-------------------------------|---------------------------------------------------|
//! | 429 "retry after N"           | puts it back, waits N, tries again                |
//! | the topic no longer exists    | holds the topic's messages until the daemon makes |
//! |                               | a new topic, then sends them there                |
//! | network error / 5xx           | puts it back, backs off (up to a minute), retries |
//! | this content is invalid (400) | the one case it gives up — logged with the text   |
//!
//! The outbox is saved to disk every couple of seconds while it changes, so a daemon
//! restart resumes where it left off.

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

/// Longest back-off between retries of a message that failed for a transient reason.
const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(60);
/// How often a changed outbox is written to disk.
const SAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(2);
/// File the outbox is saved to, in ctm's config directory.
pub(super) const OUTBOX_FILE: &str = "outbox.json";

/// Deterministic jitter fraction in the range [0.0, 1.0), derived from the
/// current wall-clock nanoseconds. Avoids adding a `rand` crate dependency.
fn simple_jitter_fraction() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos % 1000) as f64 / 1000.0
}

/// Back-off for the `n`th consecutive transient failure: 2 s, 4 s, 8 s … capped.
pub(super) fn backoff(n: u32) -> std::time::Duration {
    let secs = 1u64
        .checked_shl(n.min(10))
        .unwrap_or(u64::MAX)
        .saturating_mul(2);
    std::time::Duration::from_secs(secs).min(MAX_BACKOFF)
}

impl TelegramBot {
    /// Put a message in the outbox and wake the drainer. Never blocks on Telegram.
    pub(super) async fn enqueue(&self, msg: QueuedMessage) {
        let depth = {
            let mut q = self.queue.lock().await;
            q.push(msg);
            q.len()
        };
        self.outbox_dirty.store(true, Ordering::Release);
        self.mark_queue_depth(depth);
        self.queue_wake.notify_one();
    }

    /// The single queue drainer. Started once by the daemon; runs for its lifetime.
    ///
    /// Waking before waiting closes the race where a message is queued between the
    /// drain finishing and the wait starting (`Notify` stores one permit, so a
    /// notify that arrives first is not lost).
    pub async fn run_queue_drainer(&self) {
        loop {
            self.process_queue().await;
            self.queue_wake.notified().await;
        }
    }

    /// How many messages are still waiting for this topic. The daemon waits for this
    /// to reach zero before deleting a finished session's topic (ADR-024), so a
    /// session's last reply is not deleted along with it.
    pub async fn pending_for(&self, thread_id: i64) -> usize {
        self.queue.lock().await.pending_for(thread_id)
    }

    /// Send everything that was waiting for a vanished topic to its replacement.
    pub async fn retarget(&self, old: i64, new: i64) -> usize {
        let moved = self.queue.lock().await.retarget(old, new);
        if moved > 0 {
            tracing::info!(
                old_thread_id = old,
                new_thread_id = new,
                moved,
                "ADR-024: messages held for a vanished topic are going to its replacement"
            );
            self.outbox_dirty.store(true, Ordering::Release);
            self.queue_wake.notify_one();
        }
        moved
    }

    /// The topic is gone and nothing will replace it (its session ended and ctm deleted
    /// the topic on purpose). Returns how many messages were waiting for it.
    pub async fn discard_topic(&self, thread_id: i64) -> usize {
        let n = self.queue.lock().await.discard_topic(thread_id);
        if n > 0 {
            self.outbox_dirty.store(true, Ordering::Release);
        }
        n
    }

    /// Is this topic's traffic being held because Telegram says it is gone?
    pub async fn is_parked(&self, thread_id: i64) -> bool {
        self.queue.lock().await.is_parked(thread_id)
    }

    /// Reload what the previous process left waiting. Call before the drainer starts.
    pub async fn restore_outbox(&self) -> usize {
        let Some(path) = &self.outbox_path else {
            return 0;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return 0;
        };
        let items: Vec<QueuedMessage> = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "ADR-024: saved outbox unreadable; starting empty");
                return 0;
            }
        };
        let n = items.len();
        let mut q = self.queue.lock().await;
        q.restore(items);
        self.mark_queue_depth(q.len());
        if n > 0 {
            tracing::info!(
                restored = n,
                "ADR-024: resuming messages that were waiting when the daemon stopped"
            );
        }
        n
    }

    /// Write the outbox to disk if it changed. Atomic (temp file + rename), owner-only.
    pub async fn save_outbox(&self) {
        let Some(path) = &self.outbox_path else {
            return;
        };
        if !self.outbox_dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let snapshot = self.queue.lock().await.snapshot();
        let Ok(text) = serde_json::to_string(&snapshot) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::write(&tmp, text).and_then(|()| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&tmp, path)
        });
        if let Err(e) = written {
            self.outbox_dirty.store(true, Ordering::Release);
            tracing::warn!(error = %e, "ADR-024: could not save the outbox; will retry");
        }
    }

    /// Save immediately, changed or not (daemon shutdown).
    pub async fn save_outbox_now(&self) {
        self.outbox_dirty.store(true, Ordering::Release);
        self.save_outbox().await;
    }

    /// Save the outbox every couple of seconds while it changes. Runs for the daemon's
    /// lifetime.
    pub async fn run_outbox_saver(&self) {
        let mut tick = tokio::time::interval(SAVE_EVERY);
        loop {
            tick.tick().await;
            self.save_outbox().await;
        }
    }

    /// Drain the outbox.
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
                let next = q.next_batch(self.chunk_size);
                self.mark_queue_depth(q.len());
                match next {
                    Some(m) => m,
                    None => break, // empty, or everything left is held for a gone topic
                }
            };
            self.outbox_dirty.store(true, Ordering::Release);

            match self.send_item(&item).await {
                Ok(()) => {
                    self.aimd.lock().await.on_success();
                    self.mark_send_ok();
                }
                Err(AppError::RateLimited { retry_after_secs }) => {
                    // Telegram's own answer to "too much": nothing goes out for this
                    // bot until `retry_after`. The pacer records it; wait it out here —
                    // this task holds no permit and blocks nobody.
                    self.aimd.lock().await.on_rate_limit(retry_after_secs);
                    let wait_ms = retry_after_secs * 1000;
                    let jitter_ms = (wait_ms as f64 * 0.1 * simple_jitter_fraction()) as u64;
                    let total_wait = tokio::time::Duration::from_millis(wait_ms + jitter_ms);
                    let depth = {
                        let mut q = self.queue.lock().await;
                        q.push_front(item);
                        q.len()
                    };
                    tracing::warn!(
                        retry_after_secs,
                        queue_depth = depth,
                        "429 rate limited — holding the outbox until Telegram accepts again"
                    );
                    tokio::time::sleep(total_wait).await;
                }
                Err(AppError::TopicGone { thread_id }) => {
                    // Hold, never drop: the daemon was told (`topic_invalidated_tx`) and
                    // will make a replacement topic and `retarget` these messages.
                    let held = {
                        let mut q = self.queue.lock().await;
                        q.push_front(item);
                        q.park(thread_id);
                        q.pending_for(thread_id)
                    };
                    tracing::warn!(
                        thread_id,
                        held,
                        "ADR-024: topic is gone — holding its messages for a replacement topic"
                    );
                }
                Err(AppError::Rejected(reason)) => {
                    // The one case where retrying cannot help: Telegram refused this
                    // content itself (and the plain-text fallback, where it applied).
                    let preview: String = item.text.chars().take(120).collect();
                    tracing::error!(
                        reason = %reason,
                        thread_id = ?item.thread_id,
                        text = %preview,
                        "ADR-024: Telegram refused this message's content; it cannot be delivered"
                    );
                }
                Err(e) => {
                    // Network, 5xx, anything transient: back off and try again, forever.
                    let mut retry = item;
                    retry.retries = retry.retries.saturating_add(1);
                    let wait = backoff(retry.retries);
                    tracing::warn!(
                        attempt = retry.retries,
                        wait_secs = wait.as_secs(),
                        error = %self.scrub_token(&e.to_string()),
                        "Message send failed — will retry (never dropped)"
                    );
                    self.queue.lock().await.push_front(retry);
                    tokio::time::sleep(wait).await;
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

        let resp: TgResponse<TgMessage> = match self.api_call("sendMessage", &body).await {
            Ok(r) => r,
            Err(e) => return Err(e), // Network/5xx errors — let process_queue retry
        };

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

            tracing::warn!(
                retry_after_secs,
                "Telegram rate limited (429), honoring retry_after"
            );

            return Err(AppError::RateLimited { retry_after_secs });
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
        // Notify the daemon to clear the stale thread_id so a new topic is created.
        if code == 400 && desc.contains("TOPIC_ID_INVALID") {
            if let Some(tid) = item.thread_id {
                tracing::warn!(
                    thread_id = tid,
                    "Topic permanently deleted (TOPIC_ID_INVALID), clearing stale mapping"
                );
                let _ = self.topic_invalidated_tx.send(tid);
                return Err(AppError::TopicGone { thread_id: tid });
            }
            return Err(AppError::Rejected("Topic deleted".into()));
        }

        // "message thread not found": stale thread_id or Telegram state inconsistency.
        // Retrying with the same thread_id will fail identically — don't retry.
        // Notify the daemon to clear the stale thread_id so ensure_session_exists
        // creates a new topic on the next message.
        if code == 400 && desc.contains("message thread not found") {
            if let Some(tid) = item.thread_id {
                tracing::warn!(
                    thread_id = tid,
                    "Topic not found (stale thread_id), clearing stale mapping"
                );
                let _ = self.topic_invalidated_tx.send(tid);
                return Err(AppError::TopicGone { thread_id: tid });
            }
            return Err(AppError::Rejected("Topic not found".into()));
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
            if let Some(reply_id) = item.reply_to_message_id {
                plain_body["reply_parameters"] = serde_json::json!({ "message_id": reply_id });
            }
            if let Some(buttons) = &item.buttons {
                plain_body["reply_markup"] = build_inline_keyboard(buttons);
            }

            let plain_resp: TgResponse<TgMessage> =
                self.api_call("sendMessage", &plain_body).await?;
            if plain_resp.ok {
                return Ok(());
            }
            // Plain text fallback also failed — return immediately with the
            // secondary error so process_queue does not retry with the original
            // broken Markdown body.
            let plain_desc = plain_resp.description.unwrap_or_default();
            return Err(AppError::Rejected(format!(
                "Plain text fallback also failed: {}",
                self.scrub_token(&plain_desc)
            )));
        }

        // Any other 400 is Telegram refusing this message's content — retrying it
        // unchanged cannot succeed. Everything that is not a 400 (network, 5xx) has
        // already come back as an `Err` from `api_call` and is retried by the drainer.
        if code == 400 {
            return Err(AppError::Rejected(self.scrub_token(&desc)));
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
            hosts: Default::default(),
        }
    }

    #[tokio::test]
    async fn bot_queue_starts_empty() {
        let config = test_config();
        let bot = TelegramBot::new(&config).unwrap();
        let q = bot.queue.lock().await;
        assert_eq!(q.len(), 0);
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
            assert!((0.0..1.0).contains(&f), "jitter {f} out of [0, 1) range");
        }
    }

    #[test]
    fn transient_failures_back_off_to_a_minute_and_stay_there() {
        assert_eq!(backoff(1).as_secs(), 4);
        assert_eq!(backoff(2).as_secs(), 8);
        assert_eq!(backoff(20), MAX_BACKOFF, "capped, and never gives up");
    }

    #[test]
    fn the_pacer_spaces_group_posts_and_honours_retry_after() {
        let mut p = AimdState::new(20.0 / 60.0);
        let now = std::time::Instant::now();
        let far = now + std::time::Duration::from_secs(3600);
        let a = p.book(now, far).unwrap();
        let b = p.book(now, far).unwrap();
        assert_eq!(
            (b - a).as_secs(),
            3,
            "20 a minute is one every three seconds"
        );
        // A 429 pushes the schedule past Telegram's retry_after…
        p.on_rate_limit(40);
        let c = p.book(now, far).unwrap();
        assert!(c >= now + std::time::Duration::from_secs(40));
        // …and a caller that cannot wait that long is told so instead of blocking.
        let soon = std::time::Instant::now() + std::time::Duration::from_secs(20);
        assert!(p.book(std::time::Instant::now(), soon).is_err());
    }

    // ---------------------------------------------------------------- AimdState

    /// The budget Telegram grants a bot in a group, as messages per second.
    /// "In a group, bots are not able to send more than 20 messages per minute."
    const GROUP_BUDGET: f64 = 20.0 / 60.0;

    #[test]
    fn aimd_never_settles_above_the_group_budget() {
        // ADR-024, the regression test for the whole incident: the controller's floor
        // used to be a flat 0.5 msg/s — 30 a minute — while Telegram allows 20. Every
        // backoff still overshot, so the bot was rate-limited continuously (615 global
        // pauses in a day) and the queue never drained.
        let mut aimd = AimdState::new(GROUP_BUDGET);
        for _ in 0..50 {
            aimd.on_rate_limit(40);
            // The debounce means only the first decrease per second lands; step time
            // forward by clearing it, which is what a real 40 s pause does.
            aimd.last_decrease = None;
        }
        assert!(
            aimd.rate <= GROUP_BUDGET,
            "settled at {} msg/s, above Telegram's {} msg/s for a group",
            aimd.rate,
            GROUP_BUDGET
        );
        assert!(aimd.rate > 0.0, "and it must still send *something*");
        assert_eq!(aimd.rate, aimd.min_rate, "it converges on the floor");
    }

    #[test]
    fn aimd_recovers_toward_the_budget_but_not_past_it() {
        let mut aimd = AimdState::new(GROUP_BUDGET);
        aimd.rate = aimd.min_rate;
        for _ in 0..200 {
            aimd.on_success();
        }
        assert_eq!(
            aimd.rate, GROUP_BUDGET,
            "success walks the rate back up to the budget and stops there"
        );
    }

    #[test]
    fn aimd_on_rate_limit_halves_rate() {
        let mut aimd = AimdState::new(GROUP_BUDGET);
        aimd.rate = GROUP_BUDGET;
        aimd.on_rate_limit(30);
        assert!((aimd.rate - GROUP_BUDGET / 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn aimd_on_rate_limit_clamps_to_min() {
        let mut aimd = AimdState::new(GROUP_BUDGET);
        aimd.rate = aimd.min_rate * 1.2;
        aimd.on_rate_limit(30);
        assert_eq!(aimd.rate, aimd.min_rate);
    }

    #[test]
    fn aimd_inter_message_delay_at_max_rate() {
        let aimd = AimdState::new(GROUP_BUDGET);
        let delay = aimd.inter_message_delay();
        // 20 a minute is one every three seconds.
        assert_eq!(delay.as_secs(), 3);
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
