//! Main event loop multiplexing socket messages, Telegram updates, and cleanup timer.

use super::*;
use tokio::sync::Semaphore;

/// Returns a pseudo-random fraction in [0.0, 1.0) derived from the current
/// system clock nanoseconds. Used for jitter without a `rand` dependency.
fn simple_jitter_fraction() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos % 1000) as f64 / 1000.0
}

/// FR43: Main event loop multiplexing socket messages, Telegram updates, and cleanup timer.
///
/// Takes a consolidated `Arc<DaemonState>` instead of individual Arc fields.
pub(super) async fn run_event_loop(
    mut socket_rx: tokio::sync::broadcast::Receiver<BridgeMessage>,
    state: Arc<DaemonState>,
    socket_clients: SocketClients,
    mut topic_invalidated_rx: tokio::sync::mpsc::UnboundedReceiver<i64>,
) {
    let mut cleanup_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(CLEANUP_INTERVAL_SECS));
    cleanup_interval.tick().await; // skip first immediate tick

    let mut update_offset: i64 = 0;

    // Fix #4: Track consecutive poll failures for exponential backoff.
    let mut consecutive_poll_failures: u32 = 0;

    // Fix #6: Semaphore bounding concurrent handler tasks to 50.
    // Cleanup tasks are exempt — they must always run regardless of load.
    let handler_semaphore = Arc::new(Semaphore::new(50));

    // Pre-construct a single HandlerContext; .clone() is cheap (Arc refcount bumps).
    let base_ctx = HandlerContext {
        bot: Arc::clone(&state.bot),
        sessions: Arc::clone(&state.sessions),
        injector: Arc::clone(&state.injector),
        session_threads: Arc::clone(&state.session_threads),
        session_tmux: Arc::clone(&state.session_tmux_targets),
        recent_inputs: Arc::clone(&state.recent_telegram_inputs),
        tool_cache: Arc::clone(&state.tool_input_cache),
        compacting: Arc::clone(&state.compacting_sessions),
        pending_del: Arc::clone(&state.pending_deletions),
        custom_titles: Arc::clone(&state.session_custom_titles),
        pending_q: Arc::clone(&state.pending_questions),
        topic_locks: Arc::clone(&state.topic_creation_locks),
        bot_sessions: Arc::clone(&state.bot_sessions),
        mirroring_enabled: Arc::clone(&state.mirroring_enabled),
        config: Arc::clone(&state.config),
        socket_clients,
        pending_approval_clients: Arc::clone(&state.pending_approval_clients),
    };

    loop {
        tokio::select! {
            // Socket messages from hook clients
            msg_result = socket_rx.recv() => {
                match msg_result {
                    Ok(msg) => {
                        let ctx = base_ctx.clone();
                        let sem = handler_semaphore.clone();
                        tokio::spawn(async move {
                            let _permit = sem.acquire().await.expect("semaphore closed");
                            handle_socket_message(ctx, msg).await;
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "Socket broadcast receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("Socket broadcast channel closed");
                        break;
                    }
                }
            }

            // Telegram long-polling (poll every iteration)
            updates = base_ctx.bot.get_updates(update_offset) => {
                match updates {
                    Ok(updates) => {
                        // Fix #4: Reset failure counter on successful poll.
                        consecutive_poll_failures = 0;
                        for update in updates {
                            if update.update_id >= update_offset {
                                update_offset = update.update_id + 1;
                            }
                            let ctx = base_ctx.clone();
                            let sem = handler_semaphore.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await.expect("semaphore closed");
                                telegram_handlers::handle_telegram_update(ctx, update).await;
                            });
                        }
                    }
                    Err(e) => {
                        // Fix #4: Exponential backoff with jitter on poll failures.
                        // Schedule: 10s, 20s, 40s, 80s (cap), with ~20% jitter.
                        consecutive_poll_failures += 1;
                        let base_delay = 5u64
                            .saturating_mul(1u64 << consecutive_poll_failures.min(4));
                        let base_delay = base_delay.min(80);
                        let jitter = (base_delay as f64 * 0.2 * simple_jitter_fraction()) as u64;
                        let next_retry_secs = base_delay + jitter;
                        tracing::error!(
                            error = %e,
                            consecutive_failures = consecutive_poll_failures,
                            next_retry_secs = next_retry_secs,
                            "Failed to get Telegram updates"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_secs(next_retry_secs)).await;
                    }
                }
            }

            // Topic invalidation — a Telegram topic was permanently deleted.
            // Clear the stale thread_id from cache and DB so ensure_session_exists
            // creates a new topic on the next message.
            Some(invalidated_tid) = topic_invalidated_rx.recv() => {
                let ctx = base_ctx.clone();
                tokio::spawn(async move {
                    handle_topic_invalidated(ctx, invalidated_tid).await;
                });
            }

            // Cleanup timer — exempt from semaphore so it always runs.
            _ = cleanup_interval.tick() => {
                let ctx = base_ctx.clone();
                tokio::spawn(async move {
                    cleanup::run_cleanup(ctx).await;
                });
            }
        }
    }
}

/// Handle a permanently deleted Telegram topic by clearing the stale thread_id
/// from both the in-memory cache and the database. This allows
/// `ensure_session_exists` to create a replacement topic on the next message.
async fn handle_topic_invalidated(ctx: HandlerContext, thread_id: i64) {
    // Reverse lookup: find which session owns this thread_id
    let session_id = {
        let threads = ctx.session_threads.read().await;
        threads
            .iter()
            .find(|(_, &tid)| tid == thread_id)
            .map(|(sid, _)| sid.clone())
    };

    let Some(session_id) = session_id else {
        // Not in cache — try DB
        let tid = thread_id;
        let found = ctx
            .db_op(move |sess| {
                sess.get_session_by_thread_id(tid)
                    .ok()
                    .flatten()
                    .map(|s| s.id)
            })
            .await;
        if let Some(sid) = found {
            // Clear from DB
            let sid_clone = sid.clone();
            ctx.db_op(move |sess| {
                let _ = sess.clear_thread_id(&sid_clone);
            })
            .await;
            tracing::info!(
                session_id = %sid,
                thread_id,
                "Cleared stale thread_id from DB (topic permanently deleted)"
            );
        } else {
            tracing::debug!(
                thread_id,
                "No session found for invalidated thread_id"
            );
        }
        return;
    };

    // Clear from in-memory cache
    ctx.session_threads.write().await.remove(&session_id);

    // Clear from DB
    let sid = session_id.clone();
    ctx.db_op(move |sess| {
        let _ = sess.clear_thread_id(&sid);
    })
    .await;

    tracing::info!(
        session_id = %session_id,
        thread_id,
        "Cleared stale thread_id from cache and DB (topic permanently deleted)"
    );
}
