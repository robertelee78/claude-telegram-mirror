//! Handlers for Telegram callback queries (button presses).

use super::*;

/// Per-poll interval for the navigate-to-Submit loop (time between a `capture_pane`
/// read + a single "Down" press while walking the cursor onto the inline Submit row).
const READY_POLL_INTERVAL_MS: u64 = 200;
/// Spacing between multi-select keystrokes (Claude's Ink checkbox TUI needs a beat
/// to register each toggle/cursor move).
const MULTISELECT_KEY_DELAY_MS: u64 = 300;
/// Slack added to the option count when bounding the navigate-to-Submit loop. The
/// real Claude Code 2.1.159 multi-select widget has, below the N numbered options:
/// an `<N+1>. Type something` free-text row, an inline `Submit` row, and a
/// `<N+2>. Chat about this` row — so Submit is reachable within a handful of Downs.
/// We never need more than `total_options + NAV_TO_SUBMIT_SLACK` steps; the cap just
/// prevents an infinite loop if `capture_pane` never reports the cursor on Submit.
const NAV_TO_SUBMIT_SLACK: usize = 4;

/// Collected answer for tmux injection, preserving type information so multi-select
/// can be injected as a key sequence instead of literal text. ADR-015: ALL answers
/// are delivered by injecting keystrokes into Claude's native CLI widget — there is no
/// structured/updatedInput path anymore (the hook does not intercept the question).
enum CollectedAnswer {
    /// Single-select: the chosen option's 0-based index, plus the total option count.
    /// ADR-015: the native widget selects by digit key (1-based), so single-select is
    /// delivered as a digit press + navigate-to-Submit + Enter — NOT as the label
    /// injected as free text. `total_options` bounds the navigate-to-Submit Down loop
    /// (Submit sits below ALL option rows + the "Type something" row, regardless of
    /// which option was chosen).
    Option {
        selected_index: usize,
        total_options: usize,
    },
    /// Multi-select: the chosen option indices drive the digit toggles; `total_options`
    /// bounds the navigate-to-Submit Down loop (Submit sits below ALL option rows + the
    /// "Type something" row, regardless of how many were selected).
    MultiSelect {
        selected_indices: Vec<usize>,
        total_options: usize,
    },
    /// Free-text typed by the user, injected as literal text + Enter.
    FreeText(String),
}

/// The cursor glyph Claude Code's Ink TUI prints to the left of the focused row.
const CURSOR_MARKER: char = '\u{276F}'; // ❯

