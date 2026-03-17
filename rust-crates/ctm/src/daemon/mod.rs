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

mod callback_handlers;
mod cleanup;
mod event_loop;
mod files;
mod socket_handlers;
mod telegram_handlers;

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
use crate::types::{is_valid_session_id, BridgeMessage, MessageType};
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
    /// M5.5: Track Telegram message IDs associated with this question so they
    /// can be cleaned up (edited/deleted) when the question is answered or the
    /// session ends. Currently populated but cleanup logic is a future
    /// enhancement -- the field exists so callers can begin tracking IDs now.
    // Intentional: field pre-positioned for future cleanup enhancement
    #[allow(dead_code)] // Pre-positioned for future cleanup enhancement
    message_ids: Vec<i64>,
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
    // Intentional: preserved for future question-detail display
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

    // Epic 1: Runtime mirroring toggle
    mirroring_enabled: Arc<std::sync::atomic::AtomicBool>,

    // L3.2: Track running state
    running: Arc<std::sync::atomic::AtomicBool>,
}

/// L3.2: Programmatic daemon status.
#[allow(dead_code)] // Library API
pub struct DaemonStatus {
    /// Whether the daemon event loop is running.
    pub running: bool,
    /// Number of connected socket clients.
    pub clients: usize,
    /// Number of active sessions in the database.
    pub sessions: usize,
}

impl Daemon {
    pub fn new(config: Config) -> Result<Self> {
        let bot = Arc::new(TelegramBot::new(&config));
        let sessions = SessionManager::new(&config.config_dir, config.session_timeout)?;
        let mirroring_enabled = Arc::new(std::sync::atomic::AtomicBool::new(
            crate::config::read_mirror_status(&config.config_dir),
        ));

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
            mirroring_enabled,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// L3.2: Check if the daemon is currently running.
    #[allow(dead_code)] // Library API
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// L3.2: Get programmatic daemon status.
    #[allow(dead_code)] // Library API
    pub async fn get_status(&self) -> DaemonStatus {
        let clients = match &self.socket {
            Some(s) => s.clients_ref().lock().await.len(),
            None => 0,
        };
        let sessions_ref = Arc::clone(&self.sessions);
        let sessions = tokio::task::spawn_blocking(move || {
            sessions_ref
                .blocking_lock()
                .get_active_sessions()
                .map(|s| s.len())
                .unwrap_or(0)
        })
        .await
        .unwrap_or(0);
        DaemonStatus {
            running: self.is_running(),
            clients,
            sessions,
        }
    }

    /// L3.3: Send a user_input message to socket clients for a given session.
    #[allow(dead_code)] // Library API
    pub async fn send_to_session(&self, session_id: &str, text: &str) {
        let msg = BridgeMessage {
            msg_type: MessageType::UserInput,
            session_id: session_id.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            content: text.to_string(),
            metadata: None,
        };
        if let Some(socket) = &self.socket {
            let clients = socket.clients_ref();
            broadcast_to_clients(&clients, &msg).await;
        }
    }

    /// Start the daemon. Runs until shutdown signal.
    pub async fn start(&mut self) -> Result<()> {
        // L4.1: Double-start guard — prevent re-entering start() if already running.
        if self.running.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!("Daemon already running, ignoring start()");
            return Ok(());
        }
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
        let daemon_mirroring_enabled = Arc::clone(&self.mirroring_enabled);
        let daemon_config = Arc::new(self.config.clone());

        tokio::spawn(async move {
            event_loop::run_event_loop(
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
                daemon_mirroring_enabled,
                daemon_config,
                daemon_socket_clients,
            )
            .await;
        });

        self.running
            .store(true, std::sync::atomic::Ordering::Relaxed);
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

        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        tracing::info!("Bridge daemon stopped");
    }
}

// ====================================================================== context

/// Type alias for the connected-client map shared with `SocketServer`.
type SocketClients = Arc<Mutex<HashMap<String, Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>>>>;

