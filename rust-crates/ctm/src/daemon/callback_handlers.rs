//! Handlers for Telegram callback queries (button presses).

use super::*;

/// ADR-014 E3: How long to wait after releasing a blocked AskUserQuestion hook
/// (bare `allow`) before injecting free-text keystrokes, to let Claude render its
/// interactive TUI. Inherently racy; used only on the free-text fallback path.
const QUESTION_TUI_RENDER_WAIT_MS: u64 = 1500;

/// Collected answer for tmux injection, preserving type information
/// so multi-select can be injected as key sequences instead of text.
enum CollectedAnswer {
    /// Single-select: the chosen option's label. Structured delivery (ADR-014 E1)
    /// uses the label directly; the keystroke fallback injects it as literal text.
    Option(String),
    /// Multi-select: the chosen option labels (for the structured answers map,
    /// comma-joined) plus the indices/total needed by the keystroke fallback.
    MultiSelect {
        labels: Vec<String>,
        selected_indices: Vec<usize>,
        total_options: usize,
    },
    /// Free-text typed by the user. ADR-014 E3: there is no structured path for
    /// free-text, so its presence forces the whole answer set onto the isolated
    /// keystroke path.
    FreeText(String),
}

/// ADR-014 E1/E3: which delivery path a "Submit All" tap takes. Pure decision so it
/// is unit-testable (the review flagged the edge cases this encodes).
#[derive(Debug, PartialEq, Eq)]
enum SubmitPath {
    /// All answers are option-based AND a blocked hook client is registered:
    /// deliver structurally via updatedInput, no keystrokes.
    Structured,
    /// At least one free-text answer AND a blocked hook client: release the hook to
    /// its TUI and inject via the isolated keystroke path.
    FreeTextRelease,
    /// No blocked hook client recorded (its `_client_id` was missing or already
    /// consumed): cannot route or safely inject — surface to the user instead.
    NoClient,
}

fn classify_submit(has_free_text: bool, has_client: bool) -> SubmitPath {
    match (has_free_text, has_client) {
        (_, false) => SubmitPath::NoClient,
        (false, true) => SubmitPath::Structured,
        (true, true) => SubmitPath::FreeTextRelease,
    }
}

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
    // ADR-013: Sub-agent details callback
    else if let Some(agent_id) = data.strip_prefix("subagentdetails:") {
        handle_subagent_details_callback(ctx, agent_id, cb).await;
    }
    // AskUserQuestion callbacks
    else if data.starts_with("submitall:") {
        handle_submitall_callback(ctx, data, cb).await;
    } else if data.starts_with("change:") {
        handle_change_callback(ctx, data, cb).await;
    } else if data.starts_with("answer:") {
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
    let aid = approval_id.to_string();
    let approval = ctx
        .db_op(move |sess| sess.get_approval(&aid).ok().flatten())
        .await;

    // ADR-014 B5: tolerate stale/unknown approval IDs gracefully — daemon restarted
    // (in-memory map lost), expired, or already handled. Never crash/block/mis-route:
    // alert the user and mark the message stale instead of silently returning.
    let approval = match approval {
        Some(a) => a,
        None => {
            tracing::info!(
                approval_id,
                "ADR-014 B5: callback for unknown/expired approval"
            );
            let _ = ctx
                .bot
                .answer_callback_query(
                    &cb.id,
                    Some("This request expired or was already handled."),
                    true,
                )
                .await;
            if let Some(msg) = &cb.message {
                let _ = ctx
                    .bot
                    .edit_message_text_no_markup(
                        msg.message_id,
                        "\u{231B} This approval expired or was already handled.",
                    )
                    .await;
            }
            return;
        }
    };

    // ADR-014 B2: only emit the ApprovalResponse when THIS tap actually transitioned
    // the pending row. resolve_approval returns false if it was already resolved (a
    // double-tap or a daemon-restart race), which prevents sending two responses to
    // the hook. For abort, end the session only when the transition actually happened.
    let changed = {
        let aid = approval_id.to_string();
        let asid = approval.session_id.clone();
        let act = action.to_string();
        ctx.db_op(move |sess| {
            if act == "abort" {
                let c = sess
                    .resolve_approval(&aid, crate::types::ApprovalStatus::Rejected)
                    .unwrap_or(false);
                if c {
                    let _ = sess.end_session(&asid, crate::types::SessionStatus::Aborted);
                }
                c
            } else {
                let status = if act == "approve" {
                    crate::types::ApprovalStatus::Approved
                } else {
                    crate::types::ApprovalStatus::Rejected
                };
                sess.resolve_approval(&aid, status).unwrap_or(false)
            }
        })
        .await
    };

    if !changed {
        // ADR-014 B2/B5: a double-tap or already-handled request. Acknowledge without
        // sending a second response or re-editing the (already resolved) message.
        tracing::info!(
            approval_id,
            action,
            "ADR-014 B2: approval already resolved, ignoring duplicate tap"
        );
        let _ = ctx
            .bot
            .answer_callback_query(&cb.id, Some("Already handled."), true)
            .await;
        return;
    }

    // The decision took effect — acknowledge it to the tapping user.
    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some(action_label), false)
        .await;

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

    // ADR-014 B3: Edit the original approval message to a static resolved line
    // showing the decision AND the time (e.g. "✅ Approved · 14:03"), and REMOVE the
    // inline keyboard. This structurally prevents re-taps (no buttons remain) and
    // turns the topic into a readable audit trail. edit_message_text_no_markup both
    // sets the text and drops the keyboard in one call. Plain text (no Markdown):
    // tool names with underscores would otherwise break rendering.
    if let Some(msg) = &cb.message {
        let action_text = match action {
            "approve" => "\u{2705} Approved",
            "reject" => "\u{274C} Rejected",
            _ => "\u{1F6D1} Aborted",
        };
        let time = chrono::Local::now().format("%H:%M");
        let resolved = format!("{action_text} \u{00B7} {time}");
        let _ = ctx
            .bot
            .edit_message_text_no_markup(msg.message_id, &resolved)
            .await;
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

/// Handle sub-agent details callback (ADR-013 Part C, D7).
///
/// When a user taps the "Details" button on a sub-agent completion one-liner,
/// this handler reads the sub-agent's full output from a temp file
/// (`/tmp/ctm-subagent-{agent_id}.md`) and sends:
/// 1. A reply message with a summary (first ~500 chars)
/// 2. The full output as a `.md` file attachment via `send_document`
async fn handle_subagent_details_callback(
    ctx: &HandlerContext,
    agent_id: &str,
    cb: &CallbackQuery,
) {
    // ADR-013 GAP-1: Validate agent_id to prevent path traversal.
    // agent_id comes from user-controlled callback_data and is used in file paths.
    if !crate::types::is_valid_agent_id(agent_id) {
        tracing::warn!(
            agent_id,
            "ADR-013 GAP-1: Rejected invalid agent_id in subagent details callback (path traversal prevention)"
        );
        let _ = ctx
            .bot
            .answer_callback_query(&cb.id, Some("Invalid agent ID"), true)
            .await;
        return;
    }

    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in subagent_details");
        return;
    }

    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);
    let reply_to = cb.message.as_ref().map(|m| m.message_id);

    // Read the sub-agent output from the temp file written by Agent #3 (daemon-core).
    let temp_path = std::path::PathBuf::from(format!("/tmp/ctm-subagent-{agent_id}.md"));
    let content = match std::fs::read_to_string(&temp_path) {
        Ok(c) if !c.is_empty() => c,
        Ok(_) => {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Sub-agent output is empty"), true)
                .await;
            return;
        }
        Err(e) => {
            tracing::warn!(
                agent_id,
                error = %e,
                "ADR-013 D7: Sub-agent temp file not found or unreadable"
            );
            let _ = ctx
                .bot
                .answer_callback_query(
                    &cb.id,
                    Some("Details not available (output expired or not yet written)"),
                    true,
                )
                .await;
            return;
        }
    };

    let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;

    // 1. Send a summary reply (first ~500 chars of the output).
    let summary = if content.chars().count() > 500 {
        let truncated: String = content.chars().take(500).collect();
        format!("{truncated}\u{2026}") // ellipsis
    } else {
        content.clone()
    };

    let summary_text = format!(
        "\u{1F916} *Sub-agent output* (`{}`)\n\n{}",
        escape_markdown_v1(agent_id),
        escape_markdown_v1(&summary)
    );

    if let Some(reply_to_id) = reply_to {
        let _ = ctx
            .bot
            .send_message_reply_to(
                &summary_text,
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
                &summary_text,
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                thread_id,
            )
            .await;
    }

    // 2. Send the full output as a .md file attachment via send_document.
    if let Err(e) = ctx
        .bot
        .send_document(
            &temp_path,
            Some(&format!("Full output for sub-agent {agent_id}")),
            thread_id,
        )
        .await
    {
        tracing::warn!(
            agent_id,
            error = %e,
            "ADR-013 D7: Failed to send sub-agent output as document"
        );
        // Fall back to a text message indicating the file couldn't be sent.
        ctx.bot
            .send_message(
                &format!(
                    "_Could not send file attachment: {}_",
                    escape_markdown_v1(&e.to_string())
                ),
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                thread_id,
            )
            .await;
    }
}

