//! ADR-016 §Default enablement: keep Codex's app-server daemon alive.
//!
//! `codex app-server daemon start` is idempotent (spike: a second start answers
//! `{"status":"alreadyRunning",…}` with exit 0) and prints JSON that names the control
//! socket. ctm runs it through the **native** Codex binary (`detect::codex_binary`),
//! never the npm `codex.js` shim, so it works under launchd/systemd without `node`.

use crate::config::CodexHostConfig;
use crate::error::{AppError, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often to re-probe for a Codex install that was absent at the last look.
pub const ABSENT_POLL: Duration = Duration::from_secs(60);
const START_TIMEOUT: Duration = Duration::from_secs(45);
const SOCKET_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ensured {
    /// Control socket already accepts connections.
    AlreadyRunning,
    /// We ran `daemon start` and the socket is now reachable.
    Started,
    /// No native Codex binary and no `~/.codex` — nothing to do.
    NotInstalled,
}

async fn reachable(sock: &Path) -> bool {
    tokio::net::UnixStream::connect(sock).await.is_ok()
}

/// The `CODEX_HOME` the control socket belongs to: `<home>/app-server-control/<sock>`.
/// Every `daemon start`/`stop` is pinned to it, so the process acted on is always the
/// one behind `cx.socket_path` — never whichever home the caller's environment names.
pub fn codex_home(cx: &CodexHostConfig) -> PathBuf {
    cx.socket_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| crate::config::home_dir().join(".codex"))
}

/// Make sure the app-server daemon is up. Cheap when it already is (one connect).
pub async fn ensure_running(cx: &CodexHostConfig) -> Result<Ensured> {
    if reachable(&cx.socket_path).await {
        return Ok(Ensured::AlreadyRunning);
    }
    let Some(bin) = super::detect::codex_binary(cx.binary.as_deref()) else {
        return Ok(Ensured::NotInstalled);
    };
    start(&bin, &codex_home(cx)).await?;
    let deadline = tokio::time::Instant::now() + SOCKET_WAIT;
    while tokio::time::Instant::now() < deadline {
        if reachable(&cx.socket_path).await {
            return Ok(Ensured::Started);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(AppError::Socket(format!(
        "codex app-server daemon started but {} did not become reachable within {}s",
        cx.socket_path.display(),
        SOCKET_WAIT.as_secs()
    )))
}

/// `codex app-server daemon start` for `home`, bounded. Returns the socket path it
/// reported.
pub async fn start(bin: &Path, home: &Path) -> Result<Option<PathBuf>> {
    let out = tokio::time::timeout(
        START_TIMEOUT,
        tokio::process::Command::new(bin)
            .args(["app-server", "daemon", "start"])
            .env("CODEX_HOME", home)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| {
        AppError::Socket(format!(
            "`{} app-server daemon start` timed out",
            bin.display()
        ))
    })?
    .map_err(|e| AppError::Socket(format!("cannot run {}: {e}", bin.display())))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(AppError::Socket(format!(
            "`{} app-server daemon start` failed ({}): {}",
            bin.display(),
            out.status,
            crate::formatting::truncate(stderr.trim(), 300)
        )));
    }
    Ok(parse_start_output(&stdout))
}

/// `codex app-server daemon stop`, bounded. The only way a running daemon adopts a
/// different signed-in account is to be restarted (ADR-021).
pub async fn stop(bin: &Path, home: &Path) -> Result<()> {
    let out = tokio::time::timeout(
        START_TIMEOUT,
        tokio::process::Command::new(bin)
            .args(["app-server", "daemon", "stop"])
            .env("CODEX_HOME", home)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| {
        AppError::Socket(format!(
            "`{} app-server daemon stop` timed out",
            bin.display()
        ))
    })?
    .map_err(|e| AppError::Socket(format!("cannot run {}: {e}", bin.display())))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(AppError::Socket(format!(
            "`{} app-server daemon stop` failed ({}): {}",
            bin.display(),
            out.status,
            crate::formatting::truncate(stderr.trim(), 300)
        )));
    }
    Ok(())
}

/// Wait for the control socket to stop accepting connections after `stop`.
pub async fn wait_gone(sock: &Path, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if !reachable(sock).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

/// `{"status":"alreadyRunning"|"started",…,"socketPath":"…"}` → socket path.
pub fn parse_start_output(stdout: &str) -> Option<PathBuf> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .find_map(|v| v.get("socketPath")?.as_str().map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_output_yields_socket_path() {
        // Verbatim from `codex app-server daemon start` (0.155.1), second invocation.
        let out = r#"{"status":"alreadyRunning","backend":"pid","managedCodexPath":"/u/.codex/packages/standalone/current/bin/codex","managedCodexVersion":"0.155.1","socketPath":"/u/.codex/app-server-control/app-server-control.sock","cliVersion":"0.155.1","appServerVersion":"0.153.2"}"#;
        assert_eq!(
            parse_start_output(out),
            Some(PathBuf::from(
                "/u/.codex/app-server-control/app-server-control.sock"
            ))
        );
        assert_eq!(parse_start_output("not json\n"), None);
    }

    #[tokio::test]
    async fn not_installed_when_no_binary_and_no_state_dir() {
        // Point everything at an empty temp HOME-like layout via an unreachable socket
        // and a bogus override; `codex_binary` with a non-executable override is None.
        let dir = tempfile::tempdir().unwrap();
        let cx = CodexHostConfig {
            enabled: true,
            socket_path: dir.path().join("nope.sock"),
            binary: Some(dir.path().join("missing-codex")),
        };
        // With an explicit (missing) override the detector never falls through to PATH.
        assert_eq!(ensure_running(&cx).await.unwrap(), Ensured::NotInstalled);
    }
}
