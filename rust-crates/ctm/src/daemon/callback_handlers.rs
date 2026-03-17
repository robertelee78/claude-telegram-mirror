//! Handlers for Telegram callback queries (button presses).

use super::*;

/// Handle callback queries (button presses).
///
/// H4.2: Each sub-handler answers the callback individually with appropriate
/// text and show_alert, rather than answering early with None. This allows
/// handlers to provide meaningful feedback (e.g. "Approved", "Details expired").
pub(super) async fn handle_callback_query(ctx: &HandlerContext, cb: &CallbackQuery) {
    let data = match &cb.data {
        Some(d) => d.as_str(),
        None => return,
    };

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
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in confirm_abort");
        return;
    }
    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some("Aborting..."), false)
        .await;
    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);
    let message_id = cb.message.as_ref().map(|m| m.message_id);

    // Mark session as aborted in DB
    let aborted = {
        let sid = session_id.to_string();
        ctx.db_op(move |sess| {
            sess.end_session(&sid, crate::types::SessionStatus::Aborted)
                .is_ok()
        })
        .await
    };

    if aborted {
        // Send Ctrl-C via tmux to interrupt the running process
        let tmux_target = ctx.session_tmux.read().await.get(session_id).cloned();
        if let Some(target) = tmux_target {
            let socket = {
                let sid = session_id.to_string();
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
                    ctx.bot.chat_id(),
                    mid,
                    &format!("\u{1F6D1} Session `{session_id}` aborted."),
                    Some("Markdown"),
                )
                .await;
        }
    } else if let Some(mid) = message_id {
        let _ = ctx
            .bot
            .edit_message(
                ctx.bot.chat_id(),
                mid,
                "\u{274C} Failed to abort session.",
                None,
            )
            .await;
    }
}

/// Handle /abort cancellation callback.
async fn handle_cancel_abort_callback(ctx: &HandlerContext, cb: &CallbackQuery) {
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in cancel_abort");
        return;
    }
    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some("Cancelled"), false)
        .await;
    if let Some(msg) = &cb.message {
        let _ = ctx
            .bot
            .edit_message(
                msg.chat.id,
                msg.message_id,
                "\u{2705} Abort cancelled.",
                None,
            )
            .await;
    }
}

