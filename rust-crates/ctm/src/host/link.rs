//! ADR-016: the observer's connection to the ctm daemon.
//!
//! [`ObserverLink`] is the socket-side half shared by every host observer. It is the
//! long-lived analogue of `hook.rs::send_messages` / `send_and_wait`: one NDJSON
//! connection carrying host events up to the daemon and the daemon's replies back
//! down. The host-specific half (event translation, API calls) lives in the per-host
//! module and never touches the socket directly.
//!
//! Correlation model: a hook has one connection per approval and so matches the
//! `ApprovalResponse` on `session_id` alone. An observer has one connection for many
//! approvals, so it keeps a per-session FIFO of pending host request ids
//! ([`ApprovalFifo`]). Both native hosts serialise approvals within a session (the
//! agent blocks on each one), so FIFO order IS host order — the same guarantee the
//! hook path already relies on.

use crate::error::{AppError, Result};
use crate::types::{BridgeMessage, HostKind, MessageType, MAX_LINE_BYTES};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};

/// Bounded so a wedged host cannot let daemon replies pile up without bound; 256
/// matches the daemon's own broadcast buffer (`socket.rs`).
const INBOUND_CAPACITY: usize = 256;

/// One observer's connection to the daemon.
pub struct ObserverLink {
    kind: HostKind,
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    inbound: mpsc::Receiver<BridgeMessage>,
    reader_task: tokio::task::JoinHandle<()>,
}

impl ObserverLink {
    /// Connect to the daemon socket and start the reader task.
    ///
    /// Fails fast if the daemon is not listening — the caller's reconnect loop
    /// (`run_with_reconnect` in each host module) owns the backoff policy.
    pub async fn connect(kind: HostKind, socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path).await.map_err(|e| {
            AppError::Socket(format!(
                "observer({kind}) failed to connect to {}: {e}",
                socket_path.display()
            ))
        })?;
        let (read_half, write_half) = stream.into_split();
        let (tx, rx) = mpsc::channel(INBOUND_CAPACITY);

        // Reader: NDJSON lines from the daemon -> typed BridgeMessage -> channel. Only
        // the message types the daemon sends *down* are forwarded; anything else on the
        // line (e.g. a broadcast meant for hooks) is logged at debug and dropped.
        let reader_task = tokio::spawn(async move {
            let mut buf_reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                match crate::socket::read_bounded_line(&mut buf_reader, &mut line, MAX_LINE_BYTES)
                    .await
                {
                    Ok(0) => {
                        tracing::info!(host = %kind, "observer link: daemon closed connection");
                        break;
                    }
                    Ok(n) if n > MAX_LINE_BYTES => {
                        tracing::warn!(host = %kind, bytes = n, "observer link: oversized line dropped");
                        continue;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(host = %kind, error = %e, "observer link: read error");
                        break;
                    }
                }
                let Ok(msg) = serde_json::from_str::<BridgeMessage>(line.trim()) else {
                    tracing::debug!(host = %kind, "observer link: unparseable line ignored");
                    continue;
                };
                match msg.msg_type {
                    MessageType::HostInject
                    | MessageType::QuestionResponse
                    | MessageType::ApprovalResponse => {
                        if tx.send(msg).await.is_err() {
                            break; // receiver dropped: link is being torn down
                        }
                    }
                    other => {
                        tracing::debug!(host = %kind, msg_type = %other, "observer link: ignoring daemon broadcast");
                    }
                }
            }
        });

        Ok(Self {
            kind,
            writer: Arc::new(Mutex::new(write_half)),
            inbound: rx,
            reader_task,
        })
    }

    /// Send one `BridgeMessage` up to the daemon as an NDJSON line.
    pub async fn send(&self, msg: &BridgeMessage) -> Result<()> {
        let json = serde_json::to_string(msg)?;
        let mut w = self.writer.lock().await;
        w.write_all(format!("{json}\n").as_bytes())
            .await
            .map_err(|e| AppError::Socket(format!("observer({}) write failed: {e}", self.kind)))
    }

    /// Next daemon→observer message (`HostInject`, `QuestionResponse`,
    /// `ApprovalResponse`). `None` once the connection is gone.
    pub async fn recv(&mut self) -> Option<BridgeMessage> {
        self.inbound.recv().await
    }
}

/// Build a `BridgeMessage` stamped with `hostKind` (and `hostname` if absent), so the
/// daemon records the host at insert time (`daemon/host_dispatch::record_session_host`).
/// Free function so per-host translators stay pure and unit-testable without a link.
pub fn stamped(
    kind: HostKind,
    msg_type: MessageType,
    session_id: &str,
    content: impl Into<String>,
    mut metadata: serde_json::Map<String, serde_json::Value>,
) -> BridgeMessage {
    metadata.insert(
        "hostKind".into(),
        serde_json::Value::String(kind.as_str().into()),
    );
    metadata
        .entry("hostname")
        .or_insert_with(|| serde_json::Value::String(crate::injector::get_hostname()));
    BridgeMessage {
        msg_type,
        session_id: session_id.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        content: content.into(),
        metadata: Some(metadata),
    }
}

