//! ADR-023: a stand-in Telegram, so the daemon can be run against the behaviour that
//! actually breaks it — 429s with a `retry_after`, deleted topics, slow replies.
//!
//! The 2026-09-22 stall could not have been caught by any existing test: the input
//! that provokes it comes from Telegram, and the API origin was a hard-coded string.
//! `CTM_TELEGRAM_API_BASE` makes it reachable, and this module is the stand-in.
//!
//! Each test that uses this gets its own binary (and so its own process), because the
//! override is an environment variable and cargo runs tests inside one binary in
//! parallel threads.
#![allow(dead_code)]

use ctm::config::{CodexHostConfig, Config, HostsConfig, OpenCodeHostConfig};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixStream};

// --------------------------------------------------------------- fake Telegram

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendBehaviour {
    Ok,
    /// What Telegram does to a bot that is over its limit: refuse everything for
    /// `retry_after` seconds. Observed in the incident with retry_after 19–43.
    RateLimited {
        retry_after: u64,
    },
    /// The topic was deleted in the app; every send to it fails until ctm notices.
    TopicNotFound,
    /// Accepted, but the reply takes this long — the shape that makes handlers pile up.
    Slow {
        ms: u64,
    },
}

struct FakeState {
    send: SendBehaviour,
    /// How long `getUpdates` holds before answering (Telegram long-polls for 30 s).
    poll_hold_ms: u64,
    calls: HashMap<String, u64>,
}

#[derive(Clone)]
pub struct FakeTelegram {
    state: Arc<Mutex<FakeState>>,
    port: u16,
    next_id: Arc<AtomicI64>,
}

impl FakeTelegram {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let me = Self {
            state: Arc::new(Mutex::new(FakeState {
                send: SendBehaviour::Ok,
                poll_hold_ms: 200,
                calls: HashMap::new(),
            })),
            port,
            next_id: Arc::new(AtomicI64::new(1000)),
        };
        let srv = me.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let srv = srv.clone();
                tokio::spawn(async move { srv.serve(sock).await });
            }
        });
        me
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn set_send(&self, b: SendBehaviour) {
        self.state.lock().unwrap().send = b;
    }

    pub fn calls(&self, method: &str) -> u64 {
        *self.state.lock().unwrap().calls.get(method).unwrap_or(&0)
    }

    async fn serve(self, sock: tokio::net::TcpStream) {
        let (r, mut w) = sock.into_split();
        let mut reader = BufReader::new(r);
        loop {
            // Request line + headers.
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).await.unwrap_or(0) == 0 {
                    return;
                }
                if h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            if len > 0 {
                let mut body = vec![0u8; len];
                if reader.read_exact(&mut body).await.is_err() {
                    return;
                }
            }
            let method = path.rsplit('/').next().unwrap_or("").to_string();
            let (behaviour, hold) = {
                let mut st = self.state.lock().unwrap();
                *st.calls.entry(method.clone()).or_insert(0) += 1;
                (st.send, st.poll_hold_ms)
            };

            let body = match method.as_str() {
                "getMe" => r#"{"ok":true,"result":{"id":1,"is_bot":true,"username":"faketestbot"}}"#.to_string(),
                "getUpdates" => {
                    tokio::time::sleep(Duration::from_millis(hold)).await;
                    r#"{"ok":true,"result":[]}"#.to_string()
                }
                "createForumTopic" => {
                    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                    format!(r#"{{"ok":true,"result":{{"message_thread_id":{id},"name":"t"}}}}"#)
                }
                _ => match behaviour {
                    SendBehaviour::Ok => {
                        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                        format!(r#"{{"ok":true,"result":{{"message_id":{id},"date":1,"chat":{{"id":-100,"type":"supergroup"}}}}}}"#)
                    }
                    SendBehaviour::Slow { ms } => {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                        format!(r#"{{"ok":true,"result":{{"message_id":{id},"date":1,"chat":{{"id":-100,"type":"supergroup"}}}}}}"#)
                    }
                    SendBehaviour::RateLimited { retry_after } => format!(
                        r#"{{"ok":false,"error_code":429,"description":"Too Many Requests: retry after {retry_after}","parameters":{{"retry_after":{retry_after}}}}}"#
                    ),
                    SendBehaviour::TopicNotFound => {
                        r#"{"ok":false,"error_code":400,"description":"Bad Request: message thread not found"}"#.to_string()
                    }
                },
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
                body.len(),
                body
            );
            if w.write_all(resp.as_bytes()).await.is_err() {
                return;
            }
        }
    }
}

// ------------------------------------------------------------------- harness

pub fn config(dir: &std::path::Path) -> Config {
    Config {
        bot_token: "123456:fake-token-for-tests".into(),
        chat_id: -1001234567890,
        enabled: true,
        verbose: false,
        approvals: true,
        // The incident ran with topics on; topic creation is part of the path.
        use_threads: true,
        chunk_size: 4000,
        rate_limit: 20,
        session_timeout: 3600,
        stale_session_timeout_hours: 24,
        auto_delete_topics: false,
        topic_delete_delay_minutes: 5,
        inactivity_delete_threshold_minutes: 720,
        socket_path: dir.join("bridge.sock"),
        config_dir: dir.to_path_buf(),
        config_path: dir.join("config.json"),
        forum_enabled: true,
        hosts: HostsConfig {
            opencode: OpenCodeHostConfig {
                enabled: false,
                ..Default::default()
            },
            codex: CodexHostConfig {
                enabled: false,
                ..Default::default()
            },
        },
    }
}

/// One hook event, as `ctm hook` would write it.
pub fn event(kind: &str, session: &str, n: u64) -> String {
    let meta = format!(
        r#"{{"hostSessionId":"{session}","projectDir":"/tmp/stall-test","tool":"Bash","input":{{"command":"echo {n}"}}}}"#
    );
    format!(
        r#"{{"type":"{kind}","sessionId":"{session}","timestamp":"2026-09-22T10:00:00.000Z","content":"event {n}","metadata":{meta}}}"#
    )
}

/// Feed events into the daemon's socket the way hook clients do: connect, write,
/// disconnect — one short-lived connection per event.
pub async fn feed(
    socket: &std::path::Path,
    sessions: &[String],
    count: u64,
    every: Duration,
) -> u64 {
    let mut sent = 0;
    for n in 0..count {
        let session = &sessions[(n as usize) % sessions.len()];
        let kind = match n % 4 {
            0 => "tool_start",
            1 => "tool_result",
            2 => "agent_response",
            _ => "user_input",
        };
        if let Ok(mut s) = UnixStream::connect(socket).await {
            let line = format!("{}\n", event(kind, session, n));
            if s.write_all(line.as_bytes()).await.is_ok() {
                let _ = s.shutdown().await;
                sent += 1;
            }
        }
        tokio::time::sleep(every).await;
    }
    sent
}

pub async fn announce(socket: &std::path::Path, session: &str) {
    let line = format!(
        "{}\n",
        format_args!(
            r#"{{"type":"session_start","sessionId":"{session}","timestamp":"2026-09-22T10:00:00.000Z","content":"","metadata":{{"hostSessionId":"{session}","projectDir":"/tmp/stall-test"}}}}"#
        )
    );
    if let Ok(mut s) = UnixStream::connect(socket).await {
        let _ = s.write_all(line.as_bytes()).await;
        let _ = s.shutdown().await;
    }
}
