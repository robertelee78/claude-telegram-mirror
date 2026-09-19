//! ADR-016 §Default enablement: the OpenCode **pipe** transport.
//!
//! A bare `opencode` has no TCP listener (spike-verified: `--port` is the only way to
//! get one, and `server.port` in `opencode.json` applies to `serve`/`web` alone). What
//! it does have is a plugin loader and an in-process API client. So ctm provisions a
//! plugin (`opencode_plugin.rs`) that pipes the event bus up a local Unix socket and
//! executes raw API calls the daemon sends down — and this module is the daemon end.
//!
//! One connection = one OpenCode process (or one plugin instance: `--port` loads the
//! plugin twice in the same pid, but only one instance receives events, so the other
//! pipe simply stays idle). Each connection drives its own [`Translator`] and owns its
//! own [`ObserverLink`] to the bridge socket, so the daemon's routing
//! (`host_dispatch::send_to_host_observer`, keyed by the announcing client) needs no
//! change and several OpenCode processes coexist.
//!
//! Spike-established caveat (1.18.31): the **first API request** an OpenCode instance
//! serves after start is invisible to plugin `event` hooks, whatever it is and however
//! long after start it comes; everything after it is delivered. A TUI makes many
//! requests before the user's first prompt, and any session that is actually used is
//! announced lazily on its next event, so this never loses a topic in practice — the
//! e2e test warms the instance with a list request first, as a TUI would.
//!
//! Wire protocol, NDJSON both ways (see the template for the plugin side):
//!   up:   hello `{"ctm":"opencode","v":1,…}` first, then `{"type":"event",…}` and
//!         `{"type":"result","id","status","data","error"}`
//!   down: `{"type":"call","id","method","url","query","body"}`

use crate::config::Config;
use crate::error::{AppError, Result};
use crate::host::link::ObserverLink;
use crate::host::opencode::{classify, wire, HostCall, Translator};
use crate::host::opencode_plugin::pipe_socket_path;
use crate::socket::read_bounded_line;
use crate::types::{HostKind, MAX_LINE_BYTES};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

const KIND: HostKind = HostKind::OpenCode;
/// Bounded so a wedged daemon link cannot let a chatty OpenCode pile events up
/// without bound; matches `ObserverLink`'s inbound capacity.
const UP_CAPACITY: usize = 256;
/// Wire version the plugin must speak.
const PIPE_VERSION: u64 = 1;