/// ADR-015: Return the line in the captured pane that the cursor (`❯`) is on, if any.
///
/// ADR-015: Is the AskUserQuestion widget's cursor parked on the inline `Submit` row?
///
/// Deterministic stop condition for the navigate-to-Submit loop in `inject_answers`:
/// once true, pressing Enter once submits the whole widget (there is NO separate
/// "review your answers" screen — the Submit button is inline on the same widget).
///
/// We scan **all** `❯`-marked lines, NOT just the first. The captured pane includes
/// Claude Code's scrollback, where prior user prompts are ALSO prefixed with `❯`
/// (e.g. "❯ yes, both"). Keying off the *first* `❯` line (an earlier bug) always matched
/// a scrollback prompt and never the widget, so `cursor_is_on_submit` was silently always
/// false — the navigate-to-Submit loop never detected Submit and fell through to a blind
/// `total_options + slack` Down count that overshot Submit (the multi-select "clarify"
/// regression; single-select was unaffected because the digit press submits it directly).
/// The widget's Submit row is the only `❯` line that strips to exactly "Submit", so rows
/// like "❯ Submit answers" or "❯ 1. Submit later" never false-positive. Pure string check,
/// unit-tested without a live tmux.
fn cursor_is_on_submit(pane: &str) -> bool {
    pane.lines().any(|line| {
        line.contains(CURSOR_MARKER) && line.replace(CURSOR_MARKER, "").trim() == "Submit"
    })
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

/// ADR-015 (Codex post-I/O recheck for non-terminal edits): after a non-terminal handler
/// re-arms a question message's keyboard (answer/toggle/change), a fast prior Submit All
/// may have transitioned the entry `Active→Submitting→Resolved` and finalized (stripped
/// buttons, removed the entry) DURING the edit `.await`, so the late edit re-armed LIVE
/// buttons on an already-answered message. Re-lock the captured entry and correct it:
///   - `Resolved` → re-stale THIS message (terminal text, NO markup) so no live buttons
///     remain.
///   - `Submitting` → leave it; the Submit All finalizer still owns and will strip it.
///   - `Active` → normal; nothing to do.
///
/// Keeps the no-lock-across-I/O rule: lock → read lifecycle → DROP → then the corrective
/// edit (if any). `pub(super)` so the free-text path in `telegram_handlers` reuses it.
pub(super) async fn restale_if_resolved(
    ctx: &HandlerContext,
    entry: &Arc<Mutex<PendingQuestion>>,
    msg_id: i64,
) {
    if msg_id == 0 {
        return;
    }
    let lifecycle = { entry.lock().await.lifecycle };
    if lifecycle == QuestionLifecycle::Resolved {
        let _ = ctx
            .bot
            .edit_message_text_no_markup(msg_id, "\u{2705} Answered at terminal")
            .await;
    }
}

/// Handle single-select answer callback.
///
/// ADR-012: Taps are now tentative — the selection is stored in
/// `pending.tentative` and the keyboard is re-rendered with a ✓ prefix on
/// the chosen option. Nothing is injected into tmux until "Submit All".
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

    // Phase 1: clone the Arc + key under the read guard, then DROP it before any `.await`
    // (ADR-015 lock-across-I/O sweep: never hold the pending_q guard across bot I/O).
    let resolved: Option<(String, Arc<Mutex<PendingQuestion>>)> = {
        let pq = ctx.pending_q.read().await;
        resolve_pending_key(&pq, short_key)
            .and_then(|fk| pq.get(&fk).map(|arc| (fk, Arc::clone(arc))))
    };
    let Some((full_key, entry)) = resolved else {
        let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
        return;
    };

    // Phase 2: Per-key mutex held across state mutation AND API calls.
    // ADR-015 (Codex lock-across-I/O sweep): under the entry lock, mutate state and CLONE
    // everything the Telegram edits need; DROP the lock; THEN await the bot I/O. Never
    // hold the per-entry mutex across a bot.* await.
    //
    // ADR-015 (Codex lifecycle-gate): `lifecycle` is the SINGLE concurrency arbiter. A
    // non-terminal mutator may act ONLY while `Active`. Once Submit All sets `Submitting`
    // (it drops the lock for the multi-second inject), or a CLI answer / supersede sets
    // `Resolved`, a raced tap must NOT mutate tentative state or edit messages.
    enum AnswerOutcome {
        NotActive,
        Selected {
            option_label: String,
            updated_text: String,
            new_buttons: Vec<InlineButton>,
            msg_id: i64,
            all_answered: bool,
        },
    }
    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);
    let chat_id = ctx.config.chat_id;
    let outcome = {
        let mut pending = entry.lock().await;

        if pending.lifecycle != QuestionLifecycle::Active {
            AnswerOutcome::NotActive
        } else {
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

            // Rebuild the question message text (PLAIN TEXT — see render_question_text)
            // with the current selection shown.
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
            let all_answered = pending.tentative.len() == pending.questions.len();

            AnswerOutcome::Selected {
                option_label,
                updated_text,
                new_buttons,
                msg_id,
                all_answered,
            }
        }
        // entry mutex drops here
    };

    let (option_label, updated_text, new_buttons, msg_id, all_answered) = match outcome {
        AnswerOutcome::NotActive => {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Answer already being submitted"), false)
                .await;
            return;
        }
        AnswerOutcome::Selected {
            option_label,
            updated_text,
            new_buttons,
            msg_id,
            all_answered,
        } => (
            option_label,
            updated_text,
            new_buttons,
            msg_id,
            all_answered,
        ),
    };

    // Toast feedback (entry lock already dropped).
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

    // ADR-015 (Codex post-I/O recheck): if a fast prior Submit All finalized/resolved
    // this question during the edit above, our edit just re-armed live option buttons —
    // re-stale this message so none remain.
    restale_if_resolved(ctx, &entry, msg_id).await;

    // If all questions now have a tentative answer, send/update summary.
    if all_answered {
        let _ = send_or_update_summary(ctx, &full_key, thread_id).await;
    }
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

    // ADR-015 (Codex lock-across-I/O sweep): mutate the toggle set + build the keyboard
    // under the entry lock, CLONE the buttons, DROP the lock, THEN edit the markup.
    // ADR-015 (Codex lifecycle-gate): only mutate while `Active`; a tap racing Submit All
    // (Submitting) or a CLI answer / supersede (Resolved) must not change tentative state.
    let new_buttons: Option<Vec<InlineButton>> = {
        let mut pending = entry.lock().await;

        if pending.lifecycle != QuestionLifecycle::Active {
            None
        } else {
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

            // M2: Re-render keyboard with checkmarks.
            pending.questions.get(q_idx).map(|question| {
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
                buttons
            })
        }
        // entry mutex drops here
    };

    if let (Some(buttons), Some(msg)) = (new_buttons, &cb.message) {
        let _ = ctx
            .bot
            .edit_message_reply_markup(msg.message_id, &buttons)
            .await;
        // ADR-015 (Codex post-I/O recheck): if a fast prior Submit All finalized/resolved
        // this question during the markup edit above, we just re-armed live toggle/Done
        // buttons — re-stale this message so none remain.
        restale_if_resolved(ctx, &entry, msg.message_id).await;
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

    // Phase 1: clone the Arc + key under the read guard, then DROP it before any `.await`
    // (ADR-015 lock-across-I/O sweep: never hold the pending_q guard across bot I/O).
    let resolved: Option<(String, Arc<Mutex<PendingQuestion>>)> = {
        let pq = ctx.pending_q.read().await;
        resolve_pending_key(&pq, short_key)
            .and_then(|fk| pq.get(&fk).map(|arc| (fk, Arc::clone(arc))))
    };
    let Some((full_key, entry)) = resolved else {
        let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
        return;
    };

    // ADR-015 (Codex lock-across-I/O sweep): mutate state + build the toast text under
    // the entry lock, CLONE it out, DROP the lock, THEN await the toast and summary.
    // ADR-015 (Codex lifecycle-gate): only mutate while `Active`.
    enum SubmitOutcome {
        NotActive,
        Done {
            toast_text: String,
            all_answered: bool,
        },
    }
    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);
    let outcome = {
        let mut pending = entry.lock().await;

        if pending.lifecycle != QuestionLifecycle::Active {
            SubmitOutcome::NotActive
        } else {
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
            SubmitOutcome::Done {
                toast_text,
                all_answered,
            }
        }
        // entry mutex drops here
    };

    let (toast_text, all_answered) = match outcome {
        SubmitOutcome::NotActive => {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Answer already being submitted"), false)
                .await;
            return;
        }
        SubmitOutcome::Done {
            toast_text,
            all_answered,
        } => (toast_text, all_answered),
    };

    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some(&toast_text), false)
        .await;

    if all_answered {
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
    // Look up the per-key mutex (drop the map guard before locking the entry).
    let entry = {
        let pq = ctx.pending_q.read().await;
        pq.get(full_key).map(Arc::clone)?
    };

    // ADR-015 (Codex lock-across-I/O sweep): under the entry lock, build the summary text
    // + keyboard and capture the existing summary_message_id; DROP the lock; THEN do the
    // Telegram edit/send. Re-lock ONLY to store the resulting id, and only while the
    // entry is still Active (a concurrent resolve/submit may have taken ownership during
    // the I/O — don't resurrect a stale summary).
    let (summary_text, reply_markup, existing_summary_id) = {
        let pending = entry.lock().await;

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

        // Build reply_markup keyboard.
        let n = pending.questions.len();
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
        let reply_markup = serde_json::json!({"inline_keyboard": rows});

        (summary_text, reply_markup, pending.summary_message_id)
        // entry mutex drops here
    };

    let chat_id = ctx.config.chat_id;

    // Either edit the existing summary or send a new one (entry lock NOT held).
    if let Some(mid) = existing_summary_id {
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
                // ADR-015 (Codex post-I/O recheck): a concurrent Submit All / resolve may
                // have taken ownership (left Active) DURING this edit, after already
                // editing the same summary to its terminal text. Our edit just re-armed
                // the live "Review your answers" markup ON TOP of that. Re-lock: if no
                // longer Active, stale this summary (terminal text, NO buttons) so no live
                // review markup is left behind, and do NOT keep the id.
                let still_active = { entry.lock().await.lifecycle == QuestionLifecycle::Active };
                if still_active {
                    return Some(mid);
                }
                let _ = ctx
                    .bot
                    .edit_message(chat_id, mid, "\u{2705} Answered at terminal", None)
                    .await;
                return None;
            }
            Err(e) => {
                tracing::warn!(
                    session_id = full_key,
                    error = %e,
                    "Failed to edit summary message; will send new one"
                );
                // Clear the stale id (only if still Active).
                let mut pending = entry.lock().await;
                if pending.lifecycle == QuestionLifecycle::Active {
                    pending.summary_message_id = None;
                }
            }
        }
    }

    // Send new summary message (entry lock NOT held).
    match ctx
        .bot
        .send_message_with_raw_markup_returning(&summary_text, None, reply_markup, thread_id)
        .await
    {
        Ok(new_mid) => {
            // ADR-015 (Codex post-I/O recheck): store the id ONLY if still Active. If a
            // concurrent resolve/submit took ownership during the send, the just-sent
            // summary carries live review markup that would otherwise orphan — stale it
            // (terminal text, NO buttons) and don't store the id. Check-and-store under a
            // SINGLE lock so the decision and the store can't be split by a transition.
            let stored = {
                let mut pending = entry.lock().await;
                if pending.lifecycle == QuestionLifecycle::Active {
                    pending.summary_message_id = Some(new_mid);
                    true
                } else {
                    false
                }
            };
            if stored {
                Some(new_mid)
            } else {
                let _ = ctx
                    .bot
                    .edit_message(chat_id, new_mid, "\u{2705} Answered at terminal", None)
                    .await;
                None
            }
        }
        Err(e) => {
            tracing::warn!(session_id = full_key, error = %e, "Failed to send summary message");
            // ADR-015: the "Submit All" summary could not be sent (e.g. a transient
            // Telegram error), so the user has no Telegram button to submit. There is
            // no blocked hook to release — Claude's native CLI widget is still live, so
            // just tell the user to answer at the terminal.
            ctx.bot
                .send_message(
                    "\u{26A0}\u{FE0F} Couldn't show the Submit button (Telegram send failed). Please answer at the terminal.",
                    None,
                    thread_id,
                )
                .await;
            None
        }
    }
}

