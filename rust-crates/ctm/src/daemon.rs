//! Bridge Daemon — central coordinator for Claude Code <-> Telegram bridge.
//!
//! Ported from `daemon.ts` (1315 lines). This is the heart of the application.
//!
//! ## Architecture
//! The daemon is an async loop that:
//! 1. Starts a socket server (from socket.rs)
//! 2. Starts Telegram long-polling (from bot.rs)
//! 3. Listens for messages on the socket
//! 4. Dispatches messages to handlers
//! 5. Routes Telegram messages back to tmux via the injector
//!
//! ## Bug fixes preserved
//! BUG-001 through BUG-012 — see inline comments.

use crate::bot::{CallbackQuery, InlineButton, SendOptions, TelegramBot, TgMessage, Update};
use crate::config::{get_config_dir, Config};
use crate::error::Result;
use crate::formatting::{
    format_agent_response, format_approval_request, format_error, format_session_end,
    format_session_start, format_tool_details, format_tool_execution,
};
use crate::injector::InputInjector;
use crate::session::SessionManager;
use crate::socket::SocketServer;
use crate::summarize::{summarize_tool_action, summarize_tool_result};
use crate::types::{is_valid_session_id, BridgeMessage};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock};

// ---------------------------------------------------------------- constants

const CLEANUP_INTERVAL_SECS: u64 = 5 * 60; // 5 minutes
const ECHO_TTL_SECS: u64 = 10;
const TOOL_CACHE_TTL_SECS: u64 = 5 * 60; // 5 minutes
const QUESTION_TTL_SECS: u64 = 10 * 60; // 10 minutes
const DOWNLOAD_MAX_AGE_SECS: u64 = 24 * 60 * 60; // 24 hours
const TMUX_SESSION_TIMEOUT_HOURS: u32 = 24;
const NO_TMUX_SESSION_TIMEOUT_HOURS: u32 = 1;

// ---------------------------------------------------------------- types

/// Cached tool input for the "Details" button.
struct CachedToolInput {
    tool: String,
    input: serde_json::Value,
    timestamp: std::time::Instant,
}

/// Pending AskUserQuestion state.
struct PendingQuestion {
    session_id: String,
    questions: Vec<QuestionDef>,
    answered: Vec<bool>,
    selected_options: HashMap<usize, HashSet<usize>>,
    timestamp: std::time::Instant,
}

#[derive(Clone)]
struct QuestionDef {
    question: String,
    header: String,
    options: Vec<OptionDef>,
    multi_select: bool,
}

#[derive(Clone)]
struct OptionDef {
    label: String,
    #[allow(dead_code)]
    description: String,
}

/// Topic creation lock for BUG-002 prevention.
/// When a session is being created, concurrent handlers wait on this.
struct TopicCreationState {
    notify: Arc<tokio::sync::Notify>,
    resolved: bool,
}

/// Per-thread bot session state (mirrors grammY session in TypeScript).
struct BotSessionState {
    attached_session_id: Option<String>,
    muted: bool,
    last_activity: u64,
}

// ---------------------------------------------------------------- daemon

/// Bridge Daemon — orchestrates all components.
pub struct Daemon {
    config: Config,
    bot: Arc<TelegramBot>,
    sessions: Arc<Mutex<SessionManager>>,
    injector: Arc<Mutex<InputInjector>>,
    socket: Option<SocketServer>,

    // In-memory caches
    session_threads: Arc<RwLock<HashMap<String, i64>>>,
    session_tmux_targets: Arc<RwLock<HashMap<String, String>>>,
    recent_telegram_inputs: Arc<RwLock<HashSet<String>>>,
    tool_input_cache: Arc<RwLock<HashMap<String, CachedToolInput>>>,
    compacting_sessions: Arc<RwLock<HashSet<String>>>,
    pending_deletions: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    session_custom_titles: Arc<RwLock<HashMap<String, String>>>,
    pending_questions: Arc<RwLock<HashMap<String, PendingQuestion>>>,

    // BUG-002: Topic creation locks
    topic_creation_locks: Arc<RwLock<HashMap<String, Arc<TopicCreationState>>>>,

    // Per-thread bot session state (keyed by thread_id / message_thread_id)
    bot_sessions: Arc<RwLock<HashMap<i64, BotSessionState>>>,
}

