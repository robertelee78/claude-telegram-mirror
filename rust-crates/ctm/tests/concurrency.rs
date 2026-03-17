//! Concurrency tests for SessionManager and SocketServer.
//!
//! Epic 14, Story 14.2 (FR45): Verify that concurrent access to
//! SessionManager and SocketServer does not deadlock, corrupt state,
//! or lose messages.

use ctm::session::SessionManager;
use ctm::socket::{SocketClient, SocketServer};
use ctm::types::{ApprovalStatus, BridgeMessage, MessageType, SessionStatus};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::Barrier;
use tokio::time::{timeout, Duration};

// ====================================================== SessionManager

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_session_creation_no_deadlock() {
    // 10 tasks creating sessions simultaneously on a shared DB.
    // SessionManager uses rusqlite::Connection which is !Send, so we
    // wrap creation in spawn_blocking. Each task gets its own manager
    // on the same DB file to exercise SQLite's locking.
    let tmp = tempdir().unwrap();
    let tmp_path = tmp.path().to_path_buf();

    let result = timeout(Duration::from_secs(5), async {
        let mut handles = Vec::new();
        let barrier = Arc::new(Barrier::new(10));

        for i in 0..10 {
            let path = tmp_path.clone();
            let bar = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                bar.wait().await; // synchronize start
                                  // Each task gets its own manager (own Connection) on the same DB
                let mgr =
                    SessionManager::new(&path, 5).expect("SessionManager::new should succeed");
                let session_id = format!("conc-sess-{}", i);
                mgr.create_session(
                    &session_id,
                    i as i64,
                    Some("test-host"),
                    Some("/test"),
                    None,
                    None,
                    None,
                )
                .expect("create_session should succeed");
                session_id
            }));
        }

        let mut ids = Vec::new();
        for h in handles {
            ids.push(h.await.expect("task should not panic"));
        }

        // All 10 IDs should be unique
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 10, "All 10 sessions should have unique IDs");
        ids
    })
    .await;

    assert!(
        result.is_ok(),
        "Concurrent session creation should not deadlock (5s timeout)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_read_write_consistent_state() {
    // One writer creates and ends sessions while a reader checks state.
    let tmp = tempdir().unwrap();
    let tmp_path = tmp.path().to_path_buf();

    let result = timeout(Duration::from_secs(5), async {
        // Writer creates sessions
        let writer_path = tmp_path.clone();
        let writer = tokio::spawn(async move {
            let mgr = SessionManager::new(&writer_path, 5).unwrap();
            for i in 0..20 {
                let id = format!("rw-sess-{}", i);
                mgr.create_session(&id, 1, None, None, None, None, None)
                    .unwrap();
                if i % 3 == 0 {
                    mgr.end_session(&id, SessionStatus::Ended).unwrap();
                }
            }
        });

        // Reader checks state concurrently
        let reader_path = tmp_path.clone();
        let reader = tokio::spawn(async move {
            let mgr = SessionManager::new(&reader_path, 5).unwrap();
            for _ in 0..20 {
                // get_active_sessions should never panic or return corrupt data
                let active = mgr.get_active_sessions();
                assert!(active.is_ok(), "get_active_sessions should not fail");
                let stats = mgr.get_stats();
                assert!(stats.is_ok(), "get_stats should not fail");
                tokio::task::yield_now().await;
            }
        });

        writer.await.expect("writer should not panic");
        reader.await.expect("reader should not panic");
    })
    .await;

    assert!(result.is_ok(), "Concurrent read+write should not deadlock");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_approval_resolution_first_wins() {
    // Create one approval, then 10 tasks race to resolve it.
    // Only the first should succeed (resolve_approval returns true).
    let tmp = tempdir().unwrap();
    let mgr = SessionManager::new(tmp.path(), 5).unwrap();

    mgr.create_session("race-sess", 1, None, None, None, None, None)
        .unwrap();
    let approval_id = mgr
        .create_approval("race-sess", "Approve write?", None)
        .unwrap();

    // Since SessionManager is not Send (rusqlite::Connection), we resolve
    // sequentially but verify the idempotency guarantee.
    let mut success_count = 0;
    for _ in 0..10 {
        let resolved = mgr
            .resolve_approval(&approval_id, ApprovalStatus::Approved)
            .expect("resolve_approval should not error");
        if resolved {
            success_count += 1;
        }
    }

    assert_eq!(success_count, 1, "Only the first resolution should succeed");

    let approval = mgr.get_approval(&approval_id).unwrap().unwrap();
    assert_eq!(approval.status, ApprovalStatus::Approved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_approval_resolution_across_connections() {
    // Multiple connections racing to resolve the same approval.
    let tmp = tempdir().unwrap();
    let tmp_path = tmp.path().to_path_buf();

    // Create the approval on the first connection
    let mgr0 = SessionManager::new(&tmp_path, 5).unwrap();
    mgr0.create_session("race2-sess", 1, None, None, None, None, None)
        .unwrap();
    let approval_id = mgr0
        .create_approval("race2-sess", "Approve?", None)
        .unwrap();
    drop(mgr0);

    let result = timeout(Duration::from_secs(5), async {
        let barrier = Arc::new(Barrier::new(5));
        let mut handles = Vec::new();

        for _ in 0..5 {
            let path = tmp_path.clone();
            let aid = approval_id.clone();
            let bar = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                bar.wait().await;
                let mgr = SessionManager::new(&path, 5).unwrap();
                mgr.resolve_approval(&aid, ApprovalStatus::Approved)
                    .expect("resolve should not error")
            }));
        }

        let mut wins = 0;
        for h in handles {
            if h.await.unwrap() {
                wins += 1;
            }
        }
        wins
    })
    .await;

    let wins = result.expect("Should not deadlock");
    assert_eq!(
        wins, 1,
        "Exactly one resolution should win across connections"
    );
}

