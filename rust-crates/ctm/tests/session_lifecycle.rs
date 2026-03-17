//! Session manager integration tests.
//!
//! Exercises SessionManager through its public API with a real SQLite
//! database in a temporary directory.

use ctm::session::SessionManager;
use tempfile::tempdir;

fn make_mgr() -> (SessionManager, tempfile::TempDir) {
    let tmp = tempdir().unwrap();
    let mgr = SessionManager::new(tmp.path(), 5).unwrap();
    (mgr, tmp)
}

#[test]
fn create_session_and_verify_exists() {
    let (mgr, _tmp) = make_mgr();

    let id = mgr
        .create_session("integ-sess-1", 42, Some("myhost"), Some("/project"), None, None, None)
        .unwrap();
    assert_eq!(id, "integ-sess-1");

    let session = mgr.get_session("integ-sess-1").unwrap();
    assert!(session.is_some(), "Session should exist after creation");

    let session = session.unwrap();
    assert_eq!(session.id, "integ-sess-1");
    assert_eq!(session.chat_id, 42);
    assert_eq!(session.hostname.as_deref(), Some("myhost"));
    assert_eq!(session.project_dir.as_deref(), Some("/project"));
    assert_eq!(session.status, "active");
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
    mgr.end_session("sess-end", "ended").unwrap();

    let session = mgr.get_session("sess-end").unwrap().unwrap();
    assert_eq!(session.status, "ended");

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
    mgr.end_session("coexist-2", "ended").unwrap();
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
    assert_eq!(approval.status, "pending");
    assert_eq!(approval.session_id, "sess-appr");

    // Resolve it
    let resolved = mgr.resolve_approval(&approval_id, "approved").unwrap();
    assert!(resolved, "resolve_approval should return true");

    // Verify resolved status
    let approval = mgr.get_approval(&approval_id).unwrap().unwrap();
    assert_eq!(approval.status, "approved");

    // Cannot resolve again
    let re_resolved = mgr.resolve_approval(&approval_id, "rejected").unwrap();
    assert!(!re_resolved, "Already-resolved approval should not be re-resolved");
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
    mgr.end_session("sess-exp", "ended").unwrap();

    let approval = mgr.get_approval(&aid).unwrap().unwrap();
    assert_eq!(
        approval.status, "expired",
        "Pending approval should be expired when session ends"
    );
}

#[test]
fn reactivate_ended_session() {
    let (mgr, _tmp) = make_mgr();

    mgr.create_session("sess-react", 1, None, None, None, None, None)
        .unwrap();
    mgr.end_session("sess-react", "ended").unwrap();

    let session = mgr.get_session("sess-react").unwrap().unwrap();
    assert_eq!(session.status, "ended");

    mgr.reactivate_session("sess-react").unwrap();

    let session = mgr.get_session("sess-react").unwrap().unwrap();
    assert_eq!(session.status, "active");
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
    assert_eq!(session.tmux_socket.as_deref(), Some("/tmp/tmux-1234/default"));
}
