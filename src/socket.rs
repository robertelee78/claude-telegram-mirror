use crate::error::{AppError, Result};
use crate::types::BridgeMessage;
use nix::fcntl::{flock, FlockArg};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};

/// Unix socket server for hook communication
pub struct SocketServer {
    socket_path: PathBuf,
    pid_path: PathBuf,
    pid_file: Option<fs::File>,
}

impl SocketServer {
    pub fn new(socket_path: PathBuf) -> Self {
        let pid_path = socket_path.with_extension("pid");
        Self {
            socket_path,
            pid_path,
            pid_file: None,
        }
    }

    /// Acquire PID lock using flock(2) - atomic, no TOCTOU race (Security fix #8)
    fn acquire_pid_lock(&mut self) -> Result<()> {
        use std::io::Write;

        // Ensure directory exists with secure permissions
        if let Some(parent) = self.pid_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }

        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.pid_path)?;

        // Security fix #3: Set PID file permissions to 0o600
        fs::set_permissions(&self.pid_path, fs::Permissions::from_mode(0o600))?;

        // flock(2): atomic advisory lock, no TOCTOU race
        match flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock) {
            Ok(()) => {
                // Write our PID
                let mut file_clone = file.try_clone()?;
                file_clone.set_len(0)?;
                write!(file_clone, "{}", std::process::id())?;
                self.pid_file = Some(file);
                tracing::debug!(pid = std::process::id(), "PID lock acquired via flock");
                Ok(())
            }
            Err(nix::errno::Errno::EWOULDBLOCK) => Err(AppError::Lock(
                "Another daemon instance is already running (flock held)".to_string(),
            )),
            Err(e) => Err(AppError::Lock(format!("flock failed: {}", e))),
        }
    }

    /// Release PID lock (flock is auto-released when file descriptor closes)
    fn release_pid_lock(&mut self) {
        self.pid_file.take(); // Drop the file, releasing the flock
        let _ = fs::remove_file(&self.pid_path);
        tracing::debug!("PID lock released");
    }

    /// Clean up stale socket file if no daemon is listening
    async fn cleanup_stale_socket(&self) -> Result<()> {
        if !self.socket_path.exists() {
            return Ok(());
        }

        // Try connecting to check if socket is active
        match UnixStream::connect(&self.socket_path).await {
            Ok(_) => {
                return Err(AppError::Socket(format!(
                    "Socket {} is already in use by another process",
                    self.socket_path.display()
                )));
            }
            Err(_) => {
                // Connection failed = stale socket
                fs::remove_file(&self.socket_path)?;
                tracing::info!(path = %self.socket_path.display(), "Removed stale socket file");
            }
        }
        Ok(())
    }

    /// Start listening for connections
    /// Returns a channel receiver for incoming messages and a sender for broadcasting
    pub async fn listen(
        &mut self,
    ) -> Result<(
        mpsc::Receiver<BridgeMessage>,
        broadcast::Sender<BridgeMessage>,
    )> {
        // Step 1: Acquire PID lock (flock-based, atomic)
        self.acquire_pid_lock()?;

        // Step 2: Clean up stale socket
        if let Err(e) = self.cleanup_stale_socket().await {
            self.release_pid_lock();
            return Err(e);
        }

        // Step 3: Ensure socket directory exists with secure permissions
        if let Some(parent) = self.socket_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }

        // Step 4: Bind the listener
        let listener = UnixListener::bind(&self.socket_path).map_err(|e| {
            self.release_pid_lock();
            AppError::Io(e)
        })?;

        // Security fix #3: Set socket file permissions to 0o600
        fs::set_permissions(&self.socket_path, fs::Permissions::from_mode(0o600))?;

        tracing::info!(
            path = %self.socket_path.display(),
            pid = std::process::id(),
            "Socket server listening"
        );

        // Channels for message passing
        let (msg_tx, msg_rx) = mpsc::channel::<BridgeMessage>(256);
        let (broadcast_tx, _) = broadcast::channel::<BridgeMessage>(256);
        let broadcast_tx_clone = broadcast_tx.clone();

        // Accept connections in background
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        let tx = msg_tx.clone();
                        let btx = broadcast_tx_clone.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                handle_client_connection(stream, tx, btx).await
                            {
                                tracing::debug!("Client connection ended: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Failed to accept connection: {}", e);
                    }
                }
            }
        });

        Ok((msg_rx, broadcast_tx))
    }

    /// Clean up socket and PID files
    pub fn cleanup(&mut self) {
        if self.socket_path.exists() {
            let _ = fs::remove_file(&self.socket_path);
        }
        self.release_pid_lock();
        tracing::info!("Socket server cleaned up");
    }
}

impl Drop for SocketServer {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Handle a single client connection (NDJSON protocol)
async fn handle_client_connection(
    stream: UnixStream,
    msg_tx: mpsc::Sender<BridgeMessage>,
    broadcast_tx: broadcast::Sender<BridgeMessage>,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut broadcast_rx = broadcast_tx.subscribe();

    // Forward broadcast messages to this client
    let write_task = tokio::spawn(async move {
        while let Ok(msg) = broadcast_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                let line = format!("{}\n", json);
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    });

    // Read NDJSON lines from client
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        // Security fix #10: serde_json::from_str returns Result, no unwrap/panic
        match serde_json::from_str::<BridgeMessage>(&line) {
            Ok(msg) => {
                if msg_tx.send(msg).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to parse NDJSON message");
            }
        }
    }

    write_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_socket_server_lifecycle() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test.sock");
        let mut server = SocketServer::new(socket_path.clone());

        let result = server.listen().await;
        assert!(result.is_ok());

        // Socket file should exist with correct permissions
        assert!(socket_path.exists());
        let meta = fs::metadata(&socket_path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        server.cleanup();
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn test_pid_lock_prevents_double_start() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test.sock");

        let mut server1 = SocketServer::new(socket_path.clone());
        let _result = server1.listen().await.unwrap();

        let mut server2 = SocketServer::new(socket_path);
        let result = server2.listen().await;
        assert!(result.is_err());

        server1.cleanup();
    }
}
