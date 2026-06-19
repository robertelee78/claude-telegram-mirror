//! Handlers for socket/bridge messages (session lifecycle, tool events, agent responses, etc.).

use super::*;

/// ADR-013 GAP-3: Check if a session is a child (sub-agent) and return its label.
/// Returns Some("🤖 [Agent: {agent_id}] ") if the session has a parent_session_id.
async fn get_child_prefix(ctx: &HandlerContext, session_id: &str) -> Option<String> {
    let sid = session_id.to_string();
    ctx.db_op(move |sess| {
        sess.get_session(&sid).ok().flatten().and_then(|s| {
            s.parent_session_id.as_ref()?; // Only for child sessions
            let agent_label = s
                .agent_type
                .as_deref()
                .or(s.agent_id.as_deref())
                .unwrap_or("sub-agent");
            Some(format!("\u{1F916} [{}] ", agent_label))
        })
    })
    .await
}

/// Handler 1: session_start
pub(super) async fn handle_session_start(ctx: &HandlerContext, msg: &BridgeMessage) {
    let meta = msg.meta();
    let hostname = meta.hostname();
    let project_dir = meta.project_dir();
    let tmux_target = meta.tmux_target();
    let tmux_socket = meta.tmux_socket();

    // Create session in DB -- all fields in a single atomic INSERT (M2.12).
    {
        let sid = msg.session_id.clone();
        let chat_id = ctx.config.chat_id;
        let h = hostname.map(|s| s.to_string());
        let pd = project_dir.map(|s| s.to_string());
        let tt = tmux_target.map(|s| s.to_string());
        let ts = tmux_socket.map(|s| s.to_string());
        ctx.db_op(move |sess| {
            let _ = sess.create_session(
                &sid,
                chat_id,
                h.as_deref(),
                pd.as_deref(),
                None, // thread_id assigned later via set_session_thread
                tt.as_deref(),
                ts.as_deref(),
            );
        })
        .await;
    }

    // Cache tmux target
    if let Some(target) = tmux_target {
        ctx.session_tmux
            .write()
            .await
            .insert(msg.session_id.clone(), target.to_string());
    }

    // BUG-012: Cancel pending topic deletion if session resumes
    cleanup::cancel_pending_topic_deletion(ctx, &msg.session_id).await;

    // Suppress topic creation for non-interactive sessions (claude -p, SDK, CI).
    if meta.is_non_interactive() {
        tracing::debug!(
            session_id = %msg.session_id,
            entrypoint = ?meta.entrypoint(),
            "Non-interactive session, suppressing topic creation"
        );
        return;
    }

    // ADR-013 Part B: Parent-child session routing.
    // GAP-8: Sub-agents share the parent's session_id and send agent_id in
    // the base event. When agent_id is present, this is a sub-agent message
    // on the parent's session — no new topic is needed.
    //
    // Also check transcript_path for /subagents/ pattern as a secondary signal.
    let transcript_path = meta.transcript_path();
    let path_parent_info: Option<(String, Option<String>)> = transcript_path.and_then(|tp| {
        let parent_id = crate::types::extract_parent_session_id(tp)?;
        let agent_id = crate::types::extract_agent_id(tp);
        Some((parent_id.to_string(), agent_id.map(|s| s.to_string())))
    });

    // GAP-8: If agent_id is present in metadata, this is a sub-agent on the
    // parent's session. The session already exists (it's the parent's session_id),
    // so the topic is already cached. Store the agent info for labeling.
    let meta_agent_id = meta.agent_id().map(|s| s.to_string());
    let meta_agent_type = meta.agent_type().map(|s| s.to_string());

    // Resolve parent_info and parent_thread_id together.
    // Strategy:
    //   1. If transcript_path heuristic found a parent, use GAP-4 retry logic for thread_id.
    //   2. Otherwise, no parent (GAP-7 temporal correlation removed — superseded by GAP-8).
    let (parent_info, parent_thread_id): (Option<(String, Option<String>)>, Option<i64>) =
        if let Some((ref parent_sid, ref agent_id)) = path_parent_info {
            // Path heuristic succeeded — use GAP-4 retry logic for parent thread_id.
            let psid = parent_sid.clone();
            let mut result = None;
            for attempt in 0..3u8 {
                // Check in-memory cache first
                let cached = ctx.session_threads.read().await.get(&psid).copied();
                if cached.is_some() {
                    result = cached;
                    break;
                }
                // Fall back to DB
                let psid2 = psid.clone();
                let db_result = ctx
                    .db_op(move |sess| {
                        sess.get_session(&psid2)
                            .ok()
                            .flatten()
                            .and_then(|s| s.thread_id)
                    })
                    .await;
                if db_result.is_some() {
                    result = db_result;
                    break;
                }
                if attempt < 2 {
                    tracing::debug!(
                        session_id = %msg.session_id,
                        parent_session_id = %psid,
                        attempt = attempt + 1,
                        "ADR-013 GAP-4: Parent thread_id not found, retrying in 500ms"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
            }
            if result.is_none() {
                tracing::warn!(
                    session_id = %msg.session_id,
                    parent_session_id = %psid,
                    "ADR-013 GAP-4: Parent thread_id not found after 3 attempts — child gets own topic"
                );
            }
            (Some((parent_sid.clone(), agent_id.clone())), result)
        } else {
            (None, None)
        };

    // Store parent info in DB if this is a child session
    if let Some((ref parent_sid, ref agent_id)) = parent_info {
        let sid = msg.session_id.clone();
        let psid = parent_sid.clone();
        // GAP-8: Prefer agent_id from metadata (base event) over transcript_path extraction
        let aid = meta_agent_id.clone().or_else(|| agent_id.clone());
        let at = meta_agent_type.clone();
        ctx.db_op(move |sess| {
            let _ = sess.set_parent_info(&sid, &psid, aid.as_deref(), at.as_deref());
        })
        .await;
        tracing::info!(
            session_id = %msg.session_id,
            parent_session_id = %parent_sid,
            agent_id = ?agent_id,
            parent_thread_id = ?parent_thread_id,
            "ADR-013: Child session detected, routing to parent topic"
        );

        // ADR-013 Part C: Send "Agent spawned" one-liner to parent topic.
        if let Some(ptid) = parent_thread_id {
            let display_id = agent_id
                .as_deref()
                .unwrap_or(&msg.session_id[..std::cmp::min(12, msg.session_id.len())]);
            // ADR-013 GAP-2: Include agent_type in spawn notification if available
            let agent_type_label = msg.meta().agent_type().unwrap_or("");
            let spawn_text = if agent_type_label.is_empty() {
                format!(
                    "\u{1F916} *Agent spawned:* `{}`",
                    escape_markdown_v1(display_id)
                )
            } else {
                format!(
                    "\u{1F916} *Agent spawned:* `{}` ({})",
                    escape_markdown_v1(display_id),
                    escape_markdown_v1(agent_type_label)
                )
            };
            ctx.bot
                .send_message(
                    &spawn_text,
                    Some(&SendOptions {
                        parse_mode: Some("Markdown".into()),
                        ..Default::default()
                    }),
                    Some(ptid),
                )
                .await;
        }
    }

    // Check if session already has a thread (daemon restart scenario)
    let existing_thread = {
        let sid = msg.session_id.clone();
        ctx.db_op(move |sess| {
            sess.get_session(&sid)
                .ok()
                .flatten()
                .and_then(|s| s.thread_id)
        })
        .await
    };

    // ADR-013: If parent_thread_id is available, use it instead of creating a new topic.
    let thread_id = if let Some(ptid) = parent_thread_id {
        // Reuse parent's thread — store it for this child session too
        let sid = msg.session_id.clone();
        ctx.db_op(move |sess| {
            if let Err(e) = sess.set_session_thread(&sid, ptid) {
                tracing::error!(session_id = %sid, thread_id = ptid, error = %e, "Failed to save thread_id to DB");
            }
        })
        .await;
        ctx.session_threads
            .write()
            .await
            .insert(msg.session_id.clone(), ptid);
        tracing::info!(
            session_id = %msg.session_id,
            thread_id = ptid,
            "ADR-013: Reusing parent session's thread for child"
        );
        Some(ptid)
    } else if let Some(tid) = existing_thread {
        ctx.session_threads
            .write()
            .await
            .insert(msg.session_id.clone(), tid);
        tracing::info!(session_id = %msg.session_id, thread_id = tid, "Reusing existing thread");
        Some(tid)
    } else if ctx.config.use_threads {
        // BUG-002: Acquire topic creation lock to prevent duplicate topics
        // from concurrent messages (e.g. ensure_session_exists racing us).
        let lock = {
            let mut locks = ctx.topic_locks.write().await;
            if let Some(state) = locks.get(&msg.session_id) {
                // Another task is already creating the topic -- wait for it.
                let notify = std::sync::Arc::clone(&state.notify);
                drop(locks);
                let _ =
                    tokio::time::timeout(tokio::time::Duration::from_secs(5), notify.notified())
                        .await;
                // Topic should now exist — read it back.
                ctx.get_thread_id(&msg.session_id).await
            } else {
                let state = std::sync::Arc::new(super::TopicCreationState {
                    notify: std::sync::Arc::new(tokio::sync::Notify::new()),
                });
                locks.insert(msg.session_id.clone(), state.clone());
                drop(locks);

                let topic_name =
                    HandlerContext::format_topic_name(&msg.session_id, hostname, project_dir);
                let color_index = msg
                    .session_id
                    .bytes()
                    .fold(0u32, |acc, b| acc.wrapping_add(b as u32))
                    as usize
                    % 6;
                let created = match ctx
                    .bot
                    .create_forum_topic_resilient(&topic_name, color_index)
                    .await
                {
                    Ok(Some(tid)) => {
                        let sid = msg.session_id.clone();
                        ctx.db_op(move |sess| {
                            if let Err(e) = sess.set_session_thread(&sid, tid) {
                                tracing::error!(session_id = %sid, thread_id = tid, error = %e, "Failed to save thread_id to DB");
                            }
                            // STALE-TOPICS: record in the persistent ledger so this topic can
                            // always be found and pruned later, even if the session row is
                            // ended / thread_id cleared / row deleted.
                            if let Err(e) = sess.record_topic(tid, &sid) {
                                tracing::warn!(session_id = %sid, thread_id = tid, error = %e, "STALE-TOPICS: failed to record topic in ledger");
                            }
                        })
                        .await;
                        ctx.session_threads
                            .write()
                            .await
                            .insert(msg.session_id.clone(), tid);
                        Some(tid)
                    }
                    _ => None,
                };

                // Resolve lock so waiters can proceed.
                {
                    let locks = ctx.topic_locks.read().await;
                    if let Some(state) = locks.get(&msg.session_id) {
                        state.notify.notify_waiters();
                    }
                }
                ctx.topic_locks.write().await.remove(&msg.session_id);

                created
            }
        };
        lock
    } else {
        None
    };

    // ADR-013 D3: Build session info with tmux status indicator.
    let mut session_info = format_session_start(&msg.session_id, project_dir, hostname);
    if let Some(target) = tmux_target {
        session_info.push_str(&format!("\n\u{1F7E2} tmux: connected (`{target}`)"));
    } else {
        session_info.push_str("\n\u{1F534} tmux: not detected \u{2014} replies disabled");
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

    // H3.2: Broadcast session_start back to socket clients so hook processes
    // can discover their session's assigned threadId.
    {
        let mut broadcast_meta = serde_json::Map::new();
        if let Some(tid) = thread_id {
            broadcast_meta.insert(
                "threadId".into(),
                serde_json::Value::Number(serde_json::Number::from(tid)),
            );
        }
        if let Some(h) = hostname {
            broadcast_meta.insert("hostname".into(), serde_json::Value::String(h.to_string()));
        }
        if let Some(d) = project_dir {
            broadcast_meta.insert(
                "projectDir".into(),
                serde_json::Value::String(d.to_string()),
            );
        }
        let broadcast_msg = BridgeMessage {
            msg_type: MessageType::SessionStart,
            session_id: msg.session_id.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: "session_start".into(),
            metadata: Some(broadcast_meta),
        };
        broadcast_to_clients(&ctx.socket_clients, &broadcast_msg).await;
    }

    // BUG-002: if a topic was assigned, flush any content events that were
    // buffered because they arrived before this session's topic existed.
    if thread_id.is_some() {
        super::flush_pending_for_session(ctx, &msg.session_id).await;
    }
}

/// Handler 2: session_end
///
/// Explicit session teardown: marks the session as ended, sends a summary to
/// Telegram, and schedules topic deletion when auto-delete is enabled. This is
/// the Rust equivalent of the TypeScript `handleSessionEnd()`.  See also
/// `ensure_session_exists` which handles the start (creation) side.
pub(super) async fn handle_session_end(ctx: &HandlerContext, msg: &BridgeMessage) {
    // ADR-014 A3: `session_exit_reason == "resume"` means the session is being
    // suspended and WILL come back. Tearing down here would wrongly destroy a
    // resuming session's topic, so skip teardown entirely. The reason is carried
    // in the message content (see hook.rs SessionEnd arm). Only true terminations
    // (clear, logout, prompt_input_exit, bypass_permissions_disabled, other,
    // unknown) fall through to teardown.
    if msg.content.trim() == "resume" {
        tracing::info!(
            session_id = %msg.session_id,
            "ADR-014 A3: SessionEnd reason=resume — suspending, not tearing down"
        );
        return;
    }

    let sid = msg.session_id.clone();
    let session_opt = ctx
        .db_op(move |sess| sess.get_session(&sid).ok().flatten())
        .await;

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
                // ADR-014 A4: Delete the topic IMMEDIATELY on a true SessionEnd
                // (replaces the two-stage close→delete schedule). Phone-side history
                // is not valued; the computer retains transcript history. Then
                // SYNCHRONOUSLY clear thread_id from BOTH the DB and the
                // session_threads cache — no fire-and-forget — before returning, so a
                // late/concurrent resume cannot read a stale thread_id for an
                // already-deleted topic (A5 atomic-clear).
                //
                // Defensive: cancel any pending two-stage deletion task. After this
                // change handle_session_end no longer schedules one, but a task could
                // still exist from the inactivity sweep (cleanup.rs).
                cleanup::cancel_pending_topic_deletion(ctx, &msg.session_id).await;

                // Always drop the in-memory routing cache — the session is ending and
                // must not keep receiving sends on the hot path.
                ctx.session_threads.write().await.remove(&msg.session_id);

                // STALE-TOPICS (leak fix): clear the DB thread_id + ledger entry ONLY when
                // the topic is confirmed gone. `Ok(true)` = deleted; `Ok(false)` = HTTP 400
                // (topic already gone / invalid id) — both mean the topic no longer exists.
                // `Err` = a TRANSIENT failure (429 exhausted / 5xx / network): RETAIN the
                // thread_id so `cleanup_orphaned_threads` retries the delete. The previous
                // code cleared thread_id BEFORE deleting and on every outcome — which is
                // exactly how a failed/crashed delete left a permanent, unrecoverable
                // row-less orphan topic. The persistent ledger (kept on `Err`) is the
                // backstop so even a never-retried orphan stays findable by prune-topics.
                match ctx.bot.delete_forum_topic(tid).await {
                    Ok(_) => {
                        let sid_clear = msg.session_id.clone();
                        ctx.db_op(move |sess| {
                            let _ = sess.clear_thread_id(&sid_clear);
                            let _ = sess.forget_topic(tid);
                        })
                        .await;
                        tracing::info!(
                            session_id = %msg.session_id,
                            thread_id = tid,
                            "ADR-014 A4: Forum topic deleted/confirmed-gone on SessionEnd"
                        );
                    }
                    Err(e) => tracing::warn!(
                        session_id = %msg.session_id,
                        thread_id = tid,
                        error = %e,
                        "STALE-TOPICS: topic delete failed (transient) — retaining thread_id + ledger for retry"
                    ),
                }
            } else {
                let _ = ctx.bot.close_forum_topic(tid).await;
                ctx.session_threads.write().await.remove(&msg.session_id);
            }
        }

        // Clean up caches
        ctx.session_tmux.write().await.remove(&msg.session_id);
        ctx.custom_titles.write().await.remove(&msg.session_id);
        // BUG-002: drop any never-flushed buffered events so they cannot leak.
        ctx.pending_topic_msgs.write().await.remove(&msg.session_id);
        cleanup_pending_questions(ctx, &msg.session_id).await;

        // ADR-014 B4: capture this session's pending approval IDs BEFORE end_session
        // expires them, so we can evict their client-map entries afterward.
        let sid_pa = msg.session_id.clone();
        let approval_ids = ctx
            .db_op(move |sess| sess.get_pending_approvals(&sid_pa).unwrap_or_default())
            .await;

        let sid = msg.session_id.clone();
        ctx.db_op(move |sess| {
            let _ = sess.end_session(&sid, crate::types::SessionStatus::Ended);
        })
        .await;

        // ADR-014 B4 (review fix HIGH-2): evict the approval->client entries AFTER
        // end_session has expired the approvals. Now a button tap racing with teardown
        // hits resolve_approval on an already-expired row (returns false → no response
        // sent, B2), so removing the client entry here cannot strand a transition. This
        // closes the unbounded-growth leak without a mis-route window.
        if !approval_ids.is_empty() {
            let mut clients = ctx.pending_approval_clients.write().await;
            for ap in &approval_ids {
                clients.remove(&ap.id);
            }
        }

        // ADR-013 GAP-5: Cascade session end to child sub-agent sessions.
        // When a parent session ends, end all its active children to prevent orphans.
        let parent_sid = msg.session_id.clone();
        let children = ctx
            .db_op(move |sess| sess.get_child_sessions(&parent_sid).unwrap_or_default())
            .await;
        if !children.is_empty() {
            tracing::info!(
                session_id = %msg.session_id,
                child_count = children.len(),
                "ADR-013 GAP-5: Cascading session end to {} child session(s)",
                children.len()
            );
            for child in &children {
                let child_id = child.id.clone();
                ctx.db_op(move |sess| {
                    let _ = sess.end_session(&child_id, crate::types::SessionStatus::Ended);
                })
                .await;
                ctx.session_tmux.write().await.remove(&child.id);
                ctx.custom_titles.write().await.remove(&child.id);
                ctx.pending_topic_msgs.write().await.remove(&child.id);
            }
        }
    }
}