/// Handle single-select answer callback.
///
/// ADR-012: Taps are now tentative — the selection is stored in
/// `pending.tentative` and the keyboard is re-rendered with a ✓ prefix on
/// the chosen option. Nothing is injected into tmux until "Submit All".
///
/// Per-key Mutex: The per-entry `Mutex<PendingQuestion>` is held across
/// state mutation AND API calls to prevent concurrent callbacks for the
/// same question set from racing on message edits.
async fn handle_answer_callback(ctx: &HandlerContext, data: &str, cb: &CallbackQuery) {
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in answer");
        return;
    }
    // Format: answer:{shortSessionId}:{questionIndex}:{optionIndex}
    let parts: Vec<&str> = data.splitn(4, ':').collect();
    if parts.len() != 4 {
        let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
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

    // Phase 1: Brief read lock to get the Arc<Mutex<PendingQuestion>>.
    let (full_key, entry) = {
        let pq = ctx.pending_q.read().await;
        let fk = match resolve_pending_key(&pq, short_key) {
            Some(k) => k,
            None => {
                let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
                return;
            }
        };
        match pq.get(&fk) {
            Some(arc) => (fk, Arc::clone(arc)),
            None => {
                let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
                return;
            }
        }
    }; // RwLock released.

    // Phase 2: Per-key mutex held across state mutation AND API calls.
    let mut pending = entry.lock().await;

    // If already finalized, reject the tap.
    if pending.finalized.get(q_idx) == Some(&true) {
        let _ = ctx
            .bot
            .answer_callback_query(&cb.id, Some("Already submitted"), false)
            .await;
        return;
    }

    let option_label = pending
        .questions
        .get(q_idx)
        .and_then(|q| q.options.get(o_idx))
        .map(|o| o.label.clone())
        .unwrap_or_else(|| format!("{}", o_idx + 1));

    // Store tentative selection.
    pending
        .tentative
        .insert(q_idx, TentativeAnswer::Option(o_idx));

    // Rebuild the question message text (PLAIN TEXT — see render_question_text) with
    // the current selection shown.
    let updated_text = {
        let q = &pending.questions[q_idx];
        let mut t = super::render_question_text(q);
        t.push_str(&format!("\n\n\u{2713} Selected: {option_label}"));
        t
    };

    // Rebuild inline keyboard with ✓ prefix on selected option.
    let new_buttons: Vec<InlineButton> = {
        let q = &pending.questions[q_idx];
        q.options
            .iter()
            .enumerate()
            .map(|(idx, opt)| {
                let label = if idx == o_idx {
                    format!("\u{2713} {}", opt.label)
                } else {
                    opt.label.clone()
                };
                InlineButton {
                    text: label,
                    callback_data: format!("answer:{short_key}:{q_idx}:{idx}"),
                }
            })
            .collect()
    };

    let msg_id = pending
        .question_message_ids
        .get(q_idx)
        .copied()
        .unwrap_or(0);
    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);
    let chat_id = ctx.config.chat_id;
    let all_answered = pending.tentative.len() == pending.questions.len();

    // Toast feedback (within per-key lock — no other callback for this
    // question set can interleave).
    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some(&format!("Selected: {option_label}")), false)
        .await;

    // Edit the question message in place (keep keyboard).
    if msg_id != 0 {
        if let Err(e) = ctx
            .bot
            .edit_message_text_with_markup(chat_id, msg_id, &updated_text, None, &[new_buttons])
            .await
        {
            tracing::warn!(
                session_id = %full_key,
                q_idx,
                error = %e,
                "Failed to edit question message after tentative selection"
            );
        }
    }

    // If all questions now have a tentative answer, send/update summary.
    if all_answered {
        // send_or_update_summary needs its own lock access, so release ours first.
        drop(pending);
        let _ = send_or_update_summary(ctx, &full_key, thread_id).await;
    }
    // Otherwise per-key Mutex drops here.
}

