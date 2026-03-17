//! Socket round-trip integration tests.
//!
//! Exercises SocketServer + SocketClient through a real Unix domain socket
//! in a temporary directory, verifying NDJSON framing and flock semantics.

use ctm::socket::SocketClient;
use ctm::socket::SocketServer;
use ctm::types::{BridgeMessage, MessageType};
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;
use tokio::time::Duration;

#[tokio::test]
async fn server_client_message_roundtrip() {
    let tmp = tempdir().unwrap();
    let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700));
    let sock = tmp.path().join("bridge.sock");
    let pid = tmp.path().join("bridge.pid");

    let mut server = SocketServer::new(&sock, &pid);
    server.listen().await.unwrap();
    let mut rx = server.subscribe();

    // Allow accept loop to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = SocketClient::new();
    client.connect(&sock).await.unwrap();
    assert!(client.is_connected());

    let msg = BridgeMessage {
        msg_type: MessageType::AgentResponse,
        session_id: "test-session-1".to_string(),
        timestamp: "2024-01-01T00:00:00.000Z".to_string(),
        content: "Hello from integration test".to_string(),
        metadata: None,
    };
    client.send(&msg).await.unwrap();

    // Server receives the message via broadcast channel
    let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for message")
        .expect("broadcast recv error");

    assert_eq!(received.session_id, "test-session-1");
    assert_eq!(received.content, "Hello from integration test");
    assert_eq!(received.msg_type, MessageType::AgentResponse);

    client.disconnect();
    assert!(!client.is_connected());
    server.close().await;
}

#[tokio::test]
async fn flock_prevents_second_server() {
    let tmp = tempdir().unwrap();
    // Ensure tempdir is accessible despite process-global umask(0o177)
    // leaking from parallel SocketServer::listen() calls.
    let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700));
    let sock = tmp.path().join("bridge.sock");
    let pid = tmp.path().join("bridge.pid");

    let mut server1 = SocketServer::new(&sock, &pid);
    server1.listen().await.unwrap();

    // Second server on same paths should fail due to flock
    let sock2 = tmp.path().join("bridge2.sock");
    let mut server2 = SocketServer::new(&sock2, &pid);
    let result = server2.listen().await;
    assert!(
        result.is_err(),
        "Second server should fail to acquire flock on same PID file"
    );

    server1.close().await;
}

#[tokio::test]
async fn socket_file_cleaned_on_server_drop() {
    let tmp = tempdir().unwrap();
    let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700));
    let sock = tmp.path().join("cleanup.sock");
    let pid = tmp.path().join("cleanup.pid");

    {
        let mut server = SocketServer::new(&sock, &pid);
        server.listen().await.unwrap();
        assert!(
            sock.exists(),
            "Socket file should exist while server is alive"
        );
        // server dropped here
    }

    // After drop, socket file should be cleaned up
    assert!(
        !sock.exists(),
        "Socket file should be removed after server drop"
    );
}

#[tokio::test]
async fn multiple_messages_received_in_order() {
    let tmp = tempdir().unwrap();
    let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700));
    let sock = tmp.path().join("order.sock");
    let pid = tmp.path().join("order.pid");

    let mut server = SocketServer::new(&sock, &pid);
    server.listen().await.unwrap();
    let mut rx = server.subscribe();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = SocketClient::new();
    client.connect(&sock).await.unwrap();

    for i in 0..5 {
        let msg = BridgeMessage {
            msg_type: MessageType::ToolResult,
            session_id: format!("sess-{}", i),
            timestamp: "2024-01-01T00:00:00.000Z".to_string(),
            content: format!("message-{}", i),
            metadata: None,
        };
        client.send(&msg).await.unwrap();
    }

    for i in 0..5 {
        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out")
            .expect("recv error");
        assert_eq!(received.session_id, format!("sess-{}", i));
        assert_eq!(received.content, format!("message-{}", i));
    }

    client.disconnect();
    server.close().await;
}

#[tokio::test]
async fn client_connect_to_missing_socket_fails() {
    let tmp = tempdir().unwrap();
    let sock = tmp.path().join("nonexistent.sock");

    let mut client = SocketClient::new();
    let result = client.connect(&sock).await;
    assert!(
        result.is_err(),
        "Connecting to non-existent socket should fail"
    );
}
