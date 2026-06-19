//! Session manager integration tests.
//!
//! Exercises SessionManager through its public API with a real SQLite
//! database in a temporary directory.

use ctm::session::SessionManager;
use ctm::types::{ApprovalStatus, SessionStatus};
use tempfile::tempdir;

fn make_mgr() -> (SessionManager, tempfile::TempDir) {
    let tmp = tempdir().unwrap();
    let mgr = SessionManager::new(tmp.path(), 5).unwrap();
    (mgr, tmp)
}

#[test]
fn clear_positional_tmux_targets_migrates_only_positional() {
    // ROUTING-002: startup migration clears positional (`session:window.pane`)
    // targets but leaves stable `%N` pane ids and NULLs untouched.
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("pane-id", 1, None, None, None, Some("%24"), None)
        .unwrap();
    mgr.create_session("positional", 2, None, None, None, Some("0:1.0"), None)
        .unwrap();
    mgr.create_session("no-target", 3, None, None, None, None, None)
        .unwrap();

    let cleared = mgr.clear_positional_tmux_targets().unwrap();
    assert_eq!(cleared, 1, "only the positional row should be cleared");

    // Pane id preserved.
    assert_eq!(
        mgr.get_tmux_info("pane-id").unwrap(),
        Some(("%24".to_string(), None))
    );
    // Positional cleared to NULL (get_tmux_info maps NULL target -> None).
    assert_eq!(mgr.get_tmux_info("positional").unwrap(), None);
    // Never-set target untouched.
    assert_eq!(mgr.get_tmux_info("no-target").unwrap(), None);
}

#[test]
fn create_session_and_verify_exists() {
    let (mgr, _tmp) = make_mgr();

    let id = mgr
        .create_session(
            "integ-sess-1",
            42,
            Some("myhost"),
            Some("/project"),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(id, "integ-sess-1");

    let session = mgr.get_session("integ-sess-1").unwrap();
    assert!(session.is_some(), "Session should exist after creation");

    let session = session.unwrap();
    assert_eq!(session.id, "integ-sess-1");
    assert_eq!(session.chat_id, 42);
    assert_eq!(session.hostname.as_deref(), Some("myhost"));
    assert_eq!(session.project_dir.as_deref(), Some("/project"));
    assert_eq!(session.status, SessionStatus::Active);
}

#[test]
fn update_session_thread_id_persists() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("sess-thread", 1, None, None, None, None, None)
        .unwrap();
    mgr.set_session_thread("sess-thread", 999).unwrap();

    let session = mgr.get_session("sess-thread").unwrap().unwrap();
    assert_eq!(session.thread_id, Some(999));

    // Also verify via the dedicated getter
    let tid = mgr.get_session_thread("sess-thread").unwrap();
    assert_eq!(tid, Some(999));
}

#[test]
fn end_session_marks_ended() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("sess-end", 1, None, None, None, None, None)
        .unwrap();
    mgr.end_session("sess-end", SessionStatus::Ended).unwrap();

    let session = mgr.get_session("sess-end").unwrap().unwrap();
    assert_eq!(session.status, SessionStatus::Ended);

    // Session should not appear in active list
    let active = mgr.get_active_sessions().unwrap();
    assert!(
        active.iter().all(|s| s.id != "sess-end"),
        "Ended session should not be in active list"
    );
}

#[test]
fn multiple_sessions_coexist() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("coexist-1", 1, None, None, None, None, None)
        .unwrap();
    mgr.create_session("coexist-2", 2, None, None, None, None, None)
        .unwrap();
    mgr.create_session("coexist-3", 3, None, None, None, None, None)
        .unwrap();

    let active = mgr.get_active_sessions().unwrap();
    assert_eq!(active.len(), 3, "All three sessions should be active");

    // End one and verify the others remain
    mgr.end_session("coexist-2", SessionStatus::Ended).unwrap();
    let active = mgr.get_active_sessions().unwrap();
    assert_eq!(active.len(), 2, "Two sessions should remain active");

    let ids: Vec<&str> = active.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"coexist-1"));
    assert!(ids.contains(&"coexist-3"));
    assert!(!ids.contains(&"coexist-2"));
}