impl Daemon {
    pub fn new(config: Config) -> Result<Self> {
        let bot = Arc::new(TelegramBot::new(&config));
        let sessions = SessionManager::new(&config.config_dir, config.session_timeout)?;

        Ok(Self {
            config,
            bot,
            sessions: Arc::new(Mutex::new(sessions)),
            injector: Arc::new(Mutex::new(InputInjector::new())),
            socket: None,
            session_threads: Arc::new(RwLock::new(HashMap::new())),
            session_tmux_targets: Arc::new(RwLock::new(HashMap::new())),
            recent_telegram_inputs: Arc::new(RwLock::new(HashSet::new())),
            tool_input_cache: Arc::new(RwLock::new(HashMap::new())),
            compacting_sessions: Arc::new(RwLock::new(HashSet::new())),
            pending_deletions: Arc::new(RwLock::new(HashMap::new())),
            session_custom_titles: Arc::new(RwLock::new(HashMap::new())),
            pending_questions: Arc::new(RwLock::new(HashMap::new())),
            topic_creation_locks: Arc::new(RwLock::new(HashMap::new())),
            bot_sessions: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Start the daemon. Runs until shutdown signal.
    pub async fn start(&mut self) -> Result<()> {
        tracing::info!("Starting bridge daemon...");

        // Start socket server
        let pid_path = self.config.config_dir.join("bridge.pid");
        let mut socket = SocketServer::new(&self.config.socket_path, &pid_path);
        socket.listen().await?;
        let socket_rx = socket.subscribe();
        let daemon_socket_clients = socket.clients_ref();
        self.socket = Some(socket);

        // Verify bot connectivity
        match self.bot.get_me().await {
            Ok(me) => {
                tracing::info!(
                    username = ?me.username,
                    "Bot connected"
                );
            }
            Err(e) => {
                tracing::error!(error = %e, "Bot connectivity check failed");
                return Err(e);
            }
        }

        // H9: Auto-detect tmux session at startup
        {
            let mut inj = self.injector.lock().await;
            if let Some(info) = InputInjector::detect_tmux_session() {
                inj.set_target(&info.target, info.socket.as_deref());
                tracing::info!(
                    target = %info.target,
                    session = %info.session,
                    "Input injector auto-detected tmux session at startup"
                );
            } else if let Some(session) = InputInjector::find_claude_code_session() {
                inj.set_target(&session, None);
                tracing::info!(
                    target = %session,
                    "Input injector found Claude Code tmux session at startup"
                );
            } else {
                tracing::info!(
                    "No tmux session detected at startup — will use per-session targets"
                );
            }
        }

        // Send startup notification
        self.bot
            .send_message(
                "\u{1F7E2} *Bridge Daemon Started*\n\nClaude Code sessions will now be mirrored here.",
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                None,
            )
            .await;

        // Spawn main event loop
        let daemon_bot = Arc::clone(&self.bot);
        let daemon_sessions = Arc::clone(&self.sessions);
        let daemon_injector = Arc::clone(&self.injector);
        let daemon_session_threads = Arc::clone(&self.session_threads);
        let daemon_session_tmux = Arc::clone(&self.session_tmux_targets);
        let daemon_recent_inputs = Arc::clone(&self.recent_telegram_inputs);
        let daemon_tool_cache = Arc::clone(&self.tool_input_cache);
        let daemon_compacting = Arc::clone(&self.compacting_sessions);
        let daemon_pending_del = Arc::clone(&self.pending_deletions);
        let daemon_custom_titles = Arc::clone(&self.session_custom_titles);
        let daemon_pending_q = Arc::clone(&self.pending_questions);
        let daemon_topic_locks = Arc::clone(&self.topic_creation_locks);
        let daemon_bot_sessions = Arc::clone(&self.bot_sessions);
        let daemon_config = self.config.clone();

        tokio::spawn(async move {
            run_event_loop(
                socket_rx,
                daemon_bot,
                daemon_sessions,
                daemon_injector,
                daemon_session_threads,
                daemon_session_tmux,
                daemon_recent_inputs,
                daemon_tool_cache,
                daemon_compacting,
                daemon_pending_del,
                daemon_custom_titles,
                daemon_pending_q,
                daemon_topic_locks,
                daemon_bot_sessions,
                daemon_config,
                daemon_socket_clients,
            )
            .await;
        });

        tracing::info!("Bridge daemon started");
        Ok(())
    }

    /// Stop the daemon gracefully.
    pub async fn stop(self) {
        tracing::info!("Stopping bridge daemon...");

        // Send shutdown notification
        self.bot
            .send_message(
                "\u{1F534} *Bridge Daemon Stopped*\n\nSession mirroring is now disabled.",
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                None,
            )
            .await;

        // Close socket server
        if let Some(socket) = self.socket {
            socket.close().await;
        }

        tracing::info!("Bridge daemon stopped");
    }
}

// ====================================================================== event loop

/// Main event loop multiplexing socket messages, Telegram updates, and cleanup timer.
#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
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
    config: Config,
    socket_clients: SocketClients,
) {
    let mut cleanup_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(CLEANUP_INTERVAL_SECS));
    cleanup_interval.tick().await; // skip first immediate tick

    let mut update_offset: i64 = 0;

    loop {
        tokio::select! {
            // Socket messages from hook clients
            msg_result = socket_rx.recv() => {
                match msg_result {
                    Ok(msg) => {
                        let ctx = HandlerContext {
                            bot: Arc::clone(&bot),
                            sessions: Arc::clone(&sessions),
                            injector: Arc::clone(&injector),
                            session_threads: Arc::clone(&session_threads),
                            session_tmux: Arc::clone(&session_tmux),
                            recent_inputs: Arc::clone(&recent_inputs),
                            tool_cache: Arc::clone(&tool_cache),
                            compacting: Arc::clone(&compacting),
                            pending_del: Arc::clone(&pending_del),
                            custom_titles: Arc::clone(&custom_titles),
                            pending_q: Arc::clone(&pending_q),
                            topic_locks: Arc::clone(&topic_locks),
                            bot_sessions: Arc::clone(&bot_sessions),
                            config: config.clone(),
                            socket_clients: Arc::clone(&socket_clients),
                        };
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
            updates = bot.get_updates(update_offset) => {
                match updates {
                    Ok(updates) => {
                        for update in updates {
                            if update.update_id >= update_offset {
                                update_offset = update.update_id + 1;
                            }
                            let ctx = HandlerContext {
                                bot: Arc::clone(&bot),
                                sessions: Arc::clone(&sessions),
                                injector: Arc::clone(&injector),
                                session_threads: Arc::clone(&session_threads),
                                session_tmux: Arc::clone(&session_tmux),
                                recent_inputs: Arc::clone(&recent_inputs),
                                tool_cache: Arc::clone(&tool_cache),
                                compacting: Arc::clone(&compacting),
                                pending_del: Arc::clone(&pending_del),
                                custom_titles: Arc::clone(&custom_titles),
                                pending_q: Arc::clone(&pending_q),
                                topic_locks: Arc::clone(&topic_locks),
                                bot_sessions: Arc::clone(&bot_sessions),
                                config: config.clone(),
                                socket_clients: Arc::clone(&socket_clients),
                            };
                            tokio::spawn(async move {
                                handle_telegram_update(ctx, update).await;
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
                let ctx = HandlerContext {
                    bot: Arc::clone(&bot),
                    sessions: Arc::clone(&sessions),
                    injector: Arc::clone(&injector),
                    session_threads: Arc::clone(&session_threads),
                    session_tmux: Arc::clone(&session_tmux),
                    recent_inputs: Arc::clone(&recent_inputs),
                    tool_cache: Arc::clone(&tool_cache),
                    compacting: Arc::clone(&compacting),
                    pending_del: Arc::clone(&pending_del),
                    custom_titles: Arc::clone(&custom_titles),
                    pending_q: Arc::clone(&pending_q),
                    topic_locks: Arc::clone(&topic_locks),
                    bot_sessions: Arc::clone(&bot_sessions),
                    config: config.clone(),
                    socket_clients: Arc::clone(&socket_clients),
                };
                tokio::spawn(async move {
                    run_cleanup(ctx).await;
                });
            }
        }
    }
}

// ====================================================================== context

/// Type alias for the connected-client map shared with `SocketServer`.
type SocketClients = Arc<Mutex<HashMap<String, Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>>>>;

/// Shared context passed to all handlers.
#[derive(Clone)]
struct HandlerContext {
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
    /// Per-thread bot session state (keyed by thread_id).
    bot_sessions: Arc<RwLock<HashMap<i64, BotSessionState>>>,
    config: Config,
    /// Shared reference to the socket's connected-client map for outbound broadcasts.
    socket_clients: SocketClients,
}

impl HandlerContext {
    /// Get thread ID for a session (memory cache -> DB fallback).
    async fn get_thread_id(&self, session_id: &str) -> Option<i64> {
        // Check memory cache
        if let Some(tid) = self.session_threads.read().await.get(session_id) {
            return Some(*tid);
        }
        // Fallback to database
        let sess = self.sessions.lock().await;
        let db_session = sess.get_session(session_id).ok().flatten();
        if let Some(s) = db_session {
            if let Some(tid) = s.thread_id {
                self.session_threads
                    .write()
                    .await
                    .insert(session_id.to_string(), tid);
                return Some(tid);
            }
        }
        None
    }

    /// Wait for topic creation to complete (BUG-002 fix).
    async fn wait_for_topic(&self, session_id: &str) -> Option<i64> {
        // Fast path
        if let Some(tid) = self.get_thread_id(session_id).await {
            return Some(tid);
        }

        // Check if there's a pending creation
        let lock = {
            let locks = self.topic_locks.read().await;
            locks.get(session_id).cloned()
        };

        if let Some(state) = lock {
            if !state.resolved {
                // Wait up to 5 seconds
                let _ = tokio::time::timeout(
                    tokio::time::Duration::from_secs(5),
                    state.notify.notified(),
                )
                .await;
            }
            // Check again after notification
            return self.get_thread_id(session_id).await;
        }

        None
    }

    /// Format topic name for a session.
    fn format_topic_name(
        session_id: &str,
        hostname: Option<&str>,
        project_dir: Option<&str>,
    ) -> String {
        let mut parts = Vec::new();
        if let Some(h) = hostname {
            parts.push(h.to_string());
        }
        if let Some(p) = project_dir {
            let basename = p.rsplit('/').next().unwrap_or(p);
            parts.push(basename.to_string());
        }
        let short_id = session_id.strip_prefix("session-").unwrap_or(session_id);
        let short_id = &short_id[..std::cmp::min(8, short_id.len())];
        parts.push(short_id.to_string());

        if parts.is_empty() {
            format!("Session {short_id}")
        } else {
            parts.join(" \u{2022} ")
        }
    }

    /// Truncate a file path to show basename and parent.
    fn truncate_path(path: &str) -> String {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() <= 3 {
            return path.to_string();
        }
        format!(".../{}", parts[parts.len() - 2..].join("/"))
    }
}

// ====================================================================== socket handler

/// Dispatch a socket message to the appropriate handler.
async fn handle_socket_message(ctx: HandlerContext, msg: BridgeMessage) {
    // Validate session ID
    if !is_valid_session_id(&msg.session_id) {
        tracing::warn!(
            session_id_prefix = %msg.session_id.chars().take(20).collect::<String>(),
            msg_type = %msg.msg_type,
            "Invalid session ID, dropping message"
        );
        return;
    }

    tracing::debug!(msg_type = %msg.msg_type, session_id = %msg.session_id, "Received socket message");

    // Update session activity
    {
        let sess = ctx.sessions.lock().await;
        if sess.get_session(&msg.session_id).ok().flatten().is_some() {
            let _ = sess.update_activity(&msg.session_id);
        }
    }

    // BUG-001: Auto-update tmux target
    check_and_update_tmux_target(&ctx, &msg).await;

    match msg.msg_type.as_str() {
        "session_start" => handle_session_start(&ctx, &msg).await,
        "session_end" => handle_session_end(&ctx, &msg).await,
        "agent_response" => {
            ensure_session_exists(&ctx, &msg).await;
            handle_agent_response(&ctx, &msg).await;
        }
        "tool_start" => {
            ensure_session_exists(&ctx, &msg).await;
            handle_tool_start(&ctx, &msg).await;
        }
        "tool_result" => {
            ensure_session_exists(&ctx, &msg).await;
            handle_tool_result(&ctx, &msg).await;
        }
        "user_input" => {
            ensure_session_exists(&ctx, &msg).await;
            handle_user_input(&ctx, &msg).await;
        }
        "approval_request" => {
            ensure_session_exists(&ctx, &msg).await;
            handle_approval_request(&ctx, &msg).await;
        }
        "error" => {
            ensure_session_exists(&ctx, &msg).await;
            handle_error(&ctx, &msg).await;
        }
        "turn_complete" => {
            tracing::debug!(session_id = %msg.session_id, "Turn complete");
            if ctx.compacting.read().await.contains(&msg.session_id) {
                handle_compact_complete(&ctx, &msg.session_id).await;
            }
        }
        "pre_compact" => {
            ensure_session_exists(&ctx, &msg).await;
            handle_pre_compact(&ctx, &msg).await;
        }
        "session_rename" => {
            handle_session_rename(&ctx, &msg.session_id, &msg.content).await;
        }
        _ => {
            tracing::debug!(msg_type = %msg.msg_type, "Unknown message type");
        }
    }

    // After processing, check transcript for custom-title rename (Epic 5)
    if let Some(meta) = &msg.metadata {
        if let Some(tp) = meta.get("transcript_path").and_then(|v| v.as_str()) {
            if let Some(title) = check_for_session_rename(tp, &msg.session_id, &ctx.custom_titles) {
                handle_session_rename(&ctx, &msg.session_id, &title).await;
            }
        }
    }
}

// ====================================================================== message handlers

/// BUG-001: Auto-update tmux target on every message.
async fn check_and_update_tmux_target(ctx: &HandlerContext, msg: &BridgeMessage) {
    let meta = match &msg.metadata {
        Some(m) => m,
        None => return,
    };

    let new_target = match meta.get("tmuxTarget").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return,
    };
    let new_socket = meta.get("tmuxSocket").and_then(|v| v.as_str());

    let current = ctx.session_tmux.read().await.get(&msg.session_id).cloned();
    if current.as_deref() == Some(new_target) {
        return;
    }

    tracing::info!(
        session_id = %msg.session_id,
        old = ?current,
        new = new_target,
        "Tmux target changed, auto-updating"
    );

    ctx.session_tmux
        .write()
        .await
        .insert(msg.session_id.clone(), new_target.to_string());

    let _ = ctx
        .sessions
        .lock()
        .await
        .set_tmux_info(&msg.session_id, Some(new_target), new_socket);
}

/// Handler 1: session_start
async fn handle_session_start(ctx: &HandlerContext, msg: &BridgeMessage) {
    let meta = msg.metadata.as_ref();
    let hostname = meta
        .and_then(|m| m.get("hostname"))
        .and_then(|v| v.as_str());
    let project_dir = meta
        .and_then(|m| m.get("projectDir"))
        .and_then(|v| v.as_str());
    let tmux_target = meta
        .and_then(|m| m.get("tmuxTarget"))
        .and_then(|v| v.as_str());
    let tmux_socket = meta
        .and_then(|m| m.get("tmuxSocket"))
        .and_then(|v| v.as_str());

    // Create session in DB
    {
        let sess = ctx.sessions.lock().await;
        let _ = sess.create_session(&msg.session_id, ctx.config.chat_id, hostname, project_dir);

        if let (Some(target), _) = (tmux_target, tmux_socket) {
            let _ = sess.set_tmux_info(&msg.session_id, Some(target), tmux_socket);
        }
    }

    // Cache tmux target
    if let Some(target) = tmux_target {
        ctx.session_tmux
            .write()
            .await
            .insert(msg.session_id.clone(), target.to_string());
    }

    // BUG-012: Cancel pending topic deletion if session resumes
    cancel_pending_topic_deletion(ctx, &msg.session_id).await;

    // Check if session already has a thread (daemon restart scenario)
    let existing_thread = {
        let sess = ctx.sessions.lock().await;
        sess.get_session(&msg.session_id)
            .ok()
            .flatten()
            .and_then(|s| s.thread_id)
    };

    let thread_id = if let Some(tid) = existing_thread {
        ctx.session_threads
            .write()
            .await
            .insert(msg.session_id.clone(), tid);
        tracing::info!(session_id = %msg.session_id, thread_id = tid, "Reusing existing thread");
        Some(tid)
    } else if ctx.config.use_threads {
        let topic_name = HandlerContext::format_topic_name(&msg.session_id, hostname, project_dir);
        match ctx.bot.create_forum_topic(&topic_name, 0).await {
            Ok(Some(tid)) => {
                let sess = ctx.sessions.lock().await;
                let _ = sess.set_session_thread(&msg.session_id, tid);
                ctx.session_threads
                    .write()
                    .await
                    .insert(msg.session_id.clone(), tid);
                Some(tid)
            }
            _ => None,
        }
    } else {
        None
    };

    // BUG-002: Resolve topic creation lock
    {
        let locks = ctx.topic_locks.read().await;
        if let Some(state) = locks.get(&msg.session_id) {
            state.notify.notify_waiters();
        }
    }
    ctx.topic_locks.write().await.remove(&msg.session_id);

    // Build and send session info
    let mut session_info = format_session_start(&msg.session_id, project_dir, hostname);
    if let Some(target) = tmux_target {
        session_info.push_str(&format!("\n\u{1F4FA} tmux: `{target}`"));
    }

    ctx.bot
        .send_message(
            &session_info,
            Some(&SendOptions {
                parse_mode: Some("Markdown".into()),
                ..Default::default()
            }),
            thread_id,
        )
        .await;

    // Remove auto-pin
    if let Some(tid) = thread_id {
        let _ = ctx.bot.unpin_all_topic_messages(tid).await;
    }
}

/// Handler 2: session_end
async fn handle_session_end(ctx: &HandlerContext, msg: &BridgeMessage) {
    let session_opt = ctx
        .sessions
        .lock()
        .await
        .get_session(&msg.session_id)
        .ok()
        .flatten();

    if let Some(session) = session_opt {
        let started_at = chrono::DateTime::parse_from_rfc3339(&session.started_at).ok();
        let duration_ms = started_at.map(|s| {
            let elapsed = chrono::Utc::now() - s.to_utc();
            elapsed.num_milliseconds().max(0) as u64
        });

        let thread_id = ctx.get_thread_id(&msg.session_id).await;

        ctx.bot
            .send_message(
                &format_session_end(&msg.session_id, duration_ms),
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                thread_id,
            )
            .await;

        if let Some(tid) = thread_id {
            if ctx.config.auto_delete_topics {
                let delay_ms = ctx.config.topic_delete_delay_minutes as u64 * 60 * 1000;
                schedule_topic_deletion(ctx, &msg.session_id, tid, delay_ms).await;
            } else {
                let _ = ctx.bot.close_forum_topic(tid).await;
                ctx.session_threads.write().await.remove(&msg.session_id);
            }
        }

        // Clean up caches
        ctx.session_tmux.write().await.remove(&msg.session_id);
        ctx.custom_titles.write().await.remove(&msg.session_id);
        cleanup_pending_questions(ctx, &msg.session_id).await;

        let _ = ctx
            .sessions
            .lock()
            .await
            .end_session(&msg.session_id, "ended");
    }
}

/// Handler 3: agent_response
async fn handle_agent_response(ctx: &HandlerContext, msg: &BridgeMessage) {
    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        tracing::error!(session_id = %msg.session_id, "Topic creation timeout - dropping agent_response");
        return;
    }

    // Check mute state: if the thread is muted, suppress the message.
    if let Some(tid) = thread_id {
        let bs = ctx.bot_sessions.read().await;
        if bs.get(&tid).map(|s| s.muted).unwrap_or(false) {
            tracing::debug!(session_id = %msg.session_id, thread_id = tid, "Thread is muted, suppressing agent_response");
            return;
        }
    }

    ctx.bot
        .send_message(
            &format_agent_response(&msg.content),
            Some(&SendOptions {
                parse_mode: Some("Markdown".into()),
                ..Default::default()
            }),
            thread_id,
        )
        .await;
}

/// Handler 4: tool_start
async fn handle_tool_start(ctx: &HandlerContext, msg: &BridgeMessage) {
    let meta = msg.metadata.as_ref();
    let tool_name = meta
        .and_then(|m| m.get("tool"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");

    // Intercept AskUserQuestion tool
    if tool_name == "AskUserQuestion" {
        handle_ask_user_question(ctx, msg).await;
        return;
    }

    // Only show tool starts in verbose mode
    if !ctx.config.verbose {
        return;
    }

    let tool_input = meta
        .and_then(|m| m.get("input"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        return;
    }

    // Format brief preview
    let preview = format_tool_preview(tool_name, &tool_input);

    // Generate unique tool use ID and cache input
    let tool_use_id = format!(
        "tool_{}_{}",
        chrono::Utc::now().timestamp_millis(),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    {
        let mut cache = ctx.tool_cache.write().await;
        cache.insert(
            tool_use_id.clone(),
            CachedToolInput {
                tool: tool_name.to_string(),
                input: tool_input.clone(),
                timestamp: std::time::Instant::now(),
            },
        );
    }

    // Schedule cache expiry
    let cache_ref = Arc::clone(&ctx.tool_cache);
    let tuid = tool_use_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(TOOL_CACHE_TTL_SECS)).await;
        cache_ref.write().await.remove(&tuid);
    });

    let summary = if tool_input.is_null() {
        format!("Using {tool_name}")
    } else {
        summarize_tool_action(tool_name, &tool_input)
    };

    let text = format!("\u{1F527} {summary}\n    Tool: `{tool_name}`{preview}");

    if !tool_input.is_null() && tool_input.as_object().is_some_and(|o| !o.is_empty()) {
        ctx.bot
            .send_with_buttons(
                &text,
                vec![InlineButton {
                    text: "\u{1F4CB} Details".into(),
                    callback_data: format!("tooldetails:{tool_use_id}"),
                }],
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                thread_id,
            )
            .await;
    } else {
        ctx.bot
            .send_message(
                &text,
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                thread_id,
            )
            .await;
    }
}

/// Format a brief tool preview for the tool_start message.
fn format_tool_preview(tool_name: &str, input: &serde_json::Value) -> String {
    let s = |key: &str| input.get(key).and_then(|v| v.as_str()).unwrap_or("");

    match tool_name {
        "Read" | "Write" | "Edit" => {
            let fp = s("file_path");
            if !fp.is_empty() {
                format!(" `{}`", HandlerContext::truncate_path(fp))
            } else {
                String::new()
            }
        }
        "Bash" => {
            let cmd = s("command");
            if !cmd.is_empty() {
                let truncated = &cmd[..std::cmp::min(50, cmd.len())];
                let suffix = if cmd.len() > 50 { "..." } else { "" };
                format!("\n`{truncated}{suffix}`")
            } else {
                String::new()
            }
        }
        "Grep" | "Glob" => {
            let pattern = s("pattern");
            if !pattern.is_empty() {
                format!(" `{pattern}`")
            } else {
                String::new()
            }
        }
        "Task" => {
            let desc = s("description");
            if !desc.is_empty() {
                format!(" {desc}")
            } else {
                String::new()
            }
        }
        "WebFetch" => {
            let url = s("url");
            if !url.is_empty() {
                let truncated = &url[..std::cmp::min(40, url.len())];
                format!(" `{truncated}...`")
            } else {
                String::new()
            }
        }
        "WebSearch" => {
            let query = s("query");
            if !query.is_empty() {
                format!(" \"{query}\"")
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// Handler 5: tool_result
async fn handle_tool_result(ctx: &HandlerContext, msg: &BridgeMessage) {
    if !ctx.config.verbose {
        return;
    }

    let meta = msg.metadata.as_ref();
    let tool_name = meta
        .and_then(|m| m.get("tool"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    // H10: tool_input is stored as a JSON Value (object), not a plain string.
    // .as_str() always returns None for objects — use to_string() / as_str() on the
    // owned serialization instead.
    let tool_input_owned: Option<String> = meta.and_then(|m| m.get("input")).map(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| v.to_string())
    });
    let tool_input = tool_input_owned.as_deref();

    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        return;
    }

    let result_summary = if msg.content.is_empty() {
        "Completed (no output)".to_string()
    } else {
        summarize_tool_result(tool_name, &msg.content)
    };

    let formatted = format_tool_execution(
        tool_name,
        tool_input,
        Some(&msg.content),
        ctx.config.verbose,
    );

    ctx.bot
        .send_message(
            &format!("\u{2705} {result_summary}\n{formatted}"),
            Some(&SendOptions {
                parse_mode: Some("Markdown".into()),
                ..Default::default()
            }),
            thread_id,
        )
        .await;
}

/// Handler 6: user_input — BUG-011 echo prevention.
async fn handle_user_input(ctx: &HandlerContext, msg: &BridgeMessage) {
    let source = msg
        .metadata
        .as_ref()
        .and_then(|m| m.get("source"))
        .and_then(|v| v.as_str())
        .unwrap_or("cli");

    // Skip messages explicitly from Telegram
    if source == "telegram" {
        return;
    }

    // BUG-011: Check echo prevention set
    let input_key = format!("{}:{}", msg.session_id, msg.content.trim());
    {
        let mut recent = ctx.recent_inputs.write().await;
        if recent.remove(&input_key) {
            tracing::debug!(input_key, "Skipping echo for telegram input");
            return;
        }
    }

    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        return;
    }

    ctx.bot
        .send_message(
            &format!("\u{1F464} *User (cli):*\n{}", msg.content),
            Some(&SendOptions {
                parse_mode: Some("Markdown".into()),
                ..Default::default()
            }),
            thread_id,
        )
        .await;
}

/// Handler 7: approval_request
async fn handle_approval_request(ctx: &HandlerContext, msg: &BridgeMessage) {
    let approval_id = {
        let sess = ctx.sessions.lock().await;
        sess.create_approval(&msg.session_id, &msg.content, None)
            .unwrap_or_else(|_| "unknown".into())
    };

    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        return;
    }

    ctx.bot
        .send_with_buttons(
            &format_approval_request(&msg.content),
            vec![
                InlineButton {
                    text: "\u{2705} Approve".into(),
                    callback_data: format!("approve:{approval_id}"),
                },
                InlineButton {
                    text: "\u{274C} Reject".into(),
                    callback_data: format!("reject:{approval_id}"),
                },
                InlineButton {
                    text: "\u{1F6D1} Abort".into(),
                    callback_data: format!("abort:{approval_id}"),
                },
            ],
            Some(&SendOptions {
                parse_mode: Some("Markdown".into()),
                ..Default::default()
            }),
            thread_id,
        )
        .await;
}

/// Handler 10: error
async fn handle_error(ctx: &HandlerContext, msg: &BridgeMessage) {
    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        return;
    }
    ctx.bot
        .send_message(
            &format_error(&msg.content),
            Some(&SendOptions {
                parse_mode: Some("Markdown".into()),
                ..Default::default()
            }),
            thread_id,
        )
        .await;
}

/// Handler 12: pre_compact
async fn handle_pre_compact(ctx: &HandlerContext, msg: &BridgeMessage) {
    let trigger = msg
        .metadata
        .as_ref()
        .and_then(|m| m.get("trigger"))
        .and_then(|v| v.as_str())
        .unwrap_or("auto");

    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        return;
    }

    ctx.compacting.write().await.insert(msg.session_id.clone());

    let message = if trigger == "manual" {
        "\u{1F504} *Compacting session context...*\n\n_User requested /compact_"
    } else {
        "\u{23F3} *Context limit reached*\n\n_Summarizing conversation, please wait..._"
    };

    ctx.bot
        .send_message(
            message,
            Some(&SendOptions {
                parse_mode: Some("Markdown".into()),
                ..Default::default()
            }),
            thread_id,
        )
        .await;
}

/// Handle compact completion (called from turn_complete).
async fn handle_compact_complete(ctx: &HandlerContext, session_id: &str) {
    ctx.compacting.write().await.remove(session_id);

    let thread_id = ctx.get_thread_id(session_id).await;
    ctx.bot
        .send_message(
            "\u{2705} *Compaction complete*\n\n_Resuming session..._",
            Some(&SendOptions {
                parse_mode: Some("Markdown".into()),
                ..Default::default()
            }),
            thread_id,
        )
        .await;
}

// ====================================================================== session lifecycle

/// BUG-009/BUG-010: Ensure a session exists, creating on-the-fly if needed.
async fn ensure_session_exists(ctx: &HandlerContext, msg: &BridgeMessage) {
    let existing = ctx
        .sessions
        .lock()
        .await
        .get_session(&msg.session_id)
        .ok()
        .flatten();

    if let Some(session) = existing {
        // BUG-009: Reactivate ended session if hook events are still arriving
        if session.status != "active" {
            tracing::info!(
                session_id = %msg.session_id,
                prev_status = %session.status,
                "Reactivating ended session"
            );
            let _ = ctx
                .sessions
                .lock()
                .await
                .reactivate_session(&msg.session_id);
            cancel_pending_topic_deletion(ctx, &msg.session_id).await;
        }

        // Check if topic was deleted — need to create new one
        let thread_id = ctx.get_thread_id(&msg.session_id).await;
        if thread_id.is_none() && ctx.config.use_threads {
            let hostname = session.hostname.as_deref();
            let project_dir = session.project_dir.as_deref();
            let topic_name =
                HandlerContext::format_topic_name(&msg.session_id, hostname, project_dir);
            if let Ok(Some(tid)) = ctx.bot.create_forum_topic(&topic_name, 0).await {
                let sess = ctx.sessions.lock().await;
                let _ = sess.set_session_thread(&msg.session_id, tid);
                ctx.session_threads
                    .write()
                    .await
                    .insert(msg.session_id.clone(), tid);
                ctx.bot
                    .send_message(
                        "\u{1F504} *Session resumed*\n\n_Previous topic was auto-deleted. New topic created._",
                        Some(&SendOptions {
                            parse_mode: Some("Markdown".into()),
                            ..Default::default()
                        }),
                        Some(tid),
                    )
                    .await;
                let _ = ctx.bot.unpin_all_topic_messages(tid).await;
            }
        }
        return;
    }

    // BUG-002/BUG-010: Check if another call is already creating this session
    {
        let locks = ctx.topic_locks.read().await;
        if let Some(state) = locks.get(&msg.session_id) {
            if !state.resolved {
                let notify = Arc::clone(&state.notify);
                drop(locks);
                let _ =
                    tokio::time::timeout(tokio::time::Duration::from_secs(5), notify.notified())
                        .await;
                return;
            }
        }
    }

    // Create lock for concurrent callers
    if ctx.config.use_threads {
        let state = Arc::new(TopicCreationState {
            notify: Arc::new(tokio::sync::Notify::new()),
            resolved: false,
        });
        ctx.topic_locks
            .write()
            .await
            .insert(msg.session_id.clone(), state);
    }

    // Create session on-the-fly
    tracing::info!(session_id = %msg.session_id, "Creating session on-the-fly");
    handle_session_start(ctx, msg).await;
}

// ====================================================================== session rename (Epic 5)

/// Check transcript JSONL for custom-title record.
fn check_for_session_rename(
    transcript_path: &str,
    session_id: &str,
    custom_titles: &Arc<RwLock<HashMap<String, String>>>,
) -> Option<String> {
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};

    let mut file = fs::File::open(transcript_path).ok()?;
    let file_size = file.metadata().ok()?.len();
    let read_size = std::cmp::min(8192, file_size) as usize;
    let offset = file_size.saturating_sub(read_size as u64);
    file.seek(SeekFrom::Start(offset)).ok()?;

    let mut buffer = vec![0u8; read_size];
    file.read_exact(&mut buffer).ok()?;

    let tail = String::from_utf8_lossy(&buffer);

    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
            if record.get("type").and_then(|t| t.as_str()) == Some("custom-title") {
                if let Some(title) = record.get("customTitle").and_then(|t| t.as_str()) {
                    // Dedup is handled by handle_session_rename (H8)
                    let _ = session_id;
                    let _ = custom_titles;
                    return Some(title.to_string());
                }
            }
        }
    }
    None
}

/// Handle session rename: update Telegram forum topic name.
/// H8: Skip editForumTopic when title is unchanged (dedup).
async fn handle_session_rename(ctx: &HandlerContext, session_id: &str, custom_title: &str) {
    // H8: Dedup — skip if title is the same as last known
    {
        let titles = ctx.custom_titles.read().await;
        if titles.get(session_id).map(|s| s.as_str()) == Some(custom_title) {
            tracing::debug!(
                session_id,
                custom_title,
                "Skipping rename (title unchanged)"
            );
            return;
        }
    }
    // Update the cached title
    ctx.custom_titles
        .write()
        .await
        .insert(session_id.to_string(), custom_title.to_string());

    let thread_id = match ctx.get_thread_id(session_id).await {
        Some(tid) => tid,
        None => return,
    };

    let (hostname, project_dir) = {
        let sess = ctx.sessions.lock().await;
        let s = sess.get_session(session_id).ok().flatten();
        (
            s.as_ref().and_then(|s| s.hostname.clone()),
            s.as_ref().and_then(|s| s.project_dir.clone()),
        )
    };

    let suffix =
        HandlerContext::format_topic_name(session_id, hostname.as_deref(), project_dir.as_deref());
    let new_name = format!("{custom_title} | {suffix}");
    let new_name = &new_name[..std::cmp::min(128, new_name.len())]; // Telegram limit

    tracing::info!(session_id, custom_title, new_name, "Renaming forum topic");

    if let Ok(true) = ctx.bot.edit_forum_topic(thread_id, new_name).await {
        ctx.bot
            .send_message(
                &format!("Topic renamed: *{custom_title}*"),
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                Some(thread_id),
            )
            .await;
    }
}

// ====================================================================== topic deletion

/// Schedule topic deletion with delay (allows for session resume).
async fn schedule_topic_deletion(
    ctx: &HandlerContext,
    session_id: &str,
    thread_id: i64,
    delay_ms: u64,
) {
    let bot = Arc::clone(&ctx.bot);
    let sessions = Arc::clone(&ctx.sessions);
    let session_threads = Arc::clone(&ctx.session_threads);
    let sid = session_id.to_string();

    let handle = tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        let deleted = bot.delete_forum_topic(thread_id).await.unwrap_or(false);
        if deleted {
            tracing::info!(session_id = %sid, thread_id, "Auto-deleted forum topic");
            session_threads.write().await.remove(&sid);
            let _ = sessions.lock().await.clear_thread_id(&sid);
        } else {
            tracing::warn!(session_id = %sid, thread_id, "Failed to delete topic, falling back to close");
            let _ = bot.close_forum_topic(thread_id).await;
            session_threads.write().await.remove(&sid);
        }
    });

    ctx.pending_del
        .write()
        .await
        .insert(session_id.to_string(), handle);
}

/// BUG-012: Cancel pending topic deletion when session resumes.
async fn cancel_pending_topic_deletion(ctx: &HandlerContext, session_id: &str) {
    if let Some(handle) = ctx.pending_del.write().await.remove(session_id) {
        handle.abort();
        tracing::info!(
            session_id,
            "Cancelled pending topic deletion (session resumed)"
        );
    }
}

// ====================================================================== AskUserQuestion (Epic 3)

async fn handle_ask_user_question(ctx: &HandlerContext, msg: &BridgeMessage) {
    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        return;
    }

    let tool_input = match msg.metadata.as_ref().and_then(|m| m.get("input")) {
        Some(v) => v,
        None => return,
    };

    let questions_val = match tool_input.get("questions").and_then(|v| v.as_array()) {
        Some(q) if !q.is_empty() => q,
        _ => return,
    };

    let mut questions = Vec::new();
    for q in questions_val {
        let options = q
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|o| OptionDef {
                        label: o
                            .get("label")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        description: o
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        questions.push(QuestionDef {
            question: q
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            header: q
                .get("header")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            options,
            multi_select: q
                .get("multiSelect")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        });
    }

    let short_session_id = &msg.session_id[..std::cmp::min(20, msg.session_id.len())];
    let pending_key = short_session_id.to_string();

    {
        let mut pq = ctx.pending_q.write().await;
        let mut selected = HashMap::new();
        for (i, q) in questions.iter().enumerate() {
            if q.multi_select {
                selected.insert(i, HashSet::new());
            }
        }
        pq.insert(
            pending_key.clone(),
            PendingQuestion {
                session_id: msg.session_id.clone(),
                questions: questions.clone(),
                answered: vec![false; questions.len()],
                selected_options: selected,
                timestamp: std::time::Instant::now(),
            },
        );
    }

    // Schedule question expiry
    let pq_ref = Arc::clone(&ctx.pending_q);
    let pk = pending_key.clone();
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(QUESTION_TTL_SECS)).await;
        let mut pq = pq_ref.write().await;
        if let Some(pending) = pq.get(&pk) {
            if pending.timestamp.elapsed().as_secs() >= QUESTION_TTL_SECS {
                pq.remove(&pk);
            }
        }
    });

    // Render each question as a separate message
    for (q_idx, q) in questions.iter().enumerate() {
        let mut text = format!(
            "\u{2753} *{}*\n\n{}\n",
            escape_markdown(&q.header),
            escape_markdown(&q.question)
        );
        for opt in &q.options {
            text.push_str(&format!(
                "\n\u{2022} *{}* \u{2014} {}",
                escape_markdown(&opt.label),
                escape_markdown(&opt.description)
            ));
        }
        text.push_str("\n\n_Or type your answer in this topic_");

        let mut buttons = Vec::new();
        if q.multi_select {
            for (o_idx, opt) in q.options.iter().enumerate() {
                buttons.push(InlineButton {
                    text: opt.label.clone(),
                    callback_data: format!("toggle:{short_session_id}:{q_idx}:{o_idx}"),
                });
            }
            buttons.push(InlineButton {
                text: "\u{2705} Submit".into(),
                callback_data: format!("submit:{short_session_id}:{q_idx}"),
            });
        } else {
            for (o_idx, opt) in q.options.iter().enumerate() {
                buttons.push(InlineButton {
                    text: opt.label.clone(),
                    callback_data: format!("answer:{short_session_id}:{q_idx}:{o_idx}"),
                });
            }
        }

        ctx.bot
            .send_with_buttons(
                &text,
                buttons,
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                thread_id,
            )
            .await;
    }
}

