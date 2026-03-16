use crate::bot::{self, TelegramBot};
use crate::config::Config;
use crate::error::Result;
use crate::formatting;
use crate::injector::InputInjector;
use crate::session::SessionManager;
use crate::socket::SocketServer;
use crate::summarizer::LlmSummarizer;
use crate::types::{BridgeMessage, InlineButton, MessageType, SendOptions, SessionStatus};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::time::{self, Duration};

/// Central bridge daemon coordinating all components
pub struct Bridge {
    config: Config,
    bot: TelegramBot,
    sessions: Arc<Mutex<SessionManager>>,
    injector: Arc<Mutex<InputInjector>>,
    session_threads: Arc<RwLock<HashMap<String, i32>>>,
    session_tmux_targets: Arc<RwLock<HashMap<String, String>>>,
    recent_telegram_inputs: Arc<RwLock<HashSet<String>>>,
    tool_input_cache: Arc<RwLock<HashMap<String, CachedToolInput>>>,
    compacting_sessions: Arc<RwLock<HashSet<String>>>,
    pending_deletions: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    summarizer: Arc<LlmSummarizer>,
    /// Sessions that have already been renamed with a task description
    named_sessions: Arc<RwLock<HashSet<String>>>,
    /// Runtime toggle for mirroring (read from status.json, flipped by `ctm toggle`)
    mirroring_enabled: Arc<AtomicBool>,
}

struct CachedToolInput {
    tool: String,
    input: serde_json::Value,
    _timestamp: u64,
}

impl Bridge {
    pub fn new(config: Config) -> Result<Self> {
        let bot = TelegramBot::new(&config.bot_token, config.chat_id);
        let sessions = SessionManager::new(&config.config_dir, 5)?;
        let injector = InputInjector::new();
        let summarizer =
            LlmSummarizer::new(config.llm_summarize_url.clone(), config.llm_api_key.clone());

        let mirroring_enabled = Arc::new(AtomicBool::new(crate::config::read_mirror_status(
            &config.config_dir,
        )));

        Ok(Self {
            config,
            bot,
            sessions: Arc::new(Mutex::new(sessions)),
            injector: Arc::new(Mutex::new(injector)),
            session_threads: Arc::new(RwLock::new(HashMap::new())),
            session_tmux_targets: Arc::new(RwLock::new(HashMap::new())),
            recent_telegram_inputs: Arc::new(RwLock::new(HashSet::new())),
            tool_input_cache: Arc::new(RwLock::new(HashMap::new())),
            compacting_sessions: Arc::new(RwLock::new(HashSet::new())),
            pending_deletions: Arc::new(RwLock::new(HashMap::new())),
            summarizer: Arc::new(summarizer),
            named_sessions: Arc::new(RwLock::new(HashSet::new())),
            mirroring_enabled,
        })
    }

    /// Start the bridge daemon
    pub async fn start(&self) -> Result<()> {
        tracing::info!("Starting bridge daemon...");

        // Start socket server
        let mut socket = SocketServer::new(self.config.socket_path.clone());
        let (mut msg_rx, broadcast_tx) = socket.listen().await?;

        // Initialize input injector
        {
            let mut inj = self.injector.lock().await;
            if InputInjector::is_tmux_available() {
                if let Some(info) = InputInjector::detect_tmux_session() {
                    inj.set_target(&info.target, info.socket.as_deref());
                    tracing::info!(target = %info.target, "Input injector ready");
                }
            } else {
                tracing::warn!("tmux not available - Telegram -> CLI disabled");
            }
        }

        // Write status file with our PID
        crate::config::write_mirror_status(
            &self.config.config_dir,
            self.mirroring_enabled.load(Ordering::Relaxed),
            Some(std::process::id()),
        );

        // Send startup notification
        let _ = self
            .bot
            .send_message(
                "\u{1f7e2} *Bridge Daemon Started*\n\nClaude Code sessions will now be mirrored here.",
                &SendOptions::default(),
                None,
            )
            .await;

        // Spawn socket message handler
        let bridge = self.clone_shared();
        let btx = broadcast_tx.clone();
        let socket_task = tokio::spawn(async move {
            while let Some(msg) = msg_rx.recv().await {
                if let Err(e) = bridge.handle_socket_message(msg, &btx).await {
                    tracing::error!(error = %e, "Failed to handle socket message");
                }
            }
        });

        // Spawn Telegram update poller
        let bridge = self.clone_shared();
        let poll_task = tokio::spawn(async move {
            bridge.poll_telegram_updates().await;
        });

        // Spawn cleanup timer (every 5 minutes)
        let bridge = self.clone_shared();
        let cleanup_task = tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(300));
            loop {
                interval.tick().await;
                bridge.cleanup_stale_sessions().await;
            }
        });

        // Wait for any task to complete (shouldn't happen normally)
        tokio::select! {
            _ = socket_task => tracing::error!("Socket task ended"),
            _ = poll_task => tracing::error!("Poll task ended"),
            _ = cleanup_task => tracing::error!("Cleanup task ended"),
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received SIGINT, shutting down...");
            }
        }

        // Shutdown
        let _ = self
            .bot
            .send_message(
                "\u{1f534} *Bridge Daemon Stopped*\n\nSession mirroring is now disabled.",
                &SendOptions::default(),
                None,
            )
            .await;

        socket.cleanup();
        tracing::info!("Bridge daemon stopped");
        Ok(())
    }

    /// Create a lightweight clone for spawning into tasks
    fn clone_shared(&self) -> BridgeShared {
        BridgeShared {
            config: self.config.clone(),
            bot: self.bot.clone(),
            sessions: self.sessions.clone(),
            injector: self.injector.clone(),
            session_threads: self.session_threads.clone(),
            session_tmux_targets: self.session_tmux_targets.clone(),
            recent_telegram_inputs: self.recent_telegram_inputs.clone(),
            tool_input_cache: self.tool_input_cache.clone(),
            compacting_sessions: self.compacting_sessions.clone(),
            pending_deletions: self.pending_deletions.clone(),
            summarizer: self.summarizer.clone(),
            named_sessions: self.named_sessions.clone(),
            mirroring_enabled: self.mirroring_enabled.clone(),
        }
    }
}