/// Handle multi-select toggle callback.
///
/// ADR-012: Toggles are stored via `TentativeAnswer::MultiOption`. The "Done"
/// button (`submit:`) captures the final multi-select state as tentative and
/// triggers the summary check.
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

    // Phase 1: Brief read lock to get the Arc<Mutex<PendingQuestion>>.
    let entry = {
        let pq = ctx.pending_q.read().await;
        let fk = match resolve_pending_key(&pq, short_key) {
            Some(k) => k,
            None => return,
        };
        match pq.get(&fk) {
            Some(arc) => Arc::clone(arc),
            None => return,
        }
    };

    // Phase 2: Per-key mutex held across state mutation AND API calls.
    let mut pending = entry.lock().await;

    if pending.finalized.get(q_idx) == Some(&true) {
        return;
    }

    // Toggle the option in the MultiOption set.
    let selected = match pending
        .tentative
        .entry(q_idx)
        .or_insert_with(|| TentativeAnswer::MultiOption(HashSet::new()))
    {
        TentativeAnswer::MultiOption(set) => {
            if set.contains(&o_idx) {
                set.remove(&o_idx);
            } else {
                set.insert(o_idx);
            }
            set.clone()
        }
        // If somehow the tentative answer is not MultiOption, replace it.
        other => {
            *other = TentativeAnswer::MultiOption({
                let mut s = HashSet::new();
                s.insert(o_idx);
                s
            });
            if let TentativeAnswer::MultiOption(s) = other {
                s.clone()
            } else {
                HashSet::new()
            }
        }
    };

    // M2: Re-render keyboard with checkmarks (within per-key lock).
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
            text: "\u{2705} Done".into(),
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

/// Handle multi-select "Done" callback.
///
/// ADR-012: The "Done" button no longer immediately injects answers. It marks
/// the multi-select question as having a tentative `MultiOption` answer
/// (the set accumulated via toggles) and triggers the summary check.
async fn handle_submit_callback(ctx: &HandlerContext, data: &str, cb: &CallbackQuery) {
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in submit");
        return;
    }
    let parts: Vec<&str> = data.splitn(3, ':').collect();
    if parts.len() != 3 {
        let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
        return;
    }
    let short_key = parts[1];
    let q_idx: usize = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => {
            let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
            return;
        }
    };

    // Phase 1: Brief read lock to get the Arc<Mutex<PendingQuestion>>.
    let (full_key, entry) = {
        let pq = ctx.pending_q.read().await;
        let fk = match resolve_pending_key(&pq, short_key) {
            Some(k) => k,
            None => {
                let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
                return;
            }
        };
        match pq.get(&fk) {
            Some(arc) => (fk, Arc::clone(arc)),
            None => {
                let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
                return;
            }
        }
    };

    // Phase 2: Per-key mutex held across state mutation AND API calls.
    let mut pending = entry.lock().await;

    if pending.finalized.get(q_idx) == Some(&true) {
        let _ = ctx
            .bot
            .answer_callback_query(&cb.id, Some("Already submitted"), false)
            .await;
        return;
    }

    // Ensure we have a MultiOption entry (may already exist from toggles;
    // if the user tapped Done without toggling anything, insert an empty set).
    if !matches!(
        pending.tentative.get(&q_idx),
        Some(TentativeAnswer::MultiOption(_))
    ) {
        pending
            .tentative
            .insert(q_idx, TentativeAnswer::MultiOption(HashSet::new()));
    }

    // Build summary of selected labels for the toast.
    let toast_text = {
        if let Some(TentativeAnswer::MultiOption(set)) = pending.tentative.get(&q_idx) {
            let mut sorted: Vec<usize> = set.iter().copied().collect();
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
                "Done (none selected)".to_string()
            } else {
                format!("Done: {}", labels.join(", "))
            }
        } else {
            "Done".to_string()
        }
    };

    let all_answered = pending.tentative.len() == pending.questions.len();
    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);

    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some(&toast_text), false)
        .await;

    if all_answered {
        drop(pending);
        let _ = send_or_update_summary(ctx, &full_key, thread_id).await;
    }
}