/// Escape markdown for Telegram v1 mode.
fn escape_markdown(text: &str) -> String {
    text.replace('`', "'")
}

/// Clean up pending questions for a session.
async fn cleanup_pending_questions(ctx: &HandlerContext, session_id: &str) {
    let mut pq = ctx.pending_q.write().await;
    let keys_to_remove: Vec<String> = pq
        .iter()
        .filter(|(_, v)| v.session_id == session_id)
        .map(|(k, _)| k.clone())
        .collect();
    for k in keys_to_remove {
        pq.remove(&k);
    }
}

// ====================================================================== Telegram update handler

/// Handle an incoming Telegram update (message or callback).
async fn handle_telegram_update(ctx: HandlerContext, update: Update) {
    // Security: Check chat_id on ALL updates
    if let Some(msg) = &update.message {
        if msg.chat.id != ctx.config.chat_id {
            tracing::warn!(chat_id = msg.chat.id, "Unauthorized message");
            return;
        }
        handle_telegram_message(&ctx, msg).await;
    }

    if let Some(cb) = &update.callback_query {
        // Security check for callback queries
        if let Some(msg) = &cb.message {
            if msg.chat.id != ctx.config.chat_id {
                tracing::warn!(chat_id = msg.chat.id, "Unauthorized callback");
                return;
            }
        }
        handle_callback_query(&ctx, cb).await;
    }
}