/// Shared state that can be sent across tasks
#[derive(Clone)]
struct BridgeShared {
    config: Config,
    bot: TelegramBot,
    sessions: Arc<Mutex<SessionManager>>,
    injector: Arc<Mutex<InputInjector>>,
    session_threads: Arc<RwLock<HashMap<String, i32>>>,
    session_tmux_targets: Arc<RwLock<HashMap<String, String>>>,
    recent_telegram_inputs: Arc<RwLock<HashSet<String>>>,
    tool_input_cache: Arc<RwLock<HashMap<String, CachedToolInput>>>,
    compacting_sessions: Arc<RwLock<HashSet<String>>>,
    pending_deletions: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    summarizer: Arc<LlmSummarizer>,
    named_sessions: Arc<RwLock<HashSet<String>>>,
    mirroring_enabled: Arc<AtomicBool>,
}

impl BridgeShared {
    // ============ Socket Message Handling (CLI -> Telegram) ============

    async fn handle_socket_message(
        &self,
        msg: BridgeMessage,
        _broadcast_tx: &broadcast::Sender<BridgeMessage>,
    ) -> Result<()> {
        // CRIT-02: Validate session_id to prevent unbounded memory growth
        const MAX_SESSION_ID_LEN: usize = 128;
        if msg.session_id.len() > MAX_SESSION_ID_LEN
            || !msg
                .session_id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            tracing::warn!(
                len = msg.session_id.len(),
                "Rejecting message with invalid session_id"
            );
            return Ok(());
        }

        tracing::debug!(msg_type = ?msg.msg_type, session_id = %msg.session_id, "Socket message");

        // Update session activity
        {
            let sessions = self.sessions.lock().await;
            if sessions.get_session(&msg.session_id).is_some() {
                sessions.update_activity(&msg.session_id);
            }
        }

        // Auto-update tmux target if changed
        self.check_and_update_tmux_target(&msg).await;

        // Handle system commands (toggle/enable/disable) regardless of mirroring state
        if msg.msg_type == MessageType::Command {
            self.handle_command(&msg).await?;
            return Ok(());
        }

        // Gate: skip all Telegram sends when mirroring is disabled
        if !self.mirroring_enabled.load(Ordering::Relaxed) {
            tracing::debug!(msg_type = ?msg.msg_type, "Mirroring disabled, skipping");
            return Ok(());
        }

        match msg.msg_type {
            MessageType::SessionStart => self.handle_session_start(&msg).await?,
            MessageType::SessionEnd => self.handle_session_end(&msg).await?,
            MessageType::AgentResponse => {
                self.ensure_session_exists(&msg).await?;
                self.handle_agent_response(&msg).await?;
            }
            MessageType::ToolStart => {
                self.ensure_session_exists(&msg).await?;
                self.handle_tool_start(&msg).await?;
            }
            MessageType::ToolResult => {
                self.ensure_session_exists(&msg).await?;
                self.handle_tool_result(&msg).await?;
            }
            MessageType::UserInput => {
                self.ensure_session_exists(&msg).await?;
                self.handle_user_input(&msg).await?;
            }
            MessageType::ApprovalRequest => {
                self.ensure_session_exists(&msg).await?;
                self.handle_approval_request(&msg).await?;
            }
            MessageType::Error => {
                self.ensure_session_exists(&msg).await?;
                let thread_id = self.get_session_thread_id(&msg.session_id).await;
                let _ = self
                    .bot
                    .send_message(
                        &formatting::format_error(&msg.content),
                        &SendOptions::default(),
                        thread_id,
                    )
                    .await;
            }
            MessageType::TurnComplete => {
                tracing::debug!(session_id = %msg.session_id, "Turn complete");
                if self
                    .compacting_sessions
                    .read()
                    .await
                    .contains(&msg.session_id)
                {
                    self.handle_compact_complete(&msg.session_id).await;
                }
            }
            MessageType::PreCompact => {
                self.ensure_session_exists(&msg).await?;
                self.handle_pre_compact(&msg).await?;
            }
            MessageType::SendImage => {
                self.handle_send_image(&msg).await?;
            }
            MessageType::Command => {} // already handled above
            _ => {
                tracing::debug!(msg_type = ?msg.msg_type, "Unhandled message type");
            }
        }

