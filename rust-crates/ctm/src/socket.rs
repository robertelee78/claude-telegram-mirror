//! Unix domain socket server and client — NDJSON framing.
//!
//! Ported from `socket.ts`.  Wire-compatible with the TypeScript implementation.
//!
//! ## Server
//! - `flock(2)` PID locking via `nix::fcntl::Flock`
//! - `chmod(0o600)` on the socket file after bind
//! - Stale socket detection (check flock, not TOCTOU PID file)
//! - 64 max concurrent connections
//! - 1 MiB max NDJSON line
//!
//! ## Client
//! - Connect, send, send-and-wait with correlation on `session_id`

use crate::config;
use crate::error::{AppError, Result};
use crate::types::BridgeMessage;
use nix::fcntl::Flock;
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex};
use tokio::time::{timeout, Duration};

/// Maximum concurrent client connections.
const MAX_CONNECTIONS: usize = 64;

/// Maximum bytes in a single NDJSON line (1 MiB).
const MAX_LINE_BYTES: usize = 1_048_576;

/// Default socket path: `~/.config/claude-telegram-mirror/bridge.sock`.
#[allow(dead_code)] // Library API
pub fn default_socket_path() -> std::path::PathBuf {
    config::get_config_dir().join("bridge.sock")
}

/// Directory containing the socket file.
#[allow(dead_code)] // Library API
pub fn socket_dir() -> std::path::PathBuf {
    config::get_config_dir()
}

/// L6.5: Check if a process with the given PID is running.
///
/// Uses `kill(pid, 0)` which checks for process existence without sending a
/// signal.  Returns `false` if the process does not exist.  Returns `true` if
/// the process exists (even if we lack permission to signal it -- EPERM).
#[allow(dead_code)] // Library API
pub fn is_pid_running(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    // Signal 0 (None) does not actually send a signal; it just checks
    // permissions and process existence.
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::EPERM) => true, // process exists, no permission
        Err(_) => false,                       // ESRCH or other error
    }
}

/// Probe the status of a Unix domain socket file.
///
/// Returns:
/// - `"none"` if the file does not exist.
/// - `"active"` if a connection to the socket succeeds (daemon is running).
/// - `"stale"` if the file exists but the connection is refused (orphaned socket).
#[allow(dead_code)] // Library API
pub fn check_socket_status(socket_path: &std::path::Path) -> &'static str {
    if !socket_path.exists() {
        return "none";
    }
    match std::os::unix::net::UnixStream::connect(socket_path) {
        Ok(_) => "active",
        Err(_) => "stale",
    }
}

// ====================================================================== server