/// Handle a text/photo/document message from Telegram.
async fn handle_telegram_message(ctx: &HandlerContext, msg: &TgMessage) {
    // Handle commands
    if let Some(text) = &msg.text {
        if text.starts_with('/') {
            handle_bot_command(ctx, msg, text).await;
            return;
        }
    }

    // Handle photos
    if msg.photo.is_some() {
        handle_telegram_photo(ctx, msg).await;
        return;
    }

    // Handle documents
    if msg.document.is_some() {
        handle_telegram_document(ctx, msg).await;
        return;
    }

    // Handle text messages (forward to CLI)
    if let Some(text) = &msg.text {
        handle_telegram_text(ctx, msg, text).await;
    }
}

/// Handle text message from Telegram -> CLI.
async fn handle_telegram_text(ctx: &HandlerContext, msg: &TgMessage, text: &str) {
    // BUG-005: Ignore General topic (no threadId)
    let thread_id = match msg.message_thread_id {
        Some(tid) => tid,
        None => return,
    };

    // Only process messages for sessions we own
    let session = ctx
        .sessions
        .lock()
        .await
        .get_session_by_thread_id(thread_id)
        .ok()
        .flatten();

    let session = match session {
        Some(s) => s,
        None => return,
    };

    // Get tmux info
    let tmux_target = get_tmux_target(ctx, &session.id, session.tmux_socket.as_deref()).await;

    if let Some(target) = &tmux_target {
        let mut inj = ctx.injector.lock().await;
        let socket = session.tmux_socket.as_deref();
        inj.set_target(target, socket);
    }

    // cc command prefix: "cc clear" -> "/clear"
    if text.to_lowercase().starts_with("cc ") {
        let command = format!("/{}", text[3..].trim());
        // Track for echo prevention
        add_echo_key(ctx, &session.id, &command).await;
        let inj = ctx.injector.lock().await;
        let _ = inj.send_slash_command(&command);
        return;
    }

    // BUG-004: Interrupt commands (Escape)
    if is_interrupt_command(text) {
        let inj = ctx.injector.lock().await;
        let ok = inj.send_key("Escape").unwrap_or(false);
        let msg_text = if ok {
            "\u{23F8}\u{FE0F} *Interrupt sent* (Escape)\n\n_Claude should pause the current operation._"
        } else {
            "\u{26A0}\u{FE0F} *Could not send interrupt*\n\nNo tmux session found."
        };
        ctx.bot
            .send_message(
                msg_text,
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                Some(thread_id),
            )
            .await;
        return;
    }

    // BUG-004: Kill commands (Ctrl-C)
    if is_kill_command(text) {
        let inj = ctx.injector.lock().await;
        let ok = inj.send_key("Ctrl-C").unwrap_or(false);
        let msg_text = if ok {
            "\u{1F6D1} *Kill sent* (Ctrl-C)\n\n_Claude should exit entirely._"
        } else {
            "\u{26A0}\u{FE0F} *Could not send kill*\n\nNo tmux session found."
        };
        ctx.bot
            .send_message(
                msg_text,
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                Some(thread_id),
            )
            .await;
        return;
    }

    // Check for pending AskUserQuestion free-text answer
    if handle_free_text_answer(ctx, &session.id, text).await {
        return;
    }

    // BUG-011: Track for echo prevention
    add_echo_key(ctx, &session.id, text.trim()).await;

    // Inject into CLI via tmux
    let injected = {
        let inj = ctx.injector.lock().await;
        inj.inject(text).unwrap_or(false)
    };

    if !injected {
        // BUG-001: Clear, actionable error message
        let inj = ctx.injector.lock().await;
        let error_msg = match inj.validate_target() {
            Ok(()) => "\u{26A0}\u{FE0F} *Could not send input to CLI*\n\nNo tmux session found. Make sure Claude Code is running in tmux.",
            Err(reason) => &format!(
                "\u{26A0}\u{FE0F} *Could not send input to CLI*\n\n{reason}\n\n_Send any command in Claude to refresh the connection._"
            ),
        };
        ctx.bot
            .send_message(
                error_msg,
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                Some(thread_id),
            )
            .await;
    }
}