impl Drop for ObserverLink {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

/// Per-session FIFO of host-side approval request ids awaiting a daemon decision.
///
/// `push` when the host asks; `pop_for` when an `ApprovalResponse` for that session
/// arrives. The daemon-assigned `approvalId` is logged for audit but NOT used for
/// matching, because the observer does not know it at request time.
#[derive(Debug)]
pub struct ApprovalFifo<T> {
    by_session: HashMap<String, VecDeque<T>>,
}

// Manual impl: `derive(Default)` would wrongly require `T: Default`.
impl<T> Default for ApprovalFifo<T> {
    fn default() -> Self {
        Self {
            by_session: HashMap::new(),
        }
    }
}

impl<T> ApprovalFifo<T> {
    pub fn push(&mut self, session_id: &str, host_request: T) {
        self.by_session
            .entry(session_id.to_string())
            .or_default()
            .push_back(host_request);
    }

    /// Oldest pending request for the session, or `None` if a decision arrived for a
    /// request the observer never registered (e.g. the operator answered locally and
    /// the host already resolved it — harmless, log and drop).
    pub fn pop_for(&mut self, session_id: &str) -> Option<T> {
        let q = self.by_session.get_mut(session_id)?;
        let item = q.pop_front();
        if q.is_empty() {
            self.by_session.remove(session_id);
        }
        item
    }

    /// Drop everything for a session (session end / host resolved elsewhere).
    pub fn clear(&mut self, session_id: &str) {
        self.by_session.remove(session_id);
    }

    #[allow(dead_code)] // Library API — exercised by unit tests
    pub fn pending(&self, session_id: &str) -> usize {
        self.by_session.get(session_id).map_or(0, VecDeque::len)
    }
}

/// Reconnect policy shared by all observers: exponential backoff with a ceiling,
/// mirroring the daemon's Telegram poll backoff (`event_loop.rs`, 5s→80s).
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    current: std::time::Duration,
    max: std::time::Duration,
}

impl Backoff {
    pub const fn new() -> Self {
        Self {
            current: std::time::Duration::from_secs(2),
            max: std::time::Duration::from_secs(60),
        }
    }
    /// Current delay, then double it (capped).
    pub fn delay(&mut self) -> std::time::Duration {
        let d = self.current;
        self.current = (self.current * 2).min(self.max);
        d
    }
    pub fn reset(&mut self) {
        self.current = std::time::Duration::from_secs(2);
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_fifo_is_per_session_and_ordered() {
        let mut f: ApprovalFifo<u32> = ApprovalFifo::default();
        f.push("a", 1);
        f.push("a", 2);
        f.push("b", 9);
        assert_eq!(f.pending("a"), 2);
        assert_eq!(f.pop_for("a"), Some(1));
        assert_eq!(f.pop_for("a"), Some(2));
        assert_eq!(f.pop_for("a"), None, "empty session queue is removed");
        assert_eq!(f.pop_for("b"), Some(9));
        assert_eq!(f.pop_for("zzz"), None, "unknown session is not an error");
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut b = Backoff::new();
        assert_eq!(b.delay().as_secs(), 2);
        assert_eq!(b.delay().as_secs(), 4);
        assert_eq!(b.delay().as_secs(), 8);
        for _ in 0..10 {
            b.delay();
        }
        assert_eq!(b.delay().as_secs(), 60, "capped");
        b.reset();
        assert_eq!(b.delay().as_secs(), 2);
    }

    #[tokio::test]
    async fn link_stamps_host_kind_and_round_trips_over_a_real_socket() {
        // A throwaway listener standing in for the daemon: accept one client, read
        // one line, echo an ApprovalResponse back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let p2 = path.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut br = BufReader::new(r);
            let mut line = String::new();
            crate::socket::read_bounded_line(&mut br, &mut line, MAX_LINE_BYTES)
                .await
                .unwrap();
            let up: BridgeMessage = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(up.msg_type, MessageType::ApprovalRequest);
            assert_eq!(up.meta().host_kind(), HostKind::OpenCode);
            let down = BridgeMessage {
                msg_type: MessageType::ApprovalResponse,
                session_id: up.session_id.clone(),
                timestamp: up.timestamp.clone(),
                content: "approve".into(),
                metadata: None,
            };
            w.write_all(format!("{}\n", serde_json::to_string(&down).unwrap()).as_bytes())
                .await
                .unwrap();
            // keep the socket open until the client is done reading
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(p2);
        });

        let mut link = ObserverLink::connect(HostKind::OpenCode, &path)
            .await
            .unwrap();
        let msg = stamped(
            HostKind::OpenCode,
            MessageType::ApprovalRequest,
            "sess-1",
            "Run `ls`?",
            serde_json::Map::new(),
        );
        assert_eq!(
            msg.metadata.as_ref().unwrap()["hostKind"],
            serde_json::Value::String("opencode".into())
        );
        link.send(&msg).await.unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(2), link.recv())
            .await
            .expect("reply within 2s")
            .expect("connection alive");
        assert_eq!(reply.msg_type, MessageType::ApprovalResponse);
        assert_eq!(reply.content, "approve");
        server.await.unwrap();
    }
}