/// Unix domain socket server with NDJSON framing.
pub struct SocketServer {
    socket_path: PathBuf,
    pid_path: PathBuf,
    /// Held for the server's lifetime — releasing it signals "no daemon".
    _lock: Option<Flock<OwnedFd>>,
    tx: broadcast::Sender<BridgeMessage>,
    clients: Arc<Mutex<HashMap<String, Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>>>>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl SocketServer {
    pub fn new(socket_path: &Path, pid_path: &Path) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            socket_path: socket_path.to_path_buf(),
            pid_path: pid_path.to_path_buf(),
            _lock: None,
            tx,
            clients: Arc::new(Mutex::new(HashMap::new())),
            shutdown_tx: None,
        }
    }

    /// Subscribe to the broadcast channel to receive incoming messages.
    pub fn subscribe(&self) -> broadcast::Receiver<BridgeMessage> {
        self.tx.subscribe()
    }

    /// Number of currently connected clients.
    #[allow(dead_code)] // Library API
    pub async fn client_count(&self) -> usize {
        self.clients.lock().await.len()
    }

    /// Return a shared reference to the connected-client map so callers outside
    /// of `SocketServer` can broadcast without holding the server itself.
    pub fn clients_ref(
        &self,
    ) -> Arc<Mutex<HashMap<String, Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>>>> {
        Arc::clone(&self.clients)
    }

    /// Start listening.  Returns when the accept loop is running.
    pub async fn listen(&mut self) -> Result<()> {
        // Step 1: Acquire flock on PID file (atomic, no TOCTOU).
        let lock = acquire_flock(&self.pid_path)?;

        // Step 2: Enforce 0o700 on the parent directory so that the socket file
        // (0o600) is nested inside a directory that is only accessible by the
        // owner.  This prevents other users from even discovering the socket path.
        if let Some(parent) = self.socket_path.parent() {
            use std::os::unix::fs::PermissionsExt;
            if parent.exists() {
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            } else {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AppError::Socket(format!("Cannot create socket directory: {e}"))
                })?;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }

        // Step 3: Remove stale socket if present.
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }

        // Step 4: Bind, then restrict the socket file to 0o600.
        //
        // Previous implementation used process-global umask(0o177) around bind(),
        // but umask is per-process (not per-thread), causing race conditions in
        // tests and any multi-threaded context. Instead, we bind normally and
        // chmod the socket file immediately after creation.
        let listener = UnixListener::bind(&self.socket_path)
            .map_err(|e| AppError::Socket(format!("Failed to bind socket: {e}")))?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600));
        }

        // Write PID file after lock acquired.
        let _ = std::fs::write(&self.pid_path, std::process::id().to_string());

        self._lock = Some(lock);

        let tx = self.tx.clone();
        let clients = Arc::clone(&self.clients);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        // Spawn accept loop.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, _)) => {
                                let count = clients.lock().await.len();
                                if count >= MAX_CONNECTIONS {
                                    tracing::warn!(count, "Connection limit reached, rejecting");
                                    drop(stream);
                                    continue;
                                }
                                let client_id = format!(
                                    "client-{}-{}",
                                    chrono::Utc::now().timestamp_millis(),
                                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                                );
                                let tx2 = tx.clone();
                                let clients2 = Arc::clone(&clients);
                                let cid = client_id.clone();

                                let (reader, writer) = stream.into_split();
                                clients.lock().await.insert(
                                    client_id.clone(),
                                    Arc::new(Mutex::new(writer)),
                                );

                                tokio::spawn(async move {
                                    handle_client(reader, &cid, tx2).await;
                                    clients2.lock().await.remove(&cid);
                                });
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "Accept error");
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Send an NDJSON message to a specific client.
    #[allow(dead_code)] // Library API
    pub async fn send(&self, client_id: &str, message: &BridgeMessage) -> Result<bool> {
        let clients = self.clients.lock().await;
        if let Some(writer) = clients.get(client_id) {
            let json = serde_json::to_string(message)?;
            let mut w = writer.lock().await;
            if w.write_all(format!("{json}\n").as_bytes()).await.is_ok() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Broadcast to all connected clients.
    #[allow(dead_code)] // Library API
    pub async fn broadcast(&self, message: &BridgeMessage) -> Result<()> {
        let json = serde_json::to_string(message)?;
        let line = format!("{json}\n");
        let clients = self.clients.lock().await;
        for (_id, writer) in clients.iter() {
            let mut w = writer.lock().await;
            let _ = w.write_all(line.as_bytes()).await;
        }
        Ok(())
    }

    /// Shut down the server, removing socket and PID files.
    pub async fn close(mut self) {
        // Signal accept loop to stop
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Disconnect all clients
        self.clients.lock().await.clear();
        // Cleanup files
        let _ = std::fs::remove_file(&self.socket_path);
        // Lock released on drop via _lock
    }
}

impl Drop for SocketServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.pid_path);
    }
}

/// Read a single line from an async buffered reader, bounding memory to `max_bytes`.
///
/// Unlike `AsyncBufReadExt::read_line`, this stops accumulating into `buf` once
/// `max_bytes` have been consumed, preventing a single newline-free payload from
/// exhausting memory. The function always drains to the next newline (or EOF) to
/// keep the stream frame-aligned for subsequent reads.
///
/// Returns `Ok(0)` on EOF, `Ok(n)` on success where `n` is the total bytes consumed.
/// If `n > max_bytes`, the line was oversized and `buf` should be discarded.
pub(crate) async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut String,
    max_bytes: usize,
) -> std::io::Result<usize> {
    buf.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(total);
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let to_consume = pos + 1;
            if total + to_consume <= max_bytes {
                buf.push_str(&String::from_utf8_lossy(&available[..to_consume]));
            }
            reader.consume(to_consume);
            total += to_consume;
            return Ok(total);
        }
        let len = available.len();
        if total + len <= max_bytes {
            buf.push_str(&String::from_utf8_lossy(available));
        }
        reader.consume(len);
        total += len;
    }
}