/// Handle approval/reject/abort callback.
async fn handle_approval_callback(
    ctx: &HandlerContext,
    approval_id: &str,
    action: &str,
    cb: &CallbackQuery,
) {
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in approval");
        return;
    }
    let action_label = match action {
        "approve" => "Approved",
        "reject" => "Rejected",
        _ => "Aborted",
    };
    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some(action_label), false)
        .await;
    let aid = approval_id.to_string();
    let approval = ctx
        .db_op(move |sess| sess.get_approval(&aid).ok().flatten())
        .await;

    let approval = match approval {
        Some(a) => a,
        None => {
            tracing::warn!(approval_id, "Approval not found");
            return;
        }
    };

    {
        let aid = approval_id.to_string();
        let asid = approval.session_id.clone();
        let act = action.to_string();
        ctx.db_op(move |sess| {
            if act == "abort" {
                let _ = sess.end_session(&asid, crate::types::SessionStatus::Aborted);
                let _ = sess.resolve_approval(&aid, crate::types::ApprovalStatus::Rejected);
            } else {
                let status = if act == "approve" {
                    crate::types::ApprovalStatus::Approved
                } else {
                    crate::types::ApprovalStatus::Rejected
                };
                let _ = sess.resolve_approval(&aid, status);
            }
        })
        .await;
    }

    // ADR-006 C1 / S-2: Send `approval_response` only to the specific socket
    // client that originated the approval_request, preventing approval forgery
    // where a different hook client could intercept another session's decision.
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "approvalId".to_string(),
        serde_json::Value::String(approval_id.to_string()),
    );
    let response = BridgeMessage {
        msg_type: MessageType::ApprovalResponse,
        session_id: approval.session_id.clone(),
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        content: action.to_string(),
        metadata: Some(metadata),
    };

    // S-2: Look up the specific client that sent the approval_request.
    let originating_client = ctx
        .pending_approval_clients
        .write()
        .await
        .remove(approval_id);

    if let Some(client_id) = originating_client {
        // Send only to the originating client
        let json = match serde_json::to_string(&response) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!(error = %e, "Failed to serialise approval_response");
                return;
            }
        };
        let line = format!("{json}\n");
        let guard = ctx.socket_clients.lock().await;
        if let Some(writer) = guard.get(&client_id) {
            let mut w = writer.lock().await;
            let _ = w.write_all(line.as_bytes()).await;
            tracing::info!(
                approval_id,
                action,
                session_id = %approval.session_id,
                client_id,
                "Approval resolved and sent to originating client"
            );
        } else {
            // Client disconnected; fall back to broadcast so the hook can
            // still receive the response if it reconnected under a new client_id.
            drop(guard);
            broadcast_to_clients(&ctx.socket_clients, &response).await;
            tracing::info!(
                approval_id,
                action,
                session_id = %approval.session_id,
                "Approval resolved, originating client gone — broadcast as fallback"
            );
        }
    } else {
        // No client_id recorded (e.g. older message without metadata injection);
        // fall back to broadcast for compatibility.
        broadcast_to_clients(&ctx.socket_clients, &response).await;
        tracing::info!(
            approval_id,
            action,
            session_id = %approval.session_id,
            "Approval resolved and broadcast over socket (no originating client recorded)"
        );
    }

    // C2.2: Edit the original approval message to append the decision and remove keyboard.
    // Use plain text (parse_mode = None) because approval text may contain tool names
    // with underscores that break Markdown rendering.
    if let Some(msg) = &cb.message {
        let action_text = match action {
            "approve" => "\u{2705} Approved via Telegram",
            "reject" => "\u{274C} Rejected via Telegram",
            _ => "\u{1F6D1} Session Aborted via Telegram",
        };
        let original = msg.text.as_deref().unwrap_or("");
        let updated = format!("{original}\n\nDecision: {action_text}");
        // Attempt edit with decision appended; fall back to decision-only text on failure.
        if ctx
            .bot
            .edit_message(msg.chat.id, msg.message_id, &updated, None)
            .await
            .is_err()
        {
            let _ = ctx
                .bot
                .edit_message(
                    msg.chat.id,
                    msg.message_id,
                    &format!("Decision: {action_text}"),
                    None,
                )
                .await;
        }
    }
}

/// Handle tool details callback.
/// M4: Send details as a reply to the original message.
async fn handle_tool_details_callback(ctx: &HandlerContext, tool_use_id: &str, cb: &CallbackQuery) {
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in tool_details");
        return;
    }
    let cached = {
        let cache = ctx.tool_cache.read().await;
        cache
            .get(tool_use_id)
            .map(|c| (c.tool.clone(), c.input.clone()))
    };

    match cached {
        Some((tool, input)) => {
            let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
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
                .answer_callback_query(&cb.id, Some("Details expired (5 min cache)"), true)
                .await;
        }
    }
}

/// Handle single-select answer callback.
async fn handle_answer_callback(ctx: &HandlerContext, data: &str, cb: &CallbackQuery) {
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in answer");
        return;
    }
    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some("Selected"), false)
        .await;
    // Format: answer:{shortSessionId}:{questionIndex}:{optionIndex}
    let parts: Vec<&str> = data.splitn(4, ':').collect();
    if parts.len() != 4 {
        return;
    }
    let short_key = parts[1];
    let q_idx: usize = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => return,
    };
    let o_idx: usize = match parts[3].parse() {
        Ok(v) => v,
        Err(_) => return,
    };

    let mut pq = ctx.pending_q.write().await;
    // H6.1: Resolve short callback key to full session_id key.
    let full_key = match resolve_pending_key(&pq, short_key) {
        Some(k) => k.clone(),
        None => return,
    };
    let pending = match pq.get_mut(&full_key) {
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
        let sid = session_id.clone();
        let socket = ctx
            .db_op(move |sess| {
                sess.get_session(&sid)
                    .ok()
                    .flatten()
                    .and_then(|s| s.tmux_socket)
            })
            .await;

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

    // Clean up if all answered, and auto-submit the review screen
    if pending.answered.iter().all(|a| *a) {
        let all_session_id = pending.session_id.clone();
        drop(pq);
        ctx.pending_q.write().await.remove(&full_key);

        // After all individual answers are injected, Claude Code shows a
        // "Review your answers" confirmation with "1. Submit answers".
        // Auto-inject "1" after a short delay so the user doesn't have to
        // switch back to the console just to confirm.
        auto_submit_answers(ctx, &all_session_id).await;
    }
}