/// Handle "Submit All" callback.
///
/// ADR-015: Locks all tentative answers, injects them into Claude's native CLI widget
/// via tmux in question order (each answer's keystroke sequence ends with a single
/// Enter on the inline Submit row — there is NO separate review screen), and finally
/// edits each Telegram question message and the summary to show "Submitted".
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

    // Phase 1: Brief read lock to clone the Arc and full key, then DROP the guard before
    // any `.await` (Codex deadlock fix: never hold the `pending_q` guard across an await,
    // and in particular never across `entry.lock()`).
    let resolved: Option<(String, Arc<Mutex<PendingQuestion>>)> = {
        let pq = ctx.pending_q.read().await;
        resolve_pending_key(&pq, short_key)
            .and_then(|fk| pq.get(&fk).map(|arc| (fk, Arc::clone(arc))))
    };
    let Some((full_key, entry)) = resolved else {
        let _ = ctx
            .bot
            .answer_callback_query(&cb.id, Some("No pending question"), false)
            .await;
        return;
    };

    let chat_id = ctx.config.chat_id;
    let thread_id = cb.message.as_ref().and_then(|m| m.message_thread_id);

    // Codex B1: resolve the tmux target BEFORE we touch the pending entry's state, so a
    // missing target never strands a half-finalized entry. (Lock ordering: session_tmux
    // is acquired before the per-entry pending_q mutex.)
    let tmux_target = ctx.session_tmux.read().await.get(&full_key).cloned();

    // Phase 2: under the per-entry mutex, arbitrate ownership (Codex B3) and extract the
    // answers. We transition Active→Submitting ONLY when delivery can actually proceed
    // (a tmux target exists); otherwise we leave the entry Active for a retry / terminal
    // answer (Codex B1). ADR-015 (Codex lock-across-I/O sweep): compute an outcome under
    // the lock, CLONE everything needed, DROP the lock, THEN await the toast/notice —
    // never hold the entry mutex across bot.* I/O.
    enum SubmitAllOutcome {
        AlreadySubmitting,
        AlreadyResolved,
        Unanswered,
        NoTmux,
        Proceed {
            answers: Vec<(usize, String, CollectedAnswer)>,
            session_id: String,
            question_message_ids: Vec<i64>,
            summary_message_id: Option<i64>,
        },
    }
    let outcome = {
        let mut pending = entry.lock().await;

        if pending.lifecycle == QuestionLifecycle::Submitting {
            SubmitAllOutcome::AlreadySubmitting
        } else if pending.lifecycle == QuestionLifecycle::Resolved {
            SubmitAllOutcome::AlreadyResolved
        } else if pending.tentative.len() != pending.questions.len() {
            // "Change QN" raced "Submit All" and left a question unanswered.
            SubmitAllOutcome::Unanswered
        } else if tmux_target.is_none() {
            // Codex B1: leave the entry Active (do NOT finalize / set Submitting / remove).
            SubmitAllOutcome::NoTmux
        } else {
            // Collect all answers in question order, preserving type info for injection.
            let mut answers: Vec<(usize, String, CollectedAnswer)> = Vec::new();
            for (q_idx, q) in pending.questions.iter().enumerate() {
                let question_text = q.question.clone();
                let total_options = q.options.len();
                let answer = match pending.tentative.get(&q_idx) {
                    Some(TentativeAnswer::Option(o_idx)) => CollectedAnswer::Option {
                        selected_index: *o_idx,
                        total_options,
                    },
                    Some(TentativeAnswer::MultiOption(set)) => {
                        let mut sorted: Vec<usize> = set.iter().copied().collect();
                        sorted.sort();
                        CollectedAnswer::MultiSelect {
                            selected_indices: sorted,
                            total_options,
                        }
                    }
                    Some(TentativeAnswer::FreeText(s)) => CollectedAnswer::FreeText(s.clone()),
                    None => continue,
                };
                answers.push((q_idx, question_text, answer));
            }

            // Claim ownership: Active → Submitting. resolve_pending_question now no-ops,
            // and the map entry is NOT yet removed (so a duplicate tap hits Submitting).
            pending.lifecycle = QuestionLifecycle::Submitting;

            SubmitAllOutcome::Proceed {
                answers,
                session_id: pending.session_id.clone(),
                question_message_ids: pending.question_message_ids.clone(),
                summary_message_id: pending.summary_message_id,
            }
        }
        // per-entry Mutex drops here
    };

    let (answers, session_id, question_message_ids, summary_message_id) = match outcome {
        SubmitAllOutcome::AlreadySubmitting => {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Already submitting…"), false)
                .await;
            return;
        }
        SubmitAllOutcome::AlreadyResolved => {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Already submitted"), false)
                .await;
            return;
        }
        SubmitAllOutcome::Unanswered => {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Please answer all questions first"), true)
                .await;
            return;
        }
        SubmitAllOutcome::NoTmux => {
            tracing::warn!(
                session_id = %full_key,
                "ADR-015: tmux not detected during submitall — entry left Active for retry"
            );
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Couldn't submit — tmux not detected."), true)
                .await;
            ctx.bot
                .send_message(
                    "\u{26A0}\u{FE0F} Couldn't submit \u{2014} tmux not detected. Answer at the terminal or tap Submit again once reconnected.",
                    None,
                    thread_id,
                )
                .await;
            return;
        }
        SubmitAllOutcome::Proceed {
            answers,
            session_id,
            question_message_ids,
            summary_message_id,
        } => (
            answers,
            session_id,
            question_message_ids,
            summary_message_id,
        ),
    };
    let target = tmux_target.expect("tmux_target checked present under the entry mutex");

    let _ = ctx
        .bot
        .answer_callback_query(&cb.id, Some("Submitting..."), false)
        .await;

    let sid = session_id.clone();
    let socket = ctx
        .db_op(move |sess| {
            sess.get_session(&sid)
                .ok()
                .flatten()
                .and_then(|s| s.tmux_socket)
        })
        .await;

    // ADR-015: ALL answers are injected into Claude's native CLI widget via tmux.
    // Codex B2: only on FULL delivery do we auto-submit, mark "Submitted", and remove
    // the entry. On partial/failed delivery we restore the entry to Active (so a retry
    // or terminal answer can resolve it) and alert — never claiming a false success.
    let delivered = inject_answers(ctx, &target, socket.as_deref(), &answers).await;

    if !delivered {
        tracing::warn!(
            session_id = %session_id,
            "ADR-015: keystroke injection failed mid-flight — restoring entry to Active"
        );
        // Restore ownership so the question is answerable again. Only revert if we still
        // hold it as Submitting (a concurrent resolve cannot have run while Submitting).
        let mut pending = entry.lock().await;
        if pending.lifecycle == QuestionLifecycle::Submitting {
            pending.lifecycle = QuestionLifecycle::Active;
            for f in pending.finalized.iter_mut() {
                *f = false;
            }
        }
        drop(pending);
        ctx.bot
            .send_message(
                "\u{26A0}\u{FE0F} Couldn't deliver all answers to the terminal. Please answer at the terminal, or tap Submit again to retry.",
                None,
                thread_id,
            )
            .await;
        return;
    }

    // Full delivery: transition Submitting→Resolved and finalize UNDER the entry mutex,
    // then DROP the entry lock before touching pending_q (Codex deadlock fix: never hold
    // the entry mutex across `pending_q.write()`). The `Resolved` flag — set before the
    // lock is dropped — is what makes a concurrent resolve no-op; map removal afterward
    // is safe because resolve sees `Resolved`/absent.
    {
        let mut pending = entry.lock().await;
        pending.lifecycle = QuestionLifecycle::Resolved;
        for f in pending.finalized.iter_mut() {
            *f = true;
        }
    }
    // ADR-015 (Codex identity-checked removal): remove ONLY if the map still points at
    // OUR Arc. If a new AskUserQuestion superseded this entry between snapshot and now,
    // the key maps to the NEW (active) entry — removing by key would orphan it.
    {
        let mut pq = ctx.pending_q.write().await;
        if pq
            .get(&full_key)
            .is_some_and(|cur| Arc::ptr_eq(cur, &entry))
        {
            pq.remove(&full_key);
        }
    }

    // ADR-015: No "Review your answers" screen exists — each answer's injection already
    // landed Enter on the inline Submit row inside the widget, so the question is
    // submitted. Do NOT fire another Enter here (it would land on the now-empty Claude
    // prompt as a stray keystroke).

    // Edit each question message to show "Submitted" and strip keyboard.
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

    // Phase 1: clone the Arc + key under the read guard, then DROP it before any `.await`
    // (ADR-015 lock-across-I/O sweep: never hold the pending_q guard across bot I/O).
    let resolved: Option<(String, Arc<Mutex<PendingQuestion>>)> = {
        let pq = ctx.pending_q.read().await;
        resolve_pending_key(&pq, short_key)
            .and_then(|fk| pq.get(&fk).map(|arc| (fk, Arc::clone(arc))))
    };
    let Some((full_key, entry)) = resolved else {
        let _ = ctx
            .bot
            .answer_callback_query(&cb.id, Some("Question not found"), false)
            .await;
        return;
    };

    // ADR-015 (Codex lock-across-I/O sweep): under the entry lock, mutate state and CLONE
    // everything the edits need (the question, message ids, short key); DROP the lock;
    // THEN await the toast/delete/edit. Never hold the per-entry mutex across bot.* I/O.
    // ADR-015 (Codex lifecycle-gate): only clear/re-render while `Active`; a "Change"
    // racing Submit All (Submitting) or a CLI answer / supersede (Resolved) must no-op.
    enum ChangeOutcome {
        NotActive,
        Missing,
        Reselect {
            q: QuestionDef,
            msg_id: i64,
            summary_message_id: Option<i64>,
            short: String,
        },
    }
    let outcome = {
        let mut pending = entry.lock().await;

        if pending.lifecycle != QuestionLifecycle::Active {
            ChangeOutcome::NotActive
        } else {
            // Clear the tentative answer for this question.
            pending.tentative.remove(&q_idx);

            match pending.questions.get(q_idx).cloned() {
                None => ChangeOutcome::Missing,
                Some(q) => {
                    let msg_id = pending
                        .question_message_ids
                        .get(q_idx)
                        .copied()
                        .unwrap_or(0);
                    let summary_message_id = pending.summary_message_id.take();
                    let short = pending.session_id[..std::cmp::min(20, pending.session_id.len())]
                        .to_string();
                    ChangeOutcome::Reselect {
                        q,
                        msg_id,
                        summary_message_id,
                        short,
                    }
                }
            }
        }
        // entry mutex drops here
    };

    let (q, msg_id, summary_message_id, short) = match outcome {
        ChangeOutcome::NotActive => {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("Answer already being submitted"), false)
                .await;
            return;
        }
        ChangeOutcome::Missing => {
            let _ = ctx.bot.answer_callback_query(&cb.id, None, false).await;
            return;
        }
        ChangeOutcome::Reselect {
            q,
            msg_id,
            summary_message_id,
            short,
        } => (q, msg_id, summary_message_id, short),
    };

    // Telegram I/O (entry lock NOT held).
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

        // ADR-015 (Codex post-I/O recheck): "Change" re-arms option buttons; if a fast
        // prior Submit All finalized/resolved this question during the edit above,
        // re-stale this message so no live buttons remain.
        restale_if_resolved(ctx, &entry, msg_id).await;
    }
}

