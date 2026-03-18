//! Stale session cleanup, topic deletion scheduling, periodic maintenance.

use super::*;

// ADR-011 Fix #7: Cache size limits.
// Eviction kicks in during the cleanup cycle when these thresholds are exceeded.
const MAX_SESSION_CACHE: usize = 200;
const MAX_TOOL_CACHE: usize = 500;

/// ADR-013 E2: Default inactivity threshold for topic deletion (12 hours).
/// Topics are CLOSED after `topic_delete_delay_minutes` (stage 1, typically 15 min),
/// then DELETED after this inactivity threshold (stage 2).
/// ADR-013 GAP-6: Now read from ctx.config.inactivity_delete_threshold_minutes at runtime.
#[allow(dead_code)]
const INACTIVITY_DELETE_THRESHOLD_MINUTES: u64 = 720;

/// BUG-003: Periodic cleanup of stale sessions, orphaned threads, expired caches, old downloads.
pub(super) async fn run_cleanup(ctx: HandlerContext) {
    // Expire old approvals
    ctx.db_op(|sess| {
        let _ = sess.expire_old_approvals();
    })
    .await;

    // Clean stale sessions (differentiated timeouts)
    cleanup_stale_sessions(&ctx).await;

    // Clean orphaned threads
    cleanup_orphaned_threads(&ctx).await;

    // Clean expired tool input cache (TTL-based)
    {
        let mut cache = ctx.tool_cache.write().await;
        cache.retain(|_, v| v.timestamp.elapsed().as_secs() < TOOL_CACHE_TTL_SECS);
    }

    // ADR-011 Fix #7: Enforce cache size limits.
    enforce_cache_size_limits(&ctx).await;

    // ADR-013 E2: Inactivity-based topic cleanup sweep.
    // Close topics for sessions inactive > topic_delete_delay_minutes (stage 1).
    // Delete topics for sessions inactive > INACTIVITY_DELETE_THRESHOLD_MINUTES (stage 2).
    cleanup_inactive_topics(&ctx).await;

    // Clean old downloads
    cleanup_old_downloads();

    // ADR-013 MINOR-5: Clean up stale sub-agent temp files
    cleanup_subagent_temp_files().await;
}

/// ADR-011 Fix #7: Evict excess entries from the session-keyed caches.
///
/// Acquires locks in the documented order (session_threads -> session_tmux ->
/// custom_titles) to avoid deadlocks. Only evicts when a cache exceeds its
/// size limit; entries whose session is no longer active are removed first so
/// that live sessions are always preserved.
async fn enforce_cache_size_limits(ctx: &HandlerContext) {
    // Fetch active session IDs from the database once (avoids repeated DB calls).
    let active_ids: std::collections::HashSet<String> = ctx
        .db_op(|sess| {
            sess.get_active_sessions()
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.id)
                .collect()
        })
        .await;

    // Lock order 2: session_threads
    {
        let mut threads = ctx.session_threads.write().await;
        if threads.len() > MAX_SESSION_CACHE {
            let before = threads.len();
            threads.retain(|k, _| active_ids.contains(k));
            let evicted = before - threads.len();
            if evicted > 0 {
                tracing::info!(
                    evicted,
                    remaining = threads.len(),
                    "Cache eviction: session_threads exceeded limit"
                );
            }
        }
    }

    // Lock order 3: session_tmux
    {
        let mut tmux = ctx.session_tmux.write().await;
        if tmux.len() > MAX_SESSION_CACHE {
            let before = tmux.len();
            tmux.retain(|k, _| active_ids.contains(k));
            let evicted = before - tmux.len();
            if evicted > 0 {
                tracing::info!(
                    evicted,
                    remaining = tmux.len(),
                    "Cache eviction: session_tmux exceeded limit"
                );
            }
        }
    }

    // Lock order 4: custom_titles
    {
        let mut titles = ctx.custom_titles.write().await;
        if titles.len() > MAX_SESSION_CACHE {
            let before = titles.len();
            titles.retain(|k, _| active_ids.contains(k));
            let evicted = before - titles.len();
            if evicted > 0 {
                tracing::info!(
                    evicted,
                    remaining = titles.len(),
                    "Cache eviction: custom_titles exceeded limit"
                );
            }
        }
    }

    // Lock order 4 (tool_cache follows custom_titles in field declaration order)
    {
        let mut cache = ctx.tool_cache.write().await;
        if cache.len() > MAX_TOOL_CACHE {
            let before = cache.len();
            // Evict entries older than 60 seconds when the cache is over the limit.
            cache.retain(|_, v| v.timestamp.elapsed().as_secs() < 60);
            let evicted = before - cache.len();
            if evicted > 0 {
                tracing::info!(
                    evicted,
                    remaining = cache.len(),
                    "Cache eviction: tool_cache exceeded limit"
                );
            }
        }
    }
}

