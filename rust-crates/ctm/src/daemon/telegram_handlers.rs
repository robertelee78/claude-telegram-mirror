//! Handlers for incoming Telegram messages (text, photos, documents, bot commands).

use super::*;

/// Handle an incoming Telegram update (message or callback).
pub(super) async fn handle_telegram_update(ctx: HandlerContext, update: Update) {
    // Security: Check chat_id on ALL updates.
    // INTENTIONAL (ADR-006 L4.6): Do not reply to unauthorized chats. Replying
    // would confirm the bot's existence and function to an attacker. Silent drop
    // is more secure than the TS behavior of replying "Unauthorized."
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
        callback_handlers::handle_callback_query(&ctx, cb).await;
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
    // C2.6: Messages without a thread_id arrive in the General topic.
    // Instead of silently dropping them, send a helpful guide.
    let thread_id = match msg.message_thread_id {
        Some(tid) => tid,
        None => {
            ctx.bot
                .send_message(
                    "Send messages in a session topic, not the General chat.\n\n\
                     Each Claude Code session gets its own topic. Use /sessions to list them.",
                    None,
                    None,
                )
                .await;
            return;
        }
    };

    // Only process messages for sessions we own
    let session = ctx
        .db_op(move |sess| sess.get_session_by_thread_id(thread_id).ok().flatten())
        .await;

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

    // FR32: Cap text injection length to prevent oversized tmux payloads
    let inject_text: std::borrow::Cow<'_, str> = if text.chars().count() > MAX_INJECT_CHARS {
        tracing::warn!(
            chars = text.chars().count(),
            max = MAX_INJECT_CHARS,
            "Telegram text truncated before injection"
        );
        ctx.bot
            .send_message(
                &format!("Message truncated to {MAX_INJECT_CHARS} characters"),
                None,
                Some(thread_id),
            )
            .await;
        std::borrow::Cow::Owned(truncate(text, MAX_INJECT_CHARS))
    } else {
        std::borrow::Cow::Borrowed(text)
    };

    // Inject into CLI via tmux
    let injected = {
        let inj = ctx.injector.lock().await;
        inj.inject(&inject_text).unwrap_or(false)
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
        .db_op(move |sess| sess.get_session_by_thread_id(thread_id).ok().flatten())
        .await
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

    let downloads_dir = files::ensure_downloads_dir();
    let filename = files::sanitize_filename(&format!("photo_{}.jpg", photo.file_unique_id));
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
            // M3.1: Update lastActivity for photo messages to prevent stale cleanup
            let sid = session.id.clone();
            ctx.db_op(move |sess| {
                let _ = sess.update_activity(&sid);
            })
            .await;
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
        .db_op(move |sess| sess.get_session_by_thread_id(thread_id).ok().flatten())
        .await
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

    let original_name: String = match doc.file_name.as_deref() {
        Some(name) => name.to_string(),
        None => doc
            .mime_type
            .as_deref()
            .and_then(|m| m.split('/').next_back())
            .map(|ext| format!("unnamed.{ext}"))
            .unwrap_or_else(|| "unnamed.bin".to_string()),
    };

    let filename = files::sanitize_filename(&original_name);
    let downloads_dir = files::ensure_downloads_dir();
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
            // M3.1: Update lastActivity for document messages to prevent stale cleanup
            let sid = session.id.clone();
            ctx.db_op(move |sess| {
                let _ = sess.update_activity(&sid);
            })
            .await;
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
            let (active, pending) = ctx.db_op(|sess| sess.get_stats().unwrap_or((0, 0))).await;
            // C2.1: Add per-user (per-thread) info from bot_sessions
            let key = msg.message_thread_id.unwrap_or(msg.chat.id);
            let (attached_id, muted) = {
                let bs = ctx.bot_sessions.read().await;
                if let Some(state) = bs.get(&key) {
                    (state.attached_session_id.clone(), state.muted)
                } else {
                    (None, false)
                }
            };
            let mut status_text = format!(
                "\u{1F4CA} *Status*\n\n\
                 Active sessions: {active}\n\
                 Pending approvals: {pending}"
            );
            if let Some(sid) = &attached_id {
                status_text.push_str(&format!("\nAttached to: `{sid}`"));
            } else {
                status_text.push_str("\nAttached to: none");
            }
            status_text.push_str(&format!("\nMuted: {}", if muted { "yes" } else { "no" }));
            let mirror_on = ctx
                .mirroring_enabled
                .load(std::sync::atomic::Ordering::Relaxed);
            status_text.push_str(&format!(
                "\nMirroring: {}",
                if mirror_on { "ON" } else { "OFF" }
            ));
            ctx.bot
                .send_message(&status_text, Some(&opts), msg.message_thread_id)
                .await;
        }
        "/sessions" => {
            let sessions = ctx
                .db_op(|sess| sess.get_active_sessions().unwrap_or_default())
                .await;

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
                            ctx.bot.chat_id(),
                            sent.message_id,
                            &format!("\u{1F3D3} Pong! _{}ms_", latency),
                            Some("Markdown"),
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
                .db_op(move |sess| sess.get_session_by_thread_id(thread_id).ok().flatten())
                .await;

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
                .db_op(|sess| sess.get_active_sessions().unwrap_or_default())
                .await;
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
                // C3.2: Reset muted on attach so the user always receives
                // updates from the newly-attached session.
                state.muted = false;
                state.last_activity = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
            }
            ctx.bot
                .send_message(
                    &format!("\u{2705} Attached to session `{matched_id}`\nYou will now receive updates from this session.\nReply with text to send input."),
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
                            &format!("\u{1F50C} Detached from session `{sid}`\nYou will no longer receive updates."),
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
            // C3.4: Immediate abort — no confirmation dialog.
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
                    // Mark session as aborted in DB
                    let aborted = {
                        let sid = session_id.clone();
                        ctx.db_op(move |sess| {
                            sess.end_session(&sid, crate::types::SessionStatus::Aborted)
                                .is_ok()
                        })
                        .await
                    };

                    if aborted {
                        // Send Escape key via tmux to gracefully interrupt
                        let tmux_target = ctx.session_tmux.read().await.get(&session_id).cloned();
                        if let Some(target) = tmux_target {
                            let socket = {
                                let sid = session_id.clone();
                                ctx.db_op(move |sess| {
                                    sess.get_session(&sid)
                                        .ok()
                                        .flatten()
                                        .and_then(|s| s.tmux_socket)
                                })
                                .await
                            };
                            let mut inj = ctx.injector.lock().await;
                            inj.set_target(&target, socket.as_deref());
                            let _ = inj.send_key("Escape");
                        }

                        // Broadcast abort command to socket clients (matches TS behaviour)
                        broadcast_to_clients(
                            &ctx.socket_clients,
                            &BridgeMessage {
                                msg_type: MessageType::Command,
                                session_id: session_id.clone(),
                                timestamp: chrono::Utc::now().to_rfc3339(),
                                content: "abort".into(),
                                metadata: None,
                            },
                        )
                        .await;

                        // Clear attached session state
                        {
                            let mut bs = ctx.bot_sessions.write().await;
                            if let Some(state) = bs.get_mut(&key) {
                                state.attached_session_id = None;
                            }
                        }
                        ctx.session_tmux.write().await.remove(&session_id);

                        ctx.bot
                            .send_message(
                                &format!("\u{1F6D1} Session `{session_id}` aborted."),
                                Some(&opts),
                                thread_id,
                            )
                            .await;
                    } else {
                        ctx.bot
                            .send_message("\u{274C} Failed to abort session.", None, thread_id)
                            .await;
                    }
                }
            }
        }
        "/toggle" => {
            let new_state = if args == "on" {
                true
            } else if args == "off" {
                false
            } else {
                !ctx.mirroring_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
            };
            ctx.mirroring_enabled
                .store(new_state, std::sync::atomic::Ordering::Relaxed);
            crate::config::write_mirror_status(
                &ctx.config.config_dir,
                new_state,
                Some(std::process::id()),
            );
            let status_text = if new_state {
                "\u{1F7E2} *Telegram mirroring: ON*"
            } else {
                "\u{1F534} *Telegram mirroring: OFF*"
            };
            tracing::info!(enabled = new_state, "Mirroring toggled via Telegram");
            ctx.bot
                .send_message(status_text, Some(&opts), msg.message_thread_id)
                .await;
        }
        _ => {
            // Unknown command — ignore silently
        }
    }
}

/// Handle free-text answer to pending AskUserQuestion.
pub(super) async fn handle_free_text_answer(
    ctx: &HandlerContext,
    session_id: &str,
    text: &str,
) -> bool {
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
        let sid2 = sid.clone();
        let socket = ctx
            .db_op(move |sess| {
                sess.get_session(&sid2)
                    .ok()
                    .flatten()
                    .and_then(|s| s.tmux_socket)
            })
            .await;

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