/// ADR-015: Inject the collected answers into Claude's native AskUserQuestion widget
/// via tmux, in question order. Returns `true` only if EVERY keystroke was delivered.
///
/// Both branches drive the SAME native widget and submit the SAME way — by pressing
/// the option's digit key(s) to toggle the selection, then navigating the cursor onto
/// the inline `Submit` row (deterministically, via `capture_pane`) and pressing Enter
/// ONCE. There is NO separate "review your answers" screen; Submit is inline on the
/// widget. Free-text is the only literal-text path.
///
/// - Single-select: press the chosen option's 1-based digit, then navigate-to-Submit +
///   Enter. (Selecting by digit avoids the old "label injected as free text" risk; the
///   navigate-to-Submit + Enter is safe even if a digit auto-confirms — the loop simply
///   finds the cursor already off the widget and the best-effort Enter is harmless.)
/// - Multi-select: press a digit per selected index (toggles each checkbox), then
///   navigate-to-Submit + Enter.
/// - Free-text: literal text + Enter (`inject`).
///
/// Codex B2: `inject`/`send_key` return `Result<bool>` (`Ok(false)` = validation/tmux
/// soft failure, `Err` = hard failure). Anything other than `Ok(true)` is a delivery
/// failure: we STOP at the first one and return `false` so the caller does NOT mark the
/// Telegram messages "Submitted" and restores the pending entry for a retry / terminal
/// answer. The navigate-to-Submit loop's best-effort fallback Enter is NOT a delivery
/// failure (it logs a warning but lets the submit proceed).
async fn inject_answers(
    ctx: &HandlerContext,
    target: &str,
    socket: Option<&str>,
    answers: &[(usize, String, CollectedAnswer)],
) -> bool {
    let key_delay = tokio::time::Duration::from_millis(MULTISELECT_KEY_DELAY_MS);
    let mut inj = ctx.injector.lock().await;
    inj.set_target(target, socket);

    // Helper: treat anything but Ok(true) as a delivery failure (logged by caller).
    fn delivered(r: crate::error::Result<bool>) -> bool {
        matches!(r, Ok(true))
    }

    for (_, _, answer) in answers {
        // Normalize to the digit indices to press plus the total option count
        // (single-select = one digit; multi-select = one per toggled option; free-text
        // = none). `total_options` — NOT how many were selected — sets how far Submit
        // can be from the top option, so it bounds the navigate-to-Submit Down loop.
        let (indices, total_options): (Vec<usize>, usize) = match answer {
            CollectedAnswer::FreeText(text) => {
                // Free-text is the only literal-text path; it submits on its own Enter.
                if !delivered(inj.inject(text)) {
                    return false;
                }
                continue;
            }
            CollectedAnswer::Option {
                selected_index,
                total_options,
            } => (vec![*selected_index], *total_options),
            CollectedAnswer::MultiSelect {
                selected_indices,
                total_options,
            } => (selected_indices.clone(), *total_options),
        };

        // Toggle each selected option by its 1-based digit.
        for &idx in &indices {
            let digit = format!("{}", idx + 1);
            if !delivered(inj.send_key(&digit)) {
                return false;
            }
            tokio::time::sleep(key_delay).await;
        }
        // Navigate the cursor onto the inline Submit row, then Enter ONCE.
        if !navigate_to_submit_and_enter(&inj, total_options, key_delay).await {
            return false;
        }
    }
    true
}