/// Build and send (or edit) the summary confirmation message.
///
/// ADR-012 Phase 5: Called whenever all questions have a tentative answer.
/// Sends a summary showing all selections with "Submit All" and "Change QN"
/// buttons. If a summary was already sent, edits it in place.
///
/// Per-key Mutex: This function acquires its own per-key Mutex from the map.
/// Callers MUST drop their per-key Mutex guard before calling this to avoid
/// deadlock (the per-key Mutex is not re-entrant).
///
/// Returns the message_id of the summary, or `None` on failure.
pub(super) async fn send_or_update_summary(
    ctx: &HandlerContext,
    full_key: &str,
    thread_id: Option<i64>,
) -> Option<i64> {
    // Look up the per-key mutex.
    let entry = {
        let pq = ctx.pending_q.read().await;
        pq.get(full_key).map(Arc::clone)?
    };

    let mut pending = entry.lock().await;

    // Build the short key for callback_data (first 20 chars of session_id).
    let short = &pending.session_id[..std::cmp::min(20, pending.session_id.len())];

    // Build summary text.
    // PLAIN TEXT (sent with parse_mode None below) — answer labels are arbitrary
    // model content; Markdown here risked an HTTP 400 that would drop the Submit All
    // button and strand the blocked hook. See render_question_text.
    let mut summary_text = "\u{1F4CB} Review your answers:\n".to_string();
    for (q_idx, q) in pending.questions.iter().enumerate() {
        let answer_label = match pending.tentative.get(&q_idx) {
            Some(TentativeAnswer::Option(o_idx)) => pending
                .questions
                .get(q_idx)
                .and_then(|qq| qq.options.get(*o_idx))
                .map(|o| o.label.clone())
                .unwrap_or_else(|| format!("{}", o_idx + 1)),
            Some(TentativeAnswer::MultiOption(set)) => {
                let mut sorted: Vec<usize> = set.iter().copied().collect();
                sorted.sort();
                let labels: Vec<String> = sorted
                    .iter()
                    .filter_map(|&idx| {
                        pending
                            .questions
                            .get(q_idx)
                            .and_then(|qq| qq.options.get(idx))
                            .map(|o| o.label.clone())
                    })
                    .collect();
                if labels.is_empty() {
                    "(none)".to_string()
                } else {
                    labels.join(", ")
                }
            }
            Some(TentativeAnswer::FreeText(s)) => s.clone(),
            None => "(unanswered)".to_string(),
        };
        summary_text.push_str(&format!("\n{}. {}: {}", q_idx + 1, q.header, answer_label));
    }

    // Helper: build reply_markup keyboard.
    let build_keyboard = |short: &str, n: usize| -> serde_json::Value {
        let mut rows: Vec<serde_json::Value> = Vec::new();
        rows.push(serde_json::json!([{
            "text": "\u{2705} Submit All",
            "callback_data": format!("submitall:{short}"),
        }]));
        let mut change_row: Vec<serde_json::Value> = Vec::new();
        for q_idx in 0..n {
            change_row.push(serde_json::json!({
                "text": format!("Change Q{}", q_idx + 1),
                "callback_data": format!("change:{short}:{q_idx}"),
            }));
            if change_row.len() == 2 {
                rows.push(serde_json::Value::Array(change_row));
                change_row = Vec::new();
            }
        }
        if !change_row.is_empty() {
            rows.push(serde_json::Value::Array(change_row));
        }
        serde_json::json!({"inline_keyboard": rows})
    };

    let reply_markup = build_keyboard(short, pending.questions.len());
    let chat_id = ctx.config.chat_id;

    // Either edit the existing summary or send a new one.
    if let Some(mid) = pending.summary_message_id {
        match ctx
            .bot
            .edit_message_text_with_raw_markup(
                chat_id,
                mid,
                &summary_text,
                None,
                reply_markup.clone(),
            )
            .await
        {
            Ok(()) => {
                return Some(mid);
            }
            Err(e) => {
                tracing::warn!(
                    session_id = full_key,
                    error = %e,
                    "Failed to edit summary message; will send new one"
                );
                pending.summary_message_id = None;
            }
        }
    }

    // Send new summary message.
    match ctx
        .bot
        .send_message_with_raw_markup_returning(&summary_text, None, reply_markup, thread_id)
        .await
    {
        Ok(new_mid) => {
            pending.summary_message_id = Some(new_mid);
            Some(new_mid)
        }
        Err(e) => {
            tracing::warn!(session_id = full_key, error = %e, "Failed to send summary message");
            // ADR-014 (review follow-up, residual D5): the "Submit All" summary could
            // not be sent (e.g. a transient Telegram error), so the user has no button
            // to submit and the blocked hook would otherwise hang until timeout. Drop
            // the entry lock, tell the user, and release the hook to its terminal TUI.
            drop(pending);
            ctx.bot
                .send_message(
                    "\u{26A0}\u{FE0F} Couldn't show the Submit button (Telegram send failed). Please answer at the terminal.",
                    None,
                    thread_id,
                )
                .await;
            release_question_hook(ctx, full_key).await;
            None
        }
    }
}

/// Handle "Submit All" callback.
///
/// ADR-012 Phase 6: Locks all tentative answers, injects them into tmux in
/// question order, edits each question message and the summary to show
/// "Submitted", then auto-submits the Claude Code review screen.
/// ADR-014 E1: Build the JSON answers-map content for a structured QuestionResponse.
/// Keyed by question text; single-select uses the chosen label, multi-select uses
/// comma-joined labels (the spike-confirmed contract). Free-text entries are skipped
/// (they force the keystroke fallback, so this is only ever called with none present).
fn build_answers_map_content(answers: &[(usize, String, CollectedAnswer)]) -> String {
    let mut map = serde_json::Map::new();
    for (_, qtext, ans) in answers {
        let val = match ans {
            CollectedAnswer::Option(label) => label.clone(),
            // Multi-select: join labels with a bare comma and NO space. This is the
            // "Claude Code format" — verified against jsayubi/ccgram's production
            // question-notify.ts (`selectedLabels.join(',')`). A comma+space would
            // risk a leading space on every label after the first (" Go" != "Go") if
            // Claude Code splits on "," without trimming.
            CollectedAnswer::MultiSelect { labels, .. } => labels.join(","),
            CollectedAnswer::FreeText(_) => continue,
        };
        map.insert(qtext.clone(), serde_json::Value::String(val));
    }
    serde_json::Value::Object(map).to_string()
}