/// BUG-003: Stale session cleanup with differentiated timeouts.
async fn cleanup_stale_sessions(ctx: &HandlerContext) {
    let candidates = ctx
        .db_op(|sess| {
            sess.get_stale_session_candidates(NO_TMUX_SESSION_TIMEOUT_HOURS)
                .unwrap_or_default()
        })
        .await;

    if candidates.is_empty() {
        return;
    }

    let now = chrono::Utc::now();
    let tmux_cutoff = now
        - chrono::TimeDelta::try_hours(i64::from(TMUX_SESSION_TIMEOUT_HOURS))
            .unwrap_or(chrono::TimeDelta::hours(24));

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
        let Some(target) = tmux_target.as_deref() else { continue; };
        let pane_alive = InputInjector::is_pane_alive(target, session.tmux_socket.as_deref());
        let t = target.to_string();
        let sid = session.id.clone();
        let pane_reassigned = ctx
            .db_op(move |sess| {
                sess.is_tmux_target_owned_by_other(&t, &sid)
                    .unwrap_or(false)
            })
            .await;

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
///
/// NOTE: Stale sessions are deleted immediately (no delay) because they represent
/// abandoned sessions where the user is no longer monitoring. Using the normal
/// topic_delete_delay_minutes delay would accumulate dead topics.
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
                let sid = session.id.clone();
                ctx.db_op(move |sess| {
                    let _ = sess.clear_thread_id(&sid);
                })
                .await;
            } else {
                let _ = ctx.bot.close_forum_topic(tid).await;
                // Clear thread_id so orphaned cleanup doesn't retry endlessly
                // on a closed-but-not-deleted topic.
                let sid = session.id.clone();
                ctx.db_op(move |sess| {
                    let _ = sess.clear_thread_id(&sid);
                })
                .await;
            }
        } else {
            let _ = ctx.bot.close_forum_topic(tid).await;
        }
    }

    // Clean caches
    ctx.session_threads.write().await.remove(&session.id);
    ctx.session_tmux.write().await.remove(&session.id);
    ctx.custom_titles.write().await.remove(&session.id);

    // Clean up transcript state file
    let state_file = get_config_dir().join(format!(".last_line_{}", session.id));
    if state_file.exists() {
        let _ = std::fs::remove_file(&state_file);
    }

    let sid = session.id.clone();
    ctx.db_op(move |sess| {
        let _ = sess.end_session(&sid, crate::types::SessionStatus::Ended);
    })
    .await;
}

