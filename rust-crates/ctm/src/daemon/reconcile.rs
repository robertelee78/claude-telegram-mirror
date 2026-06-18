//! STALE-TOPICS: liveness-driven reconciliation of Telegram forum topics against the
//! Claude sessions they mirror.
//!
//! The 1:1 `session_id ↔ thread_id` map is only as clean as our knowledge of which
//! sessions are still alive. The original design tore topics down on the `SessionEnd`
//! hook (which does NOT fire on a terminal close, `kill -9`, `tmux kill-session`, or a
//! machine reboot) and on long inactivity timers — so dead sessions' topics piled up
//! for up to a day (the tmux pane-liveness check was gated behind a 24h timeout) and a
//! daemon restart never reconciled the backlog.
//!
//! This module makes pane/Claude liveness the PROMPT, authoritative signal. Because one
//! `ctm` daemon owns exactly one machine, the local tmux server is a surefire source of
//! truth for the sessions it launched. Crucially, liveness is keyed on the *specific
//! Claude session*, not merely "a pane exists":
//!
//!   * pane gone (`!is_pane_alive`)                                  → dead
//!   * pane reassigned to a newer active session-id                  → dead (superseded)
//!   * pane fell back to a shell prompt (`PaneClaudeState::Shell`)   → dead (Claude exited)
//!   * pane title still shows "Claude Code" (`RunningClaude`)        → ALIVE, never pruned
//!   * anything else (`Unknown`)                                     → kept here; the
//!     inactivity backstop in `cleanup` prunes it only after the long threshold, so a
//!     flaky title read can never delete a live session's topic.
//!
//! The sweep runs at daemon startup (drains any backlog immediately) and first in every
//! cleanup cycle. `SessionEnd` remains a fast-path optimization, not the source of truth.

use super::*;
use crate::injector::{InputInjector, PaneClaudeState};
use crate::liveness::{liveness_decision, Disposition};

/// No-tmux sessions can't be liveness-probed; fall back to a short inactivity window.
const NO_TMUX_RECONCILE_TIMEOUT_HOURS: i64 = 1;

/// Cap for the one-shot startup sweep — generous so a large post-reboot backlog clears in
/// a single pass; still bounded as a runaway backstop.
const STARTUP_RECONCILE_CAP: u32 = 1000;

/// STALE-TOPICS: one-shot reconciliation run at daemon startup. See `reconcile_topics`.
pub(super) async fn reconcile_topics_startup(ctx: &HandlerContext) -> u32 {
    reconcile_topics(ctx, STARTUP_RECONCILE_CAP).await
}

/// Reconcile every topic-owning active session against live tmux/Claude state, pruning
/// (deleting/closing) the topics of sessions that are provably dead. Returns the number
/// pruned. `max_prune` bounds one sweep so a huge backlog drains in rate-limited batches
/// rather than hammering the Telegram API in a single burst.
pub(super) async fn reconcile_topics(ctx: &HandlerContext, max_prune: u32) -> u32 {
    let sessions = ctx
        .db_op(|sess| sess.get_active_sessions().unwrap_or_default())
        .await;
    if sessions.is_empty() {
        return 0;
    }

    let now = chrono::Utc::now();
    let no_tmux_cutoff = now
        - chrono::TimeDelta::try_hours(NO_TMUX_RECONCILE_TIMEOUT_HOURS)
            .unwrap_or_else(|| chrono::TimeDelta::hours(1));

    let mut pruned = 0u32;
    for session in &sessions {
        if pruned >= max_prune {
            tracing::info!(
                max_prune,
                "STALE-TOPICS: reconcile hit per-sweep cap — remainder drains next cycle"
            );
            break;
        }

        // Child (sub-agent) sessions share the parent's topic; their lifecycle follows
        // the parent (cascade end). Never evaluate or prune a child's topic on its own —
        // doing so would delete the topic out from under a live parent.
        if session.parent_session_id.is_some() {
            continue;
        }
        // Only sessions that actually own a topic are reconcilable.
        if session.thread_id.is_none() {
            continue;
        }

        let has_tmux = session.tmux_target.is_some();
        let (pane_alive, pane_reassigned, pane_state) = match session.tmux_target.as_deref() {
            Some(target) => {
                let socket = session.tmux_socket.as_deref();
                if !InputInjector::is_pane_alive(target, socket) {
                    (false, false, PaneClaudeState::Unknown)
                } else {
                    // Pane is alive — is it still OURS, and is it still Claude?
                    let t = target.to_string();
                    let sid = session.id.clone();
                    let reassigned = ctx
                        .db_op(move |sess| {
                            sess.is_tmux_target_owned_by_other(&t, &sid)
                                .unwrap_or(false)
                        })
                        .await;
                    // Skip the extra tmux call when already-superseded.
                    let state = if reassigned {
                        PaneClaudeState::Unknown
                    } else {
                        InputInjector::pane_claude_state(target, socket)
                    };
                    (true, reassigned, state)
                }
            }
            None => (false, false, PaneClaudeState::Unknown),
        };

        let no_tmux_inactive = !has_tmux
            && chrono::DateTime::parse_from_rfc3339(&session.last_activity)
                .map(|la| la.to_utc() < no_tmux_cutoff)
                .unwrap_or(false);

        match liveness_decision(
            has_tmux,
            pane_alive,
            pane_reassigned,
            pane_state,
            no_tmux_inactive,
        ) {
            Disposition::Alive => {}
            Disposition::Dead(reason) => {
                tracing::info!(
                    session_id = %session.id,
                    thread_id = ?session.thread_id,
                    reason = reason.as_str(),
                    "STALE-TOPICS: reconcile pruning dead session's topic"
                );
                cleanup::handle_stale_session_cleanup(ctx, session, reason.as_str()).await;
                pruned += 1;
                // Rate-limit Telegram delete/close calls.
                tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
            }
        }
    }

    if pruned > 0 {
        tracing::info!(pruned, "STALE-TOPICS: reconcile sweep complete");
    }
    pruned
}
