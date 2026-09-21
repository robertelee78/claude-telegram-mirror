//! A one-shot JSON-RPC client for the Codex app-server control socket.
//!
//! `codex.rs` owns the long-lived observer connection and its pure translator; this is
//! the small request/response client the *management* paths need (`hooks/list`,
//! `config/batchWrite` — see `codex_hooks.rs`). It exists separately so a `ctm doctor
//! --fix` or a daemon start-up pass can ask Codex a question without standing up an
//! observer, and so those calls are unit-testable against a stand-in socket.
//!
//! Transport is the same as the observer's: WebSocket over the Unix control socket,
//! `initialize` first. Management calls initialize without `experimentalApi`; only the
//! ADR-022 launcher asks for it (`connect_experimental`), because
//! `thread/settings/update` is behind that capability and nothing else ctm calls is.

use crate::error::{AppError, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const CLIENT_NAME: &str = "ctm";
/// Every call is bounded: a wedged app-server must never hold up the daemon's start-up
/// or a `doctor` run.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Rpc {
    tx: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
        WsMessage,
    >,
    rx: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    >,
    next_id: u64,
    timeout: Duration,
}

impl Rpc {
    /// Connect and complete `initialize` (stable API surface only).
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        Self::connect_with(socket_path, false).await
    }

    /// Connect with the `experimentalApi` capability: `thread/settings/update` needs
    /// it (ADR-022). Nothing else in ctm should use this connection.
    pub async fn connect_experimental(socket_path: &Path) -> Result<Self> {
        Self::connect_with(socket_path, true).await
    }

    /// Per-call reply budget for this connection (default 10 s). Loading a large
    /// rollout through `thread/resume` can take longer than a management call.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn connect_with(socket_path: &Path, experimental: bool) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(socket_path)
            .await
            .map_err(|e| {
                AppError::Socket(format!(
                    "Codex app-server socket {} not reachable ({e}) — is `codex app-server daemon` running?",
                    socket_path.display()
                ))
            })?;
        let (ws, _resp) = tokio_tungstenite::client_async("ws://codex/", stream)
            .await
            .map_err(|e| AppError::Socket(format!("Codex WebSocket handshake failed: {e}")))?;
        let (tx, rx) = ws.split();
        let mut rpc = Self {
            tx,
            rx,
            next_id: 0,
            timeout: CALL_TIMEOUT,
        };
        let mut params = json!({
            "clientInfo": {
                "name": CLIENT_NAME,
                "title": "Claude Telegram Mirror",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });
        if experimental {
            params["capabilities"] = json!({"experimentalApi": true});
        }
        rpc.call("initialize", params).await?;
        Ok(rpc)
    }

    /// One request; returns its `result` or the server's error.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.tx
            .send(WsMessage::Text(req.to_string()))
            .await
            .map_err(|e| AppError::Socket(format!("Codex {method} send failed: {e}")))?;

        let budget = self.timeout;
        tokio::time::timeout(budget, async {
            while let Some(frame) = self.rx.next().await {
                let frame =
                    frame.map_err(|e| AppError::Socket(format!("Codex {method} read: {e}")))?;
                let WsMessage::Text(text) = frame else {
                    continue; // ping/pong/binary: not ours
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                // Notifications and server requests interleave with our reply.
                if v.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(err) = v.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error");
                    return Err(AppError::Socket(format!("Codex {method}: {msg}")));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            Err(AppError::Socket(format!(
                "Codex {method}: connection closed before a reply"
            )))
        })
        .await
        .map_err(|_| {
            AppError::Socket(format!(
                "Codex {method}: no reply within {}s",
                budget.as_secs()
            ))
        })?
    }
}
