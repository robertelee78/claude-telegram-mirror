//! Main event loop multiplexing socket messages, Telegram updates, and cleanup timer.

use super::*;

/// Main event loop multiplexing socket messages, Telegram updates, and cleanup timer.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_event_loop(
    mut socket_rx: tokio::sync::broadcast::Receiver<BridgeMessage>,
    bot: Arc<TelegramBot>,
    sessions: Arc<Mutex<SessionManager>>,
    injector: Arc<Mutex<InputInjector>>,
    session_threads: Arc<RwLock<HashMap<String, i64>>>,
    session_tmux: Arc<RwLock<HashMap<String, String>>>,
    recent_inputs: Arc<RwLock<HashSet<String>>>,
    tool_cache: Arc<RwLock<HashMap<String, CachedToolInput>>>,
    compacting: Arc<RwLock<HashSet<String>>>,
    pending_del: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    custom_titles: Arc<RwLock<HashMap<String, String>>>,
    pending_q: Arc<RwLock<HashMap<String, PendingQuestion>>>,
    topic_locks: Arc<RwLock<HashMap<String, Arc<TopicCreationState>>>>,
    bot_sessions: Arc<RwLock<HashMap<i64, BotSessionState>>>,
    mirroring_enabled: Arc<std::sync::atomic::AtomicBool>,
    config: Arc<Config>,
    socket_clients: SocketClients,
) {
    let mut cleanup_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(CLEANUP_INTERVAL_SECS));
    cleanup_interval.tick().await; // skip first immediate tick

    let mut update_offset: i64 = 0;

    // Pre-construct a single HandlerContext; .clone() is cheap (Arc refcount bumps).
    let base_ctx = HandlerContext {
        bot,
        sessions,
        injector,
        session_threads,
        session_tmux,
        recent_inputs,
        tool_cache,
        compacting,
        pending_del,
        custom_titles,
        pending_q,
        topic_locks,
        bot_sessions,
        mirroring_enabled,
        config,
        socket_clients,
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