/// Handle photo message from Telegram.
async fn handle_telegram_photo(ctx: &HandlerContext, msg: &TgMessage) {
    let thread_id = match msg.message_thread_id {
        Some(tid) => tid,
        None => return, // BUG-005
    };

    let session = match ctx
        .sessions
        .lock()
        .await
        .get_session_by_thread_id(thread_id)
        .ok()
        .flatten()
    {
        Some(s) => s,
        None => return,
    };

    let photos = match &msg.photo {
        Some(p) if !p.is_empty() => p,
        _ => return,
    };

    // Highest resolution is last
    let photo = &photos[photos.len() - 1];

    if let Some(size) = photo.file_size {
        if size > 20 * 1024 * 1024 {
            ctx.bot
                .send_message("File too large (max 20MB)", None, Some(thread_id))
                .await;
            return;
        }
    }

    let downloads_dir = ensure_downloads_dir();
    let filename = sanitize_filename(&format!("photo_{}.jpg", photo.file_unique_id));
    let dest_path = downloads_dir.join(&filename);

    match ctx
        .bot
        .download_file(&photo.file_id, &dest_path.to_string_lossy())
        .await
    {
        Ok(Some(local_path)) => {
            let mut injection_text = format!("[Image from Telegram: {local_path}]");
            if let Some(caption) = &msg.caption {
                injection_text.push_str(&format!(" Caption: {caption}"));
            }
            inject_to_session(ctx, &session, &injection_text, thread_id, "Photo").await;
        }
        _ => {
            ctx.bot
                .send_message("Failed to download photo", None, Some(thread_id))
                .await;
        }
    }
}

/// Handle document message from Telegram.
async fn handle_telegram_document(ctx: &HandlerContext, msg: &TgMessage) {
    let thread_id = match msg.message_thread_id {
        Some(tid) => tid,
        None => return, // BUG-005
    };

    let session = match ctx
        .sessions
        .lock()
        .await
        .get_session_by_thread_id(thread_id)
        .ok()
        .flatten()
    {
        Some(s) => s,
        None => return,
    };

    let doc = match &msg.document {
        Some(d) => d,
        None => return,
    };

    if let Some(size) = doc.file_size {
        if size > 20 * 1024 * 1024 {
            ctx.bot
                .send_message("File too large (max 20MB)", None, Some(thread_id))
                .await;
            return;
        }
    }

    let original_name = doc.file_name.as_deref().unwrap_or_else(|| {
        doc.mime_type
            .as_deref()
            .and_then(|m| m.split('/').next_back())
            .map(|ext| Box::leak(format!("unnamed.{ext}").into_boxed_str()) as &str)
            .unwrap_or("unnamed.bin")
    });

    let filename = sanitize_filename(original_name);
    let downloads_dir = ensure_downloads_dir();
    let dest_path = downloads_dir.join(&filename);

    match ctx
        .bot
        .download_file(&doc.file_id, &dest_path.to_string_lossy())
        .await
    {
        Ok(Some(local_path)) => {
            let mut injection_text = format!("[Document from Telegram: {local_path}]");
            if let Some(caption) = &msg.caption {
                injection_text.push_str(&format!(" Caption: {caption}"));
            }
            inject_to_session(ctx, &session, &injection_text, thread_id, "Document").await;
        }
        _ => {
            ctx.bot
                .send_message("Failed to download document", None, Some(thread_id))
                .await;
        }
    }
}

/// Inject text into a session's tmux pane.
async fn inject_to_session(
    ctx: &HandlerContext,
    session: &crate::session::Session,
    text: &str,
    thread_id: i64,
    what: &str,
) {
    let tmux_target = get_tmux_target(ctx, &session.id, session.tmux_socket.as_deref()).await;

    if let Some(target) = tmux_target {
        let mut inj = ctx.injector.lock().await;
        inj.set_target(&target, session.tmux_socket.as_deref());
        let ok = inj.inject(text).unwrap_or(false);
        let msg = if ok {
            format!("{what} sent to Claude")
        } else {
            format!("Failed to inject {what} path into session")
        };
        ctx.bot.send_message(&msg, None, Some(thread_id)).await;
    } else {
        ctx.bot
            .send_message(
                "No tmux session found for this topic",
                None,
                Some(thread_id),
            )
            .await;
    }
}

// ====================================================================== bot commands

