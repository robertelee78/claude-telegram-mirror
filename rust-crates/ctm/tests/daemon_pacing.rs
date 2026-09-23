//! ADR-024: ctm must pace itself under Telegram's group limit.
//!
//! The counterpart to `daemon_budget.rs`. In its own binary because the API override
//! is an environment variable and cargo runs a binary's tests in parallel threads.

mod common;

use common::{announce, config, FakeTelegram};
use ctm::daemon::Daemon;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

async fn send_event(socket: &std::path::Path, line: String) {
    if let Ok(mut s) = UnixStream::connect(socket).await {
        let _ = s.write_all(format!("{line}\n").as_bytes()).await;
        let _ = s.shutdown().await;
    }
}

fn reply_event(session: &str, marker: &str) -> String {
    format!(
        r#"{{"type":"agent_response","sessionId":"{session}","timestamp":"2026-09-23T19:00:00.000Z","content":"{marker}","metadata":{{"hostSessionId":"{session}","projectDir":"/tmp/budget-test"}}}}"#
    )
}

/// The other half: ctm must *pace itself* under the limit rather than blasting and
/// being refused. Before ADR-024 the governor allowed the configured number of
/// messages every **second**, so a busy minute produced a burst far past the group's
/// allowance, a 429, a queue-wide pause, and a backlog that never drained.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_paces_itself_under_the_group_limit_instead_of_being_refused() {
    const LIMIT: u32 = 30; // per minute, i.e. one every two seconds

    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTelegram::start().await;
    fake.enforce_group_limit(LIMIT);
    std::env::set_var("CTM_TELEGRAM_API_BASE", fake.base());
    std::env::set_var("CTM_WATCHDOG", "0");

    let mut cfg = config(dir.path());
    cfg.rate_limit = LIMIT;
    let socket = cfg.socket_path.clone();
    let mut daemon = Daemon::new(cfg).expect("daemon");
    daemon.start().await.expect("start");

    let session = "pacing-session";
    announce(&socket, session).await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // 50 replies at once — more than the minute allows, all of them Normal priority
    // so none can be shed. The daemon's only correct move is to slow down.
    // Spaced like real hook events; a tight loop of 50 connections just trips the
    // socket server's connection cap and proves nothing about pacing.
    for i in 0..50u64 {
        send_event(&socket, reply_event(session, &format!("PACED_{i}"))).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // At 30 a minute, packed, 50 replies need only a few posts. Wait for all of them.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    let markers: Vec<String> = (0..50u64).map(|i| format!("PACED_{i}")).collect();
    while std::time::Instant::now() < deadline {
        if markers.iter().all(|m| fake.was_delivered(m)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let missing: Vec<&String> = markers.iter().filter(|m| !fake.was_delivered(m)).collect();
    eprintln!(
        "posts={} throttled_429={} missing={}",
        fake.delivered().len(),
        fake.throttled(),
        missing.len()
    );

    // Telegram's FAQ allows for this explicitly: "We may allow short bursts that go
    // over this limit." What must not happen is *sustained* sending above the limit,
    // which is what produced 615 refusals in a day.
    assert!(
        fake.throttled() <= 5,
        "the daemon sent faster than Telegram allows and was refused {} times — it must pace itself, not discover the limit by hitting it",
        fake.throttled()
    );
    assert!(missing.is_empty(), "replies never arrived: {missing:?}");

    daemon.stop().await;
    std::env::remove_var("CTM_TELEGRAM_API_BASE");
    std::env::remove_var("CTM_WATCHDOG");
}