/// ADR-014 E1: Write a `QuestionResponse` back to the specific socket client whose
/// hook is blocked on this session's AskUserQuestion. Mirrors the targeted approval
/// response routing (S-2) — never broadcast, so one session's answer cannot be
/// delivered to another session's hook. `content` is the JSON answers map or the
/// free-text fallback sentinel.
/// Returns `true` only if the response was written to a still-connected client.
/// `false` means the client is gone (e.g. the hook already timed out and closed its
/// socket) or the write failed — the caller must then surface the answer rather than
/// silently dropping it (ADR-014 review: late-submit-after-timeout loss).
pub(super) async fn send_question_response(
    ctx: &HandlerContext,
    session_id: &str,
    content: &str,
    client_id: &str,
) -> bool {
    let response = BridgeMessage {
        msg_type: MessageType::QuestionResponse,
        session_id: session_id.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        content: content.to_string(),
        metadata: None,
    };
    let json = match serde_json::to_string(&response) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialise question_response");
            return false;
        }
    };
    let line = format!("{json}\n");

    // Clone the per-client writer Arc and DROP the socket_clients map lock before
    // the async write, so a slow write cannot hold the shared map lock and block
    // every other handler's outbound writes (ADR-014 review MED-1).
    let writer = {
        let guard = ctx.socket_clients.lock().await;
        guard.get(client_id).cloned()
    };
    let Some(writer) = writer else {
        tracing::warn!(
            session_id,
            client_id,
            "ADR-014 E1: originating client gone — hook already timed out / disconnected"
        );
        return false;
    };
    let mut w = writer.lock().await;
    match w.write_all(line.as_bytes()).await {
        Ok(()) => {
            tracing::info!(
                session_id,
                "ADR-014 E1: question_response sent to originating client"
            );
            true
        }
        Err(e) => {
            tracing::warn!(session_id, error = %e, "Failed to write question_response to client");
            false
        }
    }
}

/// ADR-014 (review follow-up): Release a blocked AskUserQuestion hook back to its
/// own terminal TUI when the daemon cannot render (or finish rendering) the widget.
///
/// After `handle_question_request` registers the originating socket client, EVERY
/// early-return / drop / error path in the render flow must call this — otherwise the
/// hook stays blocked on `send_and_wait` for the full 300s timeout while the user sees
/// nothing (silent ~5-minute freeze). Sending the free-text fallback sentinel makes the
/// hook emit a bare `allow` and fall back to its native TUI immediately. Removes the
/// pending-client mapping so a later "Submit All" can't try to route to a dead hook.
///
/// No-op (returns without effect) if no client was registered for this session.
pub(super) async fn release_question_hook(ctx: &HandlerContext, session_id: &str) {
    let client = ctx
        .pending_question_clients
        .write()
        .await
        .remove(session_id);
    if let Some(client_id) = client {
        let _ = send_question_response(
            ctx,
            session_id,
            crate::types::FREETEXT_FALLBACK_SENTINEL,
            &client_id,
        )
        .await;
        tracing::warn!(
            session_id,
            "ADR-014: released blocked AskUserQuestion hook to its TUI (widget unavailable)"
        );
    }
}

