//! ADR-024: when a live session's topic disappears, its replies go to a new topic.
//!
//! The daemon log from the incident held 5268 "Topic not found" warnings: a topic
//! deleted in the app made every message queued for it fail three times and then be
//! thrown away. Now they are held, a replacement topic is made, and they are sent
//! there. In its own binary: the API override is a process-wide environment variable.

mod common;

use common::{announce, config, FakeTelegram};
use ctm::daemon::Daemon;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

async fn reply(socket: &std::path::Path, session: &str, marker: &str) {
    let line = format!(
        r#"{{"type":"agent_response","sessionId":"{session}","timestamp":"2026-09-23T19:00:00.000Z","content":"{marker}","metadata":{{"hostSessionId":"{session}","projectDir":"/tmp/topic-gone"}}}}"#
    );
    if let Ok(mut s) = UnixStream::connect(socket).await {
        let _ = s.write_all(format!("{line}\n").as_bytes()).await;
        let _ = s.shutdown().await;
    }
}

async fn wait_until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replies_to_a_deleted_topic_arrive_in_its_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTelegram::start().await;
    std::env::set_var("CTM_TELEGRAM_API_BASE", fake.base());
    std::env::set_var("CTM_WATCHDOG", "0");

    let cfg = config(dir.path());
    let socket = cfg.socket_path.clone();
    let mut daemon = Daemon::new(cfg).expect("daemon");
    daemon.start().await.expect("start");

    let session = "topic-gone-session";
    announce(&socket, session).await;
    reply(&socket, session, "BEFORE_DELETE").await;
    assert!(
        wait_until(30, || fake.was_delivered("BEFORE_DELETE")).await,
        "the session's first reply arrived"
    );
    let old = fake
        .topic_of("BEFORE_DELETE")
        .flatten()
        .expect("it went to a topic");

    // The user deletes the topic in the app; the agent keeps talking.
    fake.kill_topic(old);
    for i in 0..3 {
        reply(&socket, session, &format!("AFTER_DELETE_{i}")).await;
    }

    let all = wait_until(90, || {
        (0..3).all(|i| fake.was_delivered(&format!("AFTER_DELETE_{i}")))
    })
    .await;
    assert!(
        all,
        "replies after the topic was deleted never arrived (topics created: {})",
        fake.created_topics()
    );
    for i in 0..3 {
        let landed = fake.topic_of(&format!("AFTER_DELETE_{i}")).flatten();
        assert!(
            landed.is_some() && landed != Some(old),
            "reply {i} landed in {landed:?}, not in a replacement for {old}"
        );
    }
    assert!(fake.created_topics() >= 2, "a replacement topic was made");

    daemon.stop().await;
    std::env::remove_var("CTM_TELEGRAM_API_BASE");
    std::env::remove_var("CTM_WATCHDOG");
}