#[test]
fn pending_approval_lifecycle() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("sess-appr", 1, None, None, None, None, None)
        .unwrap();

    // Create approval
    let approval_id = mgr
        .create_approval("sess-appr", "Allow Bash: rm -rf /tmp/test?", Some(12345))
        .unwrap();
    assert!(
        approval_id.starts_with("approval-"),
        "Approval ID should have 'approval-' prefix, got: {}",
        approval_id
    );

    // Verify it exists and is pending
    let approval = mgr.get_approval(&approval_id).unwrap().unwrap();
    assert_eq!(approval.status, ApprovalStatus::Pending);
    assert_eq!(approval.session_id, "sess-appr");

    // Resolve it
    let resolved = mgr
        .resolve_approval(&approval_id, ApprovalStatus::Approved)
        .unwrap();
    assert!(resolved, "resolve_approval should return true");

    // Verify resolved status
    let approval = mgr.get_approval(&approval_id).unwrap().unwrap();
    assert_eq!(approval.status, ApprovalStatus::Approved);

    // Cannot resolve again
    let re_resolved = mgr
        .resolve_approval(&approval_id, ApprovalStatus::Rejected)
        .unwrap();
    assert!(
        !re_resolved,
        "Already-resolved approval should not be re-resolved"
    );
}

#[test]
fn end_session_expires_pending_approvals() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("sess-exp", 1, None, None, None, None, None)
        .unwrap();
    let aid = mgr
        .create_approval("sess-exp", "Allow Write?", None)
        .unwrap();

    // End session should expire pending approvals
    mgr.end_session("sess-exp", SessionStatus::Ended).unwrap();

    let approval = mgr.get_approval(&aid).unwrap().unwrap();
    assert_eq!(
        approval.status,
        ApprovalStatus::Expired,
        "Pending approval should be expired when session ends"
    );
}

#[test]
fn reactivate_ended_session() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("sess-react", 1, None, None, None, None, None)
        .unwrap();
    mgr.end_session("sess-react", SessionStatus::Ended).unwrap();

    let session = mgr.get_session("sess-react").unwrap().unwrap();
    assert_eq!(session.status, SessionStatus::Ended);

    mgr.reactivate_session("sess-react").unwrap();

    let session = mgr.get_session("sess-react").unwrap().unwrap();
    assert_eq!(session.status, SessionStatus::Active);
}

#[test]
fn stats_reflect_current_state() {
    let (mgr, _tmp) = make_mgr();

    let (active, pending) = mgr.get_stats().unwrap();
    assert_eq!(active, 0);
    assert_eq!(pending, 0);

    mgr.create_session("s1", 1, None, None, None, None, None)
        .unwrap();
    mgr.create_session("s2", 2, None, None, None, None, None)
        .unwrap();
    mgr.create_approval("s1", "approve?", None).unwrap();

    let (active, pending) = mgr.get_stats().unwrap();
    assert_eq!(active, 2);
    assert_eq!(pending, 1);
}

#[test]
fn tmux_info_round_trip() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("sess-tmux", 1, None, None, None, None, None)
        .unwrap();
    mgr.set_tmux_info("sess-tmux", Some("s:0.0"), Some("/tmp/tmux-1000/default"))
        .unwrap();

    let info = mgr.get_tmux_info("sess-tmux").unwrap().unwrap();
    assert_eq!(info.0, "s:0.0");
    assert_eq!(info.1.as_deref(), Some("/tmp/tmux-1000/default"));
}

#[test]
fn create_session_with_all_fields() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session(
        "full-sess",
        100,
        Some("builder-host"),
        Some("/workspace"),
        Some(42),
        Some("s0:0.1"),
        Some("/tmp/tmux-1234/default"),
    )
    .unwrap();

    let session = mgr.get_session("full-sess").unwrap().unwrap();
    assert_eq!(session.chat_id, 100);
    assert_eq!(session.hostname.as_deref(), Some("builder-host"));
    assert_eq!(session.project_dir.as_deref(), Some("/workspace"));
    assert_eq!(session.thread_id, Some(42));
    assert_eq!(session.tmux_target.as_deref(), Some("s0:0.1"));
    assert_eq!(
        session.tmux_socket.as_deref(),
        Some("/tmp/tmux-1234/default")
    );
}

// ---- tests merged from inline #[cfg(test)] module (Story 13.6) ----