/// Clean up orphaned threads (ended sessions still with thread_ids).
async fn cleanup_orphaned_threads(ctx: &HandlerContext) {
    if !ctx.config.auto_delete_topics {
        return;
    }

    let orphans = ctx
        .db_op(|sess| sess.get_orphaned_thread_sessions().unwrap_or_default())
        .await;

    let mut cleaned = 0;
    for session in &orphans {
        if let Some(tid) = session.thread_id {
            match ctx.bot.delete_forum_topic(tid).await {
                Ok(true) => {
                    tracing::debug!(thread_id = tid, session_id = %session.id, "Deleted orphaned topic")
                }
                Ok(false) => {
                    tracing::warn!(thread_id = tid, session_id = %session.id, "Failed to delete orphaned topic (may already be deleted)")
                }
                Err(e) => {
                    tracing::warn!(thread_id = tid, session_id = %session.id, error = %e, "Failed to delete orphaned topic")
                }
            }
            let sid = session.id.clone();
            ctx.db_op(move |sess| {
                let _ = sess.clear_thread_id(&sid);
            })
            .await;
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

/// ADR-013 E2: Sweep active sessions for inactivity-based topic cleanup.
///
/// Two stages:
/// - Stage 2 (>720 min inactivity): DELETE the topic (full cleanup).
///
/// Stage 1 (close after session end) is handled by `schedule_topic_deletion`.
/// This sweep catches sessions that are technically still "active" in the DB
/// but have been idle for a long time (e.g. user walked away without ending).
async fn cleanup_inactive_topics(ctx: &HandlerContext) {
    let active_sessions = ctx
        .db_op(|sess| sess.get_active_sessions().unwrap_or_default())
        .await;

    if active_sessions.is_empty() {
        return;
    }

    let now = chrono::Utc::now();
    let threshold_minutes = ctx.config.inactivity_delete_threshold_minutes as i64;
    let delete_cutoff = now
        - chrono::TimeDelta::try_minutes(threshold_minutes)
            .unwrap_or(chrono::TimeDelta::hours(12));

    let mut cleaned = 0u32;
    for session in &active_sessions {
        let last_activity = match chrono::DateTime::parse_from_rfc3339(&session.last_activity).ok()
        {
            Some(la) => la.to_utc(),
            None => continue,
        };

        // Only process sessions that are past the delete threshold
        if last_activity >= delete_cutoff {
            continue;
        }

        let thread_id = match session.thread_id {
            Some(tid) => tid,
            None => continue,
        };

        // Skip sessions with pending deletions already scheduled
        if ctx.pending_del.read().await.contains_key(&session.id) {
            continue;
        }

        let hours_inactive = (now - last_activity).num_hours();
        tracing::info!(
            session_id = %session.id,
            hours_inactive,
            thread_id,
            "ADR-013 E2: Deleting topic for inactive session (>12h)"
        );

        // Stage 2: Delete the topic
        if ctx.config.auto_delete_topics {
            match ctx.bot.delete_forum_topic(thread_id).await {
                Ok(true) => {
                    let sid = session.id.clone();
                    ctx.db_op(move |sess| {
                        let _ = sess.clear_thread_id(&sid);
                    })
                    .await;
                    ctx.session_threads.write().await.remove(&session.id);
                }
                _ => {
                    // Fallback: close if delete fails
                    let _ = ctx.bot.close_forum_topic(thread_id).await;
                    let sid = session.id.clone();
                    ctx.db_op(move |sess| {
                        let _ = sess.clear_thread_id(&sid);
                    })
                    .await;
                    ctx.session_threads.write().await.remove(&session.id);
                }
            }
        } else {
            // ADR-013 GAP-6: Close (don't delete) inactive topics when auto_delete is off.
            let _ = ctx.bot.close_forum_topic(thread_id).await;
            ctx.session_threads.write().await.remove(&session.id);
        }

        // End the session in DB
        let sid = session.id.clone();
        ctx.db_op(move |sess| {
            let _ = sess.end_session(&sid, crate::types::SessionStatus::Ended);
        })
        .await;

        // Clean caches
        ctx.session_tmux.write().await.remove(&session.id);
        ctx.custom_titles.write().await.remove(&session.id);

        cleaned += 1;
        if cleaned >= 10 {
            break; // Rate limit: max 10 per cleanup cycle
        }

        // Rate limit between Telegram API calls
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }

    if cleaned > 0 {
        tracing::info!(
            cleaned,
            "ADR-013 E2: Completed inactivity cleanup sweep"
        );
    }
}

/// ADR-013 E2/E5: Schedule topic lifecycle with two stages.
///
/// - **Stage 1** (after `delay_ms`): CLOSE the topic (hides from list, preserves history).
/// - **Stage 2** (after 12 hours total): DELETE the topic (full cleanup).
///
/// This replaces the previous behavior that deleted immediately after the delay.
/// Schedule topic deletion with delay (allows for session resume).
pub(super) async fn schedule_topic_deletion(
    ctx: &HandlerContext,
    session_id: &str,
    thread_id: i64,
    delay_ms: u64,
) {
    let bot = Arc::clone(&ctx.bot);
    let sessions = Arc::clone(&ctx.sessions);
    let session_threads = Arc::clone(&ctx.session_threads);
    // ADR-013 MINOR-7: Capture additional caches for full cleanup on stage-2 deletion.
    let session_tmux = Arc::clone(&ctx.session_tmux);
    let custom_titles = Arc::clone(&ctx.custom_titles);
    let sid = session_id.to_string();

    // ADR-013 E5: Two-stage lifecycle.
    // Stage 1: CLOSE the topic after delay_ms (preserves history, hides from list).
    // Stage 2: DELETE the topic after inactivity_delete_threshold_minutes total.
    let inactivity_threshold_ms =
        ctx.config.inactivity_delete_threshold_minutes as u64 * 60 * 1000;
    let stage2_delay_ms = inactivity_threshold_ms;

    let handle = tokio::spawn(async move {
        // Stage 1: Wait for the configured delay, then CLOSE the topic.
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        tracing::info!(session_id = %sid, thread_id, "ADR-013 E5 Stage 1: Closing forum topic");
        let _ = bot.close_forum_topic(thread_id).await;

        // Stage 2: Wait for the remaining time until the full deletion threshold,
        // then DELETE the topic. The remaining time is (stage2 - stage1).
        let remaining_ms = stage2_delay_ms.saturating_sub(delay_ms);
        if remaining_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(remaining_ms)).await;
        }

        tracing::info!(session_id = %sid, thread_id, "ADR-013 E5 Stage 2: Deleting forum topic");
        let deleted = bot.delete_forum_topic(thread_id).await.unwrap_or(false);
        if deleted {
            tracing::info!(session_id = %sid, thread_id, "Auto-deleted forum topic (stage 2)");
        } else {
            tracing::warn!(session_id = %sid, thread_id, "Failed to delete topic in stage 2");
        }
        // Clear thread_id and cache regardless of delete success,
        // to prevent orphaned cleanup retrying endlessly.
        session_threads.write().await.remove(&sid);
        // ADR-013 MINOR-7: Also clear tmux and custom_titles caches on stage-2 deletion.
        // Previously only session_threads was cleared, leaving stale entries.
        session_tmux.write().await.remove(&sid);
        custom_titles.write().await.remove(&sid);
        let sid2 = sid.clone();
        let sess = sessions.clone();
        let _ =
            tokio::task::spawn_blocking(move || sess.blocking_lock().clear_thread_id(&sid2))
                .await;
    });

    ctx.pending_del
        .write()
        .await
        .insert(session_id.to_string(), handle);
}

/// ADR-013 MINOR-5: Clean up stale sub-agent temp files older than 24 hours.
/// These are written by handle_agent_response for the Details button callback.
async fn cleanup_subagent_temp_files() {
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return;
    };

    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(24 * 60 * 60);

    let mut cleaned = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        if !name.starts_with("ctm-subagent-") || !name.ends_with(".md") {
            continue;
        }

        let modified = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };

        if modified < cutoff {
            if std::fs::remove_file(&path).is_ok() {
                cleaned += 1;
            }
        }
    }

    if cleaned > 0 {
        tracing::info!(cleaned, "ADR-013 MINOR-5: Cleaned stale sub-agent temp files");
    }
}

/// BUG-012: Cancel pending topic deletion when session resumes.
///
/// NOTE: There is a TOCTOU window here. If the deletion task has already passed
/// the sleep and is executing the HTTP delete_forum_topic call, abort() will
/// cancel the task but the HTTP request may already be inflight. In this case,
/// the topic may be deleted even though cancellation was requested.
/// This is handled by ensure_session_exists which checks for missing thread_id
/// and creates a new topic if needed. See BUG-012 documentation.
pub(super) async fn cancel_pending_topic_deletion(ctx: &HandlerContext, session_id: &str) {
    if let Some(handle) = ctx.pending_del.write().await.remove(session_id) {
        handle.abort();
        tracing::info!(
            session_id,
            "Cancelled pending topic deletion (session resumed)"
        );
    }
}