/// ADR-015: Walk the cursor onto the inline `Submit` row of the live AskUserQuestion
/// widget, then press Enter ONCE to submit. Returns `false` only on a hard keystroke
/// delivery failure (so the caller can fail closed); a best-effort fallback Enter
/// (loop exhausted without confirming the cursor on Submit) still returns `true`.
///
/// The widget rows below the N numbered options are: `Type something`, an inline
/// `Submit`, then `Chat about this`. After toggling, the cursor position is NOT
/// deterministic, so we read `capture_pane` and, while the cursor is not yet on
/// Submit, press "Down" and re-check — bounded by `total_options + NAV_TO_SUBMIT_SLACK`
/// steps. The bound MUST scale with `total_options` (the worst-case distance from the
/// top option to Submit is all N option rows + the "Type something" row), NOT with how
/// many options were selected — otherwise a single selection in a long list would
/// under-shoot and the best-effort Enter could fire on the wrong row.
///
/// The parsing (`cursor_is_on_submit`) is unit-tested; this navigation wrapper is thin
/// (it needs a live tmux) and intentionally untested.
async fn navigate_to_submit_and_enter(
    inj: &tokio::sync::MutexGuard<'_, crate::injector::InputInjector>,
    total_options: usize,
    key_delay: tokio::time::Duration,
) -> bool {
    // Bound by the TOTAL option count: Submit sits below all N option rows + the
    // "Type something" row, so the cursor may be up to ~N+1 Downs above it regardless
    // of how many options were selected. The slack covers that "Type something" row
    // plus a small safety margin; `cursor_is_on_submit` is the real stop condition.
    let max_steps = total_options + NAV_TO_SUBMIT_SLACK;
    let interval = tokio::time::Duration::from_millis(READY_POLL_INTERVAL_MS);

    let mut landed = false;
    for step in 0..=max_steps {
        // Re-read the pane each iteration: the cursor glyph (`❯`) marks exactly one row.
        if inj
            .capture_pane()
            .as_deref()
            .is_some_and(cursor_is_on_submit)
        {
            landed = true;
            break;
        }
        if step == max_steps {
            break; // exhausted — fall through to best-effort Enter
        }
        // Not on Submit yet: step down one row and pace before re-checking.
        match inj.send_key("Down") {
            Ok(true) => {}
            // Hard failure (Err) or rejected (Ok(false)) → fail closed.
            _ => return false,
        }
        tokio::time::sleep(interval).await;
        // Small extra beat lets the Ink TUI repaint the moved cursor before recapture.
        tokio::time::sleep(key_delay).await;
    }

    if !landed {
        tracing::warn!(
            max_steps,
            "ADR-015: navigate-to-Submit exhausted without confirming cursor on Submit; \
             firing best-effort Enter"
        );
    }

    // Either the cursor is on Submit, or best-effort: press Enter ONCE.
    matches!(inj.send_key("Enter"), Ok(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The captured layout of the REAL Claude Code 2.1.159 AskUserQuestion multi-select
    /// widget, with the cursor (`❯`) parked on a numbered OPTION (not Submit). Captured
    /// live via tmux capture-pane.
    const PANE_CURSOR_ON_OPTION: &str = "\
←  ☒ Capture  ✔ Submit  →
🏈 Which languages?
❯ 1. [ ] Rust
    A systems language
  2. [✔] Go
    A cloud language
  3. [ ] Type something
     Submit
  ──────────
  4. Chat about this
Enter to select · ↑/↓ to navigate · Esc to cancel";

    /// Same widget, but the cursor has been walked down onto the inline `Submit` row.
    const PANE_CURSOR_ON_SUBMIT: &str = "\
←  ☒ Capture  ✔ Submit  →
🏈 Which languages?
  1. [✔] Rust
    A systems language
  2. [✔] Go
    A cloud language
  3. [ ] Type something
❯    Submit
  ──────────
  4. Chat about this
Enter to select · ↑/↓ to navigate · Esc to cancel";

    /// ADR-015: `cursor_is_on_submit` is the deterministic stop condition for the
    /// navigate-to-Submit loop. It must be TRUE only when the cursor (`❯`) line, with
    /// the marker + decoration stripped, is exactly "Submit".
    #[test]
    fn cursor_on_submit_detection() {
        // Cursor on an option row → NOT on Submit.
        assert!(!cursor_is_on_submit(PANE_CURSOR_ON_OPTION));
        // Cursor walked onto the inline Submit row → on Submit.
        assert!(cursor_is_on_submit(PANE_CURSOR_ON_SUBMIT));

        // Minimal positive: the exact "❯    Submit" line match used live.
        assert!(cursor_is_on_submit("❯    Submit"));
        assert!(cursor_is_on_submit("  ❯ Submit"));

        // A "Submit" row WITHOUT the cursor on it → no match.
        assert!(!cursor_is_on_submit("     Submit"));
        // Cursor on a Submit-LIKE row that isn't exactly "Submit" → no match.
        assert!(!cursor_is_on_submit("❯ Submit answers"));
        assert!(!cursor_is_on_submit("❯ 1. Submit later"));
        // No cursor anywhere / empty → no match.
        assert!(!cursor_is_on_submit("1. [ ] Rust\n2. [ ] Go"));
        assert!(!cursor_is_on_submit(""));

        // Regression (ADR-015 multi-select "clarify" bug): the captured pane includes
        // Claude Code's scrollback, whose prior prompts are ALSO `❯`-prefixed. Detection
        // must key off the WIDGET's Submit row by scanning ALL lines — not the FIRST `❯`
        // line (which is a scrollback prompt). Keying off the first `❯` always read false,
        // so the navigate-to-Submit loop blind-counted past Submit and mis-submitted.
        let scrollback = "❯ yes, both\n❯ ask again\n❯ I'm not seeing our session\n";
        assert!(!cursor_is_on_submit(&format!(
            "{scrollback}{PANE_CURSOR_ON_OPTION}"
        )));
        assert!(cursor_is_on_submit(&format!(
            "{scrollback}{PANE_CURSOR_ON_SUBMIT}"
        )));
    }
}