/// Handler 3: agent_response
pub(super) async fn handle_agent_response(ctx: &HandlerContext, msg: &BridgeMessage) {
    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        // BUG-002: buffer instead of silently dropping; flushed once the topic exists.
        super::buffer_pending_message(ctx, msg).await;
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

    // ADR-013 Part C: If this agent_response carries agentId metadata (from a
    // SubagentStop hook), write the full output to a temp file and send a
    // one-liner with a "Details" button instead of dumping the raw text.
    if let Some(agent_id) = msg.meta().agent_id() {
        // ADR-013 GAP-1: Validate agent_id to prevent path traversal.
        if !crate::types::is_valid_agent_id(agent_id) {
            tracing::warn!(
                agent_id,
                "ADR-013 GAP-1: Invalid agent_id rejected (path traversal prevention)"
            );
            return;
        }
        // Write full output to temp file for the Details callback handler.
        let temp_path = format!("/tmp/ctm-subagent-{agent_id}.md");
        if let Err(e) = std::fs::write(&temp_path, &msg.content) {
            tracing::warn!(
                agent_id,
                error = %e,
                "ADR-013: Failed to write sub-agent temp file"
            );
        }

        // Send one-liner with Details button.
        let summary = if msg.content.chars().count() > 120 {
            let truncated: String = msg.content.chars().take(120).collect();
            format!("{truncated}\u{2026}")
        } else {
            msg.content.clone()
        };
        let one_liner = format!(
            "\u{2705} *Agent completed:* `{}`\n\n_{}_",
            escape_markdown_v1(agent_id),
            escape_markdown_v1(&summary)
        );
        ctx.bot
            .send_with_buttons(
                &one_liner,
                vec![InlineButton {
                    text: "\u{1F4CB} Details".into(),
                    callback_data: format!("subagentdetails:{agent_id}"),
                }],
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                thread_id,
            )
            .await;
    } else {
        // ADR-013 GAP-3: Prefix child session messages with agent label
        let content = if let Some(prefix) = get_child_prefix(ctx, &msg.session_id).await {
            format!("{}{}", prefix, &msg.content)
        } else {
            msg.content.clone()
        };
        ctx.bot
            .send_message(
                &format_agent_response(&content),
                Some(&SendOptions {
                    parse_mode: Some("Markdown".into()),
                    ..Default::default()
                }),
                thread_id,
            )
            .await;
    }
}