/// Shared context passed to all handlers.
///
// LOCK ORDERING (acquire in this order to prevent deadlocks):
// 1. sessions (Mutex<SessionManager>)
// 2. session_threads (RwLock<HashMap>)
// 3. session_tmux (RwLock<HashMap>)
// 4. All other RwLocks in HandlerContext field declaration order:
//    recent_inputs, tool_cache, compacting, pending_del,
//    custom_titles, pending_q, topic_locks, bot_sessions
// 5. injector (Mutex<InputInjector>)
// 6. socket_clients (SocketClients)
//
// Most handlers acquire only one lock at a time (short-lived guards).
// When multiple locks must be held simultaneously, follow this order.
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
    /// Runtime mirroring toggle -- consumed by socket_handlers for toggle gating.
    mirroring_enabled: Arc<std::sync::atomic::AtomicBool>,
    config: Arc<Config>,
    /// Shared reference to the socket's connected-client map for outbound broadcasts.
    socket_clients: SocketClients,
}

impl HandlerContext {
    /// Run a blocking database operation on the `SessionManager` off the async runtime.
    ///
    /// SQLite I/O is synchronous; running it on the Tokio worker pool would block
    /// other tasks. This helper moves the lock acquisition and query onto Tokio's
    /// blocking thread pool via `spawn_blocking`, keeping the async runtime free.
    async fn db_op<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&SessionManager) -> R + Send + 'static,
        R: Send + 'static,
    {
        let sessions = Arc::clone(&self.sessions);
        tokio::task::spawn_blocking(move || {
            let guard = sessions.blocking_lock();
            f(&guard)
        })
        .await
        .expect("spawn_blocking panicked")
    }

    /// Get thread ID for a session (memory cache -> DB fallback).
    async fn get_thread_id(&self, session_id: &str) -> Option<i64> {
        // Check memory cache
        if let Some(tid) = self.session_threads.read().await.get(session_id) {
            return Some(*tid);
        }
        // Fallback to database
        let sid = session_id.to_string();
        let db_session = self
            .db_op(move |sess| sess.get_session(&sid).ok().flatten())
            .await;
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
        let sid = msg.session_id.clone();
        ctx.db_op(move |sess| {
            if sess.get_session(&sid).ok().flatten().is_some() {
                let _ = sess.update_activity(&sid);
            }
        })
        .await;
    }

    // BUG-001: Auto-update tmux target
    check_and_update_tmux_target(&ctx, &msg).await;

    // Epic 1: Toggle gating — skip outbound messages when mirroring is disabled.
    // Safety-critical paths (approvals, commands) always proceed.
    let is_always_active = matches!(
        msg.msg_type,
        MessageType::ApprovalRequest | MessageType::ApprovalResponse | MessageType::Command
    );
    if !is_always_active
        && !ctx
            .mirroring_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        tracing::debug!(msg_type = %msg.msg_type, "Mirroring disabled, dropping message");
        return;
    }

    match msg.msg_type {
        MessageType::SessionStart => socket_handlers::handle_session_start(&ctx, &msg).await,
        MessageType::SessionEnd => socket_handlers::handle_session_end(&ctx, &msg).await,
        MessageType::AgentResponse => {
            ensure_session_exists(&ctx, &msg).await;
            socket_handlers::handle_agent_response(&ctx, &msg).await;
        }
        MessageType::ToolStart => {
            ensure_session_exists(&ctx, &msg).await;
            socket_handlers::handle_tool_start(&ctx, &msg).await;
        }
        MessageType::ToolResult => {
            ensure_session_exists(&ctx, &msg).await;
            socket_handlers::handle_tool_result(&ctx, &msg).await;
        }
        MessageType::UserInput => {
            ensure_session_exists(&ctx, &msg).await;
            socket_handlers::handle_user_input(&ctx, &msg).await;
        }
        MessageType::ApprovalRequest => {
            ensure_session_exists(&ctx, &msg).await;
            socket_handlers::handle_approval_request(&ctx, &msg).await;
        }
        MessageType::Error => {
            ensure_session_exists(&ctx, &msg).await;
            socket_handlers::handle_error(&ctx, &msg).await;
        }
        MessageType::TurnComplete => {
            tracing::debug!(session_id = %msg.session_id, "Turn complete");
            if ctx.compacting.read().await.contains(&msg.session_id) {
                socket_handlers::handle_compact_complete(&ctx, &msg.session_id).await;
            }
        }
        MessageType::PreCompact => {
            ensure_session_exists(&ctx, &msg).await;
            socket_handlers::handle_pre_compact(&ctx, &msg).await;
        }
        MessageType::SessionRename => {
            socket_handlers::handle_session_rename(&ctx, &msg.session_id, &msg.content).await;
        }
        MessageType::Command => {
            socket_handlers::handle_command(&ctx, &msg).await;
        }
        MessageType::SendImage => {
            ensure_session_exists(&ctx, &msg).await;
            socket_handlers::handle_send_image(&ctx, &msg).await;
        }
        _ => {
            tracing::debug!(msg_type = %msg.msg_type, "Unknown message type");
        }
    }

    // After processing, check transcript for custom-title rename (Epic 5)
    if let Some(meta) = &msg.metadata {
        if let Some(tp) = meta.get("transcript_path").and_then(|v| v.as_str()) {
            if let Some(title) =
                socket_handlers::check_for_session_rename(tp, &msg.session_id, &ctx.custom_titles)
            {
                socket_handlers::handle_session_rename(&ctx, &msg.session_id, &title).await;
            }
        }
    }
}

