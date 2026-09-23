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

    // ADR-023: the async heartbeat. Its job is to stop landing when no worker can
    // run a task at all — the one failure a watchdog living inside the runtime
    // could never report. It also publishes the free-permit count, which is how
    // handler starvation shows up in the stall log.
    {
        let health = Arc::clone(&state.health);
        let sem = Arc::clone(&handler_semaphore);
        tokio::spawn(async move {
            let mut beat = tokio::time::interval(tokio::time::Duration::from_secs(1));
            loop {
                beat.tick().await;
                health.beat();
                health.set_permits(sem.available_permits());
            }
        });
    }

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
        session_hosts: Arc::clone(&state.session_hosts),
        session_transports: Arc::clone(&state.session_transports),
        session_host_clients: Arc::clone(&state.session_host_clients),
        pending_topic_msgs: Arc::clone(&state.pending_topic_msgs),
        orphaned_topics: Arc::clone(&state.orphaned_topics),
        flush_tx: state.flush_tx.clone(),
    };

    // BUG-002 (review M1): take the flush-request receiver. If it was already
    // taken (should not happen — single event loop), fall back to a dead channel.
    let mut flush_rx = state
        .flush_rx
        .lock()
        .await
        .take()
        .unwrap_or_else(|| tokio::sync::mpsc::unbounded_channel().1);

    // STALE-TOPICS: reconcile topics against live tmux/Claude state ONCE at startup,
    // before entering the loop. After a daemon restart or machine reboot the DB can hold
    // many "active" sessions whose panes are long gone; this drains that backlog
    // immediately instead of waiting out the first 20-minute cleanup tick (and the old
    // 24h pane-liveness gate). Higher cap than the per-cycle sweep so a large backlog
    // clears in one pass.
    let startup_pruned = reconcile::reconcile_topics_startup(&base_ctx).await;
    if startup_pruned > 0 {
        tracing::info!(
            startup_pruned,
            "STALE-TOPICS: startup reconcile pruned dead topics"
        );
    }

    loop {
        tokio::select! {
            // Socket messages from hook clients
            msg_result = socket_rx.recv() => {
                match msg_result {
                    Ok(msg) => {
                        let ctx = base_ctx.clone();
                        let sem = handler_semaphore.clone();
                        // ADR-023: registered before the permit is acquired, so a
                        // handler starved of a permit is visible as in-flight too —
                        // that is what permit exhaustion looks like from outside.
                        let health = Arc::clone(&state.health);
                        let id = health.reserve(
                            "event",
                            format!("{} {}", msg.msg_type, short_id(&msg.session_id)),
                        );
                        let guard_health = Arc::clone(&health);
                        let task = tokio::spawn(async move {
                            let _done = guard_health.guard(id);
                            let _permit = sem.acquire().await.expect("semaphore closed");
                            handle_socket_message(ctx, msg).await;
                        });
                        health.attach_abort(id, task.abort_handle());
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

            // BUG-002 (review M1): service flush requests — a session whose topic
            // just appeared after some of its events were buffered.
            Some(session_id) = flush_rx.recv() => {
                let ctx = base_ctx.clone();
                let sem = handler_semaphore.clone();
                let health = Arc::clone(&state.health);
                let id = health.reserve("flush", short_id(&session_id));
                let guard_health = Arc::clone(&health);
                let task = tokio::spawn(async move {
                    let _done = guard_health.guard(id);
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    super::flush_pending_for_session(&ctx, &session_id).await;
                });
                health.attach_abort(id, task.abort_handle());
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
                            let health = Arc::clone(&state.health);
                            let id = health.reserve("telegram", format!("update {}", update.update_id));
                            let guard_health = Arc::clone(&health);
                            let task = tokio::spawn(async move {
                                let _done = guard_health.guard(id);
                                let _permit = sem.acquire().await.expect("semaphore closed");
                                telegram_handlers::handle_telegram_update(ctx, update).await;
                            });
                            health.attach_abort(id, task.abort_handle());
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
                let health = Arc::clone(&state.health);
                let id = health.reserve("cleanup", "periodic sweep");
                let guard_health = Arc::clone(&health);
                let task = tokio::spawn(async move {
                    let _done = guard_health.guard(id);
                    cleanup::run_cleanup(ctx).await;
                });
                health.attach_abort(id, task.abort_handle());
            }
        }
    }
}

/// Short, log-friendly session id for the in-flight table.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// A topic Telegram says no longer exists (deleted in the app, or by ctm).
///
/// ADR-024: the messages already waiting for it are *held* in the outbox, not dropped.
/// If its session is still going, ctm makes a replacement topic and moves them there;
/// if the session has ended, there is nothing to post to and they are discarded (the
/// daemon waits for a finished session's outbox to drain before deleting its topic,
/// so this is not where replies are lost).
async fn handle_topic_invalidated(ctx: HandlerContext, thread_id: i64) {
    // Owner: the in-memory map first, then the store.
    let cached = {
        let threads = ctx.session_threads.read().await;
        threads
            .iter()
            .find(|(_, &tid)| tid == thread_id)
            .map(|(sid, _)| sid.clone())
    };
    let session = match cached {
        Some(sid) => {
            let sid2 = sid.clone();
            ctx.db_op(move |sess| sess.get_session(&sid2).ok().flatten())
                .await
        }
        None => {
            ctx.db_op(move |sess| sess.get_session_by_thread_id(thread_id).ok().flatten())
                .await
        }
    };

    let Some(session) = session else {
        let n = ctx.bot.discard_topic(thread_id).await;
        if n > 0 {
            tracing::warn!(
                thread_id,
                discarded = n,
                "ADR-024: messages were waiting for a topic no session owns; discarded"
            );
        }
        return;
    };

    // Forget the dead topic, but only if the session still points at it: several
    // queued messages can report the same dead topic, and a replacement may already
    // have been made by the time the later reports arrive.
    {
        let mut threads = ctx.session_threads.write().await;
        if threads.get(&session.id) == Some(&thread_id) {
            threads.remove(&session.id);
        }
    }
    if session.thread_id == Some(thread_id) {
        let sid = session.id.clone();
        ctx.db_op(move |sess| {
            let _ = sess.clear_thread_id(&sid);
        })
        .await;
    }

    if session.status != crate::types::SessionStatus::Active {
        let n = ctx.bot.discard_topic(thread_id).await;
        tracing::info!(
            session_id = %session.id,
            thread_id,
            discarded = n,
            "ADR-024: topic of an ended session is gone; nothing left to post to"
        );
        return;
    }

    ctx.orphaned_topics
        .write()
        .await
        .entry(session.id.clone())
        .or_default()
        .push(thread_id);
    tracing::info!(
        session_id = %session.id,
        thread_id,
        "ADR-024: topic is gone while its session is live — making a replacement for the held messages"
    );

    // Make the replacement now rather than waiting for the session's next event: the
    // messages being held are the ones the user is waiting for. Topic creation fails
    // fast under a rate limit (ADR-023), so keep trying, spaced out, until it lands.
    let probe = crate::types::BridgeMessage {
        msg_type: crate::types::MessageType::AgentResponse,
        session_id: session.id.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: String::new(),
        metadata: None,
    };
    for _ in 0..60 {
        super::ensure_session_exists(&ctx, &probe).await;
        if let Some(new_tid) = ctx.get_thread_id(&session.id).await {
            super::adopt_orphans(&ctx, &session.id, new_tid).await;
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
    }
    tracing::error!(
        session_id = %session.id,
        thread_id,
        "ADR-024: could not make a replacement topic in 30 minutes; the held messages go out when the session's next event creates one"
    );
}
