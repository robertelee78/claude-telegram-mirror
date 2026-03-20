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
    format_session_start, format_tool_details, format_tool_execution, truncate,
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
const DOWNLOAD_MAX_AGE_SECS: u64 = 24 * 60 * 60; // 24 hours
const TMUX_SESSION_TIMEOUT_HOURS: u32 = 24;
const NO_TMUX_SESSION_TIMEOUT_HOURS: u32 = 1;
/// FR32: Maximum characters allowed in a single text injection to tmux.
const MAX_INJECT_CHARS: usize = 8192;

// ---------------------------------------------------------------- types

/// Cached tool input for the "Details" button.
pub(super) struct CachedToolInput {
    tool: String,
    input: serde_json::Value,
    timestamp: std::time::Instant,
}

/// Tentative answer for a single AskUserQuestion question.
///
/// ADR-012: Single-select taps and free-text entries are tentative until the
/// user taps "Submit All". Only committed to tmux on final confirmation.
#[derive(Clone, Debug)]
pub(super) enum TentativeAnswer {
    /// Single-select: the index of the chosen option.
    Option(usize),
    /// Multi-select: the set of chosen option indices.
    MultiOption(HashSet<usize>),
    /// Free-text typed by the user.
    FreeText(String),
}

/// Pending AskUserQuestion state.
///
/// ADR-012: Replaced `answered`/`selected_options`/`timestamp` with the
/// unified `tentative` map and `finalized` vec. Questions persist until
/// the user taps "Submit All" or the session ends — no TTL.
pub(super) struct PendingQuestion {
    session_id: String,
    questions: Vec<QuestionDef>,
    /// Tentative selections keyed by question index. Absent = unanswered.
    tentative: HashMap<usize, TentativeAnswer>,
    /// Whether each question has been finalized (injected into tmux).
    /// Only set during the Submit All flow.
    finalized: Vec<bool>,
    /// Telegram message_id for each question message, in question order.
    /// Used to edit messages when selections change.
    question_message_ids: Vec<i64>,
    /// Telegram message_id of the summary confirmation message, if sent.
    summary_message_id: Option<i64>,
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
    description: String,
}

/// Topic creation lock for BUG-002 prevention.
/// When a session is being created, concurrent handlers wait on this.
pub(super) struct TopicCreationState {
    notify: Arc<tokio::sync::Notify>,
}

/// Per-thread bot session state (mirrors grammY session in TypeScript).
pub(super) struct BotSessionState {
    attached_session_id: Option<String>,
    muted: bool,
    last_activity: u64,
}

// ---------------------------------------------------------------- daemon

/// FR43: Consolidated shared state for the daemon.
///
/// All fields that were previously individual `Arc<…>` on `Daemon` are now
/// grouped here and wrapped in a single `Arc<DaemonState>`. This reduces
/// clone overhead and simplifies the `run_event_loop` signature.
pub(super) struct DaemonState {
    pub(super) bot: Arc<TelegramBot>,
    pub(super) sessions: Arc<Mutex<SessionManager>>,
    pub(super) injector: Arc<Mutex<InputInjector>>,

    // In-memory caches
    pub(super) session_threads: Arc<RwLock<HashMap<String, i64>>>,
    pub(super) session_tmux_targets: Arc<RwLock<HashMap<String, String>>>,
    pub(super) recent_telegram_inputs: Arc<RwLock<HashSet<String>>>,
    pub(super) tool_input_cache: Arc<RwLock<HashMap<String, CachedToolInput>>>,
    pub(super) compacting_sessions: Arc<RwLock<HashSet<String>>>,
    pub(super) pending_deletions: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    pub(super) session_custom_titles: Arc<RwLock<HashMap<String, String>>>,
    pub(super) pending_questions: Arc<RwLock<HashMap<String, Arc<Mutex<PendingQuestion>>>>>,

    // BUG-002: Topic creation locks
    pub(super) topic_creation_locks: Arc<RwLock<HashMap<String, Arc<TopicCreationState>>>>,

    // Per-thread bot session state (keyed by thread_id / message_thread_id)
    pub(super) bot_sessions: Arc<RwLock<HashMap<i64, BotSessionState>>>,

    // Epic 1: Runtime mirroring toggle
    pub(super) mirroring_enabled: Arc<std::sync::atomic::AtomicBool>,

    pub(super) config: Arc<Config>,