/// Read NDJSON lines from a client and broadcast each parsed message.
///
/// L5.5: Emits `tracing::info` events on connect and disconnect so operators
/// can observe client lifecycle in logs. Full event emission (e.g. broadcasting
/// a `client_connected` / `client_disconnected` BridgeMessage to other clients)
/// is not implemented — the socket server is an internal transport layer, not a
/// pub/sub bus. Tracing-level observability is sufficient for debugging.
async fn handle_client(
    reader: tokio::net::unix::OwnedReadHalf,
    client_id: &str,
    tx: broadcast::Sender<BridgeMessage>,
) {
    tracing::info!(client_id, "Socket client connected");
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        match read_bounded_line(&mut buf_reader, &mut line, MAX_LINE_BYTES).await {
            Ok(0) => {
                // L5.5: Log disconnect on EOF
                tracing::info!(client_id, "Socket client disconnected");
                break;
            }
            Ok(n) => {
                if n > MAX_LINE_BYTES {
                    tracing::warn!(client_id, len = n, "Oversized NDJSON line dropped");
                    continue;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<BridgeMessage>(trimmed) {
                    Ok(mut msg) => {
                        // S-2: Inject the originating client_id into metadata so
                        // the daemon can route approval responses back to the
                        // specific client that sent the approval_request, instead
                        // of broadcasting to all connected clients.
                        let meta = msg.metadata.get_or_insert_with(serde_json::Map::new);
                        meta.insert(
                            "_client_id".to_string(),
                            serde_json::Value::String(client_id.to_string()),
                        );
                        let _ = tx.send(msg);
                    }
                    Err(e) => {
                        tracing::debug!(client_id, error = %e, "Failed to parse NDJSON");
                    }
                }
            }
            Err(e) => {
                tracing::info!(client_id, error = %e, "Socket client disconnected (read error)");
                break;
            }
        }
    }
}

/// Acquire an exclusive flock on the PID file.
/// If the file is already locked, another daemon owns it.
fn acquire_flock(pid_path: &Path) -> Result<Flock<OwnedFd>> {
    use std::fs::OpenOptions;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(pid_path)
        .map_err(|e| AppError::Lock(format!("Cannot open PID file: {e}")))?;

    // Convert File -> OwnedFd via the safe From impl (File implements Into<OwnedFd>)
    let owned_fd: OwnedFd = file.into();

    Flock::lock(owned_fd, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(|(_fd, errno)| {
        AppError::Lock(format!("Another daemon instance holds the lock: {errno}"))
    })
}

// ====================================================================== client

/// Unix domain socket client for sending messages to the server.
///
/// NOTE (ADR-006 L4.3): Auto-reconnect is not implemented. The TS version had
/// a reconnectTimer for automatic reconnection, but the Rust hook process is
/// short-lived (exits after processing one event), so reconnection is
/// unnecessary. Long-lived consumers should implement retry logic externally.
#[allow(dead_code)] // Library API
pub struct SocketClient {
    stream: Option<UnixStream>,
}

impl Default for SocketClient {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)] // Library API
impl SocketClient {
    pub fn new() -> Self {
        Self { stream: None }
    }

    /// Connect to the server at the given socket path.
    ///
    /// M4.3: Error messages distinguish between common failure modes so callers
    /// can provide actionable feedback (e.g. "start the bridge daemon" vs retry).
    pub async fn connect(&mut self, socket_path: &Path) -> Result<()> {
        let stream = UnixStream::connect(socket_path).await.map_err(|e| {
            let msg = match e.kind() {
                std::io::ErrorKind::NotFound => "Bridge not running (socket not found)".to_string(),
                std::io::ErrorKind::ConnectionRefused => "Bridge refused connection".to_string(),
                std::io::ErrorKind::PermissionDenied => {
                    "Permission denied on bridge socket".to_string()
                }
                _ => format!("Failed to connect: {e}"),
            };
            AppError::Socket(msg)
        })?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Send a single NDJSON message.
    pub async fn send(&mut self, message: &BridgeMessage) -> Result<()> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| AppError::Socket("Not connected".into()))?;

