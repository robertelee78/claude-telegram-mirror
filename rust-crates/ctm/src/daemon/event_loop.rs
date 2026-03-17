//! Main event loop multiplexing socket messages, Telegram updates, and cleanup timer.

use super::*;

/// FR43: Main event loop multiplexing socket messages, Telegram updates, and cleanup timer.
///
/// Takes a consolidated `Arc<DaemonState>` instead of individual Arc fields.
pub(super) async fn run_event_loop(
    mut socket_rx: tokio::sync::broadcast::Receiver<BridgeMessage>,
    state: Arc<DaemonState>,
    socket_clients: SocketClients,
) {
    let mut cleanup_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(CLEANUP_INTERVAL_SECS));
    cleanup_interval.tick().await; // skip first immediate tick

    let mut update_offset: i64 = 0;

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
                        tokio::spawn(async move {
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
                        for update in updates {
                            if update.update_id >= update_offset {
                                update_offset = update.update_id + 1;
                            }
                            let ctx = base_ctx.clone();
                            tokio::spawn(async move {
                                telegram_handlers::handle_telegram_update(ctx, update).await;
                            });
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to get Telegram updates");
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }
                }
            }

            // Cleanup timer
            _ = cleanup_interval.tick() => {
                let ctx = base_ctx.clone();
                tokio::spawn(async move {
                    cleanup::run_cleanup(ctx).await;
                });
            }
        }
    }
}
