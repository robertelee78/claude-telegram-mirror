//! Handlers for Telegram callback queries (button presses).

use super::*;

/// Per-poll interval between a `capture_pane` read and the next keystroke/recapture
/// while navigating the widget or waiting for a screen transition.
const READY_POLL_INTERVAL_MS: u64 = 200;
/// Beat after each keystroke so Claude's Ink TUI repaints before we recapture.
const KEY_DELAY_MS: u64 = 300;
/// Slack added to a question's option count when bounding the down-only navigate loop.
/// Below the M options sit `Type something`, the `Next`/`Submit` advance row, and
/// `Chat about this`, so any focusable target is within `M + NAV_SLACK_STEPS` Downs even
/// with viewport clipping (Ink scrolls the focused row into view as we step).
const NAV_SLACK_STEPS: usize = 6;
/// Max `capture_pane` polls (× `READY_POLL_INTERVAL_MS`) to wait for an expected screen
/// transition (next question active / the confirm screen) before failing closed.
const WAIT_STATE_MAX_POLLS: usize = 25;
/// FR32-aligned cap on free-text injected into the widget's `Type something` row.
const FREETEXT_MAX_CHARS: usize = 8192;

// ADR-015: hard-coded English widget labels, isolated as constants (the parsers key off
// these; localization/CLI-version drift would require updating them + the fixtures).
const LABEL_TYPE_SOMETHING: &str = "Type something";
const LABEL_NEXT: &str = "Next";
const LABEL_SUBMIT: &str = "Submit";
const LABEL_CHAT_ABOUT: &str = "Chat about this";
const LABEL_SUBMIT_ANSWERS: &str = "Submit answers";
const LABEL_CONFIRM_PROMPT: &str = "Ready to submit your answers?";

/// ADR-015: A validated, collected answer ready for tmux injection into Claude's native
/// AskUserQuestion widget. Index variants carry ONLY in-range option indices (validated
/// against the question's option count at collection time). The per-question `multiSelect`
/// flag is carried alongside this in the answers tuple — it (not the variant) decides the
/// advance mechanism, because free-text can answer either a single- or multi-select Q.
#[derive(Debug, PartialEq, Eq)]
enum CollectedAnswer {
    /// Single-select: the chosen option's 0-based index (`< options.len()`).
    Single(usize),
    /// Multi-select: chosen 0-based indices (sorted, unique, all `< options.len()`,
    /// non-empty — an empty multi-select is rejected before injection ever begins).
    Multi(Vec<usize>),
    /// Free-text typed by the user (sanitized + length-capped at injection time).
    FreeText(String),
}

/// ADR-015 (Codex v3): the typed result of an injection attempt. The distinction drives
/// the caller's recovery: `FailedClean` means NO keystroke was acknowledged by tmux, so
/// the live widget is untouched and the Telegram entry can safely be restored to `Active`
/// for a retry / terminal answer. `FailedDirty` means at least one keystroke landed, so
/// the live widget is in an indeterminate partially-advanced state — a blind retry from
/// Q0 would corrupt it, so the caller terminalizes the entry and tells the user to finish
/// at the terminal (the live widget is still on-screen for them).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum InjectOutcome {
    Success,
    FailedClean,
    FailedDirty,
}

/// The cursor glyph Claude Code's Ink TUI prints to the left of the focused row.
const CURSOR_MARKER: char = '\u{276F}'; // ❯

// ───────────────────────────── widget pane parsing (ADR-015 v4) ─────────────────────────
//
// All parsing is scoped to the LIVE widget block — the lines from the BOTTOM-MOST tab row
// (`←  … ✔ Submit  →`) downward. The captured pane also contains Claude Code's scrollback
// (prior prompts/output, many prefixed with `❯`) ABOVE the widget; scoping to the last tab
// row rejects it (an earlier bug scanned all lines and matched scrollback). Within the
// block we classify only FOCUSABLE rows by self-describing patterns — never by the
// model-provided question/option/header text (which is arbitrary and duplicable) — and the
// volatile status line below the widget falls through as `Other` and is ignored.

/// A focusable row inside the widget block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowKind {
    /// A numbered option row `N.` (0-based index = N-1). Real options only.
    Option(usize),
    /// The `Type something` free-text row.
    TypeSomething,
    /// The unnumbered advance row (`Next` for a non-final question, `Submit` for final).
    Advance,
    /// The `Chat about this` row.
    ChatAbout,
    /// The confirm screen's `1. Submit answers` row.
    ConfirmSubmit,
}

/// One parsed focusable row: its kind, whether the cursor (`❯`) is on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct WidgetRow {
    kind: RowKind,
    has_cursor: bool,
}

/// A parsed view of the live AskUserQuestion widget.
struct WidgetView {
    /// Count of `☒` (answered) markers in the tab row.
    answered_count: usize,
    /// True if the end-of-widget confirm screen is showing.
    is_confirm: bool,
    /// Focusable rows, in top-to-bottom order.
    rows: Vec<WidgetRow>,
}