#[test]
fn duplicate_create_updates_activity() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("sess-dup", 1, None, None, None, None, None)
        .unwrap();
    let s1 = mgr.get_session("sess-dup").unwrap().unwrap();

    // create again returns same id
    let id2 = mgr
        .create_session("sess-dup", 1, None, None, None, None, None)
        .unwrap();
    assert_eq!(id2, "sess-dup");

    let s2 = mgr.get_session("sess-dup").unwrap().unwrap();
    assert!(s2.last_activity >= s1.last_activity);
}

#[test]
fn clear_thread_id_works() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("sess-ct", 1, None, None, None, None, None)
        .unwrap();
    mgr.set_session_thread("sess-ct", 777).unwrap();
    mgr.clear_thread_id("sess-ct").unwrap();

    assert!(mgr.get_session_by_thread_id(777).unwrap().is_none());
}

#[test]
fn get_session_thread_none_then_set() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("sess-gst", 1, None, None, None, None, None)
        .unwrap();

    // No thread_id set yet
    assert_eq!(mgr.get_session_thread("sess-gst").unwrap(), None);

    mgr.set_session_thread("sess-gst", 555).unwrap();
    assert_eq!(mgr.get_session_thread("sess-gst").unwrap(), Some(555));

    // Non-existent session returns None
    assert_eq!(mgr.get_session_thread("no-such").unwrap(), None);
}

#[test]
fn get_session_by_chat_id() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("sess-chat", 55, None, None, None, None, None)
        .unwrap();

    let sess = mgr.get_session_by_chat_id(55).unwrap().unwrap();
    assert_eq!(sess.id, "sess-chat");

    assert!(mgr.get_session_by_chat_id(999).unwrap().is_none());
}

#[test]
fn tmux_target_ownership() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("s1", 1, None, None, None, None, None)
        .unwrap();
    mgr.create_session("s2", 1, None, None, None, None, None)
        .unwrap();
    mgr.set_tmux_info("s1", Some("target:0.0"), None).unwrap();

    assert!(mgr
        .is_tmux_target_owned_by_other("target:0.0", "s2")
        .unwrap());
    assert!(!mgr
        .is_tmux_target_owned_by_other("target:0.0", "s1")
        .unwrap());
}

#[test]
fn stale_candidates_returns_old_sessions() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("old-1", 1, None, None, None, None, None)
        .unwrap();

    // Use test helper to set last_activity to the past
    mgr.test_set_last_activity("old-1", "2020-01-01T00:00:00.000Z")
        .unwrap();

    let stale = mgr.get_stale_session_candidates(1).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].id, "old-1");
}

#[test]
fn orphaned_thread_sessions() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("orph-1", 1, None, None, None, None, None)
        .unwrap();
    mgr.set_session_thread("orph-1", 888).unwrap();
    mgr.end_session("orph-1", SessionStatus::Ended).unwrap();

    let orphans = mgr.get_orphaned_thread_sessions().unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, "orph-1");
}

#[test]
fn cleanup_old_sessions_removes_ancient() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("ancient", 1, None, None, None, None, None)
        .unwrap();

    // Use test helper to set last_activity to the past
    mgr.test_set_last_activity("ancient", "2020-01-01T00:00:00.000Z")
        .unwrap();

    let removed = mgr.cleanup_old_sessions(7).unwrap();
    assert_eq!(removed, 1);
    assert!(mgr.get_session("ancient").unwrap().is_none());
}

// ======================================================================
// ADR-014 PR-A: lifecycle correctness
// ======================================================================

/// ADR-014 A5: the custom title (set via /rename) is persisted to the DB and
/// survives reload, so a resume after a daemon restart recovers the name.
/// Hypothesis: set_custom_title writes a value get_session can read back, and it
/// outlives end_session (the row is not deleted on end).
#[test]
fn custom_title_persists_and_survives_end() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("title-sess", 1, None, None, None, None, None)
        .unwrap();

    // No title initially.
    assert_eq!(
        mgr.get_session("title-sess").unwrap().unwrap().custom_title,
        None
    );

    mgr.set_custom_title("title-sess", "My Feature Work")
        .unwrap();
    assert_eq!(
        mgr.get_session("title-sess")
            .unwrap()
            .unwrap()
            .custom_title
            .as_deref(),
        Some("My Feature Work")
    );

    // The title must survive a true session end (the row persists; resume reads it).
    mgr.end_session("title-sess", SessionStatus::Ended).unwrap();
    assert_eq!(
        mgr.get_session("title-sess")
            .unwrap()
            .unwrap()
            .custom_title
            .as_deref(),
        Some("My Feature Work")
    );
}