        Ok(())
    }

    async fn handle_session_start(&self, msg: &BridgeMessage) -> Result<()> {
        let hostname = msg.get_metadata_str("hostname").map(|s| s.to_string());
        let project_dir = msg.get_metadata_str("projectDir").map(|s| s.to_string());
        let tmux_target = msg.get_metadata_str("tmuxTarget").map(|s| s.to_string());
        let tmux_socket = msg.get_metadata_str("tmuxSocket").map(|s| s.to_string());

        let session_id = {
            let sessions = self.sessions.lock().await;
            sessions.create_session(
                Some(&msg.session_id),
                self.config.chat_id,
                project_dir.as_deref(),
                hostname.as_deref(),
                tmux_target.as_deref(),
                tmux_socket.as_deref(),
            )?
        };

        // Cache tmux target
        if let Some(target) = &tmux_target {
            self.session_tmux_targets
                .write()
                .await
                .insert(session_id.clone(), target.clone());
        }

        // Create or reuse forum topic
        let mut thread_id = {
            let sessions = self.sessions.lock().await;
            sessions.get_session_thread(&session_id)
        };

        if let Some(tid) = thread_id {
            self.session_threads
                .write()
                .await
                .insert(session_id.clone(), tid as i32);
        } else if self.config.use_threads {
            let topic_name =
                format_topic_name(&session_id, hostname.as_deref(), project_dir.as_deref());
            if let Ok(Some(tid)) = self.bot.create_forum_topic(&topic_name).await {
                let sessions = self.sessions.lock().await;
                sessions.set_session_thread(&session_id, tid as i64);
                self.session_threads
                    .write()
                    .await
                    .insert(session_id.clone(), tid);
                thread_id = Some(tid as i64);

                // Unpin auto-pinned first message
                let _ = self.bot.unpin_all_topic_messages(tid).await;
            }
        }

        // Send session start notification
        let mut info = formatting::format_session_start(
            &session_id,
            project_dir.as_deref(),
            hostname.as_deref(),
        );
        if let Some(target) = &tmux_target {
            info.push_str(&format!("\n\u{1f4fa} tmux: `{}`", target));
        }

        let _ = self
            .bot
            .send_message(&info, &SendOptions::default(), thread_id.map(|t| t as i32))
            .await;

        Ok(())
    }

    async fn handle_session_end(&self, msg: &BridgeMessage) -> Result<()> {
        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get_session(&msg.session_id)
        };

        if let Some(session) = session {
            let duration = (chrono::Utc::now() - session.started_at)
                .num_milliseconds()
                .max(0) as u64;
            let thread_id = self.get_session_thread_id(&msg.session_id).await;

            let _ = self
                .bot
                .send_message(
                    &formatting::format_session_end(&msg.session_id, Some(duration)),
                    &SendOptions::default(),
                    thread_id,
                )
                .await;

            if let Some(tid) = thread_id {
                if self.config.auto_delete_topics {
                    let delay = Duration::from_secs(self.config.topic_delete_delay_minutes * 60);
                    let bot = self.bot.clone();
                    let sessions = self.sessions.clone();
                    let threads = self.session_threads.clone();
                    let sid = msg.session_id.clone();

                    let handle = tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        if let Ok(true) = bot.delete_forum_topic(tid).await {
                            tracing::info!(session_id = %sid, %tid, "Auto-deleted forum topic");
                            threads.write().await.remove(&sid);
                            let s = sessions.lock().await;
                            s.clear_thread_id(&sid);
                        }
                    });

                    self.pending_deletions
                        .write()
                        .await
                        .insert(msg.session_id.clone(), handle);
                } else {
                    let _ = self.bot.close_forum_topic(tid).await;
                    self.session_threads.write().await.remove(&msg.session_id);
                }
            }

            self.session_tmux_targets
                .write()
                .await
                .remove(&msg.session_id);

            let sessions = self.sessions.lock().await;
            sessions.end_session(&msg.session_id, SessionStatus::Ended);
        }

        Ok(())
    }

    async fn handle_agent_response(&self, msg: &BridgeMessage) -> Result<()> {
        let thread_id = self.get_session_thread_id(&msg.session_id).await;
        let _ = self
            .bot
            .send_message(
                &formatting::format_agent_response(&msg.content),
                &SendOptions::default(),
                thread_id,
            )
            .await;
        Ok(())
    }

    async fn handle_tool_start(&self, msg: &BridgeMessage) -> Result<()> {
        if !self.config.verbose {
            return Ok(());
        }

        let tool_name = msg
            .get_metadata_str("tool")
            .unwrap_or("Unknown")
            .to_string();
        let tool_input = msg.get_metadata_value("input").cloned();
        let thread_id = self.get_session_thread_id(&msg.session_id).await;

        // Cache tool input for details button
        let tool_use_id = format!(
            "tool_{}_{}",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4().simple()
        );
        if let Some(input) = &tool_input {
            self.tool_input_cache.write().await.insert(
                tool_use_id.clone(),
                CachedToolInput {
                    tool: tool_name.clone(),
                    input: input.clone(),
                    _timestamp: chrono::Utc::now().timestamp_millis() as u64,
                },
            );

            // Auto-expire after 5 minutes
            let cache = self.tool_input_cache.clone();
            let uid = tool_use_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(300)).await;
                cache.write().await.remove(&uid);
            });
        }

        let summary = self
            .summarizer
            .summarize(&tool_name, tool_input.as_ref())
            .await;
        let text = format!("\u{1f527} {}", summary);

        if tool_input.is_some() {
            let _ = self
                .bot
                .send_with_buttons(
                    &text,
                    &[InlineButton {
                        text: "\u{1f4cb} Details".to_string(),
                        callback_data: format!("tooldetails:{}", tool_use_id),
                    }],
                    &SendOptions::default(),
                    thread_id,
                )
                .await;
        } else {
            let _ = self
                .bot
                .send_message(&text, &SendOptions::default(), thread_id)
                .await;
        }

        Ok(())
    }

    async fn handle_tool_result(&self, msg: &BridgeMessage) -> Result<()> {
        if !self.config.verbose {
            return Ok(());
        }

        let tool_name = msg.get_metadata_str("tool").unwrap_or("Unknown");
        let thread_id = self.get_session_thread_id(&msg.session_id).await;

        let result_summary = formatting::summarize_tool_result(tool_name, &msg.content);
        let text = format!("\u{2705} {}", result_summary);

        let _ = self
            .bot
            .send_message(&text, &SendOptions::default(), thread_id)
            .await;
        Ok(())
    }

    async fn handle_user_input(&self, msg: &BridgeMessage) -> Result<()> {
        let source = msg.get_metadata_str("source").unwrap_or("cli");
        if source == "telegram" {
            return Ok(());
        }

        // Check dedup
        let input_key = format!("{}:{}", msg.session_id, msg.content.trim());
        {
            let inputs = self.recent_telegram_inputs.read().await;
            if inputs.contains(&input_key) {
                return Ok(());
            }
        }

        let thread_id = self.get_session_thread_id(&msg.session_id).await;

        // Rename topic on first user prompt to include the task description
        if let Some(tid) = thread_id {
            let already_named = self.named_sessions.read().await.contains(&msg.session_id);
            if !already_named && !msg.content.trim().is_empty() {
                self.named_sessions
                    .write()
                    .await
                    .insert(msg.session_id.clone());

                let project = {
                    let sessions = self.sessions.lock().await;
                    sessions
                        .get_session(&msg.session_id)
                        .and_then(|s| s.project_dir)
                        .and_then(|d| d.rsplit('/').next().map(|s| s.to_string()))
                };

                let task_desc = build_task_description(&msg.content);
                let topic_name = match project {
                    Some(proj) => format!("{} — {}", proj, task_desc),
                    None => task_desc,
                };

                let _ = self.bot.edit_forum_topic(tid, &topic_name).await;
            }
        }

        let _ = self
            .bot
            .send_message(
                &format!("\u{1f464} *User (cli):*\n{}", msg.content),
                &SendOptions::default(),
                thread_id,
            )
            .await;
        Ok(())
    }

    async fn handle_approval_request(&self, msg: &BridgeMessage) -> Result<()> {
        let approval_id = {
            let sessions = self.sessions.lock().await;
            sessions.create_approval(&msg.session_id, &msg.content)?
        };
        let thread_id = self.get_session_thread_id(&msg.session_id).await;

        let _ = self
            .bot
            .send_with_buttons(
                &formatting::format_approval_request(&msg.content),
                &[
                    InlineButton {
                        text: "\u{2705} Approve".to_string(),
                        callback_data: format!("approve:{}", approval_id),
                    },
                    InlineButton {
                        text: "\u{274c} Reject".to_string(),
                        callback_data: format!("reject:{}", approval_id),
                    },
                    InlineButton {
                        text: "\u{1f6d1} Abort".to_string(),
                        callback_data: format!("abort:{}", approval_id),
                    },
                ],
                &SendOptions::default(),
                thread_id,
            )
            .await;
        Ok(())
    }

    async fn handle_pre_compact(&self, msg: &BridgeMessage) -> Result<()> {
        let trigger = msg.get_metadata_str("trigger").unwrap_or("auto");
        let thread_id = self.get_session_thread_id(&msg.session_id).await;

        self.compacting_sessions
            .write()
            .await
            .insert(msg.session_id.clone());

        let text = if trigger == "manual" {
            "\u{1f504} *Compacting session context...*\n\n_User requested /compact_"
        } else {
            "\u{23f3} *Context limit reached*\n\n_Summarizing conversation, please wait..._"
        };

        let _ = self
            .bot
            .send_message(text, &SendOptions::default(), thread_id)
            .await;
        Ok(())
    }

    async fn handle_compact_complete(&self, session_id: &str) {
        self.compacting_sessions.write().await.remove(session_id);
        let thread_id = self.get_session_thread_id(session_id).await;
        let _ = self
            .bot
            .send_message(
                "\u{2705} *Compaction complete*\n\n_Resuming session..._",
                &SendOptions::default(),
                thread_id,
            )
            .await;
    }

    async fn handle_send_image(&self, msg: &BridgeMessage) -> Result<()> {
        let path = std::path::Path::new(&msg.content);

        if !path.is_absolute() || msg.content.contains("..") {
            tracing::warn!(path = %msg.content, "SendImage: invalid path");
            return Ok(());
        }
        if !path.exists() {
            tracing::warn!(path = %msg.content, "SendImage: file not found");
            return Ok(());
        }

        let thread_id = self.get_session_thread_id(&msg.session_id).await;
        let caption = msg.get_metadata_str("caption");

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let is_image = matches!(
            ext.as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
        );

        if is_image {
            self.bot.send_photo(path, caption, thread_id).await?;
        } else {
            self.bot.send_document(path, caption, thread_id).await?;
        }

        tracing::info!(path = %msg.content, session_id = %msg.session_id, "Sent file to Telegram");
        Ok(())
    }

    // ============ Telegram Update Handling (Telegram -> CLI) ============

    async fn poll_telegram_updates(&self) {
        let mut offset = 0i64;

        loop {
            match self.bot.get_updates(offset).await {
                Ok(updates) => {
                    for update in &updates {
                        // HIGH-06: Use i64 to prevent u32->i32 overflow at i32::MAX
                        offset = (update.id.0 as i64) + 1;

                        // Security fix #5: Chat ID filter on ALL updates
                        if !bot::is_authorized_chat(update, self.config.chat_id) {
                            tracing::warn!("Unauthorized update, ignoring");
                            continue;
                        }

                        self.process_telegram_update(update).await;
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to get Telegram updates");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn process_telegram_update(&self, update: &teloxide::types::Update) {
        use teloxide::types::UpdateKind;

        match &update.kind {
            UpdateKind::Message(msg) => {
                if let Some(text) = msg.text() {
                    // Handle commands
                    if text.starts_with('/') {
                        self.handle_telegram_command(msg, text).await;
                    } else {
                        self.handle_telegram_message(msg, text).await;
                    }
                } else if msg.photo().is_some() {
                    self.handle_telegram_photo(msg).await;
                } else if msg.document().is_some() {
                    self.handle_telegram_document(msg).await;
                }
            }
            UpdateKind::CallbackQuery(query) => {
                self.handle_callback_query(query).await;
            }
            _ => {}
        }
    }

    async fn handle_telegram_command(&self, msg: &teloxide::types::Message, text: &str) {
        let thread_id = msg.thread_id.map(|t| t.0 .0);
        let cmd = text.split_whitespace().next().unwrap_or("");

        match cmd {
            "/start" => {
                let _ = self
                    .bot
                    .send_message(
                        "\u{1f44b} *Claude Code Mirror Bot*\n\n\
                         I mirror your Claude Code sessions to Telegram.\n\
                         Use /help to see all available commands.",
                        &SendOptions::default(),
                        thread_id,
                    )
                    .await;
            }
            "/help" => {
                let _ = self
                    .bot
                    .send_message(
                        &formatting::format_help(),
                        &SendOptions::default(),
                        thread_id,
                    )
                    .await;
            }
            "/status" => {
                let (active, pending) = {
                    let sessions = self.sessions.lock().await;
                    sessions.get_stats()
                };
                let _ = self
                    .bot
                    .send_message(
                        &format!(
                            "\u{1f4ca} *Status*\n\nActive sessions: {}\nPending approvals: {}",
                            active, pending
                        ),
                        &SendOptions::default(),
                        thread_id,
                    )
                    .await;
            }
            "/ping" => {
                let _ = self
                    .bot
                    .send_message("\u{1f3d3} Pong!", &SendOptions::default(), thread_id)
                    .await;
            }
            _ => {
                tracing::debug!(cmd, "Unknown command");
            }
        }
    }

    async fn handle_telegram_message(&self, msg: &teloxide::types::Message, text: &str) {
        let thread_id = msg.thread_id.map(|t| t.0 .0);

        // Ignore messages in General topic (no thread_id)
        let tid = match thread_id {
            Some(t) => t,
            None => return,
        };

        // Find session by thread_id
        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get_session_by_thread_id(tid as i64)
        };

        let session = match session {
            Some(s) => s,
            None => return, // Unknown topic, ignore (multi-bot)
        };

        // Resolve tmux target BEFORE acquiring injector lock (prevent deadlock)
        let tmux_target = self
            .session_tmux_targets
            .read()
            .await
            .get(&session.id)
            .cloned();
        let (resolved_target, resolved_socket) = if let Some(target) = tmux_target {
            (Some(target), session.tmux_socket.clone())
        } else {
            // Restore from database
            let (db_target, db_socket) = {
                let sessions = self.sessions.lock().await;
                sessions.get_tmux_info(&session.id)
            };
            if let Some(target) = &db_target {
                self.session_tmux_targets
                    .write()
                    .await
                    .insert(session.id.clone(), target.clone());
            }
            (db_target, db_socket)
        };

        // Now acquire injector lock with no other locks held
        let mut inj = self.injector.lock().await;
        if let Some(target) = &resolved_target {
            inj.set_target(target, resolved_socket.as_deref());
        }

        // Check for cc command prefix
        if let Some(command) = bot::parse_cc_command(text) {
            let input_key = format!("{}:{}", session.id, command);
            self.recent_telegram_inputs
                .write()
                .await
                .insert(input_key.clone());
            let inputs = self.recent_telegram_inputs.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                inputs.write().await.remove(&input_key);
            });

            let _ = inj.send_slash_command(&command);
            return;
        }

        // Check for interrupt/kill commands
        if bot::is_interrupt_command(text) {
            let result = inj.send_key("Escape");
            let msg_text = if result.as_ref().map(|r| *r).unwrap_or(false) {
                "\u{23f8}\u{fe0f} *Interrupt sent* (Escape)\n\n_Claude should pause._"
            } else {
                "\u{26a0}\u{fe0f} *Could not send interrupt*\n\nNo tmux session found."
            };
            let _ = self
                .bot
                .send_message(msg_text, &SendOptions::default(), Some(tid))
                .await;
            return;
        }

        if bot::is_kill_command(text) {
            let result = inj.send_key("Ctrl-C");
            let msg_text = if result.as_ref().map(|r| *r).unwrap_or(false) {
                "\u{1f6d1} *Kill sent* (Ctrl-C)\n\n_Claude should exit._"
            } else {
                "\u{26a0}\u{fe0f} *Could not send kill*\n\nNo tmux session found."
            };
            let _ = self
                .bot
                .send_message(msg_text, &SendOptions::default(), Some(tid))
                .await;
            return;
        }

        // Track to prevent echo
        let input_key = format!("{}:{}", session.id, text.trim());
        self.recent_telegram_inputs
            .write()
            .await
            .insert(input_key.clone());
        let inputs = self.recent_telegram_inputs.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            inputs.write().await.remove(&input_key);
        });

        // Inject input
        match inj.inject(text) {
            Ok(true) => {
                tracing::info!(session_id = %session.id, "Injected input to CLI");
            }
            Ok(false) | Err(_) => {
                let _ = self
                    .bot
                    .send_message(
                        "\u{26a0}\u{fe0f} *Could not send input to CLI*\n\n\
                         No tmux session found. Make sure Claude Code is running in tmux.",
                        &SendOptions::default(),
                        Some(tid),
                    )
                    .await;
            }
        }
    }

    async fn handle_telegram_photo(&self, msg: &teloxide::types::Message) {
        let photos = match msg.photo() {
            Some(p) if !p.is_empty() => p,
            _ => return,
        };
        let photo = &photos[photos.len() - 1]; // largest size

        let thread_id = msg.thread_id.map(|t| t.0 .0);
        let tid = match thread_id {
            Some(t) => t,
            None => return,
        };

        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get_session_by_thread_id(tid as i64)
        };
        let session = match session {
            Some(s) => s,
            None => return,
        };

        // Create download directory
        let download_dir = std::path::Path::new("/tmp/ctm-images");
        if let Err(e) = tokio::fs::create_dir_all(download_dir).await {
            tracing::error!(error = %e, "Failed to create download directory");
            return;
        }
        let _ =
            tokio::fs::set_permissions(download_dir, std::fs::Permissions::from_mode(0o700)).await;

        let uuid = uuid::Uuid::new_v4();
        let temp_path = download_dir.join(format!("{}.downloading", uuid));

        let ext = match self.bot.download_file_to(&photo.file.id, &temp_path).await {
            Ok(ext) => ext,
            Err(e) => {
                tracing::error!(error = %e, "Failed to download photo");
                let _ = tokio::fs::remove_file(&temp_path).await;
                return;
            }
        };

        let final_path = download_dir.join(format!("{}.{}", uuid, ext));
        if let Err(e) = tokio::fs::rename(&temp_path, &final_path).await {
            tracing::error!(error = %e, "Failed to rename downloaded file");
            let _ = tokio::fs::remove_file(&temp_path).await;
            return;
        }
        let _ =
            tokio::fs::set_permissions(&final_path, std::fs::Permissions::from_mode(0o600)).await;

        let mut notification = format!(
            "[Image from Telegram: {} ({}x{})]",
            final_path.display(),
            photo.width,
            photo.height
        );
        if let Some(caption) = msg.caption() {
            notification.push_str(&format!("\nCaption: \"{}\"", caption));
        }

        // Resolve tmux target and inject
        let tmux_target = self
            .session_tmux_targets
            .read()
            .await
            .get(&session.id)
            .cloned();
        let (resolved_target, resolved_socket) = if let Some(target) = tmux_target {
            (Some(target), session.tmux_socket.clone())
        } else {
            let (db_target, db_socket) = {
                let sessions = self.sessions.lock().await;
                sessions.get_tmux_info(&session.id)
            };
            if let Some(target) = &db_target {
                self.session_tmux_targets
                    .write()
                    .await
                    .insert(session.id.clone(), target.clone());
            }
            (db_target, db_socket)
        };

        let mut inj = self.injector.lock().await;
        if let Some(target) = &resolved_target {
            inj.set_target(target, resolved_socket.as_deref());
        }

        match inj.inject(&notification) {
            Ok(true) => {
                tracing::info!(
                    session_id = %session.id,
                    path = %final_path.display(),
                    "Injected photo path to CLI"
                );
            }
            Ok(false) | Err(_) => {
                let _ = self
                    .bot
                    .send_message(
                        "\u{26a0}\u{fe0f} *Photo received but could not inject to CLI*\n\n\
                         File saved but no tmux session found.",
                        &SendOptions::default(),
                        Some(tid),
                    )
                    .await;
            }
        }
    }

    async fn handle_telegram_document(&self, msg: &teloxide::types::Message) {
        let doc = match msg.document() {
            Some(d) => d,
            None => return,
        };

        let thread_id = msg.thread_id.map(|t| t.0 .0);
        let tid = match thread_id {
            Some(t) => t,
            None => return,
        };

        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get_session_by_thread_id(tid as i64)
        };
        let session = match session {
            Some(s) => s,
            None => return,
        };

        let download_dir = std::path::Path::new("/tmp/ctm-images");
        if let Err(e) = tokio::fs::create_dir_all(download_dir).await {
            tracing::error!(error = %e, "Failed to create download directory");
            return;
        }
        let _ =
            tokio::fs::set_permissions(download_dir, std::fs::Permissions::from_mode(0o700)).await;

        let uuid = uuid::Uuid::new_v4();
        let temp_path = download_dir.join(format!("{}.downloading", uuid));

        let ext = match self.bot.download_file_to(&doc.file.id, &temp_path).await {
            Ok(ext) => ext,
            Err(e) => {
                tracing::error!(error = %e, "Failed to download document");
                let _ = tokio::fs::remove_file(&temp_path).await;
                return;
            }
        };

        // Build safe filename: UUID prefix + sanitized original name
        let safe_name = doc
            .file_name
            .as_deref()
            .map(|n| n.replace(['/', '\\'], "_"))
            .map(|n| {
                if n.starts_with('.') {
                    format!("_{}", n)
                } else {
                    n
                }
            })
            .unwrap_or_else(|| format!("file.{}", ext));
        let final_path = download_dir.join(format!("{}_{}", uuid, safe_name));

        if let Err(e) = tokio::fs::rename(&temp_path, &final_path).await {
            tracing::error!(error = %e, "Failed to rename downloaded file");
            let _ = tokio::fs::remove_file(&temp_path).await;
            return;
        }
        let _ =
            tokio::fs::set_permissions(&final_path, std::fs::Permissions::from_mode(0o600)).await;

        let file_size = format_file_size(doc.file.size as u64);
        let mut notification = format!(
            "[File from Telegram: {} ({})]",
            final_path.display(),
            file_size
        );
        if let Some(caption) = msg.caption() {
            notification.push_str(&format!("\nCaption: \"{}\"", caption));
        }

        // Resolve tmux target and inject
        let tmux_target = self
            .session_tmux_targets
            .read()
            .await
            .get(&session.id)
            .cloned();
        let (resolved_target, resolved_socket) = if let Some(target) = tmux_target {
            (Some(target), session.tmux_socket.clone())
        } else {
            let (db_target, db_socket) = {
                let sessions = self.sessions.lock().await;
                sessions.get_tmux_info(&session.id)
            };
            if let Some(target) = &db_target {
                self.session_tmux_targets
                    .write()
                    .await
                    .insert(session.id.clone(), target.clone());
            }
            (db_target, db_socket)
        };

        let mut inj = self.injector.lock().await;
        if let Some(target) = &resolved_target {
            inj.set_target(target, resolved_socket.as_deref());
        }

        match inj.inject(&notification) {
            Ok(true) => {
                tracing::info!(
                    session_id = %session.id,
                    path = %final_path.display(),
                    "Injected document path to CLI"
                );
            }
            Ok(false) | Err(_) => {
                let _ = self
                    .bot
                    .send_message(
                        "\u{26a0}\u{fe0f} *File received but could not inject to CLI*\n\n\
                         File saved but no tmux session found.",
                        &SendOptions::default(),
                        Some(tid),
                    )
                    .await;
            }
        }
    }

    async fn handle_callback_query(&self, query: &teloxide::types::CallbackQuery) {
        let data = match &query.data {
            Some(d) => d.clone(),
            None => return,
        };

        // Parse callback data: "action:id"
        let parts: Vec<&str> = data.splitn(2, ':').collect();
        if parts.len() != 2 {
            return;
        }

        let (action, id) = (parts[0], parts[1]);

        match action {
            "approve" | "reject" | "abort" => {
                let sessions = self.sessions.lock().await;
                let approval = sessions.get_approval(id);

                if let Some(approval) = approval {
                    // HIGH-03: Verify approval belongs to this chat (prevent IDOR)
                    if let Some(session) = sessions.get_session(&approval.session_id) {
                        if session.chat_id != self.config.chat_id {
                            tracing::warn!(
                                approval_id = %id,
                                "Approval belongs to different chat, rejecting"
                            );
                            let _ = self
                                .bot
                                .answer_callback_query(&query.id, Some("Unauthorized"))
                                .await;
                            return;
                        }
                    }

                    let status = match action {
                        "approve" => "approved",
                        "reject" | "abort" => "rejected",
                        _ => "rejected",
                    };
                    sessions.resolve_approval(id, status);

                    if action == "abort" {
                        sessions.end_session(&approval.session_id, SessionStatus::Aborted);
                    }
                }

                let response_text = match action {
                    "approve" => "\u{2705} Approved",
                    "reject" => "\u{274c} Rejected",
                    "abort" => "\u{1f6d1} Session Aborted",
                    _ => "Processed",
                };

                let _ = self
                    .bot
                    .answer_callback_query(&query.id, Some(response_text))
                    .await;

                // Edit the original message
                if let Some(msg) = &query.message {
                    let msg_id = msg.id().0;
                    let original_text =
                        if let teloxide::types::MaybeInaccessibleMessage::Regular(m) = msg {
                            m.text().unwrap_or("").to_string()
                        } else {
                            String::new()
                        };
                    let _ = self
                        .bot
                        .edit_message_text(
                            msg_id,
                            &format!("{}\n\nDecision: {}", original_text, response_text),
                            None,
                        )
                        .await;
                }
            }

            "tooldetails" => {
                let cache = self.tool_input_cache.read().await;
                if let Some(cached) = cache.get(id) {
                    let details = formatting::format_tool_details(&cached.tool, &cached.input);

                    // Reply with details
                    if let Some(msg) = &query.message {
                        let thread_id =
                            if let teloxide::types::MaybeInaccessibleMessage::Regular(m) = msg {
                                m.thread_id.map(|t| t.0 .0)
                            } else {
                                None
                            };
                        let _ = self
                            .bot
                            .send_message(&details, &SendOptions::default(), thread_id)
                            .await;
                    }

                    let _ = self.bot.answer_callback_query(&query.id, None).await;
                } else {
                    let _ = self
                        .bot
                        .answer_callback_query(&query.id, Some("Details expired (5 min cache)"))
                        .await;
                }
            }

            _ => {
                tracing::debug!(%action, "Unknown callback action");
            }
        }
    }

    // ============ Command Handling ============

    async fn handle_command(&self, msg: &BridgeMessage) -> Result<()> {
        let cmd = msg.content.trim().to_lowercase();
        let new_state = match cmd.as_str() {
            "toggle" => !self.mirroring_enabled.load(Ordering::Relaxed),
            "enable" | "on" => true,
            "disable" | "off" => false,
            _ => {
                tracing::debug!(cmd = %cmd, "Unknown command");
                return Ok(());
            }
        };

        self.mirroring_enabled.store(new_state, Ordering::Relaxed);
        crate::config::write_mirror_status(
            &self.config.config_dir,
            new_state,
            Some(std::process::id()),
        );

        let status_text = if new_state {
            "\u{1f7e2} *Telegram mirroring: ON*"
        } else {
            "\u{1f534} *Telegram mirroring: OFF*"
        };

        tracing::info!(enabled = new_state, "Mirroring toggled");

        // Send one confirmation message to Telegram (even when disabling)
        let _ = self
            .bot
            .send_message(status_text, &SendOptions::default(), None)
            .await;

        Ok(())
    }

    // ============ Helper Methods ============

    async fn get_session_thread_id(&self, session_id: &str) -> Option<i32> {
        // Check in-memory cache
        if let Some(tid) = self.session_threads.read().await.get(session_id) {
            return Some(*tid);
        }

        // Fallback to database
        let sessions = self.sessions.lock().await;
        if let Some(tid) = sessions.get_session_thread(session_id) {
            drop(sessions); // Release lock before write
            self.session_threads
                .write()
                .await
                .insert(session_id.to_string(), tid as i32);
            return Some(tid as i32);
        }

        None
    }

    async fn ensure_session_exists(&self, msg: &BridgeMessage) -> Result<()> {
        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get_session(&msg.session_id)
        };

        if let Some(session) = session {
            // Reactivate ended sessions if we're still receiving events
            if session.status != crate::types::SessionStatus::Active {
                let sessions = self.sessions.lock().await;
                sessions.reactivate_session(&msg.session_id);

                // Cancel pending topic deletion
                if let Some(handle) = self.pending_deletions.write().await.remove(&msg.session_id) {
                    handle.abort();
                }
            }

            // Check if topic was deleted
            let thread_id = self.get_session_thread_id(&msg.session_id).await;
            if thread_id.is_none() && self.config.use_threads {
                self.handle_session_start(msg).await?;
            }
            return Ok(());
        }

        // Create session on-the-fly
        tracing::info!(session_id = %msg.session_id, "Creating session on-the-fly");
        self.handle_session_start(msg).await?;
        Ok(())
    }

    async fn check_and_update_tmux_target(&self, msg: &BridgeMessage) {
        let new_target = msg.get_metadata_str("tmuxTarget");
        let new_socket = msg.get_metadata_str("tmuxSocket");

        let new_target = match new_target {
            Some(t) => t.to_string(),
            None => return,
        };

        let current = self
            .session_tmux_targets
            .read()
            .await
            .get(&msg.session_id)
            .cloned();

        if current.as_deref() == Some(&new_target) {
            return;
        }

        tracing::info!(
            session_id = %msg.session_id,
            old = ?current,
            new = %new_target,
            "Tmux target changed, auto-updating"
        );

        self.session_tmux_targets
            .write()
            .await
            .insert(msg.session_id.clone(), new_target.clone());

        let sessions = self.sessions.lock().await;
        sessions.set_tmux_info(&msg.session_id, Some(&new_target), new_socket);
    }

    async fn cleanup_stale_sessions(&self) {
        let sessions = self.sessions.lock().await;
        sessions.expire_old_approvals();

        // Get sessions idle for >1h
        let candidates = sessions.get_stale_session_candidates(1);
        drop(sessions);

        for session in candidates {
            if let Some(target) = &session.tmux_target {
                // Has tmux target — check if pane is still alive
                let pane_alive =
                    InputInjector::is_pane_alive(target, session.tmux_socket.as_deref());
                if !pane_alive {
                    tracing::info!(session_id = %session.id, target, "Cleaning up stale session (pane dead)");
                    self.cleanup_stale_session(&session, "tmux pane no longer exists")
                        .await;
                }
                // Pane alive = keep session, it may just be idle
            } else {
                // No tmux info at all — clean up after 1h of inactivity
                tracing::info!(session_id = %session.id, "Cleaning up stale session (no tmux, >1h idle)");
                self.cleanup_stale_session(&session, "inactivity timeout (no tmux)")
                    .await;
            }
        }
    }

    async fn cleanup_stale_session(&self, session: &crate::types::Session, reason: &str) {
        let thread_id = self.get_session_thread_id(&session.id).await;

        if let Some(tid) = thread_id {
            let _ = self
                .bot
                .send_message(
                    &format!("\u{1f50c} *Session cleaned up*\n\n_{}_", reason),
                    &SendOptions::default(),
                    Some(tid),
                )
                .await;

            // Stale sessions are truly dead — always delete the topic to keep the group clean.
            // Normal session ends respect auto_delete_topics config; stale cleanup does not.
            if self.bot.delete_forum_topic(tid).await.unwrap_or(false) {
                tracing::info!(session_id = %session.id, %tid, "Deleted stale forum topic");
                let sessions = self.sessions.lock().await;
                sessions.clear_thread_id(&session.id);
            } else {
                // Fallback to close if delete fails (missing permissions)
                let _ = self.bot.close_forum_topic(tid).await;
            }
        }

        self.session_threads.write().await.remove(&session.id);
        self.session_tmux_targets.write().await.remove(&session.id);

        let sessions = self.sessions.lock().await;
        sessions.end_session(&session.id, SessionStatus::Ended);
    }
}

