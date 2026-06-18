//! STALE-TOPICS: the pure policy for deciding whether a Claude session's Telegram topic
//! is still live, shared by the daemon's reconciliation sweep (`daemon::reconcile`) and
//! `ctm doctor --fix`. Keeping the decision here — free of tmux/DB/bot I/O — means both
//! call sites apply exactly the same rule and it can be exhaustively unit-tested.
//!
//! See `daemon::reconcile` for the rationale behind keying liveness on the *specific
//! Claude session* (pane-alive alone is insufficient).

use crate::injector::PaneClaudeState;

/// Why a session's topic is being pruned (for logs and the close-mode notice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadReason {
    PaneGone,
    Reassigned,
    ClaudeExited,
    NoTmuxInactive,
}

impl DeadReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DeadReason::PaneGone => "tmux pane no longer exists",
            DeadReason::Reassigned => "pane reassigned to a newer session",
            DeadReason::ClaudeExited => "Claude exited (pane back at a shell prompt)",
            DeadReason::NoTmuxInactive => "no tmux route and inactive",
        }
    }
}

/// Disposition of a single session's topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Alive,
    Dead(DeadReason),
}

/// PURE liveness decision — no tmux/DB/bot I/O, so it is exhaustively unit-testable.
/// The caller gathers the facts; this function encodes the policy:
///
///   * no tmux route        → only inactivity can declare it dead
///   * pane gone            → dead
///   * pane reassigned      → dead (a newer session-id took the pane)
///   * pane at a shell      → dead (Claude exited)
///   * RunningClaude        → ALIVE, regardless of idle time
///   * Unknown              → ALIVE here (never risk a false death on a flaky title; the
///     daemon's inactivity backstop handles it only after the long threshold)
pub fn liveness_decision(
    has_tmux: bool,
    pane_alive: bool,
    pane_reassigned: bool,
    pane_state: PaneClaudeState,
    no_tmux_inactive_beyond_timeout: bool,
) -> Disposition {
    if !has_tmux {
        return if no_tmux_inactive_beyond_timeout {
            Disposition::Dead(DeadReason::NoTmuxInactive)
        } else {
            Disposition::Alive
        };
    }
    if !pane_alive {
        return Disposition::Dead(DeadReason::PaneGone);
    }
    if pane_reassigned {
        return Disposition::Dead(DeadReason::Reassigned);
    }
    if pane_state == PaneClaudeState::Shell {
        return Disposition::Dead(DeadReason::ClaudeExited);
    }
    Disposition::Alive
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_pane_is_pruned() {
        assert_eq!(
            liveness_decision(true, false, false, PaneClaudeState::Unknown, false),
            Disposition::Dead(DeadReason::PaneGone)
        );
    }

    #[test]
    fn reassigned_pane_is_pruned_even_if_claude_running() {
        assert_eq!(
            liveness_decision(true, true, true, PaneClaudeState::RunningClaude, false),
            Disposition::Dead(DeadReason::Reassigned)
        );
    }

    #[test]
    fn pane_fell_back_to_shell_is_pruned() {
        assert_eq!(
            liveness_decision(true, true, false, PaneClaudeState::Shell, false),
            Disposition::Dead(DeadReason::ClaudeExited)
        );
    }

    #[test]
    fn running_claude_is_never_pruned_however_idle() {
        assert_eq!(
            liveness_decision(true, true, false, PaneClaudeState::RunningClaude, true),
            Disposition::Alive
        );
    }

    #[test]
    fn unknown_pane_state_is_kept() {
        assert_eq!(
            liveness_decision(true, true, false, PaneClaudeState::Unknown, false),
            Disposition::Alive
        );
    }

    #[test]
    fn no_tmux_session_pruned_only_after_inactivity() {
        assert_eq!(
            liveness_decision(false, false, false, PaneClaudeState::Unknown, false),
            Disposition::Alive
        );
        assert_eq!(
            liveness_decision(false, false, false, PaneClaudeState::Unknown, true),
            Disposition::Dead(DeadReason::NoTmuxInactive)
        );
    }
}