/// Handle multi-select toggle callback.
async fn handle_toggle_callback(ctx: &HandlerContext, data: &str, cb: &CallbackQuery) {
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in toggle");
        return;
    }
    let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
    let parts: Vec<&str> = data.splitn(4, ':').collect();
    if parts.len() != 4 {
        return;
    }
    let short_key = parts[1];
    let q_idx: usize = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => return,
    };
    let o_idx: usize = match parts[3].parse() {
        Ok(v) => v,
        Err(_) => return,
    };

    let mut pq = ctx.pending_q.write().await;
    // H6.1: Resolve short callback key to full session_id key.
    let full_key = match resolve_pending_key(&pq, short_key) {
        Some(k) => k.clone(),
        None => return,
    };
    let pending = match pq.get_mut(&full_key) {
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
                    callback_data: format!("toggle:{short_key}:{q_idx}:{idx}"),
                }
            })
            .collect();
        buttons.push(InlineButton {
            text: "\u{2705} Submit".into(),
            callback_data: format!("submit:{short_key}:{q_idx}"),
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
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in submit");
        return;
    }
    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some("Submitted"), false)
        .await;
    let parts: Vec<&str> = data.splitn(3, ':').collect();
    if parts.len() != 3 {
        return;
    }
    let short_key = parts[1];
    let q_idx: usize = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => return,
    };

    let mut pq = ctx.pending_q.write().await;
    // H6.1: Resolve short callback key to full session_id key.
    let full_key = match resolve_pending_key(&pq, short_key) {
        Some(k) => k.clone(),
        None => return,
    };
    let pending = match pq.get_mut(&full_key) {
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
        let sid = session_id.clone();
        let socket = ctx
            .db_op(move |sess| {
                sess.get_session(&sid)
                    .ok()
                    .flatten()
                    .and_then(|s| s.tmux_socket)
            })
            .await;

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
        let all_session_id = pending.session_id.clone();
        drop(pq);
        ctx.pending_q.write().await.remove(&full_key);

        // Auto-submit the review screen (same as handle_answer_callback).
        auto_submit_answers(ctx, &all_session_id).await;
    }
}

/// After all AskUserQuestion answers are collected, Claude Code shows a
/// "Review your answers" confirmation screen with numbered options:
///   > 1. Submit answers
///     2. Cancel
///
/// This helper waits briefly for Claude Code to render the review screen,
/// then injects "1" to auto-select "Submit answers" so the user doesn't
/// have to switch back to the console.
pub(super) async fn auto_submit_answers(ctx: &HandlerContext, session_id: &str) {
    // Brief delay for Claude Code to transition from the last question
    // to the review screen. Without this, the "1" arrives before the
    // review prompt is displayed and gets swallowed.
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let tmux_target = ctx.session_tmux.read().await.get(session_id).cloned();
    if let Some(target) = tmux_target {
        let sid = session_id.to_string();
        let socket = ctx
            .db_op(move |sess| {
                sess.get_session(&sid)
                    .ok()
                    .flatten()
                    .and_then(|s| s.tmux_socket)
            })
            .await;

        let mut inj = ctx.injector.lock().await;
        inj.set_target(&target, socket.as_deref());
        let _ = inj.inject("1");
        tracing::info!(session_id, "Auto-submitted AskUserQuestion review screen");
    }
}