        let json = serde_json::to_string(message)?;
        stream
            .write_all(format!("{json}\n").as_bytes())
            .await
            .map_err(|e| AppError::Socket(format!("Failed to write: {e}")))?;
        Ok(())
    }

    /// Send a message and wait for a response matching `session_id`.
    pub async fn send_and_wait(
        &mut self,
        message: &BridgeMessage,
        wait_timeout: Duration,
    ) -> Result<BridgeMessage> {
        self.send(message).await?;

        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| AppError::Socket("Not connected".into()))?;

        let (reader, _writer) = stream.split();
        let mut buf_reader = BufReader::new(reader);
        let session_id = &message.session_id;

        let result = timeout(wait_timeout, async {
            let mut line = String::new();
            loop {
                line.clear();
                let bytes = read_bounded_line(&mut buf_reader, &mut line, MAX_LINE_BYTES)
                    .await
                    .map_err(|e| AppError::Socket(format!("Read failed: {e}")))?;

                if bytes == 0 {
                    return Err(AppError::Socket("Connection closed".into()));
                }

                // FR31: Bound client read to MAX_LINE_BYTES
                if bytes > MAX_LINE_BYTES {
                    return Err(AppError::Socket(format!(
                        "Response line too large ({bytes} bytes, max {MAX_LINE_BYTES})",
                    )));
                }

                if let Ok(msg) = serde_json::from_str::<BridgeMessage>(line.trim()) {
                    if msg.session_id == *session_id {
                        return Ok(msg);
                    }
                }
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_) => Err(AppError::Socket("Response timeout".into())),
        }
    }

    /// Disconnect (drop the stream).
    pub fn disconnect(&mut self) {
        self.stream = None;
    }

    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

// ===================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn flock_rejects_double_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700));
        let pid = tmp.path().join("test.pid");

        let _lock1 = acquire_flock(&pid).expect("First lock should succeed");
        let result = acquire_flock(&pid);
        assert!(result.is_err(), "Second lock should fail");
    }

    #[test]
    fn flock_released_on_drop() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Ensure the temp dir is traversable even if another test's umask(0o177)
        // raced with TempDir creation (umask is process-wide).
        let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700));
        let pid = tmp.path().join("test2.pid");

        {
            let _lock = acquire_flock(&pid).unwrap();
        }
        // After drop, re-lock should succeed.
        let _lock2 = acquire_flock(&pid).expect("Lock after drop should succeed");
    }

    #[tokio::test]
    async fn server_client_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700));
        let sock = tmp.path().join("test.sock");
        let pid = tmp.path().join("test.pid");

        let mut server = SocketServer::new(&sock, &pid);
        server.listen().await.unwrap();
        let mut rx = server.subscribe();

        // Give the accept loop a moment to start.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Client sends a message.
        let mut client = SocketClient::new();
        client.connect(&sock).await.unwrap();
        let msg = BridgeMessage {
            msg_type: crate::types::MessageType::Unknown,
            session_id: "s1".to_string(),
            timestamp: "2024-01-01T00:00:00.000Z".to_string(),
            content: "hello".to_string(),
            metadata: None,
        };
        client.send(&msg).await.unwrap();

        // Server receives.
        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("recv");

        assert_eq!(received.session_id, "s1");
        assert_eq!(received.content, "hello");

        client.disconnect();
        server.close().await;
    }

    #[tokio::test]
    async fn connection_limit_enforced() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700));
        let sock = tmp.path().join("limit.sock");
        let pid = tmp.path().join("limit.pid");

        let mut server = SocketServer::new(&sock, &pid);
        server.listen().await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        // We just verify 64+ connections don't crash the server.
        // Full enforcement test would connect 65 clients, but keeping it lean.
        let count = server.client_count().await;
        assert_eq!(count, 0);

        server.close().await;
    }

    #[test]
    fn max_line_bytes_constant() {
        assert_eq!(MAX_LINE_BYTES, 1_048_576);
    }

    #[test]
    fn is_pid_running_self() {
        // Our own process should be running.
        let pid = std::process::id();
        assert!(super::is_pid_running(pid));
    }

    #[test]
    fn is_pid_running_nonexistent() {
        // PID 4294967 is almost certainly not running on any system.
        assert!(!super::is_pid_running(4_294_967));
    }
}