async fn handle_submitall_callback(ctx: &HandlerContext, data: &str, cb: &CallbackQuery) {
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in submitall");
        return;
    }
    // Format: submitall:{shortSessionId}
    let parts: Vec<&str> = data.splitn(2, ':').collect();
    if parts.len() != 2 {
        let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
        return;
    }
    let short_key = parts[1];

    // Phase 1: Brief read lock to get the Arc and full key.
    let (full_key, entry) = {
        let pq = ctx.pending_q.read().await;
        let fk = match resolve_pending_key(&pq, short_key) {
            Some(k) => k,
            None => {
                let _ = ctx
                    .bot
                    .answer_callback_query(&cb.id, Some("No pending question"), false)
                    .await;
                return;
            }
        };
        match pq.get(&fk) {
            Some(arc) => (fk, Arc::clone(arc)),
            None => {
                let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
                return;
            }
        }
    };

    // Phase 2: Per-key mutex for state extraction.
    let (answers, session_id, question_message_ids, summary_message_id) = {
        let mut pending = entry.lock().await;

        // Guard: if already all finalized, reject double-tap.
        if pending.finalized.iter().all(|f| *f) {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Already submitted"), false)
                .await;
            return;
        }

        // Guard: reject if any question is unanswered (can happen if
        // "Change QN" races with "Submit All").
        if pending.tentative.len() != pending.questions.len() {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Please answer all questions first"), true)
                .await;
            return;
        }

        // Collect all answers in question order, preserving type info so the
        // structured path (ADR-014 E1) can build the answers map by question text
        // and the keystroke fallback can still inject key sequences.
        let mut answers: Vec<(usize, String, CollectedAnswer)> = Vec::new();
        for (q_idx, q) in pending.questions.iter().enumerate() {
            let question_text = q.question.clone();
            let answer = match pending.tentative.get(&q_idx) {
                Some(TentativeAnswer::Option(o_idx)) => {
                    let label = q
                        .options
                        .get(*o_idx)
                        .map(|o| o.label.clone())
                        .unwrap_or_else(|| format!("{}", o_idx + 1));
                    CollectedAnswer::Option(label)
                }
                Some(TentativeAnswer::MultiOption(set)) => {
                    let total_options = q.options.len();
                    let mut sorted: Vec<usize> = set.iter().copied().collect();
                    sorted.sort();
                    let labels: Vec<String> = sorted
                        .iter()
                        .map(|&i| {
                            q.options
                                .get(i)
                                .map(|o| o.label.clone())
                                .unwrap_or_else(|| format!("{}", i + 1))
                        })
                        .collect();
                    CollectedAnswer::MultiSelect {
                        labels,
                        selected_indices: sorted,
                        total_options,
                    }
                }
                Some(TentativeAnswer::FreeText(s)) => CollectedAnswer::FreeText(s.clone()),
                None => continue,
            };
            answers.push((q_idx, question_text, answer));
        }

        // Mark all as finalized.
        for &(q_idx, _, _) in &answers {
            if let Some(f) = pending.finalized.get_mut(q_idx) {
                *f = true;
            }
        }

        // per-key Mutex drops here
        (
            answers,
            pending.session_id.clone(),
            pending.question_message_ids.clone(),
            pending.summary_message_id,
        )
    };

    // Remove entry from the map. No new handler can acquire it after this.
    {
        let mut pq = ctx.pending_q.write().await;
        pq.remove(&full_key);
    }

    let chat_id = ctx.config.chat_id;
    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);

    // ADR-014 E1/E2/E3: Decide structured delivery vs the isolated keystroke
    // fallback. A blocked hook is identified by an entry in pending_question_clients
    // (keyed by session_id). Structured delivery applies only when EVERY answer is
    // option-based — free-text has no structured contract (E3), so its presence
    // forces the whole set onto keystrokes.
    let has_free_text = answers
        .iter()
        .any(|(_, _, a)| matches!(a, CollectedAnswer::FreeText(_)));
    let originating_client = ctx
        .pending_question_clients
        .write()
        .await
        .remove(&session_id);

    match classify_submit(has_free_text, originating_client.is_some()) {
        SubmitPath::Structured => {
            // ---- Structured path (E1/E2): NO keystrokes, NO TUI. ----
            let client = originating_client
                .clone()
                .expect("classify_submit Structured implies a client is present");
            // Build the answers map: question text -> selected label (single) or
            // comma-joined labels (multi-select), per the spike-confirmed contract.
            let content = build_answers_map_content(&answers);
            let delivered = send_question_response(ctx, &session_id, &content, &client).await;
            if delivered {
                let _ = ctx
                    .bot
                    .answer_callback_query(&cb.id, Some("Submitted \u{2705}"), false)
                    .await;
                tracing::info!(
                    session_id = %session_id,
                    "ADR-014 E1: AskUserQuestion answered structurally via updatedInput (no keystrokes)"
                );
            } else {
                // Review fix (late-submit-after-timeout): the hook already timed out and
                // closed its socket, so the structured answer cannot be delivered. Do NOT
                // silently drop the user's answer — alert and tell them to answer at the
                // terminal (Claude has fallen back to its own TUI).
                let _ = ctx
                    .bot
                    .answer_callback_query(&cb.id, Some("This question already timed out."), true)
                    .await;
                ctx.bot
                .send_message(
                    "\u{26A0}\u{FE0F} This question timed out before your answer was submitted. If Claude is still waiting, please answer at the terminal.",
                    None,
                    thread_id,
                )
                .await;
                tracing::warn!(
                    session_id = %session_id,
                    "ADR-014: structured answer not delivered (hook gone) — user notified"
                );
            }
        }
        SubmitPath::FreeTextRelease => {
            // ---- Free-text fallback (E3): the SOLE remaining keystroke path. ----
            let client_id = originating_client
                .clone()
                .expect("classify_submit FreeTextRelease implies a client is present");
            // Release the blocked hook with a bare `allow` (no updatedInput) so Claude
            // renders its interactive TUI for us to drive. Inject ONLY if the release
            // actually reached the (still-connected) hook; otherwise it timed out and
            // injecting blind would type into the wrong screen.
            let released = send_question_response(
                ctx,
                &session_id,
                crate::types::FREETEXT_FALLBACK_SENTINEL,
                &client_id,
            )
            .await;
            if !released {
                let _ = ctx
                    .bot
                    .answer_callback_query(&cb.id, Some("This question already timed out."), true)
                    .await;
                ctx.bot
                .send_message(
                    "\u{26A0}\u{FE0F} This question timed out before your answer was submitted. Please answer at the terminal.",
                    None,
                    thread_id,
                )
                .await;
                tracing::warn!(session_id = %session_id, "ADR-014 E3: free-text release found hook gone — user notified");
            } else {
                let _ = ctx
                    .bot
                    .answer_callback_query(&cb.id, Some("Submitting..."), false)
                    .await;
                tracing::info!(
                    session_id = %session_id,
                    "ADR-014 E3: free-text answer — releasing hook and injecting via keystrokes"
                );
                // Claude needs a moment to render the TUI after the hook returns. This
                // wait is inherently racy and accepted for the free-text case only (E3).
                tokio::time::sleep(tokio::time::Duration::from_millis(
                    QUESTION_TUI_RENDER_WAIT_MS,
                ))
                .await;

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
                    for (_, _, answer) in &answers {
                        match answer {
                            // Single-option label or free-text: inject as literal text.
                            CollectedAnswer::Option(text) | CollectedAnswer::FreeText(text) => {
                                let _ = inj.inject(text);
                            }
                            CollectedAnswer::MultiSelect {
                                selected_indices,
                                total_options,
                                ..
                            } => {
                                // Claude Code's multi-select is a custom Ink (React CLI)
                                // checkbox TUI. Key bindings (from cli.js source):
                                //   Number keys (1-9) = toggle option by 1-based index
                                //   Down/Tab = move cursor; past last option focuses Submit
                                //   Enter on Submit button = submit selections
                                let key_delay = tokio::time::Duration::from_millis(300);

                                for &idx in selected_indices {
                                    let digit = format!("{}", idx + 1);
                                    let _ = inj.send_key(&digit);
                                    tokio::time::sleep(key_delay).await;
                                }
                                let downs_needed = total_options + 2; // options + Other + 1
                                for _ in 0..downs_needed {
                                    let _ = inj.send_key("Down");
                                    tokio::time::sleep(key_delay).await;
                                }
                                let _ = inj.send_key("Enter");
                            }
                        }
                    }
                    drop(inj);

                    // Auto-submit the Claude Code review screen (fallback path only).
                    auto_submit_answers(ctx, &session_id).await;
                } else {
                    tracing::warn!(
                        session_id = %session_id,
                        "ADR-013 D1: tmux not detected during submitall, answers cannot be injected"
                    );
                    ctx.bot
                    .send_message(
                        "\u{26A0}\u{FE0F} Answers could not be submitted \u{2014} tmux not detected. Please answer at the terminal.",
                        None,
                        thread_id,
                    )
                    .await;
                }
            }
        }
        SubmitPath::NoClient => {
            // ---- Degenerate: no blocked hook client recorded for this session. ----
            // Review fix (MED-2): the QuestionRequest never registered a client (its
            // _client_id was missing) or it was already consumed. There is no hook to
            // release and the terminal screen state is unknown, so we must NOT blindly
            // inject keystrokes. Surface it instead of silently doing the wrong thing.
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Couldn't submit — no active question."), true)
                .await;
            ctx.bot
            .send_message(
                "\u{26A0}\u{FE0F} Couldn't route your answer (no active question session). Please answer at the terminal.",
                None,
                thread_id,
            )
            .await;
            tracing::warn!(
                session_id = %session_id,
                "ADR-014: submitall with no originating client — answer not routed"
            );
        }
    }

    // Edit each question message to show "Submitted" and strip keyboard (both paths).
    for (q_idx, _, _) in &answers {
        let mid = question_message_ids.get(*q_idx).copied().unwrap_or(0);
        if mid != 0 {
            let _ = ctx
                .bot
                .edit_message_text_no_markup(mid, "\u{2705} Submitted")
                .await;
        }
    }

    // Edit summary message.
    if let Some(mid) = summary_message_id {
        let _ = ctx
            .bot
            .edit_message(chat_id, mid, "\u{2705} All answers submitted", None)
            .await;
    }
}

