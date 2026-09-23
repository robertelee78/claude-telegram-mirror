//! ADR-024 follow-up: tool calls with long ids reach Telegram.
//!
//! Codex names some tool items `subagent-completed-<uuid>`, which put the Details
//! button's callback data over Telegram's 64 bytes. Telegram refused the whole
//! message (`BUTTON_DATA_INVALID`) and ctm dropped it — found live on 0.2.56, where a
//! merged post of several tool calls was lost to one such button. The stand-in
//! Telegram here enforces the same 64-byte rule. Own binary: process-wide env var.

mod common;

use common::{announce, config, FakeTelegram};
use ctm::daemon::Daemon;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

async fn tool_start(socket: &std::path::Path, session: &str, id: &str, n: u64) {
    let line = format!(
        r#"{{"type":"tool_start","sessionId":"{session}","timestamp":"2026-09-23T19:00:00.000Z","content":"","metadata":{{"hostSessionId":"{session}","projectDir":"/tmp/long-ids","tool":"Bash","toolUseId":"{id}","input":{{"command":"echo LONGID_{n}"}}}}}}"#
    );
    if let Ok(mut s) = UnixStream::connect(socket).await {
        let _ = s.write_all(format!("{line}\n").as_bytes()).await;
        let _ = s.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_calls_with_long_codex_ids_are_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTelegram::start().await;
    std::env::set_var("CTM_TELEGRAM_API_BASE", fake.base());
    std::env::set_var("CTM_WATCHDOG", "0");

    let mut cfg = config(dir.path());
    cfg.verbose = true;
    let socket = cfg.socket_path.clone();
    let mut daemon = Daemon::new(cfg).expect("daemon");
    daemon.start().await.expect("start");

    let session = "long-id-session";
    announce(&socket, session).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    for n in 0..12u64 {
        let id = format!("subagent-completed-01a0bdf6-b3b0-7c50-b18e-bffb0ceb{n:04}");
        tool_start(&socket, session, &id, n).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let all = |f: &FakeTelegram| (0..12u64).all(|n| f.was_delivered(&format!("LONGID_{n}")));
    while std::time::Instant::now() < deadline && !all(&fake) {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let missing: Vec<u64> = (0..12u64)
        .filter(|n| !fake.was_delivered(&format!("LONGID_{n}")))
        .collect();
    assert!(
        missing.is_empty(),
        "tool calls lost to Telegram's button limit: {missing:?}"
    );

    daemon.stop().await;
    std::env::remove_var("CTM_TELEGRAM_API_BASE");
    std::env::remove_var("CTM_WATCHDOG");
}