/// Handler 4: tool_start
pub(super) async fn handle_tool_start(ctx: &HandlerContext, msg: &BridgeMessage) {
    let meta = msg.meta();
    let tool_name = meta.tool().unwrap_or("Unknown");

    // ADR-015: AskUserQuestion is mirrored to Telegram FROM its ToolStart (the
    // restored ADR-012 render trigger). The hook no longer intercepts the question;
    // Claude renders its native CLI widget and we mirror OUT here, injecting answers
    // IN via tmux. tool_start carries the same `input` (questions) field that
    // handle_ask_user_question reads, so it works unchanged.
    if tool_name == "AskUserQuestion" {
        handle_ask_user_question(ctx, msg).await;
        return;
    }

    // Only show tool starts in verbose mode
    if !ctx.config.verbose {
        return;
    }

    let tool_input = meta.input().cloned().unwrap_or(serde_json::Value::Null);

    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        // BUG-002: buffer instead of silently dropping; flushed once the topic exists.
        super::buffer_pending_message(ctx, msg).await;
        return;
    }

    // Format brief preview
    let preview = format_tool_preview(tool_name, &tool_input);

    // Use hook-provided toolUseId if present, otherwise generate one
    let tool_use_id = meta
        .tool_use_id()
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!(
                "tool_{}_{}",
                chrono::Utc::now().timestamp_millis(),
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            )
        });
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
        // ADR-011 Fix #9 / ADR-014 D6: ToolStart previews are high-frequency
        // diagnostic noise — sent at Low priority so a tool-spam storm cannot starve
        // or evict substantive ToolResults (Normal) or approvals/questions (Critical).
        ctx.bot
            .send_with_buttons_low(
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
        // ADR-011 Fix #9 / ADR-014 D6: Low priority — see above.
        ctx.bot
            .send_message_low(
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

/// Handler 5: tool_result
pub(super) async fn handle_tool_result(ctx: &HandlerContext, msg: &BridgeMessage) {
    // ADR-015: An AskUserQuestion tool_result means the question was answered — at the
    // terminal (Claude's native widget) or via Telegram "Submit All". Stale the
    // Telegram question (idempotently) and stop here. We deliberately do NOT post the
    // raw answer text as a tool result: that would be echo noise, and runs regardless
    // of verbose mode (the hook always emits this ToolResult).
    if msg.meta().tool() == Some("AskUserQuestion") {
        resolve_pending_question(ctx, &msg.session_id).await;
        return;
    }

    if !ctx.config.verbose {
        return;
    }

    let meta = msg.meta();
    let tool_name = meta.tool().unwrap_or("Unknown");
    // H10: tool_input is stored as a JSON Value (object), not a plain string.
    // .as_str() always returns None for objects — use to_string() / as_str() on the
    // owned serialization instead.
    let tool_input_owned: Option<String> = meta.input().map(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()))
    });
    let tool_input = tool_input_owned.as_deref();

    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        // BUG-002: buffer instead of silently dropping; flushed once the topic exists.
        super::buffer_pending_message(ctx, msg).await;
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

    // ADR-014 D6: ToolResult stays at Normal priority (NOT Low). Unlike the
    // ToolStart *preview*, the result is the substantive mirror the user actually
    // reads, so it must survive a tool-spam storm and outrank Low-tier previews.
    // (Low is reserved for ToolStart previews; Critical for approvals/questions.)
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
pub(super) async fn handle_user_input(ctx: &HandlerContext, msg: &BridgeMessage) {
    let source = msg.meta().source().unwrap_or("cli");

    // Skip messages explicitly from Telegram
    if source == "telegram" {
        return;
    }

    // BUG-011: Check echo prevention set
    // C-1: Use \0 separator to match add_echo_key's key format (prevents mismatch).
    let input_key = format!("{}\0{}", msg.session_id, msg.content.trim());
    {
        let mut recent = ctx.recent_inputs.write().await;
        if recent.remove(&input_key) {
            tracing::debug!(input_key, "Skipping echo for telegram input");
            return;
        }
    }

    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        // BUG-002: buffer instead of silently dropping; flushed once the topic exists.
        super::buffer_pending_message(ctx, msg).await;
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
pub(super) async fn handle_approval_request(ctx: &HandlerContext, msg: &BridgeMessage) {
    // BUG-002 (review M3): resolve topic readiness BEFORE creating any approval
    // state. This is safety-critical — the hook blocks up to 300s waiting for a
    // response. If the topic is not ready we buffer the *request* (preserving its
    // client_id metadata) and return WITHOUT creating an approval row, so a replay
    // after the topic exists creates exactly one approval with a visible prompt.
    // The previous order created an orphan approval row and recorded the client,
    // then dropped the prompt — the user never saw it and the hook only fell back
    // after timing out.
    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        super::buffer_pending_message(ctx, msg).await;
        return;
    }

    let approval_id = {
        let sid = msg.session_id.clone();
        let content = msg.content.clone();
        ctx.db_op(move |sess| {
            sess.create_approval(&sid, &content, None)
                .unwrap_or_else(|_| "unknown".into())
        })
        .await
    };

    // S-2: Record which socket client sent this approval_request so the
    // response can be routed back to that specific client only.
    if let Some(client_id) = msg.meta().client_id() {
        ctx.pending_approval_clients
            .write()
            .await
            .insert(approval_id.clone(), client_id.to_string());
    }

    let keyboard = crate::bot::create_approval_keyboard(&approval_id);
    let buttons: Vec<InlineButton> = keyboard.into_iter().flatten().collect();

    // ADR-014 B1 (resolves the ADR-011 Fix #9 TODO): approval requests go at
    // Critical priority so a queue backlog cannot delay the prompt the user is
    // blocked on — the top-ranked glitch cause (prompt arriving late / never).
    ctx.bot
        .send_with_buttons_critical(
            &format_approval_request(&msg.content),
            buttons,
            Some(&SendOptions {
                parse_mode: Some("Markdown".into()),
                ..Default::default()
            }),
            thread_id,
        )
        .await;
}