/// ADR-014 A5: a custom title set on one SessionManager is visible to a freshly
/// opened SessionManager over the same DB file — proving DB persistence (not just
/// the in-memory cache), which is the daemon-restart scenario.
#[test]
fn custom_title_visible_across_reopen() {
    let tmp = tempdir().unwrap();
    {
        let mgr = SessionManager::new(tmp.path(), 5).unwrap();
        mgr.create_session("reopen-sess", 7, None, None, None, None, None)
            .unwrap();
        mgr.set_custom_title("reopen-sess", "Persisted Title")
            .unwrap();
    }
    // Reopen — mimics a daemon restart with an empty in-memory cache.
    let mgr2 = SessionManager::new(tmp.path(), 5).unwrap();
    assert_eq!(
        mgr2.get_session("reopen-sess")
            .unwrap()
            .unwrap()
            .custom_title
            .as_deref(),
        Some("Persisted Title")
    );
}

/// ADR-014 A4: the DB invariant behind immediate teardown — once a topic is
/// deleted, clearing the thread_id makes get_session report no thread_id, so a
/// later resume cannot target a stale (deleted) topic.
#[test]
fn clear_thread_id_drops_stale_topic_mapping() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("teardown-sess", 1, None, None, None, None, None)
        .unwrap();
    mgr.set_session_thread("teardown-sess", 54321).unwrap();
    assert_eq!(
        mgr.get_session("teardown-sess").unwrap().unwrap().thread_id,
        Some(54321)
    );

    // Simulate A4's synchronous clear after delete_forum_topic.
    mgr.clear_thread_id("teardown-sess").unwrap();
    assert_eq!(
        mgr.get_session("teardown-sess").unwrap().unwrap().thread_id,
        None
    );
}

// ======================================================================
// ADR-014 PR-B: approval reliability (DB invariants)
// ======================================================================

/// ADR-014 B2: resolve_approval is idempotent — the first call transitions the
/// pending row and returns true; a second call (a double-tap) returns false, so
/// the handler knows not to emit a second ApprovalResponse to the hook.
#[test]
fn resolve_approval_is_idempotent() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("appr-sess", 1, None, None, None, None, None)
        .unwrap();
    let aid = mgr
        .create_approval("appr-sess", "run rm -rf", None)
        .unwrap();

    // First tap transitions the row.
    assert!(mgr
        .resolve_approval(&aid, ApprovalStatus::Approved)
        .unwrap());
    // Second tap (double-tap / restart race) does NOT transition again.
    assert!(!mgr
        .resolve_approval(&aid, ApprovalStatus::Approved)
        .unwrap());
    // A different decision also cannot re-transition an already-resolved row.
    assert!(!mgr
        .resolve_approval(&aid, ApprovalStatus::Rejected)
        .unwrap());
}

/// ADR-014 B4: pending_approval_ids reflects only still-pending approvals, so the
/// cleanup sweep can evict orphaned in-memory client entries.
#[test]
fn pending_approval_ids_tracks_status() {
    let (mgr, _tmp) = make_mgr();
    mgr.create_session("sweep-sess", 1, None, None, None, None, None)
        .unwrap();
    let a1 = mgr.create_approval("sweep-sess", "cmd1", None).unwrap();
    let a2 = mgr.create_approval("sweep-sess", "cmd2", None).unwrap();

    let mut ids = mgr.pending_approval_ids().unwrap();
    ids.sort();
    let mut expected = vec![a1.clone(), a2.clone()];
    expected.sort();
    assert_eq!(ids, expected);

    // Resolving one drops it from the pending set.
    mgr.resolve_approval(&a1, ApprovalStatus::Approved).unwrap();
    let ids = mgr.pending_approval_ids().unwrap();
    assert_eq!(ids, vec![a2]);
}

/// ADR-014 B5: get_approval returns None for an unknown id (daemon restarted /
/// expired), which the callback handler treats as a stale request rather than
/// crashing.
#[test]
fn get_approval_unknown_id_is_none() {
    let (mgr, _tmp) = make_mgr();
    assert!(mgr
        .get_approval("approval-does-not-exist")
        .unwrap()
        .is_none());
}