    // L3.2: Track running state
    pub(super) running: Arc<std::sync::atomic::AtomicBool>,

    // S-2: Map approval_id -> client_id so approval responses are routed to
    // the specific socket client that submitted the approval_request, not
    // broadcast to all connected clients.
    pub(super) pending_approval_clients: Arc<RwLock<HashMap<String, String>>>,
}

/// Bridge Daemon — orchestrates all components.
pub struct Daemon {
    /// FR43: Single Arc holding all shared daemon state.
    state: Arc<DaemonState>,
    socket: Option<SocketServer>,
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
        let bot = Arc::new(TelegramBot::new(&config)?);
        let sessions = SessionManager::new(&config.config_dir, config.session_timeout)?;
        let mirroring_enabled = Arc::new(std::sync::atomic::AtomicBool::new(
            crate::config::read_mirror_status(&config.config_dir),
        ));

        let state = Arc::new(DaemonState {
            bot,
            sessions: Arc::new(Mutex::new(sessions)),
            injector: Arc::new(Mutex::new(InputInjector::new())),
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
            config: Arc::new(config),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pending_approval_clients: Arc::new(RwLock::new(HashMap::new())),
        });

        Ok(Self {
            state,
            socket: None,
        })
    }

    /// L3.2: Check if the daemon is currently running.
    #[allow(dead_code)] // Library API
    pub fn is_running(&self) -> bool {
        self.state
            .running
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// L3.2: Get programmatic daemon status.
    #[allow(dead_code)] // Library API
    pub async fn get_status(&self) -> DaemonStatus {
        let clients = match &self.socket {
            Some(s) => s.clients_ref().lock().await.len(),
            None => 0,
        };
        let sessions_ref = Arc::clone(&self.state.sessions);
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
        if self
            .state
            .running
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            tracing::warn!("Daemon already running, ignoring start()");
            return Ok(());
        }
        tracing::info!("Starting bridge daemon...");

        // Start socket server
        let pid_path = self.state.config.config_dir.join("bridge.pid");
        let mut socket = SocketServer::new(&self.state.config.socket_path, &pid_path);
        socket.listen().await?;
        let socket_rx = socket.subscribe();
        let daemon_socket_clients = socket.clients_ref();
        self.socket = Some(socket);

        // Verify bot connectivity
        match self.state.bot.get_me().await {
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
            let mut inj = self.state.injector.lock().await;
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

        // ADR-013 F8: Warm session_tmux_targets cache from DB on startup.
        // After a daemon restart, the in-memory cache is empty. Populate it from
        // any active sessions that have stored tmux_target values in the DB.
        {
            let active_sessions = {
                let sessions = self.state.sessions.lock().await;
                sessions.get_active_sessions().unwrap_or_default()
            };
            let mut tmux_cache = self.state.session_tmux_targets.write().await;
            let mut warmed = 0u32;
            for session in &active_sessions {
                if let Some(ref target) = session.tmux_target {
                    tmux_cache.insert(session.id.clone(), target.clone());
                    warmed += 1;
                }
            }
            if warmed > 0 {
                tracing::info!(
                    warmed,
                    total_active = active_sessions.len(),
                    "ADR-013 F8: Warmed tmux cache from DB on startup"
                );
            }
        }

        // Send startup notification
        self.state
            .bot
            .send_message(
                "\u{1F7E2} *Bridge Daemon Started*\n\nClaude Code sessions will now be mirrored here.",
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                None,
            )
            .await;

        // Take the topic-invalidation receiver before spawning the event loop.
        let topic_invalidated_rx = self
            .state
            .bot
            .take_topic_invalidated_rx()
            .await
            .expect("topic_invalidated_rx already taken");

        // FR43: Spawn main event loop with consolidated state.
        let daemon_state = Arc::clone(&self.state);

        tokio::spawn(async move {
            event_loop::run_event_loop(
                socket_rx,
                daemon_state,
                daemon_socket_clients,
                topic_invalidated_rx,
            )
            .await;
        });

        self.state
            .running
            .store(true, std::sync::atomic::Ordering::Relaxed);
        tracing::info!("Bridge daemon started");
        Ok(())
    }

    /// Stop the daemon gracefully.
    pub async fn stop(self) {
        tracing::info!("Stopping bridge daemon...");

        // Send shutdown notification
        self.state
            .bot
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

        self.state
            .running
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
    pending_q: Arc<RwLock<HashMap<String, Arc<Mutex<PendingQuestion>>>>>,
    topic_locks: Arc<RwLock<HashMap<String, Arc<TopicCreationState>>>>,
    /// Per-thread bot session state (keyed by thread_id).
    bot_sessions: Arc<RwLock<HashMap<i64, BotSessionState>>>,
    /// Runtime mirroring toggle -- consumed by socket_handlers for toggle gating.
    mirroring_enabled: Arc<std::sync::atomic::AtomicBool>,
    config: Arc<Config>,
    /// Shared reference to the socket's connected-client map for outbound broadcasts.
    socket_clients: SocketClients,
    /// S-2: Maps approval_id -> client_id for targeted approval response routing.
    pending_approval_clients: Arc<RwLock<HashMap<String, String>>>,
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
        R: Send + Default + 'static,
    {
        let sessions = Arc::clone(&self.sessions);
        tokio::task::spawn_blocking(move || {
            let guard = sessions.blocking_lock();
            f(&guard)
        })
        .await
        .unwrap_or_else(|e| {
            // S-3: Log the JoinError but return a safe default instead of panicking.
            // JoinError occurs when the spawned task was cancelled (runtime shutdown)
            // or panicked internally. Returning Default::default() is safe because:
            // - bool::default() = false (operation "did not succeed")
            // - Option::default() = None (record "not found")
            // - ()::default() = () (fire-and-forget operations)
            // - Vec::default() = [] (empty result set)
            tracing::error!(error = %e, "db_op: spawn_blocking task failed, returning default");
            R::default()
        })
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
    ///
    /// Waits up to 45 seconds for a pending topic creation to finish.
    /// The generous timeout accounts for Telegram 429 rate-limit retries
    /// (observed backoffs of 30-40s in production logs).
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
            let _ =
                tokio::time::timeout(tokio::time::Duration::from_secs(45), state.notify.notified())
                    .await;
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

    // ADR-011 Fix #9: Determine message priority for future bot-level send routing.
    let priority = message_priority(&msg.msg_type);
    tracing::debug!(msg_type = %msg.msg_type, session_id = %msg.session_id, priority, "Received socket message");

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
    let tmux_updated = check_and_update_tmux_target(&ctx, &msg).await;

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
        MessageType::SessionStart => {
            // Dedup: skip full handler if session already exists and is active.
            // Activity and tmux target are already updated by check_and_update_tmux_target
            // (called unconditionally above), so an early return here is safe — we only
            // need the full handler (topic creation, Telegram "Session started" message)
            // on the first occurrence for this session.
            let sid = msg.session_id.clone();
            let is_active = ctx
                .db_op(move |sess| {
                    sess.get_session(&sid)
                        .ok()
                        .flatten()
                        .map(|s| s.status == crate::types::SessionStatus::Active)
                        .unwrap_or(false)
                })
                .await;
            if !is_active {
                socket_handlers::handle_session_start(&ctx, &msg).await;
            } else if tmux_updated {
                // ADR-013 MINOR-3: tmux target was just updated on a resumed session.
                // Send a brief confirmation so the user knows replies now work via tmux.
                // check_and_update_tmux_target (above) already stored the target.
                let thread_id = ctx.get_thread_id(&msg.session_id).await;
                if let Some(tid) = thread_id {
                    let target = ctx
                        .session_tmux
                        .read()
                        .await
                        .get(&msg.session_id)
                        .cloned()
                        .unwrap_or_default();
                    ctx.bot
                        .send_message(
                            &format!(
                                "\u{1F7E2} tmux: reconnected (`{}`)",
                                escape_markdown_v1(&target)
                            ),
                            Some(&crate::daemon::SendOptions {
                                parse_mode: Some("Markdown".into()),
                                ..Default::default()
                            }),
                            Some(tid),
                        )
                        .await;
                }
            }
        }
        MessageType::SessionEnd => socket_handlers::handle_session_end(&ctx, &msg).await,
        MessageType::AgentResponse => {
            if ensure_session_exists(&ctx, &msg).await {
                socket_handlers::handle_agent_response(&ctx, &msg).await;
            }
        }
        MessageType::ToolStart => {
            if ensure_session_exists(&ctx, &msg).await {
                socket_handlers::handle_tool_start(&ctx, &msg).await;
            }
        }
        MessageType::ToolResult => {
            if ensure_session_exists(&ctx, &msg).await {
                socket_handlers::handle_tool_result(&ctx, &msg).await;
            }
        }
        MessageType::UserInput => {
            if ensure_session_exists(&ctx, &msg).await {
                socket_handlers::handle_user_input(&ctx, &msg).await;
            }
        }
        MessageType::ApprovalRequest => {
            if ensure_session_exists(&ctx, &msg).await {
                socket_handlers::handle_approval_request(&ctx, &msg).await;
            }
        }
        MessageType::Error => {
            if ensure_session_exists(&ctx, &msg).await {
                socket_handlers::handle_error(&ctx, &msg).await;
            }
        }
        MessageType::TurnComplete => {
            tracing::debug!(session_id = %msg.session_id, "Turn complete");
            if ctx.compacting.read().await.contains(&msg.session_id) {
                socket_handlers::handle_compact_complete(&ctx, &msg.session_id).await;
            }
        }
        MessageType::PreCompact => {
            if ensure_session_exists(&ctx, &msg).await {
                socket_handlers::handle_pre_compact(&ctx, &msg).await;
            }
        }
        MessageType::SessionRename => {
            socket_handlers::handle_session_rename(&ctx, &msg.session_id, &msg.content).await;
        }
        MessageType::Command => {
            socket_handlers::handle_command(&ctx, &msg).await;
        }
        MessageType::SendImage => {
            if ensure_session_exists(&ctx, &msg).await {
                socket_handlers::handle_send_image(&ctx, &msg).await;
            }
        }
        _ => {
            tracing::debug!(msg_type = %msg.msg_type, "Unknown message type");
        }
    }

    // After processing, check transcript for custom-title rename (Epic 5)
    if let Some(tp) = msg.meta().transcript_path() {
        if let Some(title) = socket_handlers::check_for_session_rename(tp) {
            socket_handlers::handle_session_rename(&ctx, &msg.session_id, &title).await;
        }
    }
}

// ====================================================================== shared helpers

/// BUG-001: Auto-update tmux target on every message.
///
/// Returns `true` if the tmux target was newly set or changed, `false` if it
/// was already up-to-date or absent. Callers can use this to decide whether to
/// send a "reconnected" notification.
async fn check_and_update_tmux_target(ctx: &HandlerContext, msg: &BridgeMessage) -> bool {
    let meta = msg.meta();

    let new_target = match meta.tmux_target() {
        Some(t) => t,
        None => return false,
    };
    let new_socket = meta.tmux_socket();

    let current = ctx.session_tmux.read().await.get(&msg.session_id).cloned();
    if current.as_deref() == Some(new_target) {
        return false;
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

    true
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
/// Returns `false` if the session is non-interactive and should be ignored.
async fn ensure_session_exists(ctx: &HandlerContext, msg: &BridgeMessage) -> bool {
    // Suppress topic creation for non-interactive sessions (claude -p, SDK, CI).
    // These are programmatic pipeline calls, not user-driven sessions.
    let meta = msg.meta();
    if meta.is_non_interactive() {
        tracing::debug!(
            session_id = %msg.session_id,
            entrypoint = ?meta.entrypoint(),
            "Non-interactive session, suppressing"
        );
        return false;
    }
    let sid = msg.session_id.clone();
    let existing = ctx
        .db_op(move |sess| sess.get_session(&sid).ok().flatten())
        .await;

    if let Some(session) = existing {
        // BUG-009: Reactivate ended session if hook events are still arriving
        if session.status != crate::types::SessionStatus::Active {
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
            // BUG-002: Acquire topic creation lock to prevent duplicate topics
            // from concurrent messages for the same session.
            let lock = {
                let mut locks = ctx.topic_locks.write().await;
                if let Some(state) = locks.get(&msg.session_id) {
                    // Another task is already creating the topic -- wait for it.
                    let notify = Arc::clone(&state.notify);
                    drop(locks);
                    let _ = tokio::time::timeout(
                        tokio::time::Duration::from_secs(5),
                        notify.notified(),
                    )
                    .await;
                    return true;
                }
                let state = Arc::new(TopicCreationState {
                    notify: Arc::new(tokio::sync::Notify::new()),
                });
                locks.insert(msg.session_id.clone(), state.clone());
                state
            };

            let hostname = session.hostname.as_deref();
            let project_dir = session.project_dir.as_deref();
            let topic_name =
                HandlerContext::format_topic_name(&msg.session_id, hostname, project_dir);
            // Hash session_id to pick a color (6 valid Telegram topic colors)
            let color_index =
                msg.session_id
                    .bytes()
                    .fold(0u32, |acc, b| acc.wrapping_add(b as u32)) as usize
                    % 6;
            if let Ok(Some(tid)) = ctx.bot.create_forum_topic(&topic_name, color_index).await {
                let sid = msg.session_id.clone();
                ctx.db_op(move |sess| {
                    if let Err(e) = sess.set_session_thread(&sid, tid) {
                        tracing::error!(session_id = %sid, thread_id = tid, error = %e, "Failed to save thread_id to DB");
                    }
                })
                .await;
                ctx.session_threads
                    .write()
                    .await
                    .insert(msg.session_id.clone(), tid);
                // ADR-013 E3: Enhanced session resume context message.
                // Include custom title and inactivity duration if available.
                let resume_msg = {
                    let custom_title = ctx.custom_titles.read().await.get(&msg.session_id).cloned();
                    let inactivity_str = {
                        let la = chrono::DateTime::parse_from_rfc3339(&session.last_activity).ok();
                        la.map(|t| {
                            let elapsed = chrono::Utc::now() - t.to_utc();
                            let total_mins = elapsed.num_minutes().max(0);
                            if total_mins < 60 {
                                format!("{}m", total_mins)
                            } else {
                                let hours = total_mins / 60;
                                let mins = total_mins % 60;
                                format!("{}h {}m", hours, mins)
                            }
                        })
                    };

                    match (custom_title, inactivity_str) {
                        (Some(title), Some(dur)) => {
                            format!(
                                "\u{1F504} Session resumed: {title}\n_Inactive for {dur}. Previous topic was auto-deleted._"
                            )
                        }
                        (Some(title), None) => {
                            format!(
                                "\u{1F504} Session resumed: {title}\n_Previous topic was auto-deleted._"
                            )
                        }
                        (None, Some(dur)) => {
                            format!(
                                "\u{1F504} *Session resumed*\n_Inactive for {dur}. Previous topic was auto-deleted._"
                            )
                        }
                        (None, None) => {
                            "\u{1F504} *Session resumed*\n\n_Previous topic was auto-deleted. New topic created._".to_string()
                        }
                    }
                };
                ctx.bot
                    .send_message(
                        &resume_msg,
                        Some(&SendOptions {
                            parse_mode: Some("Markdown".into()),
                            ..Default::default()
                        }),
                        Some(tid),
                    )
                    .await;
                let _ = ctx.bot.unpin_all_topic_messages(tid).await;
            }

            // Release topic creation lock
            lock.notify.notify_waiters();
            ctx.topic_locks.write().await.remove(&msg.session_id);
        }
        return true;
    }

    // BUG-002/BUG-010: Atomically check-and-insert the topic creation lock.
    // Using a write lock for the entire check+insert prevents two callers from
    // both seeing "no lock" and racing to create duplicate topics.
    {
        let mut locks = ctx.topic_locks.write().await;
        if let Some(state) = locks.get(&msg.session_id) {
            let notify = Arc::clone(&state.notify);
            drop(locks);
            let _ =
                tokio::time::timeout(tokio::time::Duration::from_secs(5), notify.notified()).await;
            return true;
        }
        // Insert lock before releasing — concurrent callers will wait above.
        if ctx.config.use_threads {
            let state = Arc::new(TopicCreationState {
                notify: Arc::new(tokio::sync::Notify::new()),
            });
            locks.insert(msg.session_id.clone(), state);
        }
    }

    // Create session on-the-fly
    tracing::info!(session_id = %msg.session_id, "Creating session on-the-fly");
    socket_handlers::handle_session_start(ctx, msg).await;
    true
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
                format!(" `{}`", crate::formatting::short_path(fp))
            } else {
                String::new()
            }
        }
        "Bash" => {
            let cmd = s("command");
            if !cmd.is_empty() {
                let truncated = truncate(cmd, 50);
                format!("\n`{truncated}`")
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
                let truncated: String = url.chars().take(40).collect();
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
fn escape_markdown_v1(text: &str) -> String {
    text.replace('`', "'")
}

/// H6.1: Resolve a short session_id prefix (from callback_data) to the full
/// session_id key in the pending_questions map. Returns `None` if no match.
fn resolve_pending_key<V>(
    pq: &HashMap<String, V>,
    short_key: &str,
) -> Option<String> {
    pq.keys().find(|k| k.starts_with(short_key)).cloned()
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

/// ADR-011 Fix #9: Map a message type to its delivery priority tier.
///
/// Returns a static string label that identifies the priority band:
///   "critical" — must arrive promptly; user approval or session lifecycle events.
///   "normal"   — ordinary conversational traffic.
///   "low"      — high-volume diagnostic noise that can be deprioritised.
///
/// This function is intentionally separate from the actual priority plumbing so
/// that the mapping logic can be reviewed independently. The bot/client.rs module
/// will consume these labels once it exposes a priority-aware send interface.
fn message_priority(msg_type: &MessageType) -> &'static str {
    match msg_type {
        MessageType::ApprovalRequest
        | MessageType::SessionStart
        | MessageType::SessionEnd
        | MessageType::Error => "critical",

        MessageType::ToolStart
        | MessageType::PreCompact
        | MessageType::TurnComplete
        | MessageType::SendImage => "low",

        // AgentResponse, UserInput, ToolResult, SessionRename, and everything else.
        _ => "normal",
    }
}

/// Add an echo prevention key with TTL.
async fn add_echo_key(ctx: &HandlerContext, session_id: &str, text: &str) {
    // Use \0 as separator — cannot appear in session IDs (alphanumeric + . _ -)
    // or in UTF-8 text, preventing key collisions.
    let key = format!("{session_id}\0{text}");
    ctx.recent_inputs.write().await.insert(key.clone());

    let inputs = Arc::clone(&ctx.recent_inputs);
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(ECHO_TTL_SECS)).await;
        inputs.write().await.remove(&key);
    });
}

/// Get tmux target for a session (cache -> DB -> live detection fallback).
///
/// ADR-013 F6/D5: Three-tier lookup:
///   1. In-memory cache (zero cost)
///   2. Database (cheap SQLite query)
///   3. Live detection via tmux CLI (~100ms, only on cache miss)
///
/// When live detection succeeds, the result is stored in both cache and DB
/// so subsequent lookups hit tier 1.
async fn get_tmux_target(
    ctx: &HandlerContext,
    session_id: &str,
    _tmux_socket: Option<&str>,
) -> Option<String> {
    // Tier 1: Check in-memory cache
    if let Some(target) = ctx.session_tmux.read().await.get(session_id) {
        return Some(target.clone());
    }

    // Tier 2: Fallback to DB
    let sid = session_id.to_string();
    let tmux_info = ctx
        .db_op(move |sess| sess.get_tmux_info(&sid).ok().flatten())
        .await;
    if let Some((target, _socket)) = tmux_info {
        ctx.session_tmux
            .write()
            .await
            .insert(session_id.to_string(), target.clone());
        return Some(target);
    }

    // Tier 3: ADR-013 F6 — Live detection fallback (~100ms).
    // Try detect_tmux_session first (uses $TMUX env), then find_claude_code_session
    // (scans all tmux panes for a "claude" process).
    if let Some(info) = InputInjector::detect_tmux_session() {
        let target = info.target.clone();
        let socket = info.socket.clone();
        tracing::info!(
            session_id,
            target = %target,
            "ADR-013 F6: Live tmux detection succeeded (detect_tmux_session)"
        );
        // Store in cache
        ctx.session_tmux
            .write()
            .await
            .insert(session_id.to_string(), target.clone());
        // Store in DB
        let sid = session_id.to_string();
        let t = target.clone();
        let s = socket.clone();
        ctx.db_op(move |sess| {
            let _ = sess.set_tmux_info(&sid, Some(&t), s.as_deref());
        })
        .await;
        return Some(target);
    }

    if let Some(session_name) = InputInjector::find_claude_code_session() {
        // find_claude_code_session returns just a session name, not a full target.
        // Use "session_name:0.0" as the target (first window, first pane).
        let target = format!("{session_name}:0.0");
        tracing::info!(
            session_id,
            target = %target,
            "ADR-013 F6: Live tmux detection succeeded (find_claude_code_session)"
        );
        // Store in cache
        ctx.session_tmux
            .write()
            .await
            .insert(session_id.to_string(), target.clone());
        // Store in DB
        let sid = session_id.to_string();
        let t = target.clone();
        ctx.db_op(move |sess| {
            let _ = sess.set_tmux_info(&sid, Some(&t), None);
        })
        .await;
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
}