// ====================================================================== shared helpers

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

    {
        let sid = msg.session_id.clone();
        let target = new_target.to_string();
        let socket = new_socket.map(|s| s.to_string());
        ctx.db_op(move |sess| {
            let _ = sess.set_tmux_info(&sid, Some(&target), socket.as_deref());
        })
        .await;
    }
}

/// BUG-009/BUG-010: Ensure a session exists, creating on-the-fly if needed.
///
/// This is the Rust equivalent of the TypeScript `handleSessionStart()` for
/// on-demand creation. The Rust architecture uses lazy session creation:
/// instead of requiring an explicit `session_start` message before any other
/// message can be processed, `ensure_session_exists` is called by every
/// handler that needs a session. If the session does not exist, it is created
/// transparently — including forum topic creation when threads are enabled.
/// This differs from the TS design where `handleSessionStart` and
/// `handleSessionEnd` were standalone entry points.
async fn ensure_session_exists(ctx: &HandlerContext, msg: &BridgeMessage) {
    let sid = msg.session_id.clone();
    let existing = ctx
        .db_op(move |sess| sess.get_session(&sid).ok().flatten())
        .await;

    if let Some(session) = existing {
        // BUG-009: Reactivate ended session if hook events are still arriving
        if session.status != "active" {
            tracing::info!(
                session_id = %msg.session_id,
                prev_status = %session.status,
                "Reactivating ended session"
            );
            let sid = msg.session_id.clone();
            ctx.db_op(move |sess| {
                let _ = sess.reactivate_session(&sid);
            })
            .await;
            cleanup::cancel_pending_topic_deletion(ctx, &msg.session_id).await;
        }

        // Check if topic was deleted -- need to create new one
        let thread_id = ctx.get_thread_id(&msg.session_id).await;
        if thread_id.is_none() && ctx.config.use_threads {
            let hostname = session.hostname.as_deref();
            let project_dir = session.project_dir.as_deref();
            let topic_name =
                HandlerContext::format_topic_name(&msg.session_id, hostname, project_dir);
            if let Ok(Some(tid)) = ctx.bot.create_forum_topic(&topic_name, 0).await {
                let sid = msg.session_id.clone();
                ctx.db_op(move |sess| {
                    let _ = sess.set_session_thread(&sid, tid);
                })
                .await;
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
    socket_handlers::handle_session_start(ctx, msg).await;
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

/// Escape markdown for Telegram Markdown v1 mode.
///
/// M6.1 (INTENTIONAL): This function only escapes backticks because the call
/// sites use `parse_mode: "Markdown"` (v1), not `"MarkdownV2"`.  In v1 mode,
/// the only user-supplied text that can break parsing is backticks (which start
/// code spans).  Replacing them with single quotes is sufficient.
///
/// For MarkdownV2 mode, use `formatting::escape_markdown_v2()` which escapes
/// all 19 special characters.  The two functions are intentionally separate
/// because their escape requirements differ by parse mode.
fn escape_markdown(text: &str) -> String {
    text.replace('`', "'")
}

/// H6.1: Resolve a short session_id prefix (from callback_data) to the full
/// session_id key in the pending_questions map. Returns `None` if no match.
fn resolve_pending_key<'a>(
    pq: &'a HashMap<String, PendingQuestion>,
    short_key: &str,
) -> Option<&'a String> {
    pq.keys().find(|k| k.starts_with(short_key))
}

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
    let sid = session_id.to_string();
    let tmux_info = ctx.db_op(move |sess| sess.get_tmux_info(&sid)).await;
    if let Ok(Some((target, _socket))) = tmux_info {
        ctx.session_tmux
            .write()
            .await
            .insert(session_id.to_string(), target.clone());
        return Some(target);
    }
    None
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
}
