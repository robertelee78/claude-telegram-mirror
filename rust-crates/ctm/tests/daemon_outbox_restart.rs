//! ADR-024: messages waiting for Telegram survive a daemon restart.
//!
//! The outbox is saved to disk while it changes and on shutdown, and reloaded on
//! start. Here Telegram refuses everything (429) while three replies are queued, the
//! daemon is stopped, Telegram recovers, and a fresh daemon on the same config
//! directory must deliver them. Own binary: the API override is process-wide.

mod common;

use common::{announce, config, FakeTelegram, SendBehaviour};
use ctm::daemon::Daemon;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

async fn reply(socket: &std::path::Path, session: &str, marker: &str) {
    let line = format!(
        r#"{{"type":"agent_response","sessionId":"{session}","timestamp":"2026-09-23T19:00:00.000Z","content":"{marker}","metadata":{{"hostSessionId":"{session}","projectDir":"/tmp/restart"}}}}"#
    );
    if let Ok(mut s) = UnixStream::connect(socket).await {
        let _ = s.write_all(format!("{line}\n").as_bytes()).await;
        let _ = s.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_messages_are_delivered_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTelegram::start().await;
    std::env::set_var("CTM_TELEGRAM_API_BASE", fake.base());
    std::env::set_var("CTM_WATCHDOG", "0");

    let session = "restart-session";
    {
        let cfg = config(dir.path());
        let socket = cfg.socket_path.clone();
        let mut daemon = Daemon::new(cfg).expect("daemon");
        daemon.start().await.expect("start");
        announce(&socket, session).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        // Telegram starts refusing; the replies can only wait.
        fake.set_send(SendBehaviour::RateLimited { retry_after: 120 });
        for i in 0..3 {
            reply(&socket, session, &format!("SURVIVES_RESTART_{i}")).await;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            !fake.was_delivered("SURVIVES_RESTART_0"),
            "precondition: nothing got through while Telegram was refusing"
        );
        daemon.stop().await;
    }
    assert!(
        dir.path().join("outbox.json").exists(),
        "the waiting messages were saved"
    );

    // Telegram recovers; a new daemon starts on the same config directory.
    fake.set_send(SendBehaviour::Ok);
    let cfg = config(dir.path());
    let mut daemon = Daemon::new(cfg).expect("daemon");
    daemon.start().await.expect("start");

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let markers: Vec<String> = (0..3).map(|i| format!("SURVIVES_RESTART_{i}")).collect();
    while std::time::Instant::now() < deadline && !markers.iter().all(|m| fake.was_delivered(m)) {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let missing: Vec<&String> = markers.iter().filter(|m| !fake.was_delivered(m)).collect();
    assert!(missing.is_empty(), "lost across the restart: {missing:?}");

    daemon.stop().await;
    std::env::remove_var("CTM_TELEGRAM_API_BASE");
    std::env::remove_var("CTM_WATCHDOG");
}