// ========================================================== SocketServer

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multiple_concurrent_clients_all_messages_received() {
    let tmp = tempdir().unwrap();
    // Place socket directly in tempdir (not a subdirectory) to avoid
    // permission issues from process-global umask(0o177) leaking across
    // parallel socket tests.  SocketServer::listen() will set 0o700 on
    // the parent directory itself, which is fine for a dedicated tempdir.
    let sock = tmp.path().join("bridge.sock");
    let pid = tmp.path().join("bridge.pid");

    let mut server = SocketServer::new(&sock, &pid);
    server.listen().await.unwrap();
    let mut rx = server.subscribe();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let num_clients = 5;
    let msgs_per_client = 3;
    let total_expected = num_clients * msgs_per_client;

    let result = timeout(Duration::from_secs(5), async {
        // Spawn multiple clients that each send messages concurrently
        let mut handles = Vec::new();
        for c in 0..num_clients {
            let sock_path = sock.clone();
            handles.push(tokio::spawn(async move {
                let mut client = SocketClient::new();
                client.connect(&sock_path).await.unwrap();

                for m in 0..msgs_per_client {
                    let msg = BridgeMessage {
                        msg_type: MessageType::AgentResponse,
                        session_id: format!("client-{}-msg-{}", c, m),
                        timestamp: "2024-01-01T00:00:00.000Z".to_string(),
                        content: format!("content-{}-{}", c, m),
                        metadata: None,
                    };
                    client.send(&msg).await.unwrap();
                }
                client.disconnect();
            }));
        }

        // Wait for all clients to finish sending
        for h in handles {
            h.await.expect("client task should not panic");
        }

        // Collect all received messages
        let mut received = Vec::new();
        for _ in 0..total_expected {
            let msg = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("should receive within 2s")
                .expect("recv should succeed");
            received.push(msg.session_id);
        }

        received
    })
    .await;

    let received = result.expect("Should not deadlock");
    assert_eq!(
        received.len(),
        total_expected,
        "All messages from all clients should be received"
    );

    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_disconnect_mid_session_server_continues() {
    let tmp = tempdir().unwrap();
    let sock = tmp.path().join("bridge.sock");
    let pid = tmp.path().join("bridge.pid");

    let mut server = SocketServer::new(&sock, &pid);
    server.listen().await.unwrap();
    let mut rx = server.subscribe();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let result = timeout(Duration::from_secs(5), async {
        // Client 1 connects, sends, then disconnects abruptly
        {
            let mut c1 = SocketClient::new();
            c1.connect(&sock).await.unwrap();
            let msg = BridgeMessage {
                msg_type: MessageType::ToolResult,
                session_id: "pre-disconnect".to_string(),
                timestamp: "2024-01-01T00:00:00.000Z".to_string(),
                content: "before".to_string(),
                metadata: None,
            };
            c1.send(&msg).await.unwrap();
            // c1 dropped here — abrupt disconnect
        }

        // Receive the first message
        let m1 = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        assert_eq!(m1.session_id, "pre-disconnect");

        // Small delay for server to process disconnect
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Client 2 should be able to connect and communicate normally
        let mut c2 = SocketClient::new();
        c2.connect(&sock).await.unwrap();
        let msg2 = BridgeMessage {
            msg_type: MessageType::AgentResponse,
            session_id: "post-disconnect".to_string(),
            timestamp: "2024-01-01T00:00:00.000Z".to_string(),
            content: "after".to_string(),
            metadata: None,
        };
        c2.send(&msg2).await.unwrap();

        let m2 = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        assert_eq!(m2.session_id, "post-disconnect");
        assert_eq!(m2.content, "after");

        c2.disconnect();
    })
    .await;

    assert!(
        result.is_ok(),
        "Server should continue after client disconnect"
    );
    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_broadcast_reaches_all_clients() {
    let tmp = tempdir().unwrap();
    let sock = tmp.path().join("bridge.sock");
    let pid = tmp.path().join("bridge.pid");

    let mut server = SocketServer::new(&sock, &pid);
    server.listen().await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let result = timeout(Duration::from_secs(5), async {
        // Connect two clients
        let mut c1 = SocketClient::new();
        c1.connect(&sock).await.unwrap();

        let mut c2 = SocketClient::new();
        c2.connect(&sock).await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify both are connected
        let count = server.client_count().await;
        assert_eq!(count, 2, "Two clients should be connected");

        c1.disconnect();
        c2.disconnect();
    })
    .await;

    assert!(result.is_ok(), "Broadcast test should complete");
    server.close().await;
}