/// Handler 10: error
pub(super) async fn handle_error(ctx: &HandlerContext, msg: &BridgeMessage) {
    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        // BUG-002: buffer instead of silently dropping; flushed once the topic exists.
        super::buffer_pending_message(ctx, msg).await;
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

/// Handler: command — handle system commands (toggle/enable/disable).
pub(super) async fn handle_command(ctx: &HandlerContext, msg: &BridgeMessage) {
    let cmd = msg.content.trim().to_lowercase();
    let new_state = match cmd.as_str() {
        "toggle" => !ctx
            .mirroring_enabled
            .load(std::sync::atomic::Ordering::Relaxed),
        "enable" | "on" => true,
        "disable" | "off" => false,
        _ => {
            tracing::debug!(cmd = %cmd, "Unknown system command");
            return;
        }
    };

    ctx.mirroring_enabled
        .store(new_state, std::sync::atomic::Ordering::Relaxed);
    crate::config::write_mirror_status(&ctx.config.config_dir, new_state, Some(std::process::id()));

    let status_text = if new_state {
        "\u{1F7E2} *Telegram mirroring: ON*"
    } else {
        "\u{1F534} *Telegram mirroring: OFF*"
    };

    tracing::info!(enabled = new_state, "Mirroring toggled");

    let opts = SendOptions {
        parse_mode: Some("Markdown".into()),
        ..Default::default()
    };
    ctx.bot.send_message(status_text, Some(&opts), None).await;
}

/// Handler 12: pre_compact
pub(super) async fn handle_pre_compact(ctx: &HandlerContext, msg: &BridgeMessage) {
    let trigger = msg.meta().trigger().unwrap_or("auto");

    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        // BUG-002: intentionally NOT buffered. pre_compact is purely informational
        // ("compacting…") and stale by the time a delayed topic appears; replaying it
        // would post a misleading notice. Dropping (logged, not silent) is correct.
        tracing::warn!(session_id = %msg.session_id, "No topic — dropping pre_compact");
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
pub(super) async fn handle_compact_complete(ctx: &HandlerContext, session_id: &str) {
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

// ====================================================================== session rename (Epic 5)

/// Check transcript JSONL for custom-title record.
pub(super) fn check_for_session_rename(transcript_path: &str) -> Option<String> {
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};

    // S-1: Validate path before opening (prevents path traversal)
    let validated = crate::hook::validate_transcript_path(transcript_path)?;
    let mut file = fs::File::open(&validated).ok()?;
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
                    return Some(title.to_string());
                }
            }
        }
    }
    None
}

