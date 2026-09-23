//! ADR-024: the agent's reply must reach Telegram, whatever else is going on.
//!
//! Reported: *"I saw some messages from it, I responded, it responded, but message
//! never came to telegram."* The daemon was sending into a group at up to 20 messages
//! per **second** while Telegram allows 20 per **minute**, so the bot lived in a
//! permanent 429 (615 global pauses in one day), the queue sat pinned at its 300
//! message cap, and the eviction that kept it there threw away agent replies along
//! with tool previews.
//!
//! This test runs the real daemon against a Telegram that enforces the documented
//! group limit, floods it with tool traffic, and asserts the replies still arrive.

mod common;

use common::{announce, config, FakeTelegram};
use ctm::daemon::Daemon;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

/// Telegram's documented allowance for a group, from the Bot API FAQ.
const GROUP_LIMIT_PER_MIN: u32 = 20;

async fn send_event(socket: &std::path::Path, line: String) {
    if let Ok(mut s) = UnixStream::connect(socket).await {
        let _ = s.write_all(format!("{line}\n").as_bytes()).await;
        let _ = s.shutdown().await;
    }
}

fn tool_event(session: &str, n: u64) -> String {
    let kind = if n.is_multiple_of(2) {
        "tool_start"
    } else {
        "tool_result"
    };
    format!(
        r#"{{"type":"{kind}","sessionId":"{session}","timestamp":"2026-09-23T19:00:00.000Z","content":"tool output {n}","metadata":{{"hostSessionId":"{session}","projectDir":"/tmp/budget-test","tool":"Bash","input":{{"command":"echo {n}"}}}}}}"#
    )
}

fn reply_event(session: &str, marker: &str) -> String {
    format!(
        r#"{{"type":"agent_response","sessionId":"{session}","timestamp":"2026-09-23T19:00:00.000Z","content":"{marker}","metadata":{{"hostSessionId":"{session}","projectDir":"/tmp/budget-test"}}}}"#
    )
}

/// The user's scenario: a busy agent producing a flood of tool activity, and five
/// replies addressed to the person reading Telegram. Every reply must arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_replies_reach_telegram_while_tool_traffic_floods_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTelegram::start().await;
    fake.enforce_group_limit(GROUP_LIMIT_PER_MIN);
    std::env::set_var("CTM_TELEGRAM_API_BASE", fake.base());
    std::env::set_var("CTM_WATCHDOG", "0");

    let mut cfg = config(dir.path());
    // What a real install now carries: the group's whole budget, per minute.
    cfg.rate_limit = GROUP_LIMIT_PER_MIN;
    // Tool chatter on, as the user runs it: it is the traffic that overflowed.
    cfg.verbose = true;
    let socket = cfg.socket_path.clone();
    let mut daemon = Daemon::new(cfg).expect("daemon");
    daemon.start().await.expect("start");

    let session = "budget-session-1";
    announce(&socket, session).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 200 tool events — far more than a minute's budget — with five replies mixed in.
    let markers: Vec<String> = (0..5).map(|i| format!("REPLY_MARKER_{i}")).collect();
    for n in 0..200u64 {
        send_event(&socket, tool_event(session, n)).await;
        if n % 40 == 39 {
            let m = &markers[(n / 40) as usize];
            send_event(&socket, reply_event(session, m)).await;
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
    }

    // At one post every three seconds, packed; allow generously.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        let all_tools = (0..200u64)
            .step_by(2)
            .all(|n| fake.was_delivered(&format!("`echo {n}`")));
        if markers.iter().all(|m| fake.was_delivered(m)) && all_tools {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let delivered = fake.delivered();
    let missing: Vec<&String> = markers.iter().filter(|m| !fake.was_delivered(m)).collect();
    eprintln!(
        "delivered={} throttled_429={} previews_delivered={}",
        delivered.len(),
        fake.throttled(),
        delivered
            .iter()
            .filter(|t| t.contains("tool output"))
            .count()
    );

    assert!(
        missing.is_empty(),
        "agent replies never reached Telegram: {missing:?} (delivered {} messages, {} refused as over-limit)",
        delivered.len(),
        fake.throttled()
    );
    // Nothing dropped: every tool call made it too, packed several to a post.
    let everything = delivered.join("\n");
    let lost: Vec<u64> = (0..200u64)
        .step_by(2)
        .filter(|n| !everything.contains(&format!("`echo {n}`")))
        .collect();
    assert!(
        lost.is_empty(),
        "tool calls were dropped instead of delivered: {lost:?}"
    );
    assert!(
        delivered.len() < 40,
        "a backlog must be packed, not posted one by one ({} posts for ~300 items)",
        delivered.len()
    );
    for t in &delivered {
        assert!(
            t.encode_utf16().count() <= 4096,
            "a post exceeded Telegram's size limit ({} units)",
            t.encode_utf16().count()
        );
    }
    // And ctm stayed under the limit rather than discovering it by being refused.
    assert!(
        fake.throttled() <= 5,
        "the daemon was refused {} times — it is still sending faster than Telegram allows",
        fake.throttled()
    );

    daemon.stop().await;
    std::env::remove_var("CTM_TELEGRAM_API_BASE");
    std::env::remove_var("CTM_WATCHDOG");
}
