//! ADR-023: deleted topics and slow replies must not stop the daemon either.
//!
//! The daemon log from the incident carried 5268 "Topic not found" warnings; a topic
//! deleted in the app makes every send to it fail. Combined with slow replies that is
//! the other way handlers can pile up.

mod common;

use common::{announce, config, feed, FakeTelegram, SendBehaviour};
use ctm::daemon::Daemon;
use std::time::Duration;

/// The other shape the log was full of: 5268 "Topic not found" warnings in one file.
/// A topic deleted in the app makes every send to it fail, and a slow Telegram makes
/// each failure expensive. Handlers must still finish — the daemon's job is to notice
/// and move on, not to wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleted_topics_and_slow_replies_do_not_stop_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTelegram::start().await;
    std::env::set_var("CTM_TELEGRAM_API_BASE", fake.base());
    std::env::set_var("CTM_WATCHDOG", "0");

    let cfg = config(dir.path());
    let socket = cfg.socket_path.clone();
    let mut daemon = Daemon::new(cfg).expect("daemon");
    daemon.start().await.expect("start");
    let health = daemon.health();

    let sessions: Vec<String> = (0..10).map(|i| format!("gone-topic-{i:02}")).collect();
    for s in &sessions {
        announce(&socket, s).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Every topic has been deleted, and Telegram is answering slowly.
    fake.set_send(SendBehaviour::TopicNotFound);
    let feeder = {
        let socket = socket.clone();
        let sessions = sessions.clone();
        tokio::spawn(async move { feed(&socket, &sessions, 60, Duration::from_millis(100)).await })
    };
    tokio::time::sleep(Duration::from_secs(5)).await;
    fake.set_send(SendBehaviour::Slow { ms: 1500 });

    let sent = feeder.await.unwrap();
    // Give the in-flight tail time to drain.
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    while std::time::Instant::now() < deadline {
        let s = health.snapshot();
        if s.completed == s.received {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let last = health.snapshot();
    eprintln!(
        "sent={sent} received={} completed={} in_flight={} topics_created={}",
        last.received,
        last.completed,
        last.in_flight,
        fake.calls("createForumTopic")
    );
    assert_eq!(
        last.completed,
        last.received,
        "handlers did not finish against deleted topics / slow replies — in flight:{}",
        health.in_flight_report(20)
    );
    daemon.stop().await;
    std::env::remove_var("CTM_TELEGRAM_API_BASE");
    std::env::remove_var("CTM_WATCHDOG");
}