/// Handle session rename: update Telegram forum topic name.
/// H8: Skip editForumTopic when title is unchanged (dedup).
pub(super) async fn handle_session_rename(
    ctx: &HandlerContext,
    session_id: &str,
    custom_title: &str,
) {
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

    // ADR-014 A5: Also persist to the DB so a resume after a daemon restart (which
    // empties the in-memory cache) still recovers the title for the new topic name
    // and the "Session resumed: {title}" message.
    {
        let sid = session_id.to_string();
        let title = custom_title.to_string();
        ctx.db_op(move |sess| {
            if let Err(e) = sess.set_custom_title(&sid, &title) {
                tracing::warn!(session_id = %sid, error = %e, "ADR-014 A5: Failed to persist custom title to DB");
            }
        })
        .await;
    }

    let thread_id = match ctx.get_thread_id(session_id).await {
        Some(tid) => tid,
        None => return,
    };

    let (hostname, project_dir) = {
        let sid = session_id.to_string();
        let s = ctx
            .db_op(move |sess| sess.get_session(&sid).ok().flatten())
            .await;
        (
            s.as_ref().and_then(|s| s.hostname.clone()),
            s.as_ref().and_then(|s| s.project_dir.clone()),
        )
    };

    let suffix =
        HandlerContext::format_topic_name(session_id, hostname.as_deref(), project_dir.as_deref());
    let new_name = format!("{custom_title} | {suffix}");
    // U-2: Char-safe truncation — avoid panicking on multibyte UTF-8 characters.
    let new_name: String = new_name.chars().take(128).collect();

    tracing::info!(session_id, custom_title, new_name, "Renaming forum topic");

    if let Ok(true) = ctx.bot.edit_forum_topic(thread_id, &new_name).await {
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

// ====================================================================== AskUserQuestion (Epic 3)

/// ADR-015 (Codex render-window TOCTOU): what the AskUserQuestion render store-back
/// must do, given the entry's `lifecycle` observed after the render `.await` window.
#[derive(Debug, PartialEq, Eq)]
enum StoreBackAction {
    /// `Active` — normal path: save the captured message ids for in-place edits.
    Save,
    /// `Resolved` — the question was answered (CLI tool_result) or superseded/cleaned up
    /// during the render window, with no ids to stale at that time. Stale the messages we
    /// just rendered ourselves so no live buttons orphan.
    StaleRendered,
    /// `Submitting` — the Submit-All flow owns these messages; do nothing.
    Ignore,
}

/// Pure decision for the render store-back (unit-testable without a live bot/tmux).
fn store_back_action(lifecycle: QuestionLifecycle) -> StoreBackAction {
    match lifecycle {
        QuestionLifecycle::Active => StoreBackAction::Save,
        QuestionLifecycle::Resolved => StoreBackAction::StaleRendered,
        QuestionLifecycle::Submitting => StoreBackAction::Ignore,
    }
}

/// ADR-015: Render an AskUserQuestion to Telegram from its `tool_start`.
///
/// The hook no longer intercepts the question — Claude renders its native CLI widget,
/// and we mirror OUT to Telegram here. Answers are injected back IN via tmux (see
/// `handle_submitall_callback`). There is NO blocked hook to release: on any
/// error/empty path we simply log and return; the CLI widget renders natively
/// regardless, and the user can answer at the terminal.
pub(super) async fn handle_ask_user_question(ctx: &HandlerContext, msg: &BridgeMessage) {
    let thread_id = ctx.wait_for_topic(&msg.session_id).await;
    if thread_id.is_none() && ctx.config.use_threads {
        // BUG-002: intentionally NOT buffered. The interactive widget is stateful and
        // the CLI still renders it natively, so the user is not blocked; replaying a
        // stale question into a late topic risks double-prompts. Drop (logged).
        tracing::warn!(session_id = %msg.session_id, "No topic — dropping ask_user_question (CLI widget still renders)");
        return;
    }

    let tool_input = match msg.meta().input() {
        Some(v) => v,
        None => {
            tracing::warn!(session_id = %msg.session_id, "AskUserQuestion tool_start missing metadata.input — not mirrored (CLI widget still renders)");
            return;
        }
    };

    let questions_val = match tool_input.get("questions").and_then(|v| v.as_array()) {
        Some(q) if !q.is_empty() => q,
        _ => {
            tracing::warn!(session_id = %msg.session_id, "AskUserQuestion missing/empty questions — not mirrored (CLI widget still renders)");
            return;
        }
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

    // H6.1: Use full session_id as the pending_key to avoid collisions.
    // The short prefix is only used in callback_data (Telegram's 64-byte limit).
    let short_session_id = &msg.session_id[..std::cmp::min(20, msg.session_id.len())];
    let pending_key = msg.session_id.clone();

    // ADR-012: Insert PendingQuestion with new tentative-selection model.
    // No TTL — questions persist until Submit All or session end.
    //
    // C4: If a PendingQuestion already exists for this session (second
    // AskUserQuestion before the first was answered), supersede the old one:
    // edit its question messages to "Superseded" and delete its summary.
    let new_entry = Arc::new(Mutex::new(PendingQuestion {
        session_id: msg.session_id.clone(),
        questions: questions.clone(),
        tentative: HashMap::new(),
        finalized: vec![false; questions.len()],
        question_message_ids: Vec::new(),
        summary_message_id: None,
        lifecycle: QuestionLifecycle::Active,
    }));
    let old_entry = {
        let mut pq = ctx.pending_q.write().await;
        pq.insert(pending_key.clone(), Arc::clone(&new_entry))
    };
    if let Some(old_arc) = old_entry {
        // ADR-015 (Codex lock-across-I/O sweep): under the old entry's lock, mark it
        // terminal (Resolved) and CLONE its message ids; DROP the lock; THEN await the
        // edit/delete I/O. Marking Resolved means a still-in-flight render of the
        // superseded entry sees `Resolved` at store-back and stales its own messages
        // instead of resurrecting obsolete live buttons.
        let (old_count, old_question_ids, old_summary_id) = {
            let mut old_pq = old_arc.lock().await;
            old_pq.lifecycle = QuestionLifecycle::Resolved;
            (
                old_pq.questions.len(),
                old_pq.question_message_ids.clone(),
                old_pq.summary_message_id,
            )
            // old entry mutex drops here
        };
        tracing::info!(
            session_id = %msg.session_id,
            old_questions = old_count,
            "Superseding previous AskUserQuestion"
        );
        // Dismiss old question messages.
        for mid in old_question_ids {
            if mid != 0 {
                let _ = ctx
                    .bot
                    .edit_message_text_no_markup(mid, "\u{2B55} Superseded")
                    .await;
            }
        }
        // Delete old summary if present.
        if let Some(mid) = old_summary_id {
            let _ = ctx.bot.delete_message(ctx.config.chat_id, mid).await;
        }
    }

    // ADR-012: Render each question as a separate message, capturing message_ids
    // so we can edit them in place when the user changes their selection.
    let mut question_message_ids: Vec<i64> = Vec::new();
    for (q_idx, q) in questions.iter().enumerate() {
        // ADR-014 (review follow-up): PLAIN TEXT render (no parse_mode on the send
        // below). See render_question_text for why Markdown is unsafe here.
        let text = super::render_question_text(q);

        let mut buttons = Vec::new();
        if q.multi_select {
            for (o_idx, opt) in q.options.iter().enumerate() {
                buttons.push(InlineButton {
                    text: opt.label.clone(),
                    callback_data: format!("toggle:{short_session_id}:{q_idx}:{o_idx}"),
                });
            }
            buttons.push(InlineButton {
                text: "\u{2705} Done".into(),
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

        // ADR-012 Phase 10: Use send_with_buttons_returning to capture message_id.
        // parse_mode = None: see plain-text rationale above (D2).
        match ctx
            .bot
            .send_with_buttons_returning(&text, buttons, None, thread_id)
            .await
        {
            Ok(mid) => question_message_ids.push(mid),
            Err(e) => {
                // ADR-014 (review follow-up): this is NOT retried — the previous
                // "retrying via queue" comment was false (it only pushed the 0
                // sentinel). A queued retry would lose the message_id this flow needs
                // for in-place edits. The failure is surfaced to the user below (E4)
                // and the blocked hook is released so it falls back to its TUI.
                tracing::warn!(
                    session_id = %msg.session_id,
                    q_idx,
                    error = %e,
                    "Failed to render question message (no retry; surfaced to user + hook released)"
                );
                // 0 sentinel keeps indices aligned with `questions` (edits skip 0).
                question_message_ids.push(0);
            }
        }
    }

    // ADR-014 E4: Surface render failures instead of silently swallowing them.
    // A `0` sentinel means a question message failed to send — the user would
    // otherwise see a partial/empty prompt and Claude would block with no visible
    // way to answer. Notify in the topic so the failure is visible.
    let failed_renders = question_message_ids.iter().filter(|&&m| m == 0).count();
    if failed_renders > 0 {
        tracing::warn!(
            session_id = %msg.session_id,
            failed_renders,
            total = question_message_ids.len(),
            "ADR-014 E4: {failed_renders} AskUserQuestion message(s) failed to render"
        );
        ctx.bot
            .send_message(
                &format!(
                    "\u{26A0}\u{FE0F} {failed_renders} of {} question(s) failed to send. Please answer at the terminal.",
                    question_message_ids.len()
                ),
                None,
                thread_id,
            )
            .await;
        // ADR-015: nothing to release — the CLI widget renders natively, so a partial
        // render failure just means the user answers at the terminal. The matching
        // tool_result will stale whatever did render (resolve_pending_question).
    }

    // ADR-015 (Codex render-window TOCTOU): store the message_ids THROUGH the captured
    // `new_entry` Arc, NOT via a fresh pending_q lookup. During the render `.await`
    // window above, a CLI-answer tool_result can have run resolve_pending_question, which
    // transitioned this entry Active→Resolved and removed it from the map while
    // `question_message_ids` was still empty (so resolve staled nothing). A fresh lookup
    // would then find no entry and leave the just-rendered messages with LIVE buttons —
    // orphaned. Instead we arbitrate on the entry's own lifecycle:
    //   - Active     → normal path: save the ids for in-place edits.
    //   - Resolved   → already answered (CLI) or superseded during render: stale the
    //                  messages we just rendered ourselves so no live buttons orphan.
    //   - Submitting → the submit flow owns these messages; do nothing.
    let stale_now: Option<Vec<i64>> = {
        let mut pending = new_entry.lock().await;
        match store_back_action(pending.lifecycle) {
            StoreBackAction::Save => {
                pending.question_message_ids = question_message_ids;
                None
            }
            StoreBackAction::StaleRendered => Some(question_message_ids),
            StoreBackAction::Ignore => None,
        }
    };
    if let Some(message_ids) = stale_now {
        tracing::info!(
            session_id = %msg.session_id,
            "ADR-015: question resolved during render — staling just-rendered messages"
        );
        for mid in message_ids {
            if mid != 0 {
                let _ = ctx
                    .bot
                    .edit_message_text_no_markup(mid, "\u{2705} Answered at terminal")
                    .await;
            }
        }
    }
}

/// Clean up pending questions for a session.
///
/// LOCK ORDER (ADR-015, Codex deadlock fix): never `.await` an `entry.lock()` while the
/// `pending_q` guard is held, and never hold an entry lock across Telegram I/O. We
/// snapshot `(key, Arc)` pairs under the READ guard, DROP it, inspect/transition each
/// entry under its OWN lock (dropped before the next, capturing rendered message ids),
/// take ONE `pending_q.write()` to remove the keys, and ONLY THEN await the message
/// edits with no lock held.
pub(super) async fn cleanup_pending_questions(ctx: &HandlerContext, session_id: &str) {
    // 1. Snapshot all (key, Arc) pairs under the read guard, then DROP the guard.
    let candidates: Vec<(String, Arc<Mutex<PendingQuestion>>)> = {
        let pq = ctx.pending_q.read().await;
        pq.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect()
    };

    // 2. With NO pending_q guard held, inspect each entry under its own lock. ADR-015
    //    (Codex render-window TOCTOU): mark matching entries terminal (Resolved) and
    //    CAPTURE any already-rendered message ids so we can stale them after the locks
    //    are dropped. Entries with NO rendered ids yet are an in-flight render: leaving
    //    them Resolved makes that render's own store-back self-stale (do not capture).
    let mut to_remove: Vec<(String, Arc<Mutex<PendingQuestion>>)> = Vec::new();
    let mut to_stale: Vec<(Vec<i64>, Option<i64>)> = Vec::new();
    for (k, entry) in candidates {
        let captured = {
            let mut pending = entry.lock().await;
            if pending.session_id == session_id {
                pending.lifecycle = QuestionLifecycle::Resolved;
                let ids = pending.question_message_ids.clone();
                let summary = pending.summary_message_id;
                // Only stale here if this entry already rendered messages; otherwise the
                // in-flight render's store-back (which sees Resolved) handles staling.
                let has_rendered = ids.iter().any(|&m| m != 0) || summary.is_some();
                Some((has_rendered, ids, summary))
            } else {
                None
            }
            // entry mutex drops here
        };
        if let Some((has_rendered, ids, summary)) = captured {
            to_remove.push((k, entry));
            if has_rendered {
                to_stale.push((ids, summary));
            }
        }
    }

    // 3. One write guard to remove the matched keys (no entry lock held). ADR-015 (Codex
    //    identity-checked removal): remove a key ONLY if it still points at the SAME Arc
    //    we transitioned. A new AskUserQuestion superseding this session between snapshot
    //    and now installs a NEW active entry under the same key — don't orphan it.
    if !to_remove.is_empty() {
        let mut pq = ctx.pending_q.write().await;
        for (k, entry) in to_remove {
            if pq.get(&k).is_some_and(|cur| Arc::ptr_eq(cur, &entry)) {
                pq.remove(&k);
            }
        }
    }

    // 4. With NO lock held, stale the already-rendered Telegram messages so a torn-down
    //    session leaves no orphaned live buttons.
    for (question_message_ids, summary_message_id) in to_stale {
        for mid in question_message_ids {
            if mid != 0 {
                let _ = ctx
                    .bot
                    .edit_message_text_no_markup(mid, "\u{2705} Answered at terminal")
                    .await;
            }
        }
        if let Some(mid) = summary_message_id {
            let _ = ctx
                .bot
                .edit_message(
                    ctx.config.chat_id,
                    mid,
                    "\u{2705} Answered at terminal",
                    None,
                )
                .await;
        }
    }
}

/// ADR-015: Resolve a session's pending AskUserQuestion because it was answered.
///
/// Called from `handle_tool_result` when an AskUserQuestion tool_result arrives — the
/// signal that the question was answered (at the terminal via Claude's native widget,
/// or via Telegram "Submit All"). Stales the Telegram question messages (edit to
/// "answered at terminal", removing the inline keyboards) and drops the pending entry.
///
/// MUST be idempotent. ADR-015 (Codex B3): a Telegram "Submit All" injects keystrokes
/// that ALSO make Claude emit this tool_result, so BOTH paths run for one answer. The
/// `lifecycle` field is the arbiter, checked-and-set under the per-entry mutex:
///   - `Submitting` → Submit All owns the message edits; we no-op.
///   - `Active` → we win: set `Resolved`, capture the message ids, then (after dropping
///     the entry lock) remove the map key and stale the messages.
///   - `Resolved`, or no entry → already done; no-op.
///
/// LOCK ORDER (ADR-015, Codex deadlock fix): NEVER hold the `pending_q` guard across
/// `entry.lock()`, and NEVER hold `entry.lock()` across `pending_q.write()`. The
/// `lifecycle` flag (not co-holding both locks) is what guarantees a single owner; map
/// removal after the entry lock is dropped is safe because a late resolve sees
/// `Resolved`/absent and no-ops.
pub(super) async fn resolve_pending_question(ctx: &HandlerContext, session_id: &str) {
    // 1. Clone the Arc under the pending_q READ guard, then DROP the guard.
    let entry: Option<Arc<Mutex<PendingQuestion>>> =
        { ctx.pending_q.read().await.get(session_id).cloned() };
    let Some(entry) = entry else {
        return; // No pending question — already resolved/removed. Idempotent no-op.
    };

    // 2. Under the entry mutex ONLY (no pending_q guard held): arbitrate and capture the
    //    message ids. `won` = we transitioned Active→Resolved (so WE remove the key and
    //    stale the messages). On `Submitting`, Submit All owns both removal and edits, so
    //    we must NOT remove its still-in-flight map entry. Then DROP the entry lock.
    let outcome: Option<(Vec<i64>, Option<i64>)> = {
        let mut pending = entry.lock().await;
        match pending.lifecycle {
            QuestionLifecycle::Submitting => {
                // Submit All is mid-flight and owns the entry + terminal edits — leave it.
                tracing::debug!(
                    session_id,
                    "ADR-015: tool_result arrived while Submit All in flight — resolve no-op"
                );
                return;
            }
            QuestionLifecycle::Resolved => {
                // A duplicate tool_result after we (or Submit All) already finished. Fall
                // through to remove any stale key, but do not re-edit messages.
                None
            }
            QuestionLifecycle::Active => {
                pending.lifecycle = QuestionLifecycle::Resolved;
                Some((
                    pending.question_message_ids.clone(),
                    pending.summary_message_id,
                ))
            }
        }
    };

    // 3. With NO lock held, remove the map key — but ONLY if it still points at OUR Arc
    //    (ADR-015 Codex identity-checked removal). If a new AskUserQuestion superseded
    //    this entry between the snapshot and now, the key maps to the NEW active entry;
    //    removing by key would orphan it. Idempotent: clears the Active→Resolved winner
    //    and any already-Resolved leftover.
    {
        let mut pq = ctx.pending_q.write().await;
        if pq
            .get(session_id)
            .is_some_and(|cur| Arc::ptr_eq(cur, &entry))
        {
            pq.remove(session_id);
        }
    }

    let Some((question_message_ids, summary_message_id)) = outcome else {
        return; // Already Resolved — key cleaned up above, no edits.
    };

    for mid in question_message_ids {
        if mid != 0 {
            let _ = ctx
                .bot
                .edit_message_text_no_markup(mid, "\u{2705} Answered at terminal")
                .await;
        }
    }
    if let Some(mid) = summary_message_id {
        let _ = ctx
            .bot
            .edit_message(
                ctx.config.chat_id,
                mid,
                "\u{2705} Answered at terminal",
                None,
            )
            .await;
    }
}

/// Handle send_image message: upload a local file to Telegram.
pub(super) async fn handle_send_image(ctx: &HandlerContext, msg: &BridgeMessage) {
    let path = match files::validate_send_image_path(&msg.content) {
        Ok(p) => p,
        Err(reason) => {
            tracing::warn!(
                path = %msg.content,
                reason,
                "SendImage: invalid path"
            );
            return;
        }
    };

    let thread_id = ctx.get_thread_id(&msg.session_id).await;
    if thread_id.is_none() {
        tracing::debug!(
            session_id = %msg.session_id,
            "SendImage: no forum topic for session"
        );
        return;
    }

    let caption = msg.meta().caption();

    let result = if files::is_image_extension(path) {
        ctx.bot.send_photo(path, caption, thread_id).await
    } else {
        ctx.bot.send_document(path, caption, thread_id).await
    };

    match result {
        Ok(()) => {
            tracing::info!(
                path = %msg.content,
                session_id = %msg.session_id,
                "Sent file to Telegram"
            );
        }
        Err(e) => {
            tracing::warn!(
                path = %msg.content,
                error = %e,
                "Failed to send file to Telegram"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_markdown_v1() {
        assert_eq!(escape_markdown_v1("hello `world`"), "hello 'world'");
    }

    /// ADR-015 (Codex render-window TOCTOU): the store-back must SAVE only when the entry
    /// is still Active; if a CLI tool_result / supersede / session-end resolved it during
    /// the render window (Resolved), the store-back must stale the just-rendered messages
    /// (so no live buttons orphan); while a Submit-All is mid-flight (Submitting) it must
    /// leave the messages to that flow.
    #[test]
    fn store_back_action_branches_on_lifecycle() {
        assert_eq!(
            store_back_action(QuestionLifecycle::Active),
            StoreBackAction::Save
        );
        assert_eq!(
            store_back_action(QuestionLifecycle::Resolved),
            StoreBackAction::StaleRendered,
            "resolved-during-render must stale its own messages, not orphan live buttons"
        );
        assert_eq!(
            store_back_action(QuestionLifecycle::Submitting),
            StoreBackAction::Ignore
        );
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
                msg_type: MessageType::ApprovalResponse,
                session_id: session_id.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                content: action.to_string(),
                metadata: Some(metadata),
            };

            // Verify type matches what the TypeScript daemon broadcasts.
            assert_eq!(msg.msg_type, MessageType::ApprovalResponse);
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
            assert_eq!(parsed.msg_type, MessageType::ApprovalResponse);
            assert_eq!(parsed.content, *action);
        }
    }
}