#[derive(Debug, Deserialize)]
pub struct Hello {
    pub ctm: String,
    pub v: u64,
    #[serde(default)]
    pub pid: u64,
    #[serde(default, rename = "serverUrl")]
    pub server_url: String,
    #[serde(default)]
    pub directory: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Up {
    Event {
        event: Value,
    },
    Result {
        id: u64,
        #[serde(default)]
        status: u16,
        #[serde(default)]
        data: Value,
        #[serde(default)]
        error: Value,
    },
}

/// Parse and validate the first line a plugin sends.
pub fn parse_hello(line: &str) -> Result<Hello> {
    let h: Hello = serde_json::from_str(line.trim())
        .map_err(|e| AppError::Socket(format!("OpenCode pipe: bad hello: {e}")))?;
    if h.ctm != "opencode" {
        return Err(AppError::Socket(format!(
            "OpenCode pipe: unexpected client kind {:?}",
            h.ctm
        )));
    }
    if h.v != PIPE_VERSION {
        return Err(AppError::Socket(format!(
            "OpenCode pipe: plugin speaks v{} but this ctm expects v{PIPE_VERSION} (ctm doctor --fix rewrites the plugin)",
            h.v
        )));
    }
    Ok(h)
}

/// The `call` line for one [`HostCall`].
pub fn call_line(id: u64, c: &HostCall) -> String {
    let (method, url, body, dir) = wire(c);
    let query = dir
        .map(|d| json!({ "directory": d }))
        .unwrap_or(Value::Null);
    let v = json!({
        "type": "call", "id": id, "method": method.as_str(), "url": url,
        "query": query, "body": body,
    });
    format!("{v}\n")
}

/// Bind the pipe socket and serve plugin connections forever.
pub async fn serve(config: Arc<Config>) {
    let path = pipe_socket_path(&config.socket_path);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e, "OpenCode pipe: cannot bind — bare opencode sessions will not be mirrored");
            return;
        }
    };
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!(path = %path.display(), "OpenCode pipe listening");
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let cfg = Arc::clone(&config);
                tokio::spawn(async move {
                    if let Err(e) = run_pipe(cfg, stream).await {
                        tracing::warn!(error = %e, "OpenCode pipe: connection ended");
                    }
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "OpenCode pipe: accept failed");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

/// One plugin connection lifetime. Public so the e2e test can drive it against a real
/// OpenCode process with a stand-in bridge socket.
pub async fn run_pipe(config: Arc<Config>, stream: UnixStream) -> Result<()> {
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let mut line = String::new();

    let n = read_bounded_line(&mut reader, &mut line, MAX_LINE_BYTES)
        .await
        .map_err(|e| AppError::Socket(format!("OpenCode pipe: hello read failed: {e}")))?;
    if n == 0 {
        return Ok(()); // connected and left; nothing to report
    }
    let hello = parse_hello(&line)?;
    tracing::info!(pid = hello.pid, server_url = %hello.server_url, directory = %hello.directory, "OpenCode pipe: process connected");

    let mut link = ObserverLink::connect(KIND, &config.socket_path).await?;
    let mut tr = Translator::new();
    tr.set_default_directory(hello.directory.clone());
    let mut next_id: u64 = 0;
    let mut inflight: HashMap<u64, HostCall> = HashMap::new();

    // Reader task → channel, so the select below only awaits cancel-safe `recv`s.
    // (`read_bounded_line` consumes bytes as they arrive; cancelling it mid-line would
    // silently drop that event — possibly a `permission.asked`.)
    let (up_tx, mut up_rx) = mpsc::channel::<Up>(UP_CAPACITY);
    let reader_task = tokio::spawn(async move {
        loop {
            match read_bounded_line(&mut reader, &mut line, MAX_LINE_BYTES).await {
                Ok(0) => break,
                Ok(n) if n > MAX_LINE_BYTES => {
                    tracing::warn!(bytes = n, "OpenCode pipe: oversized line dropped");
                    continue;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "OpenCode pipe: read error");
                    break;
                }
            }
            match serde_json::from_str::<Up>(line.trim()) {
                Ok(up) => {
                    if up_tx.send(up).await.is_err() {
                        break;
                    }
                }
                Err(_) => tracing::debug!("OpenCode pipe: unparseable line ignored"),
            }
        }
    });

    let result: Result<()> = loop {
        tokio::select! {
            up = up_rx.recv() => {
                let Some(up) = up else { break Ok(()) }; // reader hit EOF/error
                match up {
                    Up::Event { event } => {
                        let mut failed = None;
                        for m in tr.on_event(&event) {
                            if let Err(e) = link.send(&m).await {
                                failed = Some(e);
                                break;
                            }
                        }
                        if let Some(e) = failed {
                            break Err(e);
                        }
                    }
                    Up::Result { id, status, data, error } => {
                        let Some(call) = inflight.remove(&id) else { continue };
                        let body = if error.is_null() { data.to_string() } else { error.to_string() };
                        if let Err(e) = classify(&call, status, &body) {
                            tracing::warn!(error = %e, call = ?call, "OpenCode: host call failed");
                        }
                    }
                }
            }
            down = link.recv() => {
                let Some(msg) = down else {
                    break Err(AppError::Socket("daemon link closed".into()));
                };
                let mut write_failed = false;
                for c in tr.on_daemon(&msg) {
                    next_id += 1;
                    let out = call_line(next_id, &c);
                    inflight.insert(next_id, c);
                    if let Err(e) = wr.write_all(out.as_bytes()).await {
                        tracing::warn!(error = %e, "OpenCode pipe: write failed");
                        write_failed = true;
                        break;
                    }
                }
                if write_failed {
                    break Ok(()); // the process is going away; reader will EOF too
                }
            }
        }
    };
    reader_task.abort();
    // On a daemon-side error there is nothing to clean up: the daemon is gone, and the
    // plugin reconnects to its successor and re-announces lazily.
    result?;

    // The OpenCode process is gone: close its topics rather than letting them age out.
    for m in tr.end_all("opencode exited") {
        let _ = link.send(&m).await;
    }
    tracing::info!(pid = hello.pid, "OpenCode pipe: process disconnected");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_is_validated() {
        let h = parse_hello(r#"{"ctm":"opencode","v":1,"pid":42,"serverUrl":"http://localhost:4096/","directory":"/p"}"#).unwrap();
        assert_eq!((h.pid, h.directory.as_str()), (42, "/p"));
        assert!(parse_hello(r#"{"ctm":"codex","v":1}"#).is_err());
        assert!(parse_hello(r#"{"ctm":"opencode","v":2}"#).is_err());
        assert!(parse_hello("not json").is_err());
    }

    #[test]
    fn call_line_carries_method_url_query_and_body() {
        let c = HostCall::PermissionReply {
            request_id: "per_1".into(),
            directory: "/proj".into(),
            reply: "once".into(),
        };
        let v: Value = serde_json::from_str(call_line(7, &c).trim()).unwrap();
        assert_eq!(v["type"], "call");
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "POST");
        assert_eq!(v["url"], "/permission/per_1/reply");
        assert_eq!(v["query"]["directory"], "/proj");
        assert_eq!(v["body"]["reply"], "once");
        // Toast has no directory → null query, so the plugin passes `undefined`.
        let t = HostCall::Toast {
            message: "m".into(),
            variant: "success".into(),
        };
        let v: Value = serde_json::from_str(call_line(8, &t).trim()).unwrap();
        assert!(v["query"].is_null());
    }

    #[test]
    fn up_lines_parse() {
        let e: Up = serde_json::from_str(
            r#"{"type":"event","event":{"type":"session.idle","properties":{"sessionID":"s"}}}"#,
        )
        .unwrap();
        assert!(matches!(e, Up::Event { .. }));
        let r: Up = serde_json::from_str(
            r#"{"type":"result","id":3,"status":404,"data":null,"error":{"name":"NotFoundError"}}"#,
        )
        .unwrap();
        match r {
            Up::Result {
                id, status, error, ..
            } => {
                assert_eq!((id, status), (3, 404));
                assert_eq!(error["name"], "NotFoundError");
            }
            _ => panic!(),
        }
    }
}