impl WidgetView {
    /// The focusable row the cursor is currently on, if any.
    fn cursor_row(&self) -> Option<RowKind> {
        self.rows.iter().find(|r| r.has_cursor).map(|r| r.kind)
    }
    /// A stable signature of the screen — the answered-tab count plus the focusable rows
    /// (kinds + which has the cursor) — used to detect a screen transition independently of
    /// the volatile status line / clock. Including `answered_count` adds a second change
    /// signal (e.g. a single-select advancing flips its tab `☐`→`☒`).
    fn signature(&self) -> String {
        let mut s = format!("a{}|", self.answered_count);
        for r in &self.rows {
            s.push_str(&format!("{:?}{};", r.kind, r.has_cursor as u8));
        }
        s
    }
}

/// Is this line the widget's tab row (`←  ☐/☒ …  ✔ Submit  →`)? Requires the `←`, the
/// `✔ <LABEL_SUBMIT>` tab, and the trailing `→` so stray scrollback text can't pose as one.
fn is_tab_row(line: &str) -> bool {
    line.contains('\u{2190}') // ←
        && line.contains('\u{2192}') // →
        && line.contains(&format!("\u{2714} {LABEL_SUBMIT}")) // ✔ Submit
}

/// ADR-015: Parse the live widget block (anchored on the BOTTOM-MOST tab row). Returns
/// `None` if no tab row is present (no live widget). Pure; unit-tested without a tmux.
fn parse_widget(pane: &str) -> Option<WidgetView> {
    let lines: Vec<&str> = pane.lines().collect();
    // Bottom-most tab row wins (the live render sits below any scrollback tab text).
    let tab_idx = lines.iter().rposition(|l| is_tab_row(l))?;
    let tab_line = lines[tab_idx];
    let answered_count = tab_line.matches('\u{2612}').count(); // ☒
    let block = &lines[tab_idx + 1..];
    let is_confirm = block.iter().any(|l| l.contains(LABEL_CONFIRM_PROMPT));

    let mut rows: Vec<WidgetRow> = Vec::new();
    for &raw in block {
        // Stop at the footer (question screen) so the status line below never parses.
        if raw.contains("to cancel") || raw.contains("Tab/Arrow keys to navigate") {
            break;
        }
        let has_cursor = raw.contains(CURSOR_MARKER);
        let stripped = raw.replace(CURSOR_MARKER, "");
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Order matters: the text-labelled rows (Type something / Chat about this /
        // Submit answers) are ALSO numbered in some screens, so match them before the
        // generic numbered-option pattern.
        let kind = if trimmed.contains(LABEL_SUBMIT_ANSWERS) {
            Some(RowKind::ConfirmSubmit)
        } else if trimmed.contains(LABEL_TYPE_SOMETHING) {
            Some(RowKind::TypeSomething)
        } else if trimmed.contains(LABEL_CHAT_ABOUT) {
            Some(RowKind::ChatAbout)
        } else if trimmed == LABEL_NEXT || trimmed == LABEL_SUBMIT {
            Some(RowKind::Advance)
        } else {
            // A numbered option row, or a description/separator/status line (→ None).
            parse_option_number(trimmed).map(|n| RowKind::Option(n - 1))
        };
        if let Some(kind) = kind {
            rows.push(WidgetRow { kind, has_cursor });
        }
    }

    Some(WidgetView {
        answered_count,
        is_confirm,
        rows,
    })
}

/// Parse the leading 1-based option number from a row like `1. [ ] Foo` / `2. Bar`.
/// Strict: digits, a dot, then whitespace and a non-space — so status-line tokens
/// (`$181.46`, `5/5`) never match.
fn parse_option_number(trimmed: &str) -> Option<usize> {
    let dot = trimmed.find('.')?;
    let (num, rest) = trimmed.split_at(dot);
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = &rest[1..]; // skip '.'
    if !rest.starts_with(' ') || rest.trim().is_empty() {
        return None;
    }
    num.parse::<usize>().ok().filter(|&n| n >= 1)
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
        /// ADR-015 v4: the callback's `q_idx`/`o_idx` are out of range for the CURRENT
        /// pending question — a STALE button (e.g. an old question's message tapped after a
        /// same-key supersede). Treat as a graceful no-op instead of panicking on a direct
        /// `pending.questions[q_idx]` index.
        Stale,
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
        } else if pending
            .questions
            .get(q_idx)
            .is_none_or(|q| o_idx >= q.options.len())
        {
            // Stale/out-of-range callback — do NOT index directly below.
            AnswerOutcome::Stale
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
        AnswerOutcome::Stale => {
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some("This option is no longer available"), true)
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
        /// ADR-015 v4: a collected answer failed validation (out-of-range option index, or
        /// an empty multi-select with no free-text). Carries the user-facing reason; the
        /// entry stays Active (no injection attempted).
        BadSelection(String),
        NoTmux,
        Proceed {
            answers: Vec<InjItem>,
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
            // ADR-015 v4: collect + VALIDATE all answers BEFORE claiming ownership. On any
            // invalid/missing answer we DON'T transition to Submitting — the entry stays
            // Active and the user is told to fix it (see `collect_and_validate_answers`).
            match collect_and_validate_answers(&pending.questions, &pending.tentative) {
                Err(reason) => SubmitAllOutcome::BadSelection(reason),
                Ok(answers) => {
                    // Claim ownership: Active → Submitting. resolve_pending_question now
                    // no-ops, and the map entry is NOT yet removed (a duplicate tap hits
                    // Submitting).
                    pending.lifecycle = QuestionLifecycle::Submitting;

                    SubmitAllOutcome::Proceed {
                        answers,
                        session_id: pending.session_id.clone(),
                        question_message_ids: pending.question_message_ids.clone(),
                        summary_message_id: pending.summary_message_id,
                    }
                }
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
        SubmitAllOutcome::BadSelection(reason) => {
            // Entry stays Active; user can fix the selection and resubmit.
            let _ = ctx
                .bot
                .answer_callback_query(&cb.id, Some(&reason), true)
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

    // ADR-015 v4: drive the answers into Claude's native CLI widget via tmux. The typed
    // outcome decides recovery: only `Success` finalizes ("Submitted", remove entry);
    // `FailedClean` (nothing landed) safely restores Active for retry; `FailedDirty`
    // (≥1 keystroke landed → live widget partially advanced) terminalizes WITHOUT retry —
    // a blind re-drive from Q0 would corrupt the half-advanced widget.
    let outcome = inject_answers(ctx, &target, socket.as_deref(), &answers).await;

    match outcome {
        InjectOutcome::FailedClean => {
            tracing::warn!(
                session_id = %session_id,
                "ADR-015 v4: injection FailedClean (no keystroke acked) — restoring entry to Active"
            );
            // Nothing was delivered — restore ownership so the question is answerable
            // again. Only revert if we still hold it as Submitting.
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
                    "\u{26A0}\u{FE0F} Couldn't deliver the answers to the terminal (nothing was entered). Answer at the terminal, or tap Submit again to retry.",
                    None,
                    thread_id,
                )
                .await;
            return;
        }
        InjectOutcome::FailedDirty => {
            tracing::warn!(
                session_id = %session_id,
                "ADR-015 v4: injection FailedDirty (≥1 keystroke landed) — terminalizing; user to finish at terminal"
            );
            // The live widget is partially advanced — retry-from-Q0 is unsafe. Terminalize:
            // mark Resolved + remove from the map so no further Telegram action targets a
            // half-answered widget, and leave the live widget on screen for the user.
            {
                let mut pending = entry.lock().await;
                if pending.lifecycle == QuestionLifecycle::Submitting {
                    pending.lifecycle = QuestionLifecycle::Resolved;
                    for f in pending.finalized.iter_mut() {
                        *f = true;
                    }
                }
            }
            {
                let mut pq = ctx.pending_q.write().await;
                if pq
                    .get(&full_key)
                    .is_some_and(|cur| Arc::ptr_eq(cur, &entry))
                {
                    pq.remove(&full_key);
                }
            }
            for it in &answers {
                let mid = question_message_ids.get(it.q_idx).copied().unwrap_or(0);
                if mid != 0 {
                    let _ = ctx
                        .bot
                        .edit_message_text_no_markup(mid, "\u{26A0}\u{FE0F} Finish at the terminal")
                        .await;
                }
            }
            if let Some(mid) = summary_message_id {
                let _ = ctx
                    .bot
                    .edit_message(
                        chat_id,
                        mid,
                        "\u{26A0}\u{FE0F} Partially submitted — please finish answering at the terminal.",
                        None,
                    )
                    .await;
            }
            ctx.bot
                .send_message(
                    "\u{26A0}\u{FE0F} I may have partially answered at the terminal, so I stopped to avoid corrupting the prompt. Please finish answering in the Claude Code terminal \u{2014} the question is still on screen there.",
                    None,
                    thread_id,
                )
                .await;
            return;
        }
        InjectOutcome::Success => { /* fall through to finalize */ }
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

    // ADR-015 v4: inject_answers already drove the widget's own Submit/confirm, so the
    // questions are submitted — do NOT fire another Enter here (it would land on the
    // now-empty Claude prompt as a stray keystroke).

    // Edit each question message to show "Submitted" and strip keyboard.
    for it in &answers {
        let mid = question_message_ids.get(it.q_idx).copied().unwrap_or(0);
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

/// ADR-015 v4: one question's collected answer plus the structural facts injection needs.
#[derive(Debug, PartialEq, Eq)]
struct InjItem {
    /// Question index (tab order); used by the caller for per-question message edits.
    q_idx: usize,
    /// Real option count (`q.options.len()`); bounds the down-only navigate loop.
    total_options: usize,
    /// The question's `multiSelect` flag — decides the ADVANCE mechanism (single-select
    /// auto-advances on commit; multi-select must navigate to the `Next`/`Submit` row).
    multi_select: bool,
    answer: CollectedAnswer,
}

/// ADR-015 v4: collect + validate the tentative answers for ALL questions into injection
/// items, in question order. `Err(reason)` (user-facing) if any question is unanswered, has
/// an out-of-range option index, an empty multi-select, or empty free-text — the caller then
/// leaves the entry `Active` without injecting.
///
/// Iterating by question index (NOT `tentative.len()`) is the real all-answered gate: a count
/// check can pass with a STALE/bad question-index key (after a same-key supersede) while a
/// real question is unanswered, which previously let injection finalize a partial/empty
/// submit as a (false) success. Out-of-range option indices from stale callbacks are rejected
/// (not silently filtered) so we never inject a selection the user didn't make.
fn collect_and_validate_answers(
    questions: &[QuestionDef],
    tentative: &HashMap<usize, TentativeAnswer>,
) -> std::result::Result<Vec<InjItem>, String> {
    let mut answers = Vec::with_capacity(questions.len());
    for (q_idx, q) in questions.iter().enumerate() {
        let total_options = q.options.len();
        let multi_select = q.multi_select;
        let answer = match tentative.get(&q_idx) {
            Some(TentativeAnswer::Option(o)) => {
                if *o >= total_options {
                    return Err(format!(
                        "Q{} selection is out of range — please re-select.",
                        q_idx + 1
                    ));
                }
                CollectedAnswer::Single(*o)
            }
            Some(TentativeAnswer::MultiOption(set)) => {
                if set.iter().any(|&i| i >= total_options) {
                    return Err(format!(
                        "Q{} has an out-of-range selection — please re-select.",
                        q_idx + 1
                    ));
                }
                let mut sorted: Vec<usize> = set.iter().copied().collect();
                sorted.sort_unstable();
                sorted.dedup();
                if sorted.is_empty() {
                    return Err(format!(
                        "Select at least one option (or type an answer) for Q{}.",
                        q_idx + 1
                    ));
                }
                CollectedAnswer::Multi(sorted)
            }
            Some(TentativeAnswer::FreeText(s)) => {
                if sanitize_freetext(s).is_empty() {
                    return Err(format!(
                        "Q{} answer is empty — please re-answer.",
                        q_idx + 1
                    ));
                }
                CollectedAnswer::FreeText(s.clone())
            }
            None => return Err("Please answer all questions first.".to_string()),
        };
        answers.push(InjItem {
            q_idx,
            total_options,
            multi_select,
            answer,
        });
    }
    Ok(answers)
}

/// Where to park the cursor inside the current question screen.
#[derive(Clone, Copy)]
enum RowTarget {
    Option(usize),
    TypeSomething,
    Advance,
}

fn row_is(kind: RowKind, target: RowTarget) -> bool {
    matches!(
        (kind, target),
        (RowKind::Option(a), RowTarget::Option(b)) if a == b
    ) || matches!(
        (kind, target),
        (RowKind::TypeSomething, RowTarget::TypeSomething) | (RowKind::Advance, RowTarget::Advance)
    )
}

/// Map a `mutated` flag to a typed failure outcome (see [`InjectOutcome`]).
fn fail_outcome(mutated: bool) -> InjectOutcome {
    if mutated {
        InjectOutcome::FailedDirty
    } else {
        InjectOutcome::FailedClean
    }
}

/// Sanitize free-text before literal injection: drop control chars (incl. newlines/CR,
/// which would prematurely submit) and cap the length (FR32-aligned).
fn sanitize_freetext(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(FREETEXT_MAX_CHARS)
        .collect()
}

/// Capture the live pane and parse the widget view (`None` if no widget / capture failed).
fn capture_view(inj: &crate::injector::InputInjector) -> Option<WidgetView> {
    let pane = inj.capture_pane()?;
    parse_widget(&pane)
}

/// Send a whitelisted key; on tmux ack set `mutated` and pace for the Ink repaint.
/// Returns false on any non-`Ok(true)` (fail closed). A Down/Up navigation key counts as
/// a mutation (conservative): once any key is acknowledged, a later failure is "dirty".
async fn press(inj: &crate::injector::InputInjector, key: &str, mutated: &mut bool) -> bool {
    match inj.send_key(key) {
        Ok(true) => {
            *mutated = true;
            tokio::time::sleep(tokio::time::Duration::from_millis(KEY_DELAY_MS)).await;
            true
        }
        _ => false,
    }
}

/// Type literal free-text (no trailing Enter) into the focused `Type something` row.
async fn type_text(inj: &crate::injector::InputInjector, text: &str, mutated: &mut bool) -> bool {
    match inj.inject_literal(text) {
        Ok(true) => {
            *mutated = true;
            tokio::time::sleep(tokio::time::Duration::from_millis(KEY_DELAY_MS)).await;
            true
        }
        _ => false,
    }
}

/// ADR-015 v4: walk the cursor (down-only) onto `target`, verifying via `capture_pane`
/// after each step. Down-only is sufficient because the widget opens each question with
/// the cursor on option 1 and we visit targets in ascending order (options sorted, then
/// the lower `Type something`/`Advance` rows). Bounded by `total_options + NAV_SLACK_STEPS`
/// so a clipped long list still resolves (Ink scrolls the focused row into view as we
/// step). Returns false on any miss/timeout/`None` capture — NO best-effort key is ever
/// fired (that was the old key-leak bug). `mutated` is set as soon as any Down is acked.
async fn place_cursor_on(
    inj: &crate::injector::InputInjector,
    target: RowTarget,
    total_options: usize,
    mutated: &mut bool,
) -> bool {
    let interval = tokio::time::Duration::from_millis(READY_POLL_INTERVAL_MS);
    let max_steps = total_options + NAV_SLACK_STEPS;
    for step in 0..=max_steps {
        match capture_view(inj) {
            Some(view) => {
                if view.cursor_row().is_some_and(|k| row_is(k, target)) {
                    return true;
                }
            }
            None => return false, // capture failed → fail closed
        }
        if step == max_steps {
            break;
        }
        if !press(inj, "Down", mutated).await {
            return false;
        }
        tokio::time::sleep(interval).await;
    }
    false
}

/// Wait until a question screen with at least one focusable row is rendered.
async fn wait_for_widget(inj: &crate::injector::InputInjector) -> bool {
    let interval = tokio::time::Duration::from_millis(READY_POLL_INTERVAL_MS);
    for _ in 0..WAIT_STATE_MAX_POLLS {
        if capture_view(inj).is_some_and(|v| !v.rows.is_empty()) {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}

/// Wait until the widget's focusable-row signature DIFFERS from `pre_sig` (captured right
/// before the advancing keystroke) — i.e. the widget advanced to the next question. The
/// signature excludes the volatile status line/clock, so only a real screen change trips
/// it. Fail closed on timeout.
async fn wait_for_transition(inj: &crate::injector::InputInjector, pre_sig: Option<&str>) -> bool {
    let interval = tokio::time::Duration::from_millis(READY_POLL_INTERVAL_MS);
    for _ in 0..WAIT_STATE_MAX_POLLS {
        if let Some(v) = capture_view(inj) {
            if !v.rows.is_empty() && Some(v.signature().as_str()) != pre_sig {
                return true;
            }
        }
        tokio::time::sleep(interval).await;
    }
    false
}

/// Wait until the end-of-widget confirm screen is showing AND the cursor is on the
/// `Submit answers` row (its default). Returns true only when both hold, so the caller's
/// subsequent Enter can never be a blind press / land on `Cancel`. Fail closed on timeout.
async fn wait_for_confirm(inj: &crate::injector::InputInjector) -> bool {
    let interval = tokio::time::Duration::from_millis(READY_POLL_INTERVAL_MS);
    for _ in 0..WAIT_STATE_MAX_POLLS {
        if let Some(v) = capture_view(inj) {
            if v.is_confirm && v.cursor_row() == Some(RowKind::ConfirmSubmit) {
                return true;
            }
        }
        tokio::time::sleep(interval).await;
    }
    false
}

/// ADR-015 v4: inject the collected answers into Claude's native AskUserQuestion widget
/// (ONE tabbed widget, processed in question order), entirely driven by reading the live
/// pane. See `docs/adr/ADR-015` and the captured state machine for the model.
///
/// Per question: place the cursor on each target row (verified) and press Enter — a
/// single-select selection (or a free-text commit) AUTO-ADVANCES; a multi-select toggles
/// each option then navigates to the `Next`/`Submit` advance row + Enter. After the final
/// question, when a confirm screen is expected (N≥2, or N=1 multi-select), wait for it and
/// press Enter on `Submit answers`. No keystroke is ever fired blindly.
///
/// Returns [`InjectOutcome`]: `Success`; `FailedClean` (nothing was acknowledged — safe to
/// retry / restore Active); or `FailedDirty` (≥1 key landed — the live widget is partially
/// advanced; caller must terminalize, not retry).
async fn inject_answers(
    ctx: &HandlerContext,
    target: &str,
    socket: Option<&str>,
    answers: &[InjItem],
) -> InjectOutcome {
    let mut inj = ctx.injector.lock().await;
    inj.set_target(target, socket);

    let n = answers.len();
    let any_multi = answers.iter().any(|it| it.multi_select);
    let mut mutated = false;

    // Ensure the widget is on screen before the first keystroke.
    if !wait_for_widget(&inj).await {
        return fail_outcome(mutated);
    }

    for (i, item) in answers.iter().enumerate() {
        let is_last = i == n - 1;
        let max = item.total_options;

        // Position the cursor for this question's answer; leave it on the row whose Enter
        // ADVANCES the widget (the selected option / the committed Type-something row for
        // single-select; the Next/Submit advance row for multi-select).
        match &item.answer {
            CollectedAnswer::Single(idx) => {
                if !place_cursor_on(&inj, RowTarget::Option(*idx), max, &mut mutated).await {
                    return fail_outcome(mutated);
                }
            }
            CollectedAnswer::Multi(idxs) => {
                for &idx in idxs {
                    if !place_cursor_on(&inj, RowTarget::Option(idx), max, &mut mutated).await {
                        return fail_outcome(mutated);
                    }
                    if !press(&inj, "Enter", &mut mutated).await {
                        return fail_outcome(mutated); // toggle
                    }
                }
                if !place_cursor_on(&inj, RowTarget::Advance, max, &mut mutated).await {
                    return fail_outcome(mutated);
                }
            }
            CollectedAnswer::FreeText(text) => {
                if !place_cursor_on(&inj, RowTarget::TypeSomething, max, &mut mutated).await {
                    return fail_outcome(mutated);
                }
                if !type_text(&inj, &sanitize_freetext(text), &mut mutated).await {
                    return fail_outcome(mutated);
                }
                // Multi-select free-text does NOT auto-advance — navigate to the advance
                // row. Single-select free-text commits+advances on the common Enter below.
                if item.multi_select
                    && !place_cursor_on(&inj, RowTarget::Advance, max, &mut mutated).await
                {
                    return fail_outcome(mutated);
                }
            }
        }

        // The advancing Enter (selects+advances / commits+advances / activates Next/Submit).
        // Snapshot the signature first so we can verify the advance actually happened.
        let pre_sig = capture_view(&inj).map(|v| v.signature());
        if !press(&inj, "Enter", &mut mutated).await {
            return fail_outcome(mutated);
        }

        if is_last {
            // End of widget. A confirm screen appears for any N≥2 (incl. all-single-select)
            // and for N=1 multi-select. N=1 single-select submits directly (no confirm).
            if n >= 2 || any_multi {
                if !wait_for_confirm(&inj).await {
                    return InjectOutcome::FailedDirty; // we pressed Enter → dirty
                }
                if !press(&inj, "Enter", &mut mutated).await {
                    return InjectOutcome::FailedDirty;
                }
            }
        } else if !wait_for_transition(&inj, pre_sig.as_deref()).await {
            return InjectOutcome::FailedDirty;
        }
    }

    InjectOutcome::Success
}

#[cfg(test)]
mod tests {
    use super::*;

    // ───────────────────────── fixtures from REAL captured frames ─────────────────────────
    // Captured live via tmux capture-pane against Claude Code 2.1.159 (N=2/N=3, single +
    // multi + free-text + confirm). Each includes `❯`-prefixed scrollback above the tab row
    // and the RuFlo status line below the footer, to exercise widget-scoping.

    /// Multi-select question (N=3 "Keys"), cursor on option 3. Two prior tabs answered (☒).
    const PANE_MULTI_OPT: &str = "\
  some earlier tool output
❯ a prior user prompt in scrollback
←  ☒ Quarter  ☒ Keys  ☐ Goals  ✔ Submit  →
Keys to beating Utah? (MULTI-SELECT)
  1. [✔] Run defense
    Stop the ground game
  2. [✔] Win turnovers
    Turnover margin
❯ 3. [ ] QB poise
    Composed quarterback play
  4. [ ] Special teams
    Field position and kicks
  5. [ ] Type something
     Next
  6. Chat about this
Enter to select · Tab/Arrow keys to navigate · Esc to cancel
  ▊ RuFlo V3.10.31 ● Robert E. Lee  │  Opus 4.8  │  ● 16% ctx  │  $181.46";

    /// Same multi-select screen, cursor walked onto the `Next` advance row.
    const PANE_MULTI_ADVANCE: &str = "\
←  ☒ Quarter  ☒ Keys  ☐ Goals  ✔ Submit  →
  1. [✔] Run defense
  2. [✔] Win turnovers
  3. [ ] QB poise
  4. [ ] Special teams
  5. [ ] Type something
❯    Next
  6. Chat about this
Enter to select · Tab/Arrow keys to navigate · Esc to cancel";

    /// Single-select question (N=3 "Quarter"), cursor on option 2. No checkboxes, and NO
    /// advance row (single-select auto-advances on selection).
    const PANE_SINGLE_OPT: &str = "\
←  ☒ Quarter  ☐ Keys  ☐ Goals  ✔ Submit  →
Which quarter decides the season? (SINGLE-SELECT)
  1. 1st half
❯ 2. 3rd quarter
  3. 4th quarter
  4. Type something.
  5. Chat about this
Enter to select · Tab/Arrow keys to navigate · Esc to cancel";

    /// Multi-select with the cursor on the `Type something` row BEFORE typing (free-text).
    const PANE_FREETEXT_PRE: &str = "\
←  ☒ FreeText  ☐ Finish  ✔ Submit  →
  1. [ ] Option A
  2. [ ] Option B
  3. [ ] Option C
❯ 4. [ ] Type something
     Next
  5. Chat about this
Enter to select · Tab/Arrow keys to navigate · Esc to cancel";

    /// The end-of-widget confirm screen, cursor defaulting to `Submit answers`. Note the
    /// status line below has NO footer (clipped) — parsing must still classify correctly.
    const PANE_CONFIRM: &str = "\
←  ☒ Quarter  ☒ Keys  ☒ Goals  ✔ Submit  →
Review your answers
 ● Which quarter? → 3rd quarter
 ● Keys? → Run defense, Win turnovers
 ● Goals? → Win Big 12, 10+ wins
Ready to submit your answers?
❯ 1. Submit answers
  2. Cancel
  ▊ RuFlo V3.10.31 ● Robert E. Lee  │  Opus 4.8  │  $181.46";

    #[test]
    fn parse_none_without_tab_row() {
        assert!(parse_widget("just some\nscrollback text\n❯ a prompt").is_none());
        assert!(parse_widget("").is_none());
    }

    #[test]
    fn parse_multi_option_screen() {
        let v = parse_widget(PANE_MULTI_OPT).expect("widget present");
        assert_eq!(v.answered_count, 2); // Quarter + Keys are ☒
        assert!(!v.is_confirm);
        assert_eq!(v.cursor_row(), Some(RowKind::Option(2))); // ❯ on "3. QB poise"
                                                              // Focusable rows: 4 options + Type something + Advance + Chat about this.
        assert!(v.rows.iter().any(|r| r.kind == RowKind::Option(0)));
        assert!(v.rows.iter().any(|r| r.kind == RowKind::Option(3)));
        assert!(v.rows.iter().any(|r| r.kind == RowKind::TypeSomething));
        assert!(v.rows.iter().any(|r| r.kind == RowKind::Advance));
        assert!(v.rows.iter().any(|r| r.kind == RowKind::ChatAbout));
        // The status line below the footer must NOT have been parsed (no spurious option).
        assert!(!v.rows.iter().any(|r| r.kind == RowKind::ConfirmSubmit));
        // "Type something" (row 5) is classified as TypeSomething, NOT Option(4).
        assert!(!v.rows.iter().any(|r| r.kind == RowKind::Option(4)));
    }

    #[test]
    fn parse_multi_advance_cursor() {
        let v = parse_widget(PANE_MULTI_ADVANCE).expect("widget present");
        assert_eq!(v.cursor_row(), Some(RowKind::Advance));
        assert!(!v.is_confirm);
    }

    #[test]
    fn parse_single_select_screen() {
        let v = parse_widget(PANE_SINGLE_OPT).expect("widget present");
        assert_eq!(v.answered_count, 1);
        assert_eq!(v.cursor_row(), Some(RowKind::Option(1))); // ❯ on "2. 3rd quarter"
        assert!(v.rows.iter().any(|r| r.kind == RowKind::TypeSomething));
        // Single-select has NO advance row.
        assert!(!v.rows.iter().any(|r| r.kind == RowKind::Advance));
    }

    #[test]
    fn parse_freetext_type_something_cursor() {
        let v = parse_widget(PANE_FREETEXT_PRE).expect("widget present");
        // Cursor on the (still-empty) "Type something" row → classified TypeSomething,
        // not Option(3), so place_cursor_on(TypeSomething) lands correctly.
        assert_eq!(v.cursor_row(), Some(RowKind::TypeSomething));
    }

    #[test]
    fn parse_confirm_screen() {
        let v = parse_widget(PANE_CONFIRM).expect("widget present");
        assert!(v.is_confirm);
        assert_eq!(v.answered_count, 3);
        assert_eq!(v.cursor_row(), Some(RowKind::ConfirmSubmit));
    }

    #[test]
    fn bottom_most_tab_row_wins() {
        // A stale earlier widget's tab row sits in scrollback ABOVE the live one. Parsing
        // must anchor on the LAST (live) tab row — answered_count and rows come from it.
        let pane = format!(
            "←  ☒ OldA  ☐ OldB  ✔ Submit  →\n  1. stale option\n❯ stale prompt\n{PANE_MULTI_OPT}"
        );
        let v = parse_widget(&pane).expect("widget present");
        assert_eq!(v.answered_count, 2); // from the LIVE tab row, not the stale (1 ☒)
        assert_eq!(v.cursor_row(), Some(RowKind::Option(2)));
    }

    #[test]
    fn scrollback_above_tab_row_excluded() {
        // `❯`-prefixed scrollback prompts above the tab row must not become focusable rows
        // nor steal the cursor (the old all-lines-scan bug). A scrollback line that even
        // contains the confirm phrase must not flip is_confirm for a question screen.
        let pane = format!(
            "❯ yes, both\n❯ Ready to submit your answers? (in my chat text)\n{PANE_MULTI_OPT}"
        );
        let v = parse_widget(&pane).expect("widget present");
        assert!(!v.is_confirm);
        assert_eq!(v.cursor_row(), Some(RowKind::Option(2)));
    }

    #[test]
    fn parse_option_number_strictness() {
        assert_eq!(parse_option_number("1. Foo"), Some(1));
        assert_eq!(parse_option_number("12. Bar baz"), Some(12));
        assert_eq!(parse_option_number("3. [ ] Toggle"), Some(3));
        // Status-line / non-option tokens must NOT parse as options.
        assert_eq!(parse_option_number("$181.46"), None);
        assert_eq!(parse_option_number("5/5"), None);
        assert_eq!(parse_option_number("● 16% ctx"), None);
        assert_eq!(parse_option_number("1."), None); // no trailing content
        assert_eq!(parse_option_number("1.no space"), None);
        assert_eq!(parse_option_number("v1.2"), None);
    }

    #[test]
    fn signature_distinguishes_screens() {
        let a = parse_widget(PANE_MULTI_OPT).unwrap().signature();
        let b = parse_widget(PANE_SINGLE_OPT).unwrap().signature();
        let c = parse_widget(PANE_MULTI_ADVANCE).unwrap().signature();
        assert_ne!(a, b);
        assert_ne!(a, c); // same screen, different cursor row → different signature
    }

    #[test]
    fn row_is_matching() {
        assert!(row_is(RowKind::Option(2), RowTarget::Option(2)));
        assert!(!row_is(RowKind::Option(2), RowTarget::Option(3)));
        assert!(row_is(RowKind::TypeSomething, RowTarget::TypeSomething));
        assert!(row_is(RowKind::Advance, RowTarget::Advance));
        assert!(!row_is(RowKind::Advance, RowTarget::Option(0)));
        assert!(!row_is(RowKind::ChatAbout, RowTarget::TypeSomething));
    }

    #[test]
    fn is_tab_row_strictness() {
        assert!(is_tab_row("←  ☒ Quarter  ☐ Keys  ✔ Submit  →"));
        // Missing one of ← / ✔ Submit / → → not a tab row.
        assert!(!is_tab_row("☒ Quarter  ✔ Submit"));
        assert!(!is_tab_row("← just an arrow → with no submit tab"));
        assert!(!is_tab_row("I'll Submit the answers →"));
    }

    #[test]
    fn sanitize_freetext_strips_and_caps() {
        // Newlines/control chars stripped (would otherwise prematurely submit).
        assert_eq!(sanitize_freetext("hi\nthere\t!"), "hithere!");
        assert_eq!(sanitize_freetext("a\r\nb"), "ab");
        // Length capped.
        let long = "x".repeat(FREETEXT_MAX_CHARS + 50);
        assert_eq!(sanitize_freetext(&long).chars().count(), FREETEXT_MAX_CHARS);
        // Ordinary text untouched.
        assert_eq!(sanitize_freetext("WR1"), "WR1");
    }

    #[test]
    fn fail_outcome_maps_mutated() {
        assert_eq!(fail_outcome(false), InjectOutcome::FailedClean);
        assert_eq!(fail_outcome(true), InjectOutcome::FailedDirty);
    }

    // ───────────────────── collect_and_validate_answers (Codex diff review) ─────────────────
    use std::collections::HashMap as TMap;
    use std::collections::HashSet as TSet;

    fn qdef(header: &str, n_options: usize, multi: bool) -> QuestionDef {
        QuestionDef {
            question: format!("{header}?"),
            header: header.to_string(),
            options: (0..n_options)
                .map(|i| OptionDef {
                    label: format!("opt{i}"),
                    description: String::new(),
                })
                .collect(),
            multi_select: multi,
        }
    }

    #[test]
    fn collect_validates_happy_path_mixed() {
        let questions = vec![qdef("Single", 3, false), qdef("Multi", 4, true)];
        let mut t: TMap<usize, TentativeAnswer> = TMap::new();
        t.insert(0, TentativeAnswer::Option(2));
        t.insert(1, TentativeAnswer::MultiOption(TSet::from([0, 2])));
        let got = collect_and_validate_answers(&questions, &t).expect("valid");
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0],
            InjItem {
                q_idx: 0,
                total_options: 3,
                multi_select: false,
                answer: CollectedAnswer::Single(2)
            }
        );
        assert_eq!(
            got[1],
            InjItem {
                q_idx: 1,
                total_options: 4,
                multi_select: true,
                answer: CollectedAnswer::Multi(vec![0, 2]) // sorted + deduped
            }
        );
    }

    #[test]
    fn collect_rejects_unanswered_question() {
        // Two questions but only Q0 answered — must NOT silently drop Q1 (the old bug that
        // let a partial/empty submit finalize as success). Also covers a stale/bad key:
        // a tentative for index 5 doesn't satisfy the real question at index 1.
        let questions = vec![qdef("A", 2, false), qdef("B", 2, false)];
        let mut t: TMap<usize, TentativeAnswer> = TMap::new();
        t.insert(0, TentativeAnswer::Option(0));
        t.insert(5, TentativeAnswer::Option(1)); // stale/out-of-range key
        let err = collect_and_validate_answers(&questions, &t).unwrap_err();
        assert!(err.contains("answer all questions"));
    }

    #[test]
    fn collect_rejects_out_of_range_indices() {
        let questions = vec![qdef("Single", 3, false)];
        let mut t: TMap<usize, TentativeAnswer> = TMap::new();
        t.insert(0, TentativeAnswer::Option(3)); // valid indices are 0..=2
        assert!(collect_and_validate_answers(&questions, &t)
            .unwrap_err()
            .contains("out of range"));

        let questions = vec![qdef("Multi", 3, true)];
        let mut t: TMap<usize, TentativeAnswer> = TMap::new();
        t.insert(0, TentativeAnswer::MultiOption(TSet::from([0, 9]))); // 9 is out of range
        assert!(collect_and_validate_answers(&questions, &t)
            .unwrap_err()
            .contains("out-of-range"));
    }

    #[test]
    fn collect_rejects_empty_multiselect_and_freetext() {
        let questions = vec![qdef("Multi", 3, true)];
        let mut t: TMap<usize, TentativeAnswer> = TMap::new();
        t.insert(0, TentativeAnswer::MultiOption(TSet::new())); // nothing picked
        assert!(collect_and_validate_answers(&questions, &t)
            .unwrap_err()
            .contains("at least one"));

        let questions = vec![qdef("FT", 2, false)];
        let mut t: TMap<usize, TentativeAnswer> = TMap::new();
        t.insert(0, TentativeAnswer::FreeText("\n\t".to_string())); // sanitizes to empty
        assert!(collect_and_validate_answers(&questions, &t)
            .unwrap_err()
            .contains("empty"));
    }

    #[test]
    fn collect_freetext_on_multiselect_is_accepted() {
        // Free-text answers a multi-select question (carried with multi_select=true so the
        // injector navigates to the advance row rather than auto-advancing).
        let questions = vec![qdef("Multi", 3, true)];
        let mut t: TMap<usize, TentativeAnswer> = TMap::new();
        t.insert(0, TentativeAnswer::FreeText("frogs".to_string()));
        let got = collect_and_validate_answers(&questions, &t).expect("valid");
        assert_eq!(
            got[0],
            InjItem {
                q_idx: 0,
                total_options: 3,
                multi_select: true,
                answer: CollectedAnswer::FreeText("frogs".to_string())
            }
        );
    }
}
