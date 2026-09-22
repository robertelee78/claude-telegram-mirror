//! ADR-023: the daemon must keep finishing handlers while Telegram refuses every send.
//!
//! Reproduces the 2026-09-22 incident exactly: a daemon that has just restarted,
//! announcing sessions (each a `createForumTopic`) while hook events pour in, with
//! Telegram answering 429 `retry_after: 40` to everything. Against the code as it
//! shipped in 0.2.53 this run ends with completed=0 and 180 handlers in flight, every
//! semaphore permit held inside a `retry_after` sleep, nothing logged — the mirror
//! silent in both directions. Against the fix it ends with every event handled.

mod common;

use common::{announce, config, feed, FakeTelegram, SendBehaviour};
use ctm::daemon::Daemon;
use std::time::Duration;

/// Telegram rate-limits every send for 40 s at a time (exactly the incident's
/// `retry_after`) while hook events keep arriving. The daemon may fall behind —
/// that is Telegram's prerogative — but it must keep *finishing handlers*. A daemon
/// that stops finishing them has stopped working in both directions, which is what
/// the user saw.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_keeps_finishing_handlers_while_telegram_rate_limits_everything() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTelegram::start().await;
    std::env::set_var("CTM_TELEGRAM_API_BASE", fake.base());
    // The watchdog must not end the test binary; the test asserts on progress and on
    // the watchdog's in-place recovery instead.
    std::env::set_var("CTM_WATCHDOG", "0");

    let cfg = config(dir.path());
    let socket = cfg.socket_path.clone();
    let mut daemon = Daemon::new(cfg).expect("daemon");
    daemon.start().await.expect("start");
    let health = daemon.health();

    // The incident's shape: a daemon that has just restarted is announcing sessions
    // (each one a `createForumTopic`) while hook events pour in for all of them.
    let sessions: Vec<String> = (0..60).map(|i| format!("stall-session-{i:03}")).collect();

    // Telegram starts refusing everything, as it did at 11:42.
    fake.set_send(SendBehaviour::RateLimited { retry_after: 40 });

    let feeder = {
        let socket = socket.clone();
        let sessions = sessions.clone();
        tokio::spawn(async move {
            for s in &sessions {
                announce(&socket, s).await;
                tokio::time::sleep(Duration::from_millis(120)).await;
            }
            feed(&socket, &sessions, 120, Duration::from_millis(120)).await
        })
    };

    // Sample progress while the flood runs.
    let mut samples = Vec::new();
    for _ in 0..12 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let s = health.snapshot();
        samples.push((
            s.received,
            s.dispatched,
            s.completed,
            s.in_flight,
            s.permits,
        ));
    }
    let sent = feeder.await.unwrap();
    let last = health.snapshot();

    eprintln!("sent={sent} samples(recv,disp,done,inflight,permits)={samples:#?}");
    eprintln!(
        "final: received={} dispatched={} completed={} in_flight={} free_permits={}\nin flight:{}",
        last.received,
        last.dispatched,
        last.completed,
        last.in_flight,
        last.permits,
        health.in_flight_report(20)
    );

    assert!(sent > 100, "the feeder got its events in ({sent})");
    assert_eq!(
        last.received, last.dispatched,
        "every event the socket accepted must reach a handler"
    );
    // The contract, and the whole point of the file: Telegram refusing every send
    // must cost the daemon *nothing* in handler capacity. Before the ADR-023 fix
    // this run finished with completed=0 and 180 handlers in flight, every permit
    // held inside a `retry_after` sleep — the incident, reproduced.
    assert_eq!(
        last.completed,
        last.received,
        "handlers did not finish under rate limiting ({} of {} done) — in flight:{}",
        last.completed,
        last.received,
        health.in_flight_report(20)
    );
    let busiest = samples.iter().map(|s| s.3).max().unwrap_or(0);
    assert!(
        busiest < 25,
        "handlers piled up waiting on Telegram (peak {busiest} in flight of a 50 permit pool); samples: {samples:?}"
    );
    let least_free = samples.iter().map(|s| s.4).min().unwrap_or(0);
    assert!(
        least_free > 25,
        "the handler pool was starved (as few as {least_free} free permits of 50)"
    );
    assert!(
        fake.calls("sendMessage") > 0,
        "the daemon actually tried to send"
    );
    // And the messages were not silently dropped on the floor: they went to the
    // queue, which is bounded and drains when Telegram recovers.
    assert!(
        fake.calls("createForumTopic") > 0,
        "topic creation was attempted"
    );

    daemon.stop().await;
    std::env::remove_var("CTM_TELEGRAM_API_BASE");
    std::env::remove_var("CTM_WATCHDOG");
}