/// Format initial topic name for a session (before first prompt is known).
/// Shows project name; will be renamed to include task once the first prompt arrives.
fn format_topic_name(
    _session_id: &str,
    hostname: Option<&str>,
    project_dir: Option<&str>,
) -> String {
    let project = project_dir
        .and_then(|d| d.rsplit('/').next())
        .unwrap_or("Claude");

    match hostname {
        Some(host) => format!("{} ({})", project, host),
        None => project.to_string(),
    }
}

/// Build a short task description from the user's first prompt.
/// Extracts the first meaningful sentence/fragment, capped at 40 chars.
fn build_task_description(prompt: &str) -> String {
    let cleaned = prompt.trim();

    // Take the first line
    let first_line = cleaned.lines().next().unwrap_or(cleaned);

    // Remove common prefixes
    let trimmed = first_line
        .trim_start_matches("please ")
        .trim_start_matches("Please ")
        .trim_start_matches("can you ")
        .trim_start_matches("Can you ")
        .trim_start_matches("could you ")
        .trim_start_matches("Could you ")
        .trim_start_matches("I need to ")
        .trim_start_matches("I want to ")
        .trim_start_matches("Let's ")
        .trim_start_matches("let's ");

    // Capitalize first letter
    let mut chars = trimmed.chars();
    let capitalized = match chars.next() {
        Some(c) => {
            let upper: String = c.to_uppercase().collect();
            format!("{}{}", upper, chars.collect::<String>())
        }
        None => "New session".to_string(),
    };

    // Truncate at 40 chars on a word boundary
    if capitalized.chars().count() <= 40 {
        capitalized
    } else {
        let mut end = 40;
        // Find last space before the limit
        for (i, c) in capitalized.char_indices() {
            if i > 40 {
                break;
            }
            if c == ' ' {
                end = i;
            }
        }
        let boundary = capitalized
            .char_indices()
            .nth(end)
            .map(|(i, _)| i)
            .unwrap_or(capitalized.len());
        format!("{}...", &capitalized[..boundary])
    }
}

/// Format bytes into human-readable size string
fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.0}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