/// Handle bot slash commands.
async fn handle_bot_command(ctx: &HandlerContext, msg: &TgMessage, text: &str) {
    let parts: Vec<&str> = text.splitn(2, ' ').collect();
    let command = parts[0].split('@').next().unwrap_or(parts[0]); // Strip @botname
    let args = parts.get(1).copied().unwrap_or("").trim();

    let opts = SendOptions {
        parse_mode: Some("Markdown".into()),
        ..Default::default()
    };

    match command {
        "/start" => {
            ctx.bot
                .send_message(
                    "\u{1F44B} *Claude Code Mirror Bot*\n\n\
                     I mirror your Claude Code sessions to Telegram, allowing you to:\n\
                     \u{2022} Monitor agent progress from your phone\n\
                     \u{2022} Send responses and commands remotely\n\
                     \u{2022} Approve/reject actions via buttons\n\n\
                     Use /help to see all available commands.",
                    Some(&opts),
                    msg.message_thread_id,
                )
                .await;
        }
        "/help" => {
            ctx.bot
                .send_message(
                    &crate::formatting::format_help(),
                    Some(&opts),
                    msg.message_thread_id,
                )
                .await;
        }
        "/status" => {
            let (active, pending) = ctx.sessions.lock().await.get_stats().unwrap_or((0, 0));
            ctx.bot
                .send_message(
                    &format!(
                        "\u{1F4CA} *Status*\n\n\
                         Active sessions: {active}\n\
                         Pending approvals: {pending}"
                    ),
                    Some(&opts),
                    msg.message_thread_id,
                )
                .await;
        }
        "/sessions" => {
            let sessions = ctx
                .sessions
                .lock()
                .await
                .get_active_sessions()
                .unwrap_or_default();

            if sessions.is_empty() {
                ctx.bot
                    .send_message("\u{1F4ED} No active sessions.", None, msg.message_thread_id)
                    .await;
            } else {
                let mut text = "\u{1F4CB} *Active Sessions:*\n\n".to_string();
                for (idx, s) in sessions.iter().enumerate() {
                    text.push_str(&format!("{}. `{}`\n", idx + 1, s.id));
                    // M6: Show session age
                    if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&s.started_at) {
                        let age_mins = (chrono::Utc::now() - started.to_utc()).num_minutes().max(0);
                        if age_mins >= 60 {
                            text.push_str(&format!(
                                "   Started: {}h {}m ago\n",
                                age_mins / 60,
                                age_mins % 60
                            ));
                        } else {
                            text.push_str(&format!("   Started: {}m ago\n", age_mins));
                        }
                    }
                    if let Some(pd) = &s.project_dir {
                        text.push_str(&format!("   Project: `{pd}`\n"));
                    }
                    text.push('\n');
                }
                ctx.bot
                    .send_message(&text, Some(&opts), msg.message_thread_id)
                    .await;
            }
        }
        "/ping" => {
            // M5: Measure actual round-trip by sending then editing
            let start = std::time::Instant::now();
            match ctx
                .bot
                .send_message_returning("\u{1F3D3} Pong!", Some(&opts), msg.message_thread_id)
                .await
            {
                Ok(sent) => {
                    let latency = start.elapsed().as_millis();
                    let _ = ctx
                        .bot
                        .edit_message(
                            sent.message_id,
                            &format!("\u{1F3D3} Pong! _{}ms_", latency),
                            msg.message_thread_id,
                        )
                        .await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Ping send failed");
                }
            }
        }
        "/rename" => {
            let thread_id = match msg.message_thread_id {
                Some(tid) => tid,
                None => {
                    ctx.bot
                        .send_message(
                            "Use /rename in a session topic, not the General chat.",
                            None,
                            None,
                        )
                        .await;
                    return;
                }
            };

            if args.is_empty() {
                ctx.bot
                    .send_message(
                        "Usage: `/rename <name>`\n\nThis renames the session in Claude Code and updates the topic.",
                        Some(&opts),
                        Some(thread_id),
                    )
                    .await;
                return;
            }

            // Look up session by thread_id and inject /rename command
            let session = ctx
                .sessions
                .lock()
                .await
                .get_session_by_thread_id(thread_id)
                .ok()
                .flatten();

            if let Some(session) = session {
                let tmux_target =
                    get_tmux_target(ctx, &session.id, session.tmux_socket.as_deref()).await;
                if let Some(target) = tmux_target {
                    let mut inj = ctx.injector.lock().await;
                    inj.set_target(&target, session.tmux_socket.as_deref());
                    let command = format!("/rename {args}");
                    if inj.send_slash_command(&command).unwrap_or(false) {
                        ctx.bot
                            .send_message(
                                &format!("Sending rename to Claude Code: *{args}*"),
                                Some(&opts),
                                Some(thread_id),
                            )
                            .await;
                    } else {
                        ctx.bot
                            .send_message(
                                "Failed to send rename to Claude Code.",
                                None,
                                Some(thread_id),
                            )
                            .await;
                    }
                } else {
                    ctx.bot
                        .send_message(
                            "No tmux session found for this topic.",
                            None,
                            Some(thread_id),
                        )
                        .await;
                }
            } else {
                ctx.bot
                    .send_message("No session found for this topic.", None, Some(thread_id))
                    .await;
            }
        }
        "/attach" => {
            let thread_id = msg.message_thread_id;
            if args.is_empty() {
                ctx.bot
                    .send_message(
                        "\u{26A0}\u{FE0F} Please provide a session ID.\n\nUsage: `/attach <session-id>`\n\nUse /sessions to see available sessions.",
                        Some(&opts),
                        thread_id,
                    )
                    .await;
                return;
            }
            // Look up session by ID or partial match
            let sessions_list = ctx
                .sessions
                .lock()
                .await
                .get_active_sessions()
                .unwrap_or_default();
            let matched_id = sessions_list
                .iter()
                .find(|s| s.id == args || s.id.contains(args))
                .map(|s| s.id.clone())
                .unwrap_or_else(|| args.to_string());

            let key = thread_id.unwrap_or(msg.chat.id);
            {
                let mut bs = ctx.bot_sessions.write().await;
                let state = bs.entry(key).or_insert_with(|| BotSessionState {
                    attached_session_id: None,
                    muted: false,
                    last_activity: 0,
                });
                state.attached_session_id = Some(matched_id.clone());
                state.last_activity = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
            }
            ctx.bot
                .send_message(
                    &format!("\u{2705} Attached to session `{matched_id}`"),
                    Some(&opts),
                    thread_id,
                )
                .await;
        }
        "/detach" => {
            let thread_id = msg.message_thread_id;
            let key = thread_id.unwrap_or(msg.chat.id);
            let prev = {
                let mut bs = ctx.bot_sessions.write().await;
                let state = bs.entry(key).or_insert_with(|| BotSessionState {
                    attached_session_id: None,
                    muted: false,
                    last_activity: 0,
                });
                state.attached_session_id.take()
            };
            match prev {
                Some(sid) => {
                    ctx.bot
                        .send_message(
                            &format!("\u{1F50C} Detached from session `{sid}`"),
                            Some(&opts),
                            thread_id,
                        )
                        .await;
                }
                None => {
                    ctx.bot
                        .send_message(
                            "\u{2139}\u{FE0F} You are not attached to any session.",
                            None,
                            thread_id,
                        )
                        .await;
                }
            }
        }
        "/mute" => {
            let thread_id = msg.message_thread_id;
            let key = thread_id.unwrap_or(msg.chat.id);
            let already_muted = {
                let mut bs = ctx.bot_sessions.write().await;
                let state = bs.entry(key).or_insert_with(|| BotSessionState {
                    attached_session_id: None,
                    muted: false,
                    last_activity: 0,
                });
                let was = state.muted;
                state.muted = true;
                state.last_activity = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                was
            };
            if already_muted {
                ctx.bot
                    .send_message("\u{1F507} Notifications already muted.", None, thread_id)
                    .await;
            } else {
                ctx.bot
                    .send_message(
                        "\u{1F507} Session muted \u{2014} agent responses suppressed.\n\nUse /unmute to resume.",
                        Some(&opts),
                        thread_id,
                    )
                    .await;
            }
        }
        "/unmute" => {
            let thread_id = msg.message_thread_id;
            let key = thread_id.unwrap_or(msg.chat.id);
            let was_muted = {
                let mut bs = ctx.bot_sessions.write().await;
                let state = bs.entry(key).or_insert_with(|| BotSessionState {
                    attached_session_id: None,
                    muted: false,
                    last_activity: 0,
                });
                let was = state.muted;
                state.muted = false;
                state.last_activity = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                was
            };
            if was_muted {
                ctx.bot
                    .send_message("\u{1F514} Session unmuted.", None, thread_id)
                    .await;
            } else {
                ctx.bot
                    .send_message("\u{1F514} Notifications already active.", None, thread_id)
                    .await;
            }
        }
        "/abort" => {
            let thread_id = msg.message_thread_id;
            let key = thread_id.unwrap_or(msg.chat.id);
            let attached = {
                let bs = ctx.bot_sessions.read().await;
                bs.get(&key).and_then(|s| s.attached_session_id.clone())
            };
            match attached {
                None => {
                    ctx.bot
                        .send_message(
                            "\u{26A0}\u{FE0F} No session attached. Use /attach first.",
                            None,
                            thread_id,
                        )
                        .await;
                }
                Some(session_id) => {
                    ctx.bot
                        .send_with_buttons(
                            &format!(
                                "\u{26A0}\u{FE0F} *Abort Session?*\n\nThis will terminate session `{session_id}`.\n\nAre you sure?"
                            ),
                            vec![
                                InlineButton {
                                    text: "\u{1F6D1} Confirm Abort".into(),
                                    callback_data: format!("confirm_abort:{session_id}"),
                                },
                                InlineButton {
                                    text: "\u{274C} Cancel".into(),
                                    callback_data: "cancel_abort".into(),
                                },
                            ],
                            Some(&opts),
                            thread_id,
                        )
                        .await;
                }
            }
        }
        _ => {
            // Unknown command — ignore silently
        }
    }
}

// ====================================================================== callback query handler

/// Handle callback queries (button presses).
async fn handle_callback_query(ctx: &HandlerContext, cb: &CallbackQuery) {
    let data = match &cb.data {
        Some(d) => d.as_str(),
        None => return,
    };

    // Answer the callback to dismiss spinner
    let _ = ctx.bot.answer_callback_query(&cb.id, None).await;

    // /abort confirmation callbacks
    if let Some(session_id) = data.strip_prefix("confirm_abort:") {
        handle_confirm_abort_callback(ctx, session_id, cb).await;
    } else if data == "cancel_abort" {
        handle_cancel_abort_callback(ctx, cb).await;
    }
    // Approval callbacks
    else if let Some(approval_id) = data.strip_prefix("approve:") {
        handle_approval_callback(ctx, approval_id, "approve", cb).await;
    } else if let Some(approval_id) = data.strip_prefix("reject:") {
        handle_approval_callback(ctx, approval_id, "reject", cb).await;
    } else if let Some(approval_id) = data.strip_prefix("abort:") {
        handle_approval_callback(ctx, approval_id, "abort", cb).await;
    }
    // Tool details callback
    else if let Some(tool_use_id) = data.strip_prefix("tooldetails:") {
        handle_tool_details_callback(ctx, tool_use_id, cb).await;
    }
    // AskUserQuestion callbacks
    else if data.starts_with("answer:") {
        handle_answer_callback(ctx, data, cb).await;
    } else if data.starts_with("toggle:") {
        handle_toggle_callback(ctx, data, cb).await;
    } else if data.starts_with("submit:") {
        handle_submit_callback(ctx, data, cb).await;
    }
}

/// Handle /abort confirmation callback.
async fn handle_confirm_abort_callback(ctx: &HandlerContext, session_id: &str, cb: &CallbackQuery) {
    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);
    let message_id = cb.message.as_ref().map(|m| m.message_id);

    // Mark session as aborted in DB
    let aborted = {
        let sess = ctx.sessions.lock().await;
        sess.end_session(session_id, "aborted").is_ok()
    };

    if aborted {
        // Send Ctrl-C via tmux to interrupt the running process
        let tmux_target = ctx.session_tmux.read().await.get(session_id).cloned();
        if let Some(target) = tmux_target {
            let socket = {
                let sess = ctx.sessions.lock().await;
                sess.get_session(session_id)
                    .ok()
                    .flatten()
                    .and_then(|s| s.tmux_socket)
            };
            let mut inj = ctx.injector.lock().await;
            inj.set_target(&target, socket.as_deref());
            let _ = inj.send_key("Ctrl-C");
        }

        // Clear attached session state for this thread
        let key = thread_id.unwrap_or_else(|| cb.message.as_ref().map(|m| m.chat.id).unwrap_or(0));
        {
            let mut bs = ctx.bot_sessions.write().await;
            if let Some(state) = bs.get_mut(&key) {
                state.attached_session_id = None;
            }
        }
        ctx.session_tmux.write().await.remove(session_id);

        // Edit the confirmation message to show outcome
        if let Some(mid) = message_id {
            let _ = ctx
                .bot
                .edit_message(
                    mid,
                    &format!("\u{1F6D1} Session `{session_id}` aborted."),
                    thread_id,
                )
                .await;
        }
    } else if let Some(mid) = message_id {
        let _ = ctx
            .bot
            .edit_message(mid, "\u{274C} Failed to abort session.", thread_id)
            .await;
    }
}

/// Handle /abort cancellation callback.
async fn handle_cancel_abort_callback(ctx: &HandlerContext, cb: &CallbackQuery) {
    if let Some(mid) = cb.message.as_ref().map(|m| m.message_id) {
        let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);
        let _ = ctx
            .bot
            .edit_message(mid, "\u{2705} Abort cancelled.", thread_id)
            .await;
    }
}

