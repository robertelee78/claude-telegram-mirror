//! Stale session cleanup, topic deletion scheduling, periodic maintenance.

use super::*;

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

    // Clean expired tool input cache
    {
        let mut cache = ctx.tool_cache.write().await;
        cache.retain(|_, v| v.timestamp.elapsed().as_secs() < TOOL_CACHE_TTL_SECS);
    }

    // Clean old downloads
    cleanup_old_downloads();
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
        let target = tmux_target.as_deref().unwrap();
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
            let _ = ctx.bot.delete_forum_topic(tid).await;
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
    let sid = session_id.to_string();

    let handle = tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        let deleted = bot.delete_forum_topic(thread_id).await.unwrap_or(false);
        if deleted {
            tracing::info!(session_id = %sid, thread_id, "Auto-deleted forum topic");
            session_threads.write().await.remove(&sid);
            let sid2 = sid.clone();
            let sess = sessions.clone();
            let _ =
                tokio::task::spawn_blocking(move || sess.blocking_lock().clear_thread_id(&sid2))
                    .await;
        } else {
            tracing::warn!(session_id = %sid, thread_id, "Failed to delete topic, falling back to close");
            let _ = bot.close_forum_topic(thread_id).await;
            session_threads.write().await.remove(&sid);
        }
    });

    ctx.pending_del
        .write()
        .await
        .insert(session_id.to_string(), handle);
}

/// BUG-012: Cancel pending topic deletion when session resumes.
pub(super) async fn cancel_pending_topic_deletion(ctx: &HandlerContext, session_id: &str) {
    if let Some(handle) = ctx.pending_del.write().await.remove(session_id) {
        handle.abort();
        tracing::info!(
            session_id,
            "Cancelled pending topic deletion (session resumed)"
        );
    }
}