/// Handle "Change QN" callback.
///
/// ADR-012 Phase 7: Clears the tentative answer for question N, deletes the
/// summary message, and re-renders the question's keyboard without checkmarks
/// so the user can pick a new option.
async fn handle_change_callback(ctx: &HandlerContext, data: &str, cb: &CallbackQuery) {
    // Defense-in-depth: verify chat ownership (ADR-006 M4.5)
    if cb.message.as_ref().map(|m| m.chat.id) != Some(ctx.config.chat_id) {
        tracing::warn!("IDOR: callback from wrong chat in change");
        return;
    }
    // Format: change:{shortSessionId}:{questionIndex}
    let parts: Vec<&str> = data.splitn(3, ':').collect();
    if parts.len() != 3 {
        let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
        return;
    }
    let short_key = parts[1];
    let q_idx: usize = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => {
            let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
            return;
        }
    };

    // Phase 1: Brief read lock to get the Arc and full key.
    let (full_key, entry) = {
        let pq = ctx.pending_q.read().await;
        let fk = match resolve_pending_key(&pq, short_key) {
            Some(k) => k,
            None => {
                let _ = ctx
                    .bot
                    .answer_callback_query(&cb.id, Some("Question not found"), false)
                    .await;
                return;
            }
        };
        match pq.get(&fk) {
            Some(arc) => (fk, Arc::clone(arc)),
            None => {
                let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
                return;
            }
        }
    };

    // Phase 2: Per-key mutex held across state mutation AND API calls.
    let mut pending = entry.lock().await;

    if pending.finalized.get(q_idx) == Some(&true) {
        let _ = ctx
            .bot
            .answer_callback_query(&cb.id, Some("Already submitted"), false)
            .await;
        return;
    }

    // Clear the tentative answer for this question.
    pending.tentative.remove(&q_idx);

    let q = match pending.questions.get(q_idx) {
        Some(q) => q.clone(),
        None => {
            let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
            return;
        }
    };
    let msg_id = pending
        .question_message_ids
        .get(q_idx)
        .copied()
        .unwrap_or(0);
    let summary_message_id = pending.summary_message_id.take();
    let short = pending.session_id[..std::cmp::min(20, pending.session_id.len())].to_string();

    // API calls within per-key lock — no concurrent handler can race.
    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some("Tap to re-select"), false)
        .await;

    let chat_id = ctx.config.chat_id;

    // Delete the summary message.
    if let Some(mid) = summary_message_id {
        if let Err(e) = ctx.bot.delete_message(chat_id, mid).await {
            tracing::warn!(
                session_id = full_key,
                error = %e,
                "Failed to delete summary message during change"
            );
        }
    }

    // Re-render the question message with original text and no checkmarks.
    // PLAIN TEXT (parse_mode None below) — see render_question_text.
    if msg_id != 0 {
        let original_text = super::render_question_text(&q);

        let buttons: Vec<InlineButton> = if q.multi_select {
            let mut btns: Vec<InlineButton> = q
                .options
                .iter()
                .enumerate()
                .map(|(o_idx, opt)| InlineButton {
                    text: opt.label.clone(),
                    callback_data: format!("toggle:{short}:{q_idx}:{o_idx}"),
                })
                .collect();
            btns.push(InlineButton {
                text: "\u{2705} Done".into(),
                callback_data: format!("submit:{short}:{q_idx}"),
            });
            btns
        } else {
            q.options
                .iter()
                .enumerate()
                .map(|(o_idx, opt)| InlineButton {
                    text: opt.label.clone(),
                    callback_data: format!("answer:{short}:{q_idx}:{o_idx}"),
                })
                .collect()
        };

        if let Err(e) = ctx
            .bot
            .edit_message_text_with_markup(chat_id, msg_id, &original_text, None, &[buttons])
            .await
        {
            tracing::warn!(
                session_id = full_key,
                q_idx,
                error = %e,
                "Failed to re-render question message after change"
            );
        }
    }
    // Per-key Mutex drops here.
}