/// Broadcast a `BridgeMessage` to all currently-connected socket clients.
///
/// Mirrors `SocketServer::broadcast` but uses the shared `SocketClients` reference
/// so handlers that don't own the `SocketServer` can still send outbound messages.
async fn broadcast_to_clients(clients: &SocketClients, message: &BridgeMessage) {
    let json = match serde_json::to_string(message) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialise broadcast message");
            return;
        }
    };
    let line = format!("{json}\n");
    let guard = clients.lock().await;
    for (_id, writer) in guard.iter() {
        let mut w = writer.lock().await;
        let _ = w.write_all(line.as_bytes()).await;
    }
}

/// Handle approval/reject/abort callback.
async fn handle_approval_callback(
    ctx: &HandlerContext,
    approval_id: &str,
    action: &str,
    _cb: &CallbackQuery,
) {
    let approval = ctx
        .sessions
        .lock()
        .await
        .get_approval(approval_id)
        .ok()
        .flatten();

    let approval = match approval {
        Some(a) => a,
        None => {
            tracing::warn!(approval_id, "Approval not found");
            return;
        }
    };

    let sess = ctx.sessions.lock().await;
    if action == "abort" {
        let _ = sess.end_session(&approval.session_id, "aborted");
        let _ = sess.resolve_approval(approval_id, "rejected");
    } else {
        let status = if action == "approve" {
            "approved"
        } else {
            "rejected"
        };
        let _ = sess.resolve_approval(approval_id, status);
    }
    drop(sess);

    // ADR-006 C1: Broadcast `approval_response` so the hook client blocked in
    // `send_and_wait()` receives the decision instead of timing out.
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "approvalId".to_string(),
        serde_json::Value::String(approval_id.to_string()),
    );
    let response = BridgeMessage {
        msg_type: "approval_response".to_string(),
        session_id: approval.session_id.clone(),
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        content: action.to_string(),
        metadata: Some(metadata),
    };
    broadcast_to_clients(&ctx.socket_clients, &response).await;
    tracing::info!(
        approval_id,
        action,
        session_id = %approval.session_id,
        "Approval resolved and broadcast over socket"
    );
}

/// Handle tool details callback.
/// M4: Send details as a reply to the original message.
async fn handle_tool_details_callback(ctx: &HandlerContext, tool_use_id: &str, cb: &CallbackQuery) {
    let cached = {
        let cache = ctx.tool_cache.read().await;
        cache
            .get(tool_use_id)
            .map(|c| (c.tool.clone(), c.input.clone()))
    };

    match cached {
        Some((tool, input)) => {
            let details = format_tool_details(&tool, &input);
            let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);
            let reply_to = cb.message.as_ref().map(|m| m.message_id);

            // M4: Reply to the original message instead of standalone
            if let Some(reply_to_id) = reply_to {
                let _ = ctx
                    .bot
                    .send_message_reply_to(
                        &details,
                        reply_to_id,
                        Some(&SendOptions {
                            parse_mode: Some("Markdown".into()),
                            ..Default::default()
                        }),
                        thread_id,
                    )
                    .await;
            } else {
                ctx.bot
                    .send_message(
                        &details,
                        Some(&SendOptions {
                            parse_mode: Some("Markdown".into()),
                            ..Default::default()
                        }),
                        thread_id,
                    )
                    .await;
            }
        }
        None => {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Details expired (5 min cache)"))
                .await;
        }
    }
}

/// Handle single-select answer callback.
async fn handle_answer_callback(ctx: &HandlerContext, data: &str, cb: &CallbackQuery) {
    // Format: answer:{sessionId}:{questionIndex}:{optionIndex}
    let parts: Vec<&str> = data.splitn(4, ':').collect();
    if parts.len() != 4 {
        return;
    }
    let session_key = parts[1];
    let q_idx: usize = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => return,
    };
    let o_idx: usize = match parts[3].parse() {
        Ok(v) => v,
        Err(_) => return,
    };

    let mut pq = ctx.pending_q.write().await;
    let pending = match pq.get_mut(session_key) {
        Some(p) => p,
        None => return,
    };

    if pending.answered.get(q_idx) == Some(&true) {
        return;
    }
    pending.answered[q_idx] = true;

    let answer_text = pending
        .questions
        .get(q_idx)
        .and_then(|q| q.options.get(o_idx))
        .map(|o| o.label.clone())
        .unwrap_or_else(|| format!("{}", o_idx + 1));

    // Inject into tmux
    let session_id = pending.session_id.clone();
    let tmux_target = ctx.session_tmux.read().await.get(&session_id).cloned();
    if let Some(target) = tmux_target {
        let sess = ctx.sessions.lock().await;
        let socket = sess
            .get_session(&session_id)
            .ok()
            .flatten()
            .and_then(|s| s.tmux_socket);
        drop(sess);

        let mut inj = ctx.injector.lock().await;
        inj.set_target(&target, socket.as_deref());
        let _ = inj.inject(&answer_text);
    }

    // M1: Edit message to show "Selected" and remove keyboard
    if let Some(msg) = &cb.message {
        let original_text = msg.text.as_deref().unwrap_or("");
        let updated = format!("{original_text}\n\n\u{2705} Selected");
        let thread_id = msg.message_thread_id;
        if ctx
            .bot
            .edit_message_text_no_markup(msg.message_id, &updated, thread_id)
            .await
            .is_err()
        {
            // Fallback: simpler edit
            let _ = ctx
                .bot
                .edit_message_text_no_markup(msg.message_id, "\u{2705} Answer sent", thread_id)
                .await;
        }
    }

    // Clean up if all answered
    if pending.answered.iter().all(|a| *a) {
        let key = session_key.to_string();
        drop(pq);
        ctx.pending_q.write().await.remove(&key);
    }
}

/// Handle multi-select toggle callback.
async fn handle_toggle_callback(ctx: &HandlerContext, data: &str, cb: &CallbackQuery) {
    let parts: Vec<&str> = data.splitn(4, ':').collect();
    if parts.len() != 4 {
        return;
    }
    let session_key = parts[1];
    let q_idx: usize = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => return,
    };
    let o_idx: usize = match parts[3].parse() {
        Ok(v) => v,
        Err(_) => return,
    };

    let mut pq = ctx.pending_q.write().await;
    let pending = match pq.get_mut(session_key) {
        Some(p) => p,
        None => return,
    };

    if pending.answered.get(q_idx) == Some(&true) {
        return;
    }

    let selected = pending.selected_options.entry(q_idx).or_default();

    if selected.contains(&o_idx) {
        selected.remove(&o_idx);
    } else {
        selected.insert(o_idx);
    }

    // M2: Re-render keyboard with checkmarks
    if let Some(question) = pending.questions.get(q_idx) {
        let mut buttons: Vec<InlineButton> = question
            .options
            .iter()
            .enumerate()
            .map(|(idx, opt)| {
                let label = if selected.contains(&idx) {
                    format!("\u{2713} {}", opt.label)
                } else {
                    opt.label.clone()
                };
                InlineButton {
                    text: label,
                    callback_data: format!("toggle:{session_key}:{q_idx}:{idx}"),
                }
            })
            .collect();
        buttons.push(InlineButton {
            text: "\u{2705} Submit".into(),
            callback_data: format!("submit:{session_key}:{q_idx}"),
        });

        if let Some(msg) = &cb.message {
            let _ = ctx
                .bot
                .edit_message_reply_markup(msg.message_id, &buttons)
                .await;
        }
    }
}

/// Handle multi-select submit callback.
async fn handle_submit_callback(ctx: &HandlerContext, data: &str, cb: &CallbackQuery) {
    let parts: Vec<&str> = data.splitn(3, ':').collect();
    if parts.len() != 3 {
        return;
    }
    let session_key = parts[1];
    let q_idx: usize = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => return,
    };

    let mut pq = ctx.pending_q.write().await;
    let pending = match pq.get_mut(session_key) {
        Some(p) => p,
        None => return,
    };

    if pending.answered.get(q_idx) == Some(&true) {
        return;
    }
    pending.answered[q_idx] = true;

    let selected = pending.selected_options.get(&q_idx);
    let answer_text = if let Some(selected) = selected {
        let mut sorted: Vec<usize> = selected.iter().copied().collect();
        sorted.sort();
        let labels: Vec<String> = sorted
            .iter()
            .filter_map(|&idx| {
                pending
                    .questions
                    .get(q_idx)
                    .and_then(|q| q.options.get(idx))
                    .map(|o| o.label.clone())
            })
            .collect();
        if labels.is_empty() {
            "none".to_string()
        } else {
            labels.join(", ")
        }
    } else {
        "none".to_string()
    };

    // Inject into tmux
    let session_id = pending.session_id.clone();
    let tmux_target = ctx.session_tmux.read().await.get(&session_id).cloned();
    if let Some(target) = tmux_target {
        let sess = ctx.sessions.lock().await;
        let socket = sess
            .get_session(&session_id)
            .ok()
            .flatten()
            .and_then(|s| s.tmux_socket);
        drop(sess);

        let mut inj = ctx.injector.lock().await;
        inj.set_target(&target, socket.as_deref());
        let _ = inj.inject(&answer_text);
    }

    // M3: Edit message to show "Submitted" and remove keyboard
    if let Some(msg) = &cb.message {
        let original_text = msg.text.as_deref().unwrap_or("");
        let updated = format!("{original_text}\n\n\u{2705} Submitted");
        let thread_id = msg.message_thread_id;
        if ctx
            .bot
            .edit_message_text_no_markup(msg.message_id, &updated, thread_id)
            .await
            .is_err()
        {
            let _ = ctx
                .bot
                .edit_message_text_no_markup(msg.message_id, "\u{2705} Submitted", thread_id)
                .await;
        }
    }

    if pending.answered.iter().all(|a| *a) {
        let key = session_key.to_string();
        drop(pq);
        ctx.pending_q.write().await.remove(&key);
    }
}

/// Handle free-text answer to pending AskUserQuestion.
async fn handle_free_text_answer(ctx: &HandlerContext, session_id: &str, text: &str) -> bool {
    let pending_key = {
        let pq = ctx.pending_q.read().await;
        pq.iter()
            .find(|(_, v)| v.session_id == session_id && v.answered.iter().any(|a| !a))
            .map(|(k, _)| k.clone())
    };

    let pending_key = match pending_key {
        Some(k) => k,
        None => return false,
    };

    let mut pq = ctx.pending_q.write().await;
    let pending = match pq.get_mut(&pending_key) {
        Some(p) => p,
        None => return false,
    };

    let q_idx = match pending.answered.iter().position(|a| !a) {
        Some(i) => i,
        None => return false,
    };

    pending.answered[q_idx] = true;

    // Inject into tmux
    let sid = pending.session_id.clone();
    let tmux_target = ctx.session_tmux.read().await.get(&sid).cloned();
    if let Some(target) = tmux_target {
        let sess = ctx.sessions.lock().await;
        let socket = sess
            .get_session(&sid)
            .ok()
            .flatten()
            .and_then(|s| s.tmux_socket);
        drop(sess);

        let mut inj = ctx.injector.lock().await;
        inj.set_target(&target, socket.as_deref());
        let _ = inj.inject(text);
    }

    if pending.answered.iter().all(|a| *a) {
        let key = pending_key.clone();
        drop(pq);
        ctx.pending_q.write().await.remove(&key);
    }

    true
}