/// After all AskUserQuestion answers are collected, Claude Code shows a
/// "Review your answers" confirmation screen with numbered options:
///   > 1. Submit answers
///   > 2. Cancel
///
/// This helper waits briefly for Claude Code to render the review screen,
/// then injects "1" to auto-select "Submit answers" so the user doesn't
/// have to switch back to the console.
pub(super) async fn auto_submit_answers(ctx: &HandlerContext, session_id: &str) {
    // Wait for Claude Code to transition from the last question to the
    // review screen. Multi-select injection takes several seconds (300ms
    // per key × N keys), so the review screen may not appear for a while.
    // 2s is enough for the single-question → review transition.
    tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

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
        // "Submit answers" is already focused (option 1). Send Enter to
        // confirm it. Using inject("1") would work but leaves a stray "1"
        // in the input buffer after the review screen dismisses.
        let _ = inj.send_key("Enter");
        tracing::info!(session_id, "Auto-submitted AskUserQuestion review screen");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-014 E1: single-select answers map to their chosen label, keyed by
    /// question text, as exact JSON.
    #[test]
    fn answers_map_single_select() {
        let answers = vec![(
            0usize,
            "Pick a color".to_string(),
            CollectedAnswer::Option("Red".to_string()),
        )];
        let content = build_answers_map_content(&answers);
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["Pick a color"], "Red");
        assert_eq!(v.as_object().unwrap().len(), 1);
    }

    /// ADR-014 E1: multi-select labels are comma-joined into one string value.
    #[test]
    fn answers_map_multi_select_comma_joined() {
        let answers = vec![(
            0usize,
            "Pick langs".to_string(),
            CollectedAnswer::MultiSelect {
                labels: vec!["Rust".to_string(), "Go".to_string()],
                selected_indices: vec![0, 1],
                total_options: 3,
            },
        )];
        let content = build_answers_map_content(&answers);
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Bare comma, NO space — matches ccgram's verified "Claude Code format".
        assert_eq!(v["Pick langs"], "Rust,Go");
    }

    /// ADR-014 E3: free-text entries are excluded from the structured map (their
    /// presence forces the keystroke fallback, so they are never delivered here).
    #[test]
    fn answers_map_skips_free_text() {
        let answers = vec![
            (
                0usize,
                "Q1".to_string(),
                CollectedAnswer::Option("A".to_string()),
            ),
            (
                1usize,
                "Q2".to_string(),
                CollectedAnswer::FreeText("typed".to_string()),
            ),
        ];
        let content = build_answers_map_content(&answers);
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["Q1"], "A");
        assert!(v.get("Q2").is_none());
    }

    /// ADR-014 review: the submit-path decision must be exactly: no client → NoClient
    /// (never blind-inject); options-only + client → Structured; any free-text +
    /// client → FreeTextRelease. Guards the edge cases both reviewers flagged.
    #[test]
    fn classify_submit_decision_table() {
        assert_eq!(classify_submit(false, true), SubmitPath::Structured);
        assert_eq!(classify_submit(true, true), SubmitPath::FreeTextRelease);
        // No client → NoClient regardless of free-text (cannot route or safely inject).
        assert_eq!(classify_submit(false, false), SubmitPath::NoClient);
        assert_eq!(classify_submit(true, false), SubmitPath::NoClient);
    }

    /// ADR-014 PR-E benchmark: the structured answer-delivery compute path
    /// (`build_answers_map_content`) must be a negligible latency source compared
    /// to the keystroke path it replaces. The keystroke path's fixed cost for a
    /// representative 4-option multi-select selecting 2 options is, from the code
    /// constants (300ms/key + 2000ms auto_submit):
    ///   2 digit keys + (4 options + 2) Down keys + 1 Enter = 9 keys × 300ms
    ///   = 2700ms, plus the 2000ms auto_submit sleep = ~4700ms.
    /// The structured path below builds the entire answers map in microseconds and
    /// then performs a single socket write — eliminating that multi-second fixed
    /// latency AND the TUI readiness race. This test measures the compute cost and
    /// asserts it is comfortably sub-millisecond per call.
    #[test]
    fn bench_structured_delivery_is_negligible() {
        let answers = vec![
            (
                0usize,
                "Which language?".to_string(),
                CollectedAnswer::Option("Rust".to_string()),
            ),
            (
                1usize,
                "Which features?".to_string(),
                CollectedAnswer::MultiSelect {
                    labels: vec!["a".to_string(), "b".to_string(), "c".to_string()],
                    selected_indices: vec![0, 1, 2],
                    total_options: 5,
                },
            ),
        ];

        let iters = 10_000u32;
        let start = std::time::Instant::now();
        let mut sink = 0usize;
        for _ in 0..iters {
            let c = build_answers_map_content(&answers);
            sink = sink.wrapping_add(c.len());
        }
        let elapsed = start.elapsed();
        let per_call_ns = elapsed.as_nanos() / iters as u128;
        assert!(sink > 0);

        // Keystroke path fixed cost for the comparable multi-select (see doc above).
        let keystroke_ms = 9 * 300 + 2000; // 4700ms
        eprintln!(
            "ADR-014 PR-E bench: structured build = {per_call_ns} ns/call ({} calls in {:?}); \
             keystroke path it replaces ~= {keystroke_ms} ms fixed latency",
            iters, elapsed
        );

        // Generous bound to stay non-flaky across machines/CI: the structured
        // compute must be < 1ms/call (it is typically a few µs). The point is that
        // it is orders of magnitude below the multi-second keystroke path.
        assert!(
            per_call_ns < 1_000_000,
            "structured delivery compute regressed to {per_call_ns} ns/call (> 1ms)"
        );
    }
}