// ====================================================================== helpers

/// BUG-004: Check if text is an interrupt command.
fn is_interrupt_command(text: &str) -> bool {
    matches!(
        text.trim().to_lowercase().as_str(),
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

/// BUG-004: Check if text is a kill command.
fn is_kill_command(text: &str) -> bool {
    matches!(
        text.trim().to_lowercase().as_str(),
        "kill" | "/kill" | "exit" | "/exit" | "quit" | "/quit" | "ctrl+c" | "ctrl-c" | "^c"
    )
}

/// Add an echo prevention key with TTL.
async fn add_echo_key(ctx: &HandlerContext, session_id: &str, text: &str) {
    let key = format!("{session_id}:{text}");
    ctx.recent_inputs.write().await.insert(key.clone());

    let inputs = Arc::clone(&ctx.recent_inputs);
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(ECHO_TTL_SECS)).await;
        inputs.write().await.remove(&key);
    });
}

/// Get tmux target for a session (cache -> DB).
async fn get_tmux_target(
    ctx: &HandlerContext,
    session_id: &str,
    _tmux_socket: Option<&str>,
) -> Option<String> {
    // Check cache
    if let Some(target) = ctx.session_tmux.read().await.get(session_id) {
        return Some(target.clone());
    }
    // Fallback to DB
    if let Ok(Some((target, _socket))) = ctx.sessions.lock().await.get_tmux_info(session_id) {
        ctx.session_tmux
            .write()
            .await
            .insert(session_id.to_string(), target.clone());
        return Some(target);
    }
    None
}

// ====================================================================== file helpers

/// Ensure downloads directory exists with restrictive permissions.
fn ensure_downloads_dir() -> PathBuf {
    let dir = get_config_dir().join("downloads");
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    dir
}

/// Sanitize filename to prevent path traversal.
fn sanitize_filename(name: &str) -> String {
    let mut safe = name.replace(['/', '\\'], "_");
    safe = safe.replace("..", "_");
    if safe.starts_with('.') {
        safe = format!("_{safe}");
    }
    if safe.len() > 200 {
        safe.truncate(200);
    }
    format!("{}_{safe}", uuid::Uuid::new_v4())
}

// ====================================================================== cleanup

/// BUG-003: Periodic cleanup of stale sessions, orphaned threads, expired caches, old downloads.
async fn run_cleanup(ctx: HandlerContext) {
    // Expire old approvals
    let _ = ctx.sessions.lock().await.expire_old_approvals();

    // Clean stale sessions (differentiated timeouts)
    cleanup_stale_sessions(&ctx).await;

    // Clean orphaned threads
    cleanup_orphaned_threads(&ctx).await;

    // Clean expired tool input cache
    {
        let mut cache = ctx.tool_cache.write().await;
        cache.retain(|_, v| v.timestamp.elapsed().as_secs() < TOOL_CACHE_TTL_SECS);
    }

    // Clean old downloads
    cleanup_old_downloads();
}

/// BUG-003: Stale session cleanup with differentiated timeouts.
async fn cleanup_stale_sessions(ctx: &HandlerContext) {
    let candidates = ctx
        .sessions
        .lock()
        .await
        .get_stale_session_candidates(NO_TMUX_SESSION_TIMEOUT_HOURS)
        .unwrap_or_default();

    if candidates.is_empty() {
        return;
    }

    let now = chrono::Utc::now();
    let tmux_cutoff = now - chrono::Duration::hours(i64::from(TMUX_SESSION_TIMEOUT_HOURS));

    for session in &candidates {
        let tmux_target = &session.tmux_target;

        // Without tmux info: clean up after 1h (already filtered by query)
        if tmux_target.is_none() {
            tracing::info!(
                session_id = %session.id,
                "Cleaning stale session (no tmux info, >1h inactive)"
            );
            handle_stale_session_cleanup(ctx, session, "inactivity timeout (no tmux info)").await;
            continue;
        }

        // With tmux info: only if older than 24h
        let last_activity = chrono::DateTime::parse_from_rfc3339(&session.last_activity).ok();
        if let Some(la) = last_activity {
            if la.to_utc() >= tmux_cutoff {
                continue; // Not old enough
            }
        }

        // Check if pane is still alive
        let target = tmux_target.as_deref().unwrap();
        let pane_alive = InputInjector::is_pane_alive(target, session.tmux_socket.as_deref());
        let pane_reassigned = ctx
            .sessions
            .lock()
            .await
            .is_tmux_target_owned_by_other(target, &session.id)
            .unwrap_or(false);

        if !pane_alive || pane_reassigned {
            let reason = if !pane_alive {
                "pane no longer exists"
            } else {
                "pane reassigned to another session"
            };
            tracing::info!(
                session_id = %session.id,
                tmux_target = target,
                reason,
                "Cleaning stale session (tmux)"
            );
            handle_stale_session_cleanup(ctx, session, reason).await;
        }
    }
}

/// Handle cleanup of a stale session.
async fn handle_stale_session_cleanup(
    ctx: &HandlerContext,
    session: &crate::session::Session,
    reason: &str,
) {
    let thread_id = ctx.get_thread_id(&session.id).await;

    if let Some(tid) = thread_id {
        let _ = ctx
            .bot
            .send_message(
                &format!("\u{1F50C} *Session ended* (terminal closed)\n\n_{reason}_"),
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                Some(tid),
            )
            .await;

        if ctx.config.auto_delete_topics {
            if ctx.bot.delete_forum_topic(tid).await.unwrap_or(false) {
                let _ = ctx.sessions.lock().await.clear_thread_id(&session.id);
            } else {
                let _ = ctx.bot.close_forum_topic(tid).await;
            }
        } else {
            let _ = ctx.bot.close_forum_topic(tid).await;
        }
    }

    // Clean caches
    ctx.session_threads.write().await.remove(&session.id);
    ctx.session_tmux.write().await.remove(&session.id);
    ctx.custom_titles.write().await.remove(&session.id);
    let _ = ctx.sessions.lock().await.end_session(&session.id, "ended");
}

/// Clean up orphaned threads (ended sessions still with thread_ids).
async fn cleanup_orphaned_threads(ctx: &HandlerContext) {
    let orphans = ctx
        .sessions
        .lock()
        .await
        .get_orphaned_thread_sessions()
        .unwrap_or_default();

    let mut cleaned = 0;
    for session in &orphans {
        if let Some(tid) = session.thread_id {
            let _ = ctx.bot.delete_forum_topic(tid).await;
            let _ = ctx.sessions.lock().await.clear_thread_id(&session.id);
            cleaned += 1;

            // Rate limit
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
        if cleaned >= 50 {
            break;
        }
    }
}

/// Clean up downloaded files older than 24 hours.
fn cleanup_old_downloads() {
    let dir = get_config_dir().join("downloads");
    if !dir.exists() {
        return;
    }

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        if let Ok(metadata) = entry.metadata() {
            if let Ok(modified) = metadata.modified() {
                if let Ok(elapsed) = modified.elapsed() {
                    if elapsed.as_secs() > DOWNLOAD_MAX_AGE_SECS {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

// ===================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_interrupt_command() {
        assert!(is_interrupt_command("stop"));
        assert!(is_interrupt_command("/stop"));
        assert!(is_interrupt_command("CANCEL"));
        assert!(is_interrupt_command("  escape  "));
        assert!(!is_interrupt_command("hello"));
        assert!(!is_interrupt_command("stopping"));
    }

    #[test]
    fn test_is_kill_command() {
        assert!(is_kill_command("kill"));
        assert!(is_kill_command("/kill"));
        assert!(is_kill_command("EXIT"));
        assert!(is_kill_command("ctrl-c"));
        assert!(is_kill_command("^c"));
        assert!(!is_kill_command("hello"));
    }

    #[test]
    fn test_sanitize_filename() {
        let name = sanitize_filename("photo_123.jpg");
        assert!(name.ends_with("_photo_123.jpg"));
        assert!(!name.contains('/'));

        let traversal = sanitize_filename("../../../etc/passwd");
        assert!(!traversal.contains(".."));

        let dotfile = sanitize_filename(".hidden");
        assert!(!dotfile.starts_with('.'));
    }

    #[test]
    fn test_format_topic_name() {
        let name = HandlerContext::format_topic_name(
            "session-abc12345def",
            Some("myhost"),
            Some("/opt/project"),
        );
        assert!(name.contains("myhost"));
        assert!(name.contains("project"));
        assert!(name.contains("abc12345"));
    }

    #[test]
    fn test_format_topic_name_no_hostname() {
        let name = HandlerContext::format_topic_name("session-xyz", None, Some("/opt/project"));
        assert!(name.contains("project"));
    }

    #[test]
    fn test_truncate_path() {
        assert_eq!(
            HandlerContext::truncate_path("/opt/project/src/file.rs"),
            ".../src/file.rs"
        );
        assert_eq!(HandlerContext::truncate_path("/a/b"), "/a/b");
    }

    #[test]
    fn test_escape_markdown() {
        assert_eq!(escape_markdown("hello `world`"), "hello 'world'");
    }

    #[test]
    fn test_format_tool_preview() {
        let input = serde_json::json!({"file_path": "/opt/project/src/main.rs"});
        let preview = format_tool_preview("Read", &input);
        assert!(preview.contains("main.rs"));

        let bash_input = serde_json::json!({"command": "cargo test"});
        let preview = format_tool_preview("Bash", &bash_input);
        assert!(preview.contains("cargo test"));
    }

    /// ADR-006 C1: Verify the `approval_response` message is correctly structured
    /// for each possible action so the hook client's `send_and_wait()` can match it.
    #[test]
    fn test_approval_response_message_structure() {
        for action in &["approve", "reject", "abort"] {
            let approval_id = "approval-test-123";
            let session_id = "session-abc12345";

            let mut metadata = serde_json::Map::new();
            metadata.insert(
                "approvalId".to_string(),
                serde_json::Value::String(approval_id.to_string()),
            );

            let msg = BridgeMessage {
                msg_type: "approval_response".to_string(),
                session_id: session_id.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                content: action.to_string(),
                metadata: Some(metadata),
            };

            // Verify type matches what the TypeScript daemon broadcasts.
            assert_eq!(msg.msg_type, "approval_response");
            assert_eq!(msg.session_id, session_id);
            assert_eq!(msg.content, *action);

            let meta = msg.metadata.as_ref().expect("metadata must be present");
            assert_eq!(
                meta.get("approvalId").and_then(|v| v.as_str()),
                Some(approval_id),
                "metadata.approvalId must equal the approval ID"
            );

            // Must round-trip through JSON (required for NDJSON framing).
            let json = serde_json::to_string(&msg).expect("must serialise");
            let parsed: BridgeMessage = serde_json::from_str(&json).expect("must deserialise");
            assert_eq!(parsed.msg_type, "approval_response");
            assert_eq!(parsed.content, *action);
        }
    }
}
